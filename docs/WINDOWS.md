# Windows

Rufust runs natively on Windows 10 and 11 (x64) with the same interface,
operation planner, helper protocol, Windows-media layout, and safety checks as
on Linux. Only the platform layer differs: device discovery, privilege, and
the disk engine. Read this before changing anything under a `windows` module.

## Privilege model

The window never runs as administrator, mirroring the unprivileged desktop on
Linux. Writing to `\\.\PhysicalDriveN` needs administrator rights, so each
operation runs in a separate elevated worker:

```text
rufust.exe (unelevated window)
  -> discover disks with zero-access IOCTL queries
  -> build and confirm the same immutable plan as on Linux
  -> create two single-instance, local-only named pipes
       \\.\pipe\rufust-<pid>-<nonce>-control   (window writes)
       \\.\pipe\rufust-<pid>-<nonce>-events    (window reads)
  -> ShellExecuteEx "runas": rufust.exe --elevated-worker <name>  (one UAC prompt)
  -> accept the pipe clients only if their process ID is that worker's
  -> send one NDJSON HelperRequest; render HelperEvents

rufust.exe --elevated-worker <name> (elevated, no window)
  -> validate the request (rufust_helper::serve_pipe -> execute)
  -> bind the source, revalidate the target, lock and dismount its volumes
  -> partition, format, write, copy, verify, flush
  -> stream events; a "cancel" line or a closed control pipe cancels
```

The worker is the desktop executable itself, so the portable download is one
file, like upstream Rufus, and the installer installs just that file. `rufust-helper.exe --execute-pipe <name>` serves the
same pipes and is what the automated tests launch. Declining UAC writes
nothing. Pipe names accept only letters, digits, and `-`.

## Device discovery and identity

`rufust-platform/src/windows.rs` enumerates `GUID_DEVINTERFACE_DISK` and
queries each disk with `IOCTL_STORAGE_GET_DEVICE_NUMBER`,
`IOCTL_STORAGE_QUERY_PROPERTY`, `IOCTL_DISK_GET_DRIVE_GEOMETRY_EX`, and
`IOCTL_DISK_IS_WRITABLE`, opening it with no access rights so no elevation is
needed. In `DeviceIdentity`:

| Field | Windows meaning |
|---|---|
| `node` | `\\.\PhysicalDriveN` (a display name, not identity) |
| `sysfs_path` | the device interface path, which embeds the PnP instance ID |
| `major` / `minor` | `0` / the disk number |
| `transport` | from `STORAGE_BUS_TYPE`; file-backed and virtual disks are `loop` |

The volume table (`FindFirstVolume`, `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`)
marks the disk holding the Windows directory (`ContainsRoot`), the system
partition named in `HKLM\SYSTEM\Setup` (`ContainsBoot`), a page or swap file
(`ContainsSwap`), mounted volumes (`MountedChildren`), and volumes spanning
several disks (`ActiveVolumeMember`). Virtual disks (attached VHDs) are never
listed, like Linux loop devices. Hot-plug polls disk interfaces and volume
names twice a second.

## Disk engine

`rufust-helper/src/windows/` mirrors the udisks2 engine step for step:

- **Revalidation.** The worker rediscovers the disk and compares the
  interface path, disk number, size, sector size, serial, and model, then
  refuses read-only media and every blocking risk above. The source must not
  live on a volume of the target.
- **Exclusive access.** Every volume on the target is locked
  (`FSCTL_LOCK_VOLUME`, retried, then forced) and dismounted before raw I/O,
  and stays locked until the write is done. Boot records are written through
  the locked, still-mounted volume, as upstream does.
- **Partitioning.** `IOCTL_DISK_CREATE_DISK` and
  `IOCTL_DISK_SET_DRIVE_LAYOUT_EX` after zeroing the first and last MiB. New
  volumes are found by their extent on the target disk, never by drive letter;
  their letters are removed while formatting (so Explorer does not offer to
  format them) and one is assigned when the job finishes.
- **File systems.** FAT, FAT32, exFAT, NTFS, and UDF come from Windows' own
  provider (`fmifs.dll` `FormatEx`). FAT32 over 32 GB, which Windows refuses,
  uses a built-in formatter ported from upstream's `FormatLargeFAT32`
  (`rufust-helper/src/fat32.rs`). ext2/3/4 and ReFS are not offered.
- **Images.** Raw and ISOHybrid images are copied in whole sectors (the final
  sector is zero-padded). gzip, bzip2, xz, lzma, zstd, and single-file zip
  images are decoded in process by memory-safe Rust decoders. Standalone
  512-byte-sector VHD/VHDX images are attached read-only through
  `virtdisk.dll` and copied from their disk; differencing disks are refused.
- **Windows installer media.** The shared `windows_media.rs` layout, copy,
  customization, and boot-record code. On MBR, the UEFI:NTFS partition is
  created as FAT12 for its files and switched to type 0xEF afterwards.
- **Verification.** Image writes are read back from the disk and hashed;
  copied files are read back after a dismount so the cache is not trusted.

The desktop's VHD/VHDX capacity preview uses `OpenVirtualDisk` with
get-info access, which needs no elevation.

## Desktop integration

- File chooser: the common Open dialog, owned by the workbench window.
- Theme: `RUFUST_THEME`, else the "app mode" (`AppsUseLightTheme`) setting.
- Settings: `%APPDATA%\Rufust\settings.conf`.
- Regional options for the Windows User Experience dialog: the system and user
  locales, display language, keyboard layout, and time zone key name, read
  from Windows as upstream does.
- `rufust.exe` embeds the original Rufus icon and an `asInvoker` manifest
  (per-monitor DPI aware, common controls 6). Release builds use the GUI
  subsystem; `--version` still prints when output is redirected.

## Building

Install Rust with rustup and the MSVC toolchain (Visual Studio Build Tools
with the "Desktop development with C++" workload, which includes the Windows
SDK resource compiler). No other system libraries are needed.

```powershell
cargo build --release -p rufust
target\release\rufust.exe
```

`cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D
warnings` run unelevated. The end-to-end tests need an elevated PowerShell;
they create and attach throwaway VHDX disks and refuse every disk that is not
file-backed virtual storage:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\windows\vhd-tests.ps1
```

They format with every provider and scheme, write raw, gzip, and VHDX images
with read-back verification, build Windows installer media (MBR BIOS+UEFI with
UEFI:NTFS, GPT UEFI, FAT32) from a synthetic ISO, run large FAT32 on a sparse
40 GiB disk, and drive the window's pipe launcher through a format and a
cancellation. CI runs them on every push.

## Packaging

The release workflow builds two downloads with the same four files:
`rufust.exe`, `README.txt` (from `packaging/windows/`), `LICENSE.txt`, and
`THIRD_PARTY.md`.

- `rufust-<version>-windows-x86_64.zip`: portable, nothing installed.
- `rufust-<version>-windows-x86_64-setup.exe`: an Inno Setup installer
  (`packaging/windows/rufust.iss`). It installs per machine into Program Files
  with a Start menu entry, an optional desktop icon, and an uninstaller.
  Because only administrators can write to Program Files, a standard user
  cannot replace the executable that the elevated worker runs. Setup itself
  is elevated, so its "Launch Rufust" option starts the app as the original
  user (`runasoriginaluser`); the window never runs elevated. Uninstalling
  keeps `%APPDATA%\Rufust\settings.conf`.

The job checks the PE machine and the `asInvoker` manifest, installs and
uninstalls the installer silently, and adds both files to `SHA256SUMS` and the
build provenance. Neither the executable nor the installer is
Authenticode-signed yet, so SmartScreen may warn on first launch.

## Open gates

- A smoke test on disposable physical USB media (flash drive and USB SSD):
  format, image write, Windows media, verification, cancellation, and
  unplugging during a write. Virtual disks do not replace it.
- Boot tests of Windows-built media in QEMU/OVMF and on hardware.
- Authenticode signing of release executables and the installer.
- ext2/3/4 and ReFS creation, drive capture, and the other features blocked
  on Linux remain blocked here.
