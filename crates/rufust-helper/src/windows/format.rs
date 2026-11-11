//! File system creation through Windows' own providers (`fmifs.dll`
//! `FormatEx`), as upstream Rufus does when VDS is not used.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

use rufust_core::plan::FileSystem;
use rufust_core::progress::CancellationToken;
use rufust_platform::windows::wide;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

use crate::HelperError;

/// FormatEx callback commands (`FILE_SYSTEM_CALLBACK_COMMAND`).
const FCC_PROGRESS: u32 = 0x00;
const FCC_INCOMPATIBLE_FILE_SYSTEM: u32 = 0x03;
const FCC_ACCESS_DENIED: u32 = 0x06;
const FCC_MEDIA_WRITE_PROTECTED: u32 = 0x07;
const FCC_VOLUME_IN_USE: u32 = 0x08;
const FCC_CANT_QUICK_FORMAT: u32 = 0x09;
const FCC_DONE: u32 = 0x0B;
const FCC_BAD_LABEL: u32 = 0x0C;
const FCC_CLUSTER_SIZE_TOO_SMALL: u32 = 0x10;
const FCC_CLUSTER_SIZE_TOO_BIG: u32 = 0x11;
const FCC_VOLUME_TOO_SMALL: u32 = 0x12;
const FCC_VOLUME_TOO_BIG: u32 = 0x13;
const FCC_NO_MEDIA_IN_DRIVE: u32 = 0x14;
const FCC_DEVICE_NOT_READY: u32 = 0x18;
const FCC_ALIGNMENT_VIOLATION: u32 = 0x24;

/// `MEDIA_TYPE` values FormatEx accepts.
const REMOVABLE_MEDIA: u32 = 11;
const FIXED_MEDIA: u32 = 12;

type Callback = unsafe extern "system" fn(command: u32, action: u32, data: *mut u8) -> u8;
type FormatEx = unsafe extern "system" fn(
    root: *const u16,
    media: u32,
    file_system: *const u16,
    label: *const u16,
    quick: i32,
    cluster_size: u32,
    callback: Callback,
);

// FormatEx's callback has no context argument, so one format runs at a time.
static FORMAT_LOCK: Mutex<()> = Mutex::new(());
static CANCELLED: AtomicBool = AtomicBool::new(false);
static SUCCEEDED: AtomicBool = AtomicBool::new(false);
static FAILURE: AtomicU32 = AtomicU32::new(0);
static PERCENT: AtomicU32 = AtomicU32::new(0);
static CANCEL_TOKEN: Mutex<Option<CancellationToken>> = Mutex::new(None);

unsafe extern "system" fn callback(command: u32, _action: u32, data: *mut u8) -> u8 {
    let cancelled = CANCEL_TOKEN
        .lock()
        .ok()
        .and_then(|token| token.as_ref().map(CancellationToken::is_requested))
        .unwrap_or(false);
    match command {
        FCC_PROGRESS if !data.is_null() => {
            // SAFETY: FormatEx passes a pointer to the percentage.
            PERCENT.store(
                unsafe { data.cast::<u32>().read_unaligned() },
                Ordering::Relaxed,
            );
        }
        FCC_DONE if !data.is_null() => {
            // SAFETY: FormatEx passes a pointer to a BOOLEAN result.
            SUCCEEDED.store(unsafe { *data } != 0, Ordering::Relaxed);
        }
        FCC_INCOMPATIBLE_FILE_SYSTEM
        | FCC_ACCESS_DENIED
        | FCC_MEDIA_WRITE_PROTECTED
        | FCC_VOLUME_IN_USE
        | FCC_CANT_QUICK_FORMAT
        | FCC_BAD_LABEL
        | FCC_CLUSTER_SIZE_TOO_SMALL
        | FCC_CLUSTER_SIZE_TOO_BIG
        | FCC_VOLUME_TOO_SMALL
        | FCC_VOLUME_TOO_BIG
        | FCC_NO_MEDIA_IN_DRIVE
        | FCC_DEVICE_NOT_READY
        | FCC_ALIGNMENT_VIOLATION => {
            FAILURE.store(command, Ordering::Relaxed);
        }
        _ => {}
    }
    if cancelled {
        CANCELLED.store(true, Ordering::Relaxed);
        return 0;
    }
    1
}

fn failure_reason(command: u32) -> &'static str {
    match command {
        FCC_INCOMPATIBLE_FILE_SYSTEM => "Windows cannot create this file system here",
        FCC_ACCESS_DENIED => "access was denied",
        FCC_MEDIA_WRITE_PROTECTED => "the media is write-protected",
        FCC_VOLUME_IN_USE => "the volume is in use",
        FCC_CANT_QUICK_FORMAT => "this volume cannot be quick formatted",
        FCC_BAD_LABEL => "the volume label is not valid for this file system",
        FCC_CLUSTER_SIZE_TOO_SMALL => "the cluster size is too small",
        FCC_CLUSTER_SIZE_TOO_BIG => "the cluster size is too large",
        FCC_VOLUME_TOO_SMALL => "the volume is too small for this file system",
        FCC_VOLUME_TOO_BIG => "the volume is too large for this file system",
        FCC_NO_MEDIA_IN_DRIVE => "there is no media in the drive",
        FCC_DEVICE_NOT_READY => "the device is not ready",
        FCC_ALIGNMENT_VIOLATION => "the partition is misaligned",
        _ => "the format provider reported an error",
    }
}

/// The name `FormatEx` knows a file system by.
pub(crate) fn provider_name(filesystem: FileSystem) -> Result<&'static str, HelperError> {
    Ok(match filesystem {
        FileSystem::Fat => "FAT",
        FileSystem::Fat32 => "FAT32",
        FileSystem::ExFat => "exFAT",
        FileSystem::Ntfs => "NTFS",
        FileSystem::Udf => "UDF",
        FileSystem::Ext2 | FileSystem::Ext3 | FileSystem::Ext4 => {
            return Err(HelperError::Operation(
                "ext2/3/4 creation is not available on Windows yet".into(),
            ))
        }
        FileSystem::Refs => {
            return Err(HelperError::Operation(
                "ReFS creation is not available".into(),
            ))
        }
    })
}

/// Format `volume` (`\\?\Volume{GUID}\`), reporting percent done.
#[allow(clippy::too_many_arguments)]
pub(crate) fn format_volume(
    volume: &str,
    filesystem: FileSystem,
    label: &str,
    cluster_size: Option<u32>,
    quick: bool,
    removable: bool,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(u32),
) -> Result<(), HelperError> {
    let name = provider_name(filesystem)?;
    let _guard = FORMAT_LOCK
        .lock()
        .map_err(|_| HelperError::Operation("format state was poisoned".into()))?;
    let library = wide("fmifs.dll");
    // SAFETY: loads a system library by name; it stays loaded for the process.
    let module = unsafe { LoadLibraryW(library.as_ptr()) };
    if module.is_null() {
        return Err(HelperError::MissingTool("fmifs.dll".into()));
    }
    // SAFETY: looks up an exported function by a NUL-terminated name.
    let Some(address) = (unsafe { GetProcAddress(module, c"FormatEx".as_ptr().cast()) }) else {
        return Err(HelperError::MissingTool("fmifs.dll FormatEx".into()));
    };
    // SAFETY: FormatEx has this signature on every supported Windows version.
    let format_ex: FormatEx = unsafe { std::mem::transmute(address) };

    // FormatEx rejects a trailing backslash on the volume name.
    let root = wide(volume.trim_end_matches('\\'));
    let file_system = wide(name);
    let label = wide(label);
    CANCELLED.store(false, Ordering::Relaxed);
    SUCCEEDED.store(false, Ordering::Relaxed);
    FAILURE.store(0, Ordering::Relaxed);
    PERCENT.store(0, Ordering::Relaxed);
    if let Ok(mut token) = CANCEL_TOKEN.lock() {
        *token = Some(cancel.clone());
    }

    let worker = std::thread::scope(|scope| {
        let handle = scope.spawn(|| {
            // SAFETY: all strings are NUL-terminated and outlive the call.
            unsafe {
                format_ex(
                    root.as_ptr(),
                    if removable {
                        REMOVABLE_MEDIA
                    } else {
                        FIXED_MEDIA
                    },
                    file_system.as_ptr(),
                    label.as_ptr(),
                    i32::from(quick),
                    // 0 asks for the default cluster size, which UDF requires.
                    match filesystem {
                        FileSystem::Udf => 0,
                        _ => cluster_size.unwrap_or(0),
                    },
                    callback,
                )
            }
        });
        let mut last = u32::MAX;
        while !handle.is_finished() {
            let percent = PERCENT.load(Ordering::Relaxed);
            if percent != last {
                progress(percent);
                last = percent;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        handle.join()
    });
    if let Ok(mut token) = CANCEL_TOKEN.lock() {
        *token = None;
    }
    if worker.is_err() {
        return Err(HelperError::Operation("the format provider crashed".into()));
    }
    if CANCELLED.load(Ordering::Relaxed) {
        return Err(HelperError::Cancelled);
    }
    if !SUCCEEDED.load(Ordering::Relaxed) {
        return Err(HelperError::Operation(format!(
            "{name} formatting failed: {}",
            failure_reason(FAILURE.load(Ordering::Relaxed))
        )));
    }
    progress(100);
    Ok(())
}
