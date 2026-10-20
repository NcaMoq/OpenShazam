//! User initiated, signature-verified update acquisition for the unpackaged
//! Windows application. Network work is intended to run on a worker thread.

use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use openshazam_update::{
    embedded_public_key, verify_installer_path, verify_signed_manifest, UpdateManifest,
    LATEST_MANIFEST_URL, LATEST_SIGNATURE_URL,
};
use std::os::windows::process::CommandExt;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 1024;
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_REDIRECTS: &str = "5";
static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub(crate) struct UpdateCandidate {
    pub(crate) manifest: UpdateManifest,
    manifest_bytes: Vec<u8>,
    signature: String,
}

pub(crate) fn check_for_update(current_version: &str) -> Result<Option<UpdateCandidate>, String> {
    let work_dir = update_directory()?;
    let manifest_path = unique_temp_path(&work_dir, "manifest");
    let signature_path = unique_temp_path(&work_dir, "signature");
    let result = (|| {
        download_to_path(LATEST_MANIFEST_URL, &manifest_path, MAX_MANIFEST_BYTES)?;
        download_to_path(LATEST_SIGNATURE_URL, &signature_path, MAX_SIGNATURE_BYTES)?;
        let manifest_bytes = fs::read(&manifest_path)
            .map_err(|error| format!("Could not read update manifest: {error}"))?;
        if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err("Update manifest is larger than the allowed limit.".to_owned());
        }
        let signature = fs::read_to_string(&signature_path)
            .map_err(|error| format!("Could not read update signature: {error}"))?;
        let key = embedded_public_key().map_err(|error| error.to_string())?;
        let manifest = verify_signed_manifest(&manifest_bytes, &signature, &key)
            .map_err(|error| error.to_string())?;
        if manifest
            .is_newer_than(current_version)
            .map_err(|error| error.to_string())?
        {
            Ok(Some(UpdateCandidate {
                manifest,
                manifest_bytes,
                signature,
            }))
        } else {
            Ok(None)
        }
    })();
    let _ = fs::remove_file(manifest_path);
    let _ = fs::remove_file(signature_path);
    result
}

pub(crate) fn download_verified_installer(candidate: &UpdateCandidate) -> Result<PathBuf, String> {
    let key = embedded_public_key().map_err(|error| error.to_string())?;
    let manifest = verify_signed_manifest(&candidate.manifest_bytes, &candidate.signature, &key)
        .map_err(|error| error.to_string())?;
    if manifest != candidate.manifest {
        return Err("Update metadata changed after the release signature was verified.".to_owned());
    }
    let directory = update_directory()?;
    let output_dir = unique_download_directory(&directory);
    fs::create_dir(&output_dir)
        .map_err(|error| format!("Could not prepare the update installer folder: {error}"))?;
    let output = output_dir.join(&manifest.installer_name);
    let url = manifest
        .installer_url()
        .map_err(|error| error.to_string())?;
    let download_result = download_to_path(
        &url,
        &output,
        manifest.installer_size_bytes.min(MAX_INSTALLER_BYTES),
    );
    if let Err(error) = download_result {
        let _ = fs::remove_file(&output);
        return Err(error);
    }
    if let Err(error) = verify_installer_path(&manifest, &output) {
        let _ = fs::remove_file(&output);
        return Err(error.to_string());
    }
    Ok(output)
}

pub(crate) fn start_installer(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Err("The verified update installer is missing.".to_owned());
    }
    Command::new(path)
        .current_dir(path.parent().unwrap_or_else(|| Path::new(".")))
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("Could not start the update installer: {error}"))
}

fn download_to_path(url: &str, path: &Path, max_bytes: u64) -> Result<(), String> {
    let output = Command::new("curl.exe")
        .arg("--fail")
        .arg("--silent")
        .arg("--show-error")
        .arg("--location")
        .arg("--proto")
        .arg("=https")
        .arg("--proto-redir")
        .arg("=https")
        .arg("--max-redirs")
        .arg(MAX_REDIRECTS)
        .arg("--connect-timeout")
        .arg("10")
        .arg("--max-time")
        .arg("60")
        .arg("--max-filesize")
        .arg(max_bytes.to_string())
        .arg("--output")
        .arg(path)
        .arg("--write-out")
        .arg("%{url_effective}")
        .arg(url)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| format!("Could not start the Windows HTTPS client: {error}"))?;
    if !output.status.success() {
        let _ = fs::remove_file(path);
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Update download failed: {}", detail.trim()));
    }
    let effective_url = String::from_utf8_lossy(&output.stdout);
    if !is_allowed_github_release_url(effective_url.trim()) {
        let _ = fs::remove_file(path);
        return Err("The update server redirected to an untrusted host.".to_owned());
    }
    let length = fs::metadata(path)
        .map_err(|error| format!("Could not inspect downloaded update data: {error}"))?
        .len();
    if length == 0 || length > max_bytes {
        let _ = fs::remove_file(path);
        return Err("Downloaded update data has an invalid length.".to_owned());
    }
    Ok(())
}

fn is_allowed_github_release_url(value: &str) -> bool {
    let Some(authority_and_path) = value.strip_prefix("https://") else {
        return false;
    };
    let authority = authority_and_path.split('/').next().unwrap_or_default();
    matches!(
        authority.to_ascii_lowercase().as_str(),
        "github.com" | "release-assets.githubusercontent.com" | "objects.githubusercontent.com"
    )
}

fn update_directory() -> Result<PathBuf, String> {
    let local_app_data = env::var_os("LOCALAPPDATA").ok_or_else(|| {
        "Windows did not provide the current user's Local AppData folder.".to_owned()
    })?;
    let directory = PathBuf::from(local_app_data)
        .join("OpenShazam")
        .join("Updates");
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create the update download folder: {error}"))?;
    Ok(directory)
}

fn unique_temp_path(directory: &Path, name: &str) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let sequence = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
    directory.join(format!(
        ".{name}.{}-{timestamp}-{sequence}.tmp",
        std::process::id()
    ))
}

fn unique_download_directory(directory: &Path) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let sequence = NEXT_FILE_ID.fetch_add(1, Ordering::Relaxed);
    directory.join(format!(
        "download-{}-{timestamp}-{sequence}",
        std::process::id()
    ))
}
