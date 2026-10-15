# OpenShazam

OpenShazam is an unofficial Windows music-recognition desktop application.
The native recognition engine (WASAPI loopback, Progressive recognition,
Pure Rust SigX, and network transport) remains in Rust. The desktop shell is
built with WinUI 3 and Windows App SDK. Development command-line tools remain
available separately on supported platforms.

## Windows desktop build (x64)

Prerequisites: Windows, Rust 1.85.0, .NET 8 SDK, Windows App SDK runtime, and
`curl` for the current recognition network path.

Run from the repository root in PowerShell:

```powershell
./tools/build-winui.ps1
./dist/win-x64/OpenShazam.exe
```

The build assembles these files side by side:

- `OpenShazam.exe`: primary WinUI 3 desktop UI
- `openshazam-ui-bridge.exe`: native Rust recognition controller used by the UI
- the managed and WinUI runtime dependencies published by the .NET SDK

The UI starts the bridge as a local child process and exchanges bounded JSONL
commands and recognition events over redirected stdin/stdout. Audio buffers,
fingerprints, and the Shazam Config's request credentials do not cross this UI
boundary. The Rust worker remains responsible for capture and recognition.
The UI runs without elevation. History (up to 40 entries) is stored under the
current user's LocalAppData/OpenShazam directory. History timestamps are
recorded in UTC milliseconds and shown in the local time zone.

**This is an unsigned development bundle, not an MSIX installer.** Code
signing, ARM64 packaging, notification-area lifecycle, and real-device/
multi-monitor visual testing are still pending. The Windows App SDK runtime
must be present for this unpackaged build.

The old Win32/Direct2D/Windows Composition desktop renderer was removed;
do not use the historical `cargo run -p openshazam -- ui` command.

## Developer recognition tools

Requirements: Rust 1.85.0 or newer. File-input experiments use `ffmpeg`
and `ffprobe`; live requests use `curl`.

```sh
cargo build --workspace --locked
cargo run --locked -p shazam-http-lab -- config-check
cargo run --locked -p shazam-http-lab -- audio-progressive-live <audio-file> <seconds>
```

On Windows, use `cargo run --locked -p openshazam -- list-devices` and
`cargo run --locked -p openshazam -- recognize-loopback`.
On macOS, use `cargo run --locked -p openshazam -- recognize-microphone-progressive`.

## Music page links

The Rust bridge uses only the current Shazam Config's
`settings.webtrackurl.href` (or plain-text legacy template), combined
with the recognized track's real `track_key`. Links must resolve to
validated HTTPS `shazam.com` URLs. A result without both inputs has no
page link; no hard-coded track-page fallback is used.

The optional developer-CLI JSONL diagnostic controls are unchanged:
`OPENSHZAM_UI_CONFIG_SNAPSHOT_JSONL=1` with `--events-jsonl` emits
Config snapshot and typed UI event records for testing.
