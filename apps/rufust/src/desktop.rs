//! Desktop integration that differs by platform: the image file chooser and
//! the system's light or dark preference.

use std::path::PathBuf;

/// File name patterns offered as "Disk images".
const IMAGE_PATTERNS: &[&str] = &[
    "*.iso", "*.img", "*.raw", "*.dmg", "*.vhd", "*.vhdx", "*.gz", "*.xz", "*.lzma", "*.bz2",
    "*.zst", "*.zstd", "*.zip", "*.wim", "*.esd", "*.ffu",
];

/// Linux: the XDG desktop portal's file chooser.
#[cfg(target_os = "linux")]
pub fn pick_image_file() -> Result<Option<PathBuf>, String> {
    use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
    use ashpd::desktop::ResponseError;

    let images = IMAGE_PATTERNS
        .iter()
        .fold(FileFilter::new("Disk images"), |filter, pattern| {
            filter.glob(pattern)
        });

    pollster::block_on(async {
        let request = SelectedFiles::open_file()
            .title("Select a disk image")
            .accept_label("Select")
            .modal(true)
            .filter(images)
            .filter(FileFilter::new("All files").glob("*"))
            .send()
            .await
            .map_err(|error| format!("Could not open the system file chooser: {error}"))?;

        let selected = match request.response() {
            Ok(selected) => selected,
            Err(ashpd::Error::Response(ResponseError::Cancelled)) => return Ok(None),
            Err(error) => return Err(format!("System file chooser failed: {error}")),
        };
        selected
            .uris()
            .first()
            .map(|uri| {
                uri.to_file_path()
                    .map_err(|_| "The selected image is not a local file.".to_owned())
            })
            .transpose()
    })
}

/// Linux: `GTK_THEME` names a dark variant.
#[cfg(target_os = "linux")]
pub fn prefers_dark() -> bool {
    std::env::var("GTK_THEME")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .contains("dark")
}

/// Windows: the common Open dialog, owned by (and modal to) the workbench.
#[cfg(windows)]
pub fn pick_image_file() -> Result<Option<PathBuf>, String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::UI::Controls::Dialogs::{
        CommDlgExtendedError, GetOpenFileNameW, OFN_DONTADDTORECENT, OFN_EXPLORER,
        OFN_FILEMUSTEXIST, OFN_HIDEREADONLY, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;

    let wide = |text: &str| std::ffi::OsStr::new(text).encode_wide().collect::<Vec<_>>();
    // Pairs of NUL-terminated name and pattern, ending with an empty string.
    let mut filter = Vec::new();
    filter.extend(wide("Disk images"));
    filter.push(0);
    filter.extend(wide(&IMAGE_PATTERNS.join(";")));
    filter.push(0);
    filter.extend(wide("All files"));
    filter.push(0);
    filter.extend(wide("*.*"));
    filter.extend([0, 0]);
    let mut title = wide("Select a disk image");
    title.push(0);
    let mut path = vec![0u16; 32_768];

    // SAFETY: zero is a valid starting value for this plain structure.
    let mut dialog: OPENFILENAMEW = unsafe { std::mem::zeroed() };
    dialog.lStructSize = size_of::<OPENFILENAMEW>() as u32;
    // SAFETY: returns this thread's active window (the workbench) or null.
    dialog.hwndOwner = unsafe { GetActiveWindow() };
    dialog.lpstrFilter = filter.as_ptr();
    dialog.nFilterIndex = 1;
    dialog.lpstrFile = path.as_mut_ptr();
    dialog.nMaxFile = path.len() as u32;
    dialog.lpstrTitle = title.as_ptr();
    dialog.Flags = OFN_EXPLORER
        | OFN_FILEMUSTEXIST
        | OFN_PATHMUSTEXIST
        | OFN_HIDEREADONLY
        | OFN_NOCHANGEDIR
        | OFN_DONTADDTORECENT;
    // SAFETY: every buffer outlives the modal call.
    if unsafe { GetOpenFileNameW(&mut dialog) } == 0 {
        // SAFETY: no arguments; zero means the user cancelled.
        return match unsafe { CommDlgExtendedError() } {
            0 => Ok(None),
            code => Err(format!("The file dialog failed (error 0x{code:x}).")),
        };
    }
    let end = path
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(path.len());
    Ok(Some(PathBuf::from(OsString::from_wide(&path[..end]))))
}

/// Windows: the "Choose your app mode" setting.
#[cfg(windows)]
pub fn prefers_dark() -> bool {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD};

    let key: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize\0"
        .encode_utf16()
        .collect();
    let value: Vec<u16> = "AppsUseLightTheme\0".encode_utf16().collect();
    let mut light = 1u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the strings are NUL-terminated and `light` holds one DWORD.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut light as *mut u32).cast(),
            &mut size,
        )
    };
    status == 0 && light == 0
}
