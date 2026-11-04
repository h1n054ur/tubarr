//! Channel search backfill state: which show mappings still get a daily channel search for their
//! back episodes, and when each last ran.

use std::collections::HashSet;

use super::{Db, MappingKind, StoreError};

/// What one backfill searches for: a show mapping without an official playlist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackfillJob {
    pub mapping_id: i64,
    pub title: String,
    pub channel_ids: Vec<String>,
    pub aliases: Vec<String>,
    /// The mapping's backfill limit, see [`super::Mapping::backfill_limit`].
    pub limit: Option<i64>,
}

impl BackfillJob {
    /// Changes whenever the search or the limit would change, which starts the backfill over.
    fn key(&self) -> String {
        let key = format!(
            "{}\n{}\n{}",
            self.title,
            self.channel_ids.join(","),
            self.aliases.join("\n")
        );
        // Without a limit the key stays what it was before limits, so no backfill starts over.
        match self.limit {
            Some(n) => format!("{key}\nlimit {n}"),
            None => key,
        }
    }
}

/// A backfill that is due, from [`Db::due_backfills`].
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DueBackfill {
    pub mapping_id: i64,
    pub started_at: i64,
    pub last_run_at: Option<i64>,
}

/// One backfill row with its mapping's title, for the Health page. Times are Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct BackfillRow {
    pub mapping_id: i64,
    pub title: String,
    pub started_at: i64,
    pub last_run_at: Option<i64>,
    pub last_found: Option<i64>,
    pub last_error: Option<String>,
    pub done: bool,
    pub retry_at: Option<i64>,
}

impl BackfillRow {
    /// When the backfill runs next with the given interval and length, or none once it is done
    /// or its `days` are over. A time in the past means it is due now.
    pub fn next_run_at(&self, interval_secs: i64, days: u32) -> Option<i64> {
        if self.done {
            return None;
        }
        let next = match (self.retry_at, self.last_run_at) {
            (Some(retry), _) => retry,
            (None, Some(last)) => last.saturating_add(interval_secs),
            (None, None) => self.started_at,
        };
        (next < self.started_at.saturating_add(i64::from(days) * 86_400)).then_some(next)
    }
}

impl Db {
    /// Every backfill row, by mapping title.
    pub async fn backfill_rows(&self) -> Result<Vec<BackfillRow>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT b.mapping_id, m.title, b.started_at, b.last_run_at, b.last_found, \
               b.last_error, b.done, b.retry_at \
             FROM mapping_backfill b JOIN mappings m ON m.id = b.mapping_id \
             ORDER BY m.title, b.mapping_id",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// The active show mappings with channels and no playlist: those a channel search helps. A
    /// mapping that releases no back episodes (a backfill limit of 0) has nothing to search for.
    pub async fn backfill_jobs(&self) -> Result<Vec<BackfillJob>, StoreError> {
        let mut jobs = Vec::new();
        for m in self.mappings().await? {
            let has_playlist = m
                .playlist_id
                .as_deref()
                .is_some_and(|p| !p.trim().is_empty());
            if m.kind != MappingKind::Show
                || !m.active
                || m.channel_ids.is_empty()
                || has_playlist
                || m.backfill_limit == Some(0)
            {
                continue;
            }
            jobs.push(BackfillJob {
                aliases: self.aliases(m.id).await?,
                mapping_id: m.id,
                title: m.title,
                channel_ids: m.channel_ids,
                limit: m.backfill_limit,
            });
        }
        Ok(jobs)
    }

    /// Keeps one backfill row per job: a new job, or one whose search changed, starts over at
    /// `now`. Rows of mappings no longer in `jobs` go, so a mapping made active again starts over.
    pub async fn sync_backfills(&self, jobs: &[BackfillJob], now: i64) -> Result<(), StoreError> {
        // IMMEDIATE takes the write lock up front, waiting out a busy writer. A plain BEGIN reads
        // first and then fails at once when another writer holds the lock.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let ids: HashSet<i64> = jobs.iter().map(|j| j.mapping_id).collect();
        let stored: Vec<i64> = sqlx::query_scalar("SELECT mapping_id FROM mapping_backfill")
            .fetch_all(&mut *tx)
            .await?;
        for gone in stored.into_iter().filter(|id| !ids.contains(id)) {
            sqlx::query("DELETE FROM mapping_backfill WHERE mapping_id = ?")
                .bind(gone)
                .execute(&mut *tx)
                .await?;
        }
        for job in jobs {
            sqlx::query(
                "INSERT INTO mapping_backfill (mapping_id, key, started_at) VALUES (?, ?, ?) \
                 ON CONFLICT (mapping_id) DO UPDATE SET key = excluded.key, \
                   started_at = excluded.started_at, last_run_at = NULL, last_error = NULL, \
                   last_found = NULL, done = 0, retry_at = NULL \
                 WHERE mapping_backfill.key <> excluded.key",
            )
            .bind(job.mapping_id)
            .bind(job.key())
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Backfills not done, started less than `days` ago, and either not run in the last
    /// `interval_secs` or with an early retry due. Never run ones first, then the longest waiting.
    pub async fn due_backfills(
        &self,
        now: i64,
        interval_secs: i64,
        days: u32,
    ) -> Result<Vec<DueBackfill>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT mapping_id, started_at, last_run_at FROM mapping_backfill \
             WHERE done = 0 AND started_at + ? > ? \
               AND (last_run_at IS NULL OR last_run_at + ? <= ? OR retry_at <= ?) \
             ORDER BY last_run_at IS NOT NULL, last_run_at, mapping_id",
        )
        .bind(i64::from(days) * 86_400)
        .bind(now)
        .bind(interval_secs)
        .bind(now)
        .bind(now)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Records a finished backfill run: how many videos it found, or why it failed. `retry_in`
    /// asks for one early retry that many seconds from now. It is ignored when this run was
    /// already that retry, so a failure that keeps coming back waits the full interval.
    pub async fn record_backfill(
        &self,
        mapping_id: i64,
        now: i64,
        result: Result<usize, &str>,
        retry_in: Option<i64>,
    ) -> Result<(), StoreError> {
        let (found, error) = match result {
            Ok(n) => (Some(i64::try_from(n).unwrap_or(i64::MAX)), None),
            Err(e) => (None, Some(e)),
        };
        sqlx::query(
            "UPDATE mapping_backfill SET last_run_at = ?, last_found = ?, last_error = ?, \
               retry_at = CASE WHEN retry_at IS NULL THEN ? END \
             WHERE mapping_id = ?",
        )
        .bind(now)
        .bind(found)
        .bind(error)
        .bind(retry_in.map(|secs| now.saturating_add(secs)))
        .bind(mapping_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Marks the backfill done once every episode in `wanted` has a release. Nothing to search
    /// for after that: new episodes come in through the feed.
    pub async fn finish_backfill_if_complete(
        &self,
        mapping_id: i64,
        wanted: &[(i64, i64)],
    ) -> Result<bool, StoreError> {
        if wanted.is_empty() {
            return Ok(false);
        }
        let released: HashSet<(i64, i64)> = sqlx::query_as(
            "SELECT season, episode FROM releases \
             WHERE mapping_id = ? AND season IS NOT NULL AND episode IS NOT NULL",
        )
        .bind(mapping_id)
        .fetch_all(&self.pool)
        .await?
        .into_iter()
        .collect();
        if !wanted.iter().all(|w| released.contains(w)) {
            return Ok(false);
        }
        self.finish_backfill(mapping_id).await
    }

    /// Marks the backfill done. `false` when it already was, or there is none.
    pub async fn finish_backfill(&self, mapping_id: i64) -> Result<bool, StoreError> {
        let done =
            sqlx::query("UPDATE mapping_backfill SET done = 1 WHERE mapping_id = ? AND done = 0")
                .bind(mapping_id)
                .execute(&self.pool)
                .await?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use crate::store::{MatchOptions, mappings::tests::drama, test_db};

    const DAY: i64 = 86_400;

    #[tokio::test]
    async fn backfills_start_over_when_the_search_changes_and_end_after_n_days() {
        let (db, _dir) = test_db::fresh().await;
        let mapping = db.insert_mapping(&drama()).await.unwrap();
        let jobs = db.backfill_jobs().await.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].title, "Kabhi Main Kabhi Tum");

        db.sync_backfills(&jobs, 1000).await.unwrap();
        let due = db.due_backfills(1000, DAY, 14).await.unwrap();
        assert_eq!(due[0].mapping_id, mapping.id);
        assert_eq!(due[0].last_run_at, None);

        // Once a day.
        db.record_backfill(mapping.id, 1000, Ok(3), None)
            .await
            .unwrap();
        assert!(
            db.due_backfills(1000 + DAY - 1, DAY, 14)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db.due_backfills(1000 + DAY, DAY, 14).await.unwrap().len(),
            1
        );
        // A sync with the same search keeps the turn.
        db.sync_backfills(&jobs, 5000).await.unwrap();
        assert!(db.due_backfills(5000, DAY, 14).await.unwrap().is_empty());
        // N days after the start it stops.
        assert!(
            db.due_backfills(1000 + 14 * DAY, DAY, 14)
                .await
                .unwrap()
                .is_empty()
        );

        // A new alias starts over.
        let mut changed = jobs.clone();
        changed[0].aliases.push("KMKT".to_owned());
        db.sync_backfills(&changed, 20 * DAY).await.unwrap();
        let due = db.due_backfills(20 * DAY, DAY, 14).await.unwrap();
        assert_eq!((due[0].started_at, due[0].last_run_at), (20 * DAY, None));

        // Every wanted episode released: done.
        assert!(
            !db.finish_backfill_if_complete(mapping.id, &[(1, 1)])
                .await
                .unwrap()
        );
        sqlx::query(
            "INSERT INTO releases (mapping_id, video_id, season, episode, video_title, language, \
             height, published_at, created_at) VALUES (?, 'abcdefghijk', 1, 1, 't', 'Urdu', 1080, 0, 0)",
        )
        .bind(mapping.id)
        .execute(db.pool())
        .await
        .unwrap();
        assert!(
            db.finish_backfill_if_complete(mapping.id, &[(1, 1)])
                .await
                .unwrap()
        );
        assert!(
            db.due_backfills(20 * DAY, DAY, 14)
                .await
                .unwrap()
                .is_empty()
        );

        // A playlist takes the mapping out, and its row goes.
        db.set_match_options(
            mapping.id,
            &MatchOptions {
                playlist_id: Some("PLsomething0000".to_owned()),
                ..MatchOptions::default()
            },
        )
        .await
        .unwrap();
        let jobs = db.backfill_jobs().await.unwrap();
        assert!(jobs.is_empty());
        db.sync_backfills(&jobs, 21 * DAY).await.unwrap();
        let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM mapping_backfill")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn a_backfill_limit_starts_the_search_over_and_zero_needs_none() {
        let (db, _dir) = test_db::fresh().await;
        let mapping = db.insert_mapping(&drama()).await.unwrap();
        db.sync_backfills(&db.backfill_jobs().await.unwrap(), 1000)
            .await
            .unwrap();
        db.record_backfill(mapping.id, 1000, Ok(3), None)
            .await
            .unwrap();
        assert!(db.finish_backfill(mapping.id).await.unwrap());
        assert!(!db.finish_backfill(mapping.id).await.unwrap());

        // A new limit is a new search: it starts over.
        db.set_backfill_limit(mapping.id, Some(5)).await.unwrap();
        let jobs = db.backfill_jobs().await.unwrap();
        assert_eq!(jobs[0].limit, Some(5));
        db.sync_backfills(&jobs, 2 * DAY).await.unwrap();
        let due = db.due_backfills(2 * DAY, DAY, 14).await.unwrap();
        assert_eq!((due[0].started_at, due[0].last_run_at), (2 * DAY, None));

        // A limit of 0 releases no back episode, so there is nothing to search for.
        db.set_backfill_limit(mapping.id, Some(0)).await.unwrap();
        assert!(db.backfill_jobs().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_passing_failure_retries_once_after_an_hour_then_waits_the_day() {
        const HOUR: i64 = 3600;
        let (db, _dir) = test_db::fresh().await;
        let mapping = db.insert_mapping(&drama()).await.unwrap();
        db.sync_backfills(&db.backfill_jobs().await.unwrap(), 0)
            .await
            .unwrap();

        db.record_backfill(mapping.id, 100, Err("timed out"), Some(HOUR))
            .await
            .unwrap();
        assert!(
            db.due_backfills(100 + HOUR - 1, DAY, 14)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db.due_backfills(100 + HOUR, DAY, 14).await.unwrap().len(),
            1
        );
        let row = &db.backfill_rows().await.unwrap()[0];
        assert_eq!(row.title, "Kabhi Main Kabhi Tum");
        assert_eq!(row.last_error.as_deref(), Some("timed out"));
        assert_eq!(row.next_run_at(DAY, 14), Some(100 + HOUR));

        // The retry fails too: no second early retry, the next run is a day later.
        let retried = 100 + HOUR;
        db.record_backfill(mapping.id, retried, Err("timed out"), Some(HOUR))
            .await
            .unwrap();
        assert!(
            db.due_backfills(retried + DAY - 1, DAY, 14)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            db.backfill_rows().await.unwrap()[0].next_run_at(DAY, 14),
            Some(retried + DAY)
        );

        // A success clears the error and the next failure gets its retry again.
        db.record_backfill(mapping.id, 2 * DAY, Ok(4), None)
            .await
            .unwrap();
        let row = &db.backfill_rows().await.unwrap()[0];
        assert_eq!((row.last_found, row.last_error.clone()), (Some(4), None));
        db.record_backfill(mapping.id, 3 * DAY, Err("HTTP Error 503"), Some(HOUR))
            .await
            .unwrap();
        assert_eq!(
            db.backfill_rows().await.unwrap()[0].retry_at,
            Some(3 * DAY + HOUR)
        );
        // Past its days there is no next run.
        assert_eq!(
            db.backfill_rows().await.unwrap()[0].next_run_at(DAY, 3),
            None
        );
    }
}
