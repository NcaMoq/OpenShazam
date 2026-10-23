$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
function Read-ContractText([string]$RelativePath) {
    (Get-Content -LiteralPath (Join-Path $root $RelativePath) -Raw).Replace("`r`n", "`n")
}
$installer = Read-ContractText 'installer/OpenShazam.iss'
$startupHost = Read-ContractText 'apps/openshazam/settings-host/SettingsHost.cpp'
$rustSettings = Read-ContractText 'apps/openshazam/src/windows_settings.rs'
$historyStore = Read-ContractText 'apps/openshazam/src/windows_history_store.rs'
$cargoWorkspace = Read-ContractText 'Cargo.toml'
$readme = Read-ContractText 'README.md'
$notifications = Read-ContractText 'apps/openshazam/src/windows_ui/notifications.rs'
$security = Read-ContractText 'SECURITY.md'
$installerBuilder = Read-ContractText 'tools/build-installer.ps1'
$sidecarVerifier = Read-ContractText 'tools/verify-sha256-sidecar.ps1'
$settingsProject = Read-ContractText 'apps/openshazam/settings-host/SettingsHost.vcxproj'
$settingsHostTest = Read-ContractText 'tools/test-windows-settings-host.ps1'
$ci = Read-ContractText '.github/workflows/ci.yml'
$release = Read-ContractText '.github/workflows/release-windows.yml'
$signing = Read-ContractText 'tools/sign-windows-artifacts.ps1'
$signingTest = Read-ContractText 'tools/test-windows-signing-contract.ps1'
$update = Read-ContractText 'crates/openshazam-update/src/lib.rs'
$updateUi = (Read-ContractText 'apps/openshazam/src/windows_ui.rs') +
    (Read-ContractText 'apps/openshazam/src/windows_ui/lifecycle.rs') +
    (Read-ContractText 'apps/openshazam/src/windows_ui/window_proc.rs') +
    (Read-ContractText 'apps/openshazam/src/windows_ui/update.rs')
$windowProc = Read-ContractText 'apps/openshazam/src/windows_ui/window_proc.rs'
$updateWindows = Read-ContractText 'apps/openshazam/src/windows_update.rs'
$artworkNetwork = Read-ContractText 'apps/openshazam/src/windows_ui/artwork_network.rs'
$httpClient = Read-ContractText 'crates/shazam-http-lab/src/lib.rs'
$updateWorkflow = Read-ContractText 'tools/prepare-release-update.ps1'
$releaseVerifier = Read-ContractText 'tools/verify-release-assets.ps1'
$gitignore = Read-ContractText '.gitignore'
$licenseConfig = Read-ContractText 'about.toml'
$licenseTemplate = Read-ContractText 'tools/third-party-notices.hbs'
$licenseGenerator = Read-ContractText 'tools/generate-third-party-notices.ps1'
$nativeLicenseNotices = Read-ContractText 'tools/native-third-party-notices.txt'
$nugetPackageConfig = Read-ContractText 'apps/openshazam/settings-host/packages.config'
$nugetTrustConfig = Read-ContractText 'NuGet.Config'
$nugetSources = [regex]::Match($nugetTrustConfig, '(?s)<packageSources>(.*?)</packageSources>').Groups[1].Value
$nativePackageVerifier = Read-ContractText 'tools/verify-native-package-signatures.ps1'
$windowsBuilder = Read-ContractText 'tools/build-windows.ps1'
$thirdPartyNotices = Read-ContractText 'THIRD-PARTY-NOTICES.txt'
$runtimePrerequisite = Read-ContractText 'tools/prepare-windows-app-sdk-runtime.ps1'

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
Assert-Contains $installer 'OPEN_SHAZAM_VERSION must be set' 'The installer script must fail instead of silently using a stale version.'
Assert-NotContains $installer '(?m)^\s*#define ProductVersion "\d+\.\d+\.\d+"$' 'The installer script must not have a hard-coded fallback version.'
Assert-Contains $installer '(?m)^DefaultDirName=\{localappdata\}\\Programs\\OpenShazam$' 'Installer must use the per-user Programs folder.'
Assert-Contains $installer '(?m)^AppId=\{\{4BA8918E-564F-4D30-9705-A63A48A69977\}$' 'The installer AppId must stay stable so an update replaces the existing installation.'
Assert-Contains $installer '(?m)^UsePreviousAppDir=yes$' 'An update must retain the existing per-user installation directory.'
Assert-NotContains $installer '(?mi)^\s*\[InstallDelete\]\s*$' 'An in-place update must not delete files before replacing the installed application.'
Assert-Contains $installer '(?m)^ArchitecturesAllowed=x64$' 'Installer must restrict the package to x64.'
Assert-Contains $installer 'THIRD-PARTY-NOTICES\.txt.*DestDir: "\{app\}"' 'The installer must include third-party license notices for the Windows product build.'
Assert-Contains $installer '(?m)^RedirectionGuard=yes$' 'Setup and uninstall must block traversal of unprivileged reparse points in writable locations.'
Assert-Contains $installer "ProductRunKey = 'Software\\Microsoft\\Windows\\CurrentVersion\\Run'" 'Startup must use the current-user Run key.'
Assert-Contains $installer "ProductRunValue = 'OpenShazam'" 'Startup must use the owned Shazam value name.'
Assert-Contains $installer 'CompareText\(ExistingCommand, StartupCommandFor\(PreviousInstallDir\)\)' 'Upgrade must validate the old owned startup command before changing its path.'
Assert-Contains $installer 'CurUninstallStepChanged\(CurUninstallStep: TUninstallStep\)' 'Uninstall cleanup must be implemented.'
Assert-Contains $installer "RegDeleteKeyIncludingSubkeys\(HKCU, 'Software\\OpenShazam'\)" 'Uninstall must remove the product settings key.'
Assert-Contains $installer 'DelTree\(LocalData, True, True, True\)' 'Uninstall must remove the complete app-owned local data directory, including history and downloaded updates.'
Assert-Contains $installer 'if LocalDataAttributes <> InvalidFileAttributes then\s+if not IsSafeHistoryDirectory\(LocalData\) or\s+not DelTree\(LocalData, True, True, True\) then\s+CleanupFailed := True' 'Uninstall must refuse to follow an unsafe app-data root and surface that cleanup failure.'
Assert-Contains $installer 'SuppressibleMsgBox\(CustomMessage\(''UninstallCleanupFailed''\), mbError, MB_OK, IDOK\)' 'Cleanup failures must be reported interactively and remain compatible with silent uninstall.'
Assert-Contains $installer 'english\.UninstallCleanupFailed=.*HKEY_CURRENT_USER\\Software' 'The English uninstaller must explain how to remove data left by a failed cleanup.'
Assert-Contains $installer 'japanese\.UninstallCleanupFailed=.*HKEY_CURRENT_USER\\Software' 'The Japanese uninstaller must explain how to remove data left by a failed cleanup.'
Assert-Contains $installer "GetFileAttributesW@kernel32\.dll stdcall uninstallonly" 'Legacy history cleanup must inspect Windows file attributes during uninstall.'
Assert-Contains $installer 'if not IsSafeHistoryDirectory\(PackageDir\) then\s+Result := False' 'Unsafe legacy package directories must not be followed and must surface as cleanup failures.'
Assert-Contains $installer 'if not IsSafeHistoryDirectory\(LocalStateDir\) then\s+Result := False' 'Unsafe legacy LocalState directories must not be followed and must surface as cleanup failures.'
Assert-Contains $installer 'if Attributes <> InvalidFileAttributes then\s+if not IsSafeHistoryFile\(FilePath\) or\s+not DeleteFile\(FilePath\) then\s+Result := False' 'Legacy history cleanup must refuse unsafe entries and report skipped or failed deletion.'
Assert-Contains $installer 'if not DeletePreviousPackageHistory\(ExpandConstant\(''\{localappdata\}\\Packages''\)\) then\s+CleanupFailed := True' 'Failed legacy package history cleanup must be surfaced during uninstall.'
Assert-NotContains $installer '(?i)StartupTask|Task Scheduler|PrivilegesRequired=admin|HKLM|ServicesRun|uninsdeletekeyifany' 'Installer contains a prohibited startup or privilege mechanism.'
Assert-NotContains $startupHost 'StartupTask|GetCurrentPackageFullName|PackageIdentity' 'Startup UI still contains a package-identity branch.'
Assert-Contains $startupHost 'StartupApproved\\\\Run' 'Settings must read Windows Startup Apps effective state.'
Assert-Contains $startupHost 'HasMatchingRunStartupValue' 'Settings must validate the exact Shazam Run command.'
Assert-Contains $startupHost 'controls::ProgressRing' 'The update status card must use a native WinUI progress ring.'
Assert-Contains $rustSettings 'LoadLibraryExW[\s\S]*PRODUCT_DLL_SEARCH_FLAGS' 'The Rust host must load product DLLs with a restricted dependency search path.'
Assert-Contains $rustSettings 'LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR[\s\S]*LOAD_LIBRARY_SEARCH_SYSTEM32' 'The settings host must resolve dependencies only from the DLL directory and System32.'
Assert-NotContains $rustSettings 'LoadLibraryW\(' 'The settings host must not use the legacy DLL search path.'
Assert-Contains $settingsHostTest 'LoadLibraryExW[\s\S]*ProductDllSearchFlags' 'The native Settings host integration test must use the production restricted DLL search policy.'
Assert-Contains $startupHost 'L"Checking for updates..."' 'The update status card must show a checking state.'
Assert-Contains $startupHost 'L"Install now"' 'The update status card must offer an explicit install action when an update is available.'
Assert-Contains $startupHost 'appearance\.Items\(\)\.Append\(winrt::box_value\(Localized\(L"Light"' 'The theme selector must contain the Light option.'
Assert-Contains $startupHost 'appearance\.Items\(\)\.Append\(winrt::box_value\(Localized\(L"Dark"' 'The theme selector must contain the Dark option.'
Assert-Contains $startupHost 'appearance\.Items\(\)\.Append\(winrt::box_value\(Localized\(\s*L"System"' 'The theme selector must contain the System option.'
Assert-Contains $startupHost 'TitleBarTheme::UseDefaultAppMode' 'System theme must use the Windows default app mode for the native title bar.'
Assert-Contains $historyStore 'OpenShazam.*history\.db' 'History must use the stable per-user OpenShazam storage root.'
Assert-NotContains $historyStore 'source_file\.display\(\)|destination_file\.display\(\)|could not open \{\}:' 'History migration errors must not expose per-user absolute paths.'
Assert-NotContains $rustSettings 'path\.display\(\)|bootstrap_path\.display\(\)' 'Settings host errors must not expose per-user absolute paths.'
Assert-Contains $notifications 'notify_update_result\([\s\S]*queue_balloon_icon\(owned_balloon_icons, None\)' 'System-icon update balloons must reserve their callback slot to preserve artwork icon ownership order.'
Assert-Contains $historyStore 'migrate_legacy_history' 'Previous history locations must be migrated safely.'
Assert-Contains $settingsProject '<WindowsTargetPlatformMinVersion>10\.0\.19041\.0</WindowsTargetPlatformMinVersion>' 'The settings host minimum Windows build must be explicit.'
Assert-Contains $readme 'Windows 10 version 2004 \(build 19041\) or later, x64' 'README must describe the supported Windows platform.'
Assert-Contains $readme 'Ed25519-signed update manifest' 'README must describe update signature verification.'
Assert-Contains $readme 'raw audio is not uploaded' 'README privacy details must state that raw audio is not uploaded.'
Assert-Contains $readme '%LOCALAPPDATA%\\OpenShazam\\history\.db' 'README must identify the local history database location.'
Assert-Contains $readme 'Windows language and locale, device model, Windows version and build' 'README must disclose the host information sent to service configuration.'
Assert-Contains $readme 'SHAZAM_TAG_URL_TEMPLATE.*SHAZAM_INID.*when both are set' 'README must disclose the paired Manual route overrides.'
Assert-Contains $readme 'public GitHub Releases endpoints, which also receive ordinary connection metadata such as your IP address and the requested asset URL' 'README must disclose the connection metadata sent to GitHub update endpoints.'
Assert-Contains $readme 'does not implement its own usage analytics or crash-reporting service' 'README must clarify that OpenShazam does not add its own analytics or crash reporting.'
Assert-Contains $readme 'currently published v0\.1\.1 installer is not Authenticode-signed' 'README must disclose the published installer signature state.'
Assert-Contains $readme 'Windows App SDK Runtime; its runtime components may participate in Windows diagnostic data collection' 'README must disclose Windows diagnostic data behavior for the bundled Settings runtime.'
Assert-Contains $readme 'Privacy & security > Diagnostics & feedback' 'README must identify the Windows diagnostic data privacy controls.'
Assert-Contains $httpClient 'manual recognition route overrides must be paired' 'The network client must require both Manual route override values.'
Assert-Contains $httpClient 'only the configured amp\.shazam\.com host is accepted' 'The Manual route override must retain the Shazam HTTPS host restriction.'
Assert-Contains $httpClient 'manual route overrides cannot select an Auto endpoint' 'Manual route overrides must not alter Auto Shazam routing.'
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
Assert-Contains $ci 'tools\\build-installer\.ps1' 'CI must compile the installer through the shared version-checked build script.'
Assert-Contains $release 'tools\\build-installer\.ps1 -Version \$env:OPEN_SHAZAM_VERSION' 'Release must build through the shared script and validate its version against Cargo.'
Assert-Contains $installerBuilder '\[workspace\.package\]' 'The shared installer build must read the Cargo workspace version.'
Assert-Contains $installerBuilder 'does not match Cargo workspace version' 'The shared installer build must reject a release version that differs from Cargo.'
Assert-Contains $installerBuilder 'Get-FileHash.*SHA256' 'The shared installer build must generate the SHA-256 sidecar.'
Assert-Contains $ci 'verify-sha256-sidecar\.ps1 -Path \$installer\.FullName' 'Windows CI must run the shared exact SHA-256 sidecar verifier.'
Assert-Contains $releaseVerifier 'verify-sha256-sidecar\.ps1[\s\S]*-Path \$installerPath' 'Downloaded release assets must run the shared exact SHA-256 sidecar verifier.'
Assert-Contains $sidecarVerifier 'expectedSidecar[\s\S]*\[IO\.File\]::ReadAllBytes[\s\S]*Base64String\(\$actualBytes\) -cne \[Convert\]::ToBase64String\(\$expectedBytes\)' 'The shared SHA-256 verifier must compare the exact sidecar bytes, including file name and line ending.'
Assert-Contains $installerBuilder 'THIRD-PARTY-NOTICES\.txt' 'The shared installer build must require generated third-party notices.'
Assert-Contains $installerBuilder 'LOCALAPPDATA.*Process[\s\S]*Programs\\Inno Setup 6\\ISCC\.exe' 'The shared installer build must find a per-user Inno Setup installation.'
Assert-Contains $readme '(?m)^\.\\tools\\build-installer\.ps1$' 'README must use the shared installer build script.'
$workspaceSection = [regex]::Match($cargoWorkspace, '(?ms)^\[workspace\.package\]\s*(?<body>.*?)(?=^\[|\z)')
Assert-True $workspaceSection.Success 'Cargo.toml must contain [workspace.package] for installer version validation.'
$workspaceVersionMatch = [regex]::Match($workspaceSection.Groups['body'].Value, '(?m)^version\s*=\s*"(?<value>[^"\r\n]+)"')
Assert-True $workspaceVersionMatch.Success 'Cargo workspace version must be present for installer version validation.'
$incorrectInstallerVersion = if ($workspaceVersionMatch.Groups['value'].Value -ceq '0.0.0') { '0.0.1' } else { '0.0.0' }
$installerVersionError = $null
try {
    & (Join-Path $root 'tools/build-installer.ps1') -Version $incorrectInstallerVersion | Out-Null
}
catch {
    $installerVersionError = $_.Exception.Message
}
Assert-Contains ([string]$installerVersionError) 'does not match Cargo workspace version' 'The shared installer build must reject a mismatched release version before invoking Inno Setup.'
Assert-Contains ($ci + $release) 'innosetup --version=6\.7\.1' 'CI and Release must use Inno Setup with the Windows RedirectionGuard support required by uninstall cleanup.'
Assert-NotContains $ci '(?m)^  push:[ \t]*(?:#.*)?$' 'Windows CI must not run automatically on every main push.'
Assert-Contains $ci '(?m)^  pull_request:\n    branches: \[main\]$' 'Pull requests targeting main must run Windows CI.'
Assert-Contains $ci '(?m)^  workflow_dispatch:$' 'Windows CI must remain manually runnable.'
Assert-Contains $ci '(?m)^  workflow_call:$' 'Release workflow must be able to require the full CI workflow.'
Assert-Contains $ci '(?ms)^      - name: Verify the isolated WinUI Settings host\s*\n        shell: powershell\s*\n        run: \.\\tools\\test-windows-settings-host\.ps1 -UseCurrentDesktop' 'The hosted WinUI Settings smoke test must use Windows PowerShell and the runner desktop; local runs stay isolated by default.'
Assert-Contains $release '(?ms)^  verify-release-secrets:\s*\n    name: Validate signing credentials before full Windows CI[\s\S]*?runs-on: windows-latest[\s\S]*?environment: stable-release[\s\S]*?OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64: \$\{\{ secrets\.OPEN_SHAZAM_RELEASE_SIGNING_CERTIFICATE_BASE64 \}\}[\s\S]*?OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD: \$\{\{ secrets\.OPEN_SHAZAM_RELEASE_SIGNING_CERTIFICATE_PASSWORD \}\}[\s\S]*?\.\\tools\\sign-windows-artifacts\.ps1 -Path @\(\) -RequireSignature -ValidateOnly[\s\S]*?OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64: \$\{\{ secrets\.OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64 \}\}' 'Release signing credentials must be validated without exposing their values before the reusable Windows CI workflow.'
Assert-Contains $release '(?m)^    needs: \[verify-release-source, verify-release-secrets\]$' 'Release source and secret preflight must both pass before the reusable Windows CI workflow runs.'
Assert-Contains $release '(?m)^    needs: \[verify-release-source, verify-release-secrets, validate\]$' 'Publishing must require source validation, signing-secret preflight, and full Windows CI.'
Assert-Contains $ci 'actions/checkout@[0-9a-f]{40}' 'CI actions must use immutable commit references.'
Assert-Contains $ci 'test-windows-settings-host\.ps1' 'CI must run the isolated WinUI Settings host check.'
Assert-Contains $ci 'test-windows-composition\.ps1' 'CI must run the native Composition integration checks.'
Assert-Contains $ci 'test-rust-unsafe-policy\.ps1' 'CI must enforce the unsafe-operation lint policy for application Rust sources.'
Assert-Contains $ci 'test-windows-signing-contract\.ps1' 'CI must test that signing fails closed when release credentials are unavailable.'
Assert-Contains $ci 'generate-third-party-notices\.ps1' 'CI must generate dependency license notices before compiling the installer.'
Assert-Contains $release 'Verify the CI-validated Windows build inputs[\s\S]*dist/windows-x64/THIRD-PARTY-NOTICES\.txt' 'Release must package the dependency notices produced and validated by CI.'
Assert-NotContains $release 'generate-third-party-notices\.ps1' 'Release must reuse dependency notices from the validated CI artifact.'
Assert-Contains $ci 'Swatinem/rust-cache@[0-9a-f]{40}' 'Windows CI must cache Rust build dependencies using an immutable action reference.'
Assert-Contains $ci "expectedAuditVersion = 'cargo-audit 0\.22\.1'" 'Windows CI must use exactly the pinned cargo-audit version.'
Assert-Contains $ci "expectedSha256 = '1523eedc57675eff07b230c06dcd545f3cb04e0ebf92f4009218c79000279a74'" 'Windows CI must pin the official cargo-audit archive digest.'
Assert-Contains $ci 'cargo-audit-x86_64-pc-windows-msvc-v0\.22\.1\.zip[\s\S]*Get-FileHash[\s\S]*actualSha256 -cne \$expectedSha256[\s\S]*Expand-Archive[\s\S]*installedAuditVersion -cne \$expectedAuditVersion[\s\S]*cargo audit --file Cargo\.lock' 'Windows CI must hash-pin, verify and version-check the official cargo-audit binary before auditing the locked workspace.'
Assert-Contains $ci 'curl\.Source.*--proto-redir ''=https''.*--output \$archivePath \$downloadUrl' 'CI must fetch the pinned security scanner over HTTPS only.'
Assert-Contains $licenseGenerator 'curl\.Source.*--proto-redir ''=https''.*--output \$archivePath \$downloadUri[\s\S]*Get-FileHash' 'The license generator must download its pinned tool over HTTPS only before verifying the archive hash.'
Assert-Contains $runtimePrerequisite 'curl\.Source.*--proto-redir ''=https''.*--output \$output \$uri[\s\S]*Get-FileHash' 'The Windows App SDK runtime must be downloaded over HTTPS only and hash-checked.'
Assert-Contains $runtimePrerequisite 'Get-AuthenticodeSignature' 'The Windows App SDK runtime signature must be verified.'
Assert-Contains $ci '\.\\tools\\prepare-windows-app-sdk-runtime\.ps1[\s\S]*\$runtimeInstaller = Join-Path \$PWD ''target/windows-app-sdk-runtime/WindowsAppRuntimeInstall-x64\.exe''[\s\S]*& \$runtimeInstaller --quiet[\s\S]*\$LASTEXITCODE -ne 0' 'Windows CI must quietly install the verified Windows App SDK runtime and fail if installation is unsuccessful.'
Assert-NotContains $ci 'cargo install cargo-audit' 'Windows CI must not spend runner time compiling cargo-audit from source.'
Assert-Contains $ci 'cargo check --workspace --all-targets --locked --no-default-features' 'CI must compile production configurations without developer CLI features.'
Assert-Contains $release 'uses: \./\.github/workflows/ci\.yml' 'Release must run the same required CI checks for its tag commit.'
Assert-Contains $release '(?m)^    needs: \[verify-release-source, verify-release-secrets, validate\]$' 'Release publication must wait for source, signing-secret, and CI validation.'
Assert-Contains $release 'Verify release tag is in main[\s\S]*git merge-base --is-ancestor "\$GITHUB_SHA" refs/remotes/origin/main' 'Release tags must be checked against protected main before signing secrets are available.'
Assert-Contains $release 'Refuse duplicate releases before Windows validation[\s\S]*gh api --paginate --jq' 'Duplicate releases must be rejected in the lightweight source gate before Windows CI consumes Actions minutes.'
Assert-Contains $release '(?m)^    needs: verify-release-source$' 'Release validation must wait for the protected-main tag check.'
Assert-Contains $release 'softprops/action-gh-release@[0-9a-f]{40}' 'Release action must use an immutable commit reference.'
Assert-Contains $release 'Shazam-Setup-.*-x64\.exe' 'Release assets must use the versioned setup executable.'
Assert-Contains $release 'outputs:\s*\n\s+version: \$\{\{ steps\.version\.outputs\.version \}\}[\s\S]*id: version[\s\S]*test_validate_release_version\.py[\s\S]*validate_release_version\.py' 'Stable tag and Cargo version validation must run in the lightweight source gate and expose the validated version.'
Assert-Contains $release '\$\{\{ needs\.verify-release-source\.outputs\.version \}\}' 'The Windows release job must use the version validated before expensive CI and secret access.'
Assert-True ($release.IndexOf('id: version') -lt $release.IndexOf('  validate:')) 'Stable tag and Cargo version validation must happen before the required Windows CI workflow.'
Assert-True ($release.IndexOf('id: version') -lt $release.IndexOf('  release:')) 'Stable tag and Cargo version validation must happen before the Windows signing job.'
Assert-True ($release.IndexOf('  verify-release-secrets:') -lt $release.IndexOf('  validate:')) 'Missing release signing secrets must fail before Windows CI consumes Actions minutes.'
$releaseJobStart = $release.IndexOf('  release:')
Assert-True ($releaseJobStart -ge 0) 'The Windows release job must exist.'
Assert-NotContains $release.Substring($releaseJobStart) 'steps\.version\.outputs\.version' 'The Windows release job must use the version output from its gated source job.'
Assert-Contains $release 'python3 tools/test_validate_release_version\.py' 'The lightweight release source gate must test its tag validator before Windows CI.'
Assert-Contains $licenseConfig '"Apache-2\.0"[\s\S]*"MIT"[\s\S]*"BSD-3-Clause"[\s\S]*"BSD-2-Clause"[\s\S]*"Unlicense"[\s\S]*"Zlib"[\s\S]*"Unicode-3\.0"' 'The cargo-about policy must explicitly allow only the reviewed permissive licenses.'
Assert-Contains $licenseConfig 'x86_64-pc-windows-msvc' 'Dependency license notices must match the shipped Windows x64 target.'
Assert-Contains $licenseTemplate '\{\{\{text\}\}\}' 'Generated dependency notices must include the complete license text.'
Assert-Contains $licenseGenerator "\`$toolVersion = '0\.9\.1'" 'The notice generator must pin cargo-about to version 0.9.1.'
Assert-Contains $licenseGenerator '\$archiveName = "cargo-about-\$toolVersion-x86_64-pc-windows-msvc\.tar\.gz"' 'The notice generator must use the pinned Windows cargo-about archive.'
Assert-Contains $licenseGenerator '318893aff6b9efd60f70470f5827b9577ae20a805cf9732d5612862a78508581' 'The notice generator must verify the pinned tool archive checksum.'
Assert-Contains $licenseGenerator 'cargo fetch --manifest-path ''apps/openshazam/Cargo.toml'' --locked' 'The notice generator must fetch all Cargo.lock sources before running cargo-about offline because metadata may inspect target-specific crates.'
Assert-Contains $licenseGenerator '--offline' 'Third-party notices must be generated from the locked local dependency sources without live license lookups.'
Assert-True ($licenseGenerator.IndexOf('cargo fetch --manifest-path') -lt $licenseGenerator.IndexOf('--offline')) 'All locked crate sources must be fetched before offline license analysis.'
Assert-Contains $licenseGenerator 'THIRD-PARTY-NOTICES\.txt is stale' 'The notice generation step must fail when the committed third-party notices do not match the locked dependency graph.'
Assert-Contains $licenseGenerator '\$generatedNoticeText = \[IO\.File\]::ReadAllText\(\$generatedNoticePath\) -replace "`r`n\?", "`n"' 'Generated license text must be normalized to LF before comparison so Windows checkout line endings do not cause false stale notices.'
Assert-Contains $licenseGenerator '\$trackedNoticeText = \[IO\.File\]::ReadAllText\(\$trackedNoticePath\) -replace "`r`n\?", "`n"' 'The reviewed notice file must be normalized to LF before comparison.'
Assert-Contains $licenseGenerator '\$generatedNoticeText -cne \$trackedNoticeText[\s\S]*Copy-Item.*trackedNoticePath' 'CI must compare the complete normalized license text with the reviewed repository copy before staging it.'
Assert-NotContains $licenseGenerator 'Get-FileHash.*generatedNoticePath|Get-FileHash.*trackedNoticePath' 'License freshness checks must not depend on platform-specific newline bytes.'
$lfNoticeText = "license line one`nlicense line two`n"
$crlfNoticeText = $lfNoticeText.Replace("`n", "`r`n")
$normalizedLfNoticeText = $lfNoticeText -replace "`r`n?", "`n"
$normalizedCrlfNoticeText = $crlfNoticeText -replace "`r`n?", "`n"
Assert-True ($normalizedLfNoticeText -ceq $normalizedCrlfNoticeText) 'The Windows checkout CRLF form and generated LF form must compare as identical notice content.'
Assert-Contains $licenseGenerator '\[switch\]\$UpdateTrackedNotice' 'Maintainers must have an explicit command to refresh notices after reviewing dependency license changes.'
Assert-Contains $licenseGenerator 'packages\.config[\s\S]*packageNoticeText[\s\S]*AppendAllText\(\$generatedNoticePath' 'Native dependency notices must be generated from the exact restored NuGet packages and compared with the reviewed repository copy.'
Assert-Contains $nativeLicenseNotices 'msvcp140\.dll[\s\S]*vcruntime140\.dll[\s\S]*vcruntime140_1\.dll[\s\S]*learn\.microsoft\.com/cpp/windows/redistributing-visual-cpp-files' 'The native notice source must identify the redistributable Visual C++ DLLs and their Microsoft licensing guidance.'
Assert-Contains $nugetPackageConfig 'Microsoft\.WindowsAppSDK.*version="1\.8\.260317003"' 'The SettingsHost must pin the Windows App SDK package whose license and notice files are redistributed.'
Assert-Contains $nugetPackageConfig 'Microsoft\.Web\.WebView2.*version="1\.0\.3179\.45"' 'The SettingsHost must restore WebView2 metadata required by the WinUI C++/WinRT projection.'
Assert-Contains $nugetTrustConfig '<packageSources>[\s\S]*<clear\s*/>[\s\S]*api\.nuget\.org/v3/index\.json' 'Native package restore must use only the explicitly configured NuGet.org source.'
Assert-True (([regex]::Matches($nugetSources, '<add key=')).Count -eq 1) 'Native package restore must configure exactly one package source.'
Assert-Contains $nugetTrustConfig '<add key="signatureValidationMode" value="require"' 'Native package restore must reject unsigned packages.'
Assert-Contains $nugetTrustConfig '<repository name="nuget\.org" serviceIndex="https://api\.nuget\.org/v3/index\.json">[\s\S]*<certificate fingerprint="0E5F38F57DC1BCC806D8494F4F90FBCEDD988B46760709CBEEC6F4219AA6157D" hashAlgorithm="SHA256" allowUntrustedRoot="false"\s*/>[\s\S]*<certificate fingerprint="5A2901D6ADA3D18260B9C6DFE2133C95D74B9EEF6AE0E5DC334C8454D1477DF4" hashAlgorithm="SHA256" allowUntrustedRoot="false"\s*/>[\s\S]*<certificate fingerprint="1F4B311D9ACC115C8DC8018B5A49E00FCE6DA8E2855F9F014CA6F34570BC482D" hashAlgorithm="SHA256" allowUntrustedRoot="false"\s*/>[\s\S]*<owners>Microsoft;webview;CppWinRT;MicrosoftReunionESTeam</owners>' 'Native package restore must trust the pinned NuGet.org repository certificates and reviewed package owners only.'
Assert-Contains $nativePackageVerifier 'foreach \(\$package in \$packageConfig\.packages\.package\)[\s\S]*verify -Signatures[\s\S]*-ConfigFile \$nugetConfigPath' 'Every pinned native package archive must pass NuGet signature and trusted-owner verification.'
Assert-Contains $windowsBuilder 'nuget restore[\s\S]*-ConfigFile \$nugetConfig[\s\S]*verify-native-package-signatures\.ps1' 'The Windows product build must apply repository NuGet trust policy during restore and verify all restored package archives.'

$sidecarTestDirectory = Join-Path ([IO.Path]::GetTempPath()) ("openshazam-sidecar-contract-" + [Guid]::NewGuid().ToString('N'))
$sidecarTempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
$sidecarTestDirectory = [IO.Path]::GetFullPath($sidecarTestDirectory)
if (-not $sidecarTestDirectory.StartsWith($sidecarTempRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The SHA-256 contract test directory escaped the system temporary directory.'
}
$null = New-Item -ItemType Directory -Path $sidecarTestDirectory
$sidecarTestInstallerName = 'Shazam-Setup-0.1.2-x64.exe'
$sidecarTestInstaller = Join-Path $sidecarTestDirectory $sidecarTestInstallerName
$sidecarTestPath = "$sidecarTestInstaller.sha256"
try {
    [IO.File]::WriteAllBytes($sidecarTestInstaller, [byte[]](1, 2, 3, 4, 5))
    $sidecarTestHash = (Get-FileHash -LiteralPath $sidecarTestInstaller -Algorithm SHA256).Hash.ToLowerInvariant()
    $sidecarTestValid = "$sidecarTestHash *$sidecarTestInstallerName`n"
    $utf8NoBom = [Text.UTF8Encoding]::new($false)
    [IO.File]::WriteAllText($sidecarTestPath, $sidecarTestValid, $utf8NoBom)
    $verifiedSidecarHash = & (Join-Path $root 'tools/verify-sha256-sidecar.ps1') -Path $sidecarTestInstaller
    Assert-True ($verifiedSidecarHash -ceq $sidecarTestHash) 'The shared SHA-256 verifier must accept the exact generated sidecar.'

    foreach ($invalidSidecar in @(
        "$sidecarTestValid unexpected",
        "$sidecarTestHash *wrong-installer.exe`n",
        "$sidecarTestHash *$sidecarTestInstallerName`r`n",
        "0000000000000000000000000000000000000000000000000000000000000000 *$sidecarTestInstallerName`n"
    )) {
        [IO.File]::WriteAllText($sidecarTestPath, $invalidSidecar, $utf8NoBom)
        $sidecarWasRejected = $false
        try {
            & (Join-Path $root 'tools/verify-sha256-sidecar.ps1') -Path $sidecarTestInstaller | Out-Null
        }
        catch {
            $sidecarWasRejected = $true
        }
        Assert-True $sidecarWasRejected 'The shared SHA-256 verifier must reject malformed, mislabeled, or mismatched sidecars.'
    }
}
finally {
    foreach ($temporaryFile in @($sidecarTestInstaller, $sidecarTestPath)) {
        if (Test-Path -LiteralPath $temporaryFile -PathType Leaf) {
            Remove-Item -LiteralPath $temporaryFile -Force
        }
    }
    $sidecarTestDirectoryExists = Test-Path -LiteralPath $sidecarTestDirectory -PathType Container
    $sidecarTestDirectoryEntries = if ($sidecarTestDirectoryExists) {
        @(Get-ChildItem -LiteralPath $sidecarTestDirectory -Force)
    }
    else {
        @()
    }
    if ($sidecarTestDirectoryExists -and $sidecarTestDirectoryEntries.Count -eq 0) {
        Remove-Item -LiteralPath $sidecarTestDirectory -Force
    }
}
Assert-Contains $settingsProject 'Microsoft\.Web\.WebView2\.Core\.winmd' 'The SettingsHost must reference WebView2 metadata required by WinUI C++/WinRT code generation.'
Assert-Contains $thirdPartyNotices 'Microsoft.WindowsAppSDK.Runtime 1\.8\.260317003 / NOTICE\.txt[\s\S]*Newtonsoft\.Json 13\.0\.1 - MIT' 'The shipped notices must include Windows App SDK runtime third-party notices.'
Assert-Contains $thirdPartyNotices 'Microsoft.Windows.CppWinRT 2\.0\.230706\.1 / LICENSE[\s\S]*MIT License[\s\S]*Copyright \(c\) Microsoft Corporation' 'The shipped notices must include the native C++/WinRT license text.'
Assert-Contains $release 'Validate Authenticode certificate before full Windows CI[\s\S]*-RequireSignature -ValidateOnly' 'Stable releases must reject an invalid code signing certificate before running the full Windows CI workflow.'
Assert-Contains $release '(?m)^    environment: stable-release$' 'Release signing secrets must be scoped to the protected stable-release environment.'
Assert-Contains $release 'secrets\.OPEN_SHAZAM_RELEASE_SIGNING_CERTIFICATE_BASE64' 'The release workflow must read the Authenticode certificate from the release environment.'
Assert-Contains $release 'secrets\.OPEN_SHAZAM_RELEASE_SIGNING_CERTIFICATE_PASSWORD' 'The release workflow must read the Authenticode password from the release environment.'
Assert-Contains $release 'secrets\.OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64' 'The release workflow must read the registered repository Ed25519 secret.'
Assert-NotContains $release 'secrets\.WINDOWS_SIGNING_CERTIFICATE_(BASE64|PASSWORD)' 'Stable release jobs must not consume repository-level Authenticode secrets.'
Assert-NotContains $release 'OPEN_SHAZAM_RELEASE_UPDATE_SIGNING_KEY_BASE64' 'The release workflow must not require an unregistered duplicate Ed25519 secret.'
Assert-Contains $release 'RequireSignature' 'The application binaries and setup executable must require Authenticode signing.'
Assert-Contains $release 'failure\(\) \|\| cancelled\(\)' 'A workflow-created draft must be removed if the Release job fails or is cancelled.'
Assert-Contains $release 'steps\.create_draft\.outputs\.release_id' 'Draft cleanup must use the ID returned by this workflow run.'
Assert-Contains $security 'OPEN_SHAZAM_RELEASE_SIGNING_CERTIFICATE_BASE64' 'SECURITY.md must document how maintainers configure the required Authenticode secret.'
Assert-Contains $security 'OPEN_SHAZAM_RELEASE_SIGNING_CERTIFICATE_PASSWORD' 'SECURITY.md must document how maintainers configure the PFX password secret.'
Assert-Contains $security 'OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64' 'SECURITY.md must document the required Ed25519 update-signing secret.'
Assert-Contains $security 'Previously published releases are not retroactively changed' 'SECURITY.md must distinguish the future signing gate from existing releases.'
Assert-Contains $security 'validates the Authenticode certificate before full Windows CI' 'SECURITY.md must describe when the Authenticode preflight runs.'
Assert-Contains $security 'Ed25519 key is checked against the embedded public key when the signed update manifest is generated' 'SECURITY.md must describe when the Ed25519 key is validated.'
Assert-NotContains $security 'stops before building if any required secret is missing or invalid' 'SECURITY.md must not claim that the Ed25519 key is fully validated before CI when its public-key match is checked during manifest generation.'
Assert-Contains $release 'gh api --paginate --jq' 'Release preflight must inspect every release page, including drafts, before creation.'
Assert-Contains $release 'Create isolated draft release' 'The release workflow must create a uniquely identified draft before uploading assets.'
Assert-Contains $ci 'Build the release manifest verifier[\s\S]*cargo build --locked --release -p openshazam-update --bin openshazam-update-tool[\s\S]*target/release/openshazam-update-tool\.exe' 'CI must build and upload the update verifier from the validated tag commit.'
Assert-Contains $ci 'workflow_call:[\s\S]*upload_release_inputs:[\s\S]*type: boolean[\s\S]*default: false' 'Normal CI runs must not upload large release-only build artifacts.'
Assert-Contains $release 'uses: \./\.github/workflows/ci\.yml[\s\S]*with:[\s\S]*upload_release_inputs: true' 'The release workflow must request validation artifacts only for release runs.'
Assert-Contains $ci 'Build the release manifest verifier[\s\S]*if: \$\{\{ inputs\.upload_release_inputs \}\}[\s\S]*cargo build --locked --release -p openshazam-update' 'Only release CI runs may build the reusable manifest verifier.'
Assert-Contains $ci 'actions/upload-artifact@[0-9a-f]{40}[\s\S]*if: \$\{\{ inputs\.upload_release_inputs \}\}' 'Only release CI runs may upload product build inputs.'
Assert-Contains $ci 'dist/windows-x64/openshazam\.exe[\s\S]*dist/windows-x64/OpenShazam\.SettingsHost\.dll[\s\S]*target/release/openshazam-update-tool\.exe' 'CI must upload all signed application inputs and the verifier required for the Release package.'
Assert-Contains $ci 'retention-days: 1' 'Release-only build inputs must have minimal artifact retention.'
Assert-Contains $release 'actions/download-artifact@[0-9a-f]{40}[\s\S]*name: Shazam-Windows-x64-validation[\s\S]*path: \.' 'The release job must consume the build inputs from its successful validation job.'
Assert-Contains $release 'Verify the CI-validated Windows build inputs[\s\S]*target/release/openshazam-update-tool\.exe' 'The release job must reject an incomplete CI artifact before signing or packaging.'
Assert-NotContains $release.Substring($releaseJobStart) 'build-windows\.ps1|generate-third-party-notices\.ps1' 'The release job must not rebuild products already built and tested by CI.'
Assert-Contains $release 'prepare-windows-app-sdk-runtime\.ps1' 'The release job must independently fetch and hash-verify the Windows App SDK prerequisite.'
Assert-Contains $release 'prepare-release-update\.ps1[\s\S]*-UseValidatedUpdateTool' 'Release manifest generation must use the verifier binary produced by the required CI run.'
Assert-Contains $updateWorkflow '\[switch\]\$UseValidatedUpdateTool[\s\S]*The Ed25519 update tool from the validated Windows CI artifact is missing' 'Release manifest generation must fail if the CI-validated verifier artifact is absent.'
Assert-Contains $release 'id: create_draft' 'The workflow must retain the exact ID of the draft it created.'
Assert-Contains $release 'Remove incomplete draft created by this workflow' 'A failed upload or verification must trigger cleanup of the isolated draft.'
Assert-Contains $release 'if: \$\{\{ \(failure\(\) \|\| cancelled\(\)\) && steps\.create_draft\.outputs\.release_id !=' 'Draft cleanup must run after failure or cancellation and only with the captured release ID.'
Assert-Contains $release 'release\.draft -ne \$true' 'Draft cleanup must refuse to delete a release that has already been published.'
Assert-Contains $release 'gh api --method DELETE "repos/\$env:GITHUB_REPOSITORY/releases/\$env:RELEASE_ID"' 'Draft cleanup must delete only the release addressed by its captured ID.'
Assert-Contains $release 'draft: true' 'A release must remain unpublished until remote assets are verified.'
Assert-Contains $release 'verify-release-assets\.ps1' 'The uploaded draft assets must be downloaded and verified before publication.'
Assert-Contains $releaseVerifier 'gh release download' 'The Release verifier must download the actual draft assets from GitHub.'
Assert-Contains $releaseVerifier 'OpenShazam-update\.json\.sig' 'The Release verifier must require the detached manifest signature.'
Assert-Contains $releaseVerifier 'Get-AuthenticodeSignature' 'The Release verifier must validate the published installer Authenticode signature.'
Assert-Contains $releaseVerifier 'installer_size_bytes' 'The Release verifier must compare the signed installer size.'
Assert-Contains $releaseVerifier '\$updateTool verify' 'The Release verifier must validate Ed25519 and installer integrity from the downloaded assets.'
Assert-Contains $release 'gh release edit .*--draft=false' 'Only a verified draft release may be published.'
Assert-Contains $signing 'if \(\$RequireSignature\)' 'The signing helper must fail if a required PFX certificate is absent.'
Assert-Contains $release 'OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64' 'A stable release must require the Ed25519 manifest signing key.'
Assert-Contains $release 'prepare-release-update.ps1' 'Release processing must generate and verify the signed update manifest.'
Assert-Contains $release 'OpenShazam-update\.json\.sig' 'The detached Ed25519 signature must be published with the manifest.'
Assert-Contains $update 'verify_strict' 'Update manifests must use strict Ed25519 verification.'
Assert-Contains $update 'verify_installer_reader' 'Downloaded installers must be checked against the signed SHA-256 and length using the opened file handle.'
Assert-Contains $update 'LATEST_MANIFEST_URL' 'The updater must use the fixed HTTPS GitHub Releases latest manifest URL.'
Assert-True ($update.Contains('https://github.com/NcaMoq/OpenShazam/releases/latest/download/OpenShazam-update.json')) 'The updater must fetch the manifest from the formal release repository.'
Assert-True ($update.Contains('https://github.com/NcaMoq/OpenShazam/releases/latest/download/OpenShazam-update.json.sig')) 'The updater must fetch the signature from the formal release repository.'
Assert-True ($updateWorkflow.Contains("'NcaMoq/OpenShazam'")) 'The signed manifest must name the formal release repository.'
Assert-True ($installer.Contains('AppPublisherURL=https://github.com/NcaMoq/OpenShazam')) 'The installer publisher URL must name the formal release repository.'
Assert-Contains $updateUi 'begin_update_check\(&mut \*app_ptr, false\)' 'The updater must check the stable signed manifest at application startup.'
Assert-Contains $updateUi 'WM_UPDATE_ACTION if data\.kind == WindowKind::Main' 'Settings must be able to request a manual update check without blocking the UI thread.'
Assert-Contains $updateUi 'automatic_setting' 'Automatic-update preference changes must be visible to the installer worker.'
Assert-Contains $updateUi 'update_auto_install_gate\.lock' 'Changing the automatic-update preference must serialize with installer launch.'
Assert-Contains $updateUi 'sender\.send\(payload\)\?;\s*Ok\(wake\(\)\)' 'Update completions must enter the durable queue before the best-effort window-message wake.'
Assert-Contains $updateUi 'fn sync_update_completion_timer[\s\S]*?TIMER_UPDATE_COMPLETION,\s*UPDATE_COMPLETION_POLL_MS' 'The updater must poll its durable completion queue while an operation is active.'
Assert-Contains $updateUi 'UpdateWorkerCompletion::Check[\s\S]*UpdateWorkerCompletion::Download[\s\S]*UpdateWorkerCompletion::Install' 'Update check, download, and install workers must all use the durable completion queue.'
Assert-NotContains $updateUi 'post_update_payload|WM_UPDATE_(CHECK|DOWNLOAD|INSTALL)_COMPLETED' 'Update workers must not move completion ownership back to best-effort message payloads.'
Assert-Contains $windowProc 'WM_UPDATE_WORKER_COMPLETED[\s\S]*drain_update_worker_completions\(app\)' 'The update worker wake message must drain queued completions on the UI thread.'
Assert-Contains $windowProc 'TIMER_UPDATE_COMPLETION[\s\S]*drain_update_worker_completions\(app\)' 'The fallback timer must drain update completions when posting a window message fails.'
Assert-Contains $updateUi 'UpdateStatus::Updated' 'The next launch must display the result of an update after checking the installed version.'
Assert-Contains $updateUi 'UPDATE_CHECK_INTERVAL_MS: u32 = 24 \* 60 \* 60 \* 1000' 'The updater must schedule a daily background check.'
Assert-Contains $updateUi 'should_begin_update_check\(interactive, app\.update_auto_enabled\)' 'Background update checks must respect the automatic update setting.'
$updateCheckCompletion = [regex]::Match($updateUi, '(?s)pub\(super\) unsafe fn handle_update_check_completed\(.*?\n\}\n\npub\(super\) unsafe fn begin_update_download')
Assert-True $updateCheckCompletion.Success 'The update-check completion handler must remain available for policy verification.'
Assert-NotContains $updateCheckCompletion.Value 'show_update_message|MessageBoxW|MessageBox\(' 'Background update-check failures must not interrupt the user with a dialog.'
Assert-Contains $updateCheckCompletion.Value 'Ok\(Some\(candidate\)\)[\s\S]*?UpdateStatus::Available[\s\S]*?should_auto_install_verified_update\(interactive, app\.update_auto_enabled\)' 'Automatic downloads must begin only after a verified update candidate is available.'
Assert-Contains $updateUi 'background_update_checks_respect_the_automatic_setting_and_manual_checks_remain_available' 'The automatic-update setting must be covered by a behavior test that preserves manual checks.'
Assert-Contains $updateUi 'only_background_checks_with_automatic_updates_enabled_auto_install_verified_updates' 'Automatic installation policy must be covered for background, manual, enabled, and disabled cases.'
Assert-Contains $updateWindows 'automatic_install_should_be_cancelled' 'Turning automatic updates off must cancel a not-yet-started automatic install.'
Assert-Contains $updateWindows 'start_installer_process_if_enabled' 'The automatic-update toggle and installer process launch must share a synchronization gate.'
Assert-Contains $updateWindows 'run_download_command_cancellable' 'Closing OpenShazam must be able to stop a pending streaming HTTPS update process.'
Assert-NotContains $updateWindows '\.arg\("--location"\)|\.arg\("--proto-redir"\)' 'The updater must not let curl follow an unchecked redirect chain.'
Assert-Contains $updateWindows 'validate_redirect_target' 'The updater must validate each HTTPS redirect before issuing the next request.'
Assert-Contains $updateWindows 'MAX_REDIRECT_RESPONSE_BYTES' 'The updater must bound redirect response bodies.'
Assert-Contains $updateWindows 'child\.kill\(\)' 'Cancellation must terminate the HTTPS child process instead of leaving curl running.'
Assert-Contains $updateUi 'cancel_and_join_update_workers\(app\)' 'The application must cancel and join update workers before it exits.'
Assert-Contains $windowProc 'let selected = (?:unsafe \{\s*)?track_tray_context_menu\(hwnd\)(?:\s*\})?;[\s\S]*?let Some\(data_ptr\) = \(unsafe \{ window_data\(hwnd\) \}\) else' 'The tray menu must reacquire GWLP_USERDATA after its nested message loop.'
$captureReleasePreparation = [regex]::Match($windowProc, '(?s)unsafe fn prepare_main_button_capture_release\([^)]*\)\s*\{(?<body>.*?)\n\}')
Assert-True $captureReleasePreparation.Success 'The capture-release preparation must be isolated from ReleaseCapture reentrancy.'
Assert-Contains $captureReleasePreparation.Groups['body'].Value 'app\.mouse_down_center = false;[\s\S]*app\.long_press_consumed = false;[\s\S]*InvalidateRect\(Some\(hwnd\)' 'Mouse state and repaint must be prepared before capture is released.'
$captureReleaseStart = $windowProc.IndexOf('if message == WM_LBUTTONUP {')
$generalDispatchStart = $windowProc.IndexOf('let Some(data_ptr) = (unsafe { window_data(hwnd) }) else', $captureReleaseStart)
Assert-True ($captureReleaseStart -ge 0 -and $generalDispatchStart -gt $captureReleaseStart) 'Captured button-up handling must run before the general mutable WndProc borrow.'
$captureReleaseHandler = $windowProc.Substring($captureReleaseStart, $generalDispatchStart - $captureReleaseStart)
Assert-Contains $captureReleaseHandler 'prepare_main_button_capture_release\(hwnd, &mut \*app_ptr\);[\s\S]*if GetCapture\(\) == hwnd \{\s*let _ = ReleaseCapture\(\);\s*\}[\s\S]*return LRESULT\(0\);' 'The main-button state borrow must end before ReleaseCapture synchronously reenters the WndProc, and only this window''s capture may be released.'
$mouseDownStart = $captureReleaseHandler.IndexOf('if message == WM_LBUTTONDOWN {')
Assert-True ($mouseDownStart -ge 0) 'Main-button capture setup must run before the general mutable WndProc borrow.'
$mouseDownHandler = $captureReleaseHandler.Substring($mouseDownStart)
Assert-Contains $mouseDownHandler 'let should_capture = \{[\s\S]*app\.long_press_consumed = false;[\s\S]*\};[\s\S]*if should_capture \{\s*let _ = SetCapture\(hwnd\);' 'Button state must be committed before SetCapture can synchronously notify a previous capture window.'
Assert-NotContains $windowProc 'WM_LBUTTONDOWN if data.kind == WindowKind::Main =>' 'The main-button capture path must not run under the general mutable WndProc borrow.'
$paintHandler = [regex]::Match($windowProc, '(?s)unsafe fn paint_window\(hwnd: HWND\)\s*-> LRESULT\s*\{(?<body>.*?)\n\}')
Assert-True $paintHandler.Success 'WM_PAINT must have a dedicated handler for synchronous GDI reentrancy.'
$paintBody = $paintHandler.Groups['body'].Value
$beginPaintPosition = $paintBody.IndexOf('BeginPaint(hwnd, &mut paint)')
$appBorrowPosition = $paintBody.IndexOf('let app = unsafe { &mut *data.app }')
$endPaintPosition = $paintBody.IndexOf('EndPaint(hwnd, &paint)')
Assert-True ($beginPaintPosition -ge 0 -and $appBorrowPosition -gt $beginPaintPosition -and $endPaintPosition -gt $appBorrowPosition) 'BeginPaint must complete its synchronous message dispatch before AppState is mutably borrowed, and EndPaint must run after that borrow ends.'
$wndProcStart = $windowProc.IndexOf('pub(super) unsafe extern "system" fn window_proc(')
$paintDispatchPosition = $windowProc.IndexOf('if message == WM_PAINT {', $wndProcStart)
$generalPaintBorrowPosition = $windowProc.IndexOf('let Some(data_ptr) = (unsafe { window_data(hwnd) }) else {', $paintDispatchPosition)
Assert-True ($wndProcStart -ge 0 -and $paintDispatchPosition -gt $wndProcStart -and $generalPaintBorrowPosition -gt $paintDispatchPosition) 'WM_PAINT must dispatch before the general WndProc mutable borrow.'
$leftButtonUpStart = $windowProc.IndexOf('WM_LBUTTONUP if data.kind == WindowKind::Main =>')
$leftButtonUpEnd = $windowProc.IndexOf('WM_TIMER if data.kind', $leftButtonUpStart)
Assert-True ($leftButtonUpStart -ge 0 -and $leftButtonUpEnd -gt $leftButtonUpStart) 'The normal main-window button-up handler must remain available.'
$leftButtonUpHandler = $windowProc.Substring($leftButtonUpStart, $leftButtonUpEnd - $leftButtonUpStart)
Assert-NotContains $leftButtonUpHandler 'ReleaseCapture\(' 'The general mutable WndProc borrow must not span ReleaseCapture reentrancy.'
$generalBorrowStart = $windowProc.IndexOf('let result = unsafe {', $generalDispatchStart)
$deferredDefaultStart = $windowProc.IndexOf('if call_default_proc {', $generalBorrowStart)
Assert-True ($generalBorrowStart -ge 0 -and $deferredDefaultStart -gt $generalBorrowStart) 'Default window processing must run after the custom-dispatch borrow scope.'
$borrowedDispatch = $windowProc.Substring($generalBorrowStart, $deferredDefaultStart - $generalBorrowStart)
Assert-NotContains $borrowedDispatch 'DefWindowProcW\(' 'Default window processing must not run while WindowData/AppState mutable references are in scope.'
Assert-Contains $windowProc 'call_default_proc = true;[\s\S]*if call_default_proc \{[\s\S]*DefWindowProcW\(hwnd, message, wparam, lparam\)' 'Unhandled messages must defer DefWindowProcW until after the mutable dispatch block returns.'
Assert-Contains $windowProc 'openshazam-controller-shutdown[\s\S]*controller\.join\(\)[\s\S]*commands\.request_shutdown\(\)' 'WndProc teardown must use a fallible named thread for controller shutdown and request shutdown if thread creation fails.'
Assert-NotContains $windowProc 'thread::spawn\(move \|\|' 'WndProc teardown must not use the panicking thread::spawn shortcut.'
Assert-Contains $windowProc 'fn dpi_suggested_bounds\(lparam: LPARAM\)[\s\S]*if lparam\.0 == 0[\s\S]*checked_sub[\s\S]*MAX_DPI_SUGGESTED_WINDOW_DIMENSION' 'The main window must validate a DPI-change rectangle before dereferencing or applying it.'
Assert-Contains $windowProc 'if message == WM_DPICHANGED \{\s*let Some\(\(suggested, width, height\)\) = dpi_suggested_bounds\(lparam\) else' 'The main window must ignore invalid DPI-change messages before updating its renderer.'
Assert-Contains $windowProc 'dpi_change_requires_a_non_null_rectangle|dpi_change_rejects_inverted_overflowing_and_unbounded_bounds' 'DPI rectangle validation must include null, overflow, and unreasonable-size cases in Rust tests.'
Assert-Contains $startupHost 'kMaxDpiSuggestedWindowDimension = 32768' 'The WinUI Settings host must bound DPI suggested dimensions.'
Assert-Contains $startupHost 'static_cast<LONGLONG>\(rect->right\) - rect->left' 'The WinUI Settings host must use overflow-safe DPI rectangle arithmetic.'
Assert-Contains $settingsHostTest 'oversizedBoundsIgnored=true' 'The WinUI Settings host integration test must verify oversized DPI rectangles are ignored.'
Assert-Contains $updateWindows 'consume_pending_update_result' 'Update success must be based on the installed version after relaunch.'
Assert-Contains $updateWindows 'cleanup_obsolete_update_artifacts' 'The updater must clean only obsolete app-owned artifacts.'
Assert-Contains $updateWindows 'SetFileInformationByHandle' 'Update cleanup must delete the same file handle it inspected.'
Assert-Contains $updateWindows 'FILE_FLAG_OPEN_REPARSE_POINT' 'Update cleanup and installer verification must inspect reparse points without following them.'
Assert-Contains $updateWindows 'lock_directory_chain' 'Update cleanup must pin every directory component against path replacement.'
Assert-Contains $updateWindows 'let mut installer_file = open_guarded_read_file\(path\)\?[\s\S]*verify_installer_reader\(&manifest, &mut installer_file\)[\s\S]*Command::new\(path\)' 'The exact file handle verified against the signed installer digest must remain open through process launch.'
Assert-Contains $updateWindows 'is_download_directory_name\(&name\)' 'Update cleanup must delete only directories that match the generated download-folder name.'
Assert-Contains $updateWindows 'is_generated_temp_file_name\(&name\)' 'Update cleanup must delete only temporary files matching the generated filename format.'
Assert-Contains $updateWindows 'update_directory_at' 'Update files must stay beneath the validated Local AppData product directory.'
Assert-Contains $updateWindows 'shazam_http_lab::system_curl_program\(\)' 'Update checks must start the HTTPS client from the Windows system directory rather than the working directory or PATH.'
Assert-Contains $updateWindows '\.arg\("--disable"\)' 'Update requests must ignore per-user curl configuration that could weaken transport checks.'
Assert-Contains $artworkNetwork 'shazam_http_lab::system_curl_program\(\)' 'Artwork downloads must start curl from the Windows system directory rather than the working directory or PATH.'
Assert-NotContains $artworkNetwork 'Command::new\("curl\.exe"\)' 'Artwork downloads must not search the working directory or PATH for curl.'
Assert-Contains $httpClient 'GetSystemDirectoryW' 'Windows network clients must resolve system executables from the protected system directory.'
Assert-Contains $httpClient 'system_powershell_program\(\)' 'Shazam configuration collection must resolve Windows PowerShell from the protected system directory.'
Assert-Contains $updateWorkflow 'verify \$manifest \$signature \$installer' 'Release generation must verify the output manifest and installer before publication.'
Assert-NotContains $updateWorkflow 'cargo \+1\.85\.0 run' 'The Ed25519 key must not be inherited by Cargo or Rust build processes.'
$updateSigningKeyClear = $updateWorkflow.IndexOf('[Environment]::SetEnvironmentVariable($signingKeyEnvironmentName, $null, ''Process'')')
$updateToolBuild = $updateWorkflow.IndexOf('& cargo +1.85.0 build')
$updateSigningKeyGrant = $updateWorkflow.IndexOf('[Environment]::SetEnvironmentVariable($signingKeyEnvironmentName, $encodedSigningKey, ''Process'')')
$updateManifestSign = $updateWorkflow.IndexOf('& $updateTool manifest')
$updateManifestKeyClear = $updateWorkflow.IndexOf('[Environment]::SetEnvironmentVariable($signingKeyEnvironmentName, $null, ''Process'')', $updateManifestSign)
$updateManifestVerify = $updateWorkflow.IndexOf('& $updateTool verify')
Assert-True ($updateSigningKeyClear -ge 0 -and $updateToolBuild -gt $updateSigningKeyClear -and $updateSigningKeyGrant -gt $updateToolBuild -and $updateManifestSign -gt $updateSigningKeyGrant -and $updateManifestKeyClear -gt $updateManifestSign -and $updateManifestVerify -gt $updateManifestKeyClear) 'The Ed25519 key must be absent during compilation and verification, and present only for manifest signing.'
$updateWorkflowFinally = [regex]::Match($updateWorkflow, '(?s)finally\s*\{(?<body>[^{}]*\$encodedSigningKey\s*=\s*\$null[^{}]*)\}')
Assert-True $updateWorkflowFinally.Success 'The Ed25519 key must be cleared when release manifest preparation exits.'
Assert-Contains $updateWorkflowFinally.Groups['body'].Value '\[Environment\]::SetEnvironmentVariable\(\$signingKeyEnvironmentName,\s*\$null,\s*''Process''\)' 'Release manifest preparation must leave the Ed25519 key absent from its process environment.'
Assert-NotContains $updateWorkflowFinally.Groups['body'].Value '\$encodedSigningKey,\s*''Process''' 'Release manifest preparation must not restore the Ed25519 key after signing.'
Assert-Contains $signing 'Code Signing enhanced key usage' 'The signing helper must validate a code-signing certificate.'
Assert-Contains $signing 'X509BasicConstraintsExtension' 'The signing helper must reject CA certificates used as release signing leaves.'
Assert-Contains $signing 'must include a Basic Constraints extension' 'The signing helper must require an explicit leaf-certificate constraint.'
Assert-Contains $signing 'X509KeyUsageExtension' 'The signing helper must validate the certificate key usage.'
Assert-Contains $signing 'must permit digital signatures' 'The signing helper must require digital-signature key usage.'
Assert-Contains $signing 'signtool verify /pa /all /v' 'The signing helper must verify Authenticode signatures.'
$certificateClearPosition = $signing.IndexOf("[Environment]::SetEnvironmentVariable('OPEN_SHAZAM_SIGNING_CERTIFICATE_BASE64'")
$passwordClearPosition = $signing.IndexOf("[Environment]::SetEnvironmentVariable('OPEN_SHAZAM_SIGNING_CERTIFICATE_PASSWORD'")
$signToolSignPosition = $signing.IndexOf('& $signtool sign')
Assert-True ($certificateClearPosition -ge 0 -and $passwordClearPosition -ge 0 -and $signToolSignPosition -gt $certificateClearPosition -and $signToolSignPosition -gt $passwordClearPosition) 'The signing helper must clear both PFX credentials from the process environment before starting signtool.'
Assert-Contains $gitignore '(?m)^\*\.pfx$' 'Local PFX signing certificates must not be added to Git by accident.'
Assert-Contains $gitignore '(?m)^\*\.p12$' 'Local PKCS#12 signing certificates must not be added to Git by accident.'
Assert-Contains $gitignore '(?m)^\*\.key$' 'Local private keys must not be added to Git by accident.'
Assert-Contains $gitignore '(?m)^\*\.p8$' 'Local PKCS#8 private keys must not be added to Git by accident.'
Assert-Contains $gitignore '(?m)^\*\.pem$' 'Local PEM-encoded keys and certificates must not be added to Git by accident.'
Assert-Contains $signing 'Remove-Item -LiteralPath \$pfxPath -Force' 'The signing helper must remove its temporary PFX file.'
Assert-Contains $signingTest 'Invoke-SigningFailureCheck ''missing certificate''' 'The signing contract test must exercise the missing-certificate failure path.'
Assert-Contains $signingTest 'password without certificate' 'The signing contract test must reject a password without its PFX.'
Assert-Contains $signingTest 'left a temporary PFX behind' 'The signing contract test must verify temporary-PFX cleanup after load failure.'
Assert-Contains $signingTest 'CA certificate' 'The signing contract test must reject a CA certificate as an Authenticode signer.'
Assert-Contains $signingTest 'certificate without code-signing EKU' 'The signing contract test must reject certificates without Code Signing EKU.'
Assert-Contains $signingTest 'expired certificate' 'The signing contract test must reject expired signing certificates.'
Assert-Contains $signingTest 'certificate without Basic Constraints' 'The signing contract test must reject certificates without a leaf constraint.'
Assert-Contains $signingTest 'certificate without digital-signature usage' 'The signing contract test must reject certificates that cannot sign code.'
Assert-Contains $signingTest 'exposed a credential value in its output' 'The signing contract test must detect accidental credential output.'

foreach ($path in @(
    'apps/openshazam/Package.appxmanifest',
    '.github/workflows/release-windows-msix.yml',
    'tools/package-windows-msix.ps1',
    'tools/verify-msix-feed.ps1'
)) {
    Assert-True (-not (Test-Path -LiteralPath (Join-Path $root $path))) "Retired MSIX artifact remains active: $path"
}

Write-Host 'Windows installer, startup, history, and signed update release contract passed.'
