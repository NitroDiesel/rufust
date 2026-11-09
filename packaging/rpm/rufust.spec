Name:           rufust
Version:        1.1.0
Release:        1%{?dist}
Summary:        Format and create bootable USB drives
License:        GPL-3.0-or-later
URL:            https://github.com/NitroDiesel/rufust
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo rust pkgconf fontconfig-devel freetype-devel
BuildRequires:  libxkbcommon-devel wayland-devel libX11-devel libxcb-devel
BuildRequires:  systemd-rpm-macros
Requires:       udisks2 polkit util-linux parted dosfstools exfatprogs ntfsprogs e2fsprogs udftools
Requires:       qemu-img libnbd
Requires:       libxkbcommon-x11 libXcursor libX11-xcb libXi
Requires:       wayland-libs libglvnd-glx libglvnd-egl
Recommends:     libarchive xz bzip2 zstd
# Rufust was published as rufus-linux up to 1.0.1.
Obsoletes:      rufus-linux < 1.1.0
Provides:       rufus-linux = %{version}-%{release}

%description
Independent Linux port inspired by Rufus. Writes and formats removable
media through a polkit-authorized helper.

%prep
%autosetup

%build
cargo build --release --locked

%install
install -Dm0755 target/release/rufust %{buildroot}%{_bindir}/rufust
install -Dm0755 target/release/rufust-helper %{buildroot}%{_libexecdir}/rufust-helper
install -Dm0644 packaging/desktop/io.github.nitrodiesel.rufust.desktop \
  %{buildroot}%{_datadir}/applications/io.github.nitrodiesel.rufust.desktop
install -Dm0644 packaging/metainfo/io.github.nitrodiesel.rufust.metainfo.xml \
  %{buildroot}%{_metainfodir}/io.github.nitrodiesel.rufust.metainfo.xml
install -Dm0644 packaging/polkit/io.github.nitrodiesel.rufust.policy \
  %{buildroot}%{_datadir}/polkit-1/actions/io.github.nitrodiesel.rufust.policy
install -Dm0644 packaging/tmpfiles/rufust.conf \
  %{buildroot}%{_tmpfilesdir}/rufust.conf
for size in 16 24 32 48 64 128 256 512; do
  install -Dm0644 "assets/icons/rufust-${size}.png" \
    "%{buildroot}%{_datadir}/icons/hicolor/${size}x${size}/apps/io.github.nitrodiesel.rufust.png"
done

%files
%license LICENSE.txt
%license assets/icons/LICENSE.txt
%doc README.md docs/
%{_bindir}/rufust
%{_libexecdir}/rufust-helper
%{_datadir}/applications/io.github.nitrodiesel.rufust.desktop
%{_metainfodir}/io.github.nitrodiesel.rufust.metainfo.xml
%{_datadir}/polkit-1/actions/io.github.nitrodiesel.rufust.policy
%{_tmpfilesdir}/rufust.conf
%{_datadir}/icons/hicolor/*/apps/io.github.nitrodiesel.rufust.png

%changelog
* Mon Nov 09 2026 Rufust contributors - 1.1.0-1
- Rename the project from Rufus Linux to Rufust

* Sat Oct 10 2026 Rufust contributors - 1.0.1-1
- Add KiB, MiB and GiB size units; KB, MB and GB are now decimal

* Sat Oct 10 2026 Rufust contributors - 1.0.0-1
- Offer the Windows User Experience options for Windows 10 and 11 media

* Sat Oct 10 2026 Rufust contributors - 0.1.8-1
- Show progress with size, speed, and time left in selectable units

* Sun Oct 04 2026 Rufust contributors - 0.1.7-1
- Offer Super Floppy Disk for non-bootable formats only, as Rufus does

* Fri Oct 02 2026 Rufust contributors - 0.1.6-1
- Write and format through udisks2 with nothing extra installed
- Create Windows installer media with UEFI:NTFS and BIOS boot

* Thu Oct 01 2026 Rufust contributors - 0.1.5-1
- Propose the ISO volume name as the USB label and fit labels to the filesystem

* Thu Oct 01 2026 Rufust contributors - 0.1.4-1
- Reskin the desktop workbench in a fixed Rufus-style window and fix alignment

* Tue Sep 15 2026 Rufust contributors - 0.1.3-1
- Enable standalone VHD/VHDX conversion through qemu-nbd and nbdcopy

* Sat Aug 15 2026 Rufust contributors - 0.1.2-1
- Add a verified portable AppImage and harden helper readiness checks

* Sun Aug 09 2026 Rufust contributors - 0.1.1-1
- Stable UI alignment, live USB detection, and privileged I/O hardening

* Wed Jul 29 2026 Rufust contributors - 0.1.0-1
- Initial technology-preview package
