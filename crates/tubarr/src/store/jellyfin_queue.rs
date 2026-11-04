//! Episodes waiting for Jellyfin to merge or split their audio only version, so a restart does
//! not lose them. Times are Unix milliseconds, since the first retries come seconds apart.

use super::{Db, StoreError};

/// What a queued episode needs from Jellyfin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(type_name = "TEXT", rename_all = "lowercase")]
pub enum VersionAction {
    /// Make the audio only file a version of the episode.
    Merge,
    /// Take every other version off the episode.
    Split,
}

/// A queued episode.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct VersionJob {
    /// The episode's main `.strm` as Jellyfin sees it.
    pub path: String,
    pub action: VersionAction,
    /// Tells this job from one queued for the same path later.
    pub queued_at_ms: i64,
    pub attempts: i64,
}

impl Db {
    /// Queues `path` for `action` at `now_ms`. A job already queued for it is replaced, so the
    /// latest change to the file wins and starts with no failed tries.
    pub async fn queue_version_job(
        &self,
        path: &str,
        action: VersionAction,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO jellyfin_merge_queue (path, action, queued_at_ms, next_at_ms) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT (path) DO UPDATE SET action = excluded.action, \
             queued_at_ms = excluded.queued_at_ms, next_at_ms = excluded.next_at_ms, \
             attempts = 0, last_error = NULL",
        )
        .bind(path)
        .bind(action)
        .bind(now_ms)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// The jobs due at `now_ms`, oldest first.
    pub async fn due_version_jobs(&self, now_ms: i64) -> Result<Vec<VersionJob>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT path, action, queued_at_ms, attempts FROM jellyfin_merge_queue \
             WHERE next_at_ms <= ? ORDER BY queued_at_ms, path",
        )
        .bind(now_ms)
        .fetch_all(&self.pool)
        .await?)
    }

    /// When the next job is due, if any is queued.
    pub async fn next_version_job_at(&self) -> Result<Option<i64>, StoreError> {
        Ok(
            sqlx::query_scalar("SELECT MIN(next_at_ms) FROM jellyfin_merge_queue")
                .fetch_one(&self.pool)
                .await?,
        )
    }

    /// How many jobs are queued, retries included.
    pub async fn version_jobs_queued(&self) -> Result<i64, StoreError> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM jellyfin_merge_queue")
                .fetch_one(&self.pool)
                .await?,
        )
    }

    /// Counts a failed try of `job` and puts its next one at `next_at_ms`. A job queued again for
    /// the same path since is left as it is.
    pub async fn version_job_failed(
        &self,
        job: &VersionJob,
        error: &str,
        next_at_ms: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE jellyfin_merge_queue SET attempts = attempts + 1, last_error = ?, \
             next_at_ms = ? WHERE path = ? AND queued_at_ms = ?",
        )
        .bind(error)
        .bind(next_at_ms)
        .bind(&job.path)
        .bind(job.queued_at_ms)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Takes `job` off the queue: done, or given up on. A job queued again for the same path
    /// since stays.
    pub async fn unqueue_version_job(&self, job: &VersionJob) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM jellyfin_merge_queue WHERE path = ? AND queued_at_ms = ?")
            .bind(&job.path)
            .bind(job.queued_at_ms)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
