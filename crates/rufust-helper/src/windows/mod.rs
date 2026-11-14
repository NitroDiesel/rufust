//! The Windows disk engine. It runs in an elevated copy of the desktop
//! program (or `rufust-helper.exe`) that the unelevated window starts
//! through UAC, and receives one typed request over a private named pipe.
//!
//! The flow mirrors the udisks2 engine: bind and check the source, revalidate
//! the target's identity, lock and dismount its volumes, write, flush,
//! verify. Partitioning uses the disk IOCTLs, file systems come from
//! Windows' own providers (or the built-in large FAT32 formatter), and Windows
//! installer media reuses the shared layout, copy, customization, and boot
//! record code.

mod disk;
mod format;
mod pipe;
mod source;
#[cfg(test)]
mod vhd_tests;

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::{Duration, Instant};

use rufust_core::plan::{
    FileSystem, ImageSourceKind, PartitionScheme, VerificationLevel, WriteMode,
};
use rufust_core::progress::{CancellationToken, ProgressStage};
use rufust_helper_protocol::{
    FormatSpec, HelperEvent, HelperOperation, HelperRequest, HelperResult, SourceSpec,
    TargetIdentity, WindowsCustomization,
};
use rufust_platform::windows::{self as platform, wide, Volume};
use rufust_platform::{discover_devices, DeviceIdentity};
use sha2::{Digest, Sha256};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows_sys::Win32::Storage::FileSystem::SetVolumeLabelW;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::engine::{
    self, boot_record_start, copy_windows_files, plan_windows_media, refuse_blocking_risks,
    verify_windows_files, Meter, RawDisk, Reporter, BAD_BLOCK_CHUNK, MIB, VERIFY_CHUNK,
};
use crate::windows_media::{self as winmedia, Span};
use crate::{check_cancel, emit, fat32, hex_lower, EventSink, ExecutionOptions, HelperError};
use disk::{AlignedBuffer, Disk, PartitionSpec, VolumeLock, GPT_BASIC_DATA};

pub use pipe::serve_pipe;

const GIB: u64 = 1024 * MIB;
/// Windows' own formatter stops at 32 GB for FAT32, as upstream notes.
const LARGE_FAT32: u64 = 32 * GIB;

/// Whether this process holds an elevated (administrator) token.
pub fn is_elevated() -> bool {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: queries this process's own token.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return false;
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut length = 0u32;
    // SAFETY: `elevation` is writable for its size.
    let ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        )
    };
    // SAFETY: closes the token opened above.
    unsafe { CloseHandle(token) };
    ok != 0 && elevation.TokenIsElevated != 0
}

/// Validate a request for this engine without touching the target.
pub fn validate_request(request: &HelperRequest) -> Result<(), HelperError> {
    request.validate()?;
    platform::disk_number(&request.target.node).ok_or_else(|| {
        HelperError::Operation("the target is not a \\\\.\\PhysicalDrive device".into())
    })?;
    match &request.operation {
        HelperOperation::WriteMedia {
            write_mode,
            source,
            format,
            install_bootloader,
            windows_customization,
            ..
        } => {
            if windows_customization.is_some() && *write_mode != WriteMode::IsoFileCopy {
                return Err(HelperError::Operation(
                    "Windows customization needs Windows installer media".into(),
                ));
            }
            if install_bootloader.is_some() || format.persistence_bytes != 0 {
                return Err(HelperError::Operation(
                    "bootloader and persistence operations are not enabled".into(),
                ));
            }
            match write_mode {
                WriteMode::DdImage => match source.kind {
                    ImageSourceKind::Raw
                    | ImageSourceKind::IsoHybrid
                    | ImageSourceKind::Vhd
                    | ImageSourceKind::Vhdx => {}
                    ImageSourceKind::CompressedRaw => {
                        source::decoder_kind(&source.path)?;
                    }
                    _ => {
                        return Err(HelperError::Operation(
                            "source kind cannot be safely raw-written".into(),
                        ))
                    }
                },
                WriteMode::IsoFileCopy => {
                    if source.kind != ImageSourceKind::Iso {
                        return Err(HelperError::Operation(
                            "file-copy media needs an ISO image".into(),
                        ));
                    }
                    winmedia::requirements(format.filesystem, format.scheme, format.boot_mode)?;
                    format::provider_name(format.filesystem)?;
                }
                _ => {
                    return Err(HelperError::Operation(
                        "this write mode is not available yet".into(),
                    ))
                }
            }
        }
        HelperOperation::FormatMedia { format, .. } => {
            format::provider_name(format.filesystem)?;
        }
        HelperOperation::CaptureImage { .. } => {
            return Err(HelperError::Operation(
                "image capture is not available yet".into(),
            ))
        }
    }
    Ok(())
}

/// Re-resolve the target and compare its identity strictly.
pub fn revalidate_target(expected: &TargetIdentity) -> Result<DeviceIdentity, HelperError> {
    let observed = discover_devices(true)?
        .into_iter()
        .find(|device| device.node == expected.node)
        .ok_or_else(|| {
            HelperError::Revalidation(format!(
                "device {} no longer present",
                expected.node.display()
            ))
        })?;
    let fingerprint = &expected.fingerprint;
    let serial = fingerprint.serial.as_deref().unwrap_or_default();
    let mismatch = if observed.minor != fingerprint.number.minor
        || observed.major != fingerprint.number.major
    {
        Some("disk number mismatch")
    } else if observed.sysfs_path != fingerprint.canonical_sysfs_path {
        Some("device interface mismatch")
    } else if observed.size_bytes != fingerprint.size_bytes {
        Some("device size changed")
    } else if observed.logical_sector_size != fingerprint.logical_block_size {
        Some("logical sector size changed")
    } else if expected.serial != serial {
        Some("request contains inconsistent serial identity")
    } else if observed.serial != serial {
        Some("device serial mismatch")
    } else if observed.model != expected.model {
        Some("device model mismatch")
    } else if observed.read_only {
        Some("device is read-only")
    } else {
        None
    };
    if let Some(reason) = mismatch {
        return Err(HelperError::Revalidation(reason.into()));
    }
    let block = platform::to_block_device(&observed, &platform::volumes());
    refuse_blocking_risks(&block)?;
    Ok(observed)
}

/// Inspect virtual capacity without elevation, for the desktop preview.
pub fn inspect_virtual_disk_for_user(
    source: &File,
    kind: ImageSourceKind,
    cancel: &CancellationToken,
) -> Result<u64, HelperError> {
    check_cancel(cancel)?;
    if !source.metadata()?.is_file() {
        return Err(HelperError::Operation(
            "virtual disk inspection requires a regular file".into(),
        ));
    }
    let path = source::final_path(source)
        .ok_or_else(|| HelperError::Operation("the image path could not be resolved".into()))?;
    source::virtual_size(&path, kind)
}

/// Execute a validated request. Writing needs an elevated process.
pub fn execute(
    mut request: HelperRequest,
    options: ExecutionOptions,
    mut sink: EventSink,
) -> Result<HelperResult, HelperError> {
    validate_request(&request)?;
    if options.dry_run {
        emit(
            &mut sink,
            HelperEvent::Accepted {
                job_id: request.job_id,
            },
        );
        emit(
            &mut sink,
            HelperEvent::Log {
                job_id: request.job_id,
                line: "Dry run: the request is valid; nothing was written.".into(),
            },
        );
        let result = HelperResult::Success;
        emit(
            &mut sink,
            HelperEvent::Finished {
                job_id: request.job_id,
                result: result.clone(),
            },
        );
        return Ok(result);
    }
    if !is_elevated() {
        return Err(HelperError::Operation(
            "writing to disks needs administrator rights".into(),
        ));
    }
    let cancel = options.cancel.clone();
    let source_file = match &request.operation {
        HelperOperation::WriteMedia { source, .. } => Some(source::bind(source)?),
        _ => None,
    };
    if let HelperOperation::WriteMedia { source, .. } = &mut request.operation {
        if matches!(source.kind, ImageSourceKind::Vhd | ImageSourceKind::Vhdx) {
            let size = source::virtual_size(&source.path, source.kind)?;
            if source
                .decompressed_size_bytes
                .is_some_and(|expected| expected != size)
            {
                return Err(HelperError::Operation(
                    "virtual disk size changed since selection".into(),
                ));
            }
            source.decompressed_size_bytes = Some(size);
        }
    }

    emit(
        &mut sink,
        HelperEvent::Accepted {
            job_id: request.job_id,
        },
    );
    let mut report = Reporter {
        job_id: request.job_id,
        sink: &mut sink,
        step: 0,
        total: 9,
    };
    report.log(format!(
        "Using the Windows disk engine (Rufust {})",
        crate::HELPER_VERSION
    ));
    report.stage(ProgressStage::Preparing, "Checking the target device");
    let identity = revalidate_target(&request.target)?;
    if let (HelperOperation::WriteMedia { source, .. }, Some(file)) =
        (&request.operation, source_file.as_ref())
    {
        let on_target = source::volume_of(file)
            .map(|volume| {
                platform::volumes()
                    .iter()
                    .any(|candidate| candidate.name == volume && candidate.on_disk(identity.minor))
            })
            .unwrap_or(false);
        if on_target {
            return Err(HelperError::Revalidation(
                "source image is stored on the target device".into(),
            ));
        }
        if source.decompressed_size_bytes.unwrap_or(source.size_bytes) > identity.size_bytes {
            return Err(HelperError::Operation(
                "image is larger than the target device".into(),
            ));
        }
    }
    check_cancel(&cancel)?;

    let target = Target {
        identity: &identity,
        cancel: &cancel,
    };
    match &request.operation {
        HelperOperation::WriteMedia {
            write_mode: WriteMode::DdImage,
            source,
            verification,
            bad_blocks,
            ..
        } => {
            let file = source_file
                .as_ref()
                .ok_or_else(|| HelperError::Operation("bound source missing".into()))?;
            write_disk_image(
                &target,
                file,
                source,
                *verification,
                *bad_blocks,
                &mut report,
            )?;
        }
        HelperOperation::WriteMedia {
            format,
            verification,
            bad_blocks,
            windows_customization,
            ..
        } => {
            let file = source_file
                .as_ref()
                .ok_or_else(|| HelperError::Operation("bound source missing".into()))?;
            write_windows_media(
                &target,
                file,
                MediaOptions {
                    format,
                    verification: *verification,
                    bad_blocks: *bad_blocks,
                    customization: windows_customization
                        .as_deref()
                        .filter(|options| !options.is_empty()),
                },
                &mut report,
            )?;
        }
        HelperOperation::FormatMedia { format, bad_blocks } => {
            format_media(&target, format, *bad_blocks, &mut report)?;
        }
        HelperOperation::CaptureImage { .. } => {
            return Err(HelperError::Operation(
                "image capture is not available yet".into(),
            ))
        }
    }

    report.step = report.total;
    report.stage(ProgressStage::Finalizing, "Complete");
    let result = HelperResult::Success;
    emit(
        &mut sink,
        HelperEvent::Finished {
            job_id: request.job_id,
            result: result.clone(),
        },
    );
    Ok(result)
}

struct Target<'a> {
    identity: &'a DeviceIdentity,
    cancel: &'a CancellationToken,
}

impl Target<'_> {
    fn number(&self) -> u32 {
        self.identity.minor
    }

    fn sector(&self) -> u64 {
        u64::from(self.identity.logical_sector_size.max(512))
    }

    /// Lock every volume on the disk, then open the disk itself.
    fn open(&self, report: &mut Reporter<'_>) -> Result<(Vec<VolumeLock>, Disk), HelperError> {
        report.stage(ProgressStage::Unmounting, "Unmounting the target");
        let locks = disk::lock_disk_volumes(self.number())?;
        check_cancel(self.cancel)?;
        report.stage(ProgressStage::Authorizing, "Opening the device");
        let disk = Disk::open(
            self.number(),
            self.identity.size_bytes,
            self.identity.logical_sector_size.max(512),
        )?;
        Ok((locks, disk))
    }
}

fn decode_error(error: io::Error) -> HelperError {
    HelperError::Operation(format!(
        "the image could not be read or decoded (invalid or truncated input): {error}"
    ))
}

struct WriteReceipt {
    bytes_written: u64,
    sha256: [u8; 32],
}

fn write_disk_image(
    target: &Target<'_>,
    file: &File,
    spec: &SourceSpec,
    verification: VerificationLevel,
    bad_blocks: bool,
    report: &mut Reporter<'_>,
) -> Result<(), HelperError> {
    let (locks, disk) = target.open(report)?;
    if bad_blocks {
        test_bad_blocks(&disk, target.cancel, report)?;
    }
    report.stage(ProgressStage::WritingImage, "Writing the image");
    let mut attached = None;
    let (reader, total): (Box<dyn Read + Send>, Option<u64>) = match spec.kind {
        ImageSourceKind::CompressedRaw => (
            source::decoder(file, &spec.path)?,
            spec.decompressed_size_bytes,
        ),
        ImageSourceKind::Vhd | ImageSourceKind::Vhdx => {
            let virtual_disk = source::attach(&spec.path, spec.kind)?;
            if spec
                .decompressed_size_bytes
                .is_some_and(|expected| expected != virtual_disk.size)
            {
                return Err(HelperError::Operation(
                    "virtual disk size changed since selection".into(),
                ));
            }
            let size = virtual_disk.size;
            let reader = Box::new(virtual_disk.disk.try_clone()?.take(size));
            attached = Some(virtual_disk);
            (reader, Some(size))
        }
        _ => {
            let mut plain = file.try_clone()?;
            plain.seek(SeekFrom::Start(0))?;
            (Box::new(plain), Some(spec.size_bytes))
        }
    };
    let limit = total.unwrap_or(disk.size).min(disk.size);
    let receipt = copy_to_disk(
        &disk,
        reader,
        limit,
        total,
        &spec.path,
        target.cancel,
        report,
    )?;
    drop(attached);
    if matches!(spec.kind, ImageSourceKind::Raw | ImageSourceKind::IsoHybrid)
        && receipt.bytes_written != spec.size_bytes
    {
        return Err(HelperError::Operation(
            "source image changed while it was being read".into(),
        ));
    }
    if let Some(expected) = total {
        if receipt.bytes_written != expected {
            return Err(HelperError::Operation(format!(
                "decoded size mismatch: expected {expected}, wrote {}",
                receipt.bytes_written
            )));
        }
    }
    if let Some(expected) = &spec.expected_sha256 {
        if hex_lower(&receipt.sha256) != expected.to_ascii_lowercase() {
            return Err(HelperError::Operation(
                "source SHA-256 did not match the expected value".into(),
            ));
        }
    }
    report.stage(ProgressStage::Syncing, "Flushing");
    disk.flush()?;
    if verification == VerificationLevel::FullReadback {
        report.stage(ProgressStage::Verifying, "Reading the device back");
        verify_disk(
            &disk,
            receipt.bytes_written,
            &receipt.sha256,
            target.cancel,
            report,
        )?;
    }
    drop(locks);
    disk.rescan();
    Ok(())
}

fn copy_to_disk(
    disk: &Disk,
    mut reader: Box<dyn Read + Send>,
    limit: u64,
    total: Option<u64>,
    path: &Path,
    cancel: &CancellationToken,
    report: &mut Reporter<'_>,
) -> Result<WriteReceipt, HelperError> {
    let chunk = MIB as usize;
    let sector = disk.sector as usize;
    let mut buffer = AlignedBuffer::new(chunk);
    let mut hasher = Sha256::new();
    let mut written = 0u64;
    let mut meter = Meter::new(total.unwrap_or(0));
    let detail = format!("Writing {}", path.display());
    loop {
        check_cancel(cancel)?;
        let bytes = buffer.as_mut_slice();
        let mut filled = 0;
        while filled < chunk {
            match reader.read(&mut bytes[filled..]) {
                Ok(0) => break,
                Ok(count) => filled += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(decode_error(error)),
            }
        }
        if filled == 0 {
            break;
        }
        if written + filled as u64 > limit {
            return Err(HelperError::Operation(
                "decoded image exceeds its declared size or target capacity".into(),
            ));
        }
        hasher.update(&bytes[..filled]);
        // Devices take whole sectors; pad the final partial sector.
        let padded = filled.div_ceil(sector) * sector;
        bytes[filled..padded].fill(0);
        disk.write_at(written, &bytes[..padded])?;
        written += filled as u64;
        if total.is_some() {
            report.bytes(ProgressStage::WritingImage, &mut meter, written, &detail);
        }
        if filled < chunk {
            break;
        }
    }
    if written == 0 {
        return Err(HelperError::Operation("source image was empty".into()));
    }
    Ok(WriteReceipt {
        bytes_written: written,
        sha256: hasher.finalize().into(),
    })
}

/// Hash the first `size` bytes of the device and compare.
fn verify_disk(
    disk: &Disk,
    size: u64,
    expected: &[u8; 32],
    cancel: &CancellationToken,
    report: &mut Reporter<'_>,
) -> Result<(), HelperError> {
    let mut buffer = AlignedBuffer::new(VERIFY_CHUNK);
    let mut meter = Meter::new(size);
    engine::verify_hash(
        size,
        expected,
        buffer.as_mut_slice(),
        u64::from(disk.sector),
        &mut |offset, bytes| disk.read_at(offset, bytes),
        cancel,
        &mut |done| report.bytes(ProgressStage::Verifying, &mut meter, done, "Verifying"),
    )
}

/// `badblocks -w`: write each pattern over the device, then read it back.
fn test_bad_blocks(
    disk: &Disk,
    cancel: &CancellationToken,
    report: &mut Reporter<'_>,
) -> Result<(), HelperError> {
    let mut written = AlignedBuffer::new(BAD_BLOCK_CHUNK);
    let mut read = AlignedBuffer::new(BAD_BLOCK_CHUNK);
    engine::test_bad_blocks(
        disk.size,
        written.as_mut_slice(),
        read.as_mut_slice(),
        RawDisk {
            write_at: &mut |offset, bytes| disk.write_at(offset, bytes),
            read_at: &mut |offset, buffer| disk.read_at(offset, buffer),
            flush: &mut || disk.flush(),
        },
        cancel,
        report,
    )
}

/// The single data partition of a plain formatted drive.
fn data_span(size: u64, sector: u64, scheme: PartitionScheme) -> Result<Span, HelperError> {
    if scheme == PartitionScheme::Mbr && size / sector > u64::from(u32::MAX) {
        return Err(HelperError::Operation(
            "MBR cannot address a disk this large; choose GPT".into(),
        ));
    }
    let tail = if scheme == PartitionScheme::Gpt {
        34 * sector
    } else {
        0
    };
    let end = size.saturating_sub(tail) / MIB * MIB;
    if end <= 2 * MIB {
        return Err(HelperError::Operation("the device is too small".into()));
    }
    Ok(Span {
        offset: MIB,
        size: end - MIB,
    })
}

fn mbr_type(filesystem: FileSystem) -> u8 {
    match filesystem {
        FileSystem::Fat => 0x0e,
        FileSystem::Fat32 => 0x0c,
        _ => 0x07,
    }
}

fn partition(
    span: Span,
    kind: u8,
    name: &'static str,
    attributes: u64,
    active: bool,
) -> PartitionSpec {
    PartitionSpec {
        offset: span.offset,
        size: span.size,
        mbr_type: kind,
        gpt_type: GPT_BASIC_DATA,
        gpt_name: name,
        gpt_attributes: attributes,
        active,
    }
}

/// The label each file system accepts (FAT and exFAT: 11 upper-case
/// characters), as the desktop already prepared it.
fn label_for(filesystem: FileSystem, label: &str) -> String {
    filesystem.volume_label(label)
}

/// Create the file system on a new, hidden volume.
fn format_filesystem(
    volume: &Volume,
    span: Span,
    format: &FormatSpec,
    target: &Target<'_>,
    report: &mut Reporter<'_>,
) -> Result<(), HelperError> {
    report.stage(ProgressStage::Formatting, format.filesystem.as_str());
    let label = label_for(format.filesystem, &format.label);
    if format.filesystem == FileSystem::Fat32 && span.size > LARGE_FAT32 {
        let sector = target.sector();
        let layout = fat32::plan(span.size, sector as u32, format.cluster_size)?;
        let hidden = u32::try_from(span.offset / sector).unwrap_or(0);
        let volume_id = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()) as u32)
            | 1;
        report.log(format!(
            "Large FAT32: {} clusters of {} bytes",
            layout.clusters,
            layout.sectors_per_cluster * layout.bytes_per_sector
        ));
        let lock = VolumeLock::acquire(volume)?;
        let mut aligned = AlignedBuffer::new(MIB as usize);
        fat32::write(
            &layout,
            hidden,
            volume_id,
            &label,
            target.cancel,
            &mut |offset, bytes| {
                let staged = &mut aligned.as_mut_slice()[..bytes.len()];
                staged.copy_from_slice(bytes);
                lock.write_at(offset, staged)
            },
        )?;
        lock.flush()?;
        drop(lock);
        set_label(volume, &label)?;
        return Ok(());
    }
    let mut meter = Meter::new(span.size);
    let detail = format!("Formatting {}", format.filesystem.as_str());
    format::format_volume(
        &volume.name,
        format.filesystem,
        &label,
        format.cluster_size,
        format.quick_format,
        target.identity.removable,
        target.cancel,
        &mut |percent| {
            let done = span.size / 100 * u64::from(percent.min(100));
            report.bytes(ProgressStage::Formatting, &mut meter, done, &detail);
        },
    )
}

/// Set a label once Windows has mounted the new file system.
fn set_label(volume: &Volume, label: &str) -> Result<(), HelperError> {
    let name = wide(&volume.name);
    let label = wide(label);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        // SAFETY: both strings are NUL-terminated.
        if unsafe { SetVolumeLabelW(name.as_ptr(), label.as_ptr()) } != 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(HelperError::Operation(format!(
                "Windows did not mount the new FAT32 volume: {}",
                io::Error::last_os_error()
            )));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}

/// The root of a mounted volume, once Windows answers for it.
fn volume_root(volume: &Volume, cancel: &CancellationToken) -> Result<PathBuf, HelperError> {
    let root = PathBuf::from(&volume.name);
    let deadline = Instant::now() + Duration::from_secs(15);
    while std::fs::read_dir(&root).is_err() {
        check_cancel(cancel)?;
        if Instant::now() >= deadline {
            return Err(HelperError::Operation(
                "Windows did not mount the formatted volume".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(root)
}

fn format_media(
    target: &Target<'_>,
    format: &FormatSpec,
    bad_blocks: bool,
    report: &mut Reporter<'_>,
) -> Result<(), HelperError> {
    let (locks, disk) = target.open(report)?;
    if bad_blocks {
        test_bad_blocks(&disk, target.cancel, report)?;
    }
    check_cancel(target.cancel)?;
    report.stage(ProgressStage::Partitioning, partition_label(format.scheme));
    let span = if format.scheme == PartitionScheme::SuperFloppy {
        Span {
            offset: 0,
            size: disk.size,
        }
    } else {
        data_span(disk.size, target.sector(), format.scheme)?
    };
    let partitions = if format.scheme == PartitionScheme::SuperFloppy {
        Vec::new()
    } else {
        vec![partition(span, mbr_type(format.filesystem), "", 0, false)]
    };
    disk::create_layout(&disk, format.scheme, &partitions)?;
    drop(locks);
    let volume =
        disk::wait_for_volume(disk.number, span.offset, target.cancel).map_err(|error| {
            if format.scheme == PartitionScheme::SuperFloppy {
                HelperError::Operation(
                "Windows mounts super-floppy volumes only on removable media; choose MBR or GPT"
                    .into(),
            )
            } else {
                error
            }
        })?;
    let letter = disk::hide_volume(&volume);
    format_filesystem(&volume, span, format, target, report)?;
    disk::show_volume(&volume, letter);
    Ok(())
}

fn partition_label(scheme: PartitionScheme) -> &'static str {
    match scheme {
        PartitionScheme::Mbr => "MBR",
        PartitionScheme::Gpt => "GPT",
        PartitionScheme::SuperFloppy => "Super floppy",
    }
}

struct MediaOptions<'a> {
    format: &'a FormatSpec,
    verification: VerificationLevel,
    bad_blocks: bool,
    customization: Option<&'a WindowsCustomization>,
}

fn write_windows_media(
    target: &Target<'_>,
    source: &File,
    options: MediaOptions<'_>,
    report: &mut Reporter<'_>,
) -> Result<(), HelperError> {
    let MediaOptions {
        format,
        verification,
        bad_blocks,
        customization,
    } = options;
    let cancel = target.cancel;
    // Everything that can be checked is checked before the first write.
    let mut plan = plan_windows_media(
        source,
        format,
        customization,
        target.identity.size_bytes,
        target.sector(),
        report,
    )?;
    let (needs, layout) = (plan.needs, plan.layout);

    let (locks, disk) = target.open(report)?;
    if bad_blocks {
        test_bad_blocks(&disk, cancel, report)?;
    }
    check_cancel(cancel)?;

    report.stage(ProgressStage::Partitioning, partition_label(format.scheme));
    let gpt = format.scheme == PartitionScheme::Gpt;
    let mut partitions = vec![partition(
        layout.data,
        mbr_type(format.filesystem),
        winmedia::MAIN_PARTITION_NAME,
        0,
        needs.bios_boot,
    )];
    if let Some(span) = layout.uefi_ntfs {
        // MBR: FAT12 until the files are in, then 0xEF.
        partitions.push(partition(
            span,
            0x01,
            winmedia::UEFI_NTFS_NAME,
            winmedia::GPT_NO_DRIVE_LETTER,
            false,
        ));
    }
    disk::create_layout(&disk, format.scheme, &partitions)?;
    drop(locks);
    let data = disk::wait_for_volume(disk.number, layout.data.offset, cancel)?;
    let letter = disk::hide_volume(&data);
    let uefi = match layout.uefi_ntfs {
        Some(span) => {
            let volume = disk::wait_for_volume(disk.number, span.offset, cancel)?;
            disk::hide_volume(&volume);
            Some(volume)
        }
        None => None,
    };
    check_cancel(cancel)?;

    format_filesystem(&data, layout.data, format, target, report)?;
    if let Some(volume) = &uefi {
        format::format_volume(
            &volume.name,
            FileSystem::Fat,
            winmedia::UEFI_NTFS_LABEL,
            None,
            true,
            target.identity.removable,
            cancel,
            &mut |_| {},
        )?;
        let root = volume_root(volume, cancel)?;
        winmedia::write_uefi_ntfs_files(&root)?;
        disk::flush_volume(volume)?;
        report.log("Installed the UEFI:NTFS boot partition.");
    }
    check_cancel(cancel)?;

    report.stage(ProgressStage::ExtractingFiles, "Copying Windows files");
    let root = volume_root(&data, cancel)?;
    let copies = copy_windows_files(&mut plan, &root, cancel, report)?;
    report.stage(ProgressStage::Syncing, "Flushing the copied files");
    disk::flush_volume(&data)?;
    check_cancel(cancel)?;

    if needs.bios_boot {
        report.stage(
            ProgressStage::InstallingBootloader,
            "Writing BIOS boot code",
        );
        install_bios_boot_code(&disk, &data, layout.data.offset, format.filesystem)?;
        report.log("Installed the Windows 7 MBR and the partition boot record.");
    }
    if uefi.is_some() && !gpt {
        disk::set_mbr_type(&disk, 1, 0xef)?;
    }

    if verification != VerificationLevel::None {
        report.stage(ProgressStage::Verifying, "Reading the copied files back");
        // A dismount drops the cache, so the files are read from the device.
        disk::cycle_volume(&data)?;
        let root = volume_root(&data, cancel)?;
        verify_windows_files(&copies, &root, &root, cancel, report)?;
    }
    disk::show_volume(&data, letter);
    Ok(())
}

fn install_bios_boot_code(
    disk: &Disk,
    data: &Volume,
    data_offset: u64,
    filesystem: FileSystem,
) -> Result<(), HelperError> {
    let sector = u64::from(disk.sector);
    let mut first = AlignedBuffer::new(sector as usize);
    disk.read_at(0, first.as_mut_slice())?;
    winmedia::patch_mbr(&mut first.as_mut_slice()[..512])?;
    disk.write_at(0, first.as_slice())?;
    disk.flush()?;

    let start = boot_record_start(data_offset, sector)?;
    let length = (winmedia::BOOT_AREA_BYTES as u64).div_ceil(sector) * sector;
    let lock = VolumeLock::acquire_mounted(data)?;
    let mut area = AlignedBuffer::new(length as usize);
    lock.read_at(0, area.as_mut_slice())?;
    winmedia::patch_boot_record(
        &mut area.as_mut_slice()[..winmedia::BOOT_AREA_BYTES],
        filesystem,
        start,
    )?;
    lock.write_at(0, area.as_slice())?;
    lock.flush()?;
    drop(lock);
    disk.rescan();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_partitions_leave_room_for_the_backup_gpt() {
        let size = 8 * GIB;
        let gpt = data_span(size, 512, PartitionScheme::Gpt).expect("gpt span");
        assert_eq!(gpt.offset, MIB);
        assert!(gpt.end() <= size - 34 * 512);
        assert_eq!(gpt.end() % MIB, 0);
        let mbr = data_span(size, 512, PartitionScheme::Mbr).expect("mbr span");
        assert_eq!(mbr.end(), size);
        assert!(data_span(MIB, 512, PartitionScheme::Mbr).is_err());
        assert!(data_span(4096 * GIB, 512, PartitionScheme::Mbr).is_err());
    }

    #[test]
    fn labels_suit_the_file_system() {
        assert_eq!(label_for(FileSystem::Fat32, "Rufust media"), "RUFUST MEDI");
        assert_eq!(label_for(FileSystem::Fat32, ""), "");
        assert_eq!(label_for(FileSystem::Ntfs, "Rufust media"), "Rufust media");
    }

    #[test]
    fn requests_need_a_physical_drive_target() {
        let mut request = crate::sample_dry_request(PathBuf::from(r"\\.\PhysicalDrive7"));
        validate_request(&request).expect("valid dry-run request");
        request.target.node = PathBuf::from(r"C:\image.img");
        assert!(validate_request(&request).is_err());
    }

    #[test]
    fn ext_and_capture_requests_are_refused() {
        let mut request = crate::sample_dry_request(PathBuf::from(r"\\.\PhysicalDrive7"));
        if let HelperOperation::FormatMedia { format, .. } = &mut request.operation {
            format.filesystem = FileSystem::Ext4;
        }
        assert!(validate_request(&request).is_err());
    }
}
