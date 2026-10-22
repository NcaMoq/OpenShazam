//! Per-user recognition-history storage.
//!
//! SQLite lives under `%LOCALAPPDATA%\OpenShazam` for every Windows install.
//! The database worker owns its connection so the recognition/UI thread never
//! waits for writes.
#![deny(unsafe_op_in_unsafe_fn)]

use std::{
    env,
    os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    },
    path::{Path, PathBuf},
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use windows::Win32::{
    Foundation::HANDLE,
    Storage::FileSystem::{
        FileStandardInfo, GetFileInformationByHandleEx, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_STANDARD_INFO,
    },
};

const SCHEMA_VERSION: i64 = 1;
const HISTORY_LIMIT: usize = 40;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecognitionSource {
    Manual,
    Auto,
}

impl RecognitionSource {
    fn as_db_value(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Auto => "auto",
        }
    }

    fn from_db_value(value: &str) -> Result<Self, String> {
        match value {
            "manual" => Ok(Self::Manual),
            "auto" => Ok(Self::Auto),
            _ => Err("unknown saved recognition source".to_owned()),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PersistedTrack {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) recognized_at_utc: SystemTime,
    pub(crate) track_key: Option<String>,
    pub(crate) artwork_url: Option<String>,
    pub(crate) hq_artwork_url: Option<String>,
    pub(crate) background_artwork_url: Option<String>,
    pub(crate) shazam_page_url: Option<String>,
    #[allow(dead_code)]
    pub(crate) source: RecognitionSource,
}

enum StoreCommand {
    Save(PersistedTrack),
    Shutdown,
}

pub(crate) struct HistoryStore {
    sender: Sender<StoreCommand>,
    worker: Option<JoinHandle<()>>,
}

impl HistoryStore {
    pub(crate) fn open() -> Result<(Self, Vec<PersistedTrack>), String> {
        let database_path = history_database_path()?;
        let (command_sender, command_receiver) = mpsc::channel();
        let (ready_sender, ready_receiver) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("openshazam-history-store".to_owned())
            .spawn(move || {
                let mut connection = match open_database(&database_path) {
                    Ok(connection) => connection,
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                        return;
                    }
                };
                let entries = match load_history(&connection) {
                    Ok(entries) => entries,
                    Err(error) => {
                        let _ = ready_sender.send(Err(error));
                        return;
                    }
                };
                if ready_sender.send(Ok(entries)).is_err() {
                    return;
                }
                while let Ok(command) = command_receiver.recv() {
                    match command {
                        StoreCommand::Save(track) => {
                            if let Err(error) = save_history_entry(&mut connection, &track) {
                                eprintln!("OpenShazam could not save recognition history: {error}");
                            }
                        }
                        StoreCommand::Shutdown => break,
                    }
                }
            })
            .map_err(|error| format!("could not start the history database worker: {error}"))?;

        let entries = match ready_receiver.recv() {
            Ok(Ok(entries)) => entries,
            Ok(Err(error)) => {
                let _ = worker.join();
                return Err(error);
            }
            Err(error) => {
                let _ = worker.join();
                return Err(format!(
                    "history database worker stopped during startup: {error}"
                ));
            }
        };

        Ok((
            Self {
                sender: command_sender,
                worker: Some(worker),
            },
            entries,
        ))
    }

    pub(crate) fn save(&self, track: PersistedTrack) {
        if self.sender.send(StoreCommand::Save(track)).is_err() {
            eprintln!(
                "OpenShazam history database worker is unavailable; this entry was not saved."
            );
        }
    }
}

impl Drop for HistoryStore {
    fn drop(&mut self) {
        let _ = self.sender.send(StoreCommand::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn history_database_path() -> Result<PathBuf, String> {
    let local_app_data = env::var_os("LOCALAPPDATA")
        .ok_or_else(|| "LOCALAPPDATA is unavailable for this Windows build".to_owned())?;
    history_database_path_at(&PathBuf::from(local_app_data))
}

fn history_database_path_at(local_app_data: &Path) -> Result<PathBuf, String> {
    ensure_real_directory(local_app_data)?;
    let local_app_data = std::fs::canonicalize(local_app_data)
        .map_err(|error| format!("could not resolve Local AppData: {error}"))?;
    let app_directory = local_app_data.join("OpenShazam");
    create_or_validate_history_directory(&app_directory)?;
    let canonical_app_directory = std::fs::canonicalize(&app_directory)
        .map_err(|error| format!("could not resolve OpenShazam history storage: {error}"))?;
    if canonical_app_directory.parent() != Some(local_app_data.as_path()) {
        return Err("refusing to store history outside Local AppData".to_owned());
    }

    let destination = canonical_app_directory.join("history.db");
    let legacy_sources = legacy_history_paths(&local_app_data)?;
    migrate_legacy_history(&destination, &legacy_sources)?;
    validate_history_file_set(&destination)?;
    Ok(destination)
}

fn create_or_validate_history_directory(path: &Path) -> Result<(), String> {
    match std::fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "could not create history storage directory: {error}"
            ));
        }
    }
    ensure_real_directory(path)
}

fn ensure_real_directory(path: &Path) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("could not inspect history storage directory: {error}"))?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(
            "refusing to use a reparse point or non-directory for history storage".to_owned(),
        );
    }
    Ok(())
}

fn validate_regular_history_file(path: &Path) -> Result<bool, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("could not inspect history file: {error}")),
    };
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err("refusing to use a reparse point or non-file for history storage".to_owned());
    }

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)
        .map_err(|error| format!("could not safely open history file metadata: {error}"))?;
    let handle_metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect history file handle: {error}"))?;
    if !handle_metadata.is_file()
        || handle_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err("refusing to use a reparse point or non-file for history storage".to_owned());
    }

    let mut standard_info = FILE_STANDARD_INFO::default();
    // SAFETY: `file` keeps a valid Windows handle alive and `standard_info` is
    // a writable buffer with exactly the size required for FileStandardInfo.
    unsafe {
        GetFileInformationByHandleEx(
            HANDLE(file.as_raw_handle() as _),
            FileStandardInfo,
            (&mut standard_info as *mut FILE_STANDARD_INFO).cast(),
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        )
    }
    .map_err(|error| format!("could not inspect history file link count: {error}"))?;
    if standard_info.NumberOfLinks != 1 {
        return Err("refusing to use a multiply linked history file".to_owned());
    }
    Ok(true)
}

fn validate_history_file_set(database_path: &Path) -> Result<bool, String> {
    let database_exists = validate_regular_history_file(database_path)?;
    let mut sidecar_exists = false;
    for suffix in ["-wal", "-shm", "-journal"] {
        sidecar_exists |= validate_regular_history_file(&append_suffix(database_path, suffix))?;
    }
    if !database_exists && sidecar_exists {
        return Err("refusing to open SQLite history sidecars without their database".to_owned());
    }
    Ok(database_exists)
}

fn legacy_history_paths(local_app_data: &Path) -> Result<Vec<PathBuf>, String> {
    let unpackaged = local_app_data.join("OpenShazam").join("Unpackaged");
    validate_optional_history_directory(&unpackaged)?;
    validate_optional_history_directory(&unpackaged.join("LocalState"))?;
    let mut paths = vec![local_app_data
        .join("OpenShazam")
        .join("Unpackaged")
        .join("LocalState")
        .join("history.db")];
    let packages = local_app_data.join("Packages");
    validate_optional_history_directory(&packages)?;
    let entries = match std::fs::read_dir(packages) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(paths),
        Err(error) => return Err(format!("could not inspect previous Shazam data: {error}")),
    };
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("could not inspect previous Shazam data: {error}"))?;
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        if file_name.starts_with("OpenShazam.Windows_") {
            let package_directory = entry.path();
            ensure_real_directory(&package_directory)?;
            let local_state = package_directory.join("LocalState");
            validate_optional_history_directory(&local_state)?;
            paths.push(local_state.join("history.db"));
        }
    }
    Ok(paths)
}

fn validate_optional_history_directory(path: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => ensure_real_directory(path).map(|()| true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("could not inspect previous Shazam data: {error}")),
    }
}

fn migrate_legacy_history(destination: &Path, candidates: &[PathBuf]) -> Result<(), String> {
    if validate_regular_history_file(destination)? {
        validate_history_file_set(destination)?;
        return Ok(());
    }
    let mut existing = Vec::new();
    for path in candidates {
        if validate_history_file_set(path)? {
            if let Some(parent) = path.parent() {
                ensure_real_directory(parent)?;
            }
            existing.push(path.clone());
        }
    }
    if existing.is_empty() {
        return Ok(());
    }
    if existing.len() != 1 {
        return Err("multiple previous Shazam history databases were found; refusing to choose or overwrite one".to_owned());
    }

    let source = &existing[0];
    let destination_parent = destination
        .parent()
        .ok_or_else(|| "history database path has no parent directory".to_owned())?;
    create_or_validate_history_directory(destination_parent)?;

    // Move SQLite sidecars first and the database last. If the process or
    // machine stops mid-migration, the destination then has no database and
    // the source database remains available. A later launch can finish the
    // move instead of mistaking the partial destination for a completed one.
    let suffixes = ["-wal", "-shm", "-journal", ""];
    let mut moves = Vec::new();
    for suffix in suffixes {
        let source_file = append_suffix(source, suffix);
        if !validate_regular_history_file(&source_file)? {
            continue;
        }
        let destination_file = append_suffix(destination, suffix);
        if validate_regular_history_file(&destination_file)? {
            return Err("refusing to overwrite existing history data".to_owned());
        }
        moves.push((source_file, destination_file));
    }

    let mut moved = Vec::new();
    for (source_file, destination_file) in moves {
        if let Err(error) = std::fs::rename(&source_file, &destination_file) {
            let mut rollback_errors = Vec::new();
            for (moved_source, moved_destination) in moved.into_iter().rev() {
                if let Err(rollback_error) = std::fs::rename(&moved_destination, &moved_source) {
                    rollback_errors.push(rollback_error.to_string());
                }
            }
            let rollback_detail = if rollback_errors.is_empty() {
                String::new()
            } else {
                format!("; rollback errors: {}", rollback_errors.join("; "))
            };
            return Err(format!(
                "could not move previous history data: {error}{rollback_detail}"
            ));
        }
        moved.push((source_file, destination_file));
    }
    Ok(())
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

fn open_database(path: &PathBuf) -> Result<Connection, String> {
    let parent = path
        .parent()
        .ok_or_else(|| "history database path has no parent directory".to_owned())?;
    create_or_validate_history_directory(parent)?;
    validate_history_file_set(path)?;
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )
    .map_err(|error| format!("could not open history database: {error}"))?;
    validate_history_file_set(path)?;
    connection
        .busy_timeout(Duration::from_secs(2))
        .map_err(|error| format!("could not configure history database timeout: {error}"))?;
    migrate(&mut connection)?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .map_err(|error| format!("could not enable history database WAL mode: {error}"))?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .map_err(|error| format!("could not configure history database durability: {error}"))?;
    Ok(connection)
}

fn migrate(connection: &mut Connection) -> Result<(), String> {
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| format!("could not read history database schema version: {error}"))?;
    if version > SCHEMA_VERSION {
        return Err(format!(
            "history database schema {version} is newer than this app supports ({SCHEMA_VERSION})"
        ));
    }
    if version == SCHEMA_VERSION {
        return Ok(());
    }

    // Every schema change is committed as a single SQLite transaction. This
    // initial migration creates a new database and does not rewrite user data.
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Exclusive)
        .map_err(|error| format!("could not start history database migration: {error}"))?;
    transaction
        .execute_batch(
            "CREATE TABLE recognition_history (
                id INTEGER PRIMARY KEY,
                recognized_at_utc_ms INTEGER NOT NULL,
                title TEXT NOT NULL,
                artist TEXT NOT NULL,
                track_key TEXT,
                artwork_url TEXT,
                hq_artwork_url TEXT,
                background_artwork_url TEXT,
                shazam_page_url TEXT,
                source TEXT NOT NULL CHECK (source IN ('manual', 'auto'))
            );
            CREATE INDEX recognition_history_order
                ON recognition_history (recognized_at_utc_ms DESC, id DESC);
            PRAGMA user_version = 1;",
        )
        .map_err(|error| format!("could not apply history database migration: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("could not commit history database migration: {error}"))
}

fn load_history(connection: &Connection) -> Result<Vec<PersistedTrack>, String> {
    let mut statement = connection
        .prepare(
            "SELECT recognized_at_utc_ms, title, artist, track_key, artwork_url,
                    hq_artwork_url, background_artwork_url, shazam_page_url, source
             FROM recognition_history
             ORDER BY recognized_at_utc_ms DESC, id DESC
             LIMIT ?1",
        )
        .map_err(|error| format!("could not prepare history restore query: {error}"))?;
    let rows = statement
        .query_map([HISTORY_LIMIT as i64], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, String>(8)?,
            ))
        })
        .map_err(|error| format!("could not query saved recognition history: {error}"))?;

    let mut entries = Vec::new();
    for row in rows {
        let (
            timestamp_ms,
            title,
            artist,
            track_key,
            artwork_url,
            hq_artwork_url,
            background_artwork_url,
            shazam_page_url,
            source,
        ) = row.map_err(|error| format!("could not read saved recognition history: {error}"))?;
        entries.push(PersistedTrack {
            title,
            artist,
            recognized_at_utc: UNIX_EPOCH + Duration::from_millis(timestamp_ms.max(0) as u64),
            track_key,
            artwork_url,
            hq_artwork_url,
            background_artwork_url,
            shazam_page_url,
            source: RecognitionSource::from_db_value(&source)?,
        });
    }
    Ok(entries)
}

fn save_history_entry(connection: &mut Connection, track: &PersistedTrack) -> Result<(), String> {
    let timestamp_ms = track
        .recognized_at_utc
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64;
    let transaction = connection
        .transaction()
        .map_err(|error| format!("could not start history save transaction: {error}"))?;
    transaction
        .execute(
            "INSERT INTO recognition_history (
                recognized_at_utc_ms, title, artist, track_key, artwork_url,
                hq_artwork_url, background_artwork_url, shazam_page_url, source
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            (
                timestamp_ms,
                &track.title,
                &track.artist,
                &track.track_key,
                &track.artwork_url,
                &track.hq_artwork_url,
                &track.background_artwork_url,
                &track.shazam_page_url,
                track.source.as_db_value(),
            ),
        )
        .map_err(|error| format!("could not write recognition history: {error}"))?;
    transaction
        .execute(
            "DELETE FROM recognition_history
             WHERE id NOT IN (
                SELECT id FROM recognition_history
                ORDER BY recognized_at_utc_ms DESC, id DESC
                LIMIT ?1
             )",
            [HISTORY_LIMIT as i64],
        )
        .map_err(|error| format!("could not prune recognition history: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("could not commit recognition history: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::windows::{fs::OpenOptionsExt as WindowsOpenOptionsExt, process::CommandExt},
        process::Command,
    };

    fn temporary_database_path() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        env::temp_dir()
            .join(format!(
                "openshazam-history-{}-{unique}",
                std::process::id()
            ))
            .join("history.db")
    }

    #[test]
    fn invalid_saved_source_error_does_not_include_database_content() {
        let private_value = "private-track-metadata";

        let error = RecognitionSource::from_db_value(private_value).unwrap_err();

        assert_eq!(error, "unknown saved recognition source");
        assert!(!error.contains(private_value));
    }

    #[test]
    fn legacy_history_and_sqlite_sidecars_move_to_the_user_data_root() {
        let root = env::temp_dir().join(format!(
            "openshazam-history-migration-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let legacy = root.join("OpenShazam/Unpackaged/LocalState/history.db");
        let destination = root.join("OpenShazam/history.db");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"sqlite database").unwrap();
        std::fs::write(append_suffix(&legacy, "-wal"), b"wal data").unwrap();
        std::fs::write(append_suffix(&legacy, "-shm"), b"shm data").unwrap();

        migrate_legacy_history(&destination, std::slice::from_ref(&legacy)).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"sqlite database");
        assert_eq!(
            std::fs::read(append_suffix(&destination, "-wal")).unwrap(),
            b"wal data"
        );
        assert_eq!(
            std::fs::read(append_suffix(&destination, "-shm")).unwrap(),
            b"shm data"
        );
        assert!(!legacy.exists());
        assert!(!append_suffix(&legacy, "-wal").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_history_migration_finishes_after_a_crash_moved_sidecars_first() {
        let root = env::temp_dir().join(format!(
            "openshazam-history-migration-recovery-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let legacy = root.join("OpenShazam/Unpackaged/LocalState/history.db");
        let destination = root.join("OpenShazam/history.db");
        let legacy_wal = append_suffix(&legacy, "-wal");
        let destination_wal = append_suffix(&destination, "-wal");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"sqlite database").unwrap();
        std::fs::write(&legacy_wal, b"wal data").unwrap();

        // Simulate interruption after the sidecar rename but before the
        // database rename. Recovery must complete the set without overwriting
        // the already-moved WAL.
        std::fs::rename(&legacy_wal, &destination_wal).unwrap();
        migrate_legacy_history(&destination, std::slice::from_ref(&legacy)).unwrap();

        assert_eq!(std::fs::read(&destination).unwrap(), b"sqlite database");
        assert_eq!(std::fs::read(&destination_wal).unwrap(), b"wal data");
        assert!(!legacy.exists());
        assert!(!legacy_wal.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_history_migration_preserves_conflicting_destination_sidecars() {
        let root = env::temp_dir().join(format!(
            "openshazam-history-migration-conflict-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let legacy = root.join("legacy/history.db");
        let destination = root.join("OpenShazam/history.db");
        let legacy_wal = append_suffix(&legacy, "-wal");
        let destination_wal = append_suffix(&destination, "-wal");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"sqlite database").unwrap();
        std::fs::write(&legacy_wal, b"legacy wal").unwrap();
        std::fs::write(&destination_wal, b"existing wal").unwrap();

        let error =
            migrate_legacy_history(&destination, std::slice::from_ref(&legacy)).unwrap_err();

        assert!(error.contains("refusing to overwrite existing history data"));
        assert_eq!(std::fs::read(&legacy).unwrap(), b"sqlite database");
        assert_eq!(std::fs::read(&legacy_wal).unwrap(), b"legacy wal");
        assert_eq!(std::fs::read(&destination_wal).unwrap(), b"existing wal");
        assert!(!destination.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_history_migration_refuses_ambiguous_sources_without_moving_data() {
        let root = env::temp_dir().join(format!(
            "openshazam-history-ambiguity-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let first = root.join("first/history.db");
        let second = root.join("second/history.db");
        let destination = root.join("OpenShazam/history.db");
        for source in [&first, &second] {
            std::fs::create_dir_all(source.parent().unwrap()).unwrap();
            std::fs::write(source, b"legacy").unwrap();
        }

        let error =
            migrate_legacy_history(&destination, &[first.clone(), second.clone()]).unwrap_err();

        assert!(error.contains("multiple previous Shazam history databases"));
        assert!(first.exists());
        assert!(second.exists());
        assert!(!destination.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_history_migration_never_overwrites_the_current_database() {
        let root = env::temp_dir().join(format!(
            "openshazam-history-existing-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let legacy = root.join("legacy/history.db");
        let destination = root.join("OpenShazam/history.db");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"old").unwrap();
        std::fs::write(&destination, b"current").unwrap();

        migrate_legacy_history(&destination, std::slice::from_ref(&legacy)).unwrap();

        assert_eq!(std::fs::read(&legacy).unwrap(), b"old");
        assert_eq!(std::fs::read(&destination).unwrap(), b"current");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_history_migration_rolls_back_if_a_sqlite_sidecar_cannot_move() {
        let root = env::temp_dir().join(format!(
            "openshazam-history-rollback-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let legacy = root.join("OpenShazam/Unpackaged/LocalState/history.db");
        let legacy_wal = append_suffix(&legacy, "-wal");
        let legacy_shm = append_suffix(&legacy, "-shm");
        let destination = root.join("OpenShazam/history.db");
        let destination_wal = append_suffix(&destination, "-wal");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, b"legacy database bytes").unwrap();
        std::fs::write(&legacy_wal, b"legacy wal bytes").unwrap();
        std::fs::write(&legacy_shm, b"legacy shm bytes").unwrap();
        let shm_handle = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0x0000_0001)
            .open(&legacy_shm)
            .unwrap();

        let error =
            migrate_legacy_history(&destination, std::slice::from_ref(&legacy)).unwrap_err();

        assert!(error.contains("could not move previous history"));
        assert!(
            !error.contains(root.to_string_lossy().as_ref()),
            "history migration errors must not expose the per-user storage path"
        );
        assert_eq!(std::fs::read(&legacy).unwrap(), b"legacy database bytes");
        assert_eq!(std::fs::read(&legacy_wal).unwrap(), b"legacy wal bytes");
        assert_eq!(std::fs::read(&legacy_shm).unwrap(), b"legacy shm bytes");
        assert!(!destination.exists());
        assert!(!destination_wal.exists());
        drop(shm_handle);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn history_survives_closing_and_reopening_the_database() {
        let path = temporary_database_path();
        let recognized_at_utc = UNIX_EPOCH + Duration::from_millis(1_797_000_123_456);
        let track = PersistedTrack {
            title: "A test track".to_owned(),
            artist: "OpenShazam test".to_owned(),
            recognized_at_utc,
            track_key: Some("track-key".to_owned()),
            artwork_url: Some("https://example.invalid/artwork.jpg".to_owned()),
            hq_artwork_url: Some("https://example.invalid/artwork-hq.jpg".to_owned()),
            background_artwork_url: Some("https://example.invalid/background.jpg".to_owned()),
            shazam_page_url: Some("https://www.shazam.com/track/123".to_owned()),
            source: RecognitionSource::Auto,
        };

        {
            let mut connection = open_database(&path).expect("open the first database connection");
            save_history_entry(&mut connection, &track).expect("save a history row");
        }

        let reopened =
            open_database(&path).expect("reopen the same database after an update-shaped restart");
        let entries = load_history(&reopened).expect("restore history after reopening");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, track.title);
        assert_eq!(entries[0].artist, track.artist);
        assert_eq!(entries[0].recognized_at_utc, track.recognized_at_utc);
        assert_eq!(entries[0].track_key, track.track_key);
        assert_eq!(entries[0].artwork_url, track.artwork_url);
        assert_eq!(entries[0].hq_artwork_url, track.hq_artwork_url);
        assert_eq!(
            entries[0].background_artwork_url,
            track.background_artwork_url
        );
        assert_eq!(entries[0].shazam_page_url, track.shazam_page_url);
        assert_eq!(entries[0].source, RecognitionSource::Auto);

        drop(reopened);
        if let Some(directory) = path.parent() {
            let _ = std::fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn history_keeps_only_the_newest_forty_entries_in_newest_first_order() {
        let path = temporary_database_path();
        let mut connection = open_database(&path).expect("open the history database");
        for index in 0..(HISTORY_LIMIT + 5) {
            let track = PersistedTrack {
                title: format!("Track {index:02}"),
                artist: "OpenShazam test".to_owned(),
                recognized_at_utc: UNIX_EPOCH + Duration::from_millis(index as u64),
                track_key: None,
                artwork_url: None,
                hq_artwork_url: None,
                background_artwork_url: None,
                shazam_page_url: None,
                source: RecognitionSource::Manual,
            };
            save_history_entry(&mut connection, &track).expect("save a history row");
        }

        let entries = load_history(&connection).expect("restore recent history");

        assert_eq!(entries.len(), HISTORY_LIMIT);
        assert_eq!(
            entries.first().map(|entry| entry.title.as_str()),
            Some("Track 44")
        );
        assert_eq!(
            entries.last().map(|entry| entry.title.as_str()),
            Some("Track 05")
        );
        drop(connection);
        if let Some(directory) = path.parent() {
            let _ = std::fs::remove_dir_all(directory);
        }
    }

    #[test]
    fn newer_history_schema_is_rejected_without_changing_its_version() {
        let path = temporary_database_path();
        let parent = path.parent().unwrap();
        std::fs::create_dir_all(parent).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        drop(connection);

        let error = open_database(&path).unwrap_err();

        assert!(error.contains("is newer than this app supports"));
        let connection = Connection::open(&path).unwrap();
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION + 1);
        drop(connection);
        let _ = std::fs::remove_dir_all(parent);
    }

    #[test]
    fn history_storage_refuses_a_junction_for_the_product_data_directory() {
        let root = env::temp_dir().join(format!(
            "openshazam-history-junction-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let local_app_data = root.join("LocalAppData");
        let target = root.join("redirected");
        let junction = local_app_data.join("OpenShazam");
        std::fs::create_dir_all(&local_app_data).unwrap();
        std::fs::create_dir_all(&target).unwrap();
        let command = format!(
            "/D /C mklink /J \"{}\" \"{}\"",
            junction.display(),
            target.display()
        );
        let output = Command::new("cmd.exe")
            .raw_arg(command)
            .output()
            .expect("create a Windows junction for the regression test");
        assert!(
            output.status.success(),
            "could not create test junction: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let error = history_database_path_at(&local_app_data).unwrap_err();

        assert!(error.contains("reparse point"));
        std::fs::remove_dir(&junction).unwrap();
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn history_storage_refuses_multiply_linked_database_files() {
        let root = env::temp_dir().join(format!(
            "openshazam-history-hardlink-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let database = root.join("history.db");
        let alias = root.join("history-copy.db");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&database, b"sqlite database").unwrap();
        std::fs::hard_link(&database, &alias).unwrap();

        let error = validate_regular_history_file(&database).unwrap_err();

        assert!(error.contains("multiply linked"));
        let _ = std::fs::remove_dir_all(root);
    }
}
