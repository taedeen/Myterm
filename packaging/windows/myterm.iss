; MyTerm installer (Inno Setup 6). Built by CI:
;   ISCC.exe /DAppVersion=0.2.0 /DSourceDir=<Release folder> [/DSIGN /Smytermsign=...] myterm.iss
; Per-user install (no admin prompt) to %LOCALAPPDATA%\Programs\MyTerm.
; Installing a newer version over an older one upgrades it in place; your
; vault and settings live in %APPDATA% and are never touched.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #error Pass /DSourceDir=<flutter Release folder>
#endif

[Setup]
AppId={{6B0E9C3E-2F4B-4E8A-9C61-7A1D3E5F8B20}
AppName=MyTerm
AppVersion={#AppVersion}
AppVerName=MyTerm {#AppVersion}
AppPublisher=MyTerm
VersionInfoVersion={#AppVersion}
VersionInfoDescription=MyTerm SSH client installer
DefaultDirName={localappdata}\Programs\MyTerm
DefaultGroupName=MyTerm
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=output
OutputBaseFilename=MyTerm-{#AppVersion}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\MyTerm.exe
UninstallDisplayName=MyTerm
CloseApplications=yes
CloseApplicationsFilter=*.exe,*.dll
RestartApplications=no
#ifdef SIGN
SignTool=mytermsign
SignedUninstaller=yes
#endif

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[InstallDelete]
; Flutter's bundled assets are replaced wholesale on upgrade.
Type: filesandordirs; Name: "{app}\data"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\MyTerm"; Filename: "{app}\MyTerm.exe"
Name: "{autodesktop}\MyTerm"; Filename: "{app}\MyTerm.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\MyTerm.exe"; Description: "Start MyTerm"; Flags: nowait postinstall skipifsilent
