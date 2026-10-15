# Assemble the WinUI 3 desktop shell and Rust recognizer side by side.
# Run on Windows x64 with Rust 1.85 and the .NET 8 SDK installed.
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$dist = Join-Path $root 'dist\win-x64'
$project = Join-Path $root 'apps\openshazam\settings-ui\OpenShazam.Settings.csproj'

Push-Location $root
try {
    & cargo build -p openshazam --bin openshazam-ui-bridge --release --locked --no-default-features
    if ($LASTEXITCODE -ne 0) { throw "Rust recognition bridge build failed ($LASTEXITCODE)" }

    & dotnet publish $project --configuration Release --runtime win-x64 --self-contained false -p:Platform=x64 --output $dist
    if ($LASTEXITCODE -ne 0) { throw "WinUI 3 publish failed ($LASTEXITCODE)" }

    Copy-Item -LiteralPath (Join-Path $root 'target\release\openshazam-ui-bridge.exe') -Destination (Join-Path $dist 'openshazam-ui-bridge.exe') -Force
    if (-not (Test-Path (Join-Path $dist 'OpenShazam.exe'))) {
        throw 'WinUI 3 OpenShazam.exe was not emitted by publish'
    }
    if (-not (Test-Path (Join-Path $dist 'openshazam-ui-bridge.exe'))) {
        throw 'Rust recognition executable was not included'
    }
    Write-Host "OpenShazam WinUI 3 bundle: $dist"
}
finally {
    Pop-Location
}
