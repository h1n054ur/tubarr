//! Jellyfin versions: merges each `<episode> - Audio only.strm` into its episode, so "Audio only"
//! shows as a version to pick, and splits them again when the audio file goes.
//!
//! Jellyfin 10.11 groups versions by name for films only, so a TV episode needs
//! `POST /Videos/MergeVersions`. That call makes the item with the widest video stream the
//! primary, and falls back to the lowest item id, so first a `PlaybackInfo` on the episode makes
//! Jellyfin probe its `.strm` and keep its 1920 width. Without it the audio file could end up the
//! primary.
//!
//! The hooks queue a job per episode file. One worker runs them: a merge waits until Jellyfin has
//! scanned both files, looking again after [`JellyfinConfig::merge_retry`], doubled each time, up
//! to [`JellyfinConfig::merge_attempts`] looks. A split runs at once. The queue is kept in the
//! database, so a restart loses none of it.
//!
//! Items are found by path in the episode list of their series, read with the API key alone: a
//! user's view can leave out whole libraries, and the list of every episode on the server is too
//! big to read each round. The series ids are kept, keyed by the series folder. An item's versions
//! are read the same way, from `GET /Items?ids=`: 10.11 answers `GET /Items/{id}` without a user
//! with 400.
//!
//! Nothing tells Jellyfin about a file Tubarr writes, so when a merge finds a file missing in a
//! series Jellyfin knows, it asks Jellyfin to refresh that series (never a library), once a job
//! and once a series a round, and looks again on the usual retry.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::{
    config::JellyfinConfig,
    hooks::audio_path,
    store::{Db, VersionAction, VersionJob},
};

/// The longest the worker sleeps before it looks at the queue again, in case a wake was missed.
const MAX_IDLE: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, thiserror::Error)]
pub enum JellyfinError {
    #[error("Jellyfin request {0} failed: {1}")]
    Http(String, String),
    #[error("Jellyfin answered {1} to {0}")]
    Status(String, StatusCode),
    #[error("Jellyfin has no administrator user to read items as; set jellyfin.user_id")]
    NoUser,
    #[error("{0} is not in Jellyfin yet")]
    NotScanned(String),
    #[error("after the merge, {0}")]
    Readback(String),
}

/// What the Health page shows.
#[derive(Debug, Clone, Default, Serialize)]
pub struct JellyfinHealth {
    /// Jobs waiting, retries included.
    pub queued: i64,
    /// The last failed try, with its Unix time.
    pub last_error: Option<String>,
    pub last_error_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ItemList {
    #[serde(default)]
    items: Vec<Item>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Item {
    id: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    media_sources: Vec<Source>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Source {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct User {
    id: String,
    #[serde(default)]
    policy: Option<Policy>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Policy {
    #[serde(default)]
    is_administrator: bool,
}

/// The Jellyfin client and the worker that runs the queue.
#[derive(Debug)]
pub struct Jellyfin {
    config: JellyfinConfig,
    db: Db,
    http: reqwest::Client,
    wake: Notify,
    /// The configured user, or the administrator found on the first merge. Only `PlaybackInfo`
    /// needs one.
    user: Mutex<Option<String>>,
    /// Series folder (as Jellyfin sees it) to series item id.
    series: Mutex<HashMap<String, String>>,
    /// The queued jobs (path and queue time) that have had their series refreshed.
    refreshed: Mutex<HashSet<(String, i64)>>,
    last_error: Mutex<Option<(i64, String)>>,
}

/// What one round of due jobs has read: each series' episodes, path to item id, whether the
/// series list was read again, and the series refreshed.
#[derive(Default)]
struct Round {
    series_listed: bool,
    episodes: HashMap<String, HashMap<String, String>>,
    refreshed: HashSet<String>,
}

/// What one try of a job came to.
enum Outcome {
    Done(&'static str),
    Retry(JellyfinError),
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// Item ids come with and without dashes, in either case.
fn same_id(a: &str, b: &str) -> bool {
    let plain = |s: &str| {
        s.chars()
            .filter(|c| *c != '-')
            .map(|c| c.to_ascii_lowercase())
            .collect::<String>()
    };
    plain(a) == plain(b)
}

impl Jellyfin {
    pub fn new(config: JellyfinConfig, db: Db) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()?;
        let user = Mutex::new(config.user_id.clone());
        Ok(Self {
            config,
            db,
            http,
            wake: Notify::new(),
            user,
            series: Mutex::new(HashMap::new()),
            refreshed: Mutex::new(HashSet::new()),
            last_error: Mutex::new(None),
        })
    }

    /// Where Jellyfin sees the file Tubarr calls `path`.
    fn jellyfin_path(&self, path: &Path) -> PathBuf {
        for (tubarr, jellyfin) in &self.config.library_paths {
            if let Ok(rest) = path.strip_prefix(tubarr) {
                return jellyfin.join(rest);
            }
        }
        path.to_path_buf()
    }

    /// Queues episode file `main` (as Tubarr sees it) for `action` and wakes the worker. A failure
    /// to store the job is logged: the file itself is already in place.
    pub async fn queue(&self, main: &Path, action: VersionAction) {
        let path = self.jellyfin_path(main);
        let shown = path.display().to_string();
        match self.db.queue_version_job(&shown, action, now_ms()).await {
            Ok(()) => self.wake.notify_one(),
            Err(e) => {
                tracing::warn!(file = shown, error = %e, "cannot queue a Jellyfin version job");
            }
        }
    }

    pub async fn health(&self) -> JellyfinHealth {
        let queued = self.db.version_jobs_queued().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cannot count the Jellyfin version jobs");
            0
        });
        let last = self
            .last_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        JellyfinHealth {
            queued,
            last_error_at: last.as_ref().map(|(at, _)| *at),
            last_error: last.map(|(_, e)| e),
        }
    }

    /// Runs due jobs until Tubarr stops, sleeping until the next is due or a job is queued.
    pub async fn run(self: Arc<Self>) {
        loop {
            self.run_due().await;
            let wait = match self.db.next_version_job_at().await {
                Ok(Some(at)) => {
                    Duration::from_millis(u64::try_from(at.saturating_sub(now_ms())).unwrap_or(0))
                        .min(MAX_IDLE)
                }
                Ok(None) => MAX_IDLE,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot read the Jellyfin version queue");
                    MAX_IDLE
                }
            };
            if wait.is_zero() {
                continue;
            }
            tokio::select! {
                () = tokio::time::sleep(wait) => {}
                () = self.wake.notified() => {}
            }
        }
    }

    /// Runs every job due now, listing the episodes of each series once for all of them.
    pub async fn run_due(&self) {
        let jobs = match self.db.due_version_jobs(now_ms()).await {
            Ok(jobs) if jobs.is_empty() => return,
            Ok(jobs) => jobs,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the Jellyfin version queue");
                return;
            }
        };
        let mut round = Round::default();
        for job in jobs {
            match self.try_job(&mut round, &job).await {
                Ok(Outcome::Done(what)) => {
                    tracing::info!(file = job.path, action = ?job.action, what, "Jellyfin versions done");
                    self.unqueue(&job).await;
                }
                Ok(Outcome::Retry(e)) | Err(e) => self.failed(&job, &e).await,
            }
        }
    }

    async fn unqueue(&self, job: &VersionJob) {
        self.refreshed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(job.path.clone(), job.queued_at_ms));
        if let Err(e) = self.db.unqueue_version_job(job).await {
            tracing::warn!(file = job.path, error = %e, "cannot take a job off the Jellyfin version queue");
        }
    }

    /// Counts a failed try and looks again later, or gives up after the last allowed one. A split
    /// gets the same retries, for when Jellyfin is down.
    async fn failed(&self, job: &VersionJob, e: &JellyfinError) {
        let attempts = u32::try_from(job.attempts)
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        let text = e.to_string();
        // Waiting for a scan is expected, so it is no error on the Health page until the end.
        let quiet = matches!(e, JellyfinError::NotScanned(_));
        if attempts >= self.config.merge_attempts {
            tracing::warn!(file = job.path, action = ?job.action, attempts, error = text, "Jellyfin versions given up on");
            self.remember(format!("{}: {text}", job.path));
            self.unqueue(job).await;
            return;
        }
        if !quiet {
            self.remember(format!("{}: {text}", job.path));
        }
        let wait = self.config.merge_retry * 2u32.saturating_pow(attempts - 1);
        tracing::info!(file = job.path, action = ?job.action, attempts, error = text, retry_in = ?wait, "Jellyfin versions not done yet");
        let next = now_ms().saturating_add(i64::try_from(wait.as_millis()).unwrap_or(i64::MAX));
        if let Err(e) = self.db.version_job_failed(job, &text, next).await {
            tracing::warn!(file = job.path, error = %e, "cannot count a failed Jellyfin version job");
        }
    }

    fn remember(&self, error: String) {
        let at = now_ms() / 1000;
        *self
            .last_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((at, error));
    }

    async fn try_job(&self, round: &mut Round, job: &VersionJob) -> Result<Outcome, JellyfinError> {
        match job.action {
            VersionAction::Merge => self.merge(round, job).await,
            VersionAction::Split => self.split(round, &job.path).await,
        }
    }

    /// Merges the audio only file of episode `path` into it, the episode first.
    async fn merge(&self, round: &mut Round, job: &VersionJob) -> Result<Outcome, JellyfinError> {
        let path = job.path.as_str();
        let Some(audio) = audio_path(Path::new(path)) else {
            return Ok(Outcome::Done("not an episode .strm"));
        };
        let audio = audio.display().to_string();
        let Some(video) = self.find(round, path).await? else {
            return Ok(self.not_scanned(round, job, path.to_owned()).await);
        };
        let video = video.as_str();
        // Read with the key alone, an audio file already merged is still listed as an item of its
        // own: the episode's versions tell whether it is merged, not the list.
        let audio_id = self.find(round, &audio).await?;
        let Some(item) = self.item(video).await? else {
            return Ok(self.not_scanned(round, job, path.to_owned()).await);
        };
        if merged(&item, video, &audio, audio_id.as_ref()) {
            return Ok(Outcome::Done("already merged"));
        }
        let Some(audio_id) = audio_id else {
            return Ok(self.not_scanned(round, job, audio).await);
        };
        // Jellyfin probes the .strm and keeps its width, so the episode wins the primary.
        let user = self.user().await?;
        self.send(
            Method::POST,
            &format!("Items/{video}/PlaybackInfo"),
            &[("userId", user.as_str())],
            Some(json!({})),
        )
        .await?;
        let ids = format!("{video},{audio_id}");
        self.send(Method::POST, "Videos/MergeVersions", &[("ids", &ids)], None)
            .await?;
        let item = self
            .item(video)
            .await?
            .ok_or_else(|| JellyfinError::Readback("the episode is gone".to_owned()))?;
        if merged(&item, video, &audio, Some(&audio_id)) {
            return Ok(Outcome::Done("merged"));
        }
        // The audio file came out first: undo, so the next try starts clean.
        self.send(
            Method::DELETE,
            &format!("Videos/{video}/AlternateSources"),
            &[],
            None,
        )
        .await?;
        Err(JellyfinError::Readback(format!(
            "the episode is not the first of its {} versions, split again",
            item.media_sources.len()
        )))
    }

    /// Takes every other version off episode `path`, so it keeps no link to a removed file.
    /// Jellyfin 10.11 fails the call on an item with no versions, so that is checked first.
    async fn split(&self, round: &mut Round, path: &str) -> Result<Outcome, JellyfinError> {
        let Some(video) = self.find(round, path).await? else {
            return Ok(Outcome::Done("not in Jellyfin"));
        };
        match self.item(&video).await? {
            None => return Ok(Outcome::Done("gone")),
            Some(item) if item.media_sources.len() < 2 => return Ok(Outcome::Done("no versions")),
            Some(_) => {}
        }
        match self
            .send(
                Method::DELETE,
                &format!("Videos/{video}/AlternateSources"),
                &[],
                None,
            )
            .await
        {
            Ok(_) => Ok(Outcome::Done("split")),
            Err(JellyfinError::Status(_, StatusCode::NOT_FOUND)) => Ok(Outcome::Done("gone")),
            Err(e) => Err(e),
        }
    }

    /// The item id of the file at `path`, from the episode list of its series. When a series id
    /// kept from an earlier round does not list the file, the series list is read again, once a
    /// round, in case the series was added again under a new id.
    async fn find(&self, round: &mut Round, path: &str) -> Result<Option<String>, JellyfinError> {
        for fresh in [false, true] {
            if fresh {
                if round.series_listed {
                    break;
                }
                self.list_series(round).await?;
            }
            let Some(series) = self.series_of(path) else {
                continue;
            };
            if !round.episodes.contains_key(&series) {
                let episodes = self
                    .list(&[
                        ("parentId", series.as_str()),
                        ("includeItemTypes", "Episode"),
                    ])
                    .await?;
                round.episodes.insert(series.clone(), episodes);
            }
            if let Some(id) = round.episodes[&series].get(path) {
                return Ok(Some(id.clone()));
            }
        }
        Ok(None)
    }

    /// A retry for `missing`, after asking Jellyfin to refresh the series that should hold it:
    /// once for the job, and once a round for the series. A series Jellyfin does not know yet
    /// is left to its library scan. A refresh that fails is logged and asked for again next try.
    async fn not_scanned(&self, round: &mut Round, job: &VersionJob, missing: String) -> Outcome {
        let key = (job.path.clone(), job.queued_at_ms);
        let asked = self
            .refreshed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(&key);
        if let Some(series) = self.series_of(&missing).filter(|_| !asked) {
            if round.refreshed.contains(&series) {
                self.refreshed_for(key);
            } else {
                match self.refresh(&series).await {
                    Ok(()) => {
                        tracing::info!(
                            file = missing,
                            series,
                            "asked Jellyfin to refresh the series"
                        );
                        round.refreshed.insert(series);
                        self.refreshed_for(key);
                    }
                    Err(e) => {
                        tracing::warn!(file = missing, series, error = %e, "cannot ask Jellyfin to refresh the series");
                    }
                }
            }
        }
        Outcome::Retry(JellyfinError::NotScanned(missing))
    }

    fn refreshed_for(&self, key: (String, i64)) {
        self.refreshed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key);
    }

    /// Asks Jellyfin to look for new and changed files under series `id`, like "Scan for new and
    /// updated files" on it. Jellyfin queues it and answers at once.
    async fn refresh(&self, id: &str) -> Result<(), JellyfinError> {
        self.send(
            Method::POST,
            &format!("Items/{id}/Refresh"),
            &[
                ("recursive", "true"),
                ("metadataRefreshMode", "Default"),
                ("imageRefreshMode", "Default"),
                ("replaceAllMetadata", "false"),
                ("replaceAllImages", "false"),
            ],
            None,
        )
        .await
        .map(drop)
    }

    /// The kept id of the series whose folder holds `path`.
    fn series_of(&self, path: &str) -> Option<String> {
        let series = self
            .series
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Path::new(path)
            .ancestors()
            .skip(1)
            .find_map(|dir| series.get(dir.to_str()?).cloned())
    }

    async fn list_series(&self, round: &mut Round) -> Result<(), JellyfinError> {
        round.series_listed = true;
        let series = self.list(&[("includeItemTypes", "Series")]).await?;
        *self
            .series
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = series;
        Ok(())
    }

    /// Path to item id of the items `filter` picks, read with the API key alone, so no user's
    /// view hides a library. Alternate versions are listed as items of their own.
    async fn list(
        &self,
        filter: &[(&str, &str)],
    ) -> Result<HashMap<String, String>, JellyfinError> {
        let mut query = vec![
            ("recursive", "true"),
            ("fields", "Path"),
            ("enableImages", "false"),
            ("enableUserData", "false"),
            ("enableTotalRecordCount", "false"),
        ];
        query.extend_from_slice(filter);
        let list: ItemList = self.get("Items", &query).await?;
        Ok(list
            .items
            .into_iter()
            .filter_map(|i| Some((i.path?, i.id)))
            .collect())
    }

    async fn user(&self) -> Result<String, JellyfinError> {
        if let Some(user) = self
            .user
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
        {
            return Ok(user);
        }
        let users: Vec<User> = self.get("Users", &[]).await?;
        let admin = users
            .into_iter()
            .find(|u| u.policy.as_ref().is_some_and(|p| p.is_administrator))
            .ok_or(JellyfinError::NoUser)?;
        *self
            .user
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(admin.id.clone());
        Ok(admin.id)
    }

    /// Item `id` with its versions, or `None` when Jellyfin has no such item. Read with the API
    /// key alone, like the listings, so no user's view hides it.
    async fn item(&self, id: &str) -> Result<Option<Item>, JellyfinError> {
        let list: ItemList = self
            .get(
                "Items",
                &[
                    ("ids", id),
                    ("fields", "MediaSources,Path"),
                    ("enableImages", "false"),
                    ("enableUserData", "false"),
                    ("enableTotalRecordCount", "false"),
                ],
            )
            .await?;
        Ok(list.items.into_iter().find(|i| same_id(&i.id, id)))
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<T, JellyfinError> {
        let body = self.send(Method::GET, path, query, None).await?;
        serde_json::from_slice(&body)
            .map_err(|e| JellyfinError::Http(path.to_owned(), e.to_string()))
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Result<bytes::Bytes, JellyfinError> {
        let what = format!("{method} /{path}");
        let mut url = self
            .config
            .url
            .join(path)
            .map_err(|e| JellyfinError::Http(what.clone(), e.to_string()))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let mut req = self.http.request(method, url).header(
            reqwest::header::AUTHORIZATION,
            format!(
                "MediaBrowser Client=\"Tubarr\", Token=\"{}\"",
                self.config.api_key.expose()
            ),
        );
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req
            .send()
            .await
            .map_err(|e| JellyfinError::Http(what.clone(), e.without_url().to_string()))?;
        let status = res.status();
        if !status.is_success() {
            return Err(JellyfinError::Status(what, status));
        }
        res.bytes()
            .await
            .map_err(|e| JellyfinError::Http(what, e.to_string()))
    }
}

/// Whether `item` (the episode `video`) holds the audio only file as a version, with the episode
/// first. The audio file is known by its item id, its path or Jellyfin's label for it.
fn merged(item: &Item, video: &str, audio: &str, audio_id: Option<&String>) -> bool {
    let [first, rest @ ..] = item.media_sources.as_slice() else {
        return false;
    };
    first.id.as_deref().is_some_and(|id| same_id(id, video))
        && rest.iter().any(|s| {
            s.id.as_deref()
                .zip(audio_id)
                .is_some_and(|(a, b)| same_id(a, b))
                || s.path.as_deref() == Some(audio)
                || s.name.as_deref() == Some(crate::hooks::LABEL.trim_start_matches(" - "))
        })
}
