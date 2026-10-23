param(
    [Parameter(Mandatory = $true)]
    [string]$Version,
    [string]$MinimumSupportedVersion = '0.1.0',
    [switch]$UseValidatedUpdateTool
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root 'dist\windows-x64'
$installer = Join-Path $dist "Shazam-Setup-$Version-x64.exe"
$manifest = Join-Path $dist 'OpenShazam-update.json'
$signature = "$manifest.sig"

if ($Version -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$') {
    throw "Update version must be stable MAJOR.MINOR.PATCH: $Version"
}
if ($MinimumSupportedVersion -notmatch '^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$') {
    throw "Minimum supported version must be stable MAJOR.MINOR.PATCH: $MinimumSupportedVersion"
}
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
    throw "The versioned installer is missing: $installer"
}
if ([string]::IsNullOrWhiteSpace($env:OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64)) {
    throw 'OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64 is required to sign a release manifest.'
}
$encodedSigningKey = $env:OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64
$signingKeyEnvironmentName = 'OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64'
$updateTool = Join-Path $root 'target\release\openshazam-update-tool.exe'

Push-Location $root
try {
    if ($UseValidatedUpdateTool) {
        if (-not (Test-Path -LiteralPath $updateTool -PathType Leaf)) {
            throw 'The Ed25519 update tool from the validated Windows CI artifact is missing.'
        }
    }
    else {
        [Environment]::SetEnvironmentVariable($signingKeyEnvironmentName, $null, 'Process')
        & cargo +1.85.0 build --locked --release -p openshazam-update --bin openshazam-update-tool
        if ($LASTEXITCODE -ne 0) { throw "Ed25519 manifest tool build failed with exit code $LASTEXITCODE." }
    }
    if (-not (Test-Path -LiteralPath $updateTool -PathType Leaf)) {
        throw 'The Ed25519 update manifest tool is unavailable.'
    }

    try {
        [Environment]::SetEnvironmentVariable($signingKeyEnvironmentName, $encodedSigningKey, 'Process')
        & $updateTool manifest $installer $Version $MinimumSupportedVersion 'NcaMoq/OpenShazam' $manifest $signature
    }
    finally {
        [Environment]::SetEnvironmentVariable($signingKeyEnvironmentName, $null, 'Process')
    }
    if ($LASTEXITCODE -ne 0) { throw "Ed25519 manifest generation failed with exit code $LASTEXITCODE." }

    & $updateTool verify $manifest $signature $installer
    if ($LASTEXITCODE -ne 0) { throw "Signed manifest verification failed with exit code $LASTEXITCODE." }

    $hash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText("$installer.sha256", "$hash *$(Split-Path -Leaf $installer)`n", [Text.UTF8Encoding]::new($false))
}
finally {
    try {
        Pop-Location
    }
    finally {
        [Environment]::SetEnvironmentVariable($signingKeyEnvironmentName, $null, 'Process')
        $encodedSigningKey = $null
    }
}

Write-Host "Release update metadata written to $dist"
