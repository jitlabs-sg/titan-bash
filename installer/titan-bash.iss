; TITAN Bash installer (Inno Setup 6+)
; Builds a per-user installer by default and can optionally add {app} to the user's PATH.

#define MyAppName "TITAN Bash"
#define MyAppExeName "titanbash.exe"
#define MyAppPublisher "TITAN Team"
#define MyAppURL "https://github.com/anthropics/titan-bash"

#ifndef MyAppVersion
  #define MyAppVersion "0.0.0"
#endif

[Setup]
AppId={{6D5E3B1F-4F5A-4D80-8C1A-4B6B4B2B9B1C}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
AppUpdatesURL={#MyAppURL}
DefaultDirName={localappdata}\Programs\{#MyAppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=titan-bash-setup-{#MyAppVersion}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ChangesEnvironment=yes
UninstallDisplayIcon={app}\{#MyAppExeName}
ArchitecturesAllowed=x64 arm64
ArchitecturesInstallIn64BitMode=x64 arm64

[Tasks]
Name: "addtopath"; Description: "Add {#MyAppName} to PATH (current user)"; GroupDescription: "Additional tasks:"; Flags: checkedonce
Name: "desktopicon"; Description: "Create a Desktop icon"; GroupDescription: "Additional tasks:"; Flags: unchecked

[Files]
Source: "..\dist\titanbash-{#MyAppVersion}-portable.exe"; DestDir: "{app}"; DestName: "{#MyAppExeName}"; Flags: ignoreversion
Source: "..\dist\tools\busybox.exe"; DestDir: "{app}\tools"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\GPL-2.0.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{userdesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{code:PathWithApp|{app}}"; Tasks: addtopath

[Code]
function NormalizePathEntry(const S: string): string;
begin
  Result := RemoveBackslashUnlessRoot(Trim(S));
end;

function PathContainsEntry(const Paths: string; const Entry: string): Boolean;
var
  I, StartPos: Integer;
  Segment: string;
  NormEntry: string;
begin
  NormEntry := Uppercase(NormalizePathEntry(Entry));
  Result := False;
  I := 1;
  while I <= Length(Paths) do
  begin
    StartPos := I;
    while (I <= Length(Paths)) and (Paths[I] <> ';') do
      I := I + 1;
    Segment := Uppercase(NormalizePathEntry(Copy(Paths, StartPos, I - StartPos)));
    if (Segment <> '') and (Segment = NormEntry) then
    begin
      Result := True;
      Exit;
    end;
    I := I + 1;
  end;
end;

function RemoveEntryFromPath(const Paths: string; const Entry: string): string;
var
  I, StartPos: Integer;
  Segment: string;
  NormEntry: string;
  OutPaths: string;
begin
  NormEntry := Uppercase(NormalizePathEntry(Entry));
  OutPaths := '';
  I := 1;
  while I <= Length(Paths) do
  begin
    StartPos := I;
    while (I <= Length(Paths)) and (Paths[I] <> ';') do
      I := I + 1;
    Segment := NormalizePathEntry(Copy(Paths, StartPos, I - StartPos));
    if (Segment <> '') and (Uppercase(Segment) <> NormEntry) then
    begin
      if OutPaths <> '' then
        OutPaths := OutPaths + ';';
      OutPaths := OutPaths + Segment;
    end;
    I := I + 1;
  end;
  Result := OutPaths;
end;

function PathWithApp(Param: string): string;
var
  Paths: string;
  AppDir: string;
begin
  AppDir := NormalizePathEntry(Param);
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths) then
    Paths := '';
  if Paths = '' then
  begin
    Result := AppDir;
    Exit;
  end;
  if PathContainsEntry(Paths, AppDir) then
  begin
    Result := Paths;
    Exit;
  end;
  Result := Paths + ';' + AppDir;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Paths: string;
  NewPaths: string;
begin
  if CurUninstallStep <> usUninstall then
    Exit;
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Paths) then
    Exit;
  NewPaths := RemoveEntryFromPath(Paths, ExpandConstant('{app}'));
  if NewPaths <> Paths then
    RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', NewPaths);
end;
