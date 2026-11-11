# Rufust

Rufust is an **independent, community-built Linux and Windows port inspired by Rufus**. It provides a native desktop workflow for inspecting, formatting, and writing removable media. It is not produced, endorsed, or supported by the upstream Rufus project or its maintainers.

> **Destructive operation warning**
>
> Writing or formatting a device permanently destroys data on that device. The application deliberately hides system disks and most USB hard disks by default, revalidates a device immediately before writing, and requires an explicit confirmation that names the target.

<p align="center">
  <img src="docs/images/rufust-light.png" width="420" alt="Rufust in the light theme, ready to write a Windows 11 ISO to a USB drive">
  &nbsp;
  <img src="docs/images/rufust-dark.png" width="420" alt="Rufust in the dark theme, copying Windows installer files with size, speed, and time left">
</p>

## Project status

Rufust is a native implementation for Linux and Windows 10/11, written in Rust; it is not a rebuild of the upstream Windows executable. On both systems it creates:

- Windows 10/11 installer drives from Windows ISOs. Files over 4 GB are supported through NTFS and the Secure Boot signed UEFI:NTFS loader, with BIOS boot records on MBR.
- Bootable drives from ISOHybrid Linux ISOs, raw and compressed disk images, and VHD/VHDX.
- Formatted drives in FAT, FAT32, exFAT, NTFS, UDF, or ext2/3/4, with optional verification and bad-block checks.

Options that still need an unfinished workflow are recognized and blocked with a direct reason. These include non-hybrid Linux ISOs, FreeDOS, Windows To Go, and persistence. [PARITY.md](docs/PARITY.md) tracks every upstream Rufus feature, and the [capability matrix](docs/CAPABILITIES.md) is the authority for each release.

The goal is output compatibility where each platform has a safe, maintained implementation. It is not to emulate Windows internals with unsafe shortcuts on Linux. On Windows, ext2/3/4 and ReFS creation are not offered; everything else above works the same.

## Current product direction

- Native Slint desktop interface on Wayland, X11, and Windows, with light and dark themes that follow the system.
- Automatic sysfs-backed device discovery with mount and device-holder safety checks.
- Plug and play: writes go through the system udisks2 service, so nothing extra has to be installed. A small polkit-authorized helper is the fallback.
- On Windows: a single portable `rufust.exe`. The window stays unprivileged; each operation runs in an elevated worker after one UAC approval, using Windows' own partitioning and file system providers plus a built-in large-FAT32 formatter.
- MBR/GPT partitioning, raw image writing, verification, and common Linux/portable filesystems.
- ISO/ISOHybrid and compressed-image analysis with clear compatibility and safety feedback.
- Logged, cancellable operations with a final flush before a device is reported ready.

The visual language is intentionally based on the physical act of preparing media: device identity is prominent, destructive state is unmistakable, and advanced controls stay quiet until requested. A progress “write track” is the signature element; it reflects the actual stages—prepare, write, verify, flush—rather than showing decorative motion.

## Build and run

The application is written in Rust with Slint. Slint renders the interface while the platform backend integrates with native Wayland, X11, or Windows windows, input, fonts, accessibility, and the desktop theme.

### Debian or Ubuntu

```sh
sudo apt install build-essential cargo rustc pkg-config libfontconfig1-dev libfreetype-dev \
  libxkbcommon-dev libwayland-dev libx11-dev libxcb1-dev polkitd pkexec
cargo build
cargo run
```

### Fedora

```sh
sudo dnf install cargo rust fontconfig-devel freetype-devel libxkbcommon-devel \
  wayland-devel libX11-devel libxcb-devel polkit pkgconf-pkg-config
cargo build
cargo run
```

### Arch Linux

```sh
sudo pacman -S --needed base-devel rust fontconfig freetype2 libxkbcommon wayland \
  libx11 libxcb polkit
cargo build
cargo run
```

### Windows

Install [Rust](https://rustup.rs/) with the MSVC toolchain and Visual Studio
Build Tools ("Desktop development with C++"). No other libraries are needed.

```powershell
cargo build --release -p rufust
target\release\rufust.exe
```

Run the window as yourself, never "as administrator": Windows asks for
approval at Start, and only the disk work runs elevated. See
[Windows](docs/WINDOWS.md) for the design and the virtual-disk test script.

For the full formatter and image-tool set, install the runtime dependencies listed in [Building and packaging](docs/BUILDING.md). During development, raw writes should be tested against disposable image files or loop devices, never a disk containing useful data.

## Install a release

Download the package for your x86_64 distribution, or the Windows zip, from
[GitHub Releases](https://github.com/NitroDiesel/rufust/releases).
Keep `SHA256SUMS` beside the downloaded package and verify it before installing:

```sh
sha256sum -c SHA256SUMS
```

Install the matching native package:

```sh
# Debian or Ubuntu
sudo apt install ./rufust_*_amd64.deb

# Fedora
sudo dnf install ./rufust-*.x86_64.rpm

# Arch Linux
sudo pacman -U ./rufust-*-x86_64.pkg.tar.zst
```

Or run the portable desktop build on an x86_64 glibc 2.28+ system:

```sh
chmod +x ./rufust-*-x86_64.AppImage
./rufust-*-x86_64.AppImage
```

If FUSE is unavailable, launch it with `--appimage-extract-and-run`. The
AppImage writes and formats through the udisks2 service that desktop
distributions already run, so it needs no installation and contains no
privileged code. When udisks2 is absent, a native package's helper is used
instead.

The native packages install the desktop application, privileged helper,
polkit policy, desktop metadata, and required runtime dependencies. Each
release is stable within its documented capability set; review the
[capability matrix](docs/CAPABILITIES.md) before writing to removable media.

### Windows

Download `rufust-*-windows-x86_64.zip` (Windows 10/11, x64), check it against
`SHA256SUMS`, extract it, and run `rufust.exe`. Nothing is installed.

```powershell
Get-FileHash .\rufust-*-windows-x86_64.zip -Algorithm SHA256
```

The window runs with your normal rights. When you confirm an operation,
Windows asks once for administrator approval, and a separate elevated copy of
`rufust.exe` does the disk work; declining writes nothing. The executable is
not code-signed yet, so SmartScreen may ask you to confirm the first launch.
Windows builds are published from the first release after 1.1.0.

`SHA256SUMS` detects download corruption. GitHub build provenance attached to
the release provides a separate publisher-verification path.

## Install layout

Distribution packages should use these paths:

| Artifact | Destination |
|---|---|
| Desktop application | `/usr/bin/rufust` |
| Privileged helper | `/usr/libexec/rufust-helper` |
| Desktop entry | `/usr/share/applications/io.github.nitrodiesel.rufust.desktop` |
| AppStream metadata | `/usr/share/metainfo/io.github.nitrodiesel.rufust.metainfo.xml` |
| Polkit policy | `/usr/share/polkit-1/actions/io.github.nitrodiesel.rufust.policy` |
| Runtime directory rule | `/usr/lib/tmpfiles.d/rufust.conf` |

The helper path is a packaging contract. Until the helper is present, packages must not install the polkit policy or imply that destructive operations are available.

## Feature notes

- **ReFS:** Linux has no safe, production-quality ReFS formatter, and Windows allows ReFS creation only on Pro for Workstations, Enterprise, and Server through VDS. ReFS creation is unavailable on both; existing ReFS media may be identified read-only.
- **ext2/3/4 on Windows:** Windows has no ext formatter, so these file systems are offered only on Linux.
- **MS-DOS:** Microsoft DOS system files are proprietary and are not distributed. A future workflow may accept user-supplied, lawfully obtained files.
- **FFU:** Windows FFU capture/apply relies on Windows servicing components. It is not promised until a maintained, independently verifiable Linux implementation exists.
- **Windows To Go:** Creation is recognized but blocked. A safe implementation
  requires WIM application, BCD generation, offline registry work, and boot
  fixtures; merely installing `wimlib` does not enable it.
- **Microsoft downloads:** Windows images, setup files, bootloaders, and tools are never bundled. Any download integration must use Microsoft-hosted sources and verify signed metadata.

## Documentation

- [Capabilities and parity](docs/CAPABILITIES.md)
- [Safety and privilege model](docs/SAFETY.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Building and packaging](docs/BUILDING.md)
- [Windows](docs/WINDOWS.md)

## License and attribution

Rufust is licensed under the GNU General Public License version 3. See [LICENSE.txt](LICENSE.txt). The upstream Rufus source is also GPLv3; derivative portions must retain their copyright notices and corresponding source. See [THIRD_PARTY.md](THIRD_PARTY.md) for dependency and asset obligations.

“Rufus” is used descriptively to identify the project this independent port is based on. Its name and branding are not a claim of upstream affiliation.
