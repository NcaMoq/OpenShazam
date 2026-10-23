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
$nugetConfig = Join-Path $root 'NuGet.Config'
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
    throw "Runtime staging is restricted to the existing directory $expectedDist"
}
$dist = $expectedDist
$packagesDir = Join-Path $hostDir 'packages'
$nativeOut = Join-Path $hostDir 'bin\x64\Release'
$packagesDirMsbuild = $packagesDir.TrimEnd('\') + '\'
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
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$vsInstallationPath = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vsInstallationPath) { throw 'Visual Studio installation path was not found.' }
$crtRedistRoot = Join-Path $vsInstallationPath 'VC\Redist\MSVC'
$crtDirectory = Get-ChildItem -LiteralPath $crtRedistRoot -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match '^\d+(\.\d+){2,3}$' } |
    Sort-Object { [version]$_.Name } -Descending |
    ForEach-Object { Join-Path $_.FullName 'x64\Microsoft.VC143.CRT' } |
    Where-Object { (Test-Path -LiteralPath (Join-Path $_ 'msvcp140.dll')) } |
    Select-Object -First 1
if (-not $crtDirectory) { throw "The x64 VC++ 14.3 runtime could not be found under $crtRedistRoot." }
$crtRuntimeFiles = @('msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll')
Push-Location $root
try {
    if (-not (Test-Path -LiteralPath $dist -PathType Container)) {
        throw "The executable destination must already exist: $dist"
    }

    & nuget restore (Join-Path $hostDir 'packages.config') `
        -PackagesDirectory $packagesDir -ConfigFile $nugetConfig -NonInteractive
    if ($LASTEXITCODE -ne 0) { throw "Could not restore C++/WinRT WinUI 3 native dependencies" }

    & (Join-Path $PSScriptRoot 'verify-native-package-signatures.ps1') -PackagesDirectory $packagesDir

    & $msbuild (Join-Path $hostDir 'SettingsHost.vcxproj') `
        /p:Configuration=Release `
        /p:Platform=x64 `
        "/p:OpenShazamPackagesDir=$packagesDirMsbuild" `
        /p:_CppCommonExtensionTargets= /t:Rebuild /m /v:minimal
    if ($LASTEXITCODE -ne 0) { throw "In-process WinUI 3 Settings host failed to compile" }

    $cargoArgs = @('+1.85.0', 'build', '--locked', '--release', '-p', 'openshazam', '--bin', 'openshazam')
    if ($Product) { $cargoArgs += '--no-default-features' }
    & cargo @cargoArgs
    if ($LASTEXITCODE -ne 0) { throw "Rust Win32/Composition main popup failed to compile" }

    Copy-Item -LiteralPath (Join-Path $root 'target\release\openshazam.exe') -Destination (Join-Path $dist 'openshazam.exe') -Force
    Copy-Item -LiteralPath (Join-Path $root 'apps\openshazam\assets\OpenShazamSquare44.png') `
        -Destination (Join-Path $dist 'OpenShazamSquare44.png') -Force
    $dll = Join-Path $nativeOut 'OpenShazam.SettingsHost.dll'
    if (-not (Test-Path -LiteralPath $dll)) { throw "SettingsHost DLL was not produced: $dll" }
    Copy-Item -LiteralPath $dll -Destination (Join-Path $dist 'OpenShazam.SettingsHost.dll') -Force

    $bootstrap = Join-Path $nativeOut 'Microsoft.WindowsAppRuntime.Bootstrap.dll'
    if (-not (Test-Path -LiteralPath $bootstrap)) { throw "Windows App SDK bootstrapper was not produced: $bootstrap" }
    Copy-Item -LiteralPath $bootstrap -Destination (Join-Path $dist 'Microsoft.WindowsAppRuntime.Bootstrap.dll') -Force
    foreach ($runtimeFile in $crtRuntimeFiles) {
        $source = Join-Path $crtDirectory $runtimeFile
        if (-not (Test-Path -LiteralPath $source -PathType Leaf)) { throw "Required VC++ runtime is missing: $source" }
        Copy-Item -LiteralPath $source -Destination (Join-Path $dist $runtimeFile) -Force
    }

    # Preserve compiled WinUI/XAML package resource indexes emitted by MSBuild.
    Get-ChildItem -LiteralPath $nativeOut -File -Filter '*.pri' | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $dist $_.Name) -Force
    }

    # Framework-dependent unpackaged apps still need an application PRI root.
    # Windows App SDK 1.8.6's MRT Core fails XamlControlsResources activation
    # when no application PRI exists; a minimal Application map lets resource
    # lookup fall through to the WinUI framework package as documented.
    $windowsKitsBin = Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'
    $makePri = Get-ChildItem -LiteralPath $windowsKitsBin -Directory -ErrorAction Stop |
        Where-Object { $_.Name -match '^10\.0\.\d+\.0$' } |
        Sort-Object { [version]$_.Name } -Descending |
        ForEach-Object { Join-Path $_.FullName 'x64\makepri.exe' } |
        Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } |
        Select-Object -First 1
    if (-not $makePri) { throw "Windows SDK makepri.exe was not found under $windowsKitsBin." }
    $resourceIndexRoot = Join-Path $root 'target\openshazam-resource-index'
    New-Item -ItemType Directory -Path $resourceIndexRoot -Force | Out-Null
    $resourceConfig = Join-Path $resourceIndexRoot 'priconfig.xml'
    & $makePri createconfig /cf $resourceConfig /dq en-US /o | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "makepri.exe could not create the application PRI configuration ($LASTEXITCODE)." }
    $resourceConfigXml = [IO.File]::ReadAllText($resourceConfig)
    $resourceConfigXml = [regex]::Replace($resourceConfigXml, '(?s)\s*<packaging>.*?</packaging>', '')
    [IO.File]::WriteAllText($resourceConfig, $resourceConfigXml, [Text.UTF8Encoding]::new($false))
    $applicationPri = Join-Path $dist 'resources.pri'
    & $makePri new /pr $resourceIndexRoot /cf $resourceConfig /of $applicationPri /o | Out-Null
    if ($LASTEXITCODE -ne 0 -or -not (Test-Path -LiteralPath $applicationPri -PathType Leaf)) {
        throw "makepri.exe failed to create the unpackaged Application resource index ($LASTEXITCODE)."
    }

    Write-Host "Runtime files staged to: $dist"
    Write-Host "Cargo build output: $(Join-Path $root 'target\release')"
    Write-Host "Settings host build output: $nativeOut"
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
    Pop-Location
}
