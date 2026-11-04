//! The SQLite store: one [`Db`] handle, migrations embedded in the binary, and a small repository
//! per table in the submodules.

mod activity;
mod aliases;
mod arr_connections;
mod arr_sync;
mod backfill;
mod backup;
mod cache;
mod channel_videos;
mod grabs;
mod jellyfin_queue;
mod languages;
mod mappings;
mod match_inputs;
mod pins;
mod playlists;
mod prebuild_queue;
mod released;
mod releases;
mod root_folders;
mod unmatched;
mod unplayable;

use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use sqlx::{
    SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

pub use activity::StoredEvent;
pub use aliases::AliasEntry;
pub use arr_connections::SavedArr;
pub use arr_sync::{ArrEntry, ArrSource, SyncCounts};
pub use backfill::{BackfillJob, BackfillRow, DueBackfill};
pub use backup::BackupPolicy;
pub use cache::{StoredVideo, SubtitleTrack};
pub use channel_videos::{
    Availability, ChannelVideo, MatchState, SHORT, SeenVideo, VideoOnChannel, VideoSource,
};
pub use grabs::{Grab, GrabStatus, NewGrab};
pub use jellyfin_queue::{VersionAction, VersionJob};
pub use languages::LanguageSource;
pub use mappings::{Mapping, MappingKind, MatchOptions, NewMapping, PlaylistMode};
pub use pins::{Pin, PinTarget, replace_pin};
pub use playlists::PlaylistItem;
pub use prebuild_queue::QueuedPrebuild;
pub use releases::{EpisodeFilter, NewRelease, ReleaseKind, StoredRelease};
pub use root_folders::RootFolderRow;
pub use unmatched::{MatchCounts, NewUnmatched, Unmatched, UnmatchedState};
pub use unplayable::{RecheckDue, ReleaseCheck, UnplayableVideo};

static MIGRATOR: Migrator = sqlx::migrate!();

/// Why a store call failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot create database folder {path}: {source}")]
    CreateDir {
        path: String,
        source: std::io::Error,
    },
    #[error("cannot open database {path}: {source}")]
    Open { path: String, source: sqlx::Error },
    #[error(
        "cannot back up the database to {path} before migrating, so nothing was migrated: \
         {source}. Free some space or fix the folder's permissions, or set [backup] \
         before_migrations = false to migrate without a backup"
    )]
    Backup { path: String, source: sqlx::Error },
    #[error("database migration failed: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("{0} already exists")]
    Duplicate(String),
    #[error("database error: {0}")]
    Sql(#[from] sqlx::Error),
}

/// A handle to the database. Cheap to clone, every clone shares one connection pool.
#[derive(Debug, Clone)]
pub struct Db {
    pool: SqlitePool,
    /// Unmatched notes for videos uploaded longer ago than this many seconds are not kept. `None`
    /// keeps every note.
    note_retention_secs: Option<i64>,
}

impl Db {
    /// Opens (or creates) the database file at `path` in WAL mode and runs any pending migrations,
    /// after a backup with the default [`BackupPolicy`].
    pub async fn open(path: &Path) -> Result<Self, StoreError> {
        Self::open_backed_up(path, BackupPolicy::default()).await
    }

    /// [`Db::open`] with the backup taken before migrations as `policy` says.
    pub async fn open_backed_up(path: &Path, policy: BackupPolicy) -> Result<Self, StoreError> {
        Self::open_with(path, policy, &MIGRATOR, SystemTime::now()).await
    }

    async fn open_with(
        path: &Path,
        policy: BackupPolicy,
        migrator: &Migrator,
        now: SystemTime,
    ) -> Result<Self, StoreError> {
        let shown = path.display().to_string();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::CreateDir {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // NORMAL is safe with WAL: a power cut can lose the last commits but never corrupts.
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await
            .map_err(|source| StoreError::Open {
                path: shown,
                source,
            })?;
        if let Some(copy) = backup::before_migrations(&pool, path, migrator, policy, now).await? {
            tracing::info!(backup = %copy.display(), "database backed up before migrating");
        }
        migrator.run(&pool).await?;
        Ok(Self {
            pool,
            note_retention_secs: None,
        })
    }

    /// This handle, sharing the pool, with the matcher's note retention: notes written through it
    /// for videos uploaded longer ago than `retention` are dropped, except those waiting on a
    /// lookup. Zero keeps every note.
    #[must_use]
    pub fn with_note_retention(mut self, retention: Duration) -> Self {
        self.note_retention_secs =
            (!retention.is_zero()).then(|| i64::try_from(retention.as_secs()).unwrap_or(i64::MAX));
        self
    }

    /// The underlying pool, for queries outside the repositories here.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Rows in the tables that grow with a library's history, for the Health page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct StoreSizes {
    pub notes: i64,
    pub channel_videos: i64,
    pub grabs: i64,
    pub activity_events: i64,
}

impl Db {
    pub async fn store_sizes(&self) -> Result<StoreSizes, StoreError> {
        let (notes, channel_videos, grabs, activity_events) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM unmatched), (SELECT count(*) FROM channel_videos), \
             (SELECT count(*) FROM grabs), (SELECT count(*) FROM activity_events)",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(StoreSizes {
            notes,
            channel_videos,
            grabs,
            activity_events,
        })
    }
}

/// The current time as Unix seconds, the unit every timestamp column uses.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// Maps a unique-constraint failure to [`StoreError::Duplicate`], anything else to [`StoreError::Sql`].
fn duplicate_or(e: sqlx::Error, what: impl FnOnce() -> String) -> StoreError {
    match e.as_database_error() {
        Some(db) if db.is_unique_violation() => StoreError::Duplicate(what()),
        _ => StoreError::Sql(e),
    }
}

#[cfg(test)]
pub(crate) mod test_db {
    use super::Db;

    /// A fresh database in a temp folder. Keep the folder alive as long as the handle.
    pub async fn fresh() -> (Db, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
        (db, dir)
    }
}
