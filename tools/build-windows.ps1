# OpenShazam: native Rust tray popup + IN-PROCESS WinUI 3 Settings XAML island.
# Run from PowerShell on Windows with Rust 1.85 MSVC, VS C++ v143,
# Windows 10+ SDK, NuGet CLI and Windows App SDK runtime.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$hostDir = Join-Path $root 'apps\openshazam\settings-host'
$dist = Join-Path $root 'dist\windows-x64'
Push-Location $root
try {
    & nuget restore (Join-Path $hostDir 'packages.config') -PackagesDirectory (Join-Path $hostDir 'packages') -NonInteractive
    if ($LASTEXITCODE -ne 0) { throw "Could not restore C++/WinRT WinUI 3 native dependencies" }

    & msbuild (Join-Path $hostDir 'SettingsHost.vcxproj') /p:Configuration=Release /p:Platform=x64 /m
    if ($LASTEXITCODE -ne 0) { throw "In-process WinUI 3 Settings host failed to compile" }

    & cargo +1.85.0 build --locked --release -p openshazam --bin openshazam
    if ($LASTEXITCODE -ne 0) { throw "Rust Win32/Composition main popup failed to compile" }

    New-Item -ItemType Directory -Path $dist -Force | Out-Null
    Copy-Item -LiteralPath (Join-Path $root 'target\release\openshazam.exe') -Destination (Join-Path $dist 'openshazam.exe') -Force
    $nativeOut = Join-Path $hostDir 'bin\x64\Release'
    $dll = Join-Path $nativeOut 'OpenShazam.SettingsHost.dll'
    if (-not (Test-Path -LiteralPath $dll)) { throw "SettingsHost DLL was not produced: $dll" }
    Copy-Item -LiteralPath $dll -Destination (Join-Path $dist 'OpenShazam.SettingsHost.dll') -Force

    # Preserve compiled WinUI/XAML package resource indexes emitted by MSBuild.
    Get-ChildItem -LiteralPath $nativeOut -File -Filter '*.pri' | ForEach-Object {
        Copy-Item -LiteralPath $_.FullName -Destination (Join-Path $dist $_.Name) -Force
    }

    Write-Host "Compiled OpenShazam: $dist"
    Write-Host "Run: $(Join-Path $dist 'openshazam.exe') ui"
    Write-Host 'The Windows App SDK Runtime is required; MSIX identity/startup-task support is not yet packaged.'
}
finally {
    Pop-Location
}
