# OpenShazam

An unofficial, open-source Shazam client for Windows, built with Rust.

Recognize music playing on your PC, view your recognition history, and automatically identify songs in the background with Auto Shazam.

![OpenShazam tray UI in light and dark themes](assets/shazam-light-dark.png)

## Features

- **Music Recognition** — Identify songs playing on your PC.
- **Auto Shazam** — Continuously recognize music in the background.
- **Recognition History** — Browse your previously identified songs.
- **Native Windows UI** — Lightweight system tray interface with light and dark theme support.

## Download

Download the latest Windows x64 installer from [GitHub Releases](https://github.com/NcaMoq/OpenShazam/releases).

Run the installer, then launch Shazam from the Start menu. Its controls are available in the system tray.

The currently published v0.1.1 installer is not Authenticode-signed, so Windows may show a security warning when it runs. New stable releases are gated on Authenticode signing; existing releases are not retroactively changed.

## System Requirements

Windows 10 version 2004 (build 19041) or later, x64. The installer includes the Windows App SDK Runtime used by Settings.

## Updates

Automatic updates are enabled by default. Open **Settings > Updates** to turn them off or check manually. Before installation, OpenShazam verifies the Ed25519-signed update manifest and the installer SHA-256 and size.

## FAQ

- **Is PC audio uploaded?** Raw audio is not uploaded or saved as an audio file; OpenShazam sends a locally generated audio signature to Shazam for recognition. See [Privacy and Limitations](#privacy-and-limitations).
- **Can I turn off automatic updates?** Yes. Change **Automatic updates** in **Settings > Updates**; you can still check manually.
- **Why can recognition stop working?** OpenShazam uses unofficial Shazam service endpoints that may change. See [Troubleshooting](#troubleshooting).

## Privacy and Limitations

The Windows app captures PC audio in memory and creates a Shazam-compatible audio signature locally. Raw audio is not uploaded or saved as an audio file. Recognition requests send the signature, sample duration, current timestamp, time zone, and empty `context` and `geolocation` objects to Shazam's unofficial HTTPS service; the `thirdparty` field is omitted. Recognition URLs use the installation ID (`INID`) returned by Shazam Config and include a random per-request tag ID when the Config route template calls for one. The INID is kept in memory for the current recognition session, not written to disk.

To obtain the recognition endpoint, OpenShazam sends a country code (default `-`), Windows language and locale, device model, Windows version and build, and Shazam-compatible platform and app-version information to Shazam Config. The recognition request also includes Shazam-compatible platform and app-version metadata. If set in the process environment, `SHAZAM_CONFIG_COUNTRY`, `SHAZAM_CONFIG_LANGUAGE`, `SHAZAM_CONFIG_LOCALE`, `SHAZAM_CONFIG_MODEL`, `SHAZAM_CONFIG_OS_VERSION`, and `SHAZAM_CONFIG_OS_BUILD` override the corresponding Config values; `TZ` can override the time zone. `SHAZAM_STREAMING_PROVIDER` may be included in the recognition URL when Shazam's Config endpoint provides a matching field. For diagnostic use, `SHAZAM_TAG_URL_TEMPLATE` and `SHAZAM_INID` can override the Manual recognition route when both are set; the route is still validated against the supported Shazam HTTPS endpoint policy, and Auto Shazam rejects these overrides. Shazam's services receive ordinary connection metadata such as your IP address. Artwork is fetched from URLs returned in track results, so the corresponding image hosts also receive your IP address. Update checks and downloads contact public GitHub Releases endpoints, which also receive ordinary connection metadata such as your IP address and the requested asset URL.

For the current Windows user, recognition history is stored in `%LOCALAPPDATA%\OpenShazam\history.db`. It contains recognized track details such as title, artist, recognition time, track identifier, artwork URLs, Shazam page URL, and recognition source. Preferences are stored in `HKCU\Software\OpenShazam`; the startup value is `OpenShazam` under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`. Update downloads and temporary update state are stored in `%LOCALAPPDATA%\OpenShazam\Updates`. Uninstalling OpenShazam removes these app-owned settings and files, including migrated OpenShazam history.

OpenShazam does not implement its own usage analytics or crash-reporting service. Settings uses the Microsoft Windows App SDK Runtime; its runtime components may participate in Windows diagnostic data collection, which is controlled by Windows. You can review the relevant options in **Settings > Privacy & security > Diagnostics & feedback** (Windows 10: **Settings > Privacy > Diagnostics & feedback**). See [Windows diagnostic data](https://learn.microsoft.com/windows/privacy/configure-windows-diagnostic-data-in-your-organization) for details.

OpenShazam is unofficial and depends on Shazam service endpoints that may change without notice. It is not affiliated with or endorsed by Apple or Shazam.

## Troubleshooting

- If recognition cannot connect, check your internet connection and try again later. Shazam may change its unofficial service endpoints.
- If Settings does not open, rerun the installer to repair the Windows App SDK Runtime prerequisite.

## Build from Source

**Requirements:** Windows, Rust 1.85, Visual Studio C++ Build Tools, Windows SDK, NuGet, and Inno Setup 6 (for installer builds).

```powershell
New-Item -ItemType Directory -Force dist/windows-x64
.\tools\build-windows.ps1 -Product
.\tools\prepare-windows-app-sdk-runtime.ps1
.\tools\build-installer.ps1
```

## License

Licensed under the [MIT License](LICENSE).
