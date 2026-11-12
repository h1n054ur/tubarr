//! Unmatched videos: what the matcher saw for a mapping but did not release, and why.

use std::collections::HashSet;

use super::{Db, MappingKind, StoreError, channel_videos::published_at_sql, unix_now};

/// Why a video has no release.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type, serde::Serialize)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum UnmatchedState {
    /// Waiting for metadata (duration or height). The next run checks again.
    Pending,
    /// Another upload of the same episode won.
    Duplicate,
    /// The title or the facts do not say for certain. A pin settles it.
    Unsure,
    /// One part of a split episode. Joined parts are not released yet.
    Partial,
    /// Older than the back episodes the mapping's backfill limit keeps. Never looked up or
    /// released. A pin or a higher limit brings it back.
    Capped,
}

/// One note the matcher writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUnmatched {
    pub video_id: String,
    pub state: UnmatchedState,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub part: Option<i64>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Unmatched {
    pub id: i64,
    pub mapping_id: i64,
    pub video_id: String,
    pub state: UnmatchedState,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub part: Option<i64>,
    pub reason: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Totals over the active mappings, for the Health page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct MatchCounts {
    /// Releases written by any matcher.
    pub released: u64,
    /// Notes waiting for metadata.
    pub pending: u64,
    /// Every other note: duplicates, unsure titles, parts.
    pub unmatched: u64,
    /// Back episodes the backfill limit leaves out on purpose, kept apart so a long back
    /// catalogue does not read as a matching problem.
    pub capped: u64,
}

impl Db {
    /// Makes `notes` the full list for a mapping: new ones are added, changed ones updated (their
    /// `created_at` kept), and notes for videos not in the list removed. Unchanged rows are not
    /// touched. With a note retention ([`Db::with_note_retention`]) notes for videos uploaded
    /// before it are left out too, except `pending` ones, which the lookups work through. The
    /// rule goes by the upload alone, so a dropped note never comes back on a later run. Returns
    /// how many rows were added or changed.
    pub async fn replace_unmatched(
        &self,
        mapping_id: i64,
        notes: &[NewUnmatched],
    ) -> Result<u64, StoreError> {
        let now = unix_now();
        // Reserve write intent before retention reads to avoid stale WAL snapshot upgrades.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let old: HashSet<String> = match self.note_retention_secs {
            Some(retention) => {
                let ids: Vec<&str> = notes
                    .iter()
                    .filter(|n| n.state != UnmatchedState::Pending)
                    .map(|n| n.video_id.as_str())
                    .collect();
                sqlx::query_scalar::<_, String>(concat!(
                    "SELECT c.video_id FROM channel_videos c \
                     LEFT JOIN videos v ON v.video_id = c.video_id \
                     WHERE c.video_id IN (SELECT value FROM json_each(?)) AND ",
                    published_at_sql!(),
                    " < ?"
                ))
                .bind(sqlx::types::Json(&ids))
                .bind(now.saturating_sub(retention))
                .fetch_all(&mut *tx)
                .await?
                .into_iter()
                .collect()
            }
            None => HashSet::new(),
        };
        #[cfg(test)]
        snapshot_tests::after_retention_read().await;
        let notes: Vec<&NewUnmatched> = notes
            .iter()
            .filter(|n| n.state == UnmatchedState::Pending || !old.contains(&n.video_id))
            .collect();
        let mut changed = 0;
        for note in &notes {
            let done = sqlx::query(
                "INSERT INTO unmatched (mapping_id, video_id, state, season, episode, part, reason, \
                 created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
                 ON CONFLICT (mapping_id, video_id) DO UPDATE SET state = excluded.state, \
                 season = excluded.season, episode = excluded.episode, part = excluded.part, \
                 reason = excluded.reason, updated_at = excluded.updated_at \
                 WHERE state IS NOT excluded.state OR season IS NOT excluded.season \
                 OR episode IS NOT excluded.episode OR part IS NOT excluded.part \
                 OR reason IS NOT excluded.reason",
            )
            .bind(mapping_id)
            .bind(&note.video_id)
            .bind(note.state)
            .bind(note.season)
            .bind(note.episode)
            .bind(note.part)
            .bind(&note.reason)
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            changed += done.rows_affected();
        }
        let keep: Vec<&str> = notes.iter().map(|n| n.video_id.as_str()).collect();
        sqlx::query(
            "DELETE FROM unmatched WHERE mapping_id = ? \
             AND video_id NOT IN (SELECT value FROM json_each(?))",
        )
        .bind(mapping_id)
        .bind(sqlx::types::Json(&keep))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(changed)
    }

    /// The notes of a mapping, by season, episode, part and video.
    pub async fn unmatched(&self, mapping_id: i64) -> Result<Vec<Unmatched>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT * FROM unmatched WHERE mapping_id = ? \
             ORDER BY season, episode, part, video_id",
        )
        .bind(mapping_id)
        .fetch_all(&self.pool)
        .await?)
    }

    /// How many videos wait for a match on active mappings, per reason, most common first.
    /// Duplicates and capped videos are left out: another upload already won, or the backfill
    /// limit leaves them out on purpose, so nothing waits on them.
    pub async fn unmatched_waiting(&self) -> Result<Vec<(UnmatchedState, i64)>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT u.state, COUNT(*) AS n FROM unmatched u JOIN mappings m ON m.id = u.mapping_id \
             WHERE m.active AND u.state NOT IN ('duplicate', 'capped') GROUP BY u.state ORDER BY n DESC, u.state",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// Videos noted as waiting for metadata on an active mapping, oldest first: by when the
    /// watcher first saw them, else (a pinned video it never saw) by the note.
    pub async fn pending_videos(&self) -> Result<Vec<String>, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT u.video_id FROM unmatched u JOIN mappings m ON m.id = u.mapping_id \
             LEFT JOIN channel_videos c ON c.video_id = u.video_id \
             WHERE u.state = 'pending' AND m.active \
             GROUP BY u.video_id ORDER BY min(coalesce(c.first_seen, u.created_at)), u.video_id",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn match_counts(&self) -> Result<MatchCounts, StoreError> {
        self.counts(None).await
    }

    /// [`Db::match_counts`] over the active film mappings only.
    pub async fn film_match_counts(&self) -> Result<MatchCounts, StoreError> {
        self.counts(Some(MappingKind::Film)).await
    }

    async fn counts(&self, kind: Option<MappingKind>) -> Result<MatchCounts, StoreError> {
        let (released, pending, unmatched, capped): (i64, i64, i64, i64) = sqlx::query_as(
            "SELECT \
               (SELECT count(*) FROM releases r JOIN mappings m ON m.id = r.mapping_id \
                  WHERE m.active AND (?1 IS NULL OR m.kind = ?1)), \
               (SELECT count(*) FROM unmatched u JOIN mappings m ON m.id = u.mapping_id \
                  WHERE m.active AND (?1 IS NULL OR m.kind = ?1) AND u.state = 'pending'), \
               (SELECT count(*) FROM unmatched u JOIN mappings m ON m.id = u.mapping_id \
                  WHERE m.active AND (?1 IS NULL OR m.kind = ?1) \
                    AND u.state NOT IN ('pending', 'capped')), \
               (SELECT count(*) FROM unmatched u JOIN mappings m ON m.id = u.mapping_id \
                  WHERE m.active AND (?1 IS NULL OR m.kind = ?1) AND u.state = 'capped')",
        )
        .bind(kind)
        .fetch_one(&self.pool)
        .await?;
        let count = |n: i64| u64::try_from(n).unwrap_or_default();
        Ok(MatchCounts {
            released: count(released),
            pending: count(pending),
            unmatched: count(unmatched),
            capped: count(capped),
        })
    }

    /// Hides (or shows again) one note in the UI. `false` when no such note exists.
    pub async fn set_unmatched_ignored(&self, id: i64, ignored: bool) -> Result<bool, StoreError> {
        let done = sqlx::query("UPDATE unmatched SET ignored = ? WHERE id = ?")
            .bind(ignored)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Removes one release row. `false` when it did not exist.
    pub async fn delete_release(&self, id: i64) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM releases WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
#[path = "unmatched_snapshot_tests.rs"]
mod snapshot_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{mappings::tests::drama, test_db};

    fn note(video_id: &str, state: UnmatchedState, reason: &str) -> NewUnmatched {
        NewUnmatched {
            video_id: video_id.to_owned(),
            state,
            season: Some(1),
            episode: Some(25),
            part: None,
            reason: reason.to_owned(),
        }
    }

    #[tokio::test]
    async fn replace_adds_updates_keeps_and_removes() {
        let (db, _dir) = test_db::fresh().await;
        let m = db.insert_mapping(&drama()).await.unwrap().id;
        let first = [
            note("a", UnmatchedState::Pending, "duration unknown"),
            note("b", UnmatchedState::Duplicate, "re-upload"),
        ];
        assert_eq!(db.replace_unmatched(m, &first).await.unwrap(), 2);
        // Same list again: nothing changes.
        assert_eq!(db.replace_unmatched(m, &first).await.unwrap(), 0);
        let before = db.unmatched(m).await.unwrap();

        let second = [note("a", UnmatchedState::Unsure, "too short")];
        assert_eq!(db.replace_unmatched(m, &second).await.unwrap(), 1);
        let after = db.unmatched(m).await.unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].state, UnmatchedState::Unsure);
        assert_eq!(after[0].created_at, before[0].created_at);

        db.replace_unmatched(m, &[]).await.unwrap();
        assert!(db.unmatched(m).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn waiting_counts_leave_duplicates_out() {
        let (db, _dir) = test_db::fresh().await;
        let m = db.insert_mapping(&drama()).await.unwrap().id;
        assert!(db.unmatched_waiting().await.unwrap().is_empty());
        let notes = [
            note("a", UnmatchedState::Unsure, "x"),
            note("b", UnmatchedState::Unsure, "y"),
            note("c", UnmatchedState::Pending, "z"),
            note("d", UnmatchedState::Duplicate, "re-upload"),
        ];
        db.replace_unmatched(m, &notes).await.unwrap();
        assert_eq!(
            db.unmatched_waiting().await.unwrap(),
            [(UnmatchedState::Unsure, 2), (UnmatchedState::Pending, 1)]
        );
    }

    #[tokio::test]
    async fn match_counts_keep_capped_apart() {
        let (db, _dir) = test_db::fresh().await;
        let m = db.insert_mapping(&drama()).await.unwrap().id;
        let notes = [
            note("a", UnmatchedState::Unsure, "x"),
            note("b", UnmatchedState::Pending, "y"),
            note("c", UnmatchedState::Capped, "old"),
            note("d", UnmatchedState::Capped, "older"),
        ];
        db.replace_unmatched(m, &notes).await.unwrap();
        assert_eq!(
            db.match_counts().await.unwrap(),
            MatchCounts {
                released: 0,
                pending: 1,
                unmatched: 1,
                capped: 2,
            }
        );
    }

    #[tokio::test]
    async fn ignored_flag_survives_a_rewrite() {
        let (db, _dir) = test_db::fresh().await;
        let m = db.insert_mapping(&drama()).await.unwrap().id;
        db.replace_unmatched(m, &[note("a", UnmatchedState::Unsure, "short")])
            .await
            .unwrap();
        let id = db.unmatched(m).await.unwrap()[0].id;
        assert!(db.set_unmatched_ignored(id, true).await.unwrap());
        db.replace_unmatched(m, &[note("a", UnmatchedState::Duplicate, "re-upload")])
            .await
            .unwrap();
        let ignored: bool = sqlx::query_scalar("SELECT ignored FROM unmatched WHERE id = ?")
            .bind(id)
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert!(ignored);
        assert!(!db.set_unmatched_ignored(9999, true).await.unwrap());
    }
}
