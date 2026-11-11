//! Windows disk discovery through the storage IOCTLs.
//!
//! Disks are enumerated through their device interfaces, so a disk's
//! interface path (which embeds the PnP instance ID) stands in for the Linux
//! sysfs path as its stable identity. `\\.\PhysicalDriveN` is the node and
//! only a display name. Every query here opens devices with no access rights,
//! which an unelevated desktop process may do; destructive work stays in the
//! elevated helper.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::io::{AsRawHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::ptr;

use rufust_core::capability::{Capability, CapabilityReport, CapabilityState, MissingRequirement};
use rufust_core::device::{BlockDevice, DeviceRisk};
use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
    SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_MORE_DATA, ERROR_WRITE_PROTECT, HANDLE, INVALID_HANDLE_VALUE,
    MAX_PATH,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FindFirstVolumeW, FindNextVolumeW, FindVolumeClose,
    GetVolumeNameForVolumeMountPointW, GetVolumePathNameW, GetVolumePathNamesForVolumeNameW,
    QueryDosDeviceW, FILE_SHARE_READ, FILE_SHARE_WRITE, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
    OPEN_EXISTING,
};
use windows_sys::Win32::System::Ioctl::{
    GUID_DEVINTERFACE_DISK, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_IS_WRITABLE,
    IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_STORAGE_QUERY_PROPERTY,
};
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows_sys::Win32::System::IO::DeviceIoControl;

use super::{DeviceIdentity, PlatformError};

/// An owned Win32 handle, closed on drop.
#[derive(Debug)]
pub struct Handle(HANDLE);

impl Handle {
    /// Take ownership of a live handle that nothing else will close.
    pub fn from_raw(handle: HANDLE) -> Self {
        Self(handle)
    }

    pub fn raw(&self) -> HANDLE {
        self.0
    }
}

impl AsRawHandle for Handle {
    fn as_raw_handle(&self) -> RawHandle {
        self.0
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

// SAFETY: kernel handles may be used from any thread.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

/// NUL-terminated UTF-16 for a Win32 call.
pub fn wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value.as_ref().encode_wide().chain(Some(0)).collect()
}

fn from_wide(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    OsString::from_wide(&buffer[..end])
        .to_string_lossy()
        .into_owned()
}

/// Open a device or volume path with exactly the requested access.
pub fn open(path: impl AsRef<OsStr>, access: u32, flags: u32) -> io::Result<Handle> {
    let name = wide(path);
    // SAFETY: `name` is NUL-terminated and outlives the call.
    let handle = unsafe {
        CreateFileW(
            name.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            ptr::null(),
            OPEN_EXISTING,
            flags,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    Ok(Handle(handle))
}

/// One buffered `DeviceIoControl`; returns the number of output bytes.
pub fn ioctl(
    device: &impl AsRawHandle,
    code: u32,
    input: &[u8],
    output: &mut [u8],
) -> io::Result<usize> {
    let mut returned = 0u32;
    // SAFETY: the handle is live for the borrow, and both buffers are valid
    // for their stated lengths.
    let ok = unsafe {
        DeviceIoControl(
            device.as_raw_handle(),
            code,
            if input.is_empty() {
                ptr::null()
            } else {
                input.as_ptr().cast()
            },
            input.len() as u32,
            if output.is_empty() {
                ptr::null_mut()
            } else {
                output.as_mut_ptr().cast()
            },
            output.len() as u32,
            &mut returned,
            ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(returned as usize)
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    bytes
        .get(offset..offset + 4)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u32::from_le_bytes)
}

fn u64_at(bytes: &[u8], offset: usize) -> u64 {
    bytes
        .get(offset..offset + 8)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u64::from_le_bytes)
}

/// `\\.\PhysicalDriveN` for disk number N.
pub fn physical_drive_path(number: u32) -> PathBuf {
    PathBuf::from(format!(r"\\.\PhysicalDrive{number}"))
}

/// The disk number of a `\\.\PhysicalDriveN` node.
pub fn disk_number(node: &Path) -> Option<u32> {
    let text = node.to_str()?;
    let digits = text
        .strip_prefix(r"\\.\PhysicalDrive")
        .or_else(|| text.strip_prefix(r"\\.\PHYSICALDRIVE"))?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// A region of a disk that a volume occupies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Extent {
    pub disk: u32,
    pub offset: u64,
    pub length: u64,
}

/// A mounted (or mountable) volume and where it lives.
#[derive(Clone, Debug)]
pub struct Volume {
    /// `\\?\Volume{GUID}\`, with the trailing backslash.
    pub name: String,
    /// The NT device, such as `\Device\HarddiskVolume3`.
    pub device: String,
    /// Drive letters and folder mount points.
    pub mount_points: Vec<PathBuf>,
    pub extents: Vec<Extent>,
}

impl Volume {
    /// `\\.\Volume{GUID}` (no trailing backslash) opens the volume itself.
    pub fn device_path(&self) -> String {
        format!(
            r"\\.\{}",
            self.name.trim_start_matches(r"\\?\").trim_end_matches('\\')
        )
    }

    pub fn on_disk(&self, disk: u32) -> bool {
        self.extents.iter().any(|extent| extent.disk == disk)
    }
}

/// Every volume the volume manager currently knows.
pub fn volumes() -> Vec<Volume> {
    let mut volumes = Vec::new();
    let mut name = [0u16; MAX_PATH as usize + 1];
    // SAFETY: the buffer is writable for its stated length.
    let find = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
    if find == INVALID_HANDLE_VALUE {
        return volumes;
    }
    loop {
        let volume = from_wide(&name);
        if let Some(entry) = describe_volume(&volume) {
            volumes.push(entry);
        }
        // SAFETY: `find` is a live search handle and the buffer is writable.
        if unsafe { FindNextVolumeW(find, name.as_mut_ptr(), name.len() as u32) } == 0 {
            break;
        }
    }
    // SAFETY: closes the search handle opened above.
    unsafe { FindVolumeClose(find) };
    volumes
}

fn describe_volume(name: &str) -> Option<Volume> {
    let guid = name.strip_prefix(r"\\?\")?.trim_end_matches('\\');
    let mut volume = Volume {
        name: name.to_owned(),
        device: String::new(),
        mount_points: mount_points(name),
        extents: Vec::new(),
    };
    let mut device = [0u16; 512];
    let query = wide(guid);
    // SAFETY: `query` is NUL-terminated and `device` writable.
    if unsafe { QueryDosDeviceW(query.as_ptr(), device.as_mut_ptr(), device.len() as u32) } != 0 {
        volume.device = from_wide(&device);
    }
    // Floppies and empty card readers have no extents; they are not disks.
    let handle = open(volume.device_path(), 0, 0).ok()?;
    let mut buffer = vec![0u8; 8 + 24 * 32];
    let length = ioctl(
        &handle,
        IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
        &[],
        &mut buffer,
    )
    .ok()?;
    let count = u32_at(&buffer, 0) as usize;
    for index in 0..count {
        let at = 8 + index * 24;
        if at + 24 > length {
            break;
        }
        volume.extents.push(Extent {
            disk: u32_at(&buffer, at),
            offset: u64_at(&buffer, at + 8),
            length: u64_at(&buffer, at + 16),
        });
    }
    Some(volume)
}

fn mount_points(volume: &str) -> Vec<PathBuf> {
    let name = wide(volume);
    let mut buffer = vec![0u16; 1024];
    loop {
        let mut needed = 0u32;
        // SAFETY: the buffer is writable for its stated length.
        let ok = unsafe {
            GetVolumePathNamesForVolumeNameW(
                name.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut needed,
            )
        };
        if ok != 0 {
            break;
        }
        // SAFETY: reads this thread's last error.
        if unsafe { GetLastError() } == ERROR_MORE_DATA && (needed as usize) > buffer.len() {
            buffer.resize(needed as usize, 0);
            continue;
        }
        return Vec::new();
    }
    buffer
        .split(|c| *c == 0)
        .take_while(|part| !part.is_empty())
        .map(|part| PathBuf::from(OsString::from_wide(part)))
        .collect()
}

/// The volume GUID path (`\\?\Volume{GUID}\`) that holds `path`.
pub fn volume_of_path(path: &Path) -> Option<String> {
    let path = wide(path);
    let mut root = [0u16; MAX_PATH as usize + 1];
    // SAFETY: both buffers are valid for their stated lengths.
    if unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), root.len() as u32) } == 0 {
        return None;
    }
    let mut name = [0u16; MAX_PATH as usize + 1];
    // SAFETY: `root` is NUL-terminated and `name` is writable.
    if unsafe {
        GetVolumeNameForVolumeMountPointW(root.as_ptr(), name.as_mut_ptr(), name.len() as u32)
    } == 0
    {
        return None;
    }
    Some(from_wide(&name))
}

/// The disks that store `path`, or nothing for network and unknown paths.
pub fn disks_holding_path(path: &Path) -> Vec<u32> {
    let Some(volume) = volume_of_path(path) else {
        return Vec::new();
    };
    describe_volume(&volume)
        .map(|volume| volume.extents.iter().map(|extent| extent.disk).collect())
        .unwrap_or_default()
}

fn disk_interfaces() -> io::Result<Vec<String>> {
    // SAFETY: the GUID is a static and the returned set is destroyed below.
    let set = unsafe {
        SetupDiGetClassDevsW(
            &GUID_DEVINTERFACE_DISK,
            ptr::null(),
            ptr::null_mut(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    };
    if set == INVALID_HANDLE_VALUE as isize {
        return Err(io::Error::last_os_error());
    }
    let mut paths = Vec::new();
    for index in 0.. {
        let mut data = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            InterfaceClassGuid: GUID_DEVINTERFACE_DISK,
            Flags: 0,
            Reserved: 0,
        };
        // SAFETY: `set` is live and `data` is initialized with its size.
        if unsafe {
            SetupDiEnumDeviceInterfaces(set, ptr::null(), &GUID_DEVINTERFACE_DISK, index, &mut data)
        } == 0
        {
            // ERROR_NO_MORE_ITEMS ends the list; any other failure ends it too.
            break;
        }
        // The detail record is a u32 size followed by the path.
        let mut buffer = vec![0u32; 1024];
        let detail = buffer
            .as_mut_ptr()
            .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        // SAFETY: the buffer is u32-aligned and large enough for the header.
        unsafe { (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32 };
        // SAFETY: the buffer is valid for its byte length.
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                set,
                &data,
                detail,
                (buffer.len() * 4) as u32,
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if ok == 0 {
            continue;
        }
        // SAFETY: the path starts after the u32 size field.
        let path = unsafe {
            std::slice::from_raw_parts(
                buffer.as_ptr().cast::<u8>().add(4).cast::<u16>(),
                (buffer.len() * 4 - 4) / 2,
            )
        };
        paths.push(from_wide(path));
    }
    // SAFETY: destroys the set created above.
    unsafe { SetupDiDestroyDeviceInfoList(set) };
    paths.sort();
    Ok(paths)
}

/// STORAGE_BUS_TYPE values that identify the transport.
fn transport_name(bus: u32) -> &'static str {
    match bus {
        7 => "usb",
        17 => "nvme",
        12 | 13 => "mmc",
        3 | 11 => "ata",
        1 | 10 => "scsi",
        // BusTypeVirtual and BusTypeFileBackedVirtual: VHDs and RAM disks.
        14 | 15 => "loop",
        _ => "unknown",
    }
}

fn descriptor_string(bytes: &[u8], offset: u32) -> String {
    let offset = offset as usize;
    if offset == 0 || offset >= bytes.len() {
        return String::new();
    }
    let end = bytes[offset..]
        .iter()
        .position(|b| *b == 0)
        .map_or(bytes.len(), |end| offset + end);
    String::from_utf8_lossy(&bytes[offset..end])
        .trim()
        .to_owned()
}

fn identify(interface: &str) -> Option<DeviceIdentity> {
    let handle = open(interface, 0, 0).ok()?;
    let mut number = [0u8; 12];
    ioctl(&handle, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], &mut number).ok()?;
    let disk = u32_at(&number, 4);

    // STORAGE_PROPERTY_QUERY { StorageDeviceProperty, PropertyStandardQuery }.
    let query = [0u8; 12];
    let mut descriptor = vec![0u8; 4096];
    let length = ioctl(
        &handle,
        IOCTL_STORAGE_QUERY_PROPERTY,
        &query,
        &mut descriptor,
    )
    .ok()?;
    let descriptor = &descriptor[..length];
    let removable = descriptor.get(10).is_some_and(|value| *value != 0);
    let vendor = descriptor_string(descriptor, u32_at(descriptor, 12));
    let model = descriptor_string(descriptor, u32_at(descriptor, 16));
    let serial = descriptor_string(descriptor, u32_at(descriptor, 24));
    let bus = u32_at(descriptor, 28);

    // DISK_GEOMETRY_EX: BytesPerSector at 20, DiskSize at 24. Fails for an
    // empty card reader, which is not a usable target.
    let mut geometry = [0u8; 256];
    ioctl(
        &handle,
        IOCTL_DISK_GET_DRIVE_GEOMETRY_EX,
        &[],
        &mut geometry,
    )
    .ok()?;
    let logical_sector_size = u32_at(&geometry, 20);
    let size_bytes = u64_at(&geometry, 24);

    let read_only = matches!(
        ioctl(&handle, IOCTL_DISK_IS_WRITABLE, &[], &mut []),
        Err(error) if error.raw_os_error() == Some(ERROR_WRITE_PROTECT as i32)
    );
    Some(DeviceIdentity {
        node: physical_drive_path(disk),
        sysfs_path: PathBuf::from(interface),
        kernel_name: format!("PhysicalDrive{disk}"),
        major: 0,
        minor: disk,
        size_bytes,
        logical_sector_size,
        model,
        vendor,
        serial,
        transport: transport_name(bus).to_owned(),
        removable,
        read_only,
    })
}

pub(super) fn discover_devices(show_fixed: bool) -> Result<Vec<DeviceIdentity>, PlatformError> {
    let interfaces = disk_interfaces()
        .map_err(|error| PlatformError::Io(format!("could not list disks: {error}")))?;
    let mut devices = interfaces
        .iter()
        .filter_map(|interface| identify(interface))
        .filter(|device| show_fixed || device.removable)
        // Like upstream, skip tiny (floppy-like) devices under 8 MiB.
        .filter(|device| device.size_bytes >= 8 * 1024 * 1024)
        .filter(|device| {
            device.logical_sector_size >= 512 && device.logical_sector_size.is_power_of_two()
        })
        .collect::<Vec<_>>();
    devices.sort_by_key(|device| device.minor);
    Ok(devices)
}

/// The volumes Windows itself runs from.
struct SystemVolumes {
    /// The volume holding the Windows directory.
    windows: Option<String>,
    /// The NT device of the boot (system) partition, such as the ESP.
    system_partition: Option<String>,
}

impl SystemVolumes {
    fn read() -> Self {
        let mut directory = [0u16; MAX_PATH as usize + 1];
        // SAFETY: the buffer is writable for its stated length.
        let length =
            unsafe { GetWindowsDirectoryW(directory.as_mut_ptr(), directory.len() as u32) };
        let windows = (length != 0)
            .then(|| volume_of_path(Path::new(&from_wide(&directory))))
            .flatten();
        Self {
            windows,
            system_partition: registry_string(r"SYSTEM\Setup", "SystemPartition"),
        }
    }
}

/// A `REG_SZ` value from HKEY_LOCAL_MACHINE.
pub fn registry_string(key: &str, value: &str) -> Option<String> {
    let key = wide(key);
    let value = wide(value);
    let mut buffer = [0u16; 512];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: all strings are NUL-terminated and the buffer size is in bytes.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    (status == 0).then(|| from_wide(&buffer))
}

/// Risks that only the volume table shows.
fn volume_risks(disk: u32, volumes: &[Volume], system: &SystemVolumes) -> Vec<DeviceRisk> {
    let on_disk = volumes
        .iter()
        .filter(|volume| volume.on_disk(disk))
        .collect::<Vec<_>>();
    let mut risks = Vec::new();
    if on_disk
        .iter()
        .any(|volume| system.windows.as_deref() == Some(volume.name.as_str()))
    {
        risks.push(DeviceRisk::ContainsRoot);
    }
    if on_disk.iter().any(|volume| {
        !volume.device.is_empty()
            && system
                .system_partition
                .as_deref()
                .is_some_and(|device| device.eq_ignore_ascii_case(&volume.device))
    }) {
        risks.push(DeviceRisk::ContainsBoot);
    }
    if on_disk.iter().any(|volume| {
        volume
            .mount_points
            .iter()
            .any(|mount| mount.join("pagefile.sys").exists() || mount.join("swapfile.sys").exists())
    }) {
        risks.push(DeviceRisk::ContainsSwap);
    }
    if !risks.contains(&DeviceRisk::ContainsRoot)
        && on_disk.iter().any(|volume| !volume.mount_points.is_empty())
    {
        risks.push(DeviceRisk::MountedChildren);
    }
    // A volume that spans several disks (a dynamic or Storage Spaces volume).
    if on_disk
        .iter()
        .any(|volume| volume.extents.iter().any(|extent| extent.disk != disk))
    {
        risks.push(DeviceRisk::ActiveVolumeMember);
    }
    risks
}

/// Map a discovered disk into a [`BlockDevice`] with its Windows risks.
pub fn to_block_device(identity: &DeviceIdentity, volumes: &[Volume]) -> BlockDevice {
    let system = SystemVolumes::read();
    block_device_with(identity, volumes, &system)
}

fn block_device_with(
    identity: &DeviceIdentity,
    volumes: &[Volume],
    system: &SystemVolumes,
) -> BlockDevice {
    let mut device = identity.to_block_device(&[]);
    for risk in volume_risks(identity.minor, volumes, system) {
        if !device.risks.contains(&risk) {
            device.risks.push(risk);
        }
    }
    device
}

pub(super) fn list_block_devices(show_fixed: bool) -> Result<Vec<BlockDevice>, PlatformError> {
    let identities = discover_devices(show_fixed)?;
    let volumes = volumes();
    let system = SystemVolumes::read();
    Ok(identities
        .iter()
        .map(|identity| block_device_with(identity, &volumes, &system))
        .collect())
}

pub(super) fn path_is_on_block_device(path: &Path, device: &BlockDevice) -> bool {
    let Some(disk) = disk_number(&device.node) else {
        return false;
    };
    disks_holding_path(path).contains(&disk)
}

/// A cheap fingerprint of attached disks and volumes for hot-plug polling.
pub fn device_snapshot() -> io::Result<Vec<OsString>> {
    let mut names = disk_interfaces()?
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
    let mut name = [0u16; MAX_PATH as usize + 1];
    // SAFETY: the buffer is writable for its stated length.
    let find = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
    if find != INVALID_HANDLE_VALUE {
        loop {
            names.push(OsString::from(from_wide(&name)));
            // SAFETY: `find` is live and the buffer writable.
            if unsafe { FindNextVolumeW(find, name.as_mut_ptr(), name.len() as u32) } == 0 {
                break;
            }
        }
        // SAFETY: closes the search handle.
        unsafe { FindVolumeClose(find) };
    }
    names.sort_unstable();
    Ok(names)
}

fn system_file(name: &str) -> bool {
    let mut directory = [0u16; MAX_PATH as usize + 1];
    // SAFETY: the buffer is writable for its stated length.
    let length = unsafe { GetWindowsDirectoryW(directory.as_mut_ptr(), directory.len() as u32) };
    length != 0
        && Path::new(&from_wide(&directory))
            .join("System32")
            .join(name)
            .is_file()
}

fn unavailable(id: &str, explanation: &str) -> CapabilityState {
    CapabilityState::Unavailable {
        missing: vec![MissingRequirement {
            id: id.to_owned(),
            explanation: explanation.to_owned(),
            remedy: None,
        }],
    }
}

/// Windows capabilities: formatting uses the system's own file system
/// providers, and decoders, UEFI:NTFS, and boot records are built in.
pub(super) fn probe_capabilities() -> CapabilityReport {
    let mut report = CapabilityReport::new();
    let formatter = if system_file("fmifs.dll") {
        CapabilityState::Available
    } else {
        unavailable(
            "fmifs.dll",
            "the Windows format provider (fmifs.dll) is missing",
        )
    };
    for capability in [
        Capability::FormatFat,
        Capability::FormatFat32,
        Capability::FormatExfat,
        Capability::FormatNtfs,
        Capability::FormatUdf,
    ] {
        report.set(capability, formatter.clone());
    }
    for capability in [
        Capability::FormatExt2,
        Capability::FormatExt3,
        Capability::FormatExt4,
    ] {
        report.set(
            capability,
            unavailable(
                "windows",
                "ext2/3/4 creation is not available on Windows yet",
            ),
        );
    }
    report.set(
        Capability::FormatRefs,
        unavailable(
            "windows-edition",
            "ReFS creation needs Windows Pro for Workstations or Enterprise and is not supported",
        ),
    );
    report.set(Capability::RawImageWrite, CapabilityState::Available);
    report.set(Capability::CompressedImageWrite, CapabilityState::Available);
    report.set(
        Capability::VirtualDiskWrite,
        if system_file("virtdisk.dll") {
            CapabilityState::Available
        } else {
            unavailable(
                "virtdisk.dll",
                "the Windows virtual disk service is missing",
            )
        },
    );
    report.set(Capability::IsoExtraction, CapabilityState::Available);
    for capability in [
        Capability::ImageCaptureDd,
        Capability::ImageCaptureVhd,
        Capability::ImageCaptureVhdx,
    ] {
        report.set(
            capability,
            unavailable("capture", "drive capture is not implemented yet"),
        );
    }
    report.set(
        Capability::ImageCaptureFfu,
        unavailable("ffu", "FFU apply and capture are not implemented"),
    );
    report.set(
        Capability::FreeDos,
        unavailable("assets/freedos", "FreeDOS assets not packaged yet"),
    );
    report.set(
        Capability::Syslinux,
        unavailable("syslinux", "Syslinux installation is not available yet"),
    );
    report.set(
        Capability::Grub,
        unavailable("grub", "GRUB installation is not available yet"),
    );
    report.set(Capability::UefiNtfs, CapabilityState::Available);
    report.set(
        Capability::LinuxPersistence,
        unavailable(
            "persistence-layout",
            "persistent partition creation is not implemented in this release",
        ),
    );
    report.set(
        Capability::WindowsInstallerCustomization,
        CapabilityState::Available,
    );
    report.set(
        Capability::WindowsToGo,
        unavailable("experimental", "Windows To Go is not available yet"),
    );
    report.set(Capability::WindowsIsoDownload, CapabilityState::Available);
    report.set(Capability::UefiShellDownload, CapabilityState::Available);
    report.set(Capability::ImageChecksums, CapabilityState::Available);
    report.set(
        Capability::UefiMediaValidation,
        unavailable(
            "uefi-validation-payload",
            "verified UEFI validation payload is not packaged",
        ),
    );
    // The bad-block test is built into the disk engine.
    report.set(Capability::BadBlocksCheck, CapabilityState::Available);
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_physical_drive_nodes() {
        assert_eq!(disk_number(Path::new(r"\\.\PhysicalDrive3")), Some(3));
        assert_eq!(disk_number(Path::new(r"\\.\PhysicalDrive12")), Some(12));
        assert_eq!(disk_number(Path::new(r"\\.\PhysicalDrive")), None);
        assert_eq!(disk_number(Path::new(r"\\.\PhysicalDrive1x")), None);
        assert_eq!(disk_number(Path::new(r"C:\")), None);
        assert_eq!(physical_drive_path(4), PathBuf::from(r"\\.\PhysicalDrive4"));
    }

    fn volume(name: &str, disks: &[u32], mount: Option<&str>) -> Volume {
        Volume {
            name: format!(r"\\?\Volume{{{name}}}\"),
            device: format!(r"\Device\HarddiskVolume-{name}"),
            mount_points: mount.map(PathBuf::from).into_iter().collect(),
            extents: disks
                .iter()
                .map(|disk| Extent {
                    disk: *disk,
                    offset: 1 << 20,
                    length: 1 << 30,
                })
                .collect(),
        }
    }

    #[test]
    fn volume_table_marks_system_and_mounted_disks() {
        let volumes = [
            volume("os", &[0], Some(r"Z:\no-such-dir\")),
            volume("esp", &[0], None),
            volume("usb", &[2], Some(r"Y:\no-such-dir\")),
            volume("span", &[3, 4], None),
        ];
        let system = SystemVolumes {
            windows: Some(volumes[0].name.clone()),
            system_partition: Some(volumes[1].device.to_ascii_uppercase()),
        };
        let os = volume_risks(0, &volumes, &system);
        assert!(os.contains(&DeviceRisk::ContainsRoot));
        assert!(os.contains(&DeviceRisk::ContainsBoot));
        assert!(!os.contains(&DeviceRisk::MountedChildren));
        assert_eq!(
            volume_risks(2, &volumes, &system),
            [DeviceRisk::MountedChildren]
        );
        assert_eq!(
            volume_risks(3, &volumes, &system),
            [DeviceRisk::ActiveVolumeMember]
        );
        assert!(volume_risks(5, &volumes, &system).is_empty());
    }

    #[test]
    fn descriptor_strings_are_trimmed_and_bounded() {
        let mut bytes = vec![0u8; 64];
        bytes[40..48].copy_from_slice(b"  Kingst");
        assert_eq!(descriptor_string(&bytes, 40), "Kingst");
        assert_eq!(descriptor_string(&bytes, 0), "");
        assert_eq!(descriptor_string(&bytes, 200), "");
    }

    #[test]
    fn discovery_finds_and_protects_the_system_disk() {
        // Enumeration needs no administrator rights, and every Windows host
        // has the disk it runs from.
        let devices = list_block_devices(true).expect("disk enumeration");
        for device in &devices {
            assert!(device.node.is_absolute());
            assert!(device.fingerprint.canonical_sysfs_path.is_absolute());
            assert!(device.fingerprint.size_bytes > 0);
        }
        assert!(
            devices
                .iter()
                .any(|device| device.has_risk(DeviceRisk::ContainsRoot)),
            "the Windows disk must be marked: {devices:#?}"
        );
    }
}
