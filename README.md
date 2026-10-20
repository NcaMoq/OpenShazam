# OpenShazam

Shazam client for Windows with a native tray UI and a Rust recognition engine.

## Build

On Windows, run `tools/build-windows.ps1 -Product`. Runtime files are staged in `dist/windows-x64`; build intermediates use the normal `target` and Visual Studio output directories. Compile the user-level installer with Inno Setup 6 using `installer/OpenShazam.iss`.

Windows releases are built from stable `vMAJOR.MINOR.PATCH` tags by GitHub Actions. Releases include an Inno Setup installer, its SHA-256 checksum, and an Ed25519-signed update manifest. Authenticode signing is optional.
