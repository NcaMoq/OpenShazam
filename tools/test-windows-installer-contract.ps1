$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$installer = Get-Content -LiteralPath (Join-Path $root 'installer/OpenShazam.iss') -Raw
$startupHost = Get-Content -LiteralPath (Join-Path $root 'apps/openshazam/settings-host/SettingsHost.cpp') -Raw
$historyStore = Get-Content -LiteralPath (Join-Path $root 'apps/openshazam/src/windows_history_store.rs') -Raw
$ci = Get-Content -LiteralPath (Join-Path $root '.github/workflows/ci.yml') -Raw
$release = Get-Content -LiteralPath (Join-Path $root '.github/workflows/release-windows.yml') -Raw
$signing = Get-Content -LiteralPath (Join-Path $root 'tools/sign-windows-artifacts.ps1') -Raw
$update = Get-Content -LiteralPath (Join-Path $root 'crates/openshazam-update/src/lib.rs') -Raw
$updateUi = Get-Content -LiteralPath (Join-Path $root 'apps/openshazam/src/windows_ui.rs') -Raw
$updateWorkflow = Get-Content -LiteralPath (Join-Path $root 'tools/prepare-release-update.ps1') -Raw

function Assert-True([bool]$Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}
function Assert-Contains([string]$Text, [string]$Pattern, [string]$Message) {
    Assert-True ($Text -match $Pattern) $Message
}
function Assert-NotContains([string]$Text, [string]$Pattern, [string]$Message) {
    Assert-True ($Text -notmatch $Pattern) $Message
}

Assert-Contains $installer '(?m)^PrivilegesRequired=lowest$' 'Installer must remain per-user and non-elevated.'
Assert-Contains $installer '(?m)^DefaultDirName=\{localappdata\}\\Programs\\OpenShazam$' 'Installer must use the per-user Programs folder.'
Assert-Contains $installer '(?m)^ArchitecturesAllowed=x64$' 'Installer must restrict the package to x64.'
Assert-Contains $installer "ProductRunKey = 'Software\\Microsoft\\Windows\\CurrentVersion\\Run'" 'Startup must use the current-user Run key.'
Assert-Contains $installer "ProductRunValue = 'OpenShazam'" 'Startup must use the owned Shazam value name.'
Assert-Contains $installer 'CompareText\(ExistingCommand, StartupCommandFor\(PreviousInstallDir\)\)' 'Upgrade must validate the old owned startup command before changing its path.'
Assert-Contains $installer 'CurUninstallStepChanged\(CurUninstallStep: TUninstallStep\)' 'Uninstall cleanup must be implemented.'
Assert-Contains $installer "RegDeleteKeyIncludingSubkeys\(HKCU, 'Software\\OpenShazam'\)" 'Uninstall must remove the product settings key.'
Assert-Contains $installer 'DelTree\(LocalData, True, True, True\)' 'Uninstall must remove the complete app-owned local data directory, including history and downloaded updates.'
Assert-NotContains $installer '(?i)StartupTask|Task Scheduler|PrivilegesRequired=admin|HKLM|ServicesRun|uninsdeletekeyifany' 'Installer contains a prohibited startup or privilege mechanism.'
Assert-NotContains $startupHost 'StartupTask|GetCurrentPackageFullName|PackageIdentity' 'Startup UI still contains a package-identity branch.'
Assert-Contains $startupHost 'StartupApproved\\\\Run' 'Settings must read Windows Startup Apps effective state.'
Assert-Contains $startupHost 'HasMatchingRunStartupValue' 'Settings must validate the exact Shazam Run command.'
Assert-Contains $startupHost 'TitleBarTheme::UseDefaultAppMode' 'System theme must use the Windows default app mode for the native title bar.'
Assert-Contains $historyStore 'OpenShazam.*history\.db' 'History must use the stable per-user OpenShazam storage root.'
Assert-Contains $historyStore 'migrate_legacy_history' 'Previous history locations must be migrated safely.'
Assert-NotContains ($ci + $release) '(?i)MSIX|MakeAppx|appinstaller|PFX_SIGNING' 'Active workflows must not build or sign MSIX packages.'
Assert-Contains $installer 'WindowsAppRuntimeInstall-x64.exe.*dontcopy' 'The Windows App SDK runtime prerequisite must be embedded in the setup payload.'
Assert-Contains $installer 'ExtractTemporaryFile\(''WindowsAppRuntimeInstall-x64.exe''\)' 'Setup must extract the runtime installer into its temporary area.'
Assert-Contains $installer 'WindowsAppRuntimeInstall-x64.exe.*--quiet' 'The Windows App SDK runtime must install quietly before Shazam can start.'
Assert-Contains $installer 'ResultCode <> 0' 'Setup must stop if the runtime prerequisite installation fails.'
Assert-Contains $installer 'PrivilegesRequired=lowest' 'Runtime installation must be launched under the current user.'
foreach ($crt in @('msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll')) {
    Assert-Contains $installer ([regex]::Escape($crt)) "Installer must include app-local VC++ dependency $crt."
    Assert-Contains (Get-Content -LiteralPath (Join-Path $root 'tools/build-windows.ps1') -Raw) ([regex]::Escape($crt)) "Build must stage app-local VC++ dependency $crt."
}
Assert-Contains (Get-Content -LiteralPath (Join-Path $root 'tools/prepare-windows-app-sdk-runtime.ps1') -Raw) 'expectedSha256' 'The Microsoft runtime download must be hash-pinned.'
Assert-Contains $ci 'prepare-windows-app-sdk-runtime.ps1' 'CI must download and verify the Microsoft runtime prerequisite.'
Assert-Contains $ci 'ISCC.exe' 'CI must compile the Inno Setup installer.'
Assert-Contains $release 'softprops/action-gh-release@v2' 'Stable Windows tags must publish the installer to GitHub Releases.'
Assert-Contains $release 'Shazam-Setup-.*-x64\.exe' 'Release assets must use the versioned setup executable.'
Assert-Contains $release 'Optionally Authenticode-sign application binaries' 'Authenticode signing must be optional for application binaries.'
Assert-Contains $release 'Optionally Authenticode-sign installer' 'Authenticode signing must be optional for the setup executable.'
Assert-Contains $signing 'Authenticode signing is not configured' 'Release signing must continue when no PFX certificate is configured.'
Assert-Contains $release 'OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64' 'A stable release must require the Ed25519 manifest signing key.'
Assert-Contains $release 'prepare-release-update.ps1' 'Release processing must generate and verify the signed update manifest.'
Assert-Contains $release 'OpenShazam-update\.json\.sig' 'The detached Ed25519 signature must be published with the manifest.'
Assert-Contains $update 'verify_strict' 'Update manifests must use strict Ed25519 verification.'
Assert-Contains $update 'verify_installer_path' 'Downloaded installers must be checked against the signed SHA-256 and length.'
Assert-Contains $update 'LATEST_MANIFEST_URL' 'The updater must use the fixed HTTPS GitHub Releases latest manifest URL.'
Assert-True ($update.Contains('https://github.com/NcaMoq/OpenShazam/releases/latest/download/OpenShazam-update.json')) 'The updater must fetch the manifest from the formal release repository.'
Assert-True ($update.Contains('https://github.com/NcaMoq/OpenShazam/releases/latest/download/OpenShazam-update.json.sig')) 'The updater must fetch the signature from the formal release repository.'
Assert-True ($updateWorkflow.Contains("'NcaMoq/OpenShazam'")) 'The signed manifest must name the formal release repository.'
Assert-True ($installer.Contains('AppPublisherURL=https://github.com/NcaMoq/OpenShazam')) 'The installer publisher URL must name the formal release repository.'
Assert-Contains $updateUi 'begin_update_check\(&mut \*app_ptr, false\)' 'The updater must check the stable signed manifest at application startup.'
Assert-Contains $updateUi 'UPDATE_CHECK_INTERVAL_MS: u32 = 24 \* 60 \* 60 \* 1000' 'The updater must schedule a daily background check.'
Assert-Contains $updateUi 'if !interactive \{' 'Background update checks must stay silent unless a verified update is available.'
Assert-Contains $updateWorkflow 'verify \$manifest \$signature \$installer' 'Release generation must verify the output manifest and installer before publication.'
Assert-Contains $signing 'Code Signing enhanced key usage' 'The signing helper must validate a code-signing certificate.'
Assert-Contains $signing 'signtool verify /pa /all /v' 'The signing helper must verify Authenticode signatures.'
Assert-Contains $signing 'Remove-Item -LiteralPath \$pfxPath -Force' 'The signing helper must remove its temporary PFX file.'

foreach ($path in @(
    'apps/openshazam/Package.appxmanifest',
    '.github/workflows/release-windows-msix.yml',
    'tools/package-windows-msix.ps1',
    'tools/verify-msix-feed.ps1'
)) {
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $root $path))) "Retired MSIX artifact remains active: $path"
}

Write-Host 'Windows installer, startup, history, and signed update release contract passed.'
