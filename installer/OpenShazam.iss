#define ProductVersion GetEnv("OPEN_SHAZAM_VERSION")
#if ProductVersion == ""
  #define ProductVersion "0.1.0"
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
ChangesAssociations=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "japanese"; MessagesFile: "compiler:Languages\Japanese.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\dist\windows-x64\openshazam.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\windows-x64\OpenShazam.SettingsHost.dll"; DestDir: "{app}"; Flags: ignoreversion
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

[Code]
const
  ProductRunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  ProductRunValue = 'OpenShazam';
  ProductUninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{4BA8918E-564F-4D30-9705-A63A48A69977}_is1';

var
  PreviousInstallDir: String;

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

procedure DeleteHistoryFiles(const BasePath: String);
var
  Suffixes: array[0..3] of String;
  I: Integer;
begin
  Suffixes[0] := '';
  Suffixes[1] := '-wal';
  Suffixes[2] := '-shm';
  Suffixes[3] := '-journal';
  for I := 0 to 3 do
    DeleteFile(BasePath + Suffixes[I]);
end;

procedure DeletePreviousPackageHistory(const PackagesDir: String);
var
  FindRec: TFindRec;
  PackageDir: String;
begin
  if FindFirst(AddBackslash(PackagesDir) + 'OpenShazam.Windows_*', FindRec) then
  begin
    try
      repeat
        if ((FindRec.Attributes and FILE_ATTRIBUTE_DIRECTORY) <> 0) and
           (FindRec.Name <> '.') and (FindRec.Name <> '..') then
        begin
          PackageDir := AddBackslash(PackagesDir) + FindRec.Name;
          DeleteHistoryFiles(PackageDir + '\LocalState\history.db');
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
begin
  if CurUninstallStep <> usPostUninstall then
    Exit;

  if RegQueryStringValue(HKCU, ProductRunKey, ProductRunValue, ExistingCommand) and
     (CompareText(ExistingCommand, StartupCommandFor(ExpandConstant('{app}'))) = 0) then
    RegDeleteValue(HKCU, ProductRunKey, ProductRunValue);

  RegDeleteKeyIncludingSubkeys(HKCU, 'Software\OpenShazam');

  LocalData := ExpandConstant('{localappdata}\OpenShazam');
  DelTree(LocalData, True, True, True);
  DeletePreviousPackageHistory(ExpandConstant('{localappdata}\Packages'));
end;
