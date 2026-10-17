//! Packaged recognition-history storage.
//!
//! SQLite lives in the package-managed LocalState folder for MSIX installs.
//! Unpackaged developer builds use a separate per-user LocalAppData location.
//! The database worker owns its connection so the recognition/UI thread never
//! waits for writes.

use std::{
    env,
    path::PathBuf,
    sync::mpsc::{self, Sender},
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use windows::{
    Storage::ApplicationData, Win32::Storage::Packaging::Appx::GetCurrentPackageFullName,
};

const SCHEMA_VERSION: i64 = 1;
const HISTORY_LIMIT: usize = 40;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const APPMODEL_ERROR_NO_PACKAGE: u32 = 15_700;

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
    if process_has_package_identity()? {
        let application_data = ApplicationData::Current()
            .map_err(|error| format!("Windows could not open package LocalState: {error}"))?;
        let local_folder = application_data
            .LocalFolder()
            .map_err(|error| format!("Windows could not locate package LocalState: {error}"))?;
        let path = local_folder
            .Path()
            .map_err(|error| format!("Windows could not read package LocalState path: {error}"))?;
        return Ok(PathBuf::from(path.to_string()).join("history.db"));
    }

    let local_app_data = env::var_os("LOCALAPPDATA")
        .ok_or_else(|| "LOCALAPPDATA is unavailable for this unpackaged build".to_owned())?;
    Ok(PathBuf::from(local_app_data)
        .join("OpenShazam")
        .join("Unpackaged")
        .join("LocalState")
        .join("history.db"))
}

fn process_has_package_identity() -> Result<bool, String> {
    let mut package_full_name_length = 0u32;
    let result = unsafe { GetCurrentPackageFullName(&mut package_full_name_length, None).0 };
    match result {
        0 | ERROR_INSUFFICIENT_BUFFER => Ok(true),
        APPMODEL_ERROR_NO_PACKAGE => Ok(false),
        code => Err(format!(
            "Windows could not determine package identity (error {code})"
        )),
    }
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
