//! Code shared by the udisks2 and Windows disk engines: progress reporting,
//! the target's blocking risks, the raw bad-block and read-back loops, and
//! the platform-neutral steps of Windows installer media.
//!
//! Each engine keeps its own device access, locking, mounting, and flushing.
//! The loops here reach the device only through closures the engine supplies,
//! with buffers it allocates (the Windows engine needs sector-aligned ones).

use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

use rufust_core::device::{BlockDevice, DeviceRisk};
use rufust_core::plan::BootMode;
use rufust_helper_protocol::{FormatSpec, HelperEvent, WindowsCustomization};
use rufust_image::isofs;
use sha2::{Digest, Sha256};

use crate::windows_media::{self as winmedia, CopiedFile};
use crate::{
    check_cancel, emit, progress, unattend, Cancellability, CancellationToken, EventSink,
    HelperError, JobId, ProgressStage, ProgressUnit,
};

pub(crate) const MIB: u64 = 1024 * 1024;

/// Risks that make a target unusable even after unmounting.
const BLOCKING_RISKS: [DeviceRisk; 8] = [
    DeviceRisk::ContainsRoot,
    DeviceRisk::ContainsBoot,
    DeviceRisk::ContainsSwap,
    DeviceRisk::ActiveRaidMember,
    DeviceRisk::ActiveVolumeMember,
    DeviceRisk::ReadOnly,
    DeviceRisk::HasDependents,
    DeviceRisk::IdentityUnstable,
];

/// Refuse a revalidated target that carries any blocking risk.
pub(crate) fn refuse_blocking_risks(block: &BlockDevice) -> Result<(), HelperError> {
    for risk in BLOCKING_RISKS {
        if block.has_risk(risk) {
            return Err(HelperError::Revalidation(format!(
                "target has blocking risk: {risk:?}"
            )));
        }
    }
    Ok(())
}

/// Stage and byte progress for one job.
pub(crate) struct Reporter<'a> {
    pub(crate) job_id: JobId,
    pub(crate) sink: &'a mut EventSink,
    pub(crate) step: u64,
    pub(crate) total: u64,
}

impl Reporter<'_> {
    pub(crate) fn stage(&mut self, stage: ProgressStage, detail: &str) {
        let event = progress(
            self.job_id,
            stage,
            self.step,
            Some(self.total),
            Some(detail.to_owned()),
        );
        emit(self.sink, event);
        self.step = (self.step + 1).min(self.total);
    }

    pub(crate) fn log(&mut self, line: impl Into<String>) {
        let event = HelperEvent::Log {
            job_id: self.job_id,
            line: line.into(),
        };
        emit(self.sink, event);
    }

    pub(crate) fn bytes(
        &mut self,
        stage: ProgressStage,
        meter: &mut Meter,
        done: u64,
        detail: &str,
    ) {
        if !meter.due(done) {
            return;
        }
        let event = HelperEvent::Progress {
            job_id: self.job_id,
            stage,
            unit: ProgressUnit::Bytes,
            completed: done,
            total: Some(meter.total),
            bytes_per_second: Some(meter.rate(done)),
            detail: Some(detail.to_owned()),
            cancellability: Cancellability::Immediate,
        };
        emit(self.sink, event);
    }
}

pub(crate) struct Meter {
    pub(crate) total: u64,
    started: Instant,
    last: Option<Instant>,
}

impl Meter {
    pub(crate) fn new(total: u64) -> Self {
        Self {
            total,
            started: Instant::now(),
            last: None,
        }
    }

    pub(crate) fn due(&mut self, done: u64) -> bool {
        let now = Instant::now();
        if done < self.total
            && self
                .last
                .is_some_and(|last| now.duration_since(last) < Duration::from_millis(200))
        {
            return false;
        }
        self.last = Some(now);
        true
    }

    pub(crate) fn rate(&self, done: u64) -> u64 {
        (done as f64 / self.started.elapsed().as_secs_f64().max(0.001)) as u64
    }
}

/// Read the image's Windows details and refuse customization it cannot take.
pub(crate) fn customization_plan(
    iso: &mut File,
    listing: &isofs::IsoListing,
    options: &WindowsCustomization,
) -> Result<(unattend::AnswerFile, u32), HelperError> {
    let windows = rufust_image::windows::inspect(iso, listing)
        .filter(|windows| windows.is_windows_10_or_later())
        .ok_or_else(|| {
            HelperError::Operation(
                "Windows User Experience options need Windows 10 or later media".into(),
            )
        })?;
    if windows.has_panther_unattend || listing.has_file(unattend::ROOT_ANSWER_FILE) {
        return Err(HelperError::Operation(
            "this image already has its own answer file".into(),
        ));
    }
    if let Some(index) = options.silent_install_index {
        if !windows
            .editions
            .iter()
            .any(|edition| edition.index == index)
        {
            return Err(HelperError::Operation(format!(
                "this image has no Windows edition {index}"
            )));
        }
    }
    let arch = windows.arch.ok_or_else(|| {
        HelperError::Operation("the Windows architecture of this image is unknown".into())
    })?;
    let language = windows.setup_language.as_deref().unwrap_or("en-US");
    Ok((
        unattend::answer_file(options, arch, language),
        windows.build,
    ))
}

/// Patterns of `badblocks -w`, each written over the whole device in turn.
const BAD_BLOCK_PATTERNS: [u8; 4] = [0xaa, 0x55, 0xff, 0x00];
/// Bytes per bad-block write and read; both buffers must be this long.
pub(crate) const BAD_BLOCK_CHUNK: usize = 4 * MIB as usize;
/// Bytes per read-back verification read; the buffer must be this long.
pub(crate) const VERIFY_CHUNK: usize = 4 * MIB as usize;

/// Write all of the bytes at a device offset.
pub(crate) type WriteAt<'a> = dyn FnMut(u64, &[u8]) -> Result<(), HelperError> + 'a;
/// Fill the whole buffer from a device offset; a short read is an error.
pub(crate) type ReadAt<'a> = dyn FnMut(u64, &mut [u8]) -> Result<(), HelperError> + 'a;

/// Positional access to the whole target device, supplied by an engine.
pub(crate) struct RawDisk<'a> {
    pub(crate) write_at: &'a mut WriteAt<'a>,
    pub(crate) read_at: &'a mut ReadAt<'a>,
    /// Make the writes durable so that the reads after it reach the device.
    pub(crate) flush: &'a mut dyn FnMut() -> Result<(), HelperError>,
}

/// `badblocks -w`: write each pattern over the first `size` bytes, flush,
/// then read it back. Bad sectors are counted in 512-byte units, and a
/// failed read counts every unit of its chunk as bad.
pub(crate) fn test_bad_blocks(
    size: u64,
    written: &mut [u8],
    read: &mut [u8],
    disk: RawDisk<'_>,
    cancel: &CancellationToken,
    report: &mut Reporter<'_>,
) -> Result<(), HelperError> {
    let chunk = BAD_BLOCK_CHUNK as u64;
    report.stage(ProgressStage::TestingMedia, "Checking for bad blocks");
    let mut meter = Meter::new(size * 2 * BAD_BLOCK_PATTERNS.len() as u64);
    let mut done = 0u64;
    let mut bad = 0u64;
    for pattern in BAD_BLOCK_PATTERNS {
        written.fill(pattern);
        let label = format!("Bad-block test: writing 0x{pattern:02x}");
        let mut offset = 0;
        while offset < size {
            check_cancel(cancel)?;
            let length = chunk.min(size - offset) as usize;
            (disk.write_at)(offset, &written[..length])?;
            offset += length as u64;
            done += length as u64;
            report.bytes(ProgressStage::TestingMedia, &mut meter, done, &label);
        }
        (disk.flush)()?;
        let label = format!("Bad-block test: reading 0x{pattern:02x}");
        let mut offset = 0;
        while offset < size {
            check_cancel(cancel)?;
            let length = chunk.min(size - offset) as usize;
            match (disk.read_at)(offset, &mut read[..length]) {
                Ok(()) => {
                    bad += read[..length]
                        .chunks(512)
                        .filter(|sector| sector.iter().any(|byte| *byte != pattern))
                        .count() as u64
                }
                Err(_) => bad += length as u64 / 512,
            }
            offset += length as u64;
            done += length as u64;
            report.bytes(ProgressStage::TestingMedia, &mut meter, done, &label);
        }
    }
    if bad > 0 {
        return Err(HelperError::Operation(format!(
            "bad-block test found {bad} unreliable sectors; this device should not be used"
        )));
    }
    report.log("Bad-block test passed.");
    Ok(())
}

/// Hash the first `size` bytes of the device and compare with `expected`.
/// Each read is rounded up to whole `align`-byte units (1 for none); only the
/// first `size` bytes are hashed. `progress` gets the bytes checked so far.
pub(crate) fn verify_hash(
    size: u64,
    expected: &[u8; 32],
    buffer: &mut [u8],
    align: u64,
    read_at: &mut ReadAt<'_>,
    cancel: &CancellationToken,
    progress: &mut dyn FnMut(u64),
) -> Result<(), HelperError> {
    let chunk = VERIFY_CHUNK as u64;
    let mut hasher = Sha256::new();
    let mut offset = 0u64;
    while offset < size {
        check_cancel(cancel)?;
        let wanted = (size - offset).min(chunk);
        let length = wanted.div_ceil(align) * align;
        read_at(offset, &mut buffer[..length as usize])?;
        hasher.update(&buffer[..wanted as usize]);
        offset += wanted;
        progress(offset);
    }
    let actual: [u8; 32] = hasher.finalize().into();
    if actual != *expected {
        return Err(HelperError::Operation(
            "verification hash did not match the written image".into(),
        ));
    }
    Ok(())
}

/// The data partition's first sector as the boot record records it.
pub(crate) fn boot_record_start(data_offset: u64, sector: u64) -> Result<u32, HelperError> {
    u32::try_from(data_offset / sector)
        .map_err(|_| HelperError::Operation("partition start is beyond MBR reach".into()))
}

/// Windows installer media as planned before anything is written.
pub(crate) struct WindowsMediaPlan<'a> {
    pub(crate) needs: winmedia::Requirements,
    pub(crate) layout: winmedia::Layout,
    iso: File,
    listing: isofs::IsoListing,
    customization: Option<((unattend::AnswerFile, u32), &'a WindowsCustomization)>,
}

/// Check the image and the target and plan the layout, before the first
/// write: a Windows ISO with the boot files the target system needs, a
/// layout and file system the files fit in, and customization the image can
/// take. `sector` is the engine's logical sector size for the layout.
pub(crate) fn plan_windows_media<'a>(
    source: &File,
    format: &FormatSpec,
    customization: Option<&'a WindowsCustomization>,
    disk_size: u64,
    sector: u64,
    report: &mut Reporter<'_>,
) -> Result<WindowsMediaPlan<'a>, HelperError> {
    let needs = winmedia::requirements(format.filesystem, format.scheme, format.boot_mode)?;
    let mut iso = source.try_clone()?;
    let listing = isofs::list(&mut iso)?
        .ok_or_else(|| HelperError::Operation("the ISO file system could not be read".into()))?;
    let has = |path: &str| listing.has_file(path);
    if !(has("sources/install.wim") || has("sources/install.esd") || has("sources/install.swm")) {
        return Err(HelperError::Operation(
            "this ISO is not Windows installation media".into(),
        ));
    }
    if needs.bios_boot && !has("bootmgr") {
        return Err(HelperError::Operation(
            "this image has no BIOS boot manager; choose UEFI".into(),
        ));
    }
    if format.boot_mode != BootMode::Bios
        && ![
            "efi/boot/bootx64.efi",
            "efi/boot/bootia32.efi",
            "efi/boot/bootaa64.efi",
        ]
        .iter()
        .any(|path| has(path))
    {
        return Err(HelperError::Operation(
            "this image has no UEFI boot loader; choose BIOS".into(),
        ));
    }
    let layout = winmedia::plan_layout(disk_size, sector, format.scheme, needs.uefi_ntfs)?;
    winmedia::check_fits(&listing, format.filesystem, layout.data.size)?;
    let customization = match customization {
        Some(options) => Some((customization_plan(&mut iso, &listing, options)?, options)),
        None => None,
    };
    report.log(format!(
        "Windows media: {} files, {} MiB, {}{}",
        listing.entries.iter().filter(|entry| !entry.is_dir).count(),
        listing.total_file_bytes() / MIB,
        format.filesystem.as_str(),
        if needs.uefi_ntfs { " + UEFI:NTFS" } else { "" }
    ));
    Ok(WindowsMediaPlan {
        needs,
        layout,
        iso,
        listing,
        customization,
    })
}

/// Copy the image's files into the mounted data volume at `root`, then
/// apply the customization, if any. The caller has already reported the
/// copy stage and mounted the volume.
pub(crate) fn copy_windows_files(
    plan: &mut WindowsMediaPlan<'_>,
    root: &Path,
    cancel: &CancellationToken,
    report: &mut Reporter<'_>,
) -> Result<Vec<CopiedFile>, HelperError> {
    let total = plan.listing.total_file_bytes();
    let mut meter = Meter::new(total);
    let mut copies = {
        let mut on_progress = |done: u64, path: &str| {
            report.bytes(
                ProgressStage::ExtractingFiles,
                &mut meter,
                done,
                &format!("Copying {path}"),
            )
        };
        winmedia::copy_listing(&mut plan.iso, &plan.listing, root, cancel, &mut on_progress)?
    };
    if let Some(((answer, build), options)) = &plan.customization {
        report.stage(
            ProgressStage::ApplyingCustomization,
            "Applying Windows customization",
        );
        for line in &answer.log {
            report.log(line.clone());
        }
        winmedia::apply_customization(
            root,
            answer,
            options.bypass_requirements,
            *build,
            &mut copies,
            &mut |line| report.log(line),
        )?;
    }
    Ok(copies)
}

/// Read the copied files back from the volume, now mounted at `read_root`,
/// and compare them with what was written under `written_root`. The caller
/// has already reported the stage and remounted the volume.
pub(crate) fn verify_windows_files(
    copies: &[CopiedFile],
    written_root: &Path,
    read_root: &Path,
    cancel: &CancellationToken,
    report: &mut Reporter<'_>,
) -> Result<(), HelperError> {
    let mut meter = Meter::new(winmedia::copied_bytes(copies));
    let mut on_progress = |done: u64| {
        report.bytes(
            ProgressStage::Verifying,
            &mut meter,
            done,
            "Verifying copied files",
        )
    };
    winmedia::verify_copies(copies, written_root, read_root, cancel, &mut on_progress)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    /// An in-memory device; `broken` is a byte range whose content never
    /// reads back as written.
    struct Memory {
        bytes: RefCell<Vec<u8>>,
        broken: Option<std::ops::Range<usize>>,
    }

    impl Memory {
        fn new(size: usize) -> Self {
            Self {
                bytes: RefCell::new(vec![0x11; size]),
                broken: None,
            }
        }

        fn write(&self, offset: u64, data: &[u8]) -> Result<(), HelperError> {
            let start = offset as usize;
            self.bytes.borrow_mut()[start..start + data.len()].copy_from_slice(data);
            Ok(())
        }

        fn read(&self, offset: u64, buffer: &mut [u8]) -> Result<(), HelperError> {
            let start = offset as usize;
            let bytes = self.bytes.borrow();
            let end = start + buffer.len();
            if end > bytes.len() {
                return Err(HelperError::Operation("short read".into()));
            }
            buffer.copy_from_slice(&bytes[start..end]);
            if let Some(broken) = &self.broken {
                for index in broken.clone().filter(|index| (start..end).contains(index)) {
                    buffer[index - start] ^= 0xff;
                }
            }
            Ok(())
        }
    }

    fn bad_blocks(device: &Memory, cancel: &CancellationToken) -> Result<(), HelperError> {
        let mut sink: EventSink = Box::new(|_| {});
        let mut report = Reporter {
            job_id: JobId::new(1),
            sink: &mut sink,
            step: 0,
            total: 1,
        };
        let size = device.bytes.borrow().len() as u64;
        test_bad_blocks(
            size,
            &mut vec![0u8; BAD_BLOCK_CHUNK],
            &mut vec![0u8; BAD_BLOCK_CHUNK],
            RawDisk {
                write_at: &mut |offset, bytes| device.write(offset, bytes),
                read_at: &mut |offset, buffer| device.read(offset, buffer),
                flush: &mut || Ok(()),
            },
            cancel,
            &mut report,
        )
    }

    #[test]
    fn bad_block_test_covers_the_whole_device_and_ends_on_zeros() {
        // Not a multiple of the chunk, so the last write is a short one.
        let device = Memory::new(BAD_BLOCK_CHUNK + 4096);
        bad_blocks(&device, &CancellationToken::new()).expect("healthy device");
        assert!(device.bytes.borrow().iter().all(|byte| *byte == 0));
    }

    #[test]
    fn bad_block_test_counts_unreliable_sectors() {
        let mut device = Memory::new(BAD_BLOCK_CHUNK + 4096);
        // Three bytes in two different 512-byte sectors.
        device.broken = Some(1022..1025);
        let error = bad_blocks(&device, &CancellationToken::new()).expect_err("bad media");
        // Two sectors, seen once for each of the four patterns.
        assert!(
            error.to_string().contains("found 8 unreliable sectors"),
            "{error}"
        );
    }

    #[test]
    fn bad_block_test_stops_when_cancelled_before_writing() {
        let device = Memory::new(8192);
        let cancel = CancellationToken::new();
        cancel.request();
        assert!(matches!(
            bad_blocks(&device, &cancel),
            Err(HelperError::Cancelled)
        ));
        assert!(device.bytes.borrow().iter().all(|byte| *byte == 0x11));
    }

    fn verify(
        device: &Memory,
        size: u64,
        expected: &[u8; 32],
        align: u64,
        cancel: &CancellationToken,
    ) -> (Result<(), HelperError>, Vec<u64>) {
        let mut seen = Vec::new();
        let result = verify_hash(
            size,
            expected,
            &mut vec![0u8; VERIFY_CHUNK],
            align,
            &mut |offset, buffer| device.read(offset, buffer),
            cancel,
            &mut |done| seen.push(done),
        );
        (result, seen)
    }

    #[test]
    fn verification_hashes_only_the_written_bytes() {
        // The image ends mid-sector; the device continues past it.
        let size = VERIFY_CHUNK as u64 + 700;
        let device = Memory::new(VERIFY_CHUNK + 1024);
        for (index, byte) in device.bytes.borrow_mut().iter_mut().enumerate() {
            *byte = (index % 251) as u8;
        }
        let expected: [u8; 32] = Sha256::digest(&device.bytes.borrow()[..size as usize]).into();
        let cancel = CancellationToken::new();
        for align in [1, 512] {
            let (result, seen) = verify(&device, size, &expected, align, &cancel);
            result.expect("matching hash");
            assert_eq!(seen, [VERIFY_CHUNK as u64, size]);
        }

        // One changed byte after the image passes; one inside it fails.
        device.bytes.borrow_mut()[size as usize] ^= 1;
        verify(&device, size, &expected, 512, &cancel)
            .0
            .expect("bytes after the image are not hashed");
        device.bytes.borrow_mut()[size as usize - 1] ^= 1;
        let error = verify(&device, size, &expected, 512, &cancel)
            .0
            .expect_err("mismatch");
        assert!(error.to_string().contains("did not match"), "{error}");
    }

    #[test]
    fn verification_reports_short_reads_and_cancellation() {
        let device = Memory::new(1024);
        let cancel = CancellationToken::new();
        // The image claims more bytes than the device holds.
        assert!(verify(&device, 4096, &[0; 32], 1, &cancel).0.is_err());
        cancel.request();
        let (result, seen) = verify(&device, 1024, &[0; 32], 1, &cancel);
        assert!(matches!(result, Err(HelperError::Cancelled)));
        assert!(seen.is_empty());
    }

    #[test]
    fn boot_record_start_is_the_partition_sector() {
        assert_eq!(boot_record_start(MIB, 512).expect("start"), 2048);
        assert_eq!(boot_record_start(MIB, 4096).expect("start"), 256);
        assert!(boot_record_start(u64::from(u32::MAX) * 512 + 512, 512).is_err());
    }

    #[test]
    fn blocking_risks_are_refused_and_mounted_volumes_are_not() {
        use rufust_core::device::{DeviceClass, DeviceFingerprint, DeviceNumber, Transport};

        let mut device = BlockDevice {
            node: "/dev/sdz".into(),
            display_name: "Test".into(),
            vendor: None,
            model: None,
            class: DeviceClass::Removable,
            transport: Transport::Usb,
            fingerprint: DeviceFingerprint {
                number: DeviceNumber::new(8, 16),
                canonical_sysfs_path: "/sys/devices/example".into(),
                size_bytes: 1 << 30,
                logical_block_size: 512,
                serial: None,
                wwn: None,
            },
            risks: vec![DeviceRisk::MountedChildren],
        };
        refuse_blocking_risks(&device).expect("mounted volumes are released later");
        for risk in BLOCKING_RISKS {
            device.risks = vec![risk];
            let error = refuse_blocking_risks(&device).expect_err("blocked");
            assert!(error.to_string().contains(&format!("{risk:?}")), "{error}");
        }
    }
}
