# Project context for continuing agents

This is the durable handoff for agents continuing Rufust. Read it before
planning changes, then follow the linked specialist documents for the branch
of work you are touching. Update this file when a release changes the baseline,
a safety invariant changes, or a capability moves between available, blocked,
and planned.

## Product identity

Rufust is an independent Linux and Windows port inspired by
[Rufus](https://github.com/pbatard/rufus). It is not produced, endorsed, or
supported by the upstream Rufus project. Preserve that disclaimer anywhere the
name and original public-domain Rufus icon could imply official affiliation.

The product target is a trustworthy USB device workbench for Arch Linux,
Debian/Ubuntu, Fedora, and Windows 10/11 (x64). Feature parity means
implementing an upstream concept safely on each platform; it does not mean exposing a control before its full operation
is implemented and verified.

## Current baseline: 1.2.1

Version 1.2.1 is the continuation baseline. Its public installers belong in the
[v1.2.1 release](https://github.com/NitroDiesel/rufust/releases/tag/v1.2.1).
1.2.0 is the first release with Windows support (below); 1.2.1 limits the
device warning to unusual targets and names the reason. The project was
named Rufus Linux (`rufus-linux`) up to 1.0.1; 1.1.0 renamed it to Rufust
everywhere. Its packages replace `rufus-linux`, and the desktop app
still reads preferences from `~/.config/rufus-linux` until it saves its own.
Release titles are `v<version>`. [`PARITY.md`](PARITY.md) tracks every
upstream Rufus feature and the order in which the rest will land.

Completed product work includes:

- plug-and-play writing: all destructive work runs through the system udisks2
  daemon as the desktop user, so the AppImage needs nothing installed; the
  native helper remains the fallback where udisks2 is not running;
- Windows installer media from Windows ISOs (detected by contents) in the
  upstream layout: NTFS/exFAT plus the signed UEFI:NTFS partition, or FAT32,
  with optional Windows 7 MBR and NTFS/FAT32 boot records for BIOS, file-level
  read-back verification, and an in-process UDF/ISO 9660 reader;
- upstream's Windows User Experience dialog on Start for Windows 10/11 ISOs
  (1.0.0): options chosen per build from the WIM index, written as a generated
  `autounattend.xml` (or `$OEM$` Panther file) with upstream's
  `appraiserres.dll` and signed Setup wrapper bypasses. The request carries
  only typed, validated choices; the helper library renders the XML;

- a native Slint desktop application with aligned compact controls, light and
  dark themes, keyboard-operable custom buttons, true blocking modals, and a
  continuous Windows-style progress bar;
- automatic block-topology watching and USB hot-plug refresh, with target
  selection preserved by strict device identity and refresh deferred during a
  confirmation or active operation;
- safe formatting for MBR, GPT, and super-floppy layouts using installed FAT,
  FAT32, exFAT, NTFS, UDF, and ext2/3/4 providers;
- bounded raw, compressed-raw, and ISOHybrid writing, optional readback
  verification, checksums, bad-block testing, cancellation, flush, and clear
  terminal status reporting;
- a short-lived polkit-authorized helper with target revalidation, source file
  descriptor binding, symlink rejection, fixed-path tool allowlists, privilege
  dropping for decoders, managed child process groups, cooperative
  cancellation, and destination synchronization;
- native Debian, Fedora, and Arch packages containing the GUI, root-owned
  helper, exact-path polkit policy, desktop metadata, and required providers;
- a portable x86_64 AppImage built on a pinned glibc 2.28 baseline, with zsync
  metadata, extraction fallback, AppDir denylist checks, cross-distribution
  loader tests, and GitHub build provenance;
- the original upstream Rufus PNG icon set at 16 through 512 pixels. The icon
  files are public domain, courtesy of PC Unleashed; keep the attribution in
  `assets/icons/LICENSE.txt` and `THIRD_PARTY.md`;
- standalone 512-byte-sector VHD/VHDX conversion through protected
  `qemu-nbd`/`nbdcopy` streaming when those tools are installed.

The authoritative feature truth is
[`CAPABILITIES.md`](CAPABILITIES.md). Recognized but blocked flows include
non-hybrid Linux ISO file-copy media, FAT32 Windows media with a WIM over
4 GB, Windows To Go, parent-dependent/4Kn/journal-replay VHD/VHDX, FreeDOS, persistence,
bootloader installation, Windows CA 2023 bootloaders, Secure Boot revocation
checks, and drive capture. Keep these disabled with a reason until an
end-to-end implementation and its fixtures, integration tests, and boot tests
exist. Supported standalone 512-byte-sector VHD/VHDX images convert when
`qemu-nbd`, `nbdinfo`, and `nbdcopy` are installed.

## Windows support (since 1.2.0)

Rufust also runs on Windows 10 and 11 (x64). The UI, planner, helper
protocol, image analysis, Windows-media layout, answer files, and boot records
are shared; device discovery, privilege, and the disk engine are Windows
modules. Read [`WINDOWS.md`](WINDOWS.md) before changing them. In short:

- the window stays unprivileged; each operation runs in an elevated copy of
  `rufust.exe` (`--elevated-worker <pipe>`), approved through UAC, that serves
  one request over two private named pipes and accepts only its own process;
- discovery uses zero-access storage IOCTLs; the disk holding Windows, the
  system partition, page files, spanned volumes, and virtual disks are
  protected or hidden;
- the engine locks and dismounts the target's volumes, partitions with the
  disk IOCTLs, formats with Windows' `FormatEx` plus a built-in large-FAT32
  formatter, decodes compressed images in process, and attaches VHD/VHDX
  read-only through `virtdisk.dll`;
- ext2/3/4 and ReFS creation are not offered on Windows; everything else in
  the Linux capability set is available there.

Verification evidence: `cargo test` and `cargo clippy -D warnings` pass on
Windows; the elevated `scripts/windows/vhd-tests.ps1` passes on throwaway VHDX
disks (every provider and scheme, raw/gzip/VHDX writes with read-back, Windows
media for MBR BIOS+UEFI with UEFI:NTFS, GPT UEFI, and FAT32, large FAT32, and
the window's pipe launcher including cancellation); discovery and the VHD
capacity preview pass under a non-administrator token; and the release build
renders the workbench in the fixed 560x720 window. CI runs these on
`windows-2022`, and the release workflow adds
`rufust-<version>-windows-x86_64-setup.exe` (Inno Setup, verified by a silent
install and uninstall) and the portable `rufust-<version>-windows-x86_64.exe`.
The v1.2.0 release is the first to include them.

## Architecture map

The user-facing flow is:

```text
Slint UI (unprivileged user)
  -> Linux device/image inspection
  -> immutable operation plan and target-specific confirmation
  -> one versioned request, executed either
       in-process through udisks2 (default; udisks2 does the root work), or
       by the root-owned one-operation helper through pkexec (fallback)
  -> structured progress, verification, flush, and exit
```

Repository ownership is divided as follows:

- `apps/rufust/` owns the Slint UI, user-session state, hot-plug watcher,
  capability presentation, and helper client.
- `crates/rufust-core/` owns domain types, device identity, eligibility,
  operation planning, safety decisions, and progress types.
- `crates/rufust-platform/` owns Linux sysfs, mount, swap, holder, and
  external-provider discovery.
- `crates/rufust-image/` owns bounded image recognition and analysis.
- `crates/rufust-helper-protocol/` owns the versioned and size-bounded NDJSON
  request/event contract.
- `crates/rufust-helper/` owns all destructive disk operations: the udisks2
  engine (`udisks.rs`, `udisks_engine.rs`, `windows_media.rs` with the bundled
  UEFI:NTFS and ms-sys boot-record assets) and the narrow privileged
  executor.
- `crates/rufust-boot/`, `rufust-downloads/`, and `rufust-i18n/` contain planned or
  partial supporting domains; their presence does not make a product flow
  available.
- `packaging/` owns distro integration, AppImage staging/auditing, icons,
  desktop metadata, and the polkit policy.
- `.github/workflows/ci.yml` is the normal quality gate;
  `.github/workflows/release.yml` builds and publishes all four installer
  formats.

On Windows the flow is the same with a different executor:

```text
Slint UI (unprivileged user)
  -> Windows disk discovery (zero-access IOCTLs)
  -> the same plan and confirmation
  -> one versioned request over private named pipes to
       rufust.exe --elevated-worker, started through UAC per operation
  -> structured progress, verification, flush, and exit
```

Windows ownership: `crates/rufust-platform/src/windows.rs` (discovery, volume
table, capabilities); `crates/rufust-helper/src/windows/` (engine, pipe
server, `FormatEx`, decoders, VHD attach, and the VHDX integration tests);
`crates/rufust-helper/src/fat32.rs` (large FAT32); `engine.rs` (progress and
Windows-media planning shared with udisks2); and in the app,
`helper_client/windows.rs` (UAC launch and pipes), `desktop.rs` (file chooser
and theme), plus Windows branches in `hotplug.rs`, `regional.rs`,
`settings.rs`, and `image_inspection.rs`.

Read [`ARCHITECTURE.md`](ARCHITECTURE.md) before moving responsibilities between
modules or changing the operation state model.

## Safety invariants

Read [`SAFETY.md`](SAFETY.md) before changing discovery, target selection,
authorization, helper protocol, destructive operations, cancellation, external
tools, downloads, or logs. The following are release boundaries, not optional
implementation details:

- The desktop stays unprivileged. Destructive work runs only through the
  system udisks2 daemon (raw descriptors from `OpenDevice`, never by opening
  device nodes) or, as a fallback, `/usr/bin/pkexec` and the root-owned
  `/usr/libexec/rufust-helper`.
- The helper accepts a typed allowlisted operation, never a shell command or an
  arbitrary root-owned output path.
- Device paths are display names, not identity. Revalidate the stable path,
  major/minor, size, model, serial, topology, mounts, swap, and holders after
  authorization and immediately before writing.
- Bind and validate the source once, reject symlinks and source-on-target
  layouts, bound all decompression and writes, lock exclusively, and report
  success only after verification and flush complete.
- Cancellation terminates managed process groups cooperatively, reaps direct
  children, and synchronizes the destination before returning the incomplete
  media warning.
- Keep external tools on reviewed absolute paths with cleared environments.
  Keep permissive udev rules, setuid launchers, direct `sudo` fallbacks, and a
  root GUI outside the design.
- On Windows, the window never runs as administrator and never opens a disk.
  Only the UAC-elevated worker writes, after the same revalidation, and only
  to a `\\.\PhysicalDriveN` whose interface path, number, size, sector size,
  serial, and model still match. Every target volume stays locked and
  dismounted during raw writes; the window's manifest stays `asInvoker`.

### AppImage trust boundary

The AppImage is a portable **unprivileged frontend**. It must not contain the
helper, polkit policy, formatters, partitioning tools, setuid files, glibc, or
graphics drivers. Everything, including writing, works portably because the
host's udisks2 performs the privileged steps under its own policy.

Without udisks2, the GUI falls back to the native helper and checks helper and policy ownership, write permissions, exact policy
action/path, effective polkit registration, and helper version immediately
before launch. Preserve the fixed privileged identity; running bytes from an
AppImage mount or user-owned extraction directory through `pkexec` would break
the reviewed trust boundary.

Read [`packaging/appimage/README.md`](../packaging/appimage/README.md) before
changing AppImage contents or launch behavior.

## UI and product behavior

The visual direction is a compact “Device Workbench,” not a clone of the
Windows window chrome. Since 0.1.4 its skin follows T3 Code: a neutral
`#0a0a0a`/`#fcfcfc` canvas, hairline borders, flat 12-pixel-radius cards, the
desktop default sans font, and one indigo-blue primary action. Its window and
layout follow upstream Rufus (`IDD_DIALOG` uses `DS_MODALFRAME |
WS_MINIMIZEBOX` without `WS_THICKFRAME`): a fixed 560x720 logical-pixel dialog
that can be minimized but not resized or maximized, with Drive properties,
Format options, and Status sections. Content that does not fit, such as Expert
options, scrolls inside the window. Tokens live in
`apps/rufust/ui/theme.slint`. The std-widgets palette cannot be restyled
in Slint 1.9, so selects, text fields, checkboxes, and the workbench scrollbar
are custom components in `components.slint`. Selects step through choices with
Up/Down/Home/End and open their list with Space, Enter, or Alt+Down. Preserve
these resolved decisions:

- no redundant in-content “Rufust / Device Workbench” title block;
- a footer bar with evenly spaced About, Advanced, Checksums, Log, and theme
  tools on the left and Close/Cancel plus the primary action on the right, as
  in upstream Rufus; image Select sits beside the boot selection, and refresh
  sits beside the device selector at the same field height;
- section titles, device details, and image controls are left-aligned. Avoid
  `alignment: center` on a horizontal layout that relies on a stretching
  spacer; Slint then gives the spacer zero width and centers everything;
- normalized 36-pixel action buttons and 13-pixel action labels, with visible
  focus rings and Space/Enter activation;
- Log, About, and destructive confirmation overlays block all interaction with
  the workbench, support Escape, and give initial focus to the safe action;
- essential status text and badges share a centerline, and progress uses one
  continuous trough/fill rather than segmented blocks;
- the original Rufus icon is used for the window, desktop integration, native
  packages, and AppImage at its exact source sizes.

For UI work, use the `frontend-design` skill and a bounded UI review subagent
when the agent environment provides them. Verify both themes in the fixed
560x720 window at scale factors 1, 1.25, and 2, including keyboard traversal,
select lists near the bottom edge, and modal click blocking. Keep the UI truthful: unavailable choices stay disabled or absent
with a concise reason.

## Known continuation points

Fixed VHD footer signatures are recognized even when the file has been renamed,
including the older 511-byte footer layout. The desktop request builder rechecks
operation availability before constructing a helper request. Regression tests
cover renamed VHDs, unchanged raw-image recognition, and refusal to construct a
raw write of container bytes.

The `codex/vhd-streaming-backend` continuation adds a tested private conversion
backend but keeps VHD/VHDX blocked at both production entry points. Read
[`VIRTUAL_DISKS.md`](VIRTUAL_DISKS.md) before changing virtual disk handling;
it records provider limits, the approved source-snapshot fallback and decoder
inactivity deadline, shared write-error cleanup, and the remaining
fault-injection, UI, packaging, and boot gates. A pinned QEMU journal fixture
now verifies read-only refusal and source preservation, with a separate repaired
control. Generated fixed/dynamic 4Kn metadata variants and writer-generated
VHD/VHDX parent chains also verify rejection without source changes. The optional
parent-chain generator is documented under `scripts/fixtures/parent-chains/`;
its .NET dependencies are not needed by Rufust or normal CI. Production VHD/VHDX
entry points remain blocked.

Cross-distribution tests found that QEMU versions disagree on DiscUtils VHD
capacity. The backend now refuses any VHD export that differs from the validated
footer, including ambiguous legacy CHS images. See `VIRTUAL_DISKS.md` before
changing that policy; accepting the smaller size could omit image data.

The snapshot copy routine also has an isolated real-ENOSPC regression using a
bounded private tmpfs. A second script verifies initial low-space refusal and
Btrfs reflink snapshots. Read `VIRTUAL_DISKS.md` for their invocations and limits;
running all ignored tests directly does not provide the required mount setup.

The continuation branch runs image inspection off the UI thread and
uses the helper library's non-root, read-only inspection API for optional
VHD/VHDX capacity previews. Configuration and Start stay disabled during the
probe; generation checks discard stale results. Closing requests cancellation
and waits for worker cleanup. Read `VIRTUAL_DISKS.md` for limits and current
verification evidence.

That integration also fixes workbench sizing and adds explicit modal keyboard
focus trapping. Light/dark and Log/About checks passed locally. Explicit native
startup sizing fixes the reproduced X11 paint offset, with a CI screenshot
regression at three scale factors. Destructive confirmation now includes source
file and virtual-disk sizes after the target identity, omits source details
when formatting, and can be exercised with the non-destructive
`confirmation-preview` example. Normal X11/Wayland desktop and physical
confirmation smoke tests remain open in `VIRTUAL_DISKS.md`.

The startup CI test exposed a missing `libxkbcommon-x11` runtime dependency.
The continuation branch declares the dynamically loaded X11, Wayland, and
OpenGL/EGL libraries in all three native recipes and checks the built package
metadata. AppImage users still supply the host display stack. Read `BUILDING.md`
for distribution package names; this correction is
included in 0.1.3.

These are known follow-ups, not claims that the current release is broken:

- Windows: the engine tests passed on a physical USB flash drive
  (`scripts/windows/usb-tests.ps1`). Still open: a USB SSD and a drive over
  32 GB, unplugging during a write, boot-testing Windows-built media, and
  Authenticode-signing the executable and installer. See `WINDOWS.md`.
- Run a packaged, real-polkit smoke test on disposable physical USB media for
  formatting, raw writing, Windows media, verification, cancellation, and
  disconnect during write. Automated tests, loop-backed udisks2 tests, and
  QEMU boots do not replace this gate.
- Windows media follow-ups, in `PARITY.md` order: the Windows CA 2023
  bootloader option (needs a WIM/LZX extractor for `boot.wim`), split WIM for
  FAT32, then non-hybrid Linux ISO file-copy with Syslinux/GRUB. Rufust
  does not rewrite `boot.wim`, so the hardware bypass always uses upstream's
  `RunSynchronous` fallback from the root `autounattend.xml`.
- Desktop theme startup currently uses `RUFUST_THEME` or `GTK_THEME`.
  On Windows it uses `RUFUST_THEME` or the app-mode setting.
  Portal-backed system theme detection and persistence of a manual
  system/light/dark preference remain unimplemented.
- `rufust-i18n` contains catalog groundwork but the Slint application still has
  hard-coded English strings. Wire one catalog source before claiming
  localization.
- FreeDOS and Windows To Go remain visible roadmap choices that resolve to a
  blocking explanation. If this interaction changes, decide deliberately
  between visible-disabled roadmap items and hiding unavailable modes, then
  test the chosen behavior.
- Filesystem choices are provider-driven. NTFS appears only when a supported
  `mkfs.ntfs`/`mkntfs` provider is installed; native packages install the
  provider, while the AppImage relies on the host's tools through udisks2.
- The AppImage compatibility claim is x86_64 glibc 2.28+ desktop Linux with a
  FUSE extraction fallback. Alpine/musl, NixOS/non-FHS layouts, headless hosts,
  and systems without udisks2 or polkit are not covered by a universal “any distro” claim.

## Repository administration follow-ups

Repository administration still has two owner requests awaiting resolution:

- Remove the old `chatgpt-codex-connector[bot]` contributor credit. Two published
  merge commits contain its co-author trailer, `a79ab63` and `a377ceb`.
  Removing a commit-derived credit may require rewriting published history;
  no such rewrite has been performed. Agree on the exact migration and release
  provenance impact with the owner before changing those commits.
- Add repository tags/topics from two screenshots whose files are unavailable.
  The exact names have been requested but not supplied. Preserve this as an
  input gap rather than guessing tags.

## Build, test, and release gates

Read [`BUILDING.md`](BUILDING.md) before changing dependencies, distro recipes,
runtime providers, install paths, or release artifacts.

Run the normal source gates from the repository root:

```sh
cargo fmt --all -- --check
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --release --locked
git diff --check
```

On Windows, run the same `cargo fmt`, `cargo test`, `cargo clippy`, and
`cargo build --release` gates, then the elevated end-to-end tests:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/windows/vhd-tests.ps1
```

Changes to desktop metadata or authorization also require:

```sh
desktop-file-validate packaging/desktop/io.github.nitrodiesel.rufust.desktop
appstreamcli validate --no-net packaging/metainfo/io.github.nitrodiesel.rufust.metainfo.xml
xmllint --noout packaging/polkit/io.github.nitrodiesel.rufust.policy
```

The release workflow must produce one Debian package, RPM, Arch package,
AppImage, AppImage zsync file, Windows installer, Windows portable executable,
and `SHA256SUMS`. The AppImage verifier must prove
its architecture, glibc 2.28 ceiling, dependency closure, lack of RPATH, update
information, metadata, executable permissions, unprivileged contents, direct
version launch, extraction fallback, Xvfb GUI startup, and loader compatibility
on Debian, Fedora, and Arch.

Every copy of the version is checked against `Cargo.toml` by
`scripts/version.sh check` in CI. `scripts/version.sh bump X.Y.Z` rewrites the
plain version strings; the changelog entries (Debian, RPM, AppStream, the
"Changes in" section of `CAPABILITIES.md`, this baseline, and the release
notes) are written by hand, and the check names any that are missing.

Release publication supports a `v*` tag or a matching `release/v<PACKAGE_VERSION>`
branch. The release branch path exists so an authenticated repository agent can
publish through the reviewed workflow without locally stored GitHub credentials.
The publish job creates the missing tag from the exact release commit, uploads
all installers, marks the release as full/latest, and attaches provenance.

Before bumping a release, find every version-bearing file with `rg` rather than
updating only Cargo metadata. At minimum, reconcile the workspace version,
lockfile, UI version, distro recipes/changelogs, AppStream release, build docs,
and `PACKAGE_VERSION`. After publication, download the public `SHA256SUMS` and
AppImage and verify them independently.

## Continuation workflow

1. Read this file and the specialist document for the requested branch of work.
2. Inspect `git status`, recent commits, open pull requests, and the public
   release before assuming the handoff state is unchanged.
3. Trace the current implementation and tests before editing. Treat the
   capability matrix as a claim to prove, not a backlog to infer from.
4. Keep each change inside the existing privilege and ownership boundaries.
5. Update tests and the relevant documentation in the same change. If feature
   availability changes, update `CAPABILITIES.md` and this baseline.
6. Run every applicable local gate, then require checks for the exact pull
   request head before merging.
7. For a release, verify the public assets and full-release status after the
   workflow completes; a successful build artifact alone is not publication.

Commit material Codex or Grok changes with the co-author trailer required by
the root `AGENTS.md` so GitHub records that contribution.
