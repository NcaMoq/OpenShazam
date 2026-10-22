param(
    [string]$PackagesDirectory
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$hostDirectory = Join-Path $root 'apps\openshazam\settings-host'
$packageConfigPath = Join-Path $hostDirectory 'packages.config'
$nugetConfigPath = Join-Path $root 'NuGet.Config'
if ([string]::IsNullOrWhiteSpace($PackagesDirectory)) {
    $PackagesDirectory = Join-Path $hostDirectory 'packages'
}
$PackagesDirectory = [IO.Path]::GetFullPath($PackagesDirectory)

foreach ($path in @($packageConfigPath, $nugetConfigPath)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Required NuGet trust input is missing: $path"
    }
}
if (-not (Test-Path -LiteralPath $PackagesDirectory -PathType Container)) {
    throw "Restored NuGet package directory is missing: $PackagesDirectory"
}

$nuget = Get-Command 'nuget.exe' -ErrorAction Stop
[xml]$packageConfig = [IO.File]::ReadAllText($packageConfigPath)
if (-not $packageConfig.packages.package -or $packageConfig.packages.package.Count -eq 0) {
    throw 'The native NuGet package manifest is empty.'
}

$verifiedCount = 0
foreach ($package in $packageConfig.packages.package) {
    $packageName = "$($package.id).$($package.version)"
    $archive = Join-Path (Join-Path $PackagesDirectory $packageName) "$packageName.nupkg"
    if (-not (Test-Path -LiteralPath $archive -PathType Leaf)) {
        throw "The locked native NuGet package archive is missing: $packageName"
    }

    & $nuget.Source verify -Signatures $archive -ConfigFile $nugetConfigPath -Verbosity quiet
    if ($LASTEXITCODE -ne 0) {
        throw "The native NuGet package signature or trusted owner check failed: $packageName"
    }
    $verifiedCount++
}

Write-Host "Verified NuGet.org repository signatures and trusted owners for $verifiedCount pinned native packages."
