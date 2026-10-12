# OpenShazam

OpenShazam is an unofficial Rust music-fingerprinting and recognition
prototype. It includes a Pure Rust SigX pipeline, a command-line HTTP
recognition experiment, and Windows WASAPI loopback recognition.

## Requirements

- Rust 1.85 or newer
- `ffmpeg` and `ffprobe` for audio-file input
- `curl` on macOS or `curl.exe` on Windows for the experimental live HTTP command
- macOS system tools (`defaults`, `sysctl`, `sw_vers`) or Windows PowerShell/CIM
  for automatic Config profile detection

## Build and test

From the repository root:

```sh
cargo build --workspace --locked
cargo test --workspace --all-targets --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

To check the Windows GNU target from macOS:

```sh
rustup target add x86_64-pc-windows-gnu
cargo check --workspace --all-targets --target x86_64-pc-windows-gnu --locked
```

## Run

Create a local mock recognition request from an audio file (no service request):

```sh
cargo run --locked -p shazam-http-lab -- audio-mock <audio-file> [seconds]
```

`SHAZAM_TAG_URL_TEMPLATE` and `SHAZAM_INID` remain paired manual overrides.
Optional `SHAZAM_CONFIG_*` variables override the detected locale, model, OS
version/build, or country (`-` is used when storefront country is unknown).

```sh
cargo run --locked -p shazam-http-lab -- audio-live <audio-file> [seconds]
```

The progressive file command feeds the Pure Rust SigX engine in PCM chunks,
submits a Signature at Config's cumulative sample threshold, and continues with
larger prefixes when recognition requests a retry. Sample duration is audio
time. File chunks are paced at roughly 100 ms; the HTTPS request runs separately
so PCM feeding continues during the response wait. Logs distinguish cumulative
sample duration, Signature-ready time, POST start, response receipt, and request
latency. Their elapsed-time origin is after Config retrieval and source decode;
the command is not a microphone adapter.

The local safety controls are `OPENSHZAM_PROGRESSIVE_MINIMUM_SAMPLE_FALLBACK_MS`,
`OPENSHZAM_PROGRESSIVE_DEFAULT_RETRY_DELAY_MS`,
`OPENSHZAM_PROGRESSIVE_RETRY_DELAY_CAP_MS`,
`OPENSHZAM_PROGRESSIVE_REQUEST_TIMEOUT_MS`, and
`OPENSHZAM_PROGRESSIVE_MAX_ATTEMPTS`. Defaults are local safeguards, not claims
about undocumented Shazam app defaults.

```sh
cargo run --locked -p shazam-http-lab -- audio-progressive-live <audio-file> [seconds]
```

Check Config retrieval without sending an audio signature:

```sh
cargo run --locked -p shazam-http-lab -- config-check
```

On Windows, Config reads the host locale, model, OS version/build, and IANA
timezone. The service currently rejects `platform=WIN` with HTTP 400 and returns
the existing Mac tag route for `MAC/MACOS`; the live command uses that route
with the Windows host profile. Windows HTTPS uses `curl.exe` with Schannel
certificate validation.

On Windows, `openshazam` can list playback endpoints, recognize 12 seconds of
system playback from the default output, or write that loopback Signature
locally. Pass a listed device ID to target a non-default output:

```sh
cargo run --locked -p openshazam -- list-devices
cargo run --locked -p openshazam -- recognize-loopback [device-id]
cargo run --locked -p openshazam -- capture-signature-loopback <new-output.sig> [device-id]
```

The existing Windows command remains fixed at 12 seconds. The progressive
adapter point is the downmixed PCM packet from `capture_loopback_frames`:
forward it to `ProgressiveSession::push_f32`, then dispatch each returned
`RecognitionJob` on a worker. This connection is not wired or Windows-runtime
tested yet.

Private Native fixtures are not included. The ordinary workspace tests do not
require them. To run the optional private-vector regression, set
`SHZ_PRIVATE_VECTOR_ROOT` to the external authorized research directory and
run `scripts/test-private-vectors.sh`; it reads fixtures in place.
