//! Signed GitHub Releases update metadata shared by the application and the
//! release tooling. The manifest bytes are signed exactly as published.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fmt, fs::File, io::Read, path::Path};
use zeroize::Zeroizing;

pub const PRODUCT_NAME: &str = "OpenShazam";
pub const LATEST_MANIFEST_URL: &str =
    "https://github.com/NcaMoq/OpenShazam/releases/latest/download/OpenShazam-update.json";
pub const LATEST_SIGNATURE_URL: &str =
    "https://github.com/NcaMoq/OpenShazam/releases/latest/download/OpenShazam-update.json.sig";
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
const TRUSTED_PUBLIC_KEY_HEX: &str =
    "4db1559feb3b107fef4bfcba5a646c50e66ad6168e9529ba6fba329ea0d3745d";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpdateManifest {
    pub schema_version: u32,
    pub product: String,
    pub repository: String,
    pub version: String,
    pub minimum_supported_version: String,
    pub architecture: String,
    pub release_tag: String,
    pub release_url: String,
    pub installer_name: String,
    pub installer_size_bytes: u64,
    pub installer_sha256: String,
}

impl UpdateManifest {
    /// Stable, pretty JSON with a trailing newline. This exact byte sequence is
    /// signed by the release key and uploaded unchanged.
    pub fn signed_bytes(&self) -> Result<Vec<u8>, UpdateError> {
        let mut bytes = serde_json::to_vec_pretty(self).map_err(UpdateError::Json)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn validate(&self) -> Result<(), UpdateError> {
        if self.schema_version != 1 {
            return Err(UpdateError::Invalid("unsupported schema version"));
        }
        if self.product != PRODUCT_NAME {
            return Err(UpdateError::Invalid("unexpected product"));
        }
        validate_repository(&self.repository)?;
        let version = StableVersion::parse(&self.version)?;
        let minimum = StableVersion::parse(&self.minimum_supported_version)?;
        if minimum > version {
            return Err(UpdateError::Invalid(
                "minimum supported version is newer than the release",
            ));
        }
        if self.architecture != "x64" {
            return Err(UpdateError::Invalid("unsupported architecture"));
        }
        let expected_tag = format!("v{}", self.version);
        if self.release_tag != expected_tag {
            return Err(UpdateError::Invalid("release tag does not match version"));
        }
        let expected_release_url = format!(
            "https://github.com/{}/releases/tag/{}",
            self.repository, expected_tag
        );
        if self.release_url != expected_release_url {
            return Err(UpdateError::Invalid(
                "release URL does not match repository and tag",
            ));
        }
        let expected_installer = format!("Shazam-Setup-{}-x64.exe", self.version);
        if self.installer_name != expected_installer {
            return Err(UpdateError::Invalid(
                "installer name does not match version and architecture",
            ));
        }
        if self.installer_size_bytes == 0 || self.installer_size_bytes > MAX_INSTALLER_BYTES {
            return Err(UpdateError::Invalid(
                "installer size is outside the accepted range",
            ));
        }
        if !is_lower_hex_sha256(&self.installer_sha256) {
            return Err(UpdateError::Invalid(
                "installer SHA-256 must be 64 lowercase hex characters",
            ));
        }
        Ok(())
    }

    /// Build the asset URL from verified repository/tag/name components. No
    /// arbitrary URL from the manifest is ever executed or downloaded.
    pub fn installer_url(&self) -> Result<String, UpdateError> {
        self.validate()?;
        Ok(format!(
            "https://github.com/{}/releases/download/{}/{}",
            self.repository, self.release_tag, self.installer_name
        ))
    }

    pub fn is_newer_than(&self, current_version: &str) -> Result<bool, UpdateError> {
        self.validate()?;
        let current = StableVersion::parse(current_version)?;
        let minimum = StableVersion::parse(&self.minimum_supported_version)?;
        if current < minimum {
            return Err(UpdateError::Invalid(
                "installed version is below the manifest minimum supported version",
            ));
        }
        Ok(StableVersion::parse(&self.version)? > current)
    }
}

pub fn parse_public_key_hex(value: &str) -> Result<VerifyingKey, UpdateError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(UpdateError::Invalid(
            "Ed25519 public key must be 64 hex characters",
        ));
    }
    let mut bytes = [0_u8; 32];
    for (index, output) in bytes.iter_mut().enumerate() {
        *output = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| UpdateError::Invalid("invalid Ed25519 public key"))?;
    }
    VerifyingKey::from_bytes(&bytes).map_err(|_| UpdateError::Invalid("invalid Ed25519 public key"))
}

pub fn embedded_public_key() -> Result<VerifyingKey, UpdateError> {
    parse_public_key_hex(TRUSTED_PUBLIC_KEY_HEX)
}

pub fn signing_key_from_base64(value: &str) -> Result<SigningKey, UpdateError> {
    let bytes = Zeroizing::new(
        STANDARD
            .decode(value.trim())
            .map_err(|_| UpdateError::Invalid("update signing key is not valid base64"))?,
    );
    if bytes.len() != 32 {
        return Err(UpdateError::Invalid(
            "update signing key must decode to a 32-byte seed",
        ));
    }
    let seed =
        Zeroizing::new(<[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| {
            UpdateError::Invalid("update signing key must decode to a 32-byte seed")
        })?);
    Ok(SigningKey::from_bytes(&seed))
}

pub fn public_key_hex(signing_key: &SigningKey) -> String {
    encode_hex(signing_key.verifying_key().as_bytes())
}

pub fn create_manifest(
    installer: &[u8],
    version: &str,
    minimum_supported_version: &str,
    repository: &str,
) -> Result<UpdateManifest, UpdateError> {
    if installer.is_empty() || installer.len() as u64 > MAX_INSTALLER_BYTES {
        return Err(UpdateError::Invalid(
            "installer size is outside the accepted range",
        ));
    }
    let version = StableVersion::parse(version)?.to_string();
    let minimum_supported_version = StableVersion::parse(minimum_supported_version)?.to_string();
    validate_repository(repository)?;
    let release_tag = format!("v{version}");
    let manifest = UpdateManifest {
        schema_version: 1,
        product: PRODUCT_NAME.to_owned(),
        repository: repository.to_owned(),
        version: version.clone(),
        minimum_supported_version,
        architecture: "x64".to_owned(),
        release_tag: release_tag.clone(),
        release_url: format!("https://github.com/{repository}/releases/tag/{release_tag}"),
        installer_name: format!("Shazam-Setup-{version}-x64.exe"),
        installer_size_bytes: installer.len() as u64,
        installer_sha256: sha256_hex(installer),
    };
    manifest.validate()?;
    Ok(manifest)
}

pub fn sign_manifest(
    manifest: &UpdateManifest,
    signing_key: &SigningKey,
) -> Result<String, UpdateError> {
    manifest.validate()?;
    let signature = signing_key.sign(&manifest.signed_bytes()?);
    Ok(STANDARD.encode(signature.to_bytes()))
}

pub fn verify_signed_manifest(
    manifest_bytes: &[u8],
    signature_base64: &str,
    public_key: &VerifyingKey,
) -> Result<UpdateManifest, UpdateError> {
    if manifest_bytes.is_empty() || manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(UpdateError::Invalid(
            "manifest size is outside the accepted range",
        ));
    }
    let signature_bytes = STANDARD
        .decode(signature_base64.trim())
        .map_err(|_| UpdateError::Invalid("manifest signature is not valid base64"))?;
    let signature = Signature::from_slice(&signature_bytes)
        .map_err(|_| UpdateError::Invalid("manifest signature has an invalid length"))?;
    public_key
        .verify_strict(manifest_bytes, &signature)
        .map_err(|_| UpdateError::BadSignature)?;
    let manifest: UpdateManifest =
        serde_json::from_slice(manifest_bytes).map_err(UpdateError::Json)?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
}

pub fn verify_installer_bytes(
    manifest: &UpdateManifest,
    installer: &[u8],
) -> Result<(), UpdateError> {
    manifest.validate()?;
    if installer.len() as u64 != manifest.installer_size_bytes {
        return Err(UpdateError::InstallerSizeMismatch);
    }
    if sha256_hex(installer) != manifest.installer_sha256 {
        return Err(UpdateError::InstallerHashMismatch);
    }
    Ok(())
}

pub fn verify_installer_path(
    manifest: &UpdateManifest,
    installer_path: &Path,
) -> Result<(), UpdateError> {
    manifest.validate()?;
    let mut file = File::open(installer_path).map_err(UpdateError::Io)?;
    let file_size = file.metadata().map_err(UpdateError::Io)?.len();
    if file_size != manifest.installer_size_bytes {
        return Err(UpdateError::InstallerSizeMismatch);
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(UpdateError::Io)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if encode_hex(&hasher.finalize()) != manifest.installer_sha256 {
        return Err(UpdateError::InstallerHashMismatch);
    }
    Ok(())
}

fn validate_repository(repository: &str) -> Result<(), UpdateError> {
    if repository != "NcaMoq/OpenShazam" {
        return Err(UpdateError::Invalid("unexpected GitHub repository"));
    }
    let mut parts = repository.split('/');
    let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err(UpdateError::Invalid("repository must be owner/name"));
    };
    let valid_segment = |segment: &str| {
        !segment.is_empty()
            && segment.len() <= 100
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    };
    if !valid_segment(owner) || !valid_segment(repo) {
        return Err(UpdateError::Invalid(
            "repository contains unsupported characters",
        ));
    }
    Ok(())
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct StableVersion(u64, u64, u64);

impl StableVersion {
    fn parse(value: &str) -> Result<Self, UpdateError> {
        let mut pieces = value.split('.');
        let parse_part = |part: Option<&str>| -> Result<u64, UpdateError> {
            let part = part.ok_or(UpdateError::Invalid(
                "version must use stable MAJOR.MINOR.PATCH",
            ))?;
            if part.is_empty()
                || (part.len() > 1 && part.starts_with('0'))
                || !part.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Err(UpdateError::Invalid(
                    "version must use stable MAJOR.MINOR.PATCH",
                ));
            }
            part.parse()
                .map_err(|_| UpdateError::Invalid("version component is too large"))
        };
        let version = Self(
            parse_part(pieces.next())?,
            parse_part(pieces.next())?,
            parse_part(pieces.next())?,
        );
        if pieces.next().is_some() {
            return Err(UpdateError::Invalid(
                "version must use stable MAJOR.MINOR.PATCH",
            ));
        }
        Ok(version)
    }
}

impl fmt::Display for StableVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Debug)]
pub enum UpdateError {
    Invalid(&'static str),
    BadSignature,
    InstallerSizeMismatch,
    InstallerHashMismatch,
    Json(serde_json::Error),
    Io(std::io::Error),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::BadSignature => f.write_str("update manifest signature verification failed"),
            Self::InstallerSizeMismatch => {
                f.write_str("installer length does not match the signed manifest")
            }
            Self::InstallerHashMismatch => {
                f.write_str("installer SHA-256 does not match the signed manifest")
            }
            Self::Json(error) => write!(f, "invalid update manifest JSON: {error}"),
            Self::Io(error) => write!(f, "could not verify installer: {error}"),
        }
    }
}

impl std::error::Error for UpdateError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed_fixture() -> (Vec<u8>, String, SigningKey, Vec<u8>) {
        let key = SigningKey::from_bytes(&[7; 32]);
        let installer = b"inno setup fixture".to_vec();
        let manifest = create_manifest(&installer, "0.1.1", "0.1.0", "NcaMoq/OpenShazam").unwrap();
        let bytes = manifest.signed_bytes().unwrap();
        let signature = sign_manifest(&manifest, &key).unwrap();
        (bytes, signature, key, installer)
    }

    #[test]
    fn signed_manifest_and_installer_are_verified() {
        let (bytes, signature, key, installer) = signed_fixture();
        let manifest = verify_signed_manifest(&bytes, &signature, &key.verifying_key()).unwrap();
        assert!(manifest.is_newer_than("0.1.0").unwrap());
        assert_eq!(
            manifest.installer_url().unwrap(),
            "https://github.com/NcaMoq/OpenShazam/releases/download/v0.1.1/Shazam-Setup-0.1.1-x64.exe"
        );
        verify_installer_bytes(&manifest, &installer).unwrap();
    }

    #[test]
    fn changed_manifest_or_installer_is_rejected() {
        let (mut bytes, signature, key, _) = signed_fixture();
        let index = bytes.iter().position(|byte| *byte == b'1').unwrap();
        bytes[index] = b'2';
        assert!(matches!(
            verify_signed_manifest(&bytes, &signature, &key.verifying_key()),
            Err(UpdateError::BadSignature)
        ));

        let (bytes, signature, key, mut installer) = signed_fixture();
        let manifest = verify_signed_manifest(&bytes, &signature, &key.verifying_key()).unwrap();
        installer[0] ^= 0x01;
        assert!(matches!(
            verify_installer_bytes(&manifest, &installer),
            Err(UpdateError::InstallerHashMismatch)
        ));
    }

    #[test]
    fn manifest_rejects_untrusted_urls_wrong_platform_and_rollback() {
        let (bytes, signature, key, _) = signed_fixture();
        let mut value: UpdateManifest = serde_json::from_slice(&bytes).unwrap();
        value.release_url = "https://attacker.example/installer.exe".to_owned();
        assert!(value.validate().is_err());
        value.release_url = "https://github.com/NcaMoq/OpenShazam/releases/tag/v0.1.1".to_owned();
        value.architecture = "arm64".to_owned();
        assert!(value.validate().is_err());
        assert!(
            !verify_signed_manifest(&bytes, &signature, &key.verifying_key())
                .unwrap()
                .is_newer_than("0.1.1")
                .unwrap()
        );
    }

    #[test]
    fn noncanonical_and_prerelease_versions_are_rejected() {
        assert!(StableVersion::parse("0.01.0").is_err());
        assert!(StableVersion::parse("0.1.0-rc.1").is_err());
        assert!(StableVersion::parse("0.1.0.1").is_err());
    }

    #[test]
    fn trusted_public_key_and_base64_seed_are_parsed() {
        assert!(embedded_public_key().is_ok());
        let encoded = STANDARD.encode([7_u8; 32]);
        let key = signing_key_from_base64(&encoded).unwrap();
        assert_eq!(
            public_key_hex(&key),
            public_key_hex(&SigningKey::from_bytes(&[7; 32]))
        );
        assert!(signing_key_from_base64("not base64!").is_err());
    }

    #[test]
    fn installer_path_verification_checks_streamed_hash_and_length() {
        let bytes = b"installer on disk";
        let (manifest_bytes, signature, key, _) = signed_fixture();
        let mut manifest: UpdateManifest = serde_json::from_slice(&manifest_bytes).unwrap();
        manifest.installer_size_bytes = bytes.len() as u64;
        manifest.installer_sha256 = sha256_hex(bytes);
        let signed_bytes = manifest.signed_bytes().unwrap();
        let signed = sign_manifest(&manifest, &key).unwrap();
        let manifest =
            verify_signed_manifest(&signed_bytes, &signed, &key.verifying_key()).unwrap();

        let path = std::env::temp_dir().join(format!(
            "openshazam-update-verify-{}-{}.bin",
            std::process::id(),
            signature.len()
        ));
        std::fs::write(&path, bytes).unwrap();
        verify_installer_path(&manifest, &path).unwrap();
        std::fs::write(&path, b"modified installer").unwrap();
        assert!(verify_installer_path(&manifest, &path).is_err());
        let _ = std::fs::remove_file(path);
    }
}
