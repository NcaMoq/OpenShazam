$ErrorActionPreference = 'Stop'

$workflowPaths = @('.github/workflows/ci.yml', '.github/workflows/release-windows.yml')
$workflowText = @($workflowPaths | ForEach-Object {
    Get-Content -LiteralPath $_ -Raw
}) -join "`n"
$actionReferences = [regex]::Matches($workflowText, '(?m)^\s*(?:-\s*)?uses:\s*(?<reference>[^#\r\n]+)')
foreach ($reference in $actionReferences) {
    $value = $reference.Groups['reference'].Value.Trim()
    if ($value.StartsWith('./', [StringComparison]::Ordinal)) { continue }
    if ($value -notmatch '@[0-9a-fA-F]{40}$') {
        throw "External GitHub Action is not pinned to a full commit SHA: $value"
    }
}

$checkoutCount = [regex]::Matches($workflowText, '(?m)^\s*-\s*uses:\s*actions/checkout@').Count
$disabledCredentialPersistenceCount = [regex]::Matches($workflowText, '(?m)^\s*persist-credentials:\s*false\s*$').Count
if ($checkoutCount -eq 0 -or $checkoutCount -ne $disabledCredentialPersistenceCount) {
    throw 'Every checkout step must pin actions/checkout and disable persisted Git credentials.'
}

$ciWorkflow = Get-Content -LiteralPath '.github/workflows/ci.yml' -Raw
foreach ($trigger in @('pull_request:', 'workflow_dispatch:', 'workflow_call:')) {
    if ($ciWorkflow -notmatch [regex]::Escape($trigger)) {
        throw "The Windows CI workflow is missing the required $trigger trigger."
    }
}
if ($ciWorkflow -notmatch '(?m)^  push:\r?\n    branches: \[main\]\r?$') {
    throw 'The Windows CI workflow must run on main pushes only.'
}
$ciValidationJob = [regex]::Match(
    $ciWorkflow,
    '(?ms)^  windows-x64:\s*\r?\n(?<job>.*?)(?=^  [A-Za-z0-9_-]+:\s*$|\z)'
)
if (-not $ciValidationJob.Success -or
    $ciValidationJob.Groups['job'].Value -notmatch '(?m)^\s+timeout-minutes:\s+60\s*$') {
    throw 'The Windows CI validation job must have a bounded 60-minute timeout.'
}

$releaseWorkflow = Get-Content -LiteralPath '.github/workflows/release-windows.yml' -Raw
foreach ($jobTimeout in @(
    @{ Name = 'verify-release-source'; Minutes = 10 },
    @{ Name = 'verify-release-secrets'; Minutes = 10 },
    @{ Name = 'release'; Minutes = 45 }
)) {
    $jobMatch = [regex]::Match(
        $releaseWorkflow,
        "(?ms)^  $($jobTimeout.Name):\s*\r?\n(?<job>.*?)(?=^  [A-Za-z0-9_-]+:\s*`$|\z)"
    )
    $expectedTimeout = "(?m)^\s+timeout-minutes:\s+$($jobTimeout.Minutes)\s*`$"
    if (-not $jobMatch.Success -or $jobMatch.Groups['job'].Value -notmatch $expectedTimeout) {
        throw "Release job '$($jobTimeout.Name)' must have a bounded $($jobTimeout.Minutes)-minute timeout."
    }
}
$ciRequiredSteps = @(
    'cargo audit --file Cargo.lock',
    'windows_update::tests::oversized_update_process_is_terminated_while_streaming',
    'cargo test --workspace --all-targets --locked',
    'cargo check --workspace --all-targets --locked --no-default-features',
    'cargo fmt --all -- --check',
    'cargo clippy --workspace --all-targets --locked -- -D warnings',
    '.\tools\test-windows-installer-contract.ps1',
    '.\tools\test-windows-signing-contract.ps1',
    '.\tools\test-windows-settings-host.ps1',
    '.\tools\test-windows-composition.ps1',
    '.\tools\build-installer.ps1'
)
foreach ($requiredStep in $ciRequiredSteps) {
    if (-not $ciWorkflow.Contains($requiredStep)) {
        throw "The Windows CI workflow is missing a required validation step: $requiredStep"
    }
}

$validateJobMatch = [regex]::Match(
    $releaseWorkflow,
    '(?ms)^  validate:\s*\r?\n(?<job>.*?)(?=^  [A-Za-z0-9_-]+:\s*$|\z)'
)
if (-not $validateJobMatch.Success -or
    $validateJobMatch.Groups['job'].Value -notmatch '(?m)^\s+uses:\s+\./\.github/workflows/ci\.yml\s*$' -or
    $validateJobMatch.Groups['job'].Value -notmatch '(?m)^\s+upload_release_inputs:\s*true\s*$') {
    throw 'The release validation job must call the complete Windows CI workflow and upload its validated build inputs.'
}
$validationJobText = $validateJobMatch.Groups['job'].Value
if ($validationJobText -match '(?mi)^    if:\s*.*(?:always|failure|cancelled)\(|^    continue-on-error:\s*true') {
    throw 'The release validation job must not override required-check failure or cancellation.'
}

$releaseJobMatch = [regex]::Match(
    $releaseWorkflow,
    '(?ms)^  release:\s*\r?\n(?<job>.*?)(?=^  [A-Za-z0-9_-]+:\s*$|\z)'
)
if (-not $releaseJobMatch.Success) {
    throw 'The release workflow is missing its publishing job.'
}
$releaseJobText = $releaseJobMatch.Groups['job'].Value
$releaseNeedsMatch = [regex]::Match($releaseJobText, '(?m)^\s+needs:\s*\[(?<jobs>[^\]]+)\]')
if (-not $releaseNeedsMatch.Success) {
    throw 'The publishing job must declare all required validation dependencies.'
}
$releaseNeeds = @($releaseNeedsMatch.Groups['jobs'].Value -split ',' | ForEach-Object { $_.Trim() })
foreach ($requiredJob in @('verify-release-source', 'verify-release-secrets', 'validate')) {
    if ($requiredJob -notin $releaseNeeds) {
        throw "The publishing job must require successful completion of '$requiredJob'."
    }
}
if ($releaseJobText -match '(?mi)^    if:\s*.*(?:always|failure|cancelled)\(|^    continue-on-error:\s*true') {
    throw 'The publishing job must not override required-check failure or cancellation.'
}
foreach ($requiredDraftSafetyCheck in @(
    'GitHub did not return a valid draft release ID',
    'steps.create_draft.outputs.release_id !=',
    '$env:RELEASE_ID -notmatch',
    '$release.id -ne [long]$env:RELEASE_ID',
    '$release.tag_name -cne $env:RELEASE_TAG',
    '$release.draft -ne $true',
    'DELETE "repos/$env:GITHUB_REPOSITORY/releases/$env:RELEASE_ID"'
)) {
    if (-not $releaseWorkflow.Contains($requiredDraftSafetyCheck)) {
        throw "The release draft lifecycle is missing a cleanup ownership check: $requiredDraftSafetyCheck"
    }
}

$signingPreflightStart = $releaseWorkflow.IndexOf("  verify-release-secrets:", [StringComparison]::Ordinal)
$validationStart = $releaseWorkflow.IndexOf("`n  validate:", [StringComparison]::Ordinal)
if ($signingPreflightStart -lt 0 -or $validationStart -le $signingPreflightStart) {
    throw 'The release workflow must validate signing credentials before its full Windows CI job.'
}
$signingPreflight = $releaseWorkflow.Substring(
    $signingPreflightStart,
    $validationStart - $signingPreflightStart
)
foreach ($requiredPreflightText in @(
    'runs-on: windows-latest',
    'environment: stable-release',
    'persist-credentials: false',
    'OPEN_SHAZAM_RELEASE_SIGNING_CERTIFICATE_BASE64',
    'OPEN_SHAZAM_RELEASE_SIGNING_CERTIFICATE_PASSWORD',
    'OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64',
    '.\tools\sign-windows-artifacts.ps1 -Path @() -RequireSignature -ValidateOnly'
)) {
    if (-not $signingPreflight.Contains($requiredPreflightText)) {
        throw "The release signing preflight is missing required validation: $requiredPreflightText"
    }
}

$version = '1.7.12'
$archiveName = "actionlint_${version}_windows_amd64.zip"
$expectedSha256 = '6e7241b51e6817ea6a047693d8e6fed13b31819c9a0dd6c5a726e1592d22f6e9'
$releaseUrl = "https://github.com/rhysd/actionlint/releases/download/v$version/$archiveName"
$temporaryRoot = if (-not [string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    $env:RUNNER_TEMP
} else {
    [IO.Path]::GetTempPath()
}
$temporaryRootFullPath = (Resolve-Path -LiteralPath $temporaryRoot -ErrorAction Stop).Path.TrimEnd([IO.Path]::DirectorySeparatorChar)
$temporaryDirectory = Join-Path $temporaryRootFullPath ("openshazam-actionlint-{0}" -f [guid]::NewGuid().ToString('N'))
$temporaryDirectoryFullPath = [IO.Path]::GetFullPath($temporaryDirectory)
$temporaryRootPrefix = $temporaryRootFullPath + [IO.Path]::DirectorySeparatorChar
if (-not $temporaryDirectoryFullPath.StartsWith($temporaryRootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The actionlint temporary directory must remain directly under the temporary root.'
}

New-Item -ItemType Directory -Path $temporaryDirectory | Out-Null
try {
    $archivePath = Join-Path $temporaryDirectory $archiveName
    $curl = Get-Command 'curl.exe' -ErrorAction Stop
    & $curl.Source --location --fail --silent --show-error --proto '=https' --proto-redir '=https' --max-redirs 5 --retry 5 --retry-all-errors --retry-delay 2 --retry-max-time 90 --output $archivePath $releaseUrl
    if ($LASTEXITCODE -ne 0) {
        throw "curl.exe exited with code $LASTEXITCODE while downloading actionlint."
    }
    $actualSha256 = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualSha256 -cne $expectedSha256) {
        throw 'The actionlint archive did not match the pinned SHA-256 checksum.'
    }

    Expand-Archive -LiteralPath $archivePath -DestinationPath $temporaryDirectory
    $actionlint = Join-Path $temporaryDirectory 'actionlint.exe'
    if (-not (Test-Path -LiteralPath $actionlint -PathType Leaf)) {
        throw 'The pinned actionlint archive did not contain actionlint.exe.'
    }

    & $actionlint $workflowPaths
    if ($LASTEXITCODE -ne 0) {
        throw "actionlint exited with code $LASTEXITCODE."
    }
    Write-Host "GitHub Actions workflows passed actionlint $version."
}
finally {
    if (Test-Path -LiteralPath $temporaryDirectoryFullPath) {
        $resolvedTemporaryDirectory = (Resolve-Path -LiteralPath $temporaryDirectoryFullPath).Path
        if (-not $resolvedTemporaryDirectory.Equals($temporaryDirectoryFullPath, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Refusing to remove the actionlint temporary directory after its resolved path changed.'
        }
        Remove-Item -LiteralPath $resolvedTemporaryDirectory -Recurse -Force
    }
}
