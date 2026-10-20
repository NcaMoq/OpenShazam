//! Per-user recognition-history storage.
//!
//! SQLite lives under `%LOCALAPPDATA%\OpenShazam` for every Windows install.
//! The database worker owns its connection so the recognition/UI thread never
//! waits for writes.

use std::{
    env,
    path::{Path, PathBuf},
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};

const SCHEMA_VERSION: i64 = 1;
const HISTORY_LIMIT: usize = 40;

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
            other => Err(format!("unknown saved recognition source `{other}`")),
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
    let local_app_data = PathBuf::from(local_app_data);
    let destination = local_app_data.join("OpenShazam").join("history.db");
    let legacy_sources = legacy_history_paths(&local_app_data)?;
    migrate_legacy_history(&destination, &legacy_sources)?;
    Ok(destination)
}

fn legacy_history_paths(local_app_data: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = vec![local_app_data
        .join("OpenShazam")
        .join("Unpackaged")
        .join("LocalState")
        .join("history.db")];
    let packages = local_app_data.join("Packages");
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
            paths.push(entry.path().join("LocalState").join("history.db"));
        }
    }
    Ok(paths)
}

fn migrate_legacy_history(destination: &Path, candidates: &[PathBuf]) -> Result<(), String> {
    if destination.exists() {
        return Ok(());
    }
    let existing: Vec<_> = candidates
        .iter()
        .filter(|path| path.is_file())
        .cloned()
        .collect();
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
    std::fs::create_dir_all(destination_parent)
        .map_err(|error| format!("could not create history storage directory: {error}"))?;

    let suffixes = ["", "-wal", "-shm", "-journal"];
    let mut moves = Vec::new();
    for suffix in suffixes {
        let source_file = append_suffix(source, suffix);
        if !source_file.exists() {
            continue;
        }
        let destination_file = append_suffix(destination, suffix);
        if destination_file.exists() {
            return Err(format!(
                "refusing to overwrite existing history data at {}",
                destination_file.display()
            ));
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
                "could not move previous history from {} to {}: {error}{rollback_detail}",
                source_file.display(),
                destination_file.display()
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
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("could not create history storage directory: {error}"))?;
    let mut connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
    )
    .map_err(|error| format!("could not open {}: {error}", path.display()))?;
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
}
