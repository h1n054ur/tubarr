//! Back episodes for shows without an official playlist, from each channel's own search tab.
//!
//! The daily list reads only a channel's newest uploads, and a busy channel's newest few hundred
//! cover days, so an older show is never seen. A backfill searches each of the show's channels
//! for its title and aliases (`/channel/<id>/search?query=...`) and stores what it finds as channel
//! uploads for the matcher. A channel that posts many short clips tagged with the show's name
//! can fill a whole search with them, pushing the full episodes past the results read, so a
//! search that comes back full is followed by one for "<name> Episode". It runs when a show is mapped, made active again, or its channels or
//! aliases change, then once a day until the matcher has released every wanted episode, or every
//! back episode the mapping's backfill limit keeps, or `backfill_days` pass. A limit of 0 keeps
//! none, so such a mapping is never searched. A search that fails for a passing reason (network, 5xx, timeout, a busy
//! database) gets one
//! retry after [`RETRY_AFTER`]. A throttle waits the full day, since the limiter pause covers it.
//! Each search is one lookup on the shared limiter, shows are spaced apart,
//! and nothing runs while lookups are paused.

use std::time::Duration;

use serde::Serialize;

use super::{
    IDLE_CHECK, LISTING_START_DELAY, ListingRun, WatchError, Watcher, check_channel, jitter,
    listing,
};
use crate::{
    resolver::ResolveError,
    store::{BackfillJob, SeenVideo, StoreError, VideoSource, unix_now},
};

/// Names searched per channel and run: the title and the first aliases. Each is one lookup, or
/// two when the first search comes back full.
pub const MAX_QUERIES: usize = 3;

/// How long a backfill that failed for a passing reason waits for its one retry.
pub const RETRY_AFTER: Duration = Duration::from_secs(60 * 60);

/// What the Health page shows for one backfill. Times are Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackfillHealth {
    pub mapping_id: i64,
    pub title: String,
    pub last_run_at: Option<i64>,
    /// Null once the backfill is done or its days are over. In the past means due now.
    pub next_run_at: Option<i64>,
    /// Videos the last run found.
    pub results_found: Option<i64>,
    pub done: bool,
    pub last_error: Option<String>,
}

/// What one turn of the backfill loop did.
#[derive(Debug)]
pub enum BackfillTurn {
    /// Lookups are paused for this long, so nothing was asked. The backfill stays due.
    Held(Duration),
    /// Nothing is due.
    Idle,
    Searched {
        mapping_id: i64,
        result: Result<ListingRun, WatchError>,
    },
}

impl Watcher {
    /// Searches one channel for `query`, waiting on the shared limiter first, and stores the
    /// results that are that channel's uploads.
    pub async fn search_channel(
        &self,
        channel_id: &str,
        query: &str,
    ) -> Result<ListingRun, WatchError> {
        self.search_channel_full(channel_id, query)
            .await
            .map(|(run, _)| run)
    }

    /// [`Self::search_channel`], and whether the search read as many results as it may, so
    /// later ones may have been cut off.
    async fn search_channel_full(
        &self,
        channel_id: &str,
        query: &str,
    ) -> Result<(ListingRun, bool), WatchError> {
        check_channel(channel_id)?;
        let size = self.settings.backfill_size;
        let args = listing::search_args(&self.ytdlp, channel_id, query, size);
        let output = self.lookup(args, channel_id).await?;
        let entries = listing::parse(&output)?;
        let full = entries.len() >= usize::try_from(size).unwrap_or(usize::MAX);
        let mut run = ListingRun { seen: 0, new: 0 };
        for entry in entries {
            // The search tab lists the channel's own uploads. Anything else is not trusted.
            if entry.channel_id.as_deref().is_some_and(|c| c != channel_id) {
                continue;
            }
            let seen = SeenVideo {
                video_id: entry.video_id,
                channel_id: channel_id.to_owned(),
                title: entry.title,
                published_at: entry.published_at,
                published_approx: entry.published_at.is_some(),
                duration_secs: entry.duration,
                source: VideoSource::Listing,
            };
            run.seen += 1;
            if self.db.record_sighting(&seen, entry.availability).await? {
                run.new += 1;
            }
        }
        Ok((run, full))
    }

    /// One turn: holds off while lookups are paused, else runs the backfill due first.
    pub async fn backfill_turn(&self) -> Result<BackfillTurn, WatchError> {
        if self.settings.backfill_days == 0 {
            return Ok(BackfillTurn::Idle);
        }
        let now = unix_now();
        let jobs = self.db.backfill_jobs().await?;
        self.db.sync_backfills(&jobs, now).await?;
        if let Some(pause) = self.limiter.status().paused_for {
            return Ok(BackfillTurn::Held(pause));
        }
        let interval = i64::try_from(self.settings.backfill_interval.as_secs()).unwrap_or(i64::MAX);
        let due = self
            .db
            .due_backfills(now, interval, self.settings.backfill_days)
            .await?;
        let Some(job) = due
            .iter()
            .find_map(|d| jobs.iter().find(|j| j.mapping_id == d.mapping_id))
        else {
            return Ok(BackfillTurn::Idle);
        };
        let mut total = ListingRun { seen: 0, new: 0 };
        for channel in &job.channel_ids {
            for name in queries(job) {
                let mut query = Some(name.clone());
                while let Some(q) = query.take() {
                    if let Some(pause) = self.limiter.status().paused_for {
                        return Ok(BackfillTurn::Held(pause));
                    }
                    match self.search_channel_full(channel, &q).await {
                        Ok((run, full)) => {
                            total.seen += run.seen;
                            total.new += run.new;
                            // A search that was not full read every result, the episodes too.
                            if full && q == name {
                                query = episode_query(&name);
                            }
                        }
                        Err(e) => {
                            let why = e.to_string();
                            let retry_in = is_transient(&e)
                                .then(|| i64::try_from(RETRY_AFTER.as_secs()).unwrap_or(i64::MAX));
                            self.db
                                .record_backfill(job.mapping_id, unix_now(), Err(&why), retry_in)
                                .await?;
                            return Ok(BackfillTurn::Searched {
                                mapping_id: job.mapping_id,
                                result: Err(e),
                            });
                        }
                    }
                }
            }
        }
        self.db
            .record_backfill(job.mapping_id, unix_now(), Ok(total.seen), None)
            .await?;
        if total.seen > 0 {
            self.notify_found();
        }
        Ok(BackfillTurn::Searched {
            mapping_id: job.mapping_id,
            result: Ok(total),
        })
    }

    /// Every backfill with its last result and next run, for the Health page.
    pub async fn backfill_health(&self) -> Result<Vec<BackfillHealth>, StoreError> {
        let interval = i64::try_from(self.settings.backfill_interval.as_secs()).unwrap_or(i64::MAX);
        Ok(self
            .db
            .backfill_rows()
            .await?
            .into_iter()
            .map(|row| BackfillHealth {
                next_run_at: row.next_run_at(interval, self.settings.backfill_days),
                mapping_id: row.mapping_id,
                title: row.title,
                last_run_at: row.last_run_at,
                results_found: row.last_found,
                done: row.done,
                last_error: row.last_error,
            })
            .collect())
    }

    pub(super) async fn backfill_loop(&self) {
        if self.settings.backfill_days == 0 {
            return;
        }
        tokio::time::sleep(LISTING_START_DELAY).await;
        let spacing = self.settings.backfill_spacing;
        loop {
            let delay = match self.backfill_turn().await {
                Ok(BackfillTurn::Held(pause)) => pause,
                Ok(BackfillTurn::Idle) => IDLE_CHECK,
                Ok(BackfillTurn::Searched {
                    mapping_id,
                    result: Ok(run),
                }) => {
                    tracing::info!(
                        mapping = mapping_id,
                        videos = run.seen,
                        new = run.new,
                        "channel search backfill done"
                    );
                    spacing + jitter(spacing)
                }
                Ok(BackfillTurn::Searched {
                    mapping_id,
                    result: Err(e),
                }) => {
                    let when = if is_transient(&e) {
                        "trying once more in an hour"
                    } else {
                        "trying again tomorrow"
                    };
                    tracing::warn!(mapping = mapping_id, error = %e, "channel search backfill failed, {when}");
                    spacing + jitter(spacing)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "cannot read the backfills");
                    IDLE_CHECK
                }
            };
            tokio::time::sleep(delay).await;
        }
    }
}

/// A failure that is likely gone in an hour: a timeout, a network error, a 5xx or a database
/// another writer held too long. A throttle (429, bot check) is not, and neither is a bad answer
/// such as a 404.
fn is_transient(e: &WatchError) -> bool {
    match e {
        WatchError::Http(e) => e.is_timeout() || e.is_connect(),
        WatchError::Store(StoreError::Sql(e)) => e
            .as_database_error()
            .and_then(|db| db.code())
            .is_some_and(|code| is_busy_code(&code)),
        WatchError::Status(code) => (500..600).contains(code),
        WatchError::Listing(ResolveError::Timeout(_)) => true,
        WatchError::Listing(ResolveError::Failed { message, .. }) => {
            let lower = message.to_lowercase();
            if lower.contains("http error 4") {
                return false;
            }
            [
                "http error 5",
                "timed out",
                "timeout",
                "connection reset",
                "connection refused",
                "connection aborted",
                "remote end closed",
                "temporary failure in name resolution",
                "name or service not known",
                "network is unreachable",
                "urlopen error",
                "transporterror",
            ]
            .iter()
            .any(|n| lower.contains(n))
        }
        _ => false,
    }
}

/// SQLITE_BUSY (5) or SQLITE_LOCKED (6), extended codes such as BUSY_SNAPSHOT (517) included:
/// the low byte is the primary code.
fn is_busy_code(code: &str) -> bool {
    code.parse::<i32>().is_ok_and(|c| matches!(c & 0xff, 5 | 6))
}

/// The title, then the aliases, without blanks or repeats (ignoring case), at most
/// [`MAX_QUERIES`].
pub fn queries(job: &BackfillJob) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in std::iter::once(&job.title).chain(&job.aliases) {
        let name = name.trim();
        if !name.is_empty() && !out.iter().any(|o| o.eq_ignore_ascii_case(name)) {
            out.push(name.to_owned());
        }
    }
    out.truncate(MAX_QUERIES);
    out
}

/// `<name> Episode`, which ranks full episodes above the clips. `None` when the name already
/// says episode.
pub fn episode_query(name: &str) -> Option<String> {
    (!name
        .to_lowercase()
        .split_whitespace()
        .any(|w| w == "episode"))
    .then(|| format!("{name} Episode"))
}

#[cfg(test)]
mod tests {
    use sqlx::Connection;

    use super::*;

    fn failed(message: &str) -> WatchError {
        WatchError::Listing(ResolveError::Failed {
            exit_code: Some(1),
            message: message.to_owned(),
        })
    }

    #[test]
    fn only_passing_failures_get_an_early_retry() {
        assert!(is_transient(&WatchError::Listing(ResolveError::Timeout(
            Duration::from_secs(60)
        ))));
        assert!(is_transient(&failed(
            "ERROR: Unable to download webpage: HTTP Error 503: Service Unavailable"
        )));
        assert!(is_transient(&failed(
            "ERROR: Unable to download webpage: <urlopen error [Errno -3] Temporary failure in name resolution>"
        )));
        assert!(is_transient(&WatchError::Status(502)));
        assert!(!is_transient(&WatchError::Status(404)));
        assert!(!is_transient(&failed(
            "ERROR: Unable to download webpage: HTTP Error 404: Not Found"
        )));
        assert!(!is_transient(&WatchError::Listing(
            ResolveError::RateLimited("HTTP Error 429".to_owned())
        )));
        assert!(!is_transient(&WatchError::Listing(ResolveError::BotCheck(
            "confirm you're not a bot".to_owned()
        ))));
        assert!(!is_transient(&failed("ERROR: something odd")));
    }

    #[test]
    fn a_busy_or_locked_database_gets_an_early_retry() {
        for code in ["5", "517", "6", "262"] {
            assert!(is_busy_code(code), "{code}");
        }
        for code in ["1", "19", "2067", "nope"] {
            assert!(!is_busy_code(code), "{code}");
        }
    }

    #[tokio::test]
    async fn a_write_blocked_by_another_writer_is_transient() {
        let dir = tempfile::tempdir().unwrap();
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(dir.path().join("busy.db"))
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(Duration::ZERO);
        let mut holder = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (x INTEGER)")
            .execute(&mut holder)
            .await
            .unwrap();
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut holder)
            .await
            .unwrap();
        let mut other = sqlx::SqliteConnection::connect_with(&options)
            .await
            .unwrap();
        let e = sqlx::query("INSERT INTO t VALUES (1)")
            .execute(&mut other)
            .await
            .unwrap_err();
        assert!(is_transient(&WatchError::Store(StoreError::Sql(e))));
        assert!(!is_transient(&WatchError::Store(StoreError::Sql(
            sqlx::Error::RowNotFound
        ))));
    }

    #[test]
    fn the_episode_query_adds_the_word_once() {
        assert_eq!(
            episode_query("Bombay Tailors").as_deref(),
            Some("Bombay Tailors Episode")
        );
        assert_eq!(episode_query("Last Episode Show"), None);
    }

    #[test]
    fn queries_are_the_title_then_new_aliases() {
        let job = BackfillJob {
            mapping_id: 1,
            title: "Dar E Nijaat".to_owned(),
            channel_ids: vec![],
            limit: None,
            aliases: vec![
                " dar e nijaat ".to_owned(),
                String::new(),
                "Darr E Nijaat".to_owned(),
                "Dar-e-Nijaat".to_owned(),
                "Fourth".to_owned(),
            ],
        };
        assert_eq!(
            queries(&job),
            ["Dar E Nijaat", "Darr E Nijaat", "Dar-e-Nijaat"]
        );
    }
}
