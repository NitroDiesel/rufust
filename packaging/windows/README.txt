Rufust for Windows
==================

Rufust is an independent, community-built Linux and Windows port inspired by
Rufus. It is not produced, endorsed, or supported by the upstream Rufus
project or its maintainers.

Run rufust.exe. Nothing is installed. Windows 10 or 11 (x64) is required.

The window runs with your normal rights. When you press Start and confirm,
Windows asks once for administrator approval for that operation only; the
disk work runs in a separate elevated copy of rufust.exe. Declining writes
nothing.

WARNING: writing or formatting permanently destroys the data on the selected
drive. Rufust hides system disks and most USB hard disks by default, checks
the drive again just before writing, and names it in the confirmation.

Rufust can:
  - write raw, ISOHybrid, compressed (.gz, .bz2, .xz, .lzma, .zst, .zip) and
    VHD/VHDX disk images, with read-back verification;
  - create Windows 10/11 installer drives from Windows ISOs (NTFS with the
    signed UEFI:NTFS boot partition, exFAT, or FAT32), including the Windows
    User Experience options;
  - format drives as FAT, FAT32 (any size), exFAT, NTFS, or UDF, with an
    optional bad-block test.

Settings are kept in %APPDATA%\Rufust\settings.conf.

The executable is not code-signed yet, so Windows SmartScreen may ask you to
confirm the first launch. Verify the download against SHA256SUMS from the
release page:

  Get-FileHash .\rufust-*-windows-x86_64.zip -Algorithm SHA256

Source, documentation, and issues: https://github.com/NitroDiesel/rufust
License: GNU General Public License version 3 (LICENSE.txt). Third-party
notices: THIRD_PARTY.md.
