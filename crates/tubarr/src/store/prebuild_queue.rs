//! Videos waiting for a background read of their sample tables, so a restart does not lose them.

use super::{Db, StoreError, unix_now};

/// A queued video and the reads that failed for it so far.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct QueuedPrebuild {
    pub video_id: String,
    pub attempts: i64,
}

impl Db {
    /// Queues `video_id` unless it is queued already.
    pub async fn queue_prebuild(&self, video_id: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO prebuild_queue (video_id, queued_at) VALUES (?, ?) \
             ON CONFLICT (video_id) DO NOTHING",
        )
        .bind(video_id)
        .bind(unix_now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every queued video, oldest first.
    pub async fn queued_prebuilds(&self) -> Result<Vec<QueuedPrebuild>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT video_id, attempts FROM prebuild_queue ORDER BY queued_at, video_id",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Counts a failed read of `video_id` and keeps its error.
    pub async fn prebuild_failed(&self, video_id: &str, error: &str) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE prebuild_queue SET attempts = attempts + 1, last_error = ? WHERE video_id = ?",
        )
        .bind(error)
        .bind(video_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Takes `video_id` off the queue: its tables are on disk, or it was given up on.
    pub async fn unqueue_prebuild(&self, video_id: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM prebuild_queue WHERE video_id = ?")
            .bind(video_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
