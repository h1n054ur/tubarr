//! The show and film matchers on their real inputs, run in the background.
//!
//! Candidates are every video the watcher stored for a mapping's channels. Wanted episodes come
//! from Sonarr: for each active show or daily mapping with a Sonarr series id, every monitored
//! episode, with or without a file. Imported episodes stay in the list on purpose, so their
//! release keeps showing up in RSS and searches the same way it did before the import. The
//! unmonitored episodes come along flagged, never to be released: their dates and numbers keep
//! an upload of one off its neighbours. Mappings
//! made by hand (no series id) have nothing to match against and are left alone.
//!
//! Wanted films come from Radarr: the films carrying the arr tag that are monitored, with or
//! without a file (for the same reason), joined to the active film mappings by TMDB id. Without
//! Radarr, or when it cannot be read, films are left as they are.
//!
//! Videos waiting on their duration get a resolver lookup here, a few per run, oldest first, and
//! never while YouTube has the limiter paused. Nothing else looks videos up for matching. Their
//! height is assumed from the config (`assumed_height`) until a lookup for a grab or a play gives
//! the real one, which the next run writes to the release.
//!
//! A run leaves a mapping alone when nothing it reads changed since its last run: a digest of the
//! mapping, its aliases and pins, the videos on its channels with their metadata and unplayable
//! marks, its playlist, Sonarr's episodes, the assumed heights and the mapping's own releases and
//! notes is kept per mapping ([`Db::match_input_lines`]) and compared first. Matching is a full
//! recompute per mapping (duplicates, parts, pins and the backfill limit look across videos), so
//! the mapping is the unit: one new upload re-matches its mapping, and a run over unchanged data
//! evaluates no video at all. A pending note changes the digest when its lookup lands.

use std::{
    collections::{BTreeSet, HashMap},
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{sync::Notify, time::Instant};

use super::{
    AssumedHeights, Candidate, Candidates, FilmSettings, Report, Wanted, WantedEpisode,
    WantedFilms, WantedSeries, WithAssumed, match_films, match_shows,
};
use crate::{
    arr::{ArrConnections, ArrError, AutoPush, Radarr, Sonarr},
    config::{self, ArrConnection},
    metrics::METRICS,
    resolver::Resolver,
    store::{
        Db, Mapping, MappingKind, MatchCounts, RecheckDue, StoreError, VideoOnChannel, unix_now,
    },
};

/// A wake-up waits this long first, so a burst of feed reads or a sync of many titles becomes
/// one run.
const SETTLE: Duration = Duration::from_secs(30);
/// A video looked up for matching is not looked up again for this long, so one that keeps
/// failing cannot hold the per-run budget.
const LOOKUP_RETRY: Duration = Duration::from_secs(6 * 60 * 60);
/// A video marked for members or Premium only is looked up again this often, as long as it was
/// first seen within [`MEMBERS_RECHECK_FOR`]. Many podcasts open an early-access episode to
/// everyone about a week later. A channel list showing it open clears the mark sooner.
const MEMBERS_RECHECK: Duration = Duration::from_secs(24 * 60 * 60);
const MEMBERS_RECHECK_FOR: Duration = Duration::from_secs(14 * 24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error(transparent)]
    Arr(#[from] ArrError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What the Health page shows about the matcher. Times are Unix seconds.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct MatcherHealth {
    pub last_run_at: Option<i64>,
    /// Why the last run failed, cleared by the next good one.
    pub last_error: Option<String>,
    /// Totals after the last good run.
    pub counts: MatchCounts,
    /// Resolver lookups the last run made for missing metadata.
    pub lookups: u32,
    /// Mappings the last run could not match, with the reason, as `[mapping id, reason]`.
    pub skipped: Vec<(i64, String)>,
    /// Mappings the last run matched, and the ones it left alone since nothing they read changed.
    pub matched: u64,
    pub unchanged: u64,
    pub films: FilmHealth,
}

/// The film side of [`MatcherHealth`].
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct FilmHealth {
    /// Film mappings Radarr wants, as of the last run.
    pub wanted: u64,
    /// Totals over the active film mappings after the last good run.
    pub counts: MatchCounts,
    /// Why Radarr could not be read for films on the last run. Shows still ran.
    pub radarr_error: Option<String>,
}

/// What one run did. `report` covers shows and films together.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunSummary {
    pub report: Report,
    pub lookups: u32,
    pub counts: MatchCounts,
    pub films: FilmHealth,
}

/// How wanted films are read from Radarr and matched.
#[derive(Debug, Clone)]
struct FilmSource {
    tag: String,
    settings: FilmSettings,
}

/// The background show and film matcher. Share it behind an `Arc` and start
/// [`ShowMatcher::run`].
#[derive(Debug)]
pub struct ShowMatcher {
    db: Db,
    resolver: Arc<Resolver<Db>>,
    arrs: ArrConnections,
    films: Option<FilmSource>,
    settings: config::Matcher,
    heights: AssumedHeights,
    wake: Arc<Notify>,
    looked_up: Mutex<HashMap<String, Instant>>,
    health: Mutex<MatcherHealth>,
    push: Option<Arc<AutoPush>>,
}

impl ShowMatcher {
    /// `wake` is what the watcher and the arr sync notify when they found something. Each run
    /// reads the wanted episodes from the Sonarr `arrs` holds at the time, if any.
    pub fn new(
        db: Db,
        resolver: Arc<Resolver<Db>>,
        arrs: ArrConnections,
        settings: config::Matcher,
        wake: Arc<Notify>,
    ) -> Self {
        Self {
            db: db.with_note_retention(settings.note_retention),
            resolver,
            arrs,
            films: None,
            heights: AssumedHeights {
                default: settings.assumed_height,
                channels: HashMap::new(),
            },
            settings,
            wake,
            looked_up: Mutex::new(HashMap::new()),
            health: Mutex::new(MatcherHealth::default()),
            push: None,
        }
    }

    /// Also matches the films carrying `tag` in Radarr, on the runs while a Radarr is connected.
    #[must_use]
    pub fn with_films(mut self, tag: String, settings: FilmSettings) -> Self {
        self.films = Some(FilmSource { tag, settings });
        self
    }

    /// Channels whose releases assume another height than `[matcher]`'s (`assumed_height` in
    /// `[channels]`).
    #[must_use]
    pub fn with_channel_heights(mut self, channels: HashMap<String, u32>) -> Self {
        self.heights.channels = channels;
        self
    }

    /// Queues the episodes and films each run releases for the first time to push to the arr.
    #[must_use]
    pub fn with_auto_push(mut self, push: Arc<AutoPush>) -> Self {
        self.push = Some(push);
        self
    }

    /// Notify this to start a run soon.
    pub fn waker(&self) -> Arc<Notify> {
        self.wake.clone()
    }

    pub fn health(&self) -> MatcherHealth {
        self.lock_health().clone()
    }

    /// Runs after every wake-up and at least every `interval`, forever.
    pub async fn run(self: Arc<Self>) {
        loop {
            tokio::select! {
                () = self.wake.notified() => tokio::time::sleep(SETTLE).await,
                () = tokio::time::sleep(self.settings.interval) => {}
            }
            match self.run_once().await {
                Ok(s) => tracing::info!(
                    released = s.report.released,
                    replaced = s.report.replaced,
                    lookups = s.lookups,
                    matched = s.report.matched,
                    unchanged = s.report.unchanged,
                    pending = s.counts.pending,
                    unmatched = s.counts.unmatched,
                    films_released = s.films.counts.released,
                    "matcher done"
                ),
                Err(e) => tracing::warn!(error = %e, "matcher failed, keeping the releases"),
            }
        }
    }

    /// One full run: match, look up what is missing, match again if a lookup landed, then set
    /// the watcher's match states. Running it twice in a row changes nothing the second time.
    pub async fn run_once(&self) -> Result<RunSummary, RunError> {
        let started = Instant::now();
        let result = self.run_inner().await;
        METRICS.matcher_seconds.observe(started.elapsed());
        let mut health = self.lock_health();
        health.last_run_at = Some(unix_now());
        match &result {
            Ok(s) => {
                METRICS.matcher_released.add(s.report.released);
                health.last_error = None;
                health.counts = s.counts;
                health.lookups = s.lookups;
                health.skipped = s.report.skipped.clone();
                health.matched = s.report.matched;
                health.unchanged = s.report.unchanged;
                health.films = s.films.clone();
            }
            Err(e) => {
                METRICS.matcher_failures.inc();
                health.last_error = Some(e.to_string());
            }
        }
        result
    }

    async fn run_inner(&self) -> Result<RunSummary, RunError> {
        let in_use = self.arrs.current();
        let wanted = match &in_use.sonarr {
            Some(connection) => wanted_series(&self.db, &Sonarr::new(connection)?).await?,
            None => Vec::new(),
        };
        let wanted = Fixed(wanted);
        // Films are matched only on runs with a Radarr to say which are wanted.
        let film_source = self.films.as_ref().zip(in_use.radarr.as_ref());
        let mut films_health = FilmHealth::default();
        let films = match film_source {
            Some((source, radarr)) => match wanted_films(&self.db, radarr, &source.tag).await {
                Ok(films) => films,
                Err(RunError::Arr(e)) => {
                    tracing::warn!(error = %e, "cannot read the films from Radarr, leaving films as they are");
                    films_health.radarr_error = Some(e.to_string());
                    Vec::new()
                }
                Err(e) => return Err(e),
            },
            None => Vec::new(),
        };
        films_health.wanted = films.len() as u64;
        let films = FixedFilms(films);

        let film_settings = film_source.map(|(source, _)| &source.settings);
        let mut report = self.match_all(&wanted, &films, film_settings).await?;
        let (pending_lookups, landed) = self.fill_metadata().await?;
        let budget_left = self
            .settings
            .lookups_per_run
            .saturating_sub(pending_lookups);
        let (verify_lookups, verified) = self.verify_releases(budget_left).await?;
        let (recheck_lookups, now_playable) = self
            .recheck_unplayable(budget_left.saturating_sub(verify_lookups))
            .await?;
        let lookups = pending_lookups + verify_lookups + recheck_lookups;
        if landed > 0 || verified > 0 || now_playable > 0 {
            let again = self.match_all(&wanted, &films, film_settings).await?;
            report.released += again.released;
            report.replaced += again.replaced;
            report.first_released.extend(again.first_released);
            report.notes_changed += again.notes_changed;
            report.matched += again.matched;
            report.not_wanted = again.not_wanted;
            report.skipped = again.skipped;
        }
        if let Some(push) = &self.push {
            push.enqueue(report.first_released.iter().cloned());
        }
        let mut channels: BTreeSet<String> = wanted
            .0
            .iter()
            .filter(|s| s.mapping.kind != MappingKind::Film)
            .flat_map(|s| s.mapping.channel_ids.iter().cloned())
            .collect();
        if let Some(settings) = film_settings
            && !films.0.is_empty()
        {
            channels.extend(films.0.iter().flat_map(|m| m.channel_ids.iter().cloned()));
            channels.extend(settings.channels.iter().cloned());
        }
        let channels: Vec<String> = channels.into_iter().collect();
        self.db.refresh_match_states(&channels).await?;
        films_health.counts = self.db.film_match_counts().await?;
        Ok(RunSummary {
            report,
            lookups,
            counts: self.db.match_counts().await?,
            films: films_health,
        })
    }

    /// One pass of the show matcher, then the film matcher (with `film_settings`), as one report,
    /// over the mappings whose digest changed since their last run. Each matched mapping then
    /// stores its new digest, taken after the run so its own writes are in it, unless it could not
    /// be matched (it is tried again every run, and stays on the Health page).
    async fn match_all(
        &self,
        wanted: &Fixed,
        films: &FixedFilms,
        film_settings: Option<&FilmSettings>,
    ) -> Result<Report, StoreError> {
        let metadata = WithAssumed {
            metadata: &self.db,
            heights: &self.heights,
        };
        let mut unchanged = 0;
        let mut series = Vec::new();
        for s in &wanted.0 {
            if self
                .unchanged(&s.mapping, self.series_digest(s).await?)
                .await?
            {
                unchanged += 1;
            } else {
                series.push(s.clone());
            }
        }
        let series = Fixed(series);
        let mut report = match_shows(&self.db, &self.db, &series, &metadata).await?;
        let mut changed_films = Vec::new();
        if let Some(settings) = film_settings {
            for film in &films.0 {
                if self
                    .unchanged(film, self.film_digest(film, settings).await?)
                    .await?
                {
                    unchanged += 1;
                } else {
                    changed_films.push(film.clone());
                }
            }
            let changed = FixedFilms(changed_films.clone());
            let f = match_films(&self.db, &self.db, &changed, &metadata, settings).await?;
            report.released += f.released;
            report.replaced += f.replaced;
            report.first_released.extend(f.first_released);
            report.notes_changed += f.notes_changed;
            report.matched += f.matched;
            report.skipped.extend(f.skipped);
        }
        report.unchanged = unchanged;
        let skipped: BTreeSet<i64> = report.skipped.iter().map(|(id, _)| *id).collect();
        for s in series.0.iter().filter(|s| !skipped.contains(&s.mapping.id)) {
            let digest = self.series_digest(s).await?;
            self.db.set_match_digest(s.mapping.id, digest).await?;
        }
        if let Some(settings) = film_settings {
            for film in changed_films.iter().filter(|f| !skipped.contains(&f.id)) {
                let digest = self.film_digest(film, settings).await?;
                self.db.set_match_digest(film.id, digest).await?;
            }
        }
        Ok(report)
    }

    /// `true` when `digest` is the one the mapping's last run stored.
    async fn unchanged(&self, mapping: &Mapping, digest: i64) -> Result<bool, StoreError> {
        Ok(self.db.match_digest(mapping.id).await? == Some(digest))
    }

    async fn series_digest(&self, series: &WantedSeries) -> Result<i64, StoreError> {
        let channels = &series.mapping.channel_ids;
        self.digest(&series.mapping, channels, &series.episodes)
            .await
    }

    /// Films also search the `[films]` channels, and the film settings decide too.
    async fn film_digest(
        &self,
        film: &Mapping,
        settings: &FilmSettings,
    ) -> Result<i64, StoreError> {
        let mut channels = film.channel_ids.clone();
        channels.extend(settings.channels.iter().cloned());
        self.digest(film, &channels, &format!("{settings:?}")).await
    }

    /// A digest of everything a run for `mapping` reads or writes, with `extra` from outside the
    /// database (Sonarr's episodes, the film settings). Only ever compared within one build, so
    /// the standard hasher's lack of a stable output across Rust releases costs one full run.
    async fn digest(
        &self,
        mapping: &Mapping,
        channels: &[String],
        extra: &impl Hash,
    ) -> Result<i64, StoreError> {
        let lines = self
            .db
            .match_input_lines(mapping.id, channels, mapping.playlist_id.as_deref())
            .await?;
        let mut hasher = DefaultHasher::new();
        format!("{mapping:?}").hash(&mut hasher);
        for channel in channels {
            self.heights.get(channel).hash(&mut hasher);
        }
        extra.hash(&mut hasher);
        lines.hash(&mut hasher);
        Ok(hasher.finish().cast_signed())
    }

    /// Looks up the longest waiting pending videos, at most `lookups_per_run`. Returns the
    /// lookups made and how many of them succeeded.
    async fn fill_metadata(&self) -> Result<(u32, u32), StoreError> {
        let budget = self.settings.lookups_per_run;
        if budget == 0 {
            return Ok((0, 0));
        }
        let pending = self.db.pending_videos().await?;
        let (mut made, mut landed) = (0, 0);
        for video_id in pending {
            if made >= budget {
                break;
            }
            if self.resolver.limiter().status().paused_for.is_some() {
                tracing::info!(
                    "YouTube has lookups paused, metadata lookups wait for the next run"
                );
                break;
            }
            if !self.claim_lookup(&video_id) {
                continue;
            }
            made += 1;
            if self.look_up(&video_id).await? == Looked::Playable {
                landed += 1;
            }
        }
        Ok((made, landed))
    }

    /// Looks up videos already released but never confirmed by a lookup (a release at an assumed
    /// height), at most `budget`, so an unplayable one leaves the feed before an arr grabs it.
    /// Returns the lookups made and how many of them learned something (real metadata, which
    /// corrects an assumed height, or an unplayable video).
    async fn verify_releases(&self, budget: u32) -> Result<(u32, u32), StoreError> {
        if budget == 0 {
            return Ok((0, 0));
        }
        let unverified = self
            .db
            .unverified_release_videos(i64::from(budget) * 4)
            .await?;
        let (made, playable, unplayable) = self.look_up_each(unverified, budget).await?;
        Ok((made, playable + unplayable))
    }

    /// Looks up videos marked unplayable for missing formats or an unfinished livestream again
    /// once the mark is [`LOOKUP_RETRY`] old, and members-only ones once a day for their first
    /// two weeks ([`MEMBERS_RECHECK`]), at most `budget`. New uploads often offer only VP9 or AV1
    /// for a while, and one that has since got H.264 video loses its mark and is matched again.
    /// Returns the lookups made and how many found the video playable.
    async fn recheck_unplayable(&self, budget: u32) -> Result<(u32, u32), StoreError> {
        if budget == 0 {
            return Ok((0, 0));
        }
        let now = unix_now();
        let due = RecheckDue {
            checked_before: now - LOOKUP_RETRY.as_secs() as i64,
            members_checked_before: now - MEMBERS_RECHECK.as_secs() as i64,
            members_seen_since: now - MEMBERS_RECHECK_FOR.as_secs() as i64,
        };
        let due = self.db.unplayable_due(due, i64::from(budget) * 4).await?;
        let (made, playable, _) = self.look_up_each(due, budget).await?;
        Ok((made, playable))
    }

    /// Looks up `video_ids` in order until `budget` lookups are made or the limiter pauses,
    /// skipping any looked up within [`LOOKUP_RETRY`]. Returns the lookups made and how many
    /// found the video playable and unplayable.
    async fn look_up_each(
        &self,
        video_ids: Vec<String>,
        budget: u32,
    ) -> Result<(u32, u32, u32), StoreError> {
        let (mut made, mut playable, mut unplayable) = (0, 0, 0);
        for video_id in video_ids {
            if made >= budget {
                break;
            }
            if self.resolver.limiter().status().paused_for.is_some() {
                break;
            }
            if !self.claim_lookup(&video_id) {
                continue;
            }
            made += 1;
            match self.look_up(&video_id).await? {
                Looked::Playable => playable += 1,
                Looked::Unplayable => unplayable += 1,
                Looked::Unknown => {}
            }
        }
        Ok((made, playable, unplayable))
    }

    /// One resolver lookup that records what it learned: the metadata of a playable video, or the
    /// mark on one that cannot play.
    async fn look_up(&self, video_id: &str) -> Result<Looked, StoreError> {
        match self.resolver.resolve(video_id).await {
            Ok(resolved) => {
                // The resolver keeps it only when the streams last long enough to cache.
                self.db.store_video(&resolved, unix_now()).await?;
                self.db.clear_unplayable(video_id).await?;
                Ok(Looked::Playable)
            }
            Err(e) if e.is_settled() => {
                let removed = self
                    .db
                    .mark_unplayable(video_id, &e.to_string(), e.retry_later())
                    .await?;
                tracing::warn!(video_id, error = %e, removed, "video cannot play, leaving it out");
                Ok(Looked::Unplayable)
            }
            Err(e) => {
                tracing::info!(video_id, error = %e, "metadata lookup failed");
                Ok(Looked::Unknown)
            }
        }
    }

    /// `false` when the video was looked up within [`LOOKUP_RETRY`].
    fn claim_lookup(&self, video_id: &str) -> bool {
        let now = Instant::now();
        let mut looked_up = self.lock_looked_up();
        looked_up.retain(|_, until| *until > now);
        if looked_up.contains_key(video_id) {
            return false;
        }
        looked_up.insert(video_id.to_owned(), now + LOOKUP_RETRY);
        true
    }

    fn lock_health(&self) -> std::sync::MutexGuard<'_, MatcherHealth> {
        self.health.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_looked_up(&self) -> std::sync::MutexGuard<'_, HashMap<String, Instant>> {
        self.looked_up.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What one lookup showed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Looked {
    Playable,
    Unplayable,
    /// It failed in a way that says nothing about the video (throttle, timeout).
    Unknown,
}

/// The active show and daily mappings with a Sonarr series and every episode Sonarr lists of each,
/// monitored or not.
/// A series Sonarr cannot answer for is left out, so its releases and notes stay as they are.
async fn wanted_series(db: &Db, sonarr: &Sonarr) -> Result<Vec<WantedSeries>, StoreError> {
    let mut out = Vec::new();
    for mapping in db.mappings().await? {
        let Some(series_id) = mapping.arr_id else {
            continue;
        };
        if mapping.kind == MappingKind::Film || !mapping.active {
            continue;
        }
        match sonarr.episodes(series_id).await {
            Ok(episodes) => out.push(WantedSeries {
                episodes: episodes
                    .into_iter()
                    .map(|e| WantedEpisode {
                        season: e.season_number,
                        episode: e.episode_number,
                        air_date: e.air_date,
                        title: e.title,
                        monitored: e.monitored,
                    })
                    .collect(),
                mapping,
            }),
            Err(e) => tracing::warn!(
                mapping = mapping.id,
                series_id,
                error = %e,
                "cannot read the episodes from Sonarr, leaving this show as it is"
            ),
        }
    }
    Ok(out)
}

/// The active film mappings whose film Radarr has tagged and monitored, matched by TMDB id.
async fn wanted_films(
    db: &Db,
    radarr: &ArrConnection,
    tag: &str,
) -> Result<Vec<Mapping>, RunError> {
    let radarr = Radarr::new(radarr)?;
    let tmdb: BTreeSet<i64> = radarr
        .tagged_movies(tag)
        .await?
        .into_iter()
        .filter(|m| m.monitored)
        .map(|m| m.tmdb_id)
        .collect();
    Ok(db
        .mappings()
        .await?
        .into_iter()
        .filter(|m| m.kind == MappingKind::Film && m.active)
        .filter(|m| m.tmdb_id.is_some_and(|id| tmdb.contains(&id)))
        .collect())
}

/// Radarr's answer read once per run.
struct FixedFilms(Vec<Mapping>);

impl WantedFilms for FixedFilms {
    async fn films(&self) -> Result<Vec<Mapping>, StoreError> {
        Ok(self.0.clone())
    }
}

/// Sonarr's answer read once per run, so the second pass asks nothing again.
struct Fixed(Vec<WantedSeries>);

impl Wanted for Fixed {
    async fn series(&self) -> Result<Vec<WantedSeries>, StoreError> {
        Ok(self.0.clone())
    }
}

/// Everything the watcher stored on those channels, whatever its match state.
impl Candidates for Db {
    async fn on_channels(&self, channel_ids: &[String]) -> Result<Vec<Candidate>, StoreError> {
        Ok(self
            .videos_on_channels(channel_ids)
            .await?
            .into_iter()
            .map(|v: VideoOnChannel| Candidate {
                video_id: v.video_id,
                channel_id: v.channel_id,
                title: v.title,
                published_at: v.published_at,
                published_approx: v.published_approx,
                duration_secs: v.duration_secs,
            })
            .collect())
    }

    async fn playlist(&self, playlist_id: &str) -> Result<Option<Vec<String>>, StoreError> {
        self.playlist_order(playlist_id).await
    }
}
