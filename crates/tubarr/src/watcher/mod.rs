//! Finding new uploads on mapped channels without getting the address flagged.
//!
//! Two loops share the work. The feed loop reads each channel's RSS feed about every 30 minutes,
//! a few seconds apart, with conditional GETs. Feeds are not lookups, so they skip the
//! [`RateLimiter`]. The listing loop reads each channel's full list with yt-dlp about once a day,
//! spread over the day, and every listing waits on the limiter like any other lookup. Both hold
//! off while the limiter is paused after a 429 or bot check. Whatever they find lands in
//! `channel_videos` for the matcher. YouTube sometimes answers every feed with 404 or 500 for
//! hours, so a channel whose feed has failed for longer than one feed interval gets a short
//! listing of its newest uploads at about the feed's pace instead, capped per hour across all
//! channels, until a feed read works again. A third loop lists the official playlists named by shows
//! ([`playlists`]) the same careful way, and a fourth searches channels for the back episodes of
//! shows without a playlist ([`backfill`]).

pub mod backfill;
pub mod feed;
pub mod handles;
pub mod listing;
pub mod playlists;
pub mod schedule;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use reqwest::Client;
use serde::Serialize;
use tokio::{sync::Notify, time::Instant};

use self::{
    feed::{FeedAnswer, Validators},
    playlists::PlaylistHealth,
    schedule::{FallbackPlan, Schedule},
};
use crate::{
    config,
    network::{self, OutboundProxy},
    ratelimit::{Lane, RateLimiter},
    resolver::{ResolveError, YtDlp},
    store::{Availability, Db, SHORT, SeenVideo, StoreError, VideoSource, unix_now},
};

/// How often an idle loop looks for new mappings.
const IDLE_CHECK: Duration = Duration::from_secs(60);
/// The first listings wait this long after start, so a restart loop cannot burst lookups.
const LISTING_START_DELAY: Duration = Duration::from_secs(120);
/// A failed or throttled listing is tried again after this long instead of waiting a full day.
const LISTING_RETRY: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum WatchError {
    #[error("feed request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("feed answered HTTP {0}")]
    Status(u16),
    #[error("feed is not valid Atom: {0}")]
    Feed(String),
    #[error("channel list failed: {0}")]
    Listing(#[from] ResolveError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("not a YouTube channel id: {0:?}")]
    InvalidChannel(String),
    #[error("not a YouTube playlist id: {0:?}")]
    InvalidPlaylist(String),
}

/// What the Health page shows for one channel. Times are Unix seconds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChannelHealth {
    pub channel_id: String,
    pub last_feed_at: Option<i64>,
    pub last_feed_error: Option<String>,
    /// Feed failures in a row, which set the backoff.
    pub feed_failures: u32,
    /// When the current run of feed failures began.
    pub feed_failing_since: Option<i64>,
    /// True while the feed has failed for longer than one interval and short listings stand in.
    pub feed_fallback: bool,
    pub last_fallback_at: Option<i64>,
    pub last_fallback_error: Option<String>,
    pub last_listing_at: Option<i64>,
    pub last_listing_error: Option<String>,
    /// Videos seen in feeds and lists since start, and how many of them were new.
    pub videos_seen: u64,
    pub videos_new: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct WatcherHealth {
    /// False while no mapping names a channel: the watcher then makes no requests at all.
    pub active: bool,
    pub channels: Vec<ChannelHealth>,
    /// The official playlists listed for playlist shows and drafts.
    pub playlists: Vec<PlaylistHealth>,
}

/// The result of one feed read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedPoll {
    NotModified,
    Read { seen: usize, new: usize },
}

/// The result of one channel listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListingRun {
    pub seen: usize,
    pub new: usize,
}

#[derive(Debug, Default)]
struct ChannelState {
    validators: Validators,
    /// When the current run of feed failures began, on the loop clock.
    failing_since: Option<Instant>,
    health: ChannelHealth,
}

/// Watches the channels named by mappings. Share it behind an `Arc` and start [`Watcher::run`].
#[derive(Debug)]
pub struct Watcher {
    settings: config::Watcher,
    db: Db,
    limiter: Arc<RateLimiter>,
    ytdlp: YtDlp,
    client: Client,
    /// How long lookups pause after a 429 or bot check, the same as the resolver's.
    throttle_pause: Duration,
    active: Mutex<bool>,
    channels: Mutex<HashMap<String, ChannelState>>,
    playlists: Mutex<HashMap<String, PlaylistHealth>>,
    /// Notified when a read stored something, so the matcher runs.
    found: Option<Arc<Notify>>,
    /// Channels watched on top of the mapped ones: the global film channels.
    extra_channels: Vec<String>,
    /// Channels whose daily list reads more (or fewer) than `listing_size` uploads.
    listing_sizes: HashMap<String, u32>,
}

/// The client for the channel feeds.
fn feed_client(proxy: Option<&OutboundProxy>) -> Result<Client, reqwest::Error> {
    let builder = Client::builder()
        .user_agent(concat!("tubarr/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30));
    network::youtube_client(builder, proxy)?.build()
}

impl Watcher {
    pub fn new(
        settings: config::Watcher,
        db: Db,
        limiter: Arc<RateLimiter>,
        ytdlp: YtDlp,
        throttle_pause: Duration,
        proxy: Option<&OutboundProxy>,
    ) -> Result<Self, reqwest::Error> {
        // `proxy` is `[network] proxy`, for the feed reads. yt-dlp gets it through its environment.
        let client = feed_client(proxy)?;
        Ok(Self {
            settings,
            db,
            limiter,
            ytdlp,
            client,
            throttle_pause,
            active: Mutex::new(false),
            channels: Mutex::new(HashMap::new()),
            playlists: Mutex::new(HashMap::new()),
            found: None,
            extra_channels: Vec::new(),
            listing_sizes: HashMap::new(),
        })
    }

    /// Also watches `channels`, whether or not a mapping names them.
    #[must_use]
    pub fn with_channels(mut self, channels: Vec<String>) -> Self {
        self.extra_channels = channels;
        self
    }

    /// Lists these channels' newest `n` uploads instead of `listing_size`.
    #[must_use]
    pub fn with_listing_sizes(mut self, sizes: HashMap<String, u32>) -> Self {
        self.listing_sizes = sizes;
        self
    }

    /// Notifies `found` after each feed read with new videos and each channel or playlist listing.
    pub fn with_found(mut self, found: Arc<Notify>) -> Self {
        self.found = Some(found);
        self
    }

    fn notify_found(&self) {
        if let Some(found) = &self.found {
            found.notify_one();
        }
    }

    pub fn health(&self) -> WatcherHealth {
        let mut channels: Vec<_> = self
            .lock_channels()
            .values()
            .map(|s| s.health.clone())
            .collect();
        channels.sort_by(|a, b| a.channel_id.cmp(&b.channel_id));
        let mut playlists: Vec<_> = self.lock_playlists().values().cloned().collect();
        playlists.sort_by(|a, b| a.playlist_id.cmp(&b.playlist_id));
        WatcherHealth {
            active: *self.active.lock().unwrap_or_else(|e| e.into_inner()),
            channels,
            playlists,
        }
    }

    /// Reads one channel feed and records what it lists.
    pub async fn poll_feed(&self, channel_id: &str) -> Result<FeedPoll, WatchError> {
        check_channel(channel_id)?;
        let url = format!("{}?channel_id={channel_id}", self.settings.feed_url);
        let mut validators = self.with_channel(channel_id, |s| s.validators.clone());
        let result = self.read_feed(channel_id, &url, &mut validators).await;
        self.with_channel(channel_id, |s| {
            let h = &mut s.health;
            h.last_feed_at = Some(unix_now());
            match &result {
                Ok(poll) => {
                    s.validators = validators;
                    s.failing_since = None;
                    h.last_feed_error = None;
                    h.feed_failures = 0;
                    h.feed_failing_since = None;
                    h.feed_fallback = false;
                    if let FeedPoll::Read { seen, new } = poll {
                        h.videos_seen += *seen as u64;
                        h.videos_new += *new as u64;
                    }
                }
                Err(e) => {
                    h.last_feed_error = Some(e.to_string());
                    h.feed_failures = h.feed_failures.saturating_add(1);
                    if s.failing_since.is_none() {
                        s.failing_since = Some(Instant::now());
                        h.feed_failing_since = Some(unix_now());
                    }
                }
            }
        });
        if matches!(result, Ok(FeedPoll::Read { new, .. }) if new > 0) {
            self.notify_found();
        }
        result
    }

    async fn read_feed(
        &self,
        channel_id: &str,
        url: &str,
        validators: &mut Validators,
    ) -> Result<FeedPoll, WatchError> {
        let entries = match feed::fetch(&self.client, url, validators).await? {
            FeedAnswer::NotModified => return Ok(FeedPoll::NotModified),
            FeedAnswer::Entries(entries) => entries,
        };
        let mut new = 0;
        for entry in &entries {
            let seen = SeenVideo {
                video_id: entry.video_id.clone(),
                channel_id: channel_id.to_owned(),
                title: entry.title.clone(),
                published_at: entry.published_at,
                published_approx: false,
                duration_secs: None,
                source: VideoSource::Rss,
            };
            let availability = if entry.short {
                Availability::Skip(SHORT)
            } else {
                Availability::Unknown
            };
            if self.db.record_sighting(&seen, availability).await? {
                new += 1;
            }
        }
        Ok(FeedPoll::Read {
            seen: entries.len(),
            new,
        })
    }

    /// Lists one channel in full with yt-dlp, waiting on the shared limiter first. A 429 or bot
    /// check pauses every lookup, as it does for the resolver.
    pub async fn list_channel(&self, channel_id: &str) -> Result<ListingRun, WatchError> {
        check_channel(channel_id)?;
        let size = self
            .listing_sizes
            .get(channel_id)
            .copied()
            .unwrap_or(self.settings.listing_size);
        let mut result = self.run_listing(channel_id, size).await;
        if result.is_ok()
            && let Err(e) = self
                .db
                .set_channel_listing_time(channel_id, unix_now())
                .await
        {
            result = Err(e.into());
        }
        self.with_channel(channel_id, |s| {
            let h = &mut s.health;
            match &result {
                Ok(run) => {
                    h.last_listing_at = Some(unix_now());
                    h.last_listing_error = None;
                    h.videos_seen += run.seen as u64;
                    h.videos_new += run.new as u64;
                }
                Err(e) => h.last_listing_error = Some(e.to_string()),
            }
        });
        // Listings also bring durations of videos already known.
        if result.is_ok() {
            self.notify_found();
        }
        result
    }

    /// Lists the newest `fallback_listing_size` uploads of a channel whose feed keeps failing,
    /// through the shared limiter like any listing. It does not count as the daily full listing.
    pub async fn list_fallback(&self, channel_id: &str) -> Result<ListingRun, WatchError> {
        check_channel(channel_id)?;
        let result = self
            .run_listing(channel_id, self.settings.fallback_listing_size)
            .await;
        self.with_channel(channel_id, |s| {
            let h = &mut s.health;
            match &result {
                Ok(run) => {
                    h.last_fallback_at = Some(unix_now());
                    h.last_fallback_error = None;
                    h.videos_seen += run.seen as u64;
                    h.videos_new += run.new as u64;
                }
                Err(e) => h.last_fallback_error = Some(e.to_string()),
            }
        });
        if matches!(result, Ok(ListingRun { new, .. }) if new > 0) {
            self.notify_found();
        }
        result
    }

    /// The channels whose feed has failed for longer than one feed interval, which also marks
    /// them on the Health page.
    fn failing_feeds(&self, now: Instant) -> Vec<String> {
        let interval = self.settings.rss_interval;
        let mut failing: Vec<String> = self
            .lock_channels()
            .iter_mut()
            .filter_map(|(c, s)| {
                s.health.feed_fallback = s.failing_since.is_some_and(|t| t + interval <= now);
                s.health.feed_fallback.then(|| c.clone())
            })
            .collect();
        failing.sort();
        failing
    }

    /// One yt-dlp run after waiting on the shared limiter. A 429 or bot check pauses every lookup.
    async fn lookup(&self, args: Vec<String>, what: &str) -> Result<Vec<u8>, WatchError> {
        self.limiter.acquire().await;
        match self.ytdlp.run(args, what, Lane::Background).await {
            Ok(output) => Ok(output),
            Err(e) => {
                if e.is_throttle() {
                    tracing::warn!(what, error = %e, pause = ?self.throttle_pause, "YouTube is throttling, pausing lookups");
                    self.limiter.penalize(self.throttle_pause);
                }
                Err(e.into())
            }
        }
    }

    async fn run_listing(&self, channel_id: &str, size: u32) -> Result<ListingRun, WatchError> {
        let args = listing::args(&self.ytdlp, channel_id, size);
        let output = self.lookup(args, channel_id).await?;
        let entries = listing::parse(&output)?;
        let mut new = 0;
        for entry in &entries {
            let seen = SeenVideo {
                video_id: entry.video_id.clone(),
                channel_id: channel_id.to_owned(),
                title: entry.title.clone(),
                published_at: entry.published_at,
                published_approx: entry.published_at.is_some(),
                duration_secs: entry.duration,
                source: VideoSource::Listing,
            };
            if self.db.record_sighting(&seen, entry.availability).await? {
                new += 1;
            }
        }
        Ok(ListingRun {
            seen: entries.len(),
            new,
        })
    }

    /// Runs the five loops until the task is dropped.
    pub async fn run(self: Arc<Self>) {
        tokio::join!(
            self.feed_loop(),
            self.fallback_loop(),
            self.listing_loop(),
            self.playlist_loop(),
            self.backfill_loop()
        );
    }

    async fn feed_loop(&self) {
        let mut schedule = Schedule::default();
        let mut last_read: Option<Instant> = None;
        loop {
            let Some((channel, due)) = self
                .next_due(&mut schedule, |_, new| {
                    let now = Instant::now();
                    new.into_iter().map(|c| (c, now)).collect()
                })
                .await
            else {
                continue;
            };
            // Never two feeds back to back, whatever the schedule says.
            let spacing = self.settings.rss_spacing + jitter(self.settings.rss_spacing);
            let start = last_read.map_or(due, |last| due.max(last + spacing));
            tokio::time::sleep_until(start).await;
            if let Some(pause) = self.limiter.status().paused_for {
                schedule.set(&channel, Instant::now() + pause);
                continue;
            }
            let result = self.poll_feed(&channel).await;
            last_read = Some(Instant::now());
            match &result {
                Ok(FeedPoll::Read { seen, new }) => {
                    tracing::debug!(channel, seen, new, "feed read");
                }
                Ok(FeedPoll::NotModified) => tracing::debug!(channel, "feed unchanged"),
                Err(e) => tracing::info!(channel, error = %e, "feed read failed, backing off"),
            }
            let failures = self.with_channel(&channel, |s| s.health.feed_failures);
            let delay = schedule::feed_delay(
                self.settings.rss_interval,
                failures,
                jitter(self.settings.rss_jitter),
            );
            schedule.set(&channel, Instant::now() + delay);
        }
    }

    /// Lists channels whose feed keeps failing, at about the feed's pace and within the hourly
    /// cap. The feed loop keeps trying their feeds, and a channel drops out the moment one works.
    async fn fallback_loop(&self) {
        if self.settings.fallback_per_hour == 0 {
            return;
        }
        let interval = self.settings.rss_interval;
        let mut plan = FallbackPlan::new(interval, self.settings.fallback_per_hour);
        // Failing feeds cannot need a list sooner than one interval, so this is often enough.
        let check = IDLE_CHECK.min(interval);
        loop {
            let now = Instant::now();
            plan.sync(&self.failing_feeds(now), now);
            let Some((channel, at)) = plan.next().filter(|(_, at)| *at <= now + check) else {
                tokio::time::sleep(check).await;
                continue;
            };
            tokio::time::sleep_until(at).await;
            // The feed may have come back while this list waited its turn.
            if !self.failing_feeds(Instant::now()).contains(&channel) {
                continue;
            }
            if let Some(pause) = self.limiter.status().paused_for {
                plan.postpone(&channel, Instant::now() + pause);
                continue;
            }
            let started = Instant::now();
            match self.list_fallback(&channel).await {
                Ok(run) => tracing::info!(
                    channel,
                    seen = run.seen,
                    new = run.new,
                    "feed failing, listed newest uploads instead"
                ),
                Err(e) => tracing::warn!(channel, error = %e, "fallback list failed"),
            }
            plan.ran(&channel, started, jitter(self.settings.rss_jitter));
        }
    }

    async fn listing_loop(&self) {
        let mut schedule = Schedule::default();
        let interval = self.settings.listing_interval;
        loop {
            let Some((channel, due)) = self
                .next_due(&mut schedule, |watcher, new| {
                    let last = watcher.listing_times.clone();
                    schedule::first_listings(
                        &new,
                        &last,
                        unix_now(),
                        Instant::now(),
                        interval,
                        LISTING_START_DELAY,
                    )
                })
                .await
            else {
                continue;
            };
            tokio::time::sleep_until(due).await;
            if let Some(pause) = self.limiter.status().paused_for {
                schedule.set(&channel, Instant::now() + pause);
                continue;
            }
            let delay = match self.list_channel(&channel).await {
                Ok(run) => {
                    tracing::info!(channel, seen = run.seen, new = run.new, "channel listed");
                    interval + jitter(interval / 24)
                }
                Err(e) => {
                    tracing::warn!(channel, error = %e, "channel list failed, trying again later");
                    LISTING_RETRY + jitter(LISTING_RETRY / 6)
                }
            };
            schedule.set(&channel, Instant::now() + delay);
        }
    }

    /// Syncs `schedule` with the mapped channels and returns the next channel once it is due
    /// within [`IDLE_CHECK`]. Otherwise sleeps up to [`IDLE_CHECK`] and returns `None`, so new
    /// and removed mappings are noticed within a minute. `plan` gives first times to new channels.
    async fn next_due<F>(&self, schedule: &mut Schedule, plan: F) -> Option<(String, Instant)>
    where
        F: FnOnce(&Context, Vec<String>) -> Vec<(String, Instant)>,
    {
        let channels = match self.db.watched_channels().await {
            Ok(mut channels) => {
                channels.extend(self.extra_channels.iter().cloned());
                channels.sort();
                channels.dedup();
                channels
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot read mapped channels");
                tokio::time::sleep(IDLE_CHECK).await;
                return None;
            }
        };
        let channels: Vec<String> = channels
            .into_iter()
            .filter(|c| {
                let ok = is_channel_id(c);
                if !ok {
                    tracing::warn!(
                        channel = c,
                        "skipping a mapping channel that is not a UC... id"
                    );
                }
                ok
            })
            .collect();
        self.set_active(&channels);
        let new = schedule.sync(&channels);
        if !new.is_empty() {
            let listing_times = self.db.channel_listing_times().await.unwrap_or_default();
            for (c, at) in plan(&Context { listing_times }, new) {
                schedule.set(&c, at);
            }
        }
        match schedule.next() {
            Some((channel, due)) if due <= Instant::now() + IDLE_CHECK => Some((channel, due)),
            _ => {
                tokio::time::sleep(IDLE_CHECK).await;
                None
            }
        }
    }

    fn set_active(&self, channels: &[String]) {
        *self.active.lock().unwrap_or_else(|e| e.into_inner()) = !channels.is_empty();
        let mut states = self.lock_channels();
        states.retain(|c, _| channels.contains(c));
        for c in channels {
            states.entry(c.clone()).or_insert_with(|| ChannelState {
                health: ChannelHealth {
                    channel_id: c.clone(),
                    ..ChannelHealth::default()
                },
                ..ChannelState::default()
            });
        }
    }

    fn with_channel<T>(&self, channel_id: &str, f: impl FnOnce(&mut ChannelState) -> T) -> T {
        let mut states = self.lock_channels();
        let state = states
            .entry(channel_id.to_owned())
            .or_insert_with(|| ChannelState {
                health: ChannelHealth {
                    channel_id: channel_id.to_owned(),
                    ..ChannelHealth::default()
                },
                ..ChannelState::default()
            });
        f(state)
    }

    fn lock_channels(&self) -> std::sync::MutexGuard<'_, HashMap<String, ChannelState>> {
        self.channels.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What a loop knows when it plans first times for new channels.
struct Context {
    listing_times: HashMap<String, i64>,
}

/// A channel id is `UC` and 22 characters of base64url.
pub fn is_channel_id(id: &str) -> bool {
    id.len() == 24
        && id.starts_with("UC")
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn check_channel(id: &str) -> Result<(), WatchError> {
    if is_channel_id(id) {
        Ok(())
    } else {
        Err(WatchError::InvalidChannel(id.chars().take(64).collect()))
    }
}

/// A random duration up to `max`.
fn jitter(max: Duration) -> Duration {
    let max = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
    if max == 0 {
        Duration::ZERO
    } else {
        Duration::from_millis(rand::random_range(0..=max))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_ids_are_checked() {
        assert!(is_channel_id("UC4JCksJF76g_MdzPVBJoC3Q"));
        assert!(!is_channel_id("4JCksJF76g_MdzPVBJoC3Q"));
        assert!(!is_channel_id("UC4JCksJF76g_MdzPVBJoC3/"));
        assert!(!is_channel_id("@arydigital"));
    }
}
