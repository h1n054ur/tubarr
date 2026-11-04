//! The Newznab searches, `/play` starts and releases Tubarr pushed to the arrs, for the Activity
//! page. They are stored in the database (grabs have their own table) and dropped by housekeeping
//! after `[activity] retention`. Recording never waits on the database: events go through a queue
//! to one writer task, which stores whatever has piled up in one transaction.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, Method, header},
    middleware::Next,
    response::Response,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{mpsc, oneshot},
    time::Instant,
};

use crate::{
    metrics::METRICS,
    store::{Db, StoreError, StoredEvent, unix_now},
};

/// Events waiting for the writer. A burst past this (the database stuck for a long time) drops
/// the newest events rather than hold up a search or a play.
const QUEUE: usize = 1024;

/// Most events the writer stores in one transaction.
const BATCH: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Activity {
    /// Sonarr or Radarr asked the indexer for releases.
    Search {
        /// `search`, `tvsearch` or `movie`.
        function: String,
        /// The search terms as one line, e.g. `tvdbid=452001 season=1 ep=3`.
        query: String,
        results: usize,
    },
    /// Tubarr pushed a release it had just released (or a catch-up found) to Sonarr or Radarr.
    ArrPush {
        app: String,
        /// The release title, as the feed has it.
        release: String,
        /// `grabbed`, `pending` (held by a delay profile), `rejected` or `failed`.
        outcome: String,
        /// The arr's rejection reasons, or why the push failed.
        reasons: Vec<String>,
    },
    /// A player opened a `.strm`.
    Play {
        video_ids: String,
        client: Option<String>,
        status: u16,
    },
}

impl Activity {
    /// The `kind` the JSON carries, also the stored row's `kind`.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Search { .. } => "search",
            Self::ArrPush { .. } => "arr_push",
            Self::Play { .. } => "play",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Event {
    /// Unix seconds.
    pub at: i64,
    #[serde(flatten)]
    pub activity: Activity,
}

#[derive(Debug)]
enum Message {
    Event(Event),
    /// Answered once every event queued before it is stored.
    Flush(oneshot::Sender<()>),
}

#[derive(Debug)]
pub struct ActivityLog {
    queue: mpsc::Sender<Message>,
    db: Db,
}

impl ActivityLog {
    /// Starts the writer task on the current Tokio runtime. It ends when the log is dropped.
    pub fn new(db: Db) -> Self {
        let (queue, rx) = mpsc::channel(QUEUE);
        tokio::spawn(write(db.clone(), rx));
        Self { queue, db }
    }

    /// Queues an event stamped now. Never waits.
    pub fn record(&self, activity: Activity) {
        let event = Event {
            at: unix_now(),
            activity,
        };
        if let Err(mpsc::error::TrySendError::Full(_)) = self.queue.try_send(Message::Event(event))
        {
            tracing::warn!("activity queue full, an event was dropped");
        }
    }

    /// Waits until every event recorded so far is stored.
    pub async fn flush(&self) {
        let (done, wait) = oneshot::channel();
        if self.queue.send(Message::Flush(done)).await.is_ok() {
            let _ = wait.await;
        }
    }

    /// One page of events, newest first, optionally only one kind, and how many there are in all.
    /// Events recorded before the call are included.
    pub async fn page(
        &self,
        kind: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Event>, i64), StoreError> {
        self.flush().await;
        let rows = self.db.activity(kind, limit, offset).await?;
        let events = rows
            .into_iter()
            .filter_map(|row| {
                let activity = serde_json::from_str(&row.data)
                    .inspect_err(|e| tracing::warn!(error = %e, "unreadable activity event"))
                    .ok()?;
                Some(Event {
                    at: row.at,
                    activity,
                })
            })
            .collect();
        Ok((events, self.db.count_activity(kind).await?))
    }
}

/// The writer: takes what the queue holds (at most [`BATCH`]), stores it, answers the flushes
/// among it, and waits for more.
async fn write(db: Db, mut rx: mpsc::Receiver<Message>) {
    let mut messages = Vec::with_capacity(BATCH);
    while rx.recv_many(&mut messages, BATCH).await > 0 {
        let mut rows = Vec::new();
        let mut flushes = Vec::new();
        for message in messages.drain(..) {
            match message {
                Message::Event(e) => rows.push(StoredEvent {
                    at: e.at,
                    kind: e.activity.kind().to_owned(),
                    data: serde_json::to_string(&e.activity).unwrap_or_default(),
                }),
                Message::Flush(done) => flushes.push(done),
            }
        }
        if !rows.is_empty()
            && let Err(e) = db.insert_activity(&rows).await
        {
            tracing::warn!(error = %e, events = rows.len(), "cannot store activity events");
        }
        for done in flushes {
            let _ = done.send(());
        }
    }
}

/// The search terms of a Newznab request in a fixed order, apikey and paging left out.
pub fn search_query(params: &std::collections::HashMap<String, String>) -> String {
    const SHOWN: [&str; 9] = [
        "q", "tvdbid", "tmdbid", "imdbid", "rid", "tvmazeid", "season", "ep", "cat",
    ];
    SHOWN
        .iter()
        .filter_map(|name| {
            let v = params.get(*name)?.trim();
            (!v.is_empty()).then(|| format!("{name}={v}"))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a `/play` request starts playback rather than continuing it: no `Range`, or one from
/// byte 0. Players send many range reads per viewing, which would flood the log.
fn is_start(method: &Method, headers: &HeaderMap) -> bool {
    if method != Method::GET {
        return false;
    }
    match headers.get(header::RANGE).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(range) => range.trim().starts_with("bytes=0-"),
    }
}

/// Middleware for the play router: notes each start with its answer, and counts every answer, the
/// media bytes it sends and the time to its first bytes for the metrics.
pub async fn record_play(log: Arc<ActivityLog>, request: Request, next: Next) -> Response {
    let asked = Instant::now();
    let start = is_start(request.method(), request.headers());
    let video_ids = request
        .uri()
        .path()
        .strip_prefix("/play/")
        .unwrap_or_default()
        .to_owned();
    let client = request
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let response = next.run(request).await;
    let status = response.status();
    METRICS.play_request(status);
    if start {
        METRICS.play_starts.inc();
    }
    if start && !video_ids.is_empty() {
        log.record(Activity::Play {
            video_ids,
            client,
            status: status.as_u16(),
        });
    }
    // Only media bodies are counted. An error body keeps its exact length this way.
    if !status.is_success() {
        return response;
    }
    let (parts, body) = response.into_parts();
    let mut first = start.then_some(asked);
    let body = body.into_data_stream().inspect(move |chunk| {
        if let Ok(bytes) = chunk {
            METRICS.play_bytes.add(bytes.len() as u64);
            if let Some(asked) = first.take() {
                METRICS.play_first_byte_seconds.observe(asked.elapsed());
            }
        }
    });
    Response::from_parts(parts, Body::from_stream(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::store::test_db;

    fn search(n: usize) -> Activity {
        Activity::Search {
            function: "tvsearch".to_owned(),
            query: format!("tvdbid=452001 ep={n}"),
            results: n,
        }
    }

    #[tokio::test]
    async fn events_outlive_the_log_that_recorded_them() {
        let (db, _dir) = test_db::fresh().await;
        let log = ActivityLog::new(db.clone());
        for n in 0..30 {
            log.record(search(n));
        }
        log.record(Activity::Play {
            video_ids: "0eD-GQCutAU".to_owned(),
            client: Some("Jellyfin-Server/10.11.11".to_owned()),
            status: 206,
        });
        log.flush().await;
        drop(log);

        let restarted = ActivityLog::new(db);
        let (events, total) = restarted.page(None, 10, 0).await.unwrap();
        assert_eq!(total, 31);
        assert_eq!(events.len(), 10);
        assert_eq!(events[0].activity.kind(), "play");
        assert_eq!(events[1].activity, search(29));
        let (searches, total) = restarted.page(Some("search"), 5, 25).await.unwrap();
        assert_eq!(total, 30);
        assert_eq!(searches.last().unwrap().activity, search(0));
    }

    #[tokio::test]
    async fn recording_does_not_wait_for_a_busy_database() {
        let (db, _dir) = test_db::fresh().await;
        let log = ActivityLog::new(db.clone());
        // Another writer holds SQLite's write lock, so the activity writer has to wait.
        let mut held = db.pool().begin().await.unwrap();
        sqlx::query("INSERT INTO activity_events (at, kind, data) VALUES (0, 'search', '{}')")
            .execute(&mut *held)
            .await
            .unwrap();
        let started = std::time::Instant::now();
        for n in 0..100 {
            log.record(search(n));
        }
        assert!(started.elapsed() < std::time::Duration::from_millis(100));
        held.rollback().await.unwrap();

        let (events, total) = log.page(None, 1, 0).await.unwrap();
        assert_eq!(total, 100);
        assert_eq!(events[0].activity, search(99));
    }

    #[test]
    fn query_leaves_the_key_out() {
        let params = [
            ("apikey", "secret"),
            ("tvdbid", "452001"),
            ("ep", "3"),
            ("q", " "),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        assert_eq!(search_query(&params), "tvdbid=452001 ep=3");
    }

    #[test]
    fn only_first_reads_count_as_starts() {
        let mut headers = HeaderMap::new();
        assert!(is_start(&Method::GET, &headers));
        assert!(!is_start(&Method::HEAD, &headers));
        headers.insert(header::RANGE, "bytes=0-".parse().unwrap());
        assert!(is_start(&Method::GET, &headers));
        headers.insert(header::RANGE, "bytes=1000-".parse().unwrap());
        assert!(!is_start(&Method::GET, &headers));
    }
}
