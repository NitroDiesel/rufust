# Capability and compatibility matrix

Rufust preserves upstream concepts where each platform has a safe implementation. The tables first describe Linux; the [Windows](#windows) section lists what differs there. It does not claim that a visible option works until it can be completed end to end. **Available** means enabled in this release; **blocked** means the input is recognized but Start remains disabled with a reason; **planned** means no complete product flow exists yet.

## Available in 1.0

| Capability | Linux implementation | Notes |
|---|---|---|
| Removable-device discovery | Linux sysfs and `/proc/self/mountinfo` | Automatic hotplug refresh plus a manual fallback; root, boot, home, read-only, held and unstable targets are rejected. USB HDDs and fixed disks are explicit expert opt-ins. |
| Authorized destructive operations | The system udisks2 daemon, driven as the desktop user; the short-lived `/usr/libexec/rufust-helper` through `pkexec` where udisks2 is not running | Nothing of ours runs as root on the udisks2 path. udisks2 applies its own polkit policy: partitioning, formatting, and mounting a removable drive need no password on an active local session; opening the raw device (image writes, bad-block tests, BIOS boot code) asks once. Target identity is resolved again immediately before writing. |
| Raw image write | Bounded streaming copy to an exclusively locked target | Supports `.img`, `.raw`, and other raw disk images. Source size, target identity and target capacity are rechecked. |
| ISOHybrid disk-image write | Same raw writer | Hybrid ISO media is written byte-for-byte. |
| Windows installer media | In-process UDF/ISO 9660 extraction onto udisks2-created partitions | Upstream layout: NTFS or exFAT plus a 1 MiB UEFI:NTFS partition (GPT basic data named `UEFI:NTFS` with the no-drive-letter attribute, or MBR type 0xEF), or a single FAT32 partition when every file fits. MBR "BIOS or UEFI" adds the Windows 7 MBR, an active partition, and the ms-sys NTFS/FAT32 boot record. Every copied file is hashed and read back. Needs udisks2. |
| Windows User Experience options | Generated answer file written with the Windows media | Upstream's dialog on Start for Windows 10/11 ISOs: bypass TPM/Secure Boot/RAM, no online account, local account, this computer's regional options, no data collection, silent erase-and-install, no BitLocker, QoL improvements, SkuSiPolicy.p7b, and S-Mode (Advanced open). `autounattend.xml` goes to the media root, or `sources/$OEM$/$$/Panther/unattend.xml` without Setup-pass options. The bypass also empties `sources/appraiserres.dll` and, for build 26000+, adds upstream's signed Setup wrapper. Added files are read back like the rest. |
| Compressed raw images | Fixed-path gzip, bzip2, xz/lzma, zstd and bsdtar providers | Decompressed output is capacity-bounded. Decoder failure or size mismatch is fatal. ZIP input should contain one disk image. |
| VHD/VHDX conversion | Read-only `qemu-nbd` + `nbdcopy` stream of a protected source copy | Requires `qemu-nbd`, `nbdinfo`, and `nbdcopy`. Container bytes are never copied as a disk image. Parent-dependent, 4Kn, journal-replay, and ambiguous-CHS images stay rejected. |
| Write verification | SHA-256 of the bytes streamed, followed by target readback | Success is reported only after hash match, `fsync`, and block cache flush. |
| Cancellation | Separate helper process termination with UI event polling | Cancellation leaves an explicit warning that the target may be incomplete. |
| MBR/GPT/super-floppy formatting | udisks2 (`parted` on the helper path) | Super-floppy correctly formats the whole device. |
| FAT/FAT32, exFAT, NTFS, UDF, ext2/3/4 | Distribution formatter tools, run by udisks2 or the helper | Only installed providers appear. Cluster sizes and FAT16 need udisks2 2.10 or newer (`mkfs-args`). Slow format zeroes the device first. |
| Bad-block overwrite test | Four-pattern write/read test (0xAA, 0x55, 0xFF, 0x00) like `badblocks -w` | Separate destructive confirmation. Built in on the udisks2 path; the native helper uses e2fsprogs `badblocks`. |
| MD5, SHA-1, SHA-256, SHA-512 | RustCrypto | Runs off the UI thread. MD5/SHA-1 are comparison hashes, never trust decisions. |
| Light and dark presentation | Device Workbench Slint UI | Fixed-size 560x720 dialog laid out like Rufus; content scrolls when Expert options are open. Device identity is shown in the device list and the destructive confirmation, with a continuous write-progress track. |
| Arch, Debian and Fedora integration | PKGBUILD, complete Debian metadata, RPM spec, desktop/AppStream/polkit metadata | Release URLs and checksums are finalized by release automation. |
| Portable x86_64 desktop | AppImage built against glibc 2.28 with FUSE extraction fallback | Plug and play: inspection, checksums, writing, formatting, and Windows media work with nothing installed beyond the host's udisks2. |

## Recognized but blocked

| Capability | Why it is blocked |
|---|---|
| Non-hybrid Linux ISO file-copy media | Extraction exists; Syslinux/GRUB installation and per-distro boot tests do not. |
| Windows media on FAT32 with a WIM over 4 GB | Upstream splits `install.wim`; that needs a WIM splitter. Start explains to choose NTFS. |
| Windows media through the native helper alone | Windows media is built through udisks2 only. |
| Windows To Go | A WIM/ESD cannot be raw-copied. A real implementation needs partition, wimlib apply, BCD and offline-registry work. |
| Parent-dependent, 4Kn, journal-replay, or CHS-mismatched VHD/VHDX | The conversion path refuses these instead of repairing the source or copying container bytes. See [virtual disk work](VIRTUAL_DISKS.md). |
| FFU apply/capture | No maintained, independently verifiable Linux servicing provider has been selected. |
| ReFS creation | Linux has no safe production ReFS formatter. |
| FreeDOS | Redistributable system files and exact boot-sector provenance are not packaged yet. |
| MS-DOS | Proprietary Microsoft system files are never bundled or fetched. |
| Linux persistence | Partition layout and distro-specific `persistence.conf`/casper behavior need image fixtures and boot tests. |
| Syslinux, GRUB2, GRUB4DOS and ReactOS installation | Planning types exist, but no incomplete bootloader path is exposed as success. |
| Windows CA 2023 signed bootloaders | Needs `EFI_EX`/`Fonts_EX` extracted from the LZX-compressed `boot.wim`; the option is not shown yet. |
| UEFI runtime validation and Secure Boot revocation checks | Verified payload, SBAT/SVN/DBX parsing and signed update data are not packaged. |
| Drive capture | Root must write through a user-opened file descriptor; arbitrary root-owned output paths are intentionally rejected. |

## Windows

Windows 10 and 11 (x64) run the same interface, planner, protocol, and
Windows-media code; the platform layer is described in
[Windows](WINDOWS.md). Unless the table says otherwise, a capability above is
available on Windows with the same checks, and every Linux block below still
applies.

| Capability | Windows implementation | Notes |
|---|---|---|
| Removable-device discovery | Disk interface enumeration and storage IOCTLs, without elevation | The disk holding Windows, the system partition, page files, mounted volumes, and spanned volumes are marked; virtual disks are never listed. Hot-plug polling of disks and volumes. |
| Authorized destructive operations | An elevated copy of `rufust.exe`, approved through UAC per operation and driven over private named pipes | The window never runs as administrator. Pipe clients must be the started worker. Target identity (interface path, disk number, size, sector size, serial, model) is resolved again before writing. |
| Exclusive access | Every target volume is locked and dismounted before raw I/O | A volume in use is retried, then force-dismounted; a volume that still cannot be locked stops the operation. |
| MBR/GPT/super-floppy formatting | `IOCTL_DISK_CREATE_DISK` / `SET_DRIVE_LAYOUT_EX` | Super floppy needs removable media, where Windows mounts a whole-disk volume. |
| FAT, FAT32, exFAT, NTFS, UDF | Windows' `fmifs.dll` `FormatEx`; FAT32 over 32 GB by a built-in formatter after upstream's `FormatLargeFAT32` | ext2/3/4 and ReFS are not offered on Windows. |
| Compressed raw images | In-process gzip, bzip2, xz, lzma, zstd, and single-file zip decoders | Capacity-bounded; decoding errors and size mismatches are fatal. Archives with several files are refused. |
| VHD/VHDX writing | Attached read-only through `virtdisk.dll` | Standalone images with 512-byte sectors; differencing disks are refused. The preview reads the capacity without elevation. |
| Windows installer media | Same layout and boot records | The MBR UEFI:NTFS partition becomes type 0xEF once its files are in. Copied files are read back after a dismount. |
| Bad-block test, verification, cancellation | Built into the engine | Cancellation is a `cancel` line on the control pipe or the window closing it. |
| Light and dark presentation | Same Slint workbench | Follows the Windows app mode; `%APPDATA%\Rufust\settings.conf`. |
| Installer | `rufust-<version>-windows-x86_64-setup.exe` (Inno Setup) | Per-machine install into Program Files with a Start menu entry and uninstaller; not code-signed yet. |
| Portable desktop | `rufust-<version>-windows-x86_64.exe` | The executable itself, nothing to extract, with an `asInvoker` manifest; not code-signed yet. |

Blocked only on Windows: ext2/3/4 creation (no Windows formatter) and ReFS
creation (only Pro for Workstations, Enterprise, and Server editions allow it,
through VDS).

## Changes in 1.2.1

The warning under the device picker appears only for unusual targets and
names the reason: an internal disk, an external hard disk or SSD, or a drive
Rufust refuses (system, boot, swap or page file, RAID/LVM/multi-disk member,
read-only, or no usable identity). Mounted volumes no longer trigger it, so an
ordinary USB flash drive with a drive letter or desktop automount shows no
warning; the engine still unmounts or dismounts them before writing.

## Changes in 1.2.0

Rufust now runs on Windows 10 and 11 (x64), alongside the Linux build, with
the capabilities in the [Windows](#windows) table. The window stays
unprivileged and each operation runs in an elevated copy of itself after one
UAC approval. Elevated tests on throwaway VHDX disks format with every
provider and scheme, write raw, gzip, and VHDX images with read-back
verification, build Windows installer media, run large FAT32, and cancel
through the worker pipes. Releases include a Windows installer
(`rufust-<version>-windows-x86_64-setup.exe`) and a portable executable
(`rufust-<version>-windows-x86_64.exe`). Linux behavior is unchanged.

## Changes in 1.1.0

Rufus Linux is renamed **Rufust**: the application, binaries (`rufust`,
`rufust-helper`), crates, desktop ID `io.github.nitrodiesel.rufust`, polkit
policy, packages, and repository. The Debian, Fedora, and Arch packages replace
an installed `rufus-linux`, AppStream lists the old ID under `<replaces>`, and
preferences saved under `~/.config/rufus-linux` are read when no Rufust
settings exist yet. The default volume label is now `RUFUST`. No feature
changed.

## Changes in 1.0.1

The Status card's size menu adds binary KiB, MiB and GiB. KB, MB and GB are
now decimal (1000-based), so a fixed unit always means what its name says.
Auto still scales in binary steps and now labels them KiB, MiB and GiB.

## Changes in 1.0.0

Start now shows upstream's **Windows User Experience** dialog for Windows 10
and 11 installer ISOs, with the options upstream offers for the image's build
(read from the `install.wim`/`install.esd` index) and its tooltips in a
details area. The choices become an answer file on the media and are listed in
the destructive confirmation. Silent install asks for the edition and the three
acknowledgements, and appends ` (SILENT)` to the label. Choices other than
silent install and S-Mode persist. Regional options come from the session
locale, the configured XKB layout, and the time zone, mapped to Windows names
through Unicode CLDR data. Media made from a Windows 11 build 26300 ISO with
the bypass and silent install booted in QEMU (UEFI, no TPM, 2.5 GB of RAM) and
went straight to "Installing Windows 11" without a prompt.

## Changes in 0.1.8

Progress readouts name their units. Byte stages show transferred and total
size, transfer rate, and time left, such as
`380.2 MB / 8.42 GB · 40.0 MB/s · 3:26 left`; other stages read "Step 3 of 7".
The Size (Auto, Bytes, KB, MB, GB) and Speed (KB/s, Kbps, MB/s, Mbps, GB/s,
Gbps) choices in the Status header persist in
`$XDG_CONFIG_HOME/rufust/settings.conf`. Byte multiples are binary, like
the device list; bit rates are decimal, as network speeds are quoted.

## Changes in 0.1.7

The partition scheme list follows upstream Rufus: **Super Floppy Disk** (the
filesystem on the whole device, no partition table) is offered only when Boot
selection is "Non bootable", under upstream's name. Leaving "Non bootable"
with it selected returns to GPT, the startup default.

## Changes in 0.1.6

Writing and formatting run through the system udisks2 daemon as the desktop
user, so the AppImage works with nothing else installed; native packages fall
back to their helper only where udisks2 is not running. Windows installer ISOs
(found by their contents, not their names) are written in upstream Rufus's
layout: NTFS by default, with the signed UEFI:NTFS partition for UEFI, or
FAT32 when no file exceeds 4 GB, plus Windows 7 MBR and NTFS/FAT32 boot
records for MBR "BIOS or UEFI" targets. Choosing a scheme sets the matching
target system. Loop-device tests run the real daemon for GPT/MBR with NTFS,
exFAT and FAT32, and OVMF and SeaBIOS boot the results through UEFI:NTFS and
the BIOS boot records. Release titles are now `v<version>`.

## Changes in 0.1.5

Selecting an ISO proposes its volume name as the USB label, as upstream Rufus
does: the UDF logical volume identifier (Windows media) or else the ISO 9660
volume identifier. A label the user typed is kept. Labels are converted to
what the chosen file system accepts, following upstream `ToValidLabel`; FAT
labels keep 11 uppercase characters, so a Windows 11 label is stored as
`CCCOMA_X64F` on FAT32. The form shows the stored label when it differs. Typed
labels are now synchronized immediately instead of only at Start.

## Changes in 0.1.4

The desktop workbench adopts a T3 Code–style skin in a fixed-size,
non-maximizable window laid out like upstream Rufus. It fixes centered section
titles, device details, and image controls; clipped action labels; and cards
hidden under the scrollbar. No capability moved between available, blocked, and
planned.

## Changes in 0.1.3

VHD recognition checks footer signatures as well as the leading signature and
filename. Renaming a fixed VHD to `.img` no longer offers it as raw media.
Native packages install `qemu-nbd`, `nbdinfo`, and `nbdcopy`. With those tools
present, Start writes a converted standalone VHD/VHDX through the existing
bounded writer. Missing tools keep Start disabled with an install remedy.
AppImage writes still need the matching native helper; the AppImage does not
bundle those providers. This does not prove every guest will boot.

## Planned secondary workflows

- Optical disc or mounted media to ISO through read-only providers.
- Signed in-app download catalog with explicit image selection.
- Package-manager-aware update policy.
- Full gettext/Fluent catalogs and RTL layouts.
- Settings persistence, log export, image drag-and-drop, and CLI preselection.
- VHD/VHDX capture after safe file-descriptor passing is available.

## Expert controls

USB hard drives and fixed/internal disks remain separated from ordinary removable devices and require an explicit session opt-in. Enabling visibility never bypasses root/boot/home, swap, holder, identity, source-on-target, size, unmount, lock, or flush checks.

Controls that weaken the safety boundary—ignoring size checks, shared writes, silent target selection, or arbitrary helper commands—are not accepted as feature-parity requirements.
