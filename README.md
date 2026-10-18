# OpenShazam

An unofficial Rust command-line music recognition prototype.

## Requirements

- Rust 1.85 or newer
- `ffmpeg` and `ffprobe` for audio-file input
- `curl` for live recognition requests

## Build

```sh
cargo build --workspace --locked
```

## Run

Check configuration:

```sh
cargo run --locked -p shazam-http-lab -- config-check
```

Recognize an audio file:

```sh
cargo run --locked -p shazam-http-lab -- audio-live <audio-file> <seconds>
cargo run --locked -p shazam-http-lab -- audio-progressive-live <audio-file> <seconds>
```

Capture from a microphone on macOS:

```sh
cargo run --locked -p openshazam -- recognize-microphone-progressive
```

Capture system audio on Windows:

```sh
cargo run --locked -p openshazam -- list-devices
cargo run --locked -p openshazam -- recognize-loopback [device-id]
```

Launch the Windows tray UI:

```sh
cargo build --locked -p shazam-http-lab -p openshazam
cargo run --locked -p openshazam -- ui
```

### Windows UI page links

The Windows tray UI resolves `UiConfigSnapshot.web_track_url_template` with a
result's `track_key`, validates the resulting Shazam HTTPS URL, and opens it in
the default browser. The link stays disabled if either input is unavailable;
the UI does not guess a URL. Config snapshots omit recognition endpoints,
installation IDs, and other Config values.

For CLI hosts, set `OPENSHZAM_UI_CONFIG_SNAPSHOT_JSONL=1` together with
`--events-jsonl`. This opts into independent `openshazam.ui-config-snapshot.v1`
and `openshazam.ui-recognition-input.v1` records. Each result wrapper names the
generation of the Config snapshot used by that recognition session. Without
the environment variable, the pre-existing JSONL records remain unchanged.

```sh
OPENSHZAM_UI_CONFIG_SNAPSHOT_JSONL=1 cargo run --locked -p openshazam -- \
  recognize-microphone-progressive --events-jsonl
```

## Windows startup

The unpackaged Windows app offers a per-user sign-in toggle in Settings.
It uses the `HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run` key
(`OpenShazam`) with a quoted `openshazam.exe ui` command. No elevation or package
identity is required; the toggle does not automatically start recognition.
The installer should remove this value when uninstalling the app.
