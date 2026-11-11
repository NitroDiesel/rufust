//! Opening the source image on Windows and turning it into a byte stream:
//! plain files, compressed images decoded in-process by memory-safe Rust
//! decoders, and VHD/VHDX attached read-only through the system's virtual
//! disk service.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr;

use rufust_core::plan::ImageSourceKind;
use rufust_helper_protocol::SourceSpec;
use rufust_platform::windows::{ioctl, wide, Handle};
use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows_sys::Win32::Storage::FileSystem::{
    GetFinalPathNameByHandleW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_READ, VOLUME_NAME_GUID,
};
use windows_sys::Win32::Storage::Vhd::{
    AttachVirtualDisk, GetVirtualDiskInformation, GetVirtualDiskPhysicalPath, OpenVirtualDisk,
    ATTACH_VIRTUAL_DISK_FLAG_NO_DRIVE_LETTER, ATTACH_VIRTUAL_DISK_FLAG_READ_ONLY,
    ATTACH_VIRTUAL_DISK_PARAMETERS, ATTACH_VIRTUAL_DISK_VERSION_1, GET_VIRTUAL_DISK_INFO,
    GET_VIRTUAL_DISK_INFO_PROVIDER_SUBTYPE, GET_VIRTUAL_DISK_INFO_SIZE,
    OPEN_VIRTUAL_DISK_FLAG_NONE, VIRTUAL_DISK_ACCESS_ATTACH_RO, VIRTUAL_DISK_ACCESS_GET_INFO,
    VIRTUAL_DISK_ACCESS_READ, VIRTUAL_STORAGE_TYPE, VIRTUAL_STORAGE_TYPE_DEVICE_VHD,
    VIRTUAL_STORAGE_TYPE_DEVICE_VHDX, VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
};
use windows_sys::Win32::System::Ioctl::IOCTL_DISK_GET_LENGTH_INFO;

use crate::HelperError;

/// Open the selected image, refusing links, and keep others from writing to
/// it while this job runs.
pub(crate) fn bind(source: &SourceSpec) -> Result<File, HelperError> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(&source.path)
        .map_err(|error| {
            HelperError::Operation(format!(
                "could not open {} for reading (is another program writing to it?): {error}",
                source.path.display()
            ))
        })?;
    let metadata = file.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(HelperError::Operation(
            "the source is a link; select the image file itself".into(),
        ));
    }
    if !metadata.is_file() {
        return Err(HelperError::Operation(
            "source is not a regular file".into(),
        ));
    }
    if metadata.len() != source.size_bytes {
        return Err(HelperError::Operation(
            "source image size changed since selection".into(),
        ));
    }
    Ok(file)
}

/// The volume (`\\?\Volume{GUID}\`) holding an open file, if it is local.
pub(crate) fn volume_of(file: &File) -> Option<String> {
    let mut buffer = vec![0u16; 1024];
    // SAFETY: the handle is live and the buffer writable for its length.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            VOLUME_NAME_GUID,
        )
    } as usize;
    if length == 0 || length >= buffer.len() {
        return None;
    }
    let path = String::from_utf16_lossy(&buffer[..length]);
    let end = path.find("}\\")?;
    Some(path[..end + 2].to_owned())
}

/// The full path of an open file, for APIs that take a path.
pub(crate) fn final_path(file: &File) -> Option<std::path::PathBuf> {
    let mut buffer = vec![0u16; 4096];
    // SAFETY: the handle is live and the buffer writable for its length.
    let length = unsafe {
        GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            0,
        )
    } as usize;
    (length != 0 && length < buffer.len())
        .then(|| std::path::PathBuf::from(String::from_utf16_lossy(&buffer[..length])))
}

/// Built-in decoder for a compressed image, chosen by extension as the
/// Linux helper chooses its external tool.
pub(crate) fn decoder_kind(path: &Path) -> Result<&'static str, HelperError> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "gz" => Ok("gzip"),
        "bz2" => Ok("bzip2"),
        "xz" => Ok("xz"),
        "lzma" => Ok("lzma"),
        "zst" | "zstd" => Ok("zstd"),
        "zip" => Ok("zip"),
        _ => Err(HelperError::Operation(
            "this compressed image format is not supported on Windows".into(),
        )),
    }
}

/// A pull-based decoder over the bound source.
pub(crate) fn decoder(source: &File, path: &Path) -> Result<Box<dyn Read + Send>, HelperError> {
    let mut file = source.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let input = BufReader::with_capacity(1024 * 1024, file);
    Ok(match decoder_kind(path)? {
        "gzip" => Box::new(flate2::read::MultiGzDecoder::new(input)),
        "bzip2" => Box::new(bzip2::read::MultiBzDecoder::new(input)),
        "xz" => Box::new(lzma_rust2::XzReader::new(input, true)),
        "lzma" => Box::new(
            lzma_rust2::LzmaReader::new_mem_limit(input, u32::MAX, None)
                .map_err(|error| HelperError::Operation(format!("invalid LZMA image: {error}")))?,
        ),
        "zstd" => Box::new(
            ruzstd::StreamingDecoder::new(input)
                .map_err(|error| HelperError::Operation(format!("invalid zstd image: {error}")))?,
        ),
        _ => zip_entry(source.try_clone()?)?,
    })
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap_or([0; 8]))
}

/// The single file inside a zip archive, decompressed. Archives holding
/// several files are refused rather than concatenated.
pub(crate) fn zip_entry(mut file: File) -> Result<Box<dyn Read + Send>, HelperError> {
    let length = file.metadata()?.len();
    // The end-of-central-directory record sits in the last 64 KiB + 22 bytes.
    let tail_length = length.min(65_557);
    let mut tail = vec![0u8; tail_length as usize];
    file.seek(SeekFrom::Start(length - tail_length))?;
    file.read_exact(&mut tail)?;
    let eocd = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|at| tail[*at..*at + 4] == [0x50, 0x4B, 0x05, 0x06])
        .ok_or_else(|| HelperError::Operation("not a zip archive".into()))?;
    let mut entries = u64::from(u16_at(&tail, eocd + 10));
    let mut directory = u64::from(u32_at(&tail, eocd + 16));
    // Zip64: the locator just before the record points to the real one.
    if (entries == 0xFFFF || directory == 0xFFFF_FFFF) && eocd >= 20 {
        let locator = eocd - 20;
        if tail[locator..locator + 4] == [0x50, 0x4B, 0x06, 0x07] {
            let mut record = [0u8; 56];
            file.seek(SeekFrom::Start(u64_at(&tail, locator + 8)))?;
            file.read_exact(&mut record)?;
            if record[..4] != [0x50, 0x4B, 0x06, 0x06] {
                return Err(HelperError::Operation("damaged zip64 archive".into()));
            }
            entries = u64_at(&record, 32);
            directory = u64_at(&record, 48);
        }
    }
    if entries > 4096 {
        return Err(HelperError::Operation(
            "zip archive has too many entries".into(),
        ));
    }
    file.seek(SeekFrom::Start(directory))?;
    let mut found = None;
    for _ in 0..entries {
        let mut header = [0u8; 46];
        file.read_exact(&mut header)?;
        if header[..4] != [0x50, 0x4B, 0x01, 0x02] {
            return Err(HelperError::Operation("damaged zip directory".into()));
        }
        let method = u16_at(&header, 10);
        let mut compressed = u64::from(u32_at(&header, 20));
        let mut uncompressed = u64::from(u32_at(&header, 24));
        let name_length = usize::from(u16_at(&header, 28));
        let extra_length = usize::from(u16_at(&header, 30));
        let comment_length = i64::from(u16_at(&header, 32));
        let mut offset = u64::from(u32_at(&header, 42));
        let mut name = vec![0u8; name_length];
        file.read_exact(&mut name)?;
        let mut extra = vec![0u8; extra_length];
        file.read_exact(&mut extra)?;
        file.seek(SeekFrom::Current(comment_length))?;
        if name.last() == Some(&b'/') {
            continue;
        }
        // Zip64 extended information replaces saturated fields in order.
        let mut at = 0;
        while at + 4 <= extra.len() {
            let id = u16_at(&extra, at);
            let size = usize::from(u16_at(&extra, at + 2));
            let body = &extra[(at + 4).min(extra.len())..(at + 4 + size).min(extra.len())];
            if id == 1 {
                let mut fields = body.chunks_exact(8).map(|c| u64_at(c, 0));
                if uncompressed == 0xFFFF_FFFF {
                    uncompressed = fields.next().unwrap_or(uncompressed);
                }
                if compressed == 0xFFFF_FFFF {
                    compressed = fields.next().unwrap_or(compressed);
                }
                if offset == 0xFFFF_FFFF {
                    offset = fields.next().unwrap_or(offset);
                }
            }
            at += 4 + size;
        }
        if found.is_some() {
            return Err(HelperError::Operation(
                "the zip archive holds more than one file; extract the image first".into(),
            ));
        }
        found = Some((method, compressed, uncompressed, offset));
    }
    let (method, compressed, _uncompressed, offset) =
        found.ok_or_else(|| HelperError::Operation("the zip archive is empty".into()))?;
    let mut local = [0u8; 30];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut local)?;
    if local[..4] != [0x50, 0x4B, 0x03, 0x04] {
        return Err(HelperError::Operation("damaged zip entry".into()));
    }
    if u16_at(&local, 6) & 1 != 0 {
        return Err(HelperError::Operation(
            "encrypted zip archives are not supported".into(),
        ));
    }
    let skip = i64::from(u16_at(&local, 26)) + i64::from(u16_at(&local, 28));
    file.seek(SeekFrom::Current(skip))?;
    let data = BufReader::with_capacity(1024 * 1024, file).take(compressed);
    match method {
        0 => Ok(Box::new(data)),
        8 => Ok(Box::new(flate2::read::DeflateDecoder::new(data))),
        other => Err(HelperError::Operation(format!(
            "zip compression method {other} is not supported"
        ))),
    }
}

/// A VHD or VHDX attached read-only for as long as this value lives.
pub(crate) struct AttachedVirtualDisk {
    _virtual_disk: Handle,
    pub disk: File,
    pub size: u64,
}

fn storage_type(kind: ImageSourceKind) -> Result<VIRTUAL_STORAGE_TYPE, HelperError> {
    let device = match kind {
        ImageSourceKind::Vhd => VIRTUAL_STORAGE_TYPE_DEVICE_VHD,
        ImageSourceKind::Vhdx => VIRTUAL_STORAGE_TYPE_DEVICE_VHDX,
        _ => return Err(HelperError::Operation("not a virtual disk".into())),
    };
    Ok(VIRTUAL_STORAGE_TYPE {
        DeviceId: device,
        VendorId: VIRTUAL_STORAGE_TYPE_VENDOR_MICROSOFT,
    })
}

fn virtual_disk_error(action: &str, code: u32) -> HelperError {
    HelperError::Operation(format!(
        "could not {action} the virtual disk: {}",
        io::Error::from_raw_os_error(code as i32)
    ))
}

fn open_virtual_disk(
    path: &Path,
    kind: ImageSourceKind,
    access: i32,
) -> Result<Handle, HelperError> {
    let storage = storage_type(kind)?;
    let name = wide(path);
    let mut handle: HANDLE = ptr::null_mut();
    // SAFETY: all pointers are valid for the call; parameters may be null.
    let status = unsafe {
        OpenVirtualDisk(
            &storage,
            name.as_ptr(),
            access,
            OPEN_VIRTUAL_DISK_FLAG_NONE,
            ptr::null(),
            &mut handle,
        )
    };
    if status != ERROR_SUCCESS {
        return Err(virtual_disk_error("open", status));
    }
    Ok(Handle::from_raw(handle))
}

fn info(handle: &Handle, version: i32) -> Result<GET_VIRTUAL_DISK_INFO, HelperError> {
    // SAFETY: zero is a valid starting value for this plain union.
    let mut info: GET_VIRTUAL_DISK_INFO = unsafe { std::mem::zeroed() };
    info.Version = version;
    let mut size = size_of::<GET_VIRTUAL_DISK_INFO>() as u32;
    // SAFETY: `info` is writable for `size` bytes.
    let status =
        unsafe { GetVirtualDiskInformation(handle.raw(), &mut size, &mut info, ptr::null_mut()) };
    if status != ERROR_SUCCESS {
        return Err(virtual_disk_error("inspect", status));
    }
    Ok(info)
}

/// Virtual capacity of a standalone, 512-byte-sector VHD/VHDX, the same
/// subset the Linux backend accepts.
pub(crate) fn virtual_size(path: &Path, kind: ImageSourceKind) -> Result<u64, HelperError> {
    let handle = open_virtual_disk(path, kind, VIRTUAL_DISK_ACCESS_GET_INFO)?;
    checked_size(&handle)
}

fn checked_size(handle: &Handle) -> Result<u64, HelperError> {
    // SAFETY: the union member matches the requested version.
    let subtype = unsafe {
        info(handle, GET_VIRTUAL_DISK_INFO_PROVIDER_SUBTYPE)?
            .Anonymous
            .ProviderSubtype
    };
    if subtype == 4 {
        return Err(HelperError::Operation(
            "differencing virtual disks depend on a parent and are not supported".into(),
        ));
    }
    // SAFETY: the union member matches the requested version.
    let size = unsafe { info(handle, GET_VIRTUAL_DISK_INFO_SIZE)?.Anonymous.Size };
    if size.SectorSize != 512 {
        return Err(HelperError::Operation(format!(
            "virtual disks with {}-byte sectors are not supported",
            size.SectorSize
        )));
    }
    if size.VirtualSize == 0 {
        return Err(HelperError::Operation("the virtual disk is empty".into()));
    }
    Ok(size.VirtualSize)
}

/// Attach the image read-only, without drive letters, and open its disk.
pub(crate) fn attach(
    path: &Path,
    kind: ImageSourceKind,
) -> Result<AttachedVirtualDisk, HelperError> {
    let handle = open_virtual_disk(
        path,
        kind,
        VIRTUAL_DISK_ACCESS_ATTACH_RO | VIRTUAL_DISK_ACCESS_READ | VIRTUAL_DISK_ACCESS_GET_INFO,
    )?;
    let size = checked_size(&handle)?;
    // SAFETY: zero is a valid starting value for this parameter block.
    let mut parameters: ATTACH_VIRTUAL_DISK_PARAMETERS = unsafe { std::mem::zeroed() };
    parameters.Version = ATTACH_VIRTUAL_DISK_VERSION_1;
    // SAFETY: the handle is live and the parameters valid for the call.
    let status = unsafe {
        AttachVirtualDisk(
            handle.raw(),
            ptr::null_mut(),
            ATTACH_VIRTUAL_DISK_FLAG_READ_ONLY | ATTACH_VIRTUAL_DISK_FLAG_NO_DRIVE_LETTER,
            0,
            &parameters,
            ptr::null(),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(virtual_disk_error("attach", status));
    }
    let mut path_buffer = vec![0u16; 512];
    let mut bytes = (path_buffer.len() * 2) as u32;
    // SAFETY: the buffer is writable for `bytes` bytes.
    let status =
        unsafe { GetVirtualDiskPhysicalPath(handle.raw(), &mut bytes, path_buffer.as_mut_ptr()) };
    if status != ERROR_SUCCESS {
        return Err(virtual_disk_error("locate", status));
    }
    let end = path_buffer
        .iter()
        .position(|c| *c == 0)
        .unwrap_or(path_buffer.len());
    let physical = String::from_utf16_lossy(&path_buffer[..end]);
    let disk = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | windows_sys::Win32::Storage::FileSystem::FILE_SHARE_WRITE)
        .open(&physical)
        .map_err(|error| HelperError::Operation(format!("could not read {physical}: {error}")))?;
    let mut length = [0u8; 8];
    ioctl(&disk, IOCTL_DISK_GET_LENGTH_INFO, &[], &mut length)
        .map_err(|error| HelperError::Operation(format!("could not size {physical}: {error}")))?;
    if u64::from_le_bytes(length) != size {
        return Err(HelperError::Operation(
            "the attached virtual disk size differs from its metadata".into(),
        ));
    }
    Ok(AttachedVirtualDisk {
        _virtual_disk: handle,
        disk,
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct Temp(std::path::PathBuf);

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn temp(name: &str, bytes: &[u8]) -> Temp {
        let path =
            std::env::temp_dir().join(format!("rufust-source-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).expect("fixture");
        Temp(path)
    }

    fn read_all(mut reader: Box<dyn Read + Send>) -> Vec<u8> {
        let mut output = Vec::new();
        reader.read_to_end(&mut output).expect("decode");
        output
    }

    fn stored_zip(name: &str, body: &[u8], extra_file: bool) -> Vec<u8> {
        let mut zip = Vec::new();
        let mut central = Vec::new();
        let mut files = vec![(name, body)];
        if extra_file {
            files.push(("second.img", body));
        }
        for (name, body) in &files {
            let offset = zip.len() as u32;
            zip.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04, 20, 0, 0, 0, 0, 0]);
            zip.extend_from_slice(&[0; 8]);
            zip.extend_from_slice(&(body.len() as u32).to_le_bytes());
            zip.extend_from_slice(&(body.len() as u32).to_le_bytes());
            zip.extend_from_slice(&(name.len() as u16).to_le_bytes());
            zip.extend_from_slice(&[0, 0]);
            zip.extend_from_slice(name.as_bytes());
            zip.extend_from_slice(body);
            central.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02, 20, 0, 20, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&[0; 8]);
            central.extend_from_slice(&(body.len() as u32).to_le_bytes());
            central.extend_from_slice(&(body.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0; 12]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let directory = zip.len() as u32;
        zip.extend_from_slice(&central);
        zip.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06, 0, 0, 0, 0]);
        zip.extend_from_slice(&(files.len() as u16).to_le_bytes());
        zip.extend_from_slice(&(files.len() as u16).to_le_bytes());
        zip.extend_from_slice(&(central.len() as u32).to_le_bytes());
        zip.extend_from_slice(&directory.to_le_bytes());
        zip.extend_from_slice(&[0, 0]);
        zip
    }

    #[test]
    fn decodes_gzip_and_zip_images() {
        let body = b"rufust image bytes".repeat(1000);
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(&body).expect("gzip fixture");
        let gz = temp("image.img.gz", &gzip.finish().expect("gzip fixture"));
        let file = File::open(&gz.0).expect("open");
        assert_eq!(read_all(decoder(&file, &gz.0).expect("gzip decoder")), body);

        let zip = temp("image.zip", &stored_zip("disk/image.img", &body, false));
        let file = File::open(&zip.0).expect("open");
        assert_eq!(read_all(decoder(&file, &zip.0).expect("zip decoder")), body);

        let two = temp("two.zip", &stored_zip("one.img", &body, true));
        let file = File::open(&two.0).expect("open");
        assert!(decoder(&file, &two.0).is_err());
    }

    #[test]
    fn unknown_compression_is_refused() {
        assert!(decoder_kind(Path::new("image.img.Z")).is_err());
        assert_eq!(decoder_kind(Path::new("IMAGE.XZ")).ok(), Some("xz"));
        assert_eq!(decoder_kind(Path::new("a.zstd")).ok(), Some("zstd"));
    }

    #[test]
    fn binding_checks_the_selected_size() {
        let image = temp("bound.img", &[0u8; 4096]);
        let mut spec = SourceSpec {
            path: image.0.clone(),
            kind: ImageSourceKind::Raw,
            size_bytes: 4096,
            decompressed_size_bytes: None,
            expected_sha256: None,
        };
        let bound = bind(&spec).expect("bind");
        assert!(volume_of(&bound).is_some_and(|volume| volume.starts_with(r"\\?\Volume{")));
        drop(bound);
        spec.size_bytes = 512;
        assert!(bind(&spec).is_err());
    }
}
