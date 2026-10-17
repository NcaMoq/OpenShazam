# OpenShazam: native Rust tray popup + IN-PROCESS WinUI 3 Settings XAML island.
# Run from PowerShell on Windows with Rust 1.85 MSVC, VS C++ v143,
# Windows 10+ SDK, NuGet CLI and Windows App SDK runtime.
param(
    [switch]$Product,
    [string]$OutputDirectory
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$hostDir = Join-Path $root 'apps\openshazam\settings-host'
$expectedDist = [IO.Path]::GetFullPath((Join-Path $root 'dist\windows-x64')).TrimEnd('\')
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $dist = $expectedDist
}
elseif ([IO.Path]::IsPathRooted($OutputDirectory)) {
    $dist = [IO.Path]::GetFullPath($OutputDirectory)
}
else {
    $dist = [IO.Path]::GetFullPath((Join-Path $root $OutputDirectory))
}
if ([IO.Path]::GetFullPath($dist).TrimEnd('\') -ine $expectedDist) {
    throw "Build output is restricted to $expectedDist"
}
$dist = $expectedDist
$packagesDir = Join-Path $dist 'packages'
$nativeOut = Join-Path $dist 'native\bin\x64\Release'
$nativeInt = Join-Path $dist 'native\obj\x64\Release'
$generatedDir = Join-Path $dist 'native\generated'
$xamlGeneratedDir = Join-Path $dist 'native\xaml'
$cargoTargetDir = Join-Path $dist 'cargo-target'
$packagesDirMsbuild = $packagesDir.TrimEnd('\') + '\'
$nativeOutMsbuild = $nativeOut.TrimEnd('\') + '\'
$nativeIntMsbuild = $nativeInt.TrimEnd('\') + '\'
$generatedDirMsbuild = $generatedDir.TrimEnd('\') + '\'
$xamlGeneratedDirMsbuild = $xamlGeneratedDir.TrimEnd('\') + '\'
$msbuildCommand = Get-Command msbuild.exe -ErrorAction SilentlyContinue
if ($msbuildCommand) {
    $msbuild = $msbuildCommand.Source
}
else {
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) {
        throw 'MSBuild was not found. Install Visual Studio 2022 with the C++ desktop workload.'
    }
    $msbuild = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find 'MSBuild\**\Bin\MSBuild.exe' | Select-Object -First 1
    if (-not $msbuild) {
        throw 'MSBuild was not found. Install Visual Studio 2022 with the C++ desktop workload.'
    }
}
Push-Location $root
$previousCargoTargetDir = $env:CARGO_TARGET_DIR
try {
    New-Item -ItemType Directory -Path $dist -Force | Out-Null
    New-Item -ItemType Directory -Path $packagesDir -Force | Out-Null
    New-Item -ItemType Directory -Path $nativeOut, $nativeInt, $generatedDir, $xamlGeneratedDir, $cargoTargetDir -Force | Out-Null

    & nuget restore (Join-Path $hostDir 'packages.config') -PackagesDirectory $packagesDir -NonInteractive
    if ($LASTEXITCODE -ne 0) { throw "Could not restore C++/WinRT WinUI 3 native dependencies" }

    & $msbuild (Join-Path $hostDir 'SettingsHost.vcxproj') `
        /p:Configuration=Release `
        /p:Platform=x64 `
        "/p:OpenShazamPackagesDir=$packagesDirMsbuild" `
        "/p:OutDir=$nativeOutMsbuild" `
        "/p:IntDir=$nativeIntMsbuild" `
        "/p:MSBuildProjectExtensionsPath=$nativeIntMsbuild" `
        "/p:GeneratedFilesDir=$generatedDirMsbuild" `
        "/p:XamlGeneratedOutputPath=$xamlGeneratedDirMsbuild" `
        /p:_CppCommonExtensionTargets= /m /v:minimal
    if ($LASTEXITCODE -ne 0) { throw "In-process WinUI 3 Settings host failed to compile" }

    $env:CARGO_TARGET_DIR = $cargoTargetDir
    $cargoArgs = @('+1.85.0', 'build', '--locked', '--release', '-p', 'openshazam', '--bin', 'openshazam')
    if ($Product) { $cargoArgs += '--no-default-features' }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw "Rust Win32/Composition main popup failed to compile" }

    Copy-Item -LiteralPath (Join-Path $cargoTargetDir 'release\openshazam.exe') -Destination (Join-Path $dist 'openshazam.exe') -Force
    $dll = Join-Path $nativeOut 'OpenShazam.SettingsHost.dll'
    if (-not (Test-Path -LiteralPath $dll)) { throw "SettingsHost DLL was not produced: $dll" }
    Copy-Item -LiteralPath $dll -Destination (Join-Path $dist 'OpenShazam.SettingsHost.dll') -Force

    $bootstrap = Join-Path $nativeOut 'Microsoft.WindowsAppRuntime.Bootstrap.dll'
    if (-not (Test-Path -LiteralPath $bootstrap)) { throw "Windows App SDK bootstrapper was not produced: $bootstrap" }
    Copy-Item -LiteralPath $bootstrap -Destination (Join-Path $dist 'Microsoft.WindowsAppRuntime.Bootstrap.dll') -Force

    # Preserve compiled WinUI/XAML package resource indexes emitted by MSBuild.
    Get-ChildItem -LiteralPath $nativeOut -File -Filter '*.pri' | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $dist $_.Name) -Force
    }

    Write-Host "Compiled OpenShazam: $dist"
    if ($Product) {
        Write-Host "Product mode: no development CLI; launch with no arguments or 'ui'."
        Write-Host "Run: $(Join-Path $dist 'openshazam.exe')"
    }
    else {
        Write-Host "Run: $(Join-Path $dist 'openshazam.exe') ui"
    }
    Write-Host 'The Windows App SDK Runtime is required on target machines.'
}
finally {
    $env:CARGO_TARGET_DIR = $previousCargoTargetDir
    Pop-Location
}
