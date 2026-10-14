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

### UI page-link inputs

The engine exposes the optional `settings.webtrackurl` value as
`ShazamConfiguration::web_track_url_template()` and
`UiConfigSnapshot`. It exposes a result's response-string `track_key` through
the existing Manual/Auto UI result types. A host must combine those inputs in
its own PageLinkResolver; OpenShazam does not substitute `{trackid}`, validate
or open a page URL. The Config snapshot omits the recognition POST endpoint,
installation ID, and other Config values.

For CLI hosts, set `OPENSHZAM_UI_CONFIG_SNAPSHOT_JSONL=1` together with
`--events-jsonl`. This opts into independent `openshazam.ui-config-snapshot.v1`
and `openshazam.ui-recognition-input.v1` records. Each result wrapper names the
generation of the Config snapshot used by that recognition session. Without
the environment variable, the pre-existing JSONL records remain unchanged.

```sh
OPENSHZAM_UI_CONFIG_SNAPSHOT_JSONL=1 cargo run --locked -p openshazam -- \
  recognize-microphone-progressive --events-jsonl
```
