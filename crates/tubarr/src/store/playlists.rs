//! Official playlists as last listed, the playlists the watcher should list, and draft requests.

use std::collections::{HashMap, HashSet};

use super::{Db, SHORT, StoreError, unix_now};

/// One video of a listed playlist.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct PlaylistItem {
    pub video_id: String,
    pub title: String,
    pub duration_secs: Option<i64>,
}

impl Db {
    /// Replaces everything stored for `playlist_id` with `items`, in order, and marks it listed.
    pub async fn replace_playlist(
        &self,
        playlist_id: &str,
        items: &[PlaylistItem],
    ) -> Result<(), StoreError> {
        let now = unix_now();
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM playlist_items WHERE playlist_id = ?")
            .bind(playlist_id)
            .execute(&mut *tx)
            .await?;
        for (position, item) in (1i64..).zip(items) {
            sqlx::query(
                "INSERT INTO playlist_items \
                 (playlist_id, position, video_id, title, duration_secs, listed_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(playlist_id)
            .bind(position)
            .bind(&item.video_id)
            .bind(&item.title)
            .bind(item.duration_secs)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "INSERT INTO playlist_scans (playlist_id, last_listing_at) VALUES (?, ?) \
             ON CONFLICT (playlist_id) DO UPDATE SET last_listing_at = excluded.last_listing_at",
        )
        .bind(playlist_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM playlist_requests WHERE playlist_id = ?")
            .bind(playlist_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// The playlist's videos in order, or `None` while it has never been listed.
    pub async fn playlist_items(
        &self,
        playlist_id: &str,
    ) -> Result<Option<Vec<PlaylistItem>>, StoreError> {
        let listed: Option<i64> =
            sqlx::query_scalar("SELECT last_listing_at FROM playlist_scans WHERE playlist_id = ?")
                .bind(playlist_id)
                .fetch_optional(&self.pool)
                .await?;
        if listed.is_none() {
            return Ok(None);
        }
        Ok(Some(
            sqlx::query_as(
                "SELECT video_id, title, duration_secs FROM playlist_items \
                 WHERE playlist_id = ? ORDER BY position",
            )
            .bind(playlist_id)
            .fetch_all(&self.pool)
            .await?,
        ))
    }

    /// The playlist's video ids in order without its YouTube Shorts, or `None` while it has never
    /// been listed: what a show numbered by playlist order counts. A Short holds no episode
    /// number, so it neither takes one nor holds up the numbers after it.
    pub async fn playlist_order(
        &self,
        playlist_id: &str,
    ) -> Result<Option<Vec<String>>, StoreError> {
        let Some(items) = self.playlist_items(playlist_id).await? else {
            return Ok(None);
        };
        let shorts: HashSet<String> = sqlx::query_scalar(
            "SELECT c.video_id FROM playlist_items p \
             JOIN channel_videos c ON c.video_id = p.video_id \
             WHERE p.playlist_id = ? AND c.skip_reason = ?",
        )
        .bind(playlist_id)
        .bind(SHORT)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect();
        Ok(Some(
            items
                .into_iter()
                .map(|i| i.video_id)
                .filter(|id| !shorts.contains(id))
                .collect(),
        ))
    }

    /// The playlists to list: those named by active mappings, then those a draft asked for.
    /// Sorted, without repeats.
    pub async fn watched_playlists(&self) -> Result<Vec<String>, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT playlist_id FROM mappings \
               WHERE active AND playlist_id IS NOT NULL AND trim(playlist_id) <> '' \
             UNION SELECT playlist_id FROM playlist_requests ORDER BY 1",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Asks the watcher to list `playlist_id` once, for a draft.
    pub async fn request_playlist(&self, playlist_id: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO playlist_requests (playlist_id, requested_at) VALUES (?, ?) \
             ON CONFLICT (playlist_id) DO NOTHING",
        )
        .bind(playlist_id)
        .bind(unix_now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Playlists a draft asked for that have not been listed since.
    pub async fn requested_playlists(&self) -> Result<Vec<String>, StoreError> {
        Ok(
            sqlx::query_scalar("SELECT playlist_id FROM playlist_requests ORDER BY playlist_id")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    /// When each playlist was last listed, as Unix seconds.
    pub async fn playlist_listing_times(&self) -> Result<HashMap<String, i64>, StoreError> {
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT playlist_id, last_listing_at FROM playlist_scans")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MatchOptions, mappings::tests::drama, test_db};

    fn item(id: &str) -> PlaylistItem {
        PlaylistItem {
            video_id: id.to_owned(),
            title: format!("Video {id}"),
            duration_secs: Some(1800),
        }
    }

    #[tokio::test]
    async fn listings_replace_the_whole_playlist_in_order() {
        let (db, _dir) = test_db::fresh().await;
        assert_eq!(db.playlist_items("PLaaaaaaaaaaaa").await.unwrap(), None);

        db.replace_playlist("PLaaaaaaaaaaaa", &[item("b"), item("a"), item("c")])
            .await
            .unwrap();
        db.replace_playlist("PLbbbbbbbbbbbb", &[item("z")])
            .await
            .unwrap();
        let ids = |items: Option<Vec<PlaylistItem>>| -> Vec<String> {
            items.unwrap().into_iter().map(|i| i.video_id).collect()
        };
        assert_eq!(
            ids(db.playlist_items("PLaaaaaaaaaaaa").await.unwrap()),
            ["b", "a", "c"]
        );

        // A video taken out and one added: nothing of the old order is left over.
        db.replace_playlist("PLaaaaaaaaaaaa", &[item("a"), item("d")])
            .await
            .unwrap();
        assert_eq!(
            ids(db.playlist_items("PLaaaaaaaaaaaa").await.unwrap()),
            ["a", "d"]
        );
        assert_eq!(
            ids(db.playlist_items("PLbbbbbbbbbbbb").await.unwrap()),
            ["z"]
        );

        // Listed but empty is not the same as never listed.
        db.replace_playlist("PLaaaaaaaaaaaa", &[]).await.unwrap();
        assert_eq!(
            db.playlist_items("PLaaaaaaaaaaaa").await.unwrap(),
            Some(vec![])
        );
        assert_eq!(db.playlist_listing_times().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn watched_playlists_come_from_active_mappings_and_requests() {
        let (db, _dir) = test_db::fresh().await;
        let mapping = db.insert_mapping(&drama()).await.unwrap();
        assert!(db.watched_playlists().await.unwrap().is_empty());
        db.set_match_options(
            mapping.id,
            &MatchOptions {
                playlist_id: Some("PLmapped000000".to_owned()),
                ..MatchOptions::default()
            },
        )
        .await
        .unwrap();
        db.request_playlist("PLasked0000000").await.unwrap();
        db.request_playlist("PLmapped000000").await.unwrap();
        assert_eq!(
            db.watched_playlists().await.unwrap(),
            ["PLasked0000000", "PLmapped000000"]
        );

        // A listing settles the request.
        db.replace_playlist("PLasked0000000", &[item("a")])
            .await
            .unwrap();
        assert_eq!(db.requested_playlists().await.unwrap(), ["PLmapped000000"]);
        db.replace_playlist("PLmapped000000", &[]).await.unwrap();
        assert_eq!(db.watched_playlists().await.unwrap(), ["PLmapped000000"]);

        sqlx::query("UPDATE mappings SET active = 0")
            .execute(db.pool())
            .await
            .unwrap();
        assert!(db.watched_playlists().await.unwrap().is_empty());
    }
}
