# OpenShazam

OpenShazam is an unofficial Rust music-fingerprinting and recognition
prototype. It includes a Pure Rust SigX pipeline, a command-line HTTP
recognition experiment, and an early microphone-to-Signature prototype.

## Requirements

- Rust 1.85 or newer
- `ffmpeg` and `ffprobe` for audio-file input
- `/usr/bin/curl` for the experimental live HTTP command
- macOS system tools (`defaults`, `sysctl`, `sw_vers`) for automatic Config profile detection
- A Windows x64 Rust toolchain for native Windows execution; that runtime path
  has not yet been validated

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

The live command fetches Config over verified HTTPS on each run and resolves
the recognition route and installation identifier in memory. It does not read
or write Shazam app preferences and does not send an AMP token. An authorized
macOS end-to-end test passed with all manual overrides unset: the CLI generated
a Pure Rust Signature and received HTTP 200 with a track result matching the
audio metadata. This is an unofficial, non-public recognition route; use it
only when you are authorized, and expect the service to change.

`SHAZAM_TAG_URL_TEMPLATE` and `SHAZAM_INID` remain paired manual overrides.
Optional `SHAZAM_CONFIG_*` variables override the detected locale, model, OS
version/build, or country (`-` is used when storefront country is unknown).

```sh
cargo run --locked -p shazam-http-lab -- audio-live <audio-file> [seconds]
```

Check Config retrieval without sending an audio signature:

```sh
cargo run --locked -p shazam-http-lab -- config-check
```

The microphone prototype writes a Signature locally and does not perform
recognition:

```sh
cargo run --locked -p openshazam -- capture-signature <new-output.sig>
```

Private Native fixtures are not included. The ordinary workspace tests do not
require them. To run the optional private-vector regression, set
`SHZ_PRIVATE_VECTOR_ROOT` to the external authorized research directory and
run `scripts/test-private-vectors.sh`; it reads fixtures in place.
