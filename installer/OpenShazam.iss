#define ProductVersion GetEnv("OPEN_SHAZAM_VERSION")
#if ProductVersion == ""
  #error OPEN_SHAZAM_VERSION must be set. Use tools/build-installer.ps1 to derive it from Cargo.toml.
#endif

[Setup]
AppId={{4BA8918E-564F-4D30-9705-A63A48A69977}
AppName=Shazam
AppVersion={#ProductVersion}
AppPublisher=OpenShazam
AppPublisherURL=https://github.com/NcaMoq/OpenShazam
DefaultDirName={localappdata}\Programs\OpenShazam
UsePreviousAppDir=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
DefaultGroupName=Shazam
DisableProgramGroupPage=yes
UninstallDisplayName=Shazam
UninstallDisplayIcon={app}\openshazam.exe
SetupIconFile=..\apps\openshazam\assets\OpenShazam.ico
OutputDir=..\dist\windows-x64
OutputBaseFilename=Shazam-Setup-{#ProductVersion}-x64
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
RedirectionGuard=yes
ChangesAssociations=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\dist\windows-x64\openshazam.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\windows-x64\OpenShazam.SettingsHost.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\windows-x64\THIRD-PARTY-NOTICES.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\windows-x64\Microsoft.WindowsAppRuntime.Bootstrap.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\windows-x64\OpenShazamSquare44.png"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\windows-x64\msvcp140.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\windows-x64\vcruntime140.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\windows-x64\vcruntime140_1.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\windows-x64\*.pri"; DestDir: "{app}"; Flags: ignoreversion skipifsourcedoesntexist
Source: "..\target\windows-app-sdk-runtime\WindowsAppRuntimeInstall-x64.exe"; Flags: dontcopy

[Icons]
Name: "{group}\Shazam"; Filename: "{app}\openshazam.exe"; Parameters: "ui"; WorkingDir: "{app}"
Name: "{autodesktop}\Shazam"; Filename: "{app}\openshazam.exe"; Parameters: "ui"; WorkingDir: "{app}"; Tasks: desktopicon

[Run]
Filename: "{app}\openshazam.exe"; Parameters: "ui"; WorkingDir: "{app}"; Description: "{cm:LaunchProgram,Shazam}"; Flags: postinstall nowait skipifsilent

[CustomMessages]
english.RuntimeInstallFailed=Windows App SDK Runtime installation failed with exit code %1. Setup cannot continue because Shazam Settings needs this runtime.
japanese.RuntimeInstallFailed=Windows App SDK Runtime のインストールに失敗しました (終了コード %1)。Shazam の設定画面に必要なため、セットアップを続行できません。
english.UninstallCleanupFailed=Some OpenShazam settings or history could not be removed. Close Shazam and manually remove the OpenShazam local data folder and the user registry key under HKEY_CURRENT_USER\Software.
japanese.UninstallCleanupFailed=OpenShazam の設定または履歴の一部を削除できませんでした。Shazam を終了し、OpenShazam のローカルデータフォルダーと HKEY_CURRENT_USER\Software 内のユーザー設定キーを手動で削除してください。

[Code]
const
  ProductRunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  ProductRunValue = 'OpenShazam';
  ProductUninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{4BA8918E-564F-4D30-9705-A63A48A69977}_is1';
  AttrDirectory = $00000010;
  AttrReparsePoint = $00000400;
  InvalidFileAttributes = $FFFFFFFF;

var
  PreviousInstallDir: String;

function GetFileAttributesW(Path: String): DWORD;
external 'GetFileAttributesW@kernel32.dll stdcall uninstallonly';

function IsSafeHistoryDirectory(const Path: String): Boolean;
var
  Attributes: DWORD;
begin
  Attributes := GetFileAttributesW(Path);
  Result := (Attributes <> InvalidFileAttributes) and
    ((Attributes and AttrDirectory) <> 0) and
    ((Attributes and AttrReparsePoint) = 0);
end;

function IsSafeHistoryFile(const Path: String): Boolean;
var
  Attributes: DWORD;
begin
  Attributes := GetFileAttributesW(Path);
  Result := (Attributes <> InvalidFileAttributes) and
    ((Attributes and AttrDirectory) = 0) and
    ((Attributes and AttrReparsePoint) = 0);
end;

function StartupCommandFor(const InstallDir: String): String;
begin
  Result := '"' + AddBackslash(RemoveBackslashUnlessRoot(InstallDir)) + 'openshazam.exe" ui --startup';
end;

function InitializeSetup(): Boolean;
begin
  PreviousInstallDir := '';
  RegQueryStringValue(HKCU, ProductUninstallKey, 'InstallLocation', PreviousInstallDir);
  PreviousInstallDir := RemoveBackslashUnlessRoot(PreviousInstallDir);
  Result := True;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
begin
  Result := '';
  ExtractTemporaryFile('WindowsAppRuntimeInstall-x64.exe');
  if not Exec(ExpandConstant('{tmp}\WindowsAppRuntimeInstall-x64.exe'), '--quiet', '',
    SW_HIDE, ewWaitUntilTerminated, ResultCode) then
  begin
    Result := FmtMessage(ExpandConstant('{cm:RuntimeInstallFailed}'), ['00000001']);
    Exit;
  end;
  if ResultCode = 3010 then
    NeedsRestart := True
  else if ResultCode <> 0 then
    Result := FmtMessage(ExpandConstant('{cm:RuntimeInstallFailed}'), [IntToStr(ResultCode)]);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ExistingCommand: String;
  NewCommand: String;
begin
  if CurStep <> ssPostInstall then
    Exit;

  if (PreviousInstallDir = '') or
     (CompareText(PreviousInstallDir, RemoveBackslashUnlessRoot(ExpandConstant('{app}'))) = 0) then
    Exit;

  if not RegQueryStringValue(HKCU, ProductRunKey, ProductRunValue, ExistingCommand) then
    Exit;
  if CompareText(ExistingCommand, StartupCommandFor(PreviousInstallDir)) <> 0 then
    Exit;

  NewCommand := StartupCommandFor(ExpandConstant('{app}'));
  RegWriteStringValue(HKCU, ProductRunKey, ProductRunValue, NewCommand);
end;

function DeleteHistoryFiles(const BasePath: String): Boolean;
var
  Suffixes: array[0..3] of String;
  Attributes: DWORD;
  FilePath: String;
  I: Integer;
begin
  Result := True;
  Suffixes[0] := '';
  Suffixes[1] := '-wal';
  Suffixes[2] := '-shm';
  Suffixes[3] := '-journal';
  for I := 0 to 3 do
  begin
    FilePath := BasePath + Suffixes[I];
    Attributes := GetFileAttributesW(FilePath);
    if Attributes <> InvalidFileAttributes then
      if not IsSafeHistoryFile(FilePath) or
         not DeleteFile(FilePath) then
        Result := False;
  end;
end;

function DeletePreviousPackageHistory(const PackagesDir: String): Boolean;
var
  FindRec: TFindRec;
  PackageDir: String;
  LocalStateDir: String;
  ParentAttributes: DWORD;
  PackagesAttributes: DWORD;
  PackageAttributes: DWORD;
  LocalStateAttributes: DWORD;
begin
  Result := True;
  ParentAttributes := GetFileAttributesW(ExtractFileDir(RemoveBackslashUnlessRoot(PackagesDir)));
  PackagesAttributes := GetFileAttributesW(PackagesDir);
  if (ParentAttributes = InvalidFileAttributes) or
     (PackagesAttributes = InvalidFileAttributes) then
    Exit;
  if not IsSafeHistoryDirectory(ExtractFileDir(RemoveBackslashUnlessRoot(PackagesDir))) or
     not IsSafeHistoryDirectory(PackagesDir) then
  begin
    Result := False;
    Exit;
  end;

  if FindFirst(AddBackslash(PackagesDir) + 'OpenShazam.Windows_*', FindRec) then
  begin
    try
      repeat
        if ((FindRec.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0) and
           (FindRec.Name <> '.') and (FindRec.Name <> '..') then
        begin
          PackageDir := AddBackslash(PackagesDir) + FindRec.Name;
          LocalStateDir := PackageDir + '\LocalState';
          PackageAttributes := GetFileAttributesW(PackageDir);
          if PackageAttributes <> InvalidFileAttributes then
            if not IsSafeHistoryDirectory(PackageDir) then
              Result := False;
          if (PackageAttributes <> InvalidFileAttributes) and
             IsSafeHistoryDirectory(PackageDir) then
          begin
            LocalStateAttributes := GetFileAttributesW(LocalStateDir);
            if LocalStateAttributes <> InvalidFileAttributes then
              if not IsSafeHistoryDirectory(LocalStateDir) then
                Result := False;
            if (LocalStateAttributes <> InvalidFileAttributes) and
               IsSafeHistoryDirectory(LocalStateDir) then
              if not DeleteHistoryFiles(LocalStateDir + '\history.db') then
                Result := False;
          end;
        end;
      until not FindNext(FindRec);
    finally
      FindClose(FindRec);
    end;
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  ExistingCommand: String;
  LocalData: String;
  CleanupFailed: Boolean;
  LocalDataAttributes: DWORD;
begin
  if CurUninstallStep <> usPostUninstall then
    Exit;

  CleanupFailed := False;
  if RegQueryStringValue(HKCU, ProductRunKey, ProductRunValue, ExistingCommand) and
     (CompareText(ExistingCommand, StartupCommandFor(ExpandConstant('{app}'))) = 0) and
     not RegDeleteValue(HKCU, ProductRunKey, ProductRunValue) then
    CleanupFailed := True;

  if RegKeyExists(HKCU, 'Software\OpenShazam') and
     not RegDeleteKeyIncludingSubkeys(HKCU, 'Software\OpenShazam') then
    CleanupFailed := True;

  LocalData := ExpandConstant('{localappdata}\OpenShazam');
  LocalDataAttributes := GetFileAttributesW(LocalData);
  if LocalDataAttributes <> InvalidFileAttributes then
    if not IsSafeHistoryDirectory(LocalData) or
       not DelTree(LocalData, True, True, True) then
      CleanupFailed := True;
  if not DeletePreviousPackageHistory(ExpandConstant('{localappdata}\Packages')) then
    CleanupFailed := True;

  if CleanupFailed then
    SuppressibleMsgBox(CustomMessage('UninstallCleanupFailed'), mbError, MB_OK, IDOK);
end;
