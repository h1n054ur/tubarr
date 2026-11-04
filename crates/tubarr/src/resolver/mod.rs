//! Turning a YouTube video id into stream addresses and metadata.
//!
//! Every lookup goes through the shared [`RateLimiter`] and lands in a [`ResolveCache`]. Jellyfin
//! probes a stream and then plays it seconds later, so concurrent requests for one id share a
//! single yt-dlp run.

mod cache;
mod fallback;
mod ytdlp;

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime},
};

use serde::Serialize;

use tokio::sync::OnceCell;

pub use cache::{CachePolicy, MemoryCache, ResolveCache};
pub use fallback::{Fallback, FallbackStats, PoToken, Step, StepCounts, Sticky, StickyStatus};
pub use ytdlp::{
    AUDIO_FORMATS, Resolved, StreamFormat, Subtitle, VIDEO_FORMATS, VideoMeta, YtDlp, classify,
    is_unfinished_live, parse, url_expiry,
};

use crate::{
    health::Events,
    metrics::METRICS,
    ratelimit::{Lane, RateLimiter},
};

/// How the reason of an unplayable mark for a members-only or Premium-only video starts, so a
/// channel list that shows the video open can find the mark.
pub const MEMBERS_ONLY: &str = "video is for members or Premium only: ";

/// Why a lookup failed. Cloneable because one failed run answers every request waiting on it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("not a YouTube video id: {0:?}")]
    InvalidId(String),
    #[error("video unavailable: {0}")]
    Unavailable(String),
    #[error("video blocked in this country: {0}")]
    GeoBlocked(String),
    #[error("rate limited by YouTube: {0}")]
    RateLimited(String),
    #[error("YouTube asked for a bot check: {0}")]
    BotCheck(String),
    #[error("video is age restricted: {0}")]
    AgeRestricted(String),
    /// For channel members or YouTube Premium only. Many channels open such an upload to everyone
    /// after a while, so it is looked up again.
    #[error("{MEMBERS_ONLY}{0}")]
    MembersOnly(String),
    #[error("video {video_id} has no {missing}")]
    NoFormats {
        video_id: String,
        missing: &'static str,
    },
    /// Live now, an upcoming premiere, or a finished stream YouTube is still processing: none of
    /// the formats Tubarr joins exist until it is done.
    #[error("livestream still processing")]
    Live,
    #[error("yt-dlp did not finish within {0:?}")]
    Timeout(Duration),
    #[error("no lookup slot is free, the next one starts in {}s", .0.as_secs())]
    Busy(Duration),
    #[error("cannot run {binary}: {message}")]
    Spawn { binary: String, message: String },
    #[error("cannot read yt-dlp output: {0}")]
    Parse(String),
    #[error("yt-dlp failed (exit code {exit_code:?}): {message}")]
    Failed {
        exit_code: Option<i32>,
        message: String,
    },
}

/// What kind of failure a [`ResolveError`] is, without the message. Health events and Discord
/// cards carry this, since the message is yt-dlp's own wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    InvalidId,
    Unavailable,
    GeoBlocked,
    RateLimited,
    BotCheck,
    AgeRestricted,
    MembersOnly,
    NoFormats,
    Live,
    Timeout,
    Busy,
    Spawn,
    Parse,
    Failed,
}

impl ResolveError {
    pub fn kind(&self) -> ErrorKind {
        match self {
            Self::InvalidId(_) => ErrorKind::InvalidId,
            Self::Unavailable(_) => ErrorKind::Unavailable,
            Self::GeoBlocked(_) => ErrorKind::GeoBlocked,
            Self::RateLimited(_) => ErrorKind::RateLimited,
            Self::BotCheck(_) => ErrorKind::BotCheck,
            Self::AgeRestricted(_) => ErrorKind::AgeRestricted,
            Self::MembersOnly(_) => ErrorKind::MembersOnly,
            Self::NoFormats { .. } => ErrorKind::NoFormats,
            Self::Live => ErrorKind::Live,
            Self::Timeout(_) => ErrorKind::Timeout,
            Self::Busy(_) => ErrorKind::Busy,
            Self::Spawn { .. } => ErrorKind::Spawn,
            Self::Parse(_) => ErrorKind::Parse,
            Self::Failed { .. } => ErrorKind::Failed,
        }
    }

    /// YouTube is pushing back on this IP, so every lookup should slow down.
    pub fn is_throttle(&self) -> bool {
        matches!(self, Self::RateLimited(_) | Self::BotCheck(_))
    }

    /// Worth asking again with a PO token or cookies: a throttle, or a request to sign in that is
    /// not about age (an age check needs a verified account, not a fallback).
    /// An answer about the video itself that asking again soon will not change, so it is
    /// remembered for a while instead of costing a lookup on every play attempt.
    pub fn is_settled(&self) -> bool {
        matches!(
            self,
            Self::NoFormats { .. }
                | Self::Live
                | Self::Unavailable(_)
                | Self::AgeRestricted(_)
                | Self::MembersOnly(_)
        )
    }

    /// A settled answer that a later lookup may still change: YouTube had not offered H.264 video
    /// or AAC audio yet, as with brand-new uploads that get them only after VP9 and AV1, the
    /// video is a livestream that has not finished, or it is for members or Premium only so far.
    pub fn retry_later(&self) -> bool {
        matches!(
            self,
            Self::NoFormats { .. } | Self::Live | Self::MembersOnly(_)
        )
    }

    pub fn needs_fallback(&self) -> bool {
        self.is_throttle()
            || matches!(self, Self::Failed { message, .. } if message.to_lowercase().contains("sign in"))
    }
}

/// A failed canary lookup: the error and the fallback step it ended on (none when no step ran).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{error}")]
pub struct ProbeFailure {
    pub error: ResolveError,
    pub step: Option<Step>,
}

impl From<ResolveError> for ProbeFailure {
    fn from(error: ResolveError) -> Self {
        Self { error, step: None }
    }
}

/// A YouTube id is 11 characters of base64url. Checking it keeps odd input away from yt-dlp.
pub fn is_video_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Whether `hit` has other stream URLs than `stale`, so it can stand in for a fresh lookup.
fn replaces(stale: &Resolved, hit: &Resolved) -> bool {
    stale.video.url != hit.video.url || stale.audio.url != hit.audio.url
}

/// Wall clock used for cache validity, replaceable in tests.
pub type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

type Flight = Arc<OnceCell<Result<Resolved, ResolveError>>>;

/// Resolves video ids through the cache, the rate limiter and yt-dlp.
pub struct Resolver<C> {
    ytdlp: YtDlp,
    limiter: Arc<RateLimiter>,
    cache: C,
    policy: CachePolicy,
    /// How long every lookup is held after YouTube answers with a 429 or a bot check.
    throttle_pause: Duration,
    clock: Clock,
    flights: Mutex<HashMap<String, Flight>>,
    /// Settled failures by video id, with when they stop being answered from memory.
    settled: Mutex<HashMap<String, (ResolveError, SystemTime)>>,
    settled_for: Duration,
    fallback: Fallback,
    fallback_stats: FallbackStats,
    sticky: Sticky,
    events: Events,
    cache_hits: AtomicU64,
    lookups: AtomicU64,
    failures: AtomicU64,
}

/// Lookup counters since start, for the Health page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ResolverStats {
    /// Requests answered from the cache without running yt-dlp.
    pub cache_hits: u64,
    /// Fresh lookups, each one limiter token however many fallback steps it took.
    pub lookups: u64,
    /// Fresh lookups that failed on every step they tried.
    pub failures: u64,
}

impl<C> std::fmt::Debug for Resolver<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Resolver")
            .field("ytdlp", &self.ytdlp)
            .field("policy", &self.policy)
            .field("throttle_pause", &self.throttle_pause)
            .field("fallback", &self.fallback)
            .finish_non_exhaustive()
    }
}

impl<C: ResolveCache> Resolver<C> {
    pub fn new(ytdlp: YtDlp, limiter: Arc<RateLimiter>, cache: C) -> Self {
        Self {
            ytdlp,
            limiter,
            cache,
            policy: CachePolicy::default(),
            throttle_pause: Duration::from_secs(15 * 60),
            clock: Arc::new(SystemTime::now),
            flights: Mutex::new(HashMap::new()),
            settled: Mutex::new(HashMap::new()),
            settled_for: Duration::from_secs(30 * 60),
            fallback: Fallback::default(),
            fallback_stats: FallbackStats::default(),
            sticky: Sticky::default(),
            events: Events::new(),
            cache_hits: AtomicU64::new(0),
            lookups: AtomicU64::new(0),
            failures: AtomicU64::new(0),
        }
    }

    pub fn with_fallback(mut self, fallback: Fallback) -> Self {
        self.fallback = fallback;
        self
    }

    /// Where the resolver reports a use of the cookies step.
    pub fn with_events(mut self, events: Events) -> Self {
        self.events = events;
        self
    }

    pub fn fallback(&self) -> &Fallback {
        &self.fallback
    }

    pub fn fallback_stats(&self) -> &FallbackStats {
        &self.fallback_stats
    }

    /// yt-dlp processes running now, lookups and the watcher's listings together.
    pub fn processes_running(&self) -> usize {
        self.ytdlp.processes.running()
    }

    /// The fallback step lookups start at for now, if any.
    pub fn sticky(&self) -> Option<StickyStatus> {
        self.sticky.status()
    }

    pub fn stats(&self) -> ResolverStats {
        ResolverStats {
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            lookups: self.lookups.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
        }
    }

    pub fn with_policy(mut self, policy: CachePolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_throttle_pause(mut self, pause: Duration) -> Self {
        self.throttle_pause = pause;
        self
    }

    /// How long a settled failure (no usable formats, unavailable) is answered without a lookup.
    pub fn with_settled_for(mut self, settled_for: Duration) -> Self {
        self.settled_for = settled_for;
        self
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The time on the clock lookups are cached by.
    pub fn now(&self) -> SystemTime {
        (self.clock)()
    }

    pub fn limiter(&self) -> &Arc<RateLimiter> {
        &self.limiter
    }

    /// The cached lookup for `video_id`, or a fresh one. Concurrent calls for the same id wait on
    /// one yt-dlp run. If the caller that started it goes away, the next waiter takes over.
    pub async fn resolve(&self, video_id: &str) -> Result<Resolved, ResolveError> {
        self.resolve_in(video_id, Lane::Background).await
    }

    /// [`Self::resolve`] for a viewer pressing play: a fresh lookup takes the limiter's play lane,
    /// ahead of queued background lookups. Answers [`ResolveError::Busy`] at once when it would
    /// still first wait longer than `max_wait`, which happens during a pause after a throttle or
    /// with several plays queued. A player gives up on a silent request after about 30 s, so a
    /// quick 503 with Retry-After serves it better than a hang.
    pub async fn resolve_for_play(
        &self,
        video_id: &str,
        max_wait: Duration,
    ) -> Result<Resolved, ResolveError> {
        if is_video_id(video_id)
            && self.cache.get(video_id, (self.clock)()).await.is_none()
            && self.settled_failure(video_id).is_none()
            && !self.lock_flights().contains_key(video_id)
        {
            let wait = self.limiter.play_next_in();
            if wait > max_wait {
                return Err(ResolveError::Busy(wait));
            }
        }
        self.resolve_in(video_id, Lane::Play).await
    }

    async fn resolve_in(&self, video_id: &str, lane: Lane) -> Result<Resolved, ResolveError> {
        if !is_video_id(video_id) {
            return Err(ResolveError::InvalidId(video_id.chars().take(64).collect()));
        }
        if let Some(hit) = self.cache.get(video_id, (self.clock)()).await {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(hit);
        }
        if let Some(e) = self.settled_failure(video_id) {
            return Err(e);
        }

        let flight = self
            .lock_flights()
            .entry(video_id.to_owned())
            .or_default()
            .clone();
        let result = flight
            .get_or_init(|| self.lookup(video_id, None, lane))
            .await
            .clone();
        self.land(video_id, &flight);
        result
    }

    /// The remembered settled failure for `video_id`, if it has not run out.
    fn settled_failure(&self, video_id: &str) -> Option<ResolveError> {
        let now = (self.clock)();
        let mut settled = self.settled.lock().unwrap_or_else(|e| e.into_inner());
        settled.retain(|_, (_, until)| *until > now);
        settled.get(video_id).map(|(e, _)| e.clone())
    }

    /// A fresh lookup to replace `stale`, whose stream URLs YouTube has started refusing. Shares the
    /// single flight with [`Self::resolve`], and a newer cached entry than `stale` is used as is, so
    /// several streams failing at once cost one yt-dlp run. Only a playing stream needs this, so it
    /// takes the play lane, and answers [`ResolveError::Busy`] like [`Self::resolve_for_play`] when
    /// it would first wait longer than `max_wait`.
    pub async fn refresh(
        &self,
        stale: &Resolved,
        max_wait: Duration,
    ) -> Result<Resolved, ResolveError> {
        let video_id = stale.meta.id.as_str();
        let cached = self
            .cache
            .get(video_id, (self.clock)())
            .await
            .is_some_and(|hit| replaces(stale, &hit));
        let flight = {
            let mut flights = self.lock_flights();
            if !cached && !flights.contains_key(video_id) {
                let wait = self.limiter.play_next_in();
                if wait > max_wait {
                    return Err(ResolveError::Busy(wait));
                }
            }
            flights.entry(video_id.to_owned()).or_default().clone()
        };
        let result = flight
            .get_or_init(|| self.lookup(video_id, Some(stale), Lane::Play))
            .await
            .clone();
        self.land(video_id, &flight);
        result
    }

    /// Removes a finished flight unless a newer one has taken its place.
    fn land(&self, video_id: &str, flight: &Flight) {
        let mut flights = self.lock_flights();
        if flights
            .get(video_id)
            .is_some_and(|f| Arc::ptr_eq(f, flight))
        {
            flights.remove(video_id);
        }
    }

    async fn lookup(
        &self,
        video_id: &str,
        stale: Option<&Resolved>,
        lane: Lane,
    ) -> Result<Resolved, ResolveError> {
        // A flight that finished between our cache miss and joining the map has filled the cache.
        if let Some(hit) = self.cache.get(video_id, (self.clock)()).await
            && stale.is_none_or(|s| replaces(s, &hit))
        {
            self.cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(hit);
        }
        self.fresh(video_id, lane)
            .await
            .map(|(resolved, _)| resolved)
            .map_err(|f| f.error)
    }

    /// A fresh lookup of `video_id` that skips the cache, for the canary. Returns the step that
    /// worked. The answer still lands in the cache.
    pub async fn probe(&self, video_id: &str) -> Result<Step, ProbeFailure> {
        if !is_video_id(video_id) {
            return Err(ResolveError::InvalidId(video_id.chars().take(64).collect()).into());
        }
        self.fresh(video_id, Lane::Background)
            .await
            .map(|(_, step)| step)
    }

    /// One limiter token in `lane`, then the fallback ladder. Caches a success and pauses the limiter when
    /// every step was throttled.
    async fn fresh(&self, video_id: &str, lane: Lane) -> Result<(Resolved, Step), ProbeFailure> {
        self.limiter.acquire_in(lane).await;
        self.lookups.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(video_id, "running yt-dlp");
        let started = std::time::Instant::now();
        let result = self
            .fallback
            .run(
                &self.ytdlp,
                video_id,
                &self.fallback_stats,
                &self.sticky,
                &self.events,
                lane,
            )
            .await;
        METRICS.lookup_seconds.observe(started.elapsed());
        match result {
            Ok((resolved, step)) => {
                let now = (self.clock)();
                match self.policy.valid_until(&resolved, now) {
                    Some(until) => self.cache.put(resolved.clone(), now, until).await,
                    None => tracing::warn!(video_id, "stream URLs expire too soon to cache"),
                }
                self.settled
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(video_id);
                Ok((resolved, step))
            }
            Err(failure) => {
                self.failures.fetch_add(1, Ordering::Relaxed);
                let e = &failure.error;
                if e.is_throttle() {
                    tracing::warn!(video_id, error = %e, pause = ?self.throttle_pause, "YouTube is throttling, pausing lookups");
                    self.limiter.penalize(self.throttle_pause);
                } else {
                    tracing::info!(video_id, error = %e, "lookup failed");
                }
                if e.is_settled() {
                    let until = (self.clock)() + self.settled_for;
                    self.settled
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(video_id.to_owned(), (e.clone(), until));
                }
                Err(failure)
            }
        }
    }

    fn lock_flights(&self) -> std::sync::MutexGuard<'_, HashMap<String, Flight>> {
        self.flights.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_ids_are_checked() {
        assert!(is_video_id("0eD-GQCutAU"));
        assert!(is_video_id("ldmPWRb-XcM"));
        assert!(is_video_id("-abc_def123"));
        assert!(!is_video_id("short"));
        assert!(!is_video_id("0eD-GQCutA/"));
        assert!(!is_video_id("0eD-GQCutAUx"));
    }
}
