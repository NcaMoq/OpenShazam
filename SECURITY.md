# Security Policy

## Supported versions

Security fixes are prepared for the latest stable release. Older releases and development builds may not receive fixes.

## Reporting a vulnerability

Use GitHub's [private vulnerability reporting form](https://github.com/NcaMoq/OpenShazam/security/advisories/new). Please do not report an unpatched vulnerability in a public issue.

Include the affected OpenShazam version, Windows version, impact, and steps to reproduce. Remove personal data, credentials, and audio content from logs or samples before attaching them. Do not include signing keys, PFX files, or passwords.

The maintainers will coordinate a fix and disclosure through the private advisory. No response-time or remediation deadline is guaranteed.

## Release signing

New stable releases produced by the current workflow require a valid Authenticode code-signing certificate and the Ed25519 update-signing key. The workflow validates the Authenticode certificate before full Windows CI and refuses to publish if either signing credential is missing or invalid. The Ed25519 key is checked against the embedded public key when the signed update manifest is generated; a mismatch prevents publication. Previously published releases are not retroactively changed. The public v0.1.1 installer is not Authenticode-signed, so Windows may show a security warning when it runs.

Configure the `stable-release` environment to accept only `v*` release tags and require an independent trusted reviewer with self-review disabled. Protect published release tags from updates and deletion, and require the Windows validation checks before changes reach `main`.

Store these environment secrets in GitHub Actions:

- `OPEN_SHAZAM_RELEASE_SIGNING_CERTIFICATE_BASE64`: base64-encoded PFX containing a non-CA code-signing certificate and its private key.
- `OPEN_SHAZAM_RELEASE_SIGNING_CERTIFICATE_PASSWORD`: the PFX password.

The repository must also have `OPEN_SHAZAM_UPDATE_SIGNING_KEY_BASE64` under **Settings > Secrets and variables > Actions**; it signs the update manifest. Keep all three values in GitHub Secrets only. Never commit them, print them in logs, or attach them to issues. The workflow verifies certificate validity, Code Signing EKU, artifact signatures, and the uploaded update signature before publishing.
