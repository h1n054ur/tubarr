//! The copy of the database taken before migrations change it, so an upgrade can be undone by
//! putting the copy back and starting the older image.
//!
//! The copy is made with `VACUUM INTO`, which reads one consistent snapshot through SQLite itself
//! (WAL contents included) while the file stays open. It lands next to the database as
//! `tubarr.db.pre-<from>-to-<to>.<UTC time>.bak`, and only the newest `keep` copies stay.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    time::SystemTime,
};

use sqlx::{SqlitePool, migrate::Migrator};
use time::OffsetDateTime;

use super::StoreError;

/// What happens before pending migrations run (`[backup]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct BackupPolicy {
    /// `false` migrates without a copy.
    pub before_migrations: bool,
    /// Copies kept, the newest first. At least 1.
    pub keep: u32,
}

impl Default for BackupPolicy {
    fn default() -> Self {
        Self {
            before_migrations: true,
            keep: 3,
        }
    }
}

/// Copies the database at `db_path` when `migrator` has migrations the database has not applied
/// yet. A new database (nothing applied) has nothing worth keeping and gets no copy. Returns the
/// copy's path.
pub(super) async fn before_migrations(
    pool: &SqlitePool,
    db_path: &Path,
    migrator: &Migrator,
    policy: BackupPolicy,
    now: SystemTime,
) -> Result<Option<PathBuf>, StoreError> {
    if !policy.before_migrations {
        return Ok(None);
    }
    let applied = applied_versions(pool).await?;
    let Some(from) = applied.iter().max().copied() else {
        return Ok(None);
    };
    let Some(to) = migrator
        .iter()
        .filter(|m| !m.migration_type.is_down_migration() && !applied.contains(&m.version))
        .map(|m| m.version)
        .max()
    else {
        return Ok(None);
    };
    let path = backup_path(db_path, from, to, now);
    copy(pool, &path)
        .await
        .map_err(|source| StoreError::Backup {
            path: path.display().to_string(),
            source,
        })?;
    rotate(db_path, policy.keep.max(1));
    Ok(Some(path))
}

async fn applied_versions(pool: &SqlitePool) -> Result<HashSet<i64>, StoreError> {
    let table: Option<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?;
    if table.is_none() {
        return Ok(HashSet::new());
    }
    let versions: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = 1")
            .fetch_all(pool)
            .await?;
    Ok(versions.into_iter().collect())
}

fn backup_path(db_path: &Path, from: i64, to: i64, now: SystemTime) -> PathBuf {
    let t = OffsetDateTime::from(now);
    let stamp = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second()
    );
    let mut name = db_path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".pre-{from:04}-to-{to:04}.{stamp}.bak"));
    db_path.with_file_name(name)
}

/// Writes the snapshot under a `.partial` name first, so a failed copy never looks like a backup.
async fn copy(pool: &SqlitePool, path: &Path) -> Result<(), sqlx::Error> {
    let mut partial = path.as_os_str().to_os_string();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    sqlx::query("VACUUM INTO ?")
        .bind(partial.to_string_lossy().into_owned())
        .execute(pool)
        .await?;
    std::fs::rename(&partial, path).map_err(sqlx::Error::Io)
}

/// Deletes all but the newest `keep` copies of `db_path`. A copy that cannot be deleted is only
/// logged: the upgrade itself already has its backup.
fn rotate(db_path: &Path, keep: u32) {
    let (Some(dir), Some(base)) = (db_path.parent(), db_path.file_name()) else {
        return;
    };
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    let prefix = format!("{}.pre-", base.to_string_lossy());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // The UTC time is the last part before `.bak`, and it sorts as text.
    let mut copies: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let stamp = name.strip_prefix(&prefix)?.strip_suffix(".bak")?;
            let stamp = stamp.rsplit('.').next()?.to_owned();
            Some((stamp, e.path()))
        })
        .collect();
    copies.sort_by(|a, b| b.cmp(a));
    for (_, old) in copies.into_iter().skip(keep as usize) {
        match std::fs::remove_file(&old) {
            Ok(()) => tracing::info!(path = %old.display(), "old database backup removed"),
            Err(e) => {
                tracing::warn!(path = %old.display(), error = %e, "old database backup not removed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use sqlx::sqlite::SqliteConnectOptions;

    use super::*;
    use crate::store::Db;

    /// 2026-10-26 12:00:00 UTC plus `secs`.
    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_793_016_000 + secs)
    }

    async fn migrator(dir: &Path, files: &[(&str, &str)]) -> Migrator {
        for (name, sql) in files {
            std::fs::write(dir.join(name), sql).unwrap();
        }
        Migrator::new(dir).await.unwrap()
    }

    const FIRST: (&str, &str) = ("0001_a.sql", "CREATE TABLE a (v TEXT);");
    const SECOND: (&str, &str) = ("0002_b.sql", "CREATE TABLE b (v TEXT);");

    async fn has_table(pool: &SqlitePool, name: &str) -> bool {
        sqlx::query_scalar::<_, String>(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
        )
        .bind(name)
        .fetch_optional(pool)
        .await
        .unwrap()
        .is_some()
    }

    fn backups(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.ends_with(".bak"))
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn a_copy_is_taken_only_when_migrations_are_pending() {
        let data = tempfile::tempdir().unwrap();
        let sql = tempfile::tempdir().unwrap();
        let path = data.path().join("tubarr.db");
        let policy = BackupPolicy::default();

        // A new database: nothing to keep.
        let db = Db::open_with(&path, policy, &migrator(sql.path(), &[FIRST]).await, at(0))
            .await
            .unwrap();
        sqlx::query("INSERT INTO a VALUES ('kept')")
            .execute(db.pool())
            .await
            .unwrap();
        drop(db);
        assert!(backups(data.path()).is_empty());

        // Nothing pending: no copy either.
        let same = migrator(sql.path(), &[FIRST]).await;
        drop(Db::open_with(&path, policy, &same, at(1)).await.unwrap());
        assert!(backups(data.path()).is_empty());

        // A pending migration: one copy, of the database before it ran.
        let next = migrator(sql.path(), &[FIRST, SECOND]).await;
        let db = Db::open_with(&path, policy, &next, at(2)).await.unwrap();
        assert!(has_table(db.pool(), "b").await);
        let names = backups(data.path());
        assert_eq!(names, ["tubarr.db.pre-0001-to-0002.20261026T120002Z.bak"]);

        let copy = SqlitePool::connect_with(
            SqliteConnectOptions::new().filename(data.path().join(&names[0])),
        )
        .await
        .unwrap();
        let rows: Vec<String> = sqlx::query_scalar("SELECT v FROM a")
            .fetch_all(&copy)
            .await
            .unwrap();
        assert_eq!(rows, ["kept"]);
        assert!(!has_table(&copy, "b").await);
    }

    #[tokio::test]
    async fn only_the_newest_copies_stay() {
        let data = tempfile::tempdir().unwrap();
        let sql = tempfile::tempdir().unwrap();
        let path = data.path().join("tubarr.db");
        drop(
            Db::open_with(
                &path,
                BackupPolicy::default(),
                &migrator(sql.path(), &[FIRST]).await,
                at(0),
            )
            .await
            .unwrap(),
        );
        for stamp in ["20250101T000000Z", "20250201T000000Z", "20250301T000000Z"] {
            let name = format!("tubarr.db.pre-0001-to-0001.{stamp}.bak");
            std::fs::write(data.path().join(name), b"old").unwrap();
        }
        std::fs::write(data.path().join("notes.bak"), b"not ours").unwrap();

        let policy = BackupPolicy {
            before_migrations: true,
            keep: 2,
        };
        let next = migrator(sql.path(), &[FIRST, SECOND]).await;
        drop(Db::open_with(&path, policy, &next, at(5)).await.unwrap());
        assert_eq!(
            backups(data.path()),
            [
                "notes.bak",
                "tubarr.db.pre-0001-to-0001.20250301T000000Z.bak",
                "tubarr.db.pre-0001-to-0002.20261026T120005Z.bak",
            ]
        );
    }

    #[tokio::test]
    async fn a_failed_copy_stops_the_migration_unless_backups_are_off() {
        let data = tempfile::tempdir().unwrap();
        let sql = tempfile::tempdir().unwrap();
        let path = data.path().join("tubarr.db");
        let policy = BackupPolicy::default();
        drop(
            Db::open_with(&path, policy, &migrator(sql.path(), &[FIRST]).await, at(0))
                .await
                .unwrap(),
        );
        // Something already sits where the copy goes, so VACUUM INTO refuses.
        std::fs::create_dir(
            data.path()
                .join("tubarr.db.pre-0001-to-0002.20261026T120003Z.bak.partial"),
        )
        .unwrap();

        let next = migrator(sql.path(), &[FIRST, SECOND]).await;
        let err = Db::open_with(&path, policy, &next, at(3))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Backup { .. }), "{err}");
        assert!(
            err.to_string().contains("before_migrations = false"),
            "{err}"
        );
        let pool = SqlitePool::connect_with(SqliteConnectOptions::new().filename(&path))
            .await
            .unwrap();
        assert!(!has_table(&pool, "b").await, "nothing migrated");
        pool.close().await;

        let off = BackupPolicy {
            before_migrations: false,
            ..policy
        };
        let db = Db::open_with(&path, off, &next, at(3)).await.unwrap();
        assert!(has_table(db.pool(), "b").await);
    }
}
