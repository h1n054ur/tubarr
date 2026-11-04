//! What a mapping's last matcher run saw, so a run can leave a mapping alone when nothing it
//! reads has changed since.

use super::{Db, StoreError, unix_now};

impl Db {
    /// Every row a matcher run for the mapping reads or writes, one line each, sorted: its
    /// aliases and pins, the videos on `channel_ids` (and pinned ones) with their metadata
    /// and unplayable marks, the playlist's items, its releases and its notes. Columns only other
    /// jobs write (when the watcher last saw a video, a video's match state, when a mark was
    /// checked, a note's ignored flag) are left out, so they never set off a run.
    pub async fn match_input_lines(
        &self,
        mapping_id: i64,
        channel_ids: &[String],
        playlist_id: Option<&str>,
    ) -> Result<Vec<String>, StoreError> {
        Ok(sqlx::query_scalar(
            "WITH vids AS ( \
               SELECT video_id FROM channel_videos \
                 WHERE channel_id IN (SELECT value FROM json_each(?2)) \
               UNION SELECT s.value FROM pins p, \
                 json_each('[\"' || replace(p.video_id, ',', '\",\"') || '\"]') s \
                 WHERE p.mapping_id = ?1) \
             SELECT line FROM ( \
               SELECT 'a' || quote(alias) AS line FROM mapping_aliases WHERE mapping_id = ?1 \
               UNION ALL SELECT 'p' || quote(season) || quote(episode) || quote(air_date) \
                 || quote(video_id) FROM pins WHERE mapping_id = ?1 \
               UNION ALL SELECT 'c' || quote(video_id) || quote(channel_id) || quote(title) \
                 || quote(published_at) || quote(published_approx) || quote(duration_secs) \
                 || quote(first_seen) || quote(skip_reason) \
                 FROM channel_videos WHERE video_id IN vids \
               UNION ALL SELECT 'v' || quote(video_id) || quote(title) || quote(duration_secs) \
                 || quote(upload_date) || quote(uploaded_at) || quote(channel_id) \
                 || quote(height) || quote(size) FROM videos WHERE video_id IN vids \
               UNION ALL SELECT 'x' || quote(video_id) FROM unplayable_videos \
                 WHERE video_id IN vids \
               UNION ALL SELECT 'l' || quote(position) || quote(video_id) || quote(title) \
                 || quote(duration_secs) FROM playlist_items WHERE playlist_id = ?3 \
               UNION ALL SELECT 'r' || quote(id) || quote(video_id) || quote(season) \
                 || quote(episode) || quote(air_date) || quote(year) || quote(imdb_id) \
                 || quote(video_title) || quote(language) || quote(height) || quote(size) \
                 || quote(published_at) || quote(part_ids) FROM releases WHERE mapping_id = ?1 \
               UNION ALL SELECT 'u' || quote(video_id) || quote(state) || quote(season) \
                 || quote(episode) || quote(part) || quote(reason) \
                 FROM unmatched WHERE mapping_id = ?1) \
             ORDER BY line",
        )
        .bind(mapping_id)
        .bind(sqlx::types::Json(channel_ids))
        .bind(playlist_id)
        .fetch_all(&self.pool)
        .await?)
    }

    /// The digest the mapping's last run stored, `None` before its first.
    pub async fn match_digest(&self, mapping_id: i64) -> Result<Option<i64>, StoreError> {
        Ok(
            sqlx::query_scalar("SELECT digest FROM match_inputs WHERE mapping_id = ?")
                .bind(mapping_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    pub async fn set_match_digest(&self, mapping_id: i64, digest: i64) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO match_inputs (mapping_id, digest, matched_at) VALUES (?, ?, ?) \
             ON CONFLICT (mapping_id) DO UPDATE SET digest = excluded.digest, \
             matched_at = excluded.matched_at",
        )
        .bind(mapping_id)
        .bind(digest)
        .bind(unix_now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
