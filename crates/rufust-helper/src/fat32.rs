//! FAT32 for volumes Windows' own formatter refuses (over 32 GB), after
//! upstream Rufus's `FormatLargeFAT32`, itself from Tom Thornhill's
//! fat32format. The layout is computed here and written through a
//! sector-aligned callback, so it is testable on any platform.

use std::io;

use rufust_core::progress::CancellationToken;

use crate::{check_cancel, HelperError};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;
const RESERVED_SECTORS: u32 = 32;
const BACKUP_BOOT_SECTOR: u32 = 6;
const FAT_COUNT: u32 = 2;
/// Fewer clusters than this would be read as FAT16.
const MIN_CLUSTERS: u64 = 65_536;
/// The top four bits of a FAT32 entry are reserved.
const MAX_CLUSTERS: u64 = 0x0FFF_FFFF;

/// Where each region of a FAT32 volume goes, in sectors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    pub bytes_per_sector: u32,
    pub sectors_per_cluster: u32,
    pub reserved_sectors: u32,
    pub fat_sectors: u32,
    pub total_sectors: u32,
    pub clusters: u32,
}

impl Layout {
    /// Reserved area, both FATs, and the root directory's cluster.
    fn system_sectors(&self) -> u64 {
        u64::from(self.reserved_sectors)
            + u64::from(FAT_COUNT) * u64::from(self.fat_sectors)
            + u64::from(self.sectors_per_cluster)
    }
}

/// Microsoft's default FAT32 cluster size for a volume of `bytes`.
pub(crate) fn default_cluster_size(bytes: u64) -> u32 {
    match bytes {
        b if b < 64 * MIB => 512,
        b if b < 128 * MIB => 1024,
        b if b < 256 * MIB => 2048,
        b if b < 8 * GIB => 4096,
        b if b < 16 * GIB => 8192,
        b if b < 32 * GIB => 16_384,
        b if b < 2048 * GIB => 32_768,
        _ => 65_536,
    }
}

pub(crate) fn plan(
    volume_bytes: u64,
    bytes_per_sector: u32,
    cluster_size: Option<u32>,
) -> Result<Layout, HelperError> {
    let refuse = |message: &str| Err(HelperError::Operation(message.into()));
    if !(512..=4096).contains(&bytes_per_sector) || !bytes_per_sector.is_power_of_two() {
        return refuse("FAT32 needs 512- to 4096-byte sectors");
    }
    let total = volume_bytes / u64::from(bytes_per_sector);
    if total < 65_536 {
        return refuse("this volume is too small for FAT32");
    }
    let Ok(total_sectors) = u32::try_from(total) else {
        return refuse("this volume is too large for FAT32 (2 TB with 512-byte sectors)");
    };
    let cluster = cluster_size.unwrap_or_else(|| default_cluster_size(volume_bytes));
    if cluster < bytes_per_sector || cluster % bytes_per_sector != 0 || cluster > 65_536 {
        return refuse("this cluster size does not suit FAT32 on this drive");
    }
    let sectors_per_cluster = cluster / bytes_per_sector;
    if !sectors_per_cluster.is_power_of_two() || sectors_per_cluster > 128 {
        return refuse("this cluster size does not suit FAT32 on this drive");
    }

    // GetFATSizeSectors: the +1 rounds up.
    let numerator = total - u64::from(RESERVED_SECTORS) + 2 * u64::from(sectors_per_cluster);
    let denominator =
        u64::from(sectors_per_cluster) * u64::from(bytes_per_sector) / 4 + u64::from(FAT_COUNT);
    let fat_sectors = numerator / denominator + 1;

    // Grow the reserved area so the data region starts on a 1 MiB boundary.
    let align = MIB / u64::from(bytes_per_sector);
    let system = u64::from(RESERVED_SECTORS) + u64::from(FAT_COUNT) * fat_sectors;
    let system = system.div_ceil(align) * align;
    let reserved = system - u64::from(FAT_COUNT) * fat_sectors;
    let Ok(reserved_sectors) = u16::try_from(reserved).map(u32::from) else {
        return refuse("FAT32 reserved area is too large");
    };

    let data = total.saturating_sub(reserved + u64::from(FAT_COUNT) * fat_sectors);
    let clusters = data / u64::from(sectors_per_cluster);
    if clusters < MIN_CLUSTERS {
        return refuse("FAT32 needs at least 65536 clusters; choose a smaller cluster size");
    }
    if clusters > MAX_CLUSTERS {
        return refuse("too many FAT32 clusters; choose a larger cluster size");
    }
    if (clusters * 4).div_ceil(u64::from(bytes_per_sector)) > fat_sectors {
        return refuse("the FAT would not cover this volume");
    }
    Ok(Layout {
        bytes_per_sector,
        sectors_per_cluster,
        reserved_sectors,
        fat_sectors: u32::try_from(fat_sectors).unwrap_or(u32::MAX),
        total_sectors,
        clusters: u32::try_from(clusters).unwrap_or(u32::MAX),
    })
}

/// The 11-byte BPB label: upper case, padded, `NO NAME` when empty.
pub(crate) fn bpb_label(label: &str) -> [u8; 11] {
    let mut bytes = [b' '; 11];
    let cleaned = label
        .chars()
        .filter(|c| c.is_ascii() && !c.is_ascii_control())
        .map(|c| match c.to_ascii_uppercase() {
            '"' | '*' | '+' | ',' | '.' | '/' | ':' | ';' | '<' | '=' | '>' | '?' | '[' | '\\'
            | ']' | '|' => '_',
            other => other,
        })
        .take(11)
        .collect::<String>();
    if cleaned.trim().is_empty() {
        bytes.copy_from_slice(b"NO NAME    ");
    } else {
        bytes[..cleaned.len()].copy_from_slice(cleaned.as_bytes());
    }
    bytes
}

fn put16(sector: &mut [u8], at: usize, value: u16) {
    sector[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(sector: &mut [u8], at: usize, value: u32) {
    sector[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

pub(crate) fn boot_sector(
    layout: &Layout,
    hidden_sectors: u32,
    volume_id: u32,
    label: &str,
) -> Vec<u8> {
    let mut sector = vec![0u8; layout.bytes_per_sector as usize];
    sector[..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    sector[3..11].copy_from_slice(b"MSWIN4.1");
    put16(&mut sector, 11, layout.bytes_per_sector as u16);
    sector[13] = layout.sectors_per_cluster as u8;
    put16(&mut sector, 14, layout.reserved_sectors as u16);
    sector[16] = FAT_COUNT as u8;
    sector[21] = 0xF8;
    put16(&mut sector, 24, 63);
    put16(&mut sector, 26, 255);
    put32(&mut sector, 28, hidden_sectors);
    put32(&mut sector, 32, layout.total_sectors);
    put32(&mut sector, 36, layout.fat_sectors);
    put32(&mut sector, 44, 2);
    put16(&mut sector, 48, 1);
    put16(&mut sector, 50, BACKUP_BOOT_SECTOR as u16);
    sector[64] = 0x80;
    sector[66] = 0x29;
    put32(&mut sector, 67, volume_id);
    sector[71..82].copy_from_slice(&bpb_label(label));
    sector[82..90].copy_from_slice(b"FAT32   ");
    sector[510] = 0x55;
    sector[511] = 0xAA;
    let end = sector.len();
    sector[end - 2] = 0x55;
    sector[end - 1] = 0xAA;
    sector
}

pub(crate) fn fs_info(layout: &Layout) -> Vec<u8> {
    let mut sector = vec![0u8; layout.bytes_per_sector as usize];
    put32(&mut sector, 0, 0x4161_5252);
    put32(&mut sector, 484, 0x6141_7272);
    // The root directory takes cluster 2.
    put32(&mut sector, 488, layout.clusters - 1);
    put32(&mut sector, 492, 3);
    put32(&mut sector, 508, 0xAA55_0000);
    sector
}

pub(crate) fn first_fat_sector(layout: &Layout) -> Vec<u8> {
    let mut sector = vec![0u8; layout.bytes_per_sector as usize];
    put32(&mut sector, 0, 0x0FFF_FFF8);
    put32(&mut sector, 4, 0x0FFF_FFFF);
    put32(&mut sector, 8, 0x0FFF_FFFF);
    sector
}

/// Write a fresh FAT32 file system through `write_at(byte offset, bytes)`.
/// Every write is a whole number of sectors at a sector-aligned offset.
pub(crate) fn write(
    layout: &Layout,
    hidden_sectors: u32,
    volume_id: u32,
    label: &str,
    cancel: &CancellationToken,
    write_at: &mut dyn FnMut(u64, &[u8]) -> io::Result<()>,
) -> Result<(), HelperError> {
    let sector = u64::from(layout.bytes_per_sector);
    let zeros = vec![0u8; MIB as usize];
    let system = layout.system_sectors() * sector;
    let mut offset = 0;
    while offset < system {
        check_cancel(cancel)?;
        let length = (system - offset).min(MIB) as usize;
        write_at(offset, &zeros[..length])?;
        offset += length as u64;
    }
    let boot = boot_sector(layout, hidden_sectors, volume_id, label);
    let info = fs_info(layout);
    for start in [0, u64::from(BACKUP_BOOT_SECTOR)] {
        write_at(start * sector, &boot)?;
        write_at((start + 1) * sector, &info)?;
    }
    let fat = first_fat_sector(layout);
    for index in 0..u64::from(FAT_COUNT) {
        let start = u64::from(layout.reserved_sectors) + index * u64::from(layout.fat_sectors);
        write_at(start * sector, &fat)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16_at(bytes: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([bytes[at], bytes[at + 1]])
    }

    fn u32_at(bytes: &[u8], at: usize) -> u32 {
        u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
    }

    #[test]
    fn large_volumes_get_microsoft_defaults_and_aligned_data() {
        for (size, sector) in [(64 * GIB, 512), (128 * GIB, 512), (256 * GIB, 4096)] {
            let layout = plan(size, sector, None).expect("FAT32 layout");
            assert_eq!(
                layout.sectors_per_cluster * sector,
                default_cluster_size(size)
            );
            let data_start = u64::from(layout.reserved_sectors)
                + u64::from(FAT_COUNT) * u64::from(layout.fat_sectors);
            assert_eq!(data_start * u64::from(sector) % MIB, 0);
            assert!(u64::from(layout.clusters) >= MIN_CLUSTERS);
            assert!(
                u64::from(layout.clusters) * 4 <= u64::from(layout.fat_sectors) * u64::from(sector)
            );
        }
        assert_eq!(default_cluster_size(64 * GIB), 32 * 1024);
        assert_eq!(default_cluster_size(3 * 1024 * GIB), 64 * 1024);
    }

    #[test]
    fn impossible_volumes_are_refused() {
        assert!(plan(16 * MIB, 512, None).is_err());
        assert!(plan(3 * 1024 * GIB, 512, None).is_err());
        assert!(plan(64 * GIB, 512, Some(1000)).is_err());
        assert!(plan(64 * GIB, 520, None).is_err());
        // Too few clusters to be FAT32.
        assert!(plan(64 * MIB, 512, Some(65_536)).is_err());
    }

    #[test]
    fn labels_follow_the_bpb_rules() {
        assert_eq!(&bpb_label("rufust"), b"RUFUST     ");
        assert_eq!(&bpb_label(""), b"NO NAME    ");
        assert_eq!(&bpb_label("a.b:c"), b"A_B_C      ");
        assert_eq!(&bpb_label("ABCDEFGHIJKLMN"), b"ABCDEFGHIJK");
    }

    #[test]
    fn writes_a_mountable_layout() {
        let size = 40 * MIB;
        let layout = plan(size, 512, Some(512)).expect("layout");
        let mut disk = vec![0xEEu8; size as usize];
        write(
            &layout,
            2048,
            0x1234_5678,
            "Rufust",
            &CancellationToken::new(),
            &mut |offset, bytes| {
                assert_eq!(offset % 512, 0);
                assert_eq!(bytes.len() % 512, 0);
                let at = offset as usize;
                disk[at..at + bytes.len()].copy_from_slice(bytes);
                Ok(())
            },
        )
        .expect("format");

        let boot = &disk[..512];
        assert_eq!(&boot[510..], &[0x55, 0xAA]);
        assert_eq!(u16_at(boot, 11), 512);
        assert_eq!(u16_at(boot, 14), layout.reserved_sectors as u16);
        assert_eq!(u32_at(boot, 28), 2048);
        assert_eq!(u32_at(boot, 32), (size / 512) as u32);
        assert_eq!(u32_at(boot, 36), layout.fat_sectors);
        assert_eq!(u32_at(boot, 67), 0x1234_5678);
        assert_eq!(&boot[71..82], b"RUFUST     ");
        assert_eq!(&boot[82..90], b"FAT32   ");
        assert_eq!(&disk[6 * 512..7 * 512], boot);

        let info = &disk[512..1024];
        assert_eq!(u32_at(info, 0), 0x4161_5252);
        assert_eq!(u32_at(info, 484), 0x6141_7272);
        assert_eq!(u32_at(info, 488), layout.clusters - 1);
        assert_eq!(u32_at(info, 508), 0xAA55_0000);

        for index in 0..FAT_COUNT {
            let at = ((layout.reserved_sectors + index * layout.fat_sectors) * 512) as usize;
            assert_eq!(u32_at(&disk, at), 0x0FFF_FFF8);
            assert_eq!(u32_at(&disk, at + 8), 0x0FFF_FFFF);
            assert!(disk[at + 12..at + 512].iter().all(|byte| *byte == 0));
        }
        // The root directory cluster is empty, and nothing past it was touched.
        let root = (layout.system_sectors() - u64::from(layout.sectors_per_cluster)) * 512;
        let end = layout.system_sectors() * 512;
        assert!(disk[root as usize..end as usize].iter().all(|b| *b == 0));
        assert_eq!(disk[end as usize], 0xEE);
    }

    #[test]
    fn cancellation_stops_before_the_boot_sector() {
        let layout = plan(40 * MIB, 512, Some(512)).expect("layout");
        let cancel = CancellationToken::new();
        cancel.request();
        let result = write(&layout, 0, 1, "", &cancel, &mut |_, _| Ok(()));
        assert!(matches!(result, Err(HelperError::Cancelled)));
    }
}
