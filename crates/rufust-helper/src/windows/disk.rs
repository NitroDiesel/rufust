//! Raw disk and volume access for the elevated Windows engine.
//!
//! Windows refuses writes to sectors of a mounted volume, so every volume on
//! the target is locked and dismounted before raw I/O and stays locked until
//! the write is done. Partitions are laid out with the disk IOCTLs, and new
//! volumes are found by their extent on the target disk, never by letter.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::{FileExt, OpenOptionsExt};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use rufust_core::plan::PartitionScheme;
use rufust_core::progress::CancellationToken;
use rufust_platform::windows::{self as platform, ioctl, wide, Volume};
use windows_sys::core::GUID;
use windows_sys::Win32::Storage::FileSystem::{
    DeleteVolumeMountPointW, GetLogicalDrives, SetVolumeMountPointW, FILE_FLAG_WRITE_THROUGH,
    FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::Com::CoCreateGuid;
use windows_sys::Win32::System::Ioctl::{
    CREATE_DISK, CREATE_DISK_0, CREATE_DISK_GPT, CREATE_DISK_MBR, DRIVE_LAYOUT_INFORMATION_EX_0,
    DRIVE_LAYOUT_INFORMATION_GPT, DRIVE_LAYOUT_INFORMATION_MBR, FSCTL_ALLOW_EXTENDED_DASD_IO,
    FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, FSCTL_UNLOCK_VOLUME, IOCTL_DISK_CREATE_DISK,
    IOCTL_DISK_SET_DRIVE_LAYOUT_EX, IOCTL_DISK_UPDATE_PROPERTIES, PARTITION_INFORMATION_EX,
    PARTITION_INFORMATION_EX_0, PARTITION_INFORMATION_GPT, PARTITION_INFORMATION_MBR,
    PARTITION_STYLE_GPT, PARTITION_STYLE_MBR, PARTITION_STYLE_RAW,
};

use crate::{check_cancel, HelperError};

pub(crate) const MIB: u64 = 1024 * 1024;
/// Every raw transfer is a multiple of this, and buffers are aligned to it.
const ALIGNMENT: usize = 4096;

/// A heap buffer whose start is aligned for unbuffered device I/O.
pub(crate) struct AlignedBuffer {
    storage: Vec<u8>,
    start: usize,
    len: usize,
}

impl AlignedBuffer {
    pub(crate) fn new(len: usize) -> Self {
        let storage = vec![0u8; len + ALIGNMENT];
        let start = storage.as_ptr().align_offset(ALIGNMENT);
        Self {
            storage,
            start,
            len,
        }
    }

    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.storage[self.start..self.start + self.len]
    }

    pub(crate) fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.storage[self.start..self.start + self.len]
    }
}

fn open_rw(path: &str) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_WRITE_THROUGH)
        .open(path)
}

fn write_all_at(file: &File, mut offset: u64, mut bytes: &[u8]) -> io::Result<()> {
    while !bytes.is_empty() {
        let written = file.seek_write(bytes, offset)?;
        if written == 0 {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "the device accepted no more data",
            ));
        }
        bytes = &bytes[written..];
        offset += written as u64;
    }
    Ok(())
}

fn read_exact_at(file: &File, mut offset: u64, mut buffer: &mut [u8]) -> io::Result<()> {
    while !buffer.is_empty() {
        let read = file.seek_read(buffer, offset)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the device ended early",
            ));
        }
        buffer = &mut buffer[read..];
        offset += read as u64;
    }
    Ok(())
}

/// The whole target disk, opened read-write.
pub(crate) struct Disk {
    file: File,
    pub number: u32,
    pub size: u64,
    pub sector: u32,
}

impl Disk {
    pub(crate) fn open(number: u32, size: u64, sector: u32) -> Result<Self, HelperError> {
        let path = platform::physical_drive_path(number);
        let file = open_rw(&path.to_string_lossy()).map_err(|error| {
            HelperError::Operation(format!("could not open {}: {error}", path.display()))
        })?;
        Ok(Self {
            file,
            number,
            size,
            sector,
        })
    }

    pub(crate) fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), HelperError> {
        write_all_at(&self.file, offset, bytes).map_err(|error| {
            HelperError::Operation(format!("write failed at byte {offset}: {error}"))
        })
    }

    pub(crate) fn read_at(&self, offset: u64, buffer: &mut [u8]) -> Result<(), HelperError> {
        read_exact_at(&self.file, offset, buffer).map_err(|error| {
            HelperError::Operation(format!("read failed at byte {offset}: {error}"))
        })
    }

    pub(crate) fn flush(&self) -> Result<(), HelperError> {
        self.file.sync_all()?;
        Ok(())
    }

    /// Ask Windows to re-read the partition table.
    pub(crate) fn rescan(&self) {
        let _ = ioctl(&self.file, IOCTL_DISK_UPDATE_PROPERTIES, &[], &mut []);
    }

    /// Zero the first and last MiB: old partition tables, the backup GPT,
    /// and stale file system signatures.
    fn wipe_ends(&self) -> Result<(), HelperError> {
        let zeros = AlignedBuffer::new(MIB as usize);
        let tail = self.size.saturating_sub(MIB) / u64::from(self.sector) * u64::from(self.sector);
        self.write_at(0, zeros.as_slice())?;
        let tail_length = (self.size - tail).min(MIB) as usize;
        self.write_at(tail, &zeros.as_slice()[..tail_length])?;
        self.flush()
    }
}

/// A locked volume. Unlocked when dropped.
pub(crate) struct VolumeLock {
    file: File,
}

impl VolumeLock {
    /// Lock and dismount, so the file system can neither use nor later
    /// rewrite sectors underneath a raw write.
    pub(crate) fn acquire(volume: &Volume) -> Result<Self, HelperError> {
        Self::lock(volume, true)
    }

    /// Lock but keep the file system mounted, for writing its boot record
    /// through this handle as upstream does.
    pub(crate) fn acquire_mounted(volume: &Volume) -> Result<Self, HelperError> {
        Self::lock(volume, false)
    }

    fn lock(volume: &Volume, dismount: bool) -> Result<Self, HelperError> {
        let path = volume.device_path();
        let file = open_rw(&path).map_err(|error| {
            HelperError::Operation(format!("could not open volume {}: {error}", volume.name))
        })?;
        // Explorer and indexers hold handles briefly; retry before forcing.
        let mut locked = false;
        for attempt in 0..20 {
            if ioctl(&file, FSCTL_LOCK_VOLUME, &[], &mut []).is_ok() {
                locked = true;
                break;
            }
            if attempt == 9 {
                // A forced dismount invalidates other handles; then lock.
                let _ = ioctl(&file, FSCTL_DISMOUNT_VOLUME, &[], &mut []);
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        if !locked {
            return Err(HelperError::Operation(format!(
                "volume {} is in use; close programs that use the drive and try again",
                mount_label(volume)
            )));
        }
        if dismount {
            ioctl(&file, FSCTL_DISMOUNT_VOLUME, &[], &mut []).map_err(|error| {
                HelperError::Operation(format!(
                    "could not dismount {}: {error}",
                    mount_label(volume)
                ))
            })?;
        }
        // Raw I/O must not be bounded by the file system's idea of its size.
        let _ = ioctl(&file, FSCTL_ALLOW_EXTENDED_DASD_IO, &[], &mut []);
        Ok(Self { file })
    }

    pub(crate) fn write_at(&self, offset: u64, bytes: &[u8]) -> io::Result<()> {
        write_all_at(&self.file, offset, bytes)
    }

    pub(crate) fn read_at(&self, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        read_exact_at(&self.file, offset, buffer)
    }

    pub(crate) fn flush(&self) -> io::Result<()> {
        self.file.sync_all()
    }
}

impl Drop for VolumeLock {
    fn drop(&mut self) {
        let _ = self.file.sync_all();
        let _ = ioctl(&self.file, FSCTL_UNLOCK_VOLUME, &[], &mut []);
    }
}

fn mount_label(volume: &Volume) -> String {
    volume
        .mount_points
        .first()
        .map(|mount| mount.display().to_string())
        .unwrap_or_else(|| volume.name.clone())
}

/// Lock and dismount every volume on `disk`.
pub(crate) fn lock_disk_volumes(disk: u32) -> Result<Vec<VolumeLock>, HelperError> {
    platform::volumes()
        .iter()
        .filter(|volume| volume.on_disk(disk))
        .map(VolumeLock::acquire)
        .collect()
}

/// Flush a mounted volume's file system to the device.
pub(crate) fn flush_volume(volume: &Volume) -> Result<(), HelperError> {
    open_rw(&volume.device_path())
        .and_then(|file| file.sync_all())
        .map_err(|error| HelperError::Operation(format!("could not flush the volume: {error}")))
}

/// Dismount a volume so the next access reads the device, not the cache.
pub(crate) fn cycle_volume(volume: &Volume) -> Result<(), HelperError> {
    drop(VolumeLock::acquire(volume)?);
    Ok(())
}

/// Wait for the volume that starts at `offset` on `disk` to appear.
pub(crate) fn wait_for_volume(
    disk: u32,
    offset: u64,
    cancel: &CancellationToken,
) -> Result<Volume, HelperError> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        check_cancel(cancel)?;
        if let Some(volume) = platform::volumes().into_iter().find(|volume| {
            volume.extents.len() == 1 && volume.on_disk(disk) && volume.extents[0].offset == offset
        }) {
            return Ok(volume);
        }
        if Instant::now() >= deadline {
            return Err(HelperError::Operation(
                "Windows did not create a volume for the new partition".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// Remove a new volume's drive letters so Explorer does not offer to format
/// it mid-operation. Returns the first letter it had.
pub(crate) fn hide_volume(volume: &Volume) -> Option<PathBuf> {
    for mount in &volume.mount_points {
        let name = wide(mount);
        // SAFETY: `name` is a NUL-terminated mount point path.
        unsafe { DeleteVolumeMountPointW(name.as_ptr()) };
    }
    volume.mount_points.first().cloned()
}

/// Give the finished volume a drive letter: `preferred` if free, else the
/// first free letter from D:.
pub(crate) fn show_volume(volume: &Volume, preferred: Option<PathBuf>) {
    let current = platform::volumes()
        .into_iter()
        .find(|candidate| candidate.name == volume.name);
    if current.is_some_and(|current| !current.mount_points.is_empty()) {
        return;
    }
    // SAFETY: no arguments; returns the bitmask of used letters.
    let used = unsafe { GetLogicalDrives() };
    let free = |letter: u8| used & (1 << (letter - b'A')) == 0;
    let preferred = preferred
        .and_then(|path| path.to_str().and_then(|text| text.bytes().next()))
        .map(|letter| letter.to_ascii_uppercase())
        .filter(|letter| letter.is_ascii_uppercase() && free(*letter));
    let Some(letter) = preferred.or_else(|| (b'D'..=b'Z').find(|letter| free(*letter))) else {
        return;
    };
    let mount = wide(format!("{}:\\", letter as char));
    let name = wide(&volume.name);
    // SAFETY: both strings are NUL-terminated.
    unsafe { SetVolumeMountPointW(mount.as_ptr(), name.as_ptr()) };
}

/// One partition to create.
#[derive(Clone)]
pub(crate) struct PartitionSpec {
    pub offset: u64,
    pub size: u64,
    pub mbr_type: u8,
    pub gpt_type: GUID,
    pub gpt_name: &'static str,
    pub gpt_attributes: u64,
    pub active: bool,
}

pub(crate) const GPT_BASIC_DATA: GUID = GUID::from_u128(0xebd0a0a2_b9e5_4433_87c0_68b6b72699c7);

fn new_guid() -> Result<GUID, HelperError> {
    let mut guid = GUID::from_u128(0);
    // SAFETY: writes one GUID.
    let status = unsafe { CoCreateGuid(&mut guid) };
    if status != 0 {
        return Err(HelperError::Operation("could not create a GUID".into()));
    }
    Ok(guid)
}

/// DRIVE_LAYOUT_INFORMATION_EX with room for four entries.
#[repr(C)]
struct DriveLayout {
    style: u32,
    count: u32,
    table: DRIVE_LAYOUT_INFORMATION_EX_0,
    entries: [PARTITION_INFORMATION_EX; 4],
}

fn bytes_of<T>(value: &T) -> &[u8] {
    // SAFETY: plain-old-data Win32 structures are read as their bytes.
    unsafe { std::slice::from_raw_parts((value as *const T).cast::<u8>(), size_of::<T>()) }
}

/// Replace the partition table: wipe both ends, create an empty MBR, GPT, or
/// raw (super-floppy) disk, then add `partitions` and let Windows re-read it.
pub(crate) fn create_layout(
    disk: &Disk,
    scheme: PartitionScheme,
    partitions: &[PartitionSpec],
) -> Result<(), HelperError> {
    if partitions.len() > 4 {
        return Err(HelperError::Operation("too many partitions".into()));
    }
    disk.wipe_ends()?;
    let handle = &disk.file;
    let fail = |step: &str, error: io::Error| {
        HelperError::Operation(format!("could not {step} the partition table: {error}"))
    };
    let style = match scheme {
        PartitionScheme::Mbr => PARTITION_STYLE_MBR,
        PartitionScheme::Gpt => PARTITION_STYLE_GPT,
        PartitionScheme::SuperFloppy => PARTITION_STYLE_RAW,
    };
    let signature = new_guid()?;
    let create = CREATE_DISK {
        PartitionStyle: style,
        Anonymous: if scheme == PartitionScheme::Gpt {
            CREATE_DISK_0 {
                Gpt: CREATE_DISK_GPT {
                    DiskId: signature,
                    MaxPartitionCount: 128,
                },
            }
        } else {
            CREATE_DISK_0 {
                Mbr: CREATE_DISK_MBR {
                    Signature: signature.data1,
                },
            }
        },
    };
    ioctl(handle, IOCTL_DISK_CREATE_DISK, bytes_of(&create), &mut [])
        .map_err(|e| fail("create", e))?;
    disk.rescan();
    if scheme == PartitionScheme::SuperFloppy {
        return Ok(());
    }

    let sector = u64::from(disk.sector);
    // SAFETY: an all-zero PARTITION_INFORMATION_EX is a valid unused entry.
    let mut layout: DriveLayout = unsafe { std::mem::zeroed() };
    layout.style = style as u32;
    if scheme == PartitionScheme::Gpt {
        layout.count = partitions.len() as u32;
        layout.table = DRIVE_LAYOUT_INFORMATION_EX_0 {
            Gpt: DRIVE_LAYOUT_INFORMATION_GPT {
                DiskId: signature,
                StartingUsableOffset: (34 * sector) as i64,
                UsableLength: disk.size.saturating_sub(68 * sector) as i64,
                MaxPartitionCount: 128,
            },
        };
    } else {
        // MBR layouts always carry four entries.
        layout.count = 4;
        layout.table = DRIVE_LAYOUT_INFORMATION_EX_0 {
            Mbr: DRIVE_LAYOUT_INFORMATION_MBR {
                Signature: signature.data1,
                CheckSum: 0,
            },
        };
    }
    for (index, spec) in partitions.iter().enumerate() {
        let entry = &mut layout.entries[index];
        entry.PartitionStyle = style;
        entry.StartingOffset = spec.offset as i64;
        entry.PartitionLength = spec.size as i64;
        entry.PartitionNumber = index as u32 + 1;
        entry.RewritePartition = 1;
        entry.Anonymous = if scheme == PartitionScheme::Gpt {
            let mut name = [0u16; 36];
            for (slot, unit) in name.iter_mut().zip(spec.gpt_name.encode_utf16()) {
                *slot = unit;
            }
            PARTITION_INFORMATION_EX_0 {
                Gpt: PARTITION_INFORMATION_GPT {
                    PartitionType: spec.gpt_type,
                    PartitionId: new_guid()?,
                    Attributes: spec.gpt_attributes,
                    Name: name,
                },
            }
        } else {
            PARTITION_INFORMATION_EX_0 {
                Mbr: PARTITION_INFORMATION_MBR {
                    PartitionType: spec.mbr_type,
                    BootIndicator: u8::from(spec.active),
                    RecognizedPartition: 1,
                    HiddenSectors: u32::try_from(spec.offset / sector).map_err(|_| {
                        HelperError::Operation("partition start is beyond MBR reach".into())
                    })?,
                    PartitionId: GUID::from_u128(0),
                },
            }
        };
    }
    for entry in layout.entries.iter_mut().skip(partitions.len()) {
        entry.PartitionStyle = style;
        entry.RewritePartition = 1;
    }
    let length = if scheme == PartitionScheme::Gpt {
        size_of::<DriveLayout>() - (4 - partitions.len()) * size_of::<PARTITION_INFORMATION_EX>()
    } else {
        size_of::<DriveLayout>()
    };
    ioctl(
        handle,
        IOCTL_DISK_SET_DRIVE_LAYOUT_EX,
        &bytes_of(&layout)[..length],
        &mut [],
    )
    .map_err(|e| fail("write", e))?;
    disk.rescan();
    Ok(())
}

/// Set the type byte of MBR partition `index` (0-based) in sector 0.
pub(crate) fn set_mbr_type(disk: &Disk, index: usize, kind: u8) -> Result<(), HelperError> {
    let mut sector = AlignedBuffer::new(disk.sector as usize);
    disk.read_at(0, sector.as_mut_slice())?;
    let at = 446 + 16 * index + 4;
    sector.as_mut_slice()[at] = kind;
    disk.write_at(0, sector.as_slice())?;
    disk.flush()?;
    disk.rescan();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligned_buffers_are_aligned_and_sized() {
        for length in [512, 4096, MIB as usize] {
            let mut buffer = AlignedBuffer::new(length);
            assert_eq!(buffer.as_slice().len(), length);
            assert_eq!(buffer.as_mut_slice().as_ptr() as usize % ALIGNMENT, 0);
        }
    }

    #[test]
    fn drive_layout_matches_the_win32_header() {
        // PartitionStyle, PartitionCount, then the 40-byte GPT union.
        assert_eq!(std::mem::offset_of!(DriveLayout, entries), 48);
        assert_eq!(size_of::<PARTITION_INFORMATION_EX>(), 144);
    }
}
