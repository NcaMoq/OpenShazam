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
