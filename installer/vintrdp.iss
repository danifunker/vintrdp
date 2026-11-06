; Inno Setup script for vintrdp (Windows). Defaults to a per-user install, with
; "install for all users" available. scripts/dist/package-windows.sh passes:
;   /DMyAppVersion=1.2.3 /DTargetArch=x86|x64|arm64 /DSourceDir=... /DLicenseFile=...

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0-dev"
#endif
#ifndef SourceDir
  #define SourceDir "..\dist"
#endif
#ifndef LicenseFile
  #define LicenseFile "..\LICENSE"
#endif
#ifndef TargetArch
  #define TargetArch "x64"
#endif
#if TargetArch == "arm64"
  #define ArchIdentifier "arm64"
#elif TargetArch == "x86"
  #define ArchIdentifier "x86compatible"
#else
  #define ArchIdentifier "x64compatible"
#endif

#define MyAppName "vintrdp"
#define MyAppPublisher "Dani Sarfati"
#define MyAppURL "https://github.com/danifunker/vintrdp"
#define MyAppExeName "vintrdp.exe"

[Setup]
; Stable: changing the AppId orphans existing installs instead of upgrading them.
AppId={{5D7A2C1E-8B34-4E6F-9A10-3C7E5F21B8D4}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}/releases
LicenseFile={#LicenseFile}
MinVersion=10.0
ArchitecturesAllowed={#ArchIdentifier}
#if TargetArch != "x86"
ArchitecturesInstallIn64BitMode={#ArchIdentifier}
#endif
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\{#MyAppName}
DisableProgramGroupPage=yes
DefaultGroupName={#MyAppName}
AllowNoIcons=yes
UninstallDisplayIcon={app}\{#MyAppExeName}
OutputBaseFilename=vintrdp-Setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#LicenseFile}"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\Uninstall {#MyAppName}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Launch {#MyAppName}"; Flags: nowait postinstall skipifsilent
