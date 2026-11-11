# Third-party software and assets

This file is a packaging checklist, not a substitute for the license texts shipped by a distribution. Packagers must generate a complete dependency and asset inventory for each release.

## Upstream Rufus

Portions derived from the Rufus source tree are licensed under **GPL-3.0-or-later**. Preserve upstream copyright headers, the GPL text, modification notices, and complete corresponding source. The upstream project name and logo may also be subject to trademark rules separate from the GPL.

The application icons under `assets/icons/rufust-*.png` are unmodified,
independently sized copies of upstream Rufus `res/icons/rufus-*.png`, anchored
to upstream content commit `2d63a109200a6921d803f824e8d4fee08ff9bd78`.
Upstream's asset-specific notice places `rufus*.*` in the **Public Domain**,
courtesy of PC Unleashed. See `assets/icons/LICENSE.txt`. This independent
port's non-endorsement notice remains prominent in the README, AppStream
description, and application About text.

## Test fixtures

The helper's test-only VHDX journal fixture comes from QEMU iotest 070,
Copyright 2013 Red Hat, Inc., GPL-2.0-or-later. Its exact source, digest, and
license are recorded in `crates/rufust-helper/tests/fixtures/README.md` and
`COPYING.QEMU` in that directory. It is not included in installed binaries.

The parent-chain fixtures in the same directory contain project-generated
synthetic bytes. Their optional generator uses the MIT-licensed LTRData
DiscUtils writer, pinned with dependency hashes and source provenance under
`scripts/fixtures/parent-chains/`. No .NET or DiscUtils binary is shipped in Rufust.

## Expected system dependencies

These tools and libraries are intended to remain system dependencies on Linux rather than copied into this repository:

| Component | Typical license | Purpose |
|---|---|---|
| Slint | GPL-3.0-only or commercial | Declarative desktop interface |
| winit and Slint render backends | Apache-2.0/MIT and component licenses | Wayland/X11 windowing and rendering |
| Fontconfig/FreeType | MIT-style and FTL/GPL | System font discovery and rendering |
| udisks2 | GPL-2.0-or-later / LGPL-2.0-or-later | Partitioning, formatting, mounting, and authorized raw access |
| zbus (crate) | MIT | D-Bus client for udisks2 |
| polkit | LGPL-2.0-or-later | Explicit privilege authorization |
| util-linux | GPL/LGPL components | Block flush, unmount, and swap management |
| GNU Parted | GPL-3.0-or-later | MBR/GPT partition creation |
| dosfstools | GPL-3.0-or-later | FAT creation/checking |
| exfatprogs | GPL-2.0-or-later | exFAT creation/checking |
| e2fsprogs | GPL/LGPL components | ext2/ext3/ext4 creation/checking |
| ntfs-3g/ntfsprogs | GPL-2.0-or-later | NTFS creation and access |
| udftools | GPL-2.0-or-later | UDF creation |
| libarchive | BSD-2-Clause | ZIP-compressed raw-image extraction |
| gzip, bzip2, xz, zstd | Various free-software licenses | Compressed raw-image decoding |

License versions above are orientation only. The installed package's license metadata is authoritative.

## Windows build

The Windows executable statically links these crates in addition to the
shared ones; their license texts ship with the crates:

| Component | License | Purpose |
|---|---|---|
| windows-sys | MIT or Apache-2.0 | Win32 bindings |
| flate2 (miniz_oxide) | MIT or Apache-2.0 | gzip, zip deflate |
| bzip2 (libbz2-rs-sys) | MIT or Apache-2.0; bzip2-1.0.6 | bzip2 |
| lzma-rust2 | Apache-2.0 | xz, lzma |
| ruzstd | MIT | zstd |
| embed-resource (build only) | MIT | Icon and manifest resources |

It calls, but never redistributes, Windows components: `fmifs.dll`
(`FormatEx`), `virtdisk.dll`, the storage and disk IOCTLs, the common file
dialog, and UAC. The Windows zip ships `LICENSE.txt` and this file.

## Bundled boot assets

- `crates/rufust-helper/assets/uefi-ntfs/`: the unchanged contents of upstream
  Rufus `res/uefi/uefi-ntfs.img` (SHA-256
  `72683fa1250eeea772d3399277b434d4e55ba8dd0dc926e52d817e701fc2eb9e`):
  Secure Boot signed UEFI:NTFS 2.8 bootloaders and ntfs-3g 1.9 drivers
  (GPL-2.0-or-later) and EfiFs 1.12 exFAT drivers (GPL-3.0). Sources:
  <https://github.com/pbatard/uefi-ntfs>, <https://github.com/pbatard/efifs>.
  Per-file digests are in `PROVENANCE.md` there; a unit test pins them.
- `crates/rufust-helper/assets/ms-sys/`: Windows 7 MBR and NTFS/FAT32 boot
  record byte arrays from upstream Rufus `src/ms-sys/inc/` (ms-sys,
  GPL-2.0-or-later), converted unchanged; see `PROVENANCE.md` there.
- `crates/rufust-helper/assets/setup/`: upstream Rufus's signed Windows 11
  Setup wrapper (`res/setup/setup_x64.exe`, `setup_arm64.exe`) and its source
  `setup.c` (GPL-3.0-or-later), unchanged. Digests are in `PROVENANCE.md`
  there; a unit test pins them.
- `apps/rufust/assets/windows-zones.txt`: IANA to Windows time zone
  names from Unicode CLDR `windowsZones.xml` (Unicode License v3), with
  aliases from the IANA tz database's links (public domain).

## Boot assets policy

FreeDOS, Syslinux, GRUB, GRUB4DOS, ReactOS, and UEFI:NTFS assets each carry their own licenses and source-offer requirements. Do not add a binary boot asset without:

1. its exact source/version and download URL;
2. its license text in `assets/licenses/`;
3. a reproducible way to obtain or rebuild it;
4. a recorded cryptographic digest; and
5. confirmation that modification does not invalidate a required Secure Boot signature.

## Microsoft material

Do **not** commit or redistribute MS-DOS files, `diskcopy.dll`, Windows ISO/WIM/ESD/FFU images, Windows bootloaders, `oscdimg.exe`, ADK files, or other Microsoft binaries. They are not covered by this project's GPL license. User-supplied material remains subject to its original license, and the user is responsible for having a valid Windows license where required.

## Translation reuse

Translations copied or adapted from upstream Rufus are derivative GPL material. Preserve translator credits and identify materially changed strings so they can be reviewed by native speakers.
