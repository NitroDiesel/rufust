//! End-to-end checks of the Windows engine on a disposable virtual disk.
//!
//! Run elevated, with a VHD(X) attached and its disk number in
//! `RUFUST_TEST_DISK` (and optionally a second one over 32 GB in
//! `RUFUST_TEST_LARGE_DISK` for the large FAT32 path):
//!
//! ```text
//! cargo test -p rufust-helper windows::vhd_tests -- --ignored --test-threads=1
//! ```
//!
//! Each test refuses any disk that is not file-backed virtual storage, so a
//! wrong number cannot reach real media. `scripts/windows/vhd-tests.ps1`
//! creates, attaches, and removes the disks.

use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rufust_core::plan::{
    BootMode, FileSystem, ImageSourceKind, PartitionScheme, VerificationLevel, WriteMode,
};
use rufust_core::progress::{CancellationToken, JobId};
use rufust_helper_protocol::{
    FormatSpec, HelperEvent, HelperOperation, HelperRequest, HelperResult, SourceSpec,
    TargetIdentity, PROTOCOL_VERSION,
};
use rufust_image::isofs::fixture::{self, Node};
use rufust_platform::windows::{self as platform, wide};
use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;

use super::*;

fn target(variable: &str) -> Option<DeviceIdentity> {
    let number: u32 = std::env::var(variable).ok()?.parse().ok()?;
    let identity = discover_devices(true)
        .expect("discovery")
        .into_iter()
        .find(|device| device.minor == number)
        .expect("the disk is attached");
    assert!(
        disposable(&identity),
        "refusing disk {number}: it is neither a file-backed virtual disk nor the \
         removable USB drive whose serial is in RUFUST_TEST_USB_SERIAL"
    );
    Some(identity)
}

/// Virtual disks are always fair game. A physical disk qualifies only as a
/// removable USB drive whose exact serial the caller named, which is how
/// `scripts/windows/usb-tests.ps1` opts one disposable stick in.
fn disposable(identity: &DeviceIdentity) -> bool {
    if identity.transport == "loop" {
        return true;
    }
    let named = std::env::var("RUFUST_TEST_USB_SERIAL").unwrap_or_default();
    identity.transport == "usb"
        && identity.removable
        && !identity.serial.is_empty()
        && identity.serial == named
}

fn request(identity: &DeviceIdentity, operation: HelperOperation) -> HelperRequest {
    let block = platform::to_block_device(identity, &platform::volumes());
    HelperRequest {
        protocol_version: PROTOCOL_VERSION,
        job_id: JobId::new(1),
        target: TargetIdentity {
            node: identity.node.clone(),
            fingerprint: block.fingerprint,
            display_name: identity.display_name(),
            model: identity.model.clone(),
            serial: identity.serial.clone(),
        },
        operation,
        action_name: "test".into(),
    }
}

fn run(identity: &DeviceIdentity, operation: HelperOperation) -> Vec<HelperEvent> {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&events);
    let result = execute(
        request(identity, operation),
        ExecutionOptions::default(),
        Box::new(move |event| sink.lock().expect("events").push(event)),
    );
    let events = events.lock().expect("events").clone();
    if let Err(error) = result {
        panic!("operation failed: {error}\nevents: {events:#?}");
    }
    assert!(events.iter().any(|event| matches!(
        event,
        HelperEvent::Finished {
            result: HelperResult::Success,
            ..
        }
    )));
    events
}

fn format(scheme: PartitionScheme, filesystem: FileSystem, label: &str) -> FormatSpec {
    FormatSpec {
        scheme,
        boot_mode: BootMode::NonBootable,
        filesystem,
        label: label.into(),
        cluster_size: None,
        persistence_bytes: 0,
        quick_format: true,
    }
}

/// File system name and label of the volume at `offset` on `disk`.
fn describe(disk: u32, offset: u64) -> (String, String, PathBuf) {
    let volume = disk::wait_for_volume(disk, offset, &CancellationToken::new()).expect("volume");
    let name = wide(&volume.name);
    let mut label = [0u16; 64];
    let mut filesystem = [0u16; 64];
    // SAFETY: both buffers are writable for their stated lengths.
    let ok = unsafe {
        GetVolumeInformationW(
            name.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            filesystem.as_mut_ptr(),
            filesystem.len() as u32,
        )
    };
    assert_ne!(ok, 0, "volume information: {}", io::Error::last_os_error());
    let text = |buffer: &[u16]| {
        let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
        String::from_utf16_lossy(&buffer[..end])
    };
    (text(&filesystem), text(&label), PathBuf::from(&volume.name))
}

fn read_disk(identity: &DeviceIdentity, offset: u64, length: usize) -> Vec<u8> {
    let disk = Disk::open(
        identity.minor,
        identity.size_bytes,
        identity.logical_sector_size,
    )
    .expect("open disk");
    let mut buffer = AlignedBuffer::new(length);
    disk.read_at(offset, buffer.as_mut_slice()).expect("read");
    buffer.as_slice().to_vec()
}

fn scratch(name: &str) -> PathBuf {
    let directory = std::env::temp_dir().join(format!("rufust-vhd-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("scratch directory");
    directory.join(name)
}

#[test]
#[ignore = "needs an elevated shell and an attached VHD in RUFUST_TEST_DISK"]
fn formats_with_each_windows_provider() {
    let Some(identity) = target("RUFUST_TEST_DISK") else {
        return;
    };
    for (scheme, filesystem, label, expected) in [
        (
            PartitionScheme::Gpt,
            FileSystem::Ntfs,
            "Rufust NTFS",
            "NTFS",
        ),
        (
            PartitionScheme::Mbr,
            FileSystem::Fat32,
            "rufust fat",
            "FAT32",
        ),
        (
            PartitionScheme::Mbr,
            FileSystem::ExFat,
            "Rufust exFAT",
            "exFAT",
        ),
        (PartitionScheme::Gpt, FileSystem::Udf, "RufustUDF", "UDF"),
    ] {
        run(
            &identity,
            HelperOperation::FormatMedia {
                format: format(scheme, filesystem, &filesystem.volume_label(label)),
                bad_blocks: false,
            },
        );
        let (name, volume_label, root) = describe(identity.minor, MIB);
        assert_eq!(name, expected, "{scheme:?} {filesystem:?}");
        assert_eq!(volume_label, filesystem.volume_label(label));
        std::fs::write(root.join("probe.txt"), b"rufust").expect("write to new volume");
        assert_eq!(
            std::fs::read(root.join("probe.txt")).expect("read"),
            b"rufust"
        );
    }
}

#[test]
#[ignore = "needs an elevated shell and an attached VHD in RUFUST_TEST_DISK"]
fn writes_and_verifies_raw_and_compressed_images() {
    let Some(identity) = target("RUFUST_TEST_DISK") else {
        return;
    };
    // A sector-unaligned size exercises the padded final write.
    let image = (0..3 * MIB + 100)
        .map(|i| (i % 251) as u8)
        .collect::<Vec<_>>();
    let raw = scratch("image.img");
    std::fs::write(&raw, &image).expect("raw fixture");
    let gz = scratch("image.img.gz");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&image).expect("gzip");
    std::fs::write(&gz, encoder.finish().expect("gzip")).expect("gz fixture");

    for (path, kind, expanded) in [
        (&raw, ImageSourceKind::Raw, None),
        (
            &gz,
            ImageSourceKind::CompressedRaw,
            Some(image.len() as u64),
        ),
    ] {
        let size = std::fs::metadata(path).expect("size").len();
        run(
            &identity,
            HelperOperation::WriteMedia {
                write_mode: WriteMode::DdImage,
                source: SourceSpec {
                    path: path.clone(),
                    kind,
                    size_bytes: size,
                    decompressed_size_bytes: expanded,
                    expected_sha256: Some(hex_lower(&Sha256::digest(&image))),
                },
                format: format(PartitionScheme::Mbr, FileSystem::Fat32, ""),
                verification: VerificationLevel::FullReadback,
                bad_blocks: false,
                install_bootloader: None,
                windows_customization: None,
            },
        );
        let written = read_disk(&identity, 0, (4 * MIB) as usize);
        assert_eq!(&written[..image.len()], &image[..], "{kind:?}");
        assert!(written[image.len()..image.len().div_ceil(512) * 512]
            .iter()
            .all(|byte| *byte == 0));
    }
}

#[test]
#[ignore = "needs an elevated shell and an attached VHD in RUFUST_TEST_DISK"]
fn builds_windows_installer_media() {
    let Some(identity) = target("RUFUST_TEST_DISK") else {
        return;
    };
    let iso = scratch("windows.iso");
    std::fs::write(
        &iso,
        fixture::udf(vec![
            Node::File("bootmgr", 50_000),
            Node::Dir(
                "sources",
                vec![
                    Node::File("boot.wim", 5_000_000),
                    Node::File("install.wim", 20_000_000),
                ],
            ),
            Node::Dir(
                "efi",
                vec![Node::Dir(
                    "boot",
                    vec![Node::File("bootx64.efi", 1_500_000)],
                )],
            ),
        ]),
    )
    .expect("ISO fixture");
    let size = std::fs::metadata(&iso).expect("size").len();
    for (scheme, boot_mode, filesystem) in [
        (PartitionScheme::Mbr, BootMode::Dual, FileSystem::Ntfs),
        (PartitionScheme::Gpt, BootMode::Uefi, FileSystem::Ntfs),
        (PartitionScheme::Mbr, BootMode::Dual, FileSystem::Fat32),
    ] {
        let mut spec = format(scheme, filesystem, "CCCOMA_X64FRE");
        spec.boot_mode = boot_mode;
        let events = run(
            &identity,
            HelperOperation::WriteMedia {
                write_mode: WriteMode::IsoFileCopy,
                source: SourceSpec {
                    path: iso.clone(),
                    kind: ImageSourceKind::Iso,
                    size_bytes: size,
                    decompressed_size_bytes: None,
                    expected_sha256: None,
                },
                format: spec,
                verification: VerificationLevel::FullReadback,
                bad_blocks: false,
                install_bootloader: None,
                windows_customization: None,
            },
        );
        assert!(events.iter().any(|event| matches!(
            event,
            HelperEvent::Progress {
                stage: ProgressStage::Verifying,
                ..
            }
        )));
        let (name, _, root) = describe(identity.minor, MIB);
        assert_eq!(name, filesystem.as_str());
        let mut install = Vec::new();
        File::open(root.join("sources").join("install.wim"))
            .expect("install.wim")
            .read_to_end(&mut install)
            .expect("read install.wim");
        assert_eq!(install.len(), 20_000_000);
        // The fixture stores files over 64 KiB as zero extents.
        assert!(install.iter().all(|byte| *byte == 0));
        let bootmgr = std::fs::read(root.join("bootmgr")).expect("bootmgr");
        assert_eq!(bootmgr.len(), 50_000);
        assert!(bootmgr
            .iter()
            .enumerate()
            .all(|(i, byte)| *byte == fixture::pattern(i as u64)));

        let mbr = read_disk(&identity, 0, 512);
        let uefi_ntfs = filesystem == FileSystem::Ntfs;
        if scheme == PartitionScheme::Mbr {
            // Windows 7 boot code, an active data partition, and 0xEF.
            assert_eq!(&mbr[..440], &winmedia_mbr()[..440]);
            assert_eq!(mbr[446], 0x80);
            assert_eq!(mbr[446 + 16 + 4], if uefi_ntfs { 0xef } else { 0 });
            let boot = read_disk(&identity, MIB, 512);
            assert_eq!(&boot[510..], &[0x55, 0xaa]);
        } else {
            assert_eq!(mbr[446 + 4], 0xee, "protective MBR");
        }
        if uefi_ntfs {
            let span = winmedia::plan_layout(
                identity.size_bytes,
                u64::from(identity.logical_sector_size),
                scheme,
                true,
            )
            .expect("layout")
            .uefi_ntfs
            .expect("UEFI:NTFS");
            let (name, label, root) = describe(identity.minor, span.offset);
            assert_eq!(name, "FAT");
            assert_eq!(label, winmedia::UEFI_NTFS_LABEL);
            assert!(root.join("EFI").join("Boot").join("bootx64.efi").is_file());
        }
    }
}

fn winmedia_mbr() -> Vec<u8> {
    let mut sector = vec![0u8; 512];
    winmedia::patch_mbr(&mut sector).expect("patch");
    sector
}

#[test]
#[ignore = "needs an elevated shell and a VHD over 32 GB in RUFUST_TEST_LARGE_DISK"]
fn formats_large_fat32_itself() {
    let Some(identity) = target("RUFUST_TEST_LARGE_DISK") else {
        return;
    };
    assert!(identity.size_bytes > LARGE_FAT32 + GIB);
    run(
        &identity,
        HelperOperation::FormatMedia {
            format: format(PartitionScheme::Mbr, FileSystem::Fat32, "big fat"),
            bad_blocks: false,
        },
    );
    let (name, label, root) = describe(identity.minor, MIB);
    assert_eq!(name, "FAT32");
    assert_eq!(label, "BIG FAT");
    std::fs::write(root.join("probe.txt"), b"rufust").expect("write");
    assert_eq!(
        std::fs::read(root.join("probe.txt")).expect("read"),
        b"rufust"
    );
}

#[test]
#[ignore = "needs an elevated shell, RUFUST_TEST_DISK, and RUFUST_TEST_VHD_SOURCE"]
fn writes_a_virtual_disk_image() {
    let Some(identity) = target("RUFUST_TEST_DISK") else {
        return;
    };
    let Some(source) = std::env::var_os("RUFUST_TEST_VHD_SOURCE").map(PathBuf::from) else {
        return;
    };
    let report = rufust_image::analyze(&source).expect("analyze");
    assert!(matches!(
        report.kind,
        ImageSourceKind::Vhd | ImageSourceKind::Vhdx
    ));
    let expanded = source::virtual_size(&source, report.kind).expect("virtual size");
    run(
        &identity,
        HelperOperation::WriteMedia {
            write_mode: WriteMode::DdImage,
            source: SourceSpec {
                path: source.clone(),
                kind: report.kind,
                size_bytes: report.size_bytes,
                decompressed_size_bytes: Some(expanded),
                expected_sha256: None,
            },
            format: format(PartitionScheme::Mbr, FileSystem::Fat32, ""),
            verification: VerificationLevel::FullReadback,
            bad_blocks: false,
            install_bootloader: None,
            windows_customization: None,
        },
    );
}
