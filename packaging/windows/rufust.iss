; Inno Setup script for the Rufust Windows installer.
;
; Build from the repository root after `cargo build -p rufust --release`:
;   iscc /DVersion=1.1.0 /DIconFile=<path to rufust.ico> packaging\windows\rufust.iss
;
; The installer writes to Program Files, so only administrators can replace
; the executable that the elevated disk worker runs. The app itself keeps its
; asInvoker manifest and is always started unelevated, also after setup.

#ifndef Version
  #error Pass the version with /DVersion=x.y.z
#endif
#define Root "..\.."

[Setup]
AppId={{6C1F3B0E-7D2A-4E59-9B8C-2F4A1D7E5C31}
AppName=Rufust
AppVersion={#Version}
AppVerName=Rufust {#Version}
AppPublisher=Rufust contributors
AppPublisherURL=https://github.com/NitroDiesel/rufust
AppSupportURL=https://github.com/NitroDiesel/rufust/issues
AppUpdatesURL=https://github.com/NitroDiesel/rufust/releases
VersionInfoVersion={#Version}
DefaultDirName={autopf}\Rufust
DefaultGroupName=Rufust
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile={#Root}\LICENSE.txt
OutputDir={#Root}\dist
OutputBaseFilename=rufust-{#Version}-windows-x86_64-setup
#ifdef IconFile
SetupIconFile={#IconFile}
#endif
UninstallDisplayIcon={app}\rufust.exe
UninstallDisplayName=Rufust
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes

[Tasks]
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Root}\target\release\rufust.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\packaging\windows\README.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#Root}\THIRD_PARTY.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Rufust"; Filename: "{app}\rufust.exe"
Name: "{autodesktop}\Rufust"; Filename: "{app}\rufust.exe"; Tasks: desktopicon

[Run]
; runasoriginaluser: setup is elevated, but the window must never be.
Filename: "{app}\rufust.exe"; Description: "{cm:LaunchProgram,Rufust}"; Flags: nowait postinstall skipifsilent runasoriginaluser
