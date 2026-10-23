//! Signature-verified update acquisition for the unpackaged
//! Windows application. Network work is intended to run on a worker thread.
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(clippy::undocumented_unsafe_blocks)]

use std::{
    env, fs,
    io::{Read, Write},
    os::windows::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt},
        io::{FromRawHandle, RawHandle},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::Mutex,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use openshazam_update::{
    embedded_public_key, verify_installer_reader, verify_signed_manifest, UpdateManifest,
    LATEST_MANIFEST_URL, LATEST_SIGNATURE_URL,
};
use std::os::windows::process::CommandExt;
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Storage::FileSystem::{
            CreateFileW, FileAttributeTagInfo, FileBasicInfo, FileDispositionInfo,
            GetFileInformationByHandleEx, SetFileInformationByHandle, DELETE,
            FILE_ATTRIBUTE_DIRECTORY,
            FILE_ATTRIBUTE_REPARSE_POINT as WIN_FILE_ATTRIBUTE_REPARSE_POINT,
            FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO, FILE_DISPOSITION_INFO,
            FILE_FLAGS_AND_ATTRIBUTES, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_READ_DATA, FILE_SHARE_MODE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
    },
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 1024;
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PENDING_UPDATE_BYTES: u64 = 256;
const MAX_HTTPS_CLIENT_OUTPUT_BYTES: usize = 8 * 1024;
const DOWNLOAD_URL_TRAILER: &[u8] = b"\nOPEN_SHAZAM_EFFECTIVE_URL:";
const DOWNLOAD_REDIRECT_URL_TRAILER: &[u8] = b"\nOPEN_SHAZAM_REDIRECT_URL:";
const DOWNLOAD_HTTP_STATUS_TRAILER: &[u8] = b"\nOPEN_SHAZAM_HTTP_STATUS:";
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
const MAX_REDIRECTS: usize = 5;
const MAX_REDIRECT_RESPONSE_BYTES: u64 = 64 * 1024;
const MAX_DOWNLOAD_ATTEMPTS: usize = 3;
static NEXT_FILE_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn try_begin_update_operation(active: &mut bool) -> bool {
    if *active {
        false
    } else {
        *active = true;
        true
    }
}

pub(crate) fn automatic_download_should_be_discarded(
    automatic: bool,
    automatic_updates_enabled: bool,
) -> bool {
    automatic && !automatic_updates_enabled
}

fn automatic_install_should_be_cancelled(automatic: bool, automatic_updates_enabled: bool) -> bool {
    automatic && !automatic_updates_enabled
}

#[derive(Clone, Debug)]
pub(crate) struct UpdateCandidate {
    pub(crate) manifest: UpdateManifest,
    manifest_bytes: Vec<u8>,
    signature: String,
}

pub(crate) fn check_for_update(
    current_version: &str,
    cancelled: &AtomicBool,
) -> Result<Option<UpdateCandidate>, String> {
    let work_dir = update_directory()?;
    let _directory_chain = lock_directory_chain(&work_dir)?;
    let manifest_path = unique_temp_path(&work_dir, "manifest");
    let signature_path = unique_temp_path(&work_dir, "signature");
    let result = (|| {
        download_to_path(
            LATEST_MANIFEST_URL,
            &manifest_path,
            MAX_MANIFEST_BYTES,
            cancelled,
        )?;
        download_to_path(
            LATEST_SIGNATURE_URL,
            &signature_path,
            MAX_SIGNATURE_BYTES,
            cancelled,
        )?;
        let manifest_file = open_guarded_read_file(&manifest_path)?;
        let mut manifest_bytes = Vec::new();
        manifest_file
            .take(MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut manifest_bytes)
            .map_err(|error| format!("Could not read update manifest: {error}"))?;
        if manifest_bytes.len() as u64 > MAX_MANIFEST_BYTES {
            return Err("Update manifest is larger than the allowed limit.".to_owned());
        }
        let signature_file = open_guarded_read_file(&signature_path)?;
        let mut signature_bytes = Vec::new();
        signature_file
            .take(MAX_SIGNATURE_BYTES + 1)
            .read_to_end(&mut signature_bytes)
            .map_err(|error| format!("Could not read update signature: {error}"))?;
        if signature_bytes.len() as u64 > MAX_SIGNATURE_BYTES {
            return Err("Update signature is larger than the allowed limit.".to_owned());
        }
        let signature = String::from_utf8(signature_bytes)
            .map_err(|_| "The update signature was not valid UTF-8.".to_owned())?;
        let key = embedded_public_key().map_err(|error| error.to_string())?;
        let manifest = verify_signed_manifest(&manifest_bytes, &signature, &key)
            .map_err(|error| error.to_string())?;
        if cancelled.load(Ordering::Acquire) {
            return Err("The update check was cancelled.".to_owned());
        }
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
    remove_file_by_handle_at_path(&manifest_path);
    remove_file_by_handle_at_path(&signature_path);
    result
}

pub(crate) fn download_verified_installer(
    candidate: &UpdateCandidate,
    cancelled: &AtomicBool,
) -> Result<PathBuf, String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("The update download was cancelled.".to_owned());
    }
    let key = embedded_public_key().map_err(|error| error.to_string())?;
    let manifest = verify_signed_manifest(&candidate.manifest_bytes, &candidate.signature, &key)
        .map_err(|error| error.to_string())?;
    if manifest != candidate.manifest {
        return Err("Update metadata changed after the release signature was verified.".to_owned());
    }
    let url = manifest
        .installer_url()
        .map_err(|error| error.to_string())?;
    let directory = update_directory()?;
    let _directory_chain = lock_directory_chain(&directory)?;
    let output_dir = unique_download_directory(&directory);
    fs::create_dir(&output_dir)
        .map_err(|error| format!("Could not prepare the update installer folder: {error}"))?;
    let output = output_dir.join(&manifest.installer_name);
    let download_directory_handle = match open_guarded_directory(&output_dir) {
        Ok(handle) => handle,
        Err(error) => {
            return Err(format!(
                "Could not secure the update installer folder: {error}"
            ));
        }
    };
    let download_result = download_to_path(
        &url,
        &output,
        manifest.installer_size_bytes.min(MAX_INSTALLER_BYTES),
        cancelled,
    );
    if let Err(error) = download_result {
        drop(download_directory_handle);
        let _ = remove_staged_installer_at(&directory, &output, &manifest.installer_name);
        return Err(error);
    }
    let mut installer_file = match open_guarded_read_file(&output) {
        Ok(file) => file,
        Err(error) => {
            drop(download_directory_handle);
            let _ = remove_staged_installer_at(&directory, &output, &manifest.installer_name);
            return Err(format!(
                "Could not reopen the downloaded installer safely: {error}"
            ));
        }
    };
    if let Err(error) = verify_installer_reader(&manifest, &mut installer_file) {
        drop(installer_file);
        drop(download_directory_handle);
        let _ = remove_staged_installer_at(&directory, &output, &manifest.installer_name);
        return Err(error.to_string());
    }
    if cancelled.load(Ordering::Acquire) {
        drop(installer_file);
        drop(download_directory_handle);
        let _ = remove_staged_installer_at(&directory, &output, &manifest.installer_name);
        return Err("The update download was cancelled.".to_owned());
    }
    drop(installer_file);
    drop(download_directory_handle);
    Ok(output)
}

pub(crate) fn remove_staged_installer(path: &Path, expected_name: &str) -> Result<(), String> {
    let directory = update_directory()?;
    remove_staged_installer_at(&directory, path, expected_name)
}

fn remove_staged_installer_at(
    directory: &Path,
    path: &Path,
    expected_name: &str,
) -> Result<(), String> {
    if !is_versioned_installer_name(expected_name)
        || path.file_name() != Some(std::ffi::OsStr::new(expected_name))
    {
        return Err("Refusing to remove an unexpected update installer.".to_owned());
    }

    let canonical_directory = fs::canonicalize(directory)
        .map_err(|error| format!("Could not resolve the update folder: {error}"))?;
    if canonical_directory != directory {
        return Err("Refusing to remove an installer through an aliased update folder.".to_owned());
    }
    let _directory_chain = lock_directory_chain(directory)?;
    let download_directory = path
        .parent()
        .ok_or_else(|| "The staged installer did not have a parent folder.".to_owned())?;
    if download_directory.parent() != Some(directory) {
        return Err(
            "Refusing to remove an installer outside its private download folder.".to_owned(),
        );
    }
    let canonical_download_directory = fs::canonicalize(download_directory)
        .map_err(|error| format!("Could not resolve the staged installer folder: {error}"))?;
    if canonical_download_directory.parent() != Some(canonical_directory.as_path())
        || !is_download_directory_name(
            canonical_download_directory
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        )
    {
        return Err(
            "Refusing to remove an installer outside its private download folder.".to_owned(),
        );
    }

    let download_handle = open_guarded_directory_for_delete(download_directory)?;
    match open_guarded_regular_file(path) {
        Ok(installer_handle) => {
            delete_by_handle(&installer_handle)
                .map_err(|error| format!("Could not remove the staged installer: {error}"))?;
            drop(installer_handle);
        }
        Err(error) => {
            if !path_entry_is_missing(path)? {
                return Err(format!("Could not inspect the staged installer: {error}"));
            }
        }
    }

    let is_empty = fs::read_dir(download_directory)
        .map_err(|error| format!("Could not inspect the staged installer folder: {error}"))?
        .next()
        .is_none();
    if is_empty {
        // The directory handle pins this exact directory while it is marked for deletion.
        let _ = delete_by_handle(&download_handle);
    }
    Ok(())
}

fn verify_staged_installer_for_install(
    candidate: &UpdateCandidate,
    path: &Path,
    verify_manifest: impl FnOnce(&[u8], &str) -> Result<UpdateManifest, String>,
) -> Result<fs::File, String> {
    let manifest = verify_manifest(&candidate.manifest_bytes, &candidate.signature)?;
    if manifest != candidate.manifest
        || path.file_name() != Some(std::ffi::OsStr::new(&manifest.installer_name))
    {
        return Err("The installer does not match the signed update metadata.".to_owned());
    }
    let mut installer_file = open_guarded_read_file(path)?;
    verify_installer_reader(&manifest, &mut installer_file).map_err(|error| error.to_string())?;
    Ok(installer_file)
}

pub(crate) fn start_installer(
    candidate: &UpdateCandidate,
    path: &Path,
    automatic: bool,
    cancelled: &AtomicBool,
    automatic_updates_enabled: &AtomicBool,
    automatic_install_gate: &Mutex<()>,
) -> Result<bool, String> {
    if cancelled.load(Ordering::Acquire) {
        return Ok(false);
    }
    if automatic
        && automatic_install_should_be_cancelled(
            automatic,
            automatic_updates_enabled.load(Ordering::Acquire),
        )
    {
        return Ok(false);
    }
    let directory = update_directory()?;
    let _directory_chain = lock_directory_chain(&directory)?;
    let download_directory = path
        .parent()
        .ok_or_else(|| "The verified update installer has no download folder.".to_owned())?;
    if download_directory.parent() != Some(directory.as_path())
        || !is_download_directory_name(
            download_directory
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        )
    {
        return Err("The verified update installer is outside private update storage.".to_owned());
    }
    let _download_directory_handle = open_guarded_directory(download_directory)?;
    let key = embedded_public_key().map_err(|error| error.to_string())?;
    let _installer_file =
        verify_staged_installer_for_install(candidate, path, |bytes, signature| {
            verify_signed_manifest(bytes, signature, &key).map_err(|error| error.to_string())
        })?;
    if cancelled.load(Ordering::Acquire) {
        return Ok(false);
    }
    let pending_marker = write_pending_update_marker(&candidate.manifest.version)?;
    if automatic
        && automatic_install_should_be_cancelled(
            automatic,
            automatic_updates_enabled.load(Ordering::Acquire),
        )
    {
        remove_file_by_handle_at_path(&pending_marker);
        return Ok(false);
    }
    let mut command = Command::new(path);
    if automatic {
        command.args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"]);
    }
    let start_result = start_installer_process_if_enabled(
        automatic,
        cancelled,
        automatic_updates_enabled,
        automatic_install_gate,
        || {
            command
                .current_dir(path.parent().unwrap_or_else(|| Path::new(".")))
                .spawn()
                .map(|_| ())
                .map_err(|error| format!("Could not start the update installer: {error}"))
        },
    );
    finish_installer_launch(&pending_marker, start_result)
}

fn finish_installer_launch(
    pending_marker: &Path,
    start_result: Result<bool, String>,
) -> Result<bool, String> {
    match start_result {
        Ok(true) => Ok(true),
        Ok(false) => {
            remove_file_by_handle_at_path(pending_marker);
            Ok(false)
        }
        Err(error) => {
            remove_file_by_handle_at_path(pending_marker);
            Err(error)
        }
    }
}

fn start_installer_process_if_enabled(
    automatic: bool,
    cancelled: &AtomicBool,
    automatic_updates_enabled: &AtomicBool,
    automatic_install_gate: &Mutex<()>,
    start: impl FnOnce() -> Result<(), String>,
) -> Result<bool, String> {
    let _gate = automatic_install_gate
        .lock()
        .map_err(|_| "Could not synchronize the update installer setting.".to_owned())?;
    if cancelled.load(Ordering::Acquire)
        || (automatic
            && automatic_install_should_be_cancelled(
                automatic,
                automatic_updates_enabled.load(Ordering::Acquire),
            ))
    {
        return Ok(false);
    }
    start()?;
    Ok(true)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PendingUpdateResult {
    Installed { version: String },
    Failed { expected_version: String },
}

fn write_pending_update_marker(version: &str) -> Result<PathBuf, String> {
    let directory = update_directory()?;
    write_pending_update_marker_at(&directory, version)
}

fn write_pending_update_marker_at(directory: &Path, version: &str) -> Result<PathBuf, String> {
    if parse_stable_version(version).is_none() {
        return Err("The signed update manifest contains an invalid version.".to_owned());
    }
    let _directory_chain = lock_directory_chain(directory)?;
    let marker = directory.join("pending-update.txt");
    let temporary = unique_temp_path(directory, "pending-update");
    let contents = format!("OpenShazam pending update v1\n{version}\n");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(&temporary)
        .map_err(|error| format!("Could not create the update result marker: {error}"))?;
    if let Err(error) = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
    {
        drop(file);
        remove_file_by_handle_at_path(&temporary);
        return Err(format!(
            "Could not persist the update result marker: {error}"
        ));
    }
    drop(file);
    if let Err(error) = fs::rename(&temporary, &marker) {
        remove_file_by_handle_at_path(&temporary);
        return Err(format!(
            "Could not commit the update result marker: {error}"
        ));
    }
    Ok(marker)
}

pub(crate) fn consume_pending_update_result(
    current_version: &str,
) -> Result<Option<PendingUpdateResult>, String> {
    let directory = update_directory()?;
    consume_pending_update_result_at(&directory, current_version)
}

fn consume_pending_update_result_at(
    directory: &Path,
    current_version: &str,
) -> Result<Option<PendingUpdateResult>, String> {
    let _directory_chain = lock_directory_chain(directory)?;
    let marker = directory.join("pending-update.txt");
    let marker_file = match open_guarded_read_file(&marker) {
        Ok(file) => file,
        Err(error) => {
            if path_entry_is_missing(&marker)? {
                return Ok(None);
            }
            return Err(format!(
                "Could not inspect the update result marker: {error}"
            ));
        }
    };
    if marker_file
        .metadata()
        .map_err(|error| format!("Could not inspect the update result marker: {error}"))?
        .len()
        > MAX_PENDING_UPDATE_BYTES
    {
        drop(marker_file);
        remove_file_by_handle_at_path(&marker);
        return Err(
            "The update result marker was larger than the allowed size and was cleared.".to_owned(),
        );
    }
    let mut content_bytes = Vec::new();
    marker_file
        .take(MAX_PENDING_UPDATE_BYTES + 1)
        .read_to_end(&mut content_bytes)
        .map_err(|error| format!("Could not read the update result marker: {error}"))?;
    if content_bytes.len() as u64 > MAX_PENDING_UPDATE_BYTES {
        remove_file_by_handle_at_path(&marker);
        return Err(
            "The update result marker was larger than the allowed size and was cleared.".to_owned(),
        );
    }
    let contents = match String::from_utf8(content_bytes) {
        Ok(contents) => contents,
        Err(_) => {
            remove_file_by_handle_at_path(&marker);
            return Err("The update result marker was not valid UTF-8 and was cleared.".to_owned());
        }
    };
    // `take` moved and closed the guarded handle before deleting this exact file.
    remove_file_by_handle_at_path(&marker);
    let mut lines = contents.lines();
    if lines.next() != Some("OpenShazam pending update v1") {
        return Err("The update result marker has an unsupported format.".to_owned());
    }
    let expected_version = lines
        .next()
        .filter(|version| parse_stable_version(version).is_some())
        .ok_or_else(|| "The update result marker contains an invalid version.".to_owned())?;
    if lines.next().is_some() {
        return Err("The update result marker has unexpected data.".to_owned());
    }
    if version_at_least(current_version, expected_version) {
        Ok(Some(PendingUpdateResult::Installed {
            version: expected_version.to_owned(),
        }))
    } else {
        Ok(Some(PendingUpdateResult::Failed {
            expected_version: expected_version.to_owned(),
        }))
    }
}

pub(crate) fn cleanup_obsolete_update_artifacts() -> Result<usize, String> {
    let directory = update_directory()?;
    cleanup_obsolete_update_artifacts_at(&directory, SystemTime::now())
}

fn cleanup_obsolete_update_artifacts_at(
    directory: &Path,
    now: SystemTime,
) -> Result<usize, String> {
    let canonical_directory = fs::canonicalize(directory)
        .map_err(|error| format!("Could not resolve the update folder: {error}"))?;
    if canonical_directory != directory {
        return Err("Refusing to clean an update folder through an aliased path.".to_owned());
    }
    let _directory_chain = lock_directory_chain(directory)?;
    let entries = fs::read_dir(&canonical_directory)
        .map_err(|error| format!("Could not list the update folder: {error}"))?;
    let minimum_age = Duration::from_secs(7 * 24 * 60 * 60);
    let mut removed = 0;

    for entry in entries {
        let entry = entry.map_err(|error| format!("Could not inspect an update file: {error}"))?;
        let path = entry.path();
        if path.parent() != Some(canonical_directory.as_path()) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let temporary_file = is_generated_temp_file_name(&name);
        let downloaded_directory = is_download_directory_name(&name);
        if !temporary_file && !downloaded_directory {
            continue;
        }
        if temporary_file {
            let Ok(file) = open_guarded_regular_file(&path) else {
                continue;
            };
            if handle_is_at_least_age(&file, now, minimum_age).unwrap_or(false)
                && delete_by_handle(&file).is_ok()
            {
                removed += 1;
            }
            continue;
        }
        let Ok(download_directory) = open_guarded_directory_for_delete(&path) else {
            continue;
        };

        let children = match fs::read_dir(&path) {
            Ok(children) => children,
            Err(_) => continue,
        };
        let mut safe_to_remove_directory = true;
        let mut removed_from_directory = 0;
        for child in children {
            let child = match child {
                Ok(child) => child,
                Err(_) => {
                    safe_to_remove_directory = false;
                    break;
                }
            };
            let child_path = child.path();
            if child_path.parent() != Some(path.as_path()) {
                safe_to_remove_directory = false;
                break;
            }
            let child_name = child.file_name();
            let child_name = child_name.to_string_lossy();
            if !is_versioned_installer_name(&child_name) {
                safe_to_remove_directory = false;
                break;
            }
            let Ok(child_file) = open_guarded_regular_file(&child_path) else {
                safe_to_remove_directory = false;
                break;
            };
            if !handle_is_at_least_age(&child_file, now, minimum_age).unwrap_or(false)
                || delete_by_handle(&child_file).is_err()
            {
                safe_to_remove_directory = false;
                break;
            }
            drop(child_file);
            removed_from_directory += 1;
        }
        if safe_to_remove_directory && delete_by_handle(&download_directory).is_ok() {
            removed += removed_from_directory + 1;
        }
    }
    Ok(removed)
}

struct GuardedHandle(HANDLE);

impl Drop for GuardedHandle {
    fn drop(&mut self) {
        // SAFETY: `GuardedHandle` is the unique owner of this live handle from
        // `CreateFileW`; it is not `Copy` or `Clone`, so this is its only close.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

fn lock_directory_chain(directory: &Path) -> Result<Vec<GuardedHandle>, String> {
    let canonical = fs::canonicalize(directory)
        .map_err(|error| format!("Could not resolve the update directory: {error}"))?;
    if canonical != directory {
        return Err("Refusing to lock an aliased update directory.".to_owned());
    }
    let mut ancestors: Vec<_> = directory
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .collect();
    ancestors.reverse();
    let mut handles = Vec::with_capacity(ancestors.len());
    for ancestor in ancestors {
        handles.push(open_guarded_directory(ancestor)?);
    }
    Ok(handles)
}

fn remove_file_by_handle_at_path(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    let Ok(_parent_chain) = lock_directory_chain(parent) else {
        return;
    };
    if let Ok(file) = open_guarded_regular_file(path) {
        let _ = delete_by_handle(&file);
    }
}

fn open_guarded_directory(path: &Path) -> Result<GuardedHandle, String> {
    open_guarded_directory_with_access(path, FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0)
}

fn open_guarded_directory_for_delete(path: &Path) -> Result<GuardedHandle, String> {
    open_guarded_directory_with_access(
        path,
        DELETE.0 | FILE_LIST_DIRECTORY.0 | FILE_READ_ATTRIBUTES.0,
    )
}

fn open_guarded_directory_with_access(path: &Path, access: u32) -> Result<GuardedHandle, String> {
    let handle = open_guarded_handle(
        path,
        access,
        FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0,
        FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0,
    )?;
    let attributes = handle_attributes(&handle)?;
    if attributes & FILE_ATTRIBUTE_DIRECTORY.0 == 0
        || attributes & WIN_FILE_ATTRIBUTE_REPARSE_POINT.0 != 0
    {
        return Err("Refusing to operate through a non-directory or reparse point.".to_owned());
    }
    Ok(handle)
}

fn open_guarded_regular_file(path: &Path) -> Result<GuardedHandle, String> {
    let handle = open_guarded_handle(
        path,
        DELETE.0 | FILE_READ_ATTRIBUTES.0,
        0,
        FILE_FLAG_OPEN_REPARSE_POINT.0,
    )?;
    let attributes = handle_attributes(&handle)?;
    if attributes & (FILE_ATTRIBUTE_DIRECTORY.0 | WIN_FILE_ATTRIBUTE_REPARSE_POINT.0) != 0 {
        return Err("Refusing to remove a reparse point or non-file entry.".to_owned());
    }
    Ok(handle)
}

fn open_guarded_read_file(path: &Path) -> Result<fs::File, String> {
    let handle = open_guarded_handle(
        path,
        FILE_READ_DATA.0 | FILE_READ_ATTRIBUTES.0,
        FILE_SHARE_READ.0,
        FILE_FLAG_OPEN_REPARSE_POINT.0,
    )?;
    let raw_handle = handle.0;
    std::mem::forget(handle);
    // SAFETY: `raw_handle` came from a successful `CreateFileW`, and ownership
    // was detached from `GuardedHandle` exactly once immediately above. The
    // returned `File` becomes its sole owner and closes it on drop.
    let file = unsafe { fs::File::from_raw_handle(raw_handle.0 as RawHandle) };
    let metadata = file
        .metadata()
        .map_err(|error| format!("Could not inspect a guarded update file: {error}"))?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("Refusing to read a reparse point or non-file update entry.".to_owned());
    }
    Ok(file)
}

fn open_guarded_handle(
    path: &Path,
    access: u32,
    share_mode: u32,
    flags: u32,
) -> Result<GuardedHandle, String> {
    let mut path_wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    path_wide.push(0);
    // SAFETY: `path_wide` is a valid NUL-terminated UTF-16 path and remains
    // alive for the synchronous call; the remaining arguments are values.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(path_wide.as_ptr()),
            access,
            FILE_SHARE_MODE(share_mode),
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(flags),
            None,
        )
    }
    .map_err(|error| format!("Could not open a guarded update path: {error}"))?;
    Ok(GuardedHandle(handle))
}

fn handle_attributes(handle: &GuardedHandle) -> Result<u32, String> {
    let mut information = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: `handle` owns an open kernel handle, and `information` points to
    // a writable buffer whose size is exactly the structure passed below.
    unsafe {
        GetFileInformationByHandleEx(
            handle.0,
            FileAttributeTagInfo,
            (&mut information as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
            std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
        )
    }
    .map_err(|error| format!("Could not inspect a guarded update path: {error}"))?;
    Ok(information.FileAttributes)
}

fn handle_is_at_least_age(
    handle: &GuardedHandle,
    now: SystemTime,
    minimum_age: Duration,
) -> Result<bool, String> {
    let mut information = FILE_BASIC_INFO::default();
    // SAFETY: `handle` owns an open kernel handle, and `information` points to
    // a writable buffer whose size is exactly the structure passed below.
    unsafe {
        GetFileInformationByHandleEx(
            handle.0,
            FileBasicInfo,
            (&mut information as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    }
    .map_err(|error| format!("Could not inspect update file age: {error}"))?;
    let Some(modified) = windows_filetime_to_system_time(information.LastWriteTime) else {
        return Ok(false);
    };
    Ok(is_at_least_age(Some(modified), now, minimum_age))
}

fn windows_filetime_to_system_time(filetime: i64) -> Option<SystemTime> {
    const WINDOWS_TO_UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
    let unix_ticks = filetime.checked_sub(WINDOWS_TO_UNIX_EPOCH_TICKS)?;
    if unix_ticks < 0 {
        return None;
    }
    UNIX_EPOCH.checked_add(Duration::from_nanos((unix_ticks as u64).checked_mul(100)?))
}

fn delete_by_handle(handle: &GuardedHandle) -> Result<(), String> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `handle` owns the file or directory opened with DELETE access;
    // `disposition` is a valid read-only structure for this synchronous call.
    unsafe {
        SetFileInformationByHandle(
            handle.0,
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    }
    .map_err(|error| format!("Could not mark the guarded update path for deletion: {error}"))
}

fn is_at_least_age(modified: Option<SystemTime>, now: SystemTime, minimum_age: Duration) -> bool {
    modified
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age >= minimum_age)
}

fn is_versioned_installer_name(name: &str) -> bool {
    name.strip_prefix("Shazam-Setup-")
        .and_then(|name| name.strip_suffix("-x64.exe"))
        .is_some_and(|version| parse_stable_version(version).is_some())
}

fn parse_stable_version(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.');
    let major = parse_version_component(parts.next()?)?;
    let minor = parse_version_component(parts.next()?)?;
    let patch = parse_version_component(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    Some([major, minor, patch])
}

fn parse_version_component(component: &str) -> Option<u64> {
    if component.is_empty()
        || (component.len() > 1 && component.starts_with('0'))
        || !component.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    component.parse().ok()
}

fn version_at_least(current: &str, expected: &str) -> bool {
    match (
        parse_stable_version(current),
        parse_stable_version(expected),
    ) {
        (Some(current), Some(expected)) => current >= expected,
        _ => false,
    }
}

fn download_to_path(
    url: &str,
    path: &Path,
    max_bytes: u64,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    download_with_partial_cleanup(
        path,
        cancelled,
        || download_to_path_once(url, path, max_bytes, cancelled),
        |delay| wait_for_download_retry(cancelled, delay),
    )
}

fn download_with_partial_cleanup(
    path: &Path,
    cancelled: &AtomicBool,
    mut attempt: impl FnMut() -> Result<(), DownloadAttemptError>,
    wait: impl FnMut(Duration) -> bool,
) -> Result<(), String> {
    download_with_retries(
        cancelled,
        || {
            let result = attempt();
            if result.is_err() {
                cleanup_partial_download(path)?;
            }
            result
        },
        wait,
    )
}

fn cleanup_partial_download(path: &Path) -> Result<(), DownloadAttemptError> {
    remove_file_by_handle_at_path(path);
    if !path_entry_is_missing(path).map_err(DownloadAttemptError::permanent)? {
        return Err(DownloadAttemptError::permanent(
            "Could not remove the incomplete update download.",
        ));
    }
    Ok(())
}

fn path_entry_is_missing(path: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(true),
        Err(error) => Err(format!("Could not inspect an update path: {error}")),
    }
}

fn download_to_path_once(
    url: &str,
    path: &Path,
    max_bytes: u64,
    cancelled: &AtomicBool,
) -> Result<(), DownloadAttemptError> {
    if cancelled.load(Ordering::Acquire) {
        return Err(DownloadAttemptError::permanent(
            "The update download was cancelled.",
        ));
    }
    let curl_program = shazam_http_lab::system_curl_program().map_err(|error| {
        DownloadAttemptError::permanent(format!(
            "Could not locate the Windows HTTPS client: {error}"
        ))
    })?;
    let mut request_url = url.to_owned();
    let mut redirects_followed = 0;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Err(DownloadAttemptError::permanent(
                "The update download was cancelled.",
            ));
        }
        if !is_allowed_github_release_url(&request_url) {
            return Err(DownloadAttemptError::permanent(
                "The update server redirected to an untrusted host.",
            ));
        }

        let mut command = Command::new(&curl_program);
        command
            .arg("--disable")
            .arg("--fail")
            .arg("--silent")
            .arg("--show-error")
            // Do not let curl follow a server-controlled redirect. The target
            // is validated below before it can become the next request URL.
            .arg("--proto")
            .arg("=https")
            .arg("--connect-timeout")
            .arg("10")
            .arg("--max-time")
            .arg("600")
            .arg("--max-filesize")
            .arg(max_bytes.to_string())
            .arg("--output")
            .arg("-")
            .arg("--write-out")
            .arg(
                "\nOPEN_SHAZAM_EFFECTIVE_URL:%{url_effective}\nOPEN_SHAZAM_REDIRECT_URL:%{redirect_url}\nOPEN_SHAZAM_HTTP_STATUS:%{http_code}\n",
            )
            .arg(&request_url)
            .creation_flags(CREATE_NO_WINDOW);
        let output = run_download_command_cancellable(command, cancelled, path, max_bytes)
            .map_err(DownloadAttemptError::permanent)?;

        if !is_allowed_github_release_url(output.effective_url.trim()) {
            return Err(DownloadAttemptError::permanent(
                "The update server redirected to an untrusted host.",
            ));
        }
        if is_transient_http_status(output.http_status) {
            return Err(DownloadAttemptError {
                message: format!(
                    "The update server temporarily returned HTTP {}.",
                    output.http_status
                ),
                retryable: true,
            });
        }
        if !output.status.success() {
            return Err(failed_download_attempt(
                output.status.code(),
                output.http_status,
            ));
        }

        if (300..400).contains(&output.http_status) {
            let Some(target) = validate_redirect_target(
                output.http_status,
                &output.redirect_url,
                redirects_followed,
                output.size_bytes,
            )?
            else {
                return Err(DownloadAttemptError::permanent(
                    "The update server returned an unexpected redirect response.",
                ));
            };
            cleanup_partial_download(path)?;
            request_url = target.to_owned();
            redirects_followed += 1;
            continue;
        }

        if !(200..300).contains(&output.http_status) || !output.redirect_url.is_empty() {
            return Err(DownloadAttemptError::permanent(
                "The update server returned an unexpected HTTP response.",
            ));
        }
        let downloaded_file =
            open_guarded_read_file(path).map_err(DownloadAttemptError::permanent)?;
        let length = downloaded_file
            .metadata()
            .map_err(|error| {
                DownloadAttemptError::permanent(format!(
                    "Could not inspect downloaded update data: {error}"
                ))
            })?
            .len();
        if length == 0 || length > max_bytes || length != output.size_bytes {
            drop(downloaded_file);
            return Err(DownloadAttemptError::permanent(
                "Downloaded update data has an invalid length.",
            ));
        }
        drop(downloaded_file);
        return Ok(());
    }
}

fn validate_redirect_target(
    status: u16,
    redirect_url: &str,
    redirects_followed: usize,
    response_bytes: u64,
) -> Result<Option<&str>, DownloadAttemptError> {
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return Ok(None);
    }
    if response_bytes > MAX_REDIRECT_RESPONSE_BYTES {
        return Err(DownloadAttemptError::permanent(
            "The update server returned an oversized redirect response.",
        ));
    }
    if redirects_followed >= MAX_REDIRECTS {
        return Err(DownloadAttemptError::permanent(
            "The update server returned too many redirects.",
        ));
    }
    let redirect_url = redirect_url.trim();
    if !is_allowed_github_release_url(redirect_url) {
        return Err(DownloadAttemptError::permanent(
            "The update server redirected to an untrusted host.",
        ));
    }
    Ok(Some(redirect_url))
}

#[derive(Debug)]
struct DownloadAttemptError {
    message: String,
    retryable: bool,
}

impl DownloadAttemptError {
    fn permanent(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

fn failed_download_attempt(exit_code: Option<i32>, http_status: u16) -> DownloadAttemptError {
    DownloadAttemptError {
        // HTTPS client diagnostics can contain signed redirect URLs. Keep the
        // user-facing error limited to the numeric HTTP status.
        message: format!("Update download failed with HTTP {http_status}."),
        retryable: is_transient_download_failure(exit_code, http_status),
    }
}

fn download_with_retries<T>(
    cancelled: &AtomicBool,
    mut attempt: impl FnMut() -> Result<T, DownloadAttemptError>,
    mut wait: impl FnMut(Duration) -> bool,
) -> Result<T, String> {
    for attempt_index in 0..MAX_DOWNLOAD_ATTEMPTS {
        if cancelled.load(Ordering::Acquire) {
            return Err("The update download was cancelled.".to_owned());
        }
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error) if error.retryable && attempt_index + 1 < MAX_DOWNLOAD_ATTEMPTS => {
                let delay = Duration::from_millis(500 * (1_u64 << attempt_index));
                if !wait(delay) || cancelled.load(Ordering::Acquire) {
                    return Err("The update download was cancelled.".to_owned());
                }
            }
            Err(error) => return Err(error.message),
        }
    }
    unreachable!("download retry loop always returns after its final attempt")
}

fn wait_for_download_retry(cancelled: &AtomicBool, delay: Duration) -> bool {
    let started = Instant::now();
    while started.elapsed() < delay {
        if cancelled.load(Ordering::Acquire) {
            return false;
        }
        let remaining = delay.saturating_sub(started.elapsed());
        thread::sleep(remaining.min(Duration::from_millis(50)));
    }
    !cancelled.load(Ordering::Acquire)
}

fn is_transient_download_failure(exit_code: Option<i32>, http_status: u16) -> bool {
    is_transient_http_status(http_status)
        || matches!(exit_code, Some(5 | 6 | 7 | 18 | 28 | 52 | 55 | 56 | 92))
}

fn is_transient_http_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504)
}

#[derive(Debug)]
struct DownloadFileOutput {
    effective_url: String,
    redirect_url: String,
    http_status: u16,
    size_bytes: u64,
}

#[derive(Debug)]
struct DownloadCommandOutput {
    status: std::process::ExitStatus,
    effective_url: String,
    redirect_url: String,
    http_status: u16,
    size_bytes: u64,
}

#[allow(dead_code)]
#[derive(Debug)]
enum DownloadCommandTiming {
    ChildSpawn(Duration),
    SizeLimitDetected(Duration),
    ChildKill(Duration),
    ChildWait(Duration),
    StdoutThreadJoin(Duration),
    StderrThreadJoin(Duration),
}

type DownloadCommandTimingSender = std::sync::mpsc::Sender<DownloadCommandTiming>;

#[cfg(test)]
fn read_limited_download(
    reader: impl Read,
    path: &Path,
    max_bytes: u64,
) -> Result<DownloadFileOutput, String> {
    read_limited_download_timed(reader, path, max_bytes, None, None)
}

fn read_limited_download_timed(
    reader: impl Read,
    path: &Path,
    max_bytes: u64,
    timing: Option<DownloadCommandTimingSender>,
    operation_started: Option<Instant>,
) -> Result<DownloadFileOutput, String> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ.0)
        .open(path)
        .map_err(|error| format!("Could not create the update download file: {error}"))?;
    read_limited_download_into_timed(reader, &mut file, max_bytes, timing, operation_started)
}

#[cfg(test)]
fn read_limited_download_into(
    reader: impl Read,
    file: &mut impl Write,
    max_bytes: u64,
) -> Result<DownloadFileOutput, String> {
    read_limited_download_into_timed(reader, file, max_bytes, None, None)
}

fn read_limited_download_into_timed(
    mut reader: impl Read,
    file: &mut impl Write,
    max_bytes: u64,
    timing: Option<DownloadCommandTimingSender>,
    operation_started: Option<Instant>,
) -> Result<DownloadFileOutput, String> {
    let mut tail = Vec::with_capacity(MAX_HTTPS_CLIENT_OUTPUT_BYTES + 4096);
    let mut buffer = [0; 4096];
    let mut written = 0u64;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| format!("Could not read the update download: {error}"))?;
        if count == 0 {
            break;
        }
        tail.extend_from_slice(&buffer[..count]);
        if tail.len() > MAX_HTTPS_CLIENT_OUTPUT_BYTES {
            let flush_length = tail.len() - MAX_HTTPS_CLIENT_OUTPUT_BYTES;
            let flush_bytes = u64::try_from(flush_length)
                .map_err(|_| "The update download size overflowed.".to_owned())?;
            if written
                .checked_add(flush_bytes)
                .is_none_or(|size| size > max_bytes)
            {
                if let Some(operation_started) = operation_started {
                    record_download_timing(
                        &timing,
                        DownloadCommandTiming::SizeLimitDetected(operation_started.elapsed()),
                    );
                }
                return Err("The update download exceeded its signed size limit.".to_owned());
            }
            file.write_all(&tail[..flush_length])
                .map_err(|error| format!("Could not write the update download: {error}"))?;
            written += flush_bytes;
            tail.drain(..flush_length);
        }
    }

    let marker_start = tail
        .windows(DOWNLOAD_URL_TRAILER.len())
        .rposition(|window| window == DOWNLOAD_URL_TRAILER)
        .ok_or_else(|| "The HTTPS client did not return its effective download URL.".to_owned())?;
    let url_start = marker_start + DOWNLOAD_URL_TRAILER.len();
    let trailer = &tail[url_start..];
    let redirect_marker_start = trailer
        .windows(DOWNLOAD_REDIRECT_URL_TRAILER.len())
        .rposition(|window| window == DOWNLOAD_REDIRECT_URL_TRAILER)
        .ok_or_else(|| "The HTTPS client did not return its redirect URL.".to_owned())?;
    let after_redirect = &trailer[redirect_marker_start + DOWNLOAD_REDIRECT_URL_TRAILER.len()..];
    let status_marker_start = after_redirect
        .windows(DOWNLOAD_HTTP_STATUS_TRAILER.len())
        .rposition(|window| window == DOWNLOAD_HTTP_STATUS_TRAILER)
        .ok_or_else(|| "The HTTPS client did not return its HTTP status.".to_owned())?;
    let effective_url_bytes = &trailer[..redirect_marker_start];
    let redirect_url_bytes = &after_redirect[..status_marker_start];
    let status_bytes = &after_redirect[status_marker_start + DOWNLOAD_HTTP_STATUS_TRAILER.len()..];
    if effective_url_bytes.is_empty()
        || effective_url_bytes.len() > MAX_HTTPS_CLIENT_OUTPUT_BYTES
        || effective_url_bytes
            .iter()
            .any(|byte| matches!(*byte, b'\r' | b'\n'))
        || redirect_url_bytes.len() > MAX_HTTPS_CLIENT_OUTPUT_BYTES
        || redirect_url_bytes
            .iter()
            .any(|byte| matches!(*byte, b'\r' | b'\n'))
        || status_bytes.len() != 4
        || status_bytes[3] != b'\n'
        || !status_bytes[..3].iter().all(u8::is_ascii_digit)
    {
        return Err("The HTTPS client returned malformed download metadata.".to_owned());
    }
    let effective_url = std::str::from_utf8(effective_url_bytes)
        .map_err(|_| "The HTTPS client returned a non-UTF-8 download URL.".to_owned())?
        .to_owned();
    let redirect_url = std::str::from_utf8(redirect_url_bytes)
        .map_err(|_| "The HTTPS client returned a non-UTF-8 redirect URL.".to_owned())?
        .to_owned();
    let http_status = std::str::from_utf8(&status_bytes[..3])
        .map_err(|_| "The HTTPS client returned an invalid HTTP status.".to_owned())?
        .parse::<u16>()
        .map_err(|_| "The HTTPS client returned an invalid HTTP status.".to_owned())?;
    let body_tail = &tail[..marker_start];
    let body_tail_size = u64::try_from(body_tail.len())
        .map_err(|_| "The update download size overflowed.".to_owned())?;
    let size_bytes = written
        .checked_add(body_tail_size)
        .ok_or_else(|| "The update download size overflowed.".to_owned())?;
    if size_bytes > max_bytes {
        return Err("The update download exceeded its signed size limit.".to_owned());
    }
    file.write_all(body_tail)
        .and_then(|()| file.flush())
        .map_err(|error| format!("Could not finish writing the update download: {error}"))?;

    Ok(DownloadFileOutput {
        effective_url,
        redirect_url,
        http_status,
        size_bytes,
    })
}

fn run_download_command_cancellable(
    command: Command,
    cancelled: &AtomicBool,
    path: &Path,
    max_bytes: u64,
) -> Result<DownloadCommandOutput, String> {
    run_download_command_cancellable_timed(command, cancelled, path, max_bytes, None)
}

fn run_download_command_cancellable_timed(
    mut command: Command,
    cancelled: &AtomicBool,
    path: &Path,
    max_bytes: u64,
    timing: Option<DownloadCommandTimingSender>,
) -> Result<DownloadCommandOutput, String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("The update operation was cancelled.".to_owned());
    }
    let operation_started = timing.as_ref().map(|_| Instant::now());
    let spawn_started = timing.as_ref().map(|_| Instant::now());
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not run the Windows HTTPS client: {error}"))?;
    if let Some(spawn_started) = spawn_started {
        record_download_timing(
            &timing,
            DownloadCommandTiming::ChildSpawn(spawn_started.elapsed()),
        );
    }
    let Some(stdout) = child.stdout.take() else {
        terminate_download_child(&mut child, &timing);
        return Err("The HTTPS client download pipe was not available.".to_owned());
    };
    let Some(stderr) = child.stderr.take() else {
        terminate_download_child(&mut child, &timing);
        return Err("The HTTPS client error pipe was not available.".to_owned());
    };
    let output_path = path.to_owned();
    let stdout_timing = timing.clone();
    let stdout_reader = match thread::Builder::new()
        .name("openshazam-update-download-output".to_owned())
        .spawn(move || {
            read_limited_download_timed(
                stdout,
                &output_path,
                max_bytes,
                stdout_timing,
                operation_started,
            )
        }) {
        Ok(reader) => reader,
        Err(error) => {
            terminate_download_child(&mut child, &timing);
            return Err(format!("Could not start the download writer: {error}"));
        }
    };
    let stderr_reader = match thread::Builder::new()
        .name("openshazam-update-stderr".to_owned())
        .spawn(move || drain_bounded_output(stderr, MAX_HTTPS_CLIENT_OUTPUT_BYTES))
    {
        Ok(reader) => reader,
        Err(error) => {
            terminate_download_child(&mut child, &timing);
            let _ = join_stdout_reader(stdout_reader, &timing);
            return Err(format!("Could not start the HTTPS error reader: {error}"));
        }
    };

    let mut stdout_reader = Some(stdout_reader);
    let mut downloaded = None;
    let status = loop {
        if cancelled.load(Ordering::Acquire) {
            terminate_download_child(&mut child, &timing);
            if let Some(reader) = stdout_reader.take() {
                let _ = join_stdout_reader(reader, &timing);
            }
            let _ = join_stderr_reader(stderr_reader, &timing);
            return Err("The update operation was cancelled.".to_owned());
        }
        if downloaded.is_none()
            && stdout_reader
                .as_ref()
                .is_some_and(thread::JoinHandle::is_finished)
        {
            let reader = stdout_reader.take().expect("finished reader is present");
            match join_stdout_reader(reader, &timing) {
                Ok(Ok(output)) => downloaded = Some(output),
                Ok(Err(error)) => {
                    terminate_download_child(&mut child, &timing);
                    let _ = join_stderr_reader(stderr_reader, &timing);
                    return Err(error);
                }
                Err(_) => {
                    terminate_download_child(&mut child, &timing);
                    let _ = join_stderr_reader(stderr_reader, &timing);
                    return Err("The HTTPS download writer stopped unexpectedly.".to_owned());
                }
            }
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                terminate_download_child(&mut child, &timing);
                if let Some(reader) = stdout_reader.take() {
                    let _ = join_stdout_reader(reader, &timing);
                }
                let _ = join_stderr_reader(stderr_reader, &timing);
                return Err(format!("Could not monitor the HTTPS client: {error}"));
            }
        }
    };
    let downloaded = match downloaded {
        Some(downloaded) => downloaded,
        None => match join_stdout_reader(
            stdout_reader
                .take()
                .expect("download writer is present until it finishes"),
            &timing,
        ) {
            Ok(Ok(downloaded)) => downloaded,
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err("The HTTPS download writer stopped unexpectedly.".to_owned()),
        },
    };
    let stderr_exceeded_limit = join_stderr_reader(stderr_reader, &timing)
        .map_err(|_| "The HTTPS error reader stopped unexpectedly.".to_owned())?
        .map_err(|error| format!("Could not read HTTPS client error output: {error}"))?;
    if stderr_exceeded_limit {
        return Err("The HTTPS client produced too much diagnostic output.".to_owned());
    }
    if cancelled.load(Ordering::Acquire) {
        return Err("The update operation was cancelled.".to_owned());
    }
    Ok(DownloadCommandOutput {
        status,
        effective_url: downloaded.effective_url,
        redirect_url: downloaded.redirect_url,
        http_status: downloaded.http_status,
        size_bytes: downloaded.size_bytes,
    })
}

fn record_download_timing(
    timing: &Option<DownloadCommandTimingSender>,
    measurement: DownloadCommandTiming,
) {
    if let Some(timing) = timing {
        let _ = timing.send(measurement);
    }
}

fn terminate_download_child(
    child: &mut std::process::Child,
    timing: &Option<DownloadCommandTimingSender>,
) {
    if timing.is_some() {
        let kill_started = Instant::now();
        let _ = child.kill();
        record_download_timing(
            timing,
            DownloadCommandTiming::ChildKill(kill_started.elapsed()),
        );
        let wait_started = Instant::now();
        let _ = child.wait();
        record_download_timing(
            timing,
            DownloadCommandTiming::ChildWait(wait_started.elapsed()),
        );
    } else {
        let _ = child.kill();
        let _ = child.wait();
    }
}

fn join_stderr_reader(
    reader: thread::JoinHandle<std::io::Result<bool>>,
    timing: &Option<DownloadCommandTimingSender>,
) -> thread::Result<std::io::Result<bool>> {
    if timing.is_none() {
        return reader.join();
    }
    let join_started = Instant::now();
    let result = reader.join();
    record_download_timing(
        timing,
        DownloadCommandTiming::StderrThreadJoin(join_started.elapsed()),
    );
    result
}

fn join_stdout_reader(
    reader: thread::JoinHandle<Result<DownloadFileOutput, String>>,
    timing: &Option<DownloadCommandTimingSender>,
) -> thread::Result<Result<DownloadFileOutput, String>> {
    if timing.is_none() {
        return reader.join();
    }
    let join_started = Instant::now();
    let result = reader.join();
    record_download_timing(
        timing,
        DownloadCommandTiming::StdoutThreadJoin(join_started.elapsed()),
    );
    result
}

fn drain_bounded_output(mut reader: impl Read, limit: usize) -> std::io::Result<bool> {
    let mut buffer = [0_u8; 1024];
    let mut total_bytes = 0_usize;
    let mut exceeded_limit = false;
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(count);
        exceeded_limit |= total_bytes > limit;
        buffer[..count].fill(0);
    }
    Ok(exceeded_limit)
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
    update_directory_at(&PathBuf::from(local_app_data))
}

fn update_directory_at(local_app_data: &Path) -> Result<PathBuf, String> {
    validate_update_directory(local_app_data)?;
    let canonical_local_app_data = fs::canonicalize(local_app_data)
        .map_err(|error| format!("Could not resolve the Local AppData folder: {error}"))?;

    let app_directory = local_app_data.join("OpenShazam");
    create_or_validate_update_directory(&app_directory)?;
    let canonical_app_directory = fs::canonicalize(&app_directory)
        .map_err(|error| format!("Could not resolve the OpenShazam data folder: {error}"))?;
    if canonical_app_directory.parent() != Some(canonical_local_app_data.as_path()) {
        return Err("Refusing to use an OpenShazam data folder outside Local AppData.".to_owned());
    }

    let update_directory = app_directory.join("Updates");
    create_or_validate_update_directory(&update_directory)?;
    let canonical_update_directory = fs::canonicalize(&update_directory)
        .map_err(|error| format!("Could not resolve the update download folder: {error}"))?;
    if canonical_update_directory.parent() != Some(canonical_app_directory.as_path()) {
        return Err("Refusing to use an update folder outside OpenShazam data.".to_owned());
    }
    Ok(canonical_update_directory)
}

fn create_or_validate_update_directory(path: &Path) -> Result<(), String> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("Could not create an update folder: {error}")),
    }
    validate_update_directory(path)
}

fn validate_update_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect an update folder: {error}"))?;
    if metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || !metadata.is_dir()
    {
        return Err("Refusing to use an update folder that is not a real directory.".to_owned());
    }
    Ok(())
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

fn is_download_directory_name(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("download-") else {
        return false;
    };
    is_generated_unique_suffix(suffix)
}

fn is_generated_temp_file_name(name: &str) -> bool {
    [".manifest.", ".signature.", ".pending-update."]
        .into_iter()
        .any(|prefix| {
            name.strip_prefix(prefix)
                .and_then(|suffix| suffix.strip_suffix(".tmp"))
                .is_some_and(is_generated_unique_suffix)
        })
}

fn is_generated_unique_suffix(suffix: &str) -> bool {
    let mut parts = suffix.split('-');
    let Some(process_id) = parts.next() else {
        return false;
    };
    let Some(timestamp) = parts.next() else {
        return false;
    };
    let Some(sequence) = parts.next() else {
        return false;
    };
    parts.next().is_none()
        && [process_id, timestamp, sequence]
            .into_iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::{
        automatic_download_should_be_discarded, automatic_install_should_be_cancelled,
        cleanup_obsolete_update_artifacts_at, cleanup_partial_download,
        consume_pending_update_result_at, download_with_partial_cleanup, download_with_retries,
        drain_bounded_output, failed_download_attempt, finish_installer_launch,
        is_allowed_github_release_url, is_download_directory_name, is_transient_download_failure,
        parse_stable_version, path_entry_is_missing, read_limited_download,
        read_limited_download_into, remove_staged_installer_at, run_download_command_cancellable,
        run_download_command_cancellable_timed, start_installer_process_if_enabled,
        try_begin_update_operation, update_directory_at, validate_redirect_target,
        verify_staged_installer_for_install, version_at_least, write_pending_update_marker_at,
        DownloadAttemptError, DownloadCommandTiming, PendingUpdateResult, UpdateCandidate,
        CREATE_NO_WINDOW, DOWNLOAD_HTTP_STATUS_TRAILER, DOWNLOAD_REDIRECT_URL_TRAILER,
        DOWNLOAD_URL_TRAILER, MAX_HTTPS_CLIENT_OUTPUT_BYTES,
    };
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::process::CommandExt;
    use std::{
        fs::{self, File, FileTimes, OpenOptions},
        io::{self, Cursor, Write},
        process::Command,
        sync::mpsc,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex,
        },
        thread,
        time::{Duration, SystemTime},
    };

    struct StorageFullWriter {
        file: File,
        remaining_bytes: usize,
    }

    impl Write for StorageFullWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.remaining_bytes == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    "simulated disk full",
                ));
            }
            let allowed = bytes.len().min(self.remaining_bytes);
            let written = self.file.write(&bytes[..allowed])?;
            self.remaining_bytes -= written;
            Ok(written)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.file.flush()
        }
    }

    #[test]
    fn update_result_version_requires_a_stable_three_part_version() {
        assert_eq!(parse_stable_version("0.1.1"), Some([0, 1, 1]));
        assert_eq!(parse_stable_version("1.20.300"), Some([1, 20, 300]));
        assert_eq!(parse_stable_version("01.1.1"), None);
        assert_eq!(parse_stable_version("0.1"), None);
        assert_eq!(parse_stable_version("0.1.1-preview"), None);
    }

    #[test]
    fn update_result_requires_the_expected_version_to_be_installed() {
        assert!(version_at_least("0.1.1", "0.1.1"));
        assert!(version_at_least("0.1.2", "0.1.1"));
        assert!(!version_at_least("0.1.0", "0.1.1"));
        assert!(!version_at_least("invalid", "0.1.1"));
    }

    #[test]
    fn pending_update_marker_reports_the_installed_version_and_failed_update() {
        let (root, directory) = create_test_update_directory();
        let marker = write_pending_update_marker_at(&directory, "0.1.2").unwrap();

        assert_eq!(
            consume_pending_update_result_at(&directory, "0.1.2").unwrap(),
            Some(PendingUpdateResult::Installed {
                version: "0.1.2".to_owned()
            })
        );
        assert!(!marker.exists());

        let marker = write_pending_update_marker_at(&directory, "0.1.3").unwrap();
        assert_eq!(
            consume_pending_update_result_at(&directory, "0.1.2").unwrap(),
            Some(PendingUpdateResult::Failed {
                expected_version: "0.1.3".to_owned()
            })
        );
        assert!(!marker.exists());
        assert_eq!(
            consume_pending_update_result_at(&directory, "0.1.3").unwrap(),
            None
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_installer_is_reverified_before_it_can_be_started() {
        const TEST_SIGNING_KEY_BASE64: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";
        let installer_bytes = b"verified staged installer bytes";
        let manifest = openshazam_update::create_manifest(
            installer_bytes,
            "0.1.2",
            "0.1.0",
            "NcaMoq/OpenShazam",
        )
        .unwrap();
        let signing_key = openshazam_update::signing_key_from_base64(TEST_SIGNING_KEY_BASE64)
            .expect("test key must decode");
        let public_key = signing_key.verifying_key();
        let candidate = UpdateCandidate {
            manifest_bytes: manifest.signed_bytes().unwrap(),
            signature: openshazam_update::sign_manifest(&manifest, &signing_key).unwrap(),
            manifest,
        };
        let verify_manifest = |bytes: &[u8], signature: &str| {
            openshazam_update::verify_signed_manifest(bytes, signature, &public_key)
                .map_err(|error| error.to_string())
        };
        let (root, _) = create_test_update_directory();
        let installer_path = root.join(&candidate.manifest.installer_name);
        fs::write(&installer_path, installer_bytes).unwrap();

        let verified_file =
            verify_staged_installer_for_install(&candidate, &installer_path, verify_manifest)
                .expect("the signed installer bytes must be accepted");
        assert_eq!(
            verified_file.metadata().unwrap().len(),
            installer_bytes.len() as u64
        );
        drop(verified_file);

        fs::write(&installer_path, vec![b'X'; installer_bytes.len()]).unwrap();
        let error =
            verify_staged_installer_for_install(&candidate, &installer_path, |bytes, signature| {
                openshazam_update::verify_signed_manifest(bytes, signature, &public_key)
                    .map_err(|error| error.to_string())
            })
            .unwrap_err();
        assert!(error.contains("SHA-256"));

        fs::write(&installer_path, installer_bytes).unwrap();
        let mut tampered_signature = candidate.clone();
        tampered_signature.signature.push('!');
        assert!(verify_staged_installer_for_install(
            &tampered_signature,
            &installer_path,
            |bytes, signature| {
                openshazam_update::verify_signed_manifest(bytes, signature, &public_key)
                    .map_err(|error| error.to_string())
            },
        )
        .is_err());

        let mut mismatched_candidate = candidate.clone();
        mismatched_candidate.manifest.version = "0.1.3".to_owned();
        assert!(verify_staged_installer_for_install(
            &mismatched_candidate,
            &installer_path,
            |bytes, signature| {
                openshazam_update::verify_signed_manifest(bytes, signature, &public_key)
                    .map_err(|error| error.to_string())
            },
        )
        .is_err());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installer_launch_failure_or_cancellation_removes_the_pending_marker() {
        for (result, expected) in [
            (
                Err("simulated spawn failure".to_owned()),
                Err("simulated spawn failure".to_owned()),
            ),
            (Ok(false), Ok(false)),
        ] {
            let (root, directory) = create_test_update_directory();
            let marker = directory.join("pending-update.txt");
            fs::write(&marker, "pending").unwrap();

            assert_eq!(finish_installer_launch(&marker, result), expected);
            assert!(!marker.exists());
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn successful_installer_spawn_keeps_the_marker_for_next_startup_verification() {
        let (root, directory) = create_test_update_directory();
        let marker = directory.join("pending-update.txt");
        fs::write(&marker, "pending").unwrap();

        assert_eq!(finish_installer_launch(&marker, Ok(true)), Ok(true));
        assert!(marker.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_pending_update_marker_is_cleared_and_reported() {
        let (root, directory) = create_test_update_directory();
        let marker = directory.join("pending-update.txt");
        fs::write(&marker, "not a pending update marker\n").unwrap();

        assert!(consume_pending_update_result_at(&directory, "0.1.2").is_err());
        assert!(!marker.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn non_utf8_pending_update_marker_is_cleared_and_reported() {
        let (root, directory) = create_test_update_directory();
        let marker = directory.join("pending-update.txt");
        fs::write(&marker, [0xff, 0xfe, 0x80]).unwrap();

        let error = consume_pending_update_result_at(&directory, "0.1.2").unwrap_err();
        assert!(error.contains("was cleared"));
        assert!(!marker.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_pending_update_marker_is_removed_for_recovery() {
        let (root, directory) = create_test_update_directory();
        let marker = directory.join("pending-update.txt");
        fs::write(&marker, vec![b'x'; 257]).unwrap();

        assert!(consume_pending_update_result_at(&directory, "0.1.2").is_err());
        assert!(!marker.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn turning_off_automatic_updates_discards_only_automatic_downloads() {
        assert!(automatic_download_should_be_discarded(true, false));
        assert!(!automatic_download_should_be_discarded(true, true));
        assert!(!automatic_download_should_be_discarded(false, false));
    }

    #[test]
    fn turning_off_automatic_updates_cancels_only_automatic_installation() {
        assert!(automatic_install_should_be_cancelled(true, false));
        assert!(!automatic_install_should_be_cancelled(true, true));
        assert!(!automatic_install_should_be_cancelled(false, false));
    }

    #[test]
    fn transient_download_failures_retry_with_a_bounded_backoff() {
        let cancelled = AtomicBool::new(false);
        let attempts = AtomicUsize::new(0);
        let mut delays = Vec::new();
        let result = download_with_retries(
            &cancelled,
            || {
                let attempt = attempts.fetch_add(1, Ordering::Relaxed);
                if attempt < 2 {
                    Err(DownloadAttemptError {
                        message: "temporary network failure".to_owned(),
                        retryable: true,
                    })
                } else {
                    Ok("downloaded")
                }
            },
            |delay| {
                delays.push(delay);
                true
            },
        )
        .unwrap();

        assert_eq!(result, "downloaded");
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
        assert_eq!(delays, [Duration::from_millis(500), Duration::from_secs(1)]);
    }

    #[test]
    fn persistent_transient_download_failures_stop_after_three_attempts() {
        let cancelled = AtomicBool::new(false);
        let attempts = AtomicUsize::new(0);
        let mut waits = 0;
        let error = download_with_retries(
            &cancelled,
            || {
                let attempt = attempts.fetch_add(1, Ordering::Relaxed) + 1;
                Err::<(), _>(DownloadAttemptError {
                    message: format!("network attempt {attempt} failed"),
                    retryable: true,
                })
            },
            |_| {
                waits += 1;
                true
            },
        )
        .unwrap_err();

        assert_eq!(error, "network attempt 3 failed");
        assert_eq!(attempts.load(Ordering::Relaxed), 3);
        assert_eq!(waits, 2);
    }

    #[test]
    fn permanent_download_failures_and_cancellation_do_not_retry() {
        let cancelled = AtomicBool::new(false);
        let attempts = AtomicUsize::new(0);
        let error = download_with_retries(
            &cancelled,
            || {
                attempts.fetch_add(1, Ordering::Relaxed);
                Err::<(), _>(DownloadAttemptError::permanent("invalid response"))
            },
            |_| panic!("permanent failures must not be retried"),
        )
        .unwrap_err();
        assert_eq!(error, "invalid response");
        assert_eq!(attempts.load(Ordering::Relaxed), 1);

        let attempts = AtomicUsize::new(0);
        let error = download_with_retries(
            &cancelled,
            || {
                attempts.fetch_add(1, Ordering::Relaxed);
                Err::<(), _>(DownloadAttemptError {
                    message: "temporary network failure".to_owned(),
                    retryable: true,
                })
            },
            |_| {
                cancelled.store(true, Ordering::Release);
                false
            },
        )
        .unwrap_err();
        assert!(error.contains("cancelled"));
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn only_transient_network_and_server_failures_are_retried() {
        for exit_code in [5, 6, 7, 18, 28, 52, 55, 56, 92] {
            assert!(is_transient_download_failure(Some(exit_code), 0));
        }
        for status in [408, 425, 429, 500, 502, 503, 504] {
            assert!(is_transient_download_failure(Some(22), status));
        }
        for (exit_code, status) in [(Some(22), 404), (Some(23), 0), (Some(35), 0), (None, 0)] {
            assert!(!is_transient_download_failure(exit_code, status));
        }
    }

    #[test]
    fn update_redirects_accept_only_the_exact_https_github_asset_hosts() {
        for url in [
            "https://github.com/NcaMoq/OpenShazam/releases/download/v0.1.2/setup.exe",
            "https://release-assets.githubusercontent.com/assets/123/setup.exe",
            "https://objects.githubusercontent.com/github-production-release-asset-123",
        ] {
            assert!(is_allowed_github_release_url(url), "{url}");
        }

        for url in [
            "http://github.com/NcaMoq/OpenShazam/releases/download/v0.1.2/setup.exe",
            "https://github.com.example.invalid/NcaMoq/OpenShazam/setup.exe",
            "https://github.com@attacker.example/setup.exe",
            "https://github.com:443/NcaMoq/OpenShazam/setup.exe",
            "https://attacker.example/github.com/setup.exe",
            "file:///C:/Users/Public/setup.exe",
        ] {
            assert!(!is_allowed_github_release_url(url), "{url}");
        }
    }

    #[test]
    fn disabling_auto_update_before_the_gated_launch_prevents_installer_start() {
        let enabled = Arc::new(AtomicBool::new(true));
        let gate = Arc::new(Mutex::new(()));
        let started = Arc::new(AtomicBool::new(false));
        let held_gate = gate.lock().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let worker_enabled = Arc::clone(&enabled);
        let worker_gate = Arc::clone(&gate);
        let worker_started = Arc::clone(&started);
        let worker = thread::spawn(move || {
            start_installer_process_if_enabled(
                true,
                &worker_cancelled,
                &worker_enabled,
                &worker_gate,
                || {
                    worker_started.store(true, Ordering::Release);
                    Ok(())
                },
            )
        });

        enabled.store(false, Ordering::Release);
        drop(held_gate);

        assert!(!worker.join().unwrap().unwrap());
        assert!(!started.load(Ordering::Acquire));
    }

    #[test]
    fn manual_installer_launch_does_not_depend_on_auto_update_setting() {
        let enabled = AtomicBool::new(false);
        let cancelled = AtomicBool::new(false);
        let gate = Mutex::new(());
        let mut started = false;

        assert!(
            start_installer_process_if_enabled(false, &cancelled, &enabled, &gate, || {
                started = true;
                Ok(())
            })
            .unwrap()
        );
        assert!(started);
    }

    #[test]
    fn shutdown_cancellation_prevents_installer_launch() {
        let cancelled = AtomicBool::new(true);
        let enabled = AtomicBool::new(true);
        let gate = Mutex::new(());
        let mut started = false;

        assert!(
            !start_installer_process_if_enabled(false, &cancelled, &enabled, &gate, || {
                started = true;
                Ok(())
            },)
            .unwrap()
        );
        assert!(!started);
    }

    #[test]
    fn cancelling_an_update_command_terminates_its_process() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-download-cancel-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("installer.exe");
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            worker_cancelled.store(true, Ordering::Release);
        });
        let started = std::time::Instant::now();
        let mut command = Command::new("powershell.exe");
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Start-Sleep -Seconds 30",
        ]);
        command.creation_flags(CREATE_NO_WINDOW);

        let error = run_download_command_cancellable(command, &cancelled, &path, 1024).unwrap_err();

        canceller.join().unwrap();
        assert!(error.contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(fs::metadata(&path).unwrap().len(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn https_client_diagnostics_are_discarded_while_the_pipe_is_drained() {
        let input = vec![b'x'; 20_000];
        let exceeded_limit = drain_bounded_output(Cursor::new(input), 128).unwrap();

        assert!(exceeded_limit);
    }

    #[test]
    fn https_client_output_at_the_limit_is_accepted() {
        let input = vec![b'x'; 128];
        let exceeded_limit = drain_bounded_output(Cursor::new(input.clone()), input.len()).unwrap();

        assert!(!exceeded_limit);
    }

    #[test]
    fn update_download_http_failure_message_contains_only_status() {
        let error = failed_download_attempt(Some(56), 0);

        assert_eq!(error.message, "Update download failed with HTTP 0.");
        assert!(error.retryable);
    }

    #[test]
    fn streamed_update_download_accepts_exact_limit_and_separates_the_effective_url() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-download-limit-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("installer.exe");
        let body = b"verified installer bytes";
        let url = "https://github.com/NcaMoq/OpenShazam/releases/download/v0.1.2/Shazam-Setup-0.1.2-x64.exe";
        let mut response = body.to_vec();
        response.extend_from_slice(DOWNLOAD_URL_TRAILER);
        response.extend_from_slice(url.as_bytes());
        response.extend_from_slice(DOWNLOAD_REDIRECT_URL_TRAILER);
        response.extend_from_slice(DOWNLOAD_HTTP_STATUS_TRAILER);
        response.extend_from_slice(b"200\n");

        let result =
            read_limited_download(Cursor::new(response), &path, body.len() as u64).unwrap();

        assert_eq!(result.effective_url, url);
        assert!(result.redirect_url.is_empty());
        assert_eq!(result.http_status, 200);
        assert_eq!(result.size_bytes, body.len() as u64);
        assert_eq!(fs::read(&path).unwrap(), body);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn streamed_http_error_status_is_preserved_for_retry_decisions() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-download-status-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("error-response.tmp");
        let mut response = b"temporary service error".to_vec();
        response.extend_from_slice(DOWNLOAD_URL_TRAILER);
        response.extend_from_slice(
            b"https://github.com/NcaMoq/OpenShazam/releases/latest/download/OpenShazam-update.json",
        );
        response.extend_from_slice(DOWNLOAD_REDIRECT_URL_TRAILER);
        response.extend_from_slice(DOWNLOAD_HTTP_STATUS_TRAILER);
        response.extend_from_slice(b"503\n");

        let result = read_limited_download(Cursor::new(response), &path, 1024).unwrap();

        assert_eq!(result.http_status, 503);
        assert!(is_transient_download_failure(Some(22), result.http_status));
        assert_eq!(fs::read(&path).unwrap(), b"temporary service error");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn redirect_responses_expose_only_allowlisted_targets_and_are_bounded() {
        let github_asset =
            "https://release-assets.githubusercontent.com/github-production-release-asset";
        assert_eq!(
            validate_redirect_target(302, github_asset, 0, 12).unwrap(),
            Some(github_asset)
        );
        assert!(validate_redirect_target(302, "https://attacker.example/file", 0, 12).is_err());
        assert!(validate_redirect_target(302, github_asset, 5, 12).is_err());
        assert!(validate_redirect_target(
            302,
            github_asset,
            0,
            super::MAX_REDIRECT_RESPONSE_BYTES + 1
        )
        .is_err());
        assert!(validate_redirect_target(304, github_asset, 0, 12)
            .unwrap()
            .is_none());
    }

    #[test]
    fn streamed_redirect_response_keeps_its_target_separate_from_body() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-download-redirect-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("redirect.tmp");
        let body = b"redirect response body";
        let url =
            "https://github.com/NcaMoq/OpenShazam/releases/latest/download/OpenShazam-update.json";
        let target = "https://release-assets.githubusercontent.com/signed-asset";
        let mut response = body.to_vec();
        response.extend_from_slice(DOWNLOAD_URL_TRAILER);
        response.extend_from_slice(url.as_bytes());
        response.extend_from_slice(DOWNLOAD_REDIRECT_URL_TRAILER);
        response.extend_from_slice(target.as_bytes());
        response.extend_from_slice(DOWNLOAD_HTTP_STATUS_TRAILER);
        response.extend_from_slice(b"302\n");

        let parsed = read_limited_download(Cursor::new(response), &path, 1024).unwrap();

        assert_eq!(parsed.effective_url, url);
        assert_eq!(parsed.redirect_url, target);
        assert_eq!(parsed.http_status, 302);
        assert_eq!(parsed.size_bytes, body.len() as u64);
        assert_eq!(fs::read(&path).unwrap(), body);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn streamed_update_download_rejects_unknown_length_overflow_before_writing_past_limit() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-download-overflow-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("installer.exe");
        let body = vec![b'x'; 16 * 1024];
        let mut response = body;
        response.extend_from_slice(DOWNLOAD_URL_TRAILER);
        response.extend_from_slice(
            b"https://github.com/NcaMoq/OpenShazam/releases/download/v0.1.2/file.exe\n",
        );

        let error = read_limited_download(Cursor::new(response), &path, 1024).unwrap_err();

        assert!(error.contains("signed size limit"));
        assert!(fs::metadata(&path).unwrap().len() <= 1024);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn storage_full_while_streaming_removes_the_partial_download() {
        let (root, directory) = create_test_update_directory();
        let path = directory.join("disk-full.exe");
        let response = vec![b'x'; MAX_HTTPS_CLIENT_OUTPUT_BYTES + 8192];

        let error = download_with_partial_cleanup(
            &path,
            &AtomicBool::new(false),
            || {
                let mut writer = StorageFullWriter {
                    file: File::create(&path).unwrap(),
                    remaining_bytes: 1024,
                };
                let result = read_limited_download_into(
                    Cursor::new(&response),
                    &mut writer,
                    (MAX_HTTPS_CLIENT_OUTPUT_BYTES + 16_384) as u64,
                );
                drop(writer);
                result.map(|_| ()).map_err(DownloadAttemptError::permanent)
            },
            |_| true,
        )
        .unwrap_err();

        assert!(error.contains("Could not write the update download"));
        assert!(path_entry_is_missing(&path).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_update_process_is_terminated_while_streaming() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-download-process-limit-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let path = root.join("installer.exe");
        let cancelled = AtomicBool::new(false);
        let mut command = Command::new("cmd.exe");
        command.args(["/D", "/Q", "/C", "for /L %i in (1,1,2147483647) do @echo x"]);
        command.creation_flags(CREATE_NO_WINDOW);
        let started = std::time::Instant::now();
        let (timing_sender, timing_receiver) = mpsc::channel();

        let error = run_download_command_cancellable_timed(
            command,
            &cancelled,
            &path,
            1024,
            Some(timing_sender),
        )
        .unwrap_err();

        let timings = timing_receiver.try_iter().collect::<Vec<_>>();
        assert!(error.contains("signed size limit"));
        assert!(timings
            .iter()
            .any(|timing| matches!(timing, DownloadCommandTiming::ChildSpawn(_))));
        assert!(timings
            .iter()
            .any(|timing| matches!(timing, DownloadCommandTiming::SizeLimitDetected(_))));
        assert!(timings
            .iter()
            .any(|timing| matches!(timing, DownloadCommandTiming::ChildKill(_))));
        assert!(timings
            .iter()
            .any(|timing| matches!(timing, DownloadCommandTiming::ChildWait(_))));
        assert!(timings
            .iter()
            .any(|timing| matches!(timing, DownloadCommandTiming::StdoutThreadJoin(_))));
        assert!(timings
            .iter()
            .any(|timing| matches!(timing, DownloadCommandTiming::StderrThreadJoin(_))));
        eprintln!(
            "oversized download process timings: {timings:?}; total: {:?}",
            started.elapsed()
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "oversized download exceeded 5 seconds; stage timings: {timings:?}"
        );
        assert!(fs::metadata(&path).unwrap().len() <= 1024);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn update_operation_guard_rejects_overlapping_work() {
        let mut active = false;
        assert!(try_begin_update_operation(&mut active));
        assert!(active);
        assert!(!try_begin_update_operation(&mut active));
        active = false;
        assert!(try_begin_update_operation(&mut active));
    }

    #[test]
    fn update_cleanup_removes_only_old_generated_artifacts() {
        let directory = std::env::temp_dir().join(format!(
            "openshazam-update-cleanup-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let stale_temp = directory.join(".manifest.123-456-789.tmp");
        let fresh_temp = directory.join(".signature.123-457-790.tmp");
        let unrelated_temp = directory.join(".manifest.archive.tmp");
        let completed_download = directory.join("download-123-456-789");
        fs::create_dir(&completed_download).unwrap();
        let incomplete_download = directory.join("download-123-457-790");
        fs::create_dir(&incomplete_download).unwrap();
        let unrelated_download = directory.join("download-archive");
        fs::create_dir(&unrelated_download).unwrap();
        let downloaded_installer = completed_download.join("Shazam-Setup-0.1.2-x64.exe");
        let unrelated_installer = unrelated_download.join("Shazam-Setup-0.1.2-x64.exe");
        let unrelated = directory.join("notes.txt");
        write_old_file(&stale_temp);
        File::create(&fresh_temp).unwrap();
        write_old_file(&downloaded_installer);
        write_old_file(&unrelated_installer);
        write_old_file(&unrelated_temp);
        File::create(&unrelated).unwrap();

        let canonical_directory = fs::canonicalize(&directory).unwrap();
        let removed =
            cleanup_obsolete_update_artifacts_at(&canonical_directory, SystemTime::now()).unwrap();

        assert_eq!(removed, 4);
        assert!(!stale_temp.exists());
        assert!(fresh_temp.exists());
        assert!(unrelated_temp.exists());
        assert!(!completed_download.exists());
        assert!(!incomplete_download.exists());
        assert!(unrelated_download.exists());
        assert!(unrelated_installer.exists());
        assert!(unrelated.exists());
        fs::remove_file(&unrelated_temp).unwrap();
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn update_cleanup_preserves_an_installer_locked_without_sharing() {
        let directory = std::env::temp_dir().join(format!(
            "openshazam-update-locked-installer-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let download_directory = directory.join("download-123-456-789");
        fs::create_dir(&download_directory).unwrap();
        let installer = download_directory.join("Shazam-Setup-0.1.2-x64.exe");
        write_old_file(&installer);

        let running_installer = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&installer)
            .unwrap();
        let canonical_directory = fs::canonicalize(&directory).unwrap();
        assert_eq!(
            cleanup_obsolete_update_artifacts_at(&canonical_directory, SystemTime::now()).unwrap(),
            0
        );
        assert!(installer.exists());
        assert!(download_directory.exists());

        drop(running_installer);
        assert_eq!(
            cleanup_obsolete_update_artifacts_at(&canonical_directory, SystemTime::now()).unwrap(),
            2
        );
        assert!(!installer.exists());
        assert!(!download_directory.exists());
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn incomplete_download_cleanup_waits_until_the_file_is_unlocked() {
        let (root, directory) = create_test_update_directory();
        let path = directory.join("incomplete-download.tmp");
        fs::write(&path, b"partial download").unwrap();
        let open_download = OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&path)
            .unwrap();

        let error = cleanup_partial_download(&path).unwrap_err();
        assert!(error.message.contains("Could not remove"));
        assert!(path.exists());

        drop(open_download);
        cleanup_partial_download(&path).unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn guarded_cleanup_handles_prevent_path_swaps_until_validation_finishes() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-update-path-lock-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let directory = root.join("Updates");
        fs::create_dir_all(&directory).unwrap();
        let directory = fs::canonicalize(&directory).unwrap();
        let replacement_directory = directory.with_file_name("Updates-replacement");
        let installer = directory.join("Shazam-Setup-0.1.2-x64.exe");
        let replacement_installer = directory.join("replacement.exe");
        File::create(&installer).unwrap();

        let directory_handles = super::lock_directory_chain(&directory).unwrap();
        assert!(fs::rename(&directory, &replacement_directory).is_err());
        drop(directory_handles);

        let installer_handle = super::open_guarded_regular_file(&installer).unwrap();
        assert!(fs::rename(&installer, &replacement_installer).is_err());
        drop(installer_handle);
        fs::rename(&installer, &replacement_installer).unwrap();

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn guarded_read_handle_allows_the_verified_executable_to_start() {
        let command_interpreter = std::env::var_os("COMSPEC").unwrap();
        let executable =
            super::open_guarded_read_file(std::path::Path::new(&command_interpreter)).unwrap();
        let status = Command::new(command_interpreter)
            .args(["/C", "exit", "0"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()
            .unwrap();

        assert!(status.success());
        drop(executable);
    }

    #[test]
    fn guarded_read_handle_prevents_verified_installer_replacement() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-update-read-lock-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let installer = root.join("Shazam-Setup-0.1.2-x64.exe");
        let replacement = root.join("replacement.exe");
        fs::write(&installer, b"verified installer bytes").unwrap();

        let verified_handle = super::open_guarded_read_file(&installer).unwrap();
        assert!(OpenOptions::new().write(true).open(&installer).is_err());
        assert!(fs::rename(&installer, &replacement).is_err());
        assert!(installer.exists());

        drop(verified_handle);
        assert!(OpenOptions::new().write(true).open(&installer).is_ok());
        fs::rename(&installer, &replacement).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_installer_cleanup_removes_only_a_versioned_file_in_its_private_folder() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-staged-cleanup-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let directory = root.join("OpenShazam").join("Updates");
        fs::create_dir_all(&directory).unwrap();
        let directory = fs::canonicalize(&directory).unwrap();
        let download_directory = directory.join("download-123-456-789");
        fs::create_dir_all(&download_directory).unwrap();
        let installer = download_directory.join("Shazam-Setup-0.1.2-x64.exe");
        let unrelated = download_directory.join("notes.txt");
        File::create(&installer).unwrap();
        File::create(&unrelated).unwrap();

        remove_staged_installer_at(&directory, &installer, "Shazam-Setup-0.1.2-x64.exe").unwrap();

        assert!(!installer.exists());
        assert!(unrelated.exists());
        assert!(download_directory.is_dir());
        fs::remove_file(&unrelated).unwrap();
        remove_staged_installer_at(&directory, &installer, "Shazam-Setup-0.1.2-x64.exe").unwrap();
        assert!(!download_directory.exists());
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn staged_installer_cleanup_refuses_paths_outside_generated_download_folders() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-staged-cleanup-outside-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let directory = root.join("OpenShazam").join("Updates");
        let outside_directory = root.join("outside");
        fs::create_dir_all(&directory).unwrap();
        fs::create_dir(&outside_directory).unwrap();
        let installer = outside_directory.join("Shazam-Setup-0.1.2-x64.exe");
        File::create(&installer).unwrap();

        assert!(
            remove_staged_installer_at(&directory, &installer, "Shazam-Setup-0.1.2-x64.exe")
                .is_err()
        );
        assert!(installer.exists());
        assert!(is_download_directory_name("download-123-456-789"));
        assert!(!is_download_directory_name("download-incomplete"));
        assert!(!is_download_directory_name("download-1-2-3-extra"));
        assert!(super::is_generated_temp_file_name(
            ".pending-update.123-456-789.tmp"
        ));
        assert!(!super::is_generated_temp_file_name(
            ".pending-update.notes.tmp"
        ));
        assert!(!super::is_generated_temp_file_name(
            ".manifest.1-2-3.tmp.extra"
        ));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn update_storage_is_created_only_under_local_app_data() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-update-root-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();

        let directory = update_directory_at(&root).unwrap();
        let canonical_app_directory = fs::canonicalize(root.join("OpenShazam")).unwrap();

        assert_eq!(
            directory,
            fs::canonicalize(root.join("OpenShazam/Updates")).unwrap()
        );
        assert_eq!(directory.parent(), Some(canonical_app_directory.as_path()));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn update_path_presence_distinguishes_missing_and_dangling_entries() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-update-path-presence-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let present = root.join("present.exe");
        File::create(&present).unwrap();
        let missing = root.join("missing.exe");
        assert!(!path_entry_is_missing(&present).unwrap());
        assert!(path_entry_is_missing(&missing).unwrap());

        let dangling_link = root.join("dangling-link.exe");
        match std::os::windows::fs::symlink_file(&missing, &dangling_link) {
            Ok(()) => {
                assert!(!dangling_link.exists());
                assert!(!path_entry_is_missing(&dangling_link).unwrap());
                fs::remove_file(&dangling_link).unwrap();
            }
            Err(error)
                if matches!(error.kind(), std::io::ErrorKind::PermissionDenied)
                    || error.raw_os_error() == Some(1314) =>
            {
                eprintln!("Skipping dangling-link assertion without symlink privilege: {error}");
            }
            Err(error) => panic!("could not create dangling update test link: {error}"),
        }
        let target_directory = root.join("junction-target");
        fs::create_dir(&target_directory).unwrap();
        let dangling_junction = root.join("dangling-junction");
        create_junction(&dangling_junction, &target_directory);
        fs::remove_dir(&target_directory).unwrap();
        assert!(!dangling_junction.exists());
        assert!(!path_entry_is_missing(&dangling_junction).unwrap());
        fs::remove_dir(&dangling_junction).unwrap();
        fs::remove_file(present).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn update_storage_refuses_a_junction_in_the_product_data_path() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-update-junction-root-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let outside = root.join("outside");
        fs::create_dir_all(&outside).unwrap();
        let product_directory = root.join("OpenShazam");
        create_junction(&product_directory, &outside);

        assert!(update_directory_at(&root).is_err());
        assert!(!outside.join("Updates").exists());

        fs::remove_dir(&product_directory).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn update_cleanup_does_not_follow_junctions_inside_download_storage() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-update-junction-cleanup-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let directory = root.join("OpenShazam/Updates");
        let outside = root.join("outside");
        fs::create_dir_all(&directory).unwrap();
        fs::create_dir(&outside).unwrap();
        let outside_file = outside.join("Shazam-Setup-0.1.2-x64.exe");
        write_old_file(&outside_file);
        let junction = directory.join("download-123-456-789");
        assert!(is_download_directory_name(
            junction.file_name().unwrap().to_str().unwrap()
        ));
        create_junction(&junction, &outside);

        let canonical_directory = fs::canonicalize(&directory).unwrap();
        cleanup_obsolete_update_artifacts_at(&canonical_directory, SystemTime::now()).unwrap();

        assert!(junction.exists());
        assert!(outside_file.exists());
        fs::remove_dir(&junction).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn update_cleanup_refuses_a_junction_at_the_update_storage_root() {
        let root = std::env::temp_dir().join(format!(
            "openshazam-update-root-junction-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let app_directory = root.join("OpenShazam");
        let outside = root.join("outside");
        fs::create_dir_all(&app_directory).unwrap();
        fs::create_dir(&outside).unwrap();
        let outside_file = outside.join("Shazam-Setup-0.1.2-x64.exe");
        write_old_file(&outside_file);
        let junction = app_directory.join("Updates");
        create_junction(&junction, &outside);

        assert!(cleanup_obsolete_update_artifacts_at(&junction, SystemTime::now()).is_err());
        assert!(outside_file.exists());

        fs::remove_dir(&junction).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    fn create_test_update_directory() -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "openshazam-update-marker-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        let directory = update_directory_at(&root).unwrap();
        (root, directory)
    }

    fn write_old_file(path: &std::path::Path) {
        let file = File::create(path).unwrap();
        file.set_times(
            FileTimes::new().set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1)),
        )
        .unwrap();
    }

    fn create_junction(path: &std::path::Path, target: &std::path::Path) {
        let command = format!(
            "/D /C mklink /J \"{}\" \"{}\"",
            path.display(),
            target.display()
        );
        let output = Command::new("cmd.exe").raw_arg(command).output().unwrap();
        assert!(
            output.status.success(),
            "Could not create a test junction: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
