param(
    [Parameter(Mandatory = $true)]
    [string]$Version,
    [string]$Repository = 'NcaMoq/OpenShazam',
    [string]$MinimumSupportedVersion = '0.1.0'
)

$ErrorActionPreference = 'Stop'
$tag = "v$Version"
$repositoryRoot = Split-Path -Parent $PSScriptRoot
if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP) -or
    -not (Test-Path -LiteralPath $env:RUNNER_TEMP -PathType Container)) {
    throw 'RUNNER_TEMP is unavailable for release asset verification.'
}
$runnerTempDirectory = [IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd([IO.Path]::DirectorySeparatorChar)
$verificationDirectory = [IO.Path]::GetFullPath((Join-Path $runnerTempDirectory ("openshazam-release-verify-{0}" -f [guid]::NewGuid().ToString('N'))))
$expectedNames = @(
    "Shazam-Setup-$Version-x64.exe",
    "Shazam-Setup-$Version-x64.exe.sha256",
    'OpenShazam-update.json',
    'OpenShazam-update.json.sig'
)

if ([string]::IsNullOrWhiteSpace($env:GH_TOKEN)) {
    throw 'GH_TOKEN is required to verify uploaded draft release assets.'
}
$runnerTempPrefix = $runnerTempDirectory + [IO.Path]::DirectorySeparatorChar
if (-not $verificationDirectory.StartsWith($runnerTempPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The release verification directory is outside RUNNER_TEMP.'
}
New-Item -ItemType Directory -Path $verificationDirectory -Force | Out-Null

try {
    gh release download $tag --repo $Repository --dir $verificationDirectory
    if ($LASTEXITCODE -ne 0) { throw 'Could not download the uploaded draft release assets.' }

    $actualNames = @(Get-ChildItem -LiteralPath $verificationDirectory -File |
        ForEach-Object { $_.Name } | Sort-Object)
    $sortedExpectedNames = @($expectedNames | Sort-Object)
    if (Compare-Object -ReferenceObject $sortedExpectedNames -DifferenceObject $actualNames) {
        throw "Draft release assets do not match the expected four-file set: $($actualNames -join ', ')"
    }

    $releaseResponse = gh api "repos/$Repository/releases/tags/$tag"
    if ($LASTEXITCODE -ne 0) { throw 'Could not inspect the uploaded draft release.' }
    $release = $releaseResponse | ConvertFrom-Json
    if ($release.tag_name -cne $tag -or -not $release.draft -or $release.prerelease) {
        throw 'The uploaded release is not the expected unpublished draft.'
    }

    $installerName = "Shazam-Setup-$Version-x64.exe"
    $installerPath = Join-Path $verificationDirectory $installerName
    $manifestPath = Join-Path $verificationDirectory 'OpenShazam-update.json'
    $signaturePath = Join-Path $verificationDirectory 'OpenShazam-update.json.sig'
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    $actualSize = (Get-Item -LiteralPath $installerPath).Length
    $actualHash = & (Join-Path $PSScriptRoot 'verify-sha256-sidecar.ps1') -Path $installerPath
    if ($manifest.installer_sha256 -cne $actualHash) { throw 'The uploaded manifest hash does not match the installer.' }
    if ([uint64]$manifest.installer_size_bytes -ne [uint64]$actualSize) {
        throw 'The uploaded manifest size does not match the installer.'
    }

    $expectedReleaseUrl = "https://github.com/$Repository/releases/tag/$tag"
    if ($manifest.schema_version -ne 1 -or
        $manifest.product -cne 'OpenShazam' -or
        $manifest.repository -cne $Repository -or
        $manifest.version -cne $Version -or
        $manifest.minimum_supported_version -cne $MinimumSupportedVersion -or
        $manifest.architecture -cne 'x64' -or
        $manifest.release_tag -cne $tag -or
        $manifest.release_url -cne $expectedReleaseUrl -or
        $manifest.installer_name -cne $installerName) {
        throw 'The uploaded update manifest does not match the release contract.'
    }

    $signature = Get-AuthenticodeSignature -LiteralPath $installerPath
    if ($signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or
        -not $signature.SignerCertificate) {
        throw 'The uploaded installer does not have a valid Authenticode signature.'
    }

    $updateTool = Join-Path $repositoryRoot 'target\release\openshazam-update-tool.exe'
    if (-not (Test-Path -LiteralPath $updateTool -PathType Leaf)) {
        throw 'The signed update verifier was not built before asset publication.'
    }
    & $updateTool verify $manifestPath $signaturePath $installerPath
    if ($LASTEXITCODE -ne 0) { throw 'The uploaded Ed25519 signature or installer verification failed.' }

    Write-Host "Verified the uploaded draft assets for $tag ($actualSize bytes, SHA-256 $actualHash)."
}
finally {
    if (Test-Path -LiteralPath $verificationDirectory) {
        $resolvedVerificationDirectory = (Resolve-Path -LiteralPath $verificationDirectory -ErrorAction Stop).Path
        if (-not $resolvedVerificationDirectory.StartsWith($runnerTempPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Refusing to remove a release verification directory outside RUNNER_TEMP.'
        }
        Remove-Item -LiteralPath $verificationDirectory -Recurse -Force
    }
}
