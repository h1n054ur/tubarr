//! Handing what Tubarr has just released straight to Sonarr and Radarr (`POST /api/v3/release/push`).
//!
//! The arrs' RSS sync reads only the newest releases of the indexer, so an older episode a
//! backfill or a playlist found, or a film added without a search, would wait for a search by
//! hand, and a search asks every indexer the arr has. When the matcher writes the first release
//! for an episode or film, the target is queued here instead. A background task pushes the
//! release the Newznab feed would list first for it (same title, same `t=get` link), one per arr
//! at most every `push_interval`, only for what the arr still wants (monitored, no file), with
//! Tubarr's indexer name and pinned to Tubarr's download client. The arr runs its usual decision
//! engine on it: quality, profile and delay rules still apply.
//!
//! Field names come from Sonarr's and Radarr's `ReleasePushController` and `ReleaseResource`
//! (`title`, `downloadUrl`, `protocol`, `publishDate`, `size`, `indexer`, `downloadClientId`; the
//! answer is a list of releases with `approved`, `temporarilyRejected`, `rejected` and
//! `rejections`).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use tokio::sync::Notify;
use url::Url;

use super::{App, ArrClient, ArrConnections, ArrError, Episode, Movie, http_client};
use crate::{
    activity::{Activity, ActivityLog},
    config::{ArrSettings, Secret},
    metrics::METRICS,
    newznab::{Context, Release},
    store::{Db, MappingKind, PinTarget, StoreError},
};

/// How long a looked-up download client id is trusted before it is read again.
const CLIENT_TTL: Duration = Duration::from_secs(10 * 60);

/// An episode or film to push, by the arr's own series or film id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PushTarget {
    Episode {
        series_id: i64,
        season: i64,
        episode: i64,
    },
    /// A daily show's episode by its `YYYY-MM-DD` air date.
    AirDate {
        series_id: i64,
        date: String,
    },
    Film {
        movie_id: i64,
    },
}

impl PushTarget {
    /// The target of a release on a mapping of `kind`. `None` for a mapping made by hand, which
    /// has no arr id.
    pub fn for_release(kind: MappingKind, arr_id: Option<i64>, target: &PinTarget) -> Option<Self> {
        let id = arr_id?;
        Some(match (kind, target) {
            (MappingKind::Film, PinTarget::Film) => Self::Film { movie_id: id },
            (MappingKind::Film, _) | (_, PinTarget::Film) => return None,
            (_, PinTarget::Episode { season, episode }) => Self::Episode {
                series_id: id,
                season: *season,
                episode: *episode,
            },
            (_, PinTarget::AirDate(date)) => Self::AirDate {
                series_id: id,
                date: date.clone(),
            },
        })
    }

    pub fn app(&self) -> App {
        match self {
            Self::Film { .. } => App::Radarr,
            Self::Episode { .. } | Self::AirDate { .. } => App::Sonarr,
        }
    }

    /// The arr's series or film id and the release target, as the releases table keys them.
    fn release_key(&self) -> (bool, i64, PinTarget) {
        match self {
            Self::Episode {
                series_id,
                season,
                episode,
            } => (
                false,
                *series_id,
                PinTarget::Episode {
                    season: *season,
                    episode: *episode,
                },
            ),
            Self::AirDate { series_id, date } => {
                (false, *series_id, PinTarget::AirDate(date.clone()))
            }
            Self::Film { movie_id } => (true, *movie_id, PinTarget::Film),
        }
    }

    fn is_episode(&self, e: &Episode) -> bool {
        match self {
            Self::Episode {
                season, episode, ..
            } => e.season_number == *season && e.episode_number == *episode,
            Self::AirDate { date, .. } => e.air_date.as_deref() == Some(date.as_str()),
            Self::Film { .. } => false,
        }
    }
}

/// What the download links in the feed are built from, so a push carries the same link.
#[derive(Debug, Clone)]
pub struct FeedLinks {
    pub public_url: Url,
    /// The key Sonarr and Radarr use for `/api`, repeated in each link.
    pub api_key: Secret,
    /// `[newznab] report_size`: off, a push carries no size, like the feed.
    pub report_size: bool,
}

/// What the arr did with a pushed release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Approved: the arr sent it to Tubarr's download client.
    Grabbed,
    /// Held back for now (a delay profile), with the reasons. The arr grabs it later itself.
    Pending(Vec<String>),
    /// Refused, with the arr's reasons.
    Rejected(Vec<String>),
    /// The push did not get an answer: unreachable, an HTTP error, no Tubarr client.
    Failed(String),
}

impl Outcome {
    fn name(&self) -> &'static str {
        match self {
            Self::Grabbed => "grabbed",
            Self::Pending(_) => "pending",
            Self::Rejected(_) => "rejected",
            Self::Failed(_) => "failed",
        }
    }

    fn reasons(&self) -> Vec<String> {
        match self {
            Self::Grabbed => Vec::new(),
            Self::Pending(r) | Self::Rejected(r) => r.clone(),
            Self::Failed(e) => vec![e.clone()],
        }
    }
}

/// One push a flush sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pushed {
    pub app: App,
    /// The release title, as the feed has it.
    pub title: String,
    pub outcome: Outcome,
}

/// The body of `POST /api/v3/release/push` (a `ReleaseResource`, only the fields a push reads).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PushBody<'a> {
    title: &'a str,
    download_url: &'a str,
    protocol: &'static str,
    publish_date: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<i64>,
    indexer: &'a str,
    download_client_id: i64,
}

/// One release of the push answer: the arr's decision.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Decision {
    approved: bool,
    temporarily_rejected: bool,
    rejections: Option<Vec<String>>,
}

/// A download client as `GET /api/v3/downloadclient` lists it.
#[derive(Debug, Deserialize)]
struct DownloadClient {
    id: i64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    protocol: Option<String>,
    #[serde(default)]
    enable: bool,
}

impl ArrClient {
    /// The id of the enabled Usenet download client named `name` (compared without case).
    async fn download_client_id(&self, name: &str) -> Result<i64, ArrError> {
        let clients: Vec<DownloadClient> = self.get("api/v3/downloadclient", &[]).await?;
        clients
            .into_iter()
            .find(|c| {
                c.enable
                    && c.protocol.as_deref() == Some("usenet")
                    && c.name
                        .as_deref()
                        .is_some_and(|n| n.eq_ignore_ascii_case(name))
            })
            .map(|c| c.id)
            .ok_or_else(|| ArrError::ClientMissing {
                app: self.app,
                name: name.to_owned(),
            })
    }
}

#[derive(Debug, Default)]
struct State {
    /// In the order the matcher released them, without repeats.
    queue: Vec<PushTarget>,
    last_sent: HashMap<App, Instant>,
    /// Tubarr's download client id in each arr, when it was read and from which arr URL.
    clients: HashMap<App, (i64, Instant, Url)>,
}

/// The queue and its background task. Share it behind an `Arc` and start [`AutoPush::run`].
#[derive(Debug)]
pub struct AutoPush {
    arrs: ArrConnections,
    http: reqwest::Client,
    db: Db,
    links: FeedLinks,
    interval: Duration,
    indexer: String,
    download_client: String,
    activity: Arc<ActivityLog>,
    state: Mutex<State>,
    wake: Notify,
}

impl AutoPush {
    /// Pushes go to whichever arrs `arrs` holds at the time, paced by `settings.push_interval`,
    /// named after `settings.indexer` and pinned to `settings.download_client`. Releases come from
    /// `db`. Fails only if the HTTP client cannot be built.
    pub fn new(
        arrs: ArrConnections,
        settings: &ArrSettings,
        links: FeedLinks,
        db: Db,
        activity: Arc<ActivityLog>,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            arrs,
            http: http_client()?,
            db,
            links,
            interval: settings.push_interval,
            indexer: settings.indexer.clone(),
            download_client: settings.download_client.clone(),
            activity,
            state: Mutex::new(State::default()),
            wake: Notify::new(),
        })
    }

    /// Queues targets for pushing and returns how many were new. Targets for an arr that is not
    /// set, or already queued, are left out.
    pub fn enqueue(&self, targets: impl IntoIterator<Item = PushTarget>) -> usize {
        let mut added = 0;
        {
            let mut state = self.lock();
            for target in targets {
                if self.arrs.get(target.app()).is_some() && !state.queue.contains(&target) {
                    state.queue.push(target);
                    added += 1;
                }
            }
        }
        if added > 0 {
            self.wake.notify_one();
        }
        added
    }

    /// Catch-up: queues every episode and film that has a release in Tubarr. The pusher skips
    /// those the arr no longer wants, so this replaces a search over the whole library. Returns
    /// how many were newly queued.
    pub async fn queue_released(&self) -> Result<usize, StoreError> {
        let targets = self.db.released_arr_targets().await?;
        Ok(self.enqueue(
            targets
                .into_iter()
                .filter_map(|(kind, id, target)| PushTarget::for_release(kind, Some(id), &target)),
        ))
    }

    /// Targets waiting for a push.
    pub fn queued(&self) -> Vec<PushTarget> {
        self.lock().queue.clone()
    }

    /// Sends what is due at `now`: per arr at most one push, and none while its last one is
    /// younger than the interval. Targets the arr no longer wants, or without a release, are
    /// dropped on the way.
    pub async fn flush_at(&self, now: Instant) -> Vec<Pushed> {
        let mut sent = Vec::new();
        for app in [App::Sonarr, App::Radarr] {
            if let Some(p) = self.flush_app(app, now).await {
                sent.push(p);
            }
        }
        sent
    }

    /// Flushes whenever something is queued and due, forever.
    pub async fn run(self: Arc<Self>) {
        loop {
            match self.next_due(Instant::now()) {
                None => self.wake.notified().await,
                Some(wait) if !wait.is_zero() => {
                    tokio::select! {
                        () = tokio::time::sleep(wait) => {}
                        () = self.wake.notified() => continue,
                    }
                }
                Some(_) => {}
            }
            for p in self.flush_at(Instant::now()).await {
                tracing::info!(app = %p.app, release = %p.title, outcome = p.outcome.name(), reasons = ?p.outcome.reasons(), "pushed a release to the arr");
            }
        }
    }

    /// A client for the arr connected right now, if any.
    fn client(&self, app: App) -> Option<ArrClient> {
        let connection = self.arrs.get(app)?;
        Some(ArrClient::with_http(app, &connection, self.http.clone()))
    }

    /// How long until the next flush has something to send. `None` when nothing is queued.
    fn next_due(&self, now: Instant) -> Option<Duration> {
        let state = self.lock();
        [App::Sonarr, App::Radarr]
            .into_iter()
            .filter(|app| state.queue.iter().any(|t| t.app() == *app))
            .map(|app| match state.last_sent.get(&app) {
                Some(last) => (*last + self.interval).saturating_duration_since(now),
                None => Duration::ZERO,
            })
            .min()
    }

    /// Takes the arr's next queued target, if its interval is over.
    fn next_target(&self, app: App, now: Instant) -> Option<PushTarget> {
        let mut state = self.lock();
        if state
            .last_sent
            .get(&app)
            .is_some_and(|last| now < *last + self.interval)
        {
            return None;
        }
        let at = state.queue.iter().position(|t| t.app() == app)?;
        Some(state.queue.remove(at))
    }

    async fn flush_app(&self, app: App, now: Instant) -> Option<Pushed> {
        let Some(client) = self.client(app) else {
            // The arr was removed in Settings: what was queued for it can no longer be sent.
            self.lock().queue.retain(|t| t.app() != app);
            return None;
        };
        let client = &client;
        // Episode lists read in this flush, per series.
        let mut episodes: HashMap<i64, Vec<Episode>> = HashMap::new();
        let (target, release) = loop {
            let target = self.next_target(app, now)?;
            match self.wanted(client, &target, &mut episodes).await {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) if transient(&e) => {
                    tracing::warn!(app = %app, error = %e, "cannot read what the arr wants, pushing later");
                    self.requeue(app, Some(target), now);
                    return None;
                }
                Err(e) => {
                    tracing::warn!(app = %app, target = ?target, error = %e, "cannot read what the arr wants, dropped");
                    continue;
                }
            }
            match self.release(&target).await {
                Ok(Some(release)) => break (target, release),
                Ok(None) => {
                    tracing::debug!(target = ?target, "no release to push");
                }
                Err(e) => {
                    tracing::warn!(target = ?target, error = %e, "cannot read the release to push, dropped");
                }
            }
        };
        let result = self.push(app, client, &release, now).await;
        let outcome = match &result {
            Ok(outcome) => outcome.clone(),
            Err(e) => Outcome::Failed(e.to_string()),
        };
        METRICS.arr_push(&app.to_string(), outcome.name());
        self.activity.record(Activity::ArrPush {
            app: app.to_string(),
            release: release.title.clone(),
            outcome: outcome.name().to_owned(),
            reasons: outcome.reasons(),
        });
        let back = match &result {
            Err(e) if transient(e) => Some(target),
            _ => None,
        };
        self.requeue(app, back, now);
        Some(Pushed {
            app,
            title: release.title,
            outcome,
        })
    }

    /// Whether the arr still has the target monitored and without a file.
    async fn wanted(
        &self,
        client: &ArrClient,
        target: &PushTarget,
        episodes: &mut HashMap<i64, Vec<Episode>>,
    ) -> Result<bool, ArrError> {
        match target {
            PushTarget::Film { movie_id } => {
                let movie: Movie = client.get(&format!("api/v3/movie/{movie_id}"), &[]).await?;
                Ok(movie.monitored && !movie.has_file)
            }
            PushTarget::Episode { series_id, .. } | PushTarget::AirDate { series_id, .. } => {
                if !episodes.contains_key(series_id) {
                    let list: Vec<Episode> = client
                        .get("api/v3/episode", &[("seriesId", series_id.to_string())])
                        .await?;
                    episodes.insert(*series_id, list);
                }
                Ok(episodes
                    .get(series_id)
                    .into_iter()
                    .flatten()
                    .find(|e| target.is_episode(e))
                    .is_some_and(|e| e.monitored && !e.has_file))
            }
        }
    }

    /// The release the feed lists first for the target, sized like the feed.
    async fn release(&self, target: &PushTarget) -> Result<Option<Release>, StoreError> {
        let (film, arr_id, pin) = target.release_key();
        let row = self.db.release_for_arr_target(film, arr_id, &pin).await?;
        Ok(row.and_then(Release::from_stored).map(|mut release| {
            if !self.links.report_size {
                release.size = None;
            }
            release
        }))
    }

    async fn push(
        &self,
        app: App,
        client: &ArrClient,
        release: &Release,
        now: Instant,
    ) -> Result<Outcome, ArrError> {
        let client_id = self.client_id(app, client, now).await?;
        let ctx = Context {
            public_url: &self.links.public_url,
            api_key: self.links.api_key.expose(),
            report_size: self.links.report_size,
        };
        let download_url = ctx.download_url(&release.video_id);
        let body = PushBody {
            title: &release.title,
            download_url: &download_url,
            protocol: "usenet",
            publish_date: rfc3339(release.published_at),
            size: release.size,
            indexer: &self.indexer,
            download_client_id: client_id,
        };
        let answer: Result<Vec<Decision>, ArrError> =
            client.post("api/v3/release/push", &body).await;
        let decisions = match answer {
            Ok(d) => d,
            Err(e) => {
                // A client removed or renamed in the arr shows up as a failure: read it again.
                self.lock().clients.remove(&app);
                return Err(e);
            }
        };
        let Some(decision) = decisions.into_iter().next() else {
            return Ok(Outcome::Failed(format!(
                "{app} answered the push with no decision"
            )));
        };
        let reasons = decision.rejections.unwrap_or_default();
        Ok(if decision.approved {
            Outcome::Grabbed
        } else if decision.temporarily_rejected {
            Outcome::Pending(reasons)
        } else {
            Outcome::Rejected(reasons)
        })
    }

    /// Tubarr's download client id in the arr, read again after [`CLIENT_TTL`].
    async fn client_id(&self, app: App, client: &ArrClient, now: Instant) -> Result<i64, ArrError> {
        if let Some((id, at, base)) = self.lock().clients.get(&app)
            && now.saturating_duration_since(*at) < CLIENT_TTL
            && *base == client.base
        {
            return Ok(*id);
        }
        let id = client.download_client_id(&self.download_client).await?;
        self.lock()
            .clients
            .insert(app, (id, now, client.base.clone()));
        Ok(id)
    }

    /// Puts a target back at the front of the queue and starts the arr's interval.
    fn requeue(&self, app: App, target: Option<PushTarget>, now: Instant) {
        let mut state = self.lock();
        state.last_sent.insert(app, now);
        if let Some(target) = target
            && !state.queue.contains(&target)
        {
            state.queue.insert(0, target);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// ISO 8601 in UTC, what the arrs read `publishDate` as. Out-of-range times give the epoch.
fn rfc3339(unix: i64) -> String {
    OffsetDateTime::from_unix_timestamp(unix)
        .unwrap_or(OffsetDateTime::UNIX_EPOCH)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// A failure worth trying again later: the arr was down, answered 5xx or asked to slow down.
fn transient(e: &ArrError) -> bool {
    match e {
        ArrError::Unreachable { .. } => true,
        ArrError::Status { status, .. } => {
            status.is_server_error() || *status == StatusCode::TOO_MANY_REQUESTS
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_targets_follow_the_mapping_kind() {
        let ep = PinTarget::Episode {
            season: 1,
            episode: 3,
        };
        assert_eq!(
            PushTarget::for_release(MappingKind::Show, Some(11), &ep),
            Some(PushTarget::Episode {
                series_id: 11,
                season: 1,
                episode: 3
            })
        );
        assert_eq!(
            PushTarget::for_release(
                MappingKind::Daily,
                Some(12),
                &PinTarget::AirDate("2026-10-01".to_owned())
            ),
            Some(PushTarget::AirDate {
                series_id: 12,
                date: "2026-10-01".to_owned()
            })
        );
        assert_eq!(
            PushTarget::for_release(MappingKind::Film, Some(7), &PinTarget::Film),
            Some(PushTarget::Film { movie_id: 7 })
        );
        assert_eq!(PushTarget::for_release(MappingKind::Show, None, &ep), None);
        assert_eq!(
            PushTarget::for_release(MappingKind::Film, Some(7), &ep),
            None
        );
    }

    #[test]
    fn publish_date_is_iso_utc() {
        assert_eq!(rfc3339(1_700_000_000), "2023-11-14T22:13:20Z");
    }
}
