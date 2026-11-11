//! Read-only background inspection; write authorization remains in the helper.

use std::fs::OpenOptions;
use std::path::Path;

use rufust_core::plan::ImageSourceKind;
use rufust_core::progress::CancellationToken;
use rufust_image::{ImageError, ImageReport};

pub fn inspect(path: &Path, cancel: &CancellationToken) -> Result<ImageReport, ImageError> {
    check_cancel(cancel)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        // Open a link itself rather than its target, then refuse it below;
        // backup semantics lets a directory open so it is reported as such.
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    }
    let mut file = options.open(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let attributes = file.metadata()?.file_attributes();
        if attributes & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(ImageError::Unsupported(
                "links are not accepted; select the image file itself".into(),
            ));
        }
    }
    let mut report = rufust_image::analyze_file(path, &mut file)?;
    check_cancel(cancel)?;
    if matches!(report.kind, ImageSourceKind::Vhd | ImageSourceKind::Vhdx) {
        match rufust_helper::inspect_virtual_disk_for_user(&file, report.kind, cancel) {
            Ok(size) => report.decompressed_size_bytes = Some(size),
            Err(rufust_helper::HelperError::MissingTool(tool)) => report.notes.push(format!(
                "Disk size unavailable: {tool}. Install qemu-nbd and nbdinfo to inspect virtual disk capacity."
            )),
            Err(error) => report
                .notes
                .push(format!("Disk size could not be inspected: {error}")),
        }
    }
    check_cancel(cancel)?;
    Ok(report)
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), ImageError> {
    if cancel.is_requested() {
        Err(ImageError::Unsupported("image inspection cancelled".into()))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "rufust-preview-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).expect("private test directory");
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn raw_preview_rejects_symlinks_and_honors_cancellation() {
        let fixture = Fixture::new();
        let source = fixture.0.join("source.img");
        std::fs::write(&source, [0u8; 4096]).expect("raw fixture");
        let cancel = CancellationToken::new();
        let report = inspect(&source, &cancel).expect("raw preview");
        assert_eq!(report.kind, ImageSourceKind::Raw);
        assert_eq!(report.size_bytes, 4096);
        #[cfg(unix)]
        {
            let link = fixture.0.join("link.img");
            std::os::unix::fs::symlink(&source, &link).expect("symlink fixture");
            assert!(inspect(&link, &cancel).is_err());
        }
        assert!(matches!(
            inspect(&fixture.0, &cancel),
            Err(ImageError::NotAFile(_))
        ));
        cancel.request();
        assert!(inspect(&source, &cancel)
            .expect_err("cancelled")
            .to_string()
            .contains("cancelled"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    #[ignore = "requires non-root user, qemu-img, qemu-nbd and nbdinfo"]
    fn desktop_virtual_preview_reports_capacity_without_changing_source() {
        assert_ne!(
            unsafe { libc::geteuid() },
            0,
            "run desktop preview as a user"
        );
        let fixture = Fixture::new();
        for (format, extension) in [("vpc", "vhd"), ("vhdx", "vhdx")] {
            let source = fixture.0.join(format!("source.{extension}"));
            assert!(std::process::Command::new("/usr/bin/qemu-img")
                .args(["create", "-f", format])
                .arg(&source)
                .arg("16M")
                .status()
                .expect("create virtual fixture")
                .success());
            let before = std::fs::read(&source).expect("fixture bytes");
            let report = inspect(&source, &CancellationToken::new()).expect("virtual preview");
            let expanded = report.decompressed_size_bytes.expect("inspected disk size");
            // VPC geometry can round the requested size upward.
            assert!((16 * 1024 * 1024..17 * 1024 * 1024).contains(&expanded));
            assert_eq!(expanded % 512, 0);
            assert_eq!(std::fs::read(&source).expect("unchanged fixture"), before);
            assert!(report
                .notes
                .iter()
                .any(|note| note.contains("conversion is not available")));
        }
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;

    /// Run unelevated (for example under `runas /trustlevel:0x20000`) with
    /// `RUFUST_TEST_VHD_SOURCE` naming a VHD or VHDX.
    #[test]
    #[ignore = "needs RUFUST_TEST_VHD_SOURCE"]
    fn virtual_disk_preview_reports_capacity() {
        let Some(source) = std::env::var_os("RUFUST_TEST_VHD_SOURCE") else {
            return;
        };
        let report = inspect(std::path::Path::new(&source), &CancellationToken::new())
            .expect("virtual preview");
        let expanded = report.decompressed_size_bytes.expect("inspected disk size");
        assert!(expanded >= 16 * 1024 * 1024 && expanded % 512 == 0);
    }
}
