# Windows releases

Stable `vMAJOR.MINOR.PATCH` tags build the x64 Inno Setup installer in `NcaMoq/OpenShazam`. The tag must match `[workspace.package].version` in `Cargo.toml`. The workflow uploads the installer, its SHA-256 sidecar, and an Ed25519-signed `OpenShazam-update.json` manifest to the same GitHub Release. The application reads the signed manifest from that repository's stable latest Release and verifies its signature, version, architecture, file length, and SHA-256 before starting Setup.

## Required update signing secret

`OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64` is required. It must be the Base64-encoded 32-byte Ed25519 seed whose public key matches `TRUSTED_PUBLIC_KEY_HEX` in `crates/openshazam-update/src/lib.rs`. The local key-generation helper writes the seed outside the repository under `%LOCALAPPDATA%\OpenShazamReleaseSigning\ed25519-seed.base64` and restricts the folder to the current Windows user. Add that file as a protected GitHub Actions secret; never commit or upload the seed as a Release asset.

Authenticode is optional. If a code-signing certificate is available, configure `WINDOWS_SIGNING_CERTIFICATE_BASE64` and `WINDOWS_SIGNING_CERTIFICATE_PASSWORD`. Without them, Release remains unsigned and Windows may show a SmartScreen warning. Ed25519 protects update authenticity but does not establish Windows publisher reputation.

## Release checks

Before publishing a stable tag, run the Windows CI workflow manually and check the workspace tests, Windows Settings Host build, and Inno Setup contract checks. Validate workflow syntax with actionlint. The release job creates and verifies the final manifest only after optional Authenticode signing, so the signed SHA-256 describes the exact published installer bytes.

## Not yet verified

For `v0.1.0`, verify the first GitHub Release and its published assets. GitHub-hosted installation and the end-to-end update from `v0.1.0` to `v0.1.1`, including history and settings preservation, are planned for `v0.1.1`. The local release check signs and verifies the manifest and exact installer bytes; it does not verify downloads from a published Release.
