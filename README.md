# OpenShazam

An unofficial, open-source Shazam client for Windows, built with Rust.

Recognize music playing on your PC, view your recognition history, and automatically identify songs in the background with Auto Shazam.

![OpenShazam tray UI in light and dark themes](assets/shazam-light-dark.png)

## Features

- **Music Recognition** — Identify songs playing on your PC.
- **Auto Shazam** — Continuously recognize music in the background.
- **Recognition History** — Browse your previously identified songs.
- **Native Windows UI** — Lightweight system tray interface with light and dark theme support.
- **Secure Updates** — Built-in update checks with Ed25519 signature verification.

## Download

Download the latest Windows x64 installer from [GitHub Releases](https://github.com/NcaMoq/OpenShazam/releases).

Run the installer, then launch Shazam from the Start menu. Its controls are available in the system tray.

## Build from Source

**Requirements:** Windows, Rust 1.85, Visual Studio C++ Build Tools, Windows SDK, NuGet, and Inno Setup 6 (for installer builds).

```powershell
New-Item -ItemType Directory -Force dist/windows-x64
.\tools\build-windows.ps1 -Product
```

To create the Windows installer, compile `installer/OpenShazam.iss` with Inno Setup 6.

## Disclaimer

OpenShazam is an independent, unofficial project and is not affiliated with or endorsed by Apple Inc. or Shazam.

## License

Licensed under the [MIT License](LICENSE).
