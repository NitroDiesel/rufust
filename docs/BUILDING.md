# Building and packaging

## Source build

Minimum build dependencies:

- Rust stable toolchain (`cargo`, `rustc`)
- `pkg-config`
- Fontconfig, FreeType, libxkbcommon, and Wayland/X11 development files for Slint's native platform backend
- udisks2 at runtime for writing and formatting (the native helper with polkit
  and `pkexec` is the fallback where udisks2 is not running)

```sh
cargo build
cargo test
cargo run
```

Use `cargo build --release` for packaging. Do not run the complete GUI with `sudo`; privileged steps belong to udisks2 or the helper only.

The desktop pins a direct winit-backend dependency to the same version as Slint
to set the initial native window size before creating the OpenGL drawable. It
reuses the existing renderer dependencies. Keep the Rust initial dimensions and
Slint preferred dimensions together when changing the default window size.

The X11 startup regression requires Xvfb, xauth, xdotool, ImageMagick, Mesa,
libxkbcommon-x11, and a session D-Bus launcher. Ubuntu's `libxkbcommon-dev` does
not supply the X11 runtime; install `libxkbcommon-x11-0` explicitly. Run the test
without a window manager on a private display:

```sh
timeout 90s xvfb-run -a -s '-screen 0 2200x1800x24' \
  dbus-run-session bash scripts/ci/desktop-render.sh \
  target/release/rufust target/ui-validation/ci
```

It launches only the unprivileged GUI, captures light/dark frames at three
scale factors and sizes, and checks for unpainted startup gutters. It does not
write devices or replace keyboard, modal, real-desktop, or physical-media tests.

## Runtime capability providers

Native packages explicitly require the display libraries below. Winit and
Glutin load them with `dlopen`, so linked-library dependency scanners cannot
discover them. The release workflow checks each generated package's metadata.
AppImage users supply these libraries through their host display stack.

| Display runtime | Debian/Ubuntu | Fedora | Arch |
|---|---|---|---|
| X11 keyboard, cursor, input, XCB bridge | `libxkbcommon-x11-0 libxcursor1 libx11-xcb1 libxi6` | `libxkbcommon-x11 libXcursor libX11-xcb libXi` | `libxkbcommon-x11 libxcursor libxi libx11 libxcb` |
| Wayland client and EGL bridge | `libwayland-client0 libwayland-egl1` | `wayland-libs` | `wayland` |
| OpenGL/EGL dispatch | `libgl1 libegl1` | `libglvnd-glx libglvnd-egl` | `libglvnd` |

The host still supplies a working display server and graphics driver. Metadata
checks do not replace installation and GUI tests on a clean desktop.

Distribution installers include the filesystem formatters below so the complete format menu works immediately. Archive decoders remain optional. Provider detection still fails closed and shows a direct remedy if a tool is removed.

| Feature | Executables/libraries | Debian/Ubuntu | Fedora | Arch |
|---|---|---|---|---|
| Core device/partition | `parted`, `blockdev`, `umount`, `swapoff`, `udevadm` | `parted util-linux udev` | `parted util-linux systemd-udev` | `parted util-linux systemd` |
| FAT/FAT32 | `mkfs.fat` | `dosfstools` | `dosfstools` | `dosfstools` |
| exFAT | `mkfs.exfat` | `exfatprogs` | `exfatprogs` | `exfatprogs` |
| ext2/3/4 | `mke2fs` | `e2fsprogs` | `e2fsprogs` | `e2fsprogs` |
| NTFS | `mkfs.ntfs` / `mkntfs` | `ntfs-3g` | `ntfsprogs` | `ntfsprogs` |
| UDF | `mkudffs` | `udftools` | `udftools` | `udftools` |
| Archive formats | libarchive, xz, bzip2, zstd | `libarchive-tools xz-utils bzip2 zstd` | `libarchive xz bzip2 zstd` | `libarchive xz bzip2 zstd` |

Package names can change; verify them against the distribution release being targeted. A missing provider disables only its feature and displays the package/executable needed.

Native packages install `qemu-nbd`, `nbdinfo`, and `nbdcopy` so VHD/VHDX
conversion is available after install: `qemu-utils libnbd-bin` on Debian/Ubuntu
or `qemu-img libnbd` on Fedora/Arch. `bzip2` remains test-only. Host tools also
enable read-only desktop capacity inspection; writing additionally needs
`nbdcopy`. AppImage users supply the host display stack; writes go through the
host's udisks2. Run the desktop
preview test as a non-root user with `cargo test -p rufust
desktop_virtual_preview --locked -- --ignored`. See
[`VIRTUAL_DISKS.md`](VIRTUAL_DISKS.md) for remaining boot and physical-media
gates.

## Packaging metadata

The `packaging/` directory contains integration metadata and starter recipes:

- `desktop/` — freedesktop desktop entry;
- `metainfo/` — AppStream metadata;
- `polkit/` — authorization policy for the narrow helper;
- `tmpfiles/` — volatile root-owned runtime directory;
- `debian/`, `rpm/`, `arch/` — distribution recipes;
- `appimage/` — AppImage staging, verification, launcher, and security notes;
- `windows/` — the Inno Setup installer script and the README it installs.

Recipes intentionally do not download proprietary boot or Windows assets. Release builders must be reproducible, use Cargo's locked dependencies, generate a software bill of materials, and preserve license texts.

## Install from a staged release build

The exact binaries depend on the final workspace layout. A conventional staged install is:

```sh
cargo build --release --locked
install -Dm0755 target/release/rufust "$DESTDIR/usr/bin/rufust"
install -Dm0755 target/release/rufust-helper "$DESTDIR/usr/libexec/rufust-helper"
install -Dm0644 packaging/desktop/io.github.nitrodiesel.rufust.desktop \
  "$DESTDIR/usr/share/applications/io.github.nitrodiesel.rufust.desktop"
install -Dm0644 packaging/metainfo/io.github.nitrodiesel.rufust.metainfo.xml \
  "$DESTDIR/usr/share/metainfo/io.github.nitrodiesel.rufust.metainfo.xml"
install -Dm0644 packaging/polkit/io.github.nitrodiesel.rufust.policy \
  "$DESTDIR/usr/share/polkit-1/actions/io.github.nitrodiesel.rufust.policy"
install -Dm0644 packaging/tmpfiles/rufust.conf \
  "$DESTDIR/usr/lib/tmpfiles.d/rufust.conf"
```

If the helper is not built, omit the helper, polkit policy, and tmpfiles rule. The desktop then writes only through udisks2 and never attempts raw access itself.

The udisks2 integration test drives the real daemon on loop devices it creates
(no root, no real disks). Run it from an active desktop session:

```sh
cargo test -p rufust-helper loop_devices -- --ignored
```

Set `RUFUST_TEST_KEEP_DIR` (and optionally `RUFUST_TEST_EFI_APP`) to keep the
resulting Windows-layout disk images for OVMF/SeaBIOS boot checks.

## AppImage

Build the AppImage GUI on the pinned Rocky Linux 8 baseline used by the release
workflow, then stage and verify it with:

```sh
packaging/appimage/stage-appdir.sh target/release/rufust Rufust.AppDir
appimagetool --runtime-file runtime-x86_64 Rufust.AppDir rufust.AppImage
packaging/appimage/verify-appimage.sh rufust.AppImage 1.2.1 2.28
```

The release workflow supplies SHA-verified appimagetool and type-2 runtime
artifacts and embeds GitHub zsync update information. Do not stage the helper,
polkit policy, setuid files, partitioning tools, formatters, glibc, or graphics
drivers in the AppDir. The native package owns the privileged integration.

## Windows

Build on Windows 10/11 with the Rust MSVC toolchain (rustup) and Visual Studio
Build Tools with the "Desktop development with C++" workload. The Windows SDK
resource compiler embeds the original Rufus icon and the `asInvoker` manifest
into `rufust.exe`; `apps/rufust/build.rs` assembles the icon from
`assets/icons/`. No other system libraries or tools are needed: formatting
uses Windows' own providers, and decoders are linked in.

```powershell
cargo build --release --locked -p rufust
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

From an elevated PowerShell, `scripts\windows\vhd-tests.ps1` runs the
end-to-end tests on throwaway VHDX disks (see [Windows](WINDOWS.md)). CI runs
the same steps on `windows-2022`.

The release workflow's Windows job publishes the bare executable as the
portable `rufust-<version>-windows-x86_64.exe`, as upstream Rufus does. It
checks the reported version, the x86-64 PE machine, and the embedded
`asInvoker` manifest before the executable joins `SHA256SUMS` and the build
provenance. Do not
ship a manifest that requests elevation: the window must stay unprivileged.

The same job compiles `packaging/windows/rufust.iss` with Inno Setup 6 into
`rufust-<version>-windows-x86_64-setup.exe`, then installs it silently, checks
the installed files, Start menu entry, and uninstall entry, and uninstalls it
again. To build the installer yourself after the release build:

```powershell
$icon = (Get-ChildItem target\release\build\rufust-*\out\rufust.ico)[0].FullName
& "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe" /DVersion=1.2.1 "/DIconFile=$icon" packaging\windows\rufust.iss
```

The installer lands in `dist\`.
