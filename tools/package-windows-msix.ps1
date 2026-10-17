param(
    [string]$Version = '0.1.0.0',
    [string]$SigningCertificateThumbprint,
    [string]$Publisher,
    [string]$PackageUri,
    [string]$AppInstallerUri,
    [ValidateRange(0, 255)]
    [int]$HoursBetweenUpdateChecks = 12
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$distRoot = [IO.Path]::GetFullPath((Join-Path $root 'dist\windows-x64')).TrimEnd('\')
$targetRoot = Join-Path $root 'target\windows-msix'
$staging = Join-Path $targetRoot "package-$Version"
$validation = Join-Path $targetRoot "unpacked-$Version"
$packagePath = Join-Path $targetRoot "OpenShazam-$Version-x64.msix"

if ($Version -notmatch '^\d+\.\d+\.\d+\.\d+$') {
    throw "MSIX version must have four numeric components: $Version"
}

function Remove-SafeTargetDirectory([string]$path) {
    $fullPath = [IO.Path]::GetFullPath($path)
    $targetPrefix = $targetRoot.TrimEnd([IO.Path]::DirectorySeparatorChar) + [IO.Path]::DirectorySeparatorChar
    if (-not $fullPath.StartsWith($targetPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Refusing to clear a path outside $targetRoot`: $fullPath"
    }
    if (Test-Path -LiteralPath $fullPath) {
        $resolved = (Resolve-Path -LiteralPath $fullPath -ErrorAction Stop).Path
        if (-not $resolved.StartsWith($targetPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Resolved build path escaped $targetRoot`: $resolved"
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}

New-Item -ItemType Directory -Path $targetRoot -Force | Out-Null
Remove-SafeTargetDirectory $staging
Remove-SafeTargetDirectory $validation

$buildScript = Join-Path $PSScriptRoot 'build-windows.ps1'
& $buildScript -Product -OutputDirectory $distRoot
if ($LASTEXITCODE -ne 0) { throw 'Windows product build failed.' }

New-Item -ItemType Directory -Path $staging -Force | Out-Null
$runtimeFiles = @(
    'openshazam.exe',
    'OpenShazam.SettingsHost.dll',
    'Microsoft.WindowsAppRuntime.Bootstrap.dll'
)
foreach ($name in $runtimeFiles) {
    $runtimeFile = Join-Path $distRoot $name
    if (-not (Test-Path -LiteralPath $runtimeFile -PathType Leaf)) {
        throw "Missing runtime file from product build: $runtimeFile"
    }
    Copy-Item -LiteralPath $runtimeFile -Destination $staging -Force
}
Get-ChildItem -LiteralPath $distRoot -File -Filter '*.pri' | ForEach-Object {
    Copy-Item -LiteralPath $_.FullName -Destination $staging -Force
}
$assetStage = Join-Path $staging 'assets'
New-Item -ItemType Directory -Path $assetStage -Force | Out-Null
foreach ($name in @('OpenShazamSquare44.png', 'OpenShazamSquare150.png', 'OpenShazamStoreLogo.png')) {
    $asset = Join-Path $root "apps\openshazam\assets\$name"
    if (-not (Test-Path -LiteralPath $asset -PathType Leaf)) { throw "Missing package logo: $asset" }
    Copy-Item -LiteralPath $asset -Destination $assetStage -Force
}

$manifestSource = Join-Path $root 'apps\openshazam\Package.appxmanifest'
[xml]$manifest = Get-Content -LiteralPath $manifestSource -Raw
$identity = $manifest.SelectSingleNode("/*[local-name()='Package']/*[local-name()='Identity']")
if (-not $identity) { throw 'Package manifest has no Identity element.' }
$identity.SetAttribute('Version', $Version)
if (-not [string]::IsNullOrWhiteSpace($Publisher)) {
    $identity.SetAttribute('Publisher', $Publisher)
}
$startupTask = $manifest.SelectSingleNode("//*[local-name()='StartupTask' and @TaskId='OpenShazamStartup']")
if (-not $startupTask -or $startupTask.GetAttribute('Enabled') -ne 'false') {
    throw 'Package manifest must declare OpenShazamStartup disabled by default.'
}
$manifest.Save((Join-Path $staging 'AppxManifest.xml'))

$sdkBinRoot = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
$kitDirs = Get-ChildItem -LiteralPath $sdkBinRoot -Directory | Where-Object { $_.Name -match '^10\.\d+\.\d+\.\d+$' }
$makeAppx = $null
$signTool = $null
foreach ($kit in ($kitDirs | Sort-Object { [version]$_.Name } -Descending)) {
    $makeCandidate = Join-Path $kit.FullName 'x64\makeappx.exe'
    $signCandidate = Join-Path $kit.FullName 'x64\signtool.exe'
    if (-not $makeAppx -and (Test-Path -LiteralPath $makeCandidate)) { $makeAppx = $makeCandidate }
    if (-not $signTool -and (Test-Path -LiteralPath $signCandidate)) { $signTool = $signCandidate }
}
if (-not $makeAppx) { throw 'Windows SDK MakeAppx x64 was not found.' }

& $makeAppx pack /d $staging /p $packagePath /o
if ($LASTEXITCODE -ne 0) { throw 'MakeAppx failed to create the MSIX package.' }
& $makeAppx unpack /p $packagePath /d $validation
if ($LASTEXITCODE -ne 0) { throw 'MakeAppx could not unpack the generated MSIX package.' }
$unpackedManifest = Join-Path $validation 'AppxManifest.xml'
if (-not (Test-Path -LiteralPath (Join-Path $validation 'openshazam.exe'))) {
    throw 'The generated package does not contain the product executable.'
}
[xml]$unpackedXml = Get-Content -LiteralPath $unpackedManifest -Raw
$unpackedStartupTask = $unpackedXml.SelectSingleNode("//*[local-name()='StartupTask' and @TaskId='OpenShazamStartup']")
if (-not $unpackedStartupTask -or $unpackedStartupTask.GetAttribute('Enabled') -ne 'false') {
    throw 'The packaged startup task is missing or is not disabled by default.'
}

if (-not [string]::IsNullOrWhiteSpace($SigningCertificateThumbprint)) {
    if (-not $signTool) { throw 'Windows SDK SignTool x64 was not found.' }
    $certificate = Get-ChildItem Cert:\CurrentUser\My, Cert:\LocalMachine\My -ErrorAction SilentlyContinue |
        Where-Object { $_.Thumbprint -ieq $SigningCertificateThumbprint -and $_.HasPrivateKey } |
        Select-Object -First 1
    if (-not $certificate) { throw "Signing certificate with a private key was not found: $SigningCertificateThumbprint" }
    if ($certificate.Subject -cne $identity.GetAttribute('Publisher')) {
        throw "Signing certificate subject '$($certificate.Subject)' does not match MSIX Publisher '$($identity.GetAttribute('Publisher'))'."
    }
    & $signTool sign /fd SHA256 /sha1 $certificate.Thumbprint $packagePath
    if ($LASTEXITCODE -ne 0) { throw 'SignTool failed to sign the MSIX package.' }
    & $signTool verify /pa /v $packagePath
    if ($LASTEXITCODE -ne 0) { throw 'SignTool could not verify the signed MSIX package.' }
}

$hasPackageUri = -not [string]::IsNullOrWhiteSpace($PackageUri)
$hasAppInstallerUri = -not [string]::IsNullOrWhiteSpace($AppInstallerUri)
if ($hasPackageUri -xor $hasAppInstallerUri) {
    throw 'PackageUri and AppInstallerUri must be supplied together.'
}
if ($hasPackageUri) {
    if ([string]::IsNullOrWhiteSpace($SigningCertificateThumbprint)) {
        throw 'An App Installer update feed requires a signed MSIX; no certificate was supplied.'
    }
    foreach ($uriText in @($PackageUri, $AppInstallerUri)) {
        try { $parsedUri = [Uri]$uriText } catch { throw "Update feed URI is invalid: $uriText" }
        if (-not $parsedUri.IsAbsoluteUri -or $parsedUri.Scheme -ne 'https') {
            throw "Update feed URI must be an absolute HTTPS URL: $uriText"
        }
    }
    $appInstallerPath = Join-Path $targetRoot "OpenShazam-$Version.appinstaller"
    $appInstallerNamespace = 'http://schemas.microsoft.com/appx/appinstaller/2021'
    $appInstallerXml = [Xml.XmlDocument]::new()
    $null = $appInstallerXml.AppendChild($appInstallerXml.CreateXmlDeclaration('1.0', 'utf-8', $null))
    $appInstaller = $appInstallerXml.CreateElement('AppInstaller', $appInstallerNamespace)
    $appInstaller.SetAttribute('Version', $Version)
    $appInstaller.SetAttribute('Uri', $AppInstallerUri)
    $null = $appInstallerXml.AppendChild($appInstaller)

    $mainPackage = $appInstallerXml.CreateElement('MainPackage', $appInstallerNamespace)
    $mainPackage.SetAttribute('Name', $identity.GetAttribute('Name'))
    $mainPackage.SetAttribute('Publisher', $identity.GetAttribute('Publisher'))
    $mainPackage.SetAttribute('Version', $Version)
    $mainPackage.SetAttribute('ProcessorArchitecture', 'x64')
    $mainPackage.SetAttribute('Uri', $PackageUri)
    $null = $appInstaller.AppendChild($mainPackage)

    $updateSettings = $appInstallerXml.CreateElement('UpdateSettings', $appInstallerNamespace)
    $onLaunch = $appInstallerXml.CreateElement('OnLaunch', $appInstallerNamespace)
    $onLaunch.SetAttribute('HoursBetweenUpdateChecks', $HoursBetweenUpdateChecks.ToString([Globalization.CultureInfo]::InvariantCulture))
    $onLaunch.SetAttribute('ShowPrompt', 'false')
    $onLaunch.SetAttribute('UpdateBlocksActivation', 'false')
    $null = $updateSettings.AppendChild($onLaunch)
    $null = $appInstaller.AppendChild($updateSettings)
    $appInstallerXml.Save($appInstallerPath)
}

$hash = (Get-FileHash -LiteralPath $packagePath -Algorithm SHA256).Hash.ToLowerInvariant()
Set-Content -LiteralPath "$packagePath.sha256" -Value "$hash  $(Split-Path -Leaf $packagePath)" -Encoding ascii

Get-Item -LiteralPath $packagePath | Select-Object FullName, Length, LastWriteTimeUtc
Write-Host 'MSIX package and extracted payload validated. StartupTask is disabled by default.'
Write-Host 'Package entry point launches the tray UI without arguments; development CLI features are excluded.'
Write-Host "Package and validation output: $targetRoot"
if (-not $SigningCertificateThumbprint) {
    Write-Host 'Package is unsigned and for local package validation only; Windows installation and update are not validated.'
}
