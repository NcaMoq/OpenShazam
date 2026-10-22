[CmdletBinding()]
param(
    [string]$OutputDirectory = 'dist\windows-x64',
    [switch]$UpdateTrackedNotice
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$expectedOutputDirectory = [IO.Path]::GetFullPath((Join-Path $root 'dist\windows-x64')).TrimEnd('\')
if ([IO.Path]::IsPathRooted($OutputDirectory)) {
    $outputDirectoryFullPath = [IO.Path]::GetFullPath($OutputDirectory).TrimEnd('\')
} else {
    $outputDirectoryFullPath = [IO.Path]::GetFullPath((Join-Path $root $OutputDirectory)).TrimEnd('\')
}
if ($outputDirectoryFullPath -ine $expectedOutputDirectory) {
    throw "Third-party notices may only be generated in $expectedOutputDirectory."
}
if (-not (Test-Path -LiteralPath $expectedOutputDirectory -PathType Container)) {
    throw "The output directory must already exist: $expectedOutputDirectory"
}

$toolVersion = '0.9.1'
$archiveName = "cargo-about-$toolVersion-x86_64-pc-windows-msvc.tar.gz"
$expectedSha256 = '318893aff6b9efd60f70470f5827b9577ae20a805cf9732d5612862a78508581'
$downloadUri = "https://github.com/EmbarkStudios/cargo-about/releases/download/$toolVersion/$archiveName"
$temporaryRoot = if (-not [string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
    $env:RUNNER_TEMP
} else {
    [IO.Path]::GetTempPath()
}
$temporaryRootFullPath = [IO.Path]::GetFullPath($temporaryRoot).TrimEnd('\')
$temporaryDirectory = Join-Path $temporaryRootFullPath ("openshazam-cargo-about-{0}" -f [guid]::NewGuid().ToString('N'))
$temporaryDirectoryFullPath = [IO.Path]::GetFullPath($temporaryDirectory)
$temporaryRootPrefix = $temporaryRootFullPath + [IO.Path]::DirectorySeparatorChar
if (-not $temporaryDirectoryFullPath.StartsWith($temporaryRootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
    throw 'The license tool extraction directory must remain under the temporary root.'
}

New-Item -ItemType Directory -Path $temporaryDirectoryFullPath | Out-Null
$locationPushed = $false
try {
    Push-Location -LiteralPath $root
    $locationPushed = $true
    $archivePath = Join-Path $temporaryDirectoryFullPath $archiveName
    $curl = Get-Command 'curl.exe' -ErrorAction Stop
    & $curl.Source --location --fail --silent --show-error --proto '=https' --proto-redir '=https' --max-redirs 5 --connect-timeout 10 --max-time 600 --retry 2 --output $archivePath $downloadUri
    if ($LASTEXITCODE -ne 0) {
        throw "curl.exe exited with code $LASTEXITCODE while downloading cargo-about."
    }
    $actualSha256 = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actualSha256 -cne $expectedSha256) {
        throw 'The cargo-about archive did not match its pinned SHA-256 checksum.'
    }

    $tarPath = Join-Path $env:WINDIR 'System32\tar.exe'
    if (-not (Test-Path -LiteralPath $tarPath -PathType Leaf)) {
        throw 'The Windows tar utility required to unpack cargo-about is unavailable.'
    }
    & $tarPath -xzf $archivePath -C $temporaryDirectoryFullPath
    if ($LASTEXITCODE -ne 0) {
        throw "Could not extract the pinned cargo-about archive (exit $LASTEXITCODE)."
    }

    $toolPath = Join-Path $temporaryDirectoryFullPath "cargo-about-$toolVersion-x86_64-pc-windows-msvc\cargo-about.exe"
    if (-not (Test-Path -LiteralPath $toolPath -PathType Leaf)) {
        throw 'The pinned cargo-about archive did not contain cargo-about.exe.'
    }
    $installedVersion = (& $toolPath --version | Out-String).Trim()
    if ($LASTEXITCODE -ne 0 -or $installedVersion -cne "cargo-about $toolVersion") {
        throw 'The extracted cargo-about binary did not report the pinned version.'
    }

    $generatedNoticePath = Join-Path $temporaryDirectoryFullPath 'THIRD-PARTY-NOTICES.txt'
    & $toolPath generate `
        --manifest-path 'apps/openshazam/Cargo.toml' `
        --config 'about.toml' `
        --target 'x86_64-pc-windows-msvc' `
        --no-default-features `
        --locked `
        --offline `
        --fail `
        --output-file $generatedNoticePath `
        'tools/third-party-notices.hbs'
    if ($LASTEXITCODE -ne 0) {
        throw "cargo-about could not generate the third-party notices (exit $LASTEXITCODE)."
    }

    $settingsHostDirectory = Join-Path $root 'apps\openshazam\settings-host'
    $nativePackagesDirectory = Join-Path $settingsHostDirectory 'packages'
    $packageConfigPath = Join-Path $settingsHostDirectory 'packages.config'
    if (-not (Test-Path -LiteralPath $packageConfigPath -PathType Leaf)) {
        throw 'The SettingsHost NuGet package manifest is missing.'
    }
    [xml]$packageConfig = [IO.File]::ReadAllText($packageConfigPath)
    $nativeNoticeBuilder = [System.Text.StringBuilder]::new()
    $nativeNoticePath = Join-Path $PSScriptRoot 'native-third-party-notices.txt'
    if (-not (Test-Path -LiteralPath $nativeNoticePath -PathType Leaf)) {
        throw 'The native Windows redistributable notice source is missing.'
    }
    $nativeNoticeText = [IO.File]::ReadAllText($nativeNoticePath)
    $nativeNoticeText = $nativeNoticeText -replace "`r`n?", "`n"
    [void]$nativeNoticeBuilder.Append($nativeNoticeText.TrimEnd("`n")).Append("`n`n")

    foreach ($package in $packageConfig.packages.package) {
        $packageDirectory = Join-Path $nativePackagesDirectory ($package.id + '.' + $package.version)
        if (-not (Test-Path -LiteralPath $packageDirectory -PathType Container)) {
            throw "The restored NuGet package is missing: $($package.id) $($package.version)."
        }
        $packageNotices = @(Get-ChildItem -LiteralPath $packageDirectory -File |
            Where-Object { $_.Name -in @('LICENSE', 'LICENSE.txt', 'license.txt', 'NOTICE.txt', 'ThirdPartyNotices.txt') } |
            Sort-Object Name)
        if ($packageNotices.Count -eq 0) {
            throw "The restored NuGet package has no recognized license or notice file: $($package.id) $($package.version)."
        }
        foreach ($packageNotice in $packageNotices) {
            $packageNoticeText = [IO.File]::ReadAllText($packageNotice.FullName)
            $packageNoticeText = $packageNoticeText -replace "`r`n?", "`n"
            [void]$nativeNoticeBuilder.Append('-------------------------------------------------------------------------------').Append("`n")
            [void]$nativeNoticeBuilder.Append($package.id).Append(' ').Append($package.version).Append(' / ').Append($packageNotice.Name).Append("`n`n")
            [void]$nativeNoticeBuilder.Append($packageNoticeText.TrimEnd("`n")).Append("`n`n")
        }
    }
    $utf8WithoutBom = [System.Text.UTF8Encoding]::new($false)
    [IO.File]::AppendAllText($generatedNoticePath, $nativeNoticeBuilder.ToString(), $utf8WithoutBom)

    $trackedNoticePath = Join-Path $root 'THIRD-PARTY-NOTICES.txt'
    if (-not (Test-Path -LiteralPath $generatedNoticePath -PathType Leaf) -or
        (Get-Item -LiteralPath $generatedNoticePath).Length -lt 1) {
        throw 'cargo-about did not produce the third-party notices file.'
    }
    if (-not (Test-Path -LiteralPath $trackedNoticePath -PathType Leaf)) {
        throw 'The generated third-party notice file is missing from the repository.'
    }
    $generatedNoticeHash = (Get-FileHash -LiteralPath $generatedNoticePath -Algorithm SHA256).Hash
    if ($UpdateTrackedNotice) {
        $trackedNoticeHash = ''
        if (Test-Path -LiteralPath $trackedNoticePath -PathType Leaf) {
            $trackedNoticeHash = (Get-FileHash -LiteralPath $trackedNoticePath -Algorithm SHA256).Hash
        }
        if ($generatedNoticeHash -cne $trackedNoticeHash) {
            Copy-Item -LiteralPath $generatedNoticePath -Destination $trackedNoticePath -Force
            Write-Host 'Updated THIRD-PARTY-NOTICES.txt from the pinned cargo-about output.'
        }
    }
    else {
        if (-not (Test-Path -LiteralPath $trackedNoticePath -PathType Leaf)) {
            throw 'The generated third-party notice file is missing from the repository.'
        }
        $trackedNoticeHash = (Get-FileHash -LiteralPath $trackedNoticePath -Algorithm SHA256).Hash
        if ($generatedNoticeHash -cne $trackedNoticeHash) {
            throw 'THIRD-PARTY-NOTICES.txt is stale; regenerate it with -UpdateTrackedNotice before building.'
        }
    }

    $noticePath = Join-Path $expectedOutputDirectory 'THIRD-PARTY-NOTICES.txt'
    Copy-Item -LiteralPath $trackedNoticePath -Destination $noticePath -Force
    Write-Host "Verified and staged Windows x64 third-party notices: $noticePath"
}
finally {
    if ($locationPushed) {
        Pop-Location
    }
    if (Test-Path -LiteralPath $temporaryDirectoryFullPath) {
        $resolvedTemporaryDirectory = (Resolve-Path -LiteralPath $temporaryDirectoryFullPath).Path
        if (-not $resolvedTemporaryDirectory.Equals($temporaryDirectoryFullPath, [StringComparison]::OrdinalIgnoreCase) -or
            -not $resolvedTemporaryDirectory.StartsWith($temporaryRootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw 'Refusing to remove the cargo-about temporary directory after its resolved path changed.'
        }
        Remove-Item -LiteralPath $resolvedTemporaryDirectory -Recurse -Force
    }
}
