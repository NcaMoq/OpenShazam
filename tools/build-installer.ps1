[CmdletBinding()]
param(
    [string]$Version,
    [string]$ISCCPath
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$cargoPath = Join-Path $root 'Cargo.toml'
$cargoText = Get-Content -LiteralPath $cargoPath -Raw
$workspaceSection = [regex]::Match(
    $cargoText,
    '(?ms)^\[workspace\.package\]\s*(?<body>.*?)(?=^\[|\z)'
)
if (-not $workspaceSection.Success) {
    throw 'Cargo.toml is missing [workspace.package].'
}

$manifestVersion = [regex]::Match(
    $workspaceSection.Groups['body'].Value,
    '(?m)^version\s*=\s*"(?<value>[^"\r\n]+)"'
)
if (-not $manifestVersion.Success) {
    throw 'Cargo workspace version is missing.'
}

$workspaceVersion = $manifestVersion.Groups['value'].Value
if ($workspaceVersion -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$') {
    throw "Cargo workspace version must be a stable MAJOR.MINOR.PATCH version; found '$workspaceVersion'."
}
if (-not [string]::IsNullOrWhiteSpace($Version) -and $Version -cne $workspaceVersion) {
    throw "Requested installer version '$Version' does not match Cargo workspace version '$workspaceVersion'."
}

$distDirectory = Join-Path $root 'dist\windows-x64'
$requiredInputs = @(
    'openshazam.exe',
    'OpenShazam.SettingsHost.dll',
    'THIRD-PARTY-NOTICES.txt',
    'Microsoft.WindowsAppRuntime.Bootstrap.dll',
    'OpenShazamSquare44.png',
    'msvcp140.dll',
    'vcruntime140.dll',
    'vcruntime140_1.dll'
)
foreach ($name in $requiredInputs) {
    $path = Join-Path $distDirectory $name
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Required application build output is missing: $path. Run tools/build-windows.ps1 -Product first."
    }
}

$runtimeInstaller = Join-Path $root 'target\windows-app-sdk-runtime\WindowsAppRuntimeInstall-x64.exe'
if (-not (Test-Path -LiteralPath $runtimeInstaller -PathType Leaf)) {
    throw "Windows App SDK runtime installer is missing: $runtimeInstaller. Run tools/prepare-windows-app-sdk-runtime.ps1 first."
}

if ([string]::IsNullOrWhiteSpace($ISCCPath)) {
    $configuredISCC = [Environment]::GetEnvironmentVariable('INNO_SETUP_ISCC', 'Process')
    if (-not [string]::IsNullOrWhiteSpace($configuredISCC)) {
        $ISCCPath = $configuredISCC
    }
}
if ([string]::IsNullOrWhiteSpace($ISCCPath)) {
    $programFilesX86 = [Environment]::GetEnvironmentVariable('ProgramFiles(x86)', 'Process')
    if (-not [string]::IsNullOrWhiteSpace($programFilesX86)) {
        $standardISCC = Join-Path $programFilesX86 'Inno Setup 6\ISCC.exe'
        if (Test-Path -LiteralPath $standardISCC -PathType Leaf) {
            $ISCCPath = $standardISCC
        }
    }
}
if ([string]::IsNullOrWhiteSpace($ISCCPath)) {
    $programFiles = [Environment]::GetEnvironmentVariable('ProgramFiles', 'Process')
    if (-not [string]::IsNullOrWhiteSpace($programFiles)) {
        $standard64BitISCC = Join-Path $programFiles 'Inno Setup 6\ISCC.exe'
        if (Test-Path -LiteralPath $standard64BitISCC -PathType Leaf) {
            $ISCCPath = $standard64BitISCC
        }
    }
}
if ([string]::IsNullOrWhiteSpace($ISCCPath)) {
    $localAppData = [Environment]::GetEnvironmentVariable('LOCALAPPDATA', 'Process')
    if (-not [string]::IsNullOrWhiteSpace($localAppData)) {
        $userScopeISCC = Join-Path $localAppData 'Programs\Inno Setup 6\ISCC.exe'
        if (Test-Path -LiteralPath $userScopeISCC -PathType Leaf) {
            $ISCCPath = $userScopeISCC
        }
    }
}
if ([string]::IsNullOrWhiteSpace($ISCCPath)) {
    $isccCommand = Get-Command 'ISCC.exe' -ErrorAction SilentlyContinue
    if ($isccCommand) {
        $ISCCPath = $isccCommand.Source
    }
}
if ([string]::IsNullOrWhiteSpace($ISCCPath) -or -not (Test-Path -LiteralPath $ISCCPath -PathType Leaf)) {
    throw 'Inno Setup 6 compiler was not found. Install Inno Setup 6 or pass -ISCCPath / set INNO_SETUP_ISCC.'
}

New-Item -ItemType Directory -Path $distDirectory -Force | Out-Null
$installerPath = Join-Path $distDirectory "Shazam-Setup-$workspaceVersion-x64.exe"
$previousVersion = [Environment]::GetEnvironmentVariable('OPEN_SHAZAM_VERSION', 'Process')
$locationPushed = $false
try {
    [Environment]::SetEnvironmentVariable('OPEN_SHAZAM_VERSION', $workspaceVersion, 'Process')
    Push-Location -LiteralPath $root
    $locationPushed = $true
    & $ISCCPath '/Qp' 'installer\OpenShazam.iss'
    if ($LASTEXITCODE -ne 0) {
        throw "Inno Setup compiler failed with exit code $LASTEXITCODE."
    }
    if (-not (Test-Path -LiteralPath $installerPath -PathType Leaf)) {
        throw "Inno Setup did not produce the expected versioned installer: $installerPath"
    }

    $installer = Get-Item -LiteralPath $installerPath
    if ($installer.Length -le 0) {
        throw "Inno Setup produced an empty installer: $installerPath"
    }
    $hash = (Get-FileHash -LiteralPath $installerPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $sidecarPath = "$installerPath.sha256"
    $sidecar = "$hash *$(Split-Path -Leaf $installerPath)`n"
    [IO.File]::WriteAllText($sidecarPath, $sidecar, [Text.UTF8Encoding]::new($false))

    Write-Output "Installer: $installerPath"
    Write-Output "SHA-256: $hash"
    Write-Output "Checksum: $sidecarPath"
}
finally {
    if ($locationPushed) {
        Pop-Location
    }
    [Environment]::SetEnvironmentVariable('OPEN_SHAZAM_VERSION', $previousVersion, 'Process')
}
