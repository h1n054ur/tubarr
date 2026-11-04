//! Listing the official playlists that number playlist shows, about once a day each.
//!
//! A playlist listing is one lookup like a channel listing: it waits on the shared limiter, holds
//! off while the limiter is paused, and a 429 or bot check pauses every lookup. Playlists named by
//! active mappings are listed about once an interval. One never listed before (a draft asked for
//! it, or a mapping just got it) is listed soon.
//! The entries are channel uploads, so they land in `channel_videos` too.

use std::{collections::HashMap, time::Duration};

use serde::Serialize;
use tokio::time::Instant;

use super::{
    LISTING_RETRY, LISTING_START_DELAY, ListingRun, WatchError, Watcher, jitter, listing,
    schedule::{self, Schedule},
};
use crate::store::{PlaylistItem, SeenVideo, VideoSource, unix_now};

/// A playlist listing reads at most this many videos: a few continuation pages, enough for any
/// drama's season.
const MAX_PLAYLIST_VIDEOS: u32 = 500;
/// Playlists never listed before go this far apart, so a burst of new ones does not hit YouTube at
/// once.
const FIRST_LISTING_GAP: Duration = Duration::from_secs(5 * 60);

/// What the Health page shows for one playlist. Times are Unix seconds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PlaylistHealth {
    pub playlist_id: String,
    pub last_listing_at: Option<i64>,
    /// Videos in the last listing.
    pub videos: Option<u64>,
    pub last_listing_error: Option<String>,
}

/// What the playlist loop did with a playlist that came due.
#[derive(Debug)]
pub enum PlaylistTurn {
    /// Lookups are paused for this long, so nothing was asked.
    Held(Duration),
    Listed(Result<ListingRun, WatchError>),
}

/// A playlist id is 12 to 64 characters of base64url (`PL...`, `OLAK5uy_...`, `UU...`).
pub fn is_playlist_id(id: &str) -> bool {
    (12..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

impl Watcher {
    /// Lists one playlist with yt-dlp, waiting on the shared limiter first, and stores its order.
    pub async fn list_playlist(&self, playlist_id: &str) -> Result<ListingRun, WatchError> {
        if !is_playlist_id(playlist_id) {
            return Err(WatchError::InvalidPlaylist(
                playlist_id.chars().take(64).collect(),
            ));
        }
        let result = self.run_playlist_listing(playlist_id).await;
        self.with_playlist(playlist_id, |h| match &result {
            Ok(run) => {
                h.last_listing_at = Some(unix_now());
                h.videos = Some(run.seen as u64);
                h.last_listing_error = None;
            }
            Err(e) => h.last_listing_error = Some(e.to_string()),
        });
        if result.is_ok() {
            self.notify_found();
        }
        result
    }

    async fn run_playlist_listing(&self, playlist_id: &str) -> Result<ListingRun, WatchError> {
        let args = listing::playlist_args(&self.ytdlp, playlist_id, MAX_PLAYLIST_VIDEOS);
        let output = self.lookup(args, playlist_id).await?;
        let entries = listing::parse(&output)?;
        let mut new = 0;
        for entry in &entries {
            // Without a channel the matcher could not place it anyway. The order still keeps it.
            let Some(channel_id) = &entry.channel_id else {
                continue;
            };
            let seen = SeenVideo {
                video_id: entry.video_id.clone(),
                channel_id: channel_id.clone(),
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
        let items: Vec<PlaylistItem> = entries
            .iter()
            .map(|e| PlaylistItem {
                video_id: e.video_id.clone(),
                title: e.title.clone(),
                duration_secs: e.duration,
            })
            .collect();
        self.db.replace_playlist(playlist_id, &items).await?;
        Ok(ListingRun {
            seen: entries.len(),
            new,
        })
    }

    /// One turn of the playlist loop: holds off while the limiter is paused, else lists.
    pub async fn playlist_turn(&self, playlist_id: &str) -> PlaylistTurn {
        match self.limiter.status().paused_for {
            Some(pause) => PlaylistTurn::Held(pause),
            None => PlaylistTurn::Listed(self.list_playlist(playlist_id).await),
        }
    }

    pub(super) async fn playlist_loop(&self) {
        let mut schedule = Schedule::default();
        let started = Instant::now();
        let interval = self.settings.playlist_interval;
        loop {
            let Some((playlist, due)) = self.next_playlist_due(&mut schedule, started).await else {
                continue;
            };
            tokio::time::sleep_until(due).await;
            let delay = match self.playlist_turn(&playlist).await {
                PlaylistTurn::Held(pause) => pause,
                PlaylistTurn::Listed(Ok(run)) => {
                    tracing::info!(
                        playlist,
                        videos = run.seen,
                        new = run.new,
                        "playlist listed"
                    );
                    interval + jitter(interval / 24)
                }
                PlaylistTurn::Listed(Err(e)) => {
                    tracing::warn!(playlist, error = %e, "playlist list failed, trying again later");
                    LISTING_RETRY + jitter(LISTING_RETRY / 6)
                }
            };
            schedule.set(&playlist, Instant::now() + delay);
        }
    }

    /// Like `next_due` for channels: syncs `schedule` with the watched playlists and returns the
    /// next one once it is due within a minute, else sleeps that minute and returns `None`.
    async fn next_playlist_due(
        &self,
        schedule: &mut Schedule,
        started: Instant,
    ) -> Option<(String, Instant)> {
        let read = async {
            let watched = self.db.watched_playlists().await?;
            let requested = self.db.requested_playlists().await?;
            Ok::<_, WatchError>((watched, requested))
        };
        let (watched, requested) = match read.await {
            Ok(lists) => lists,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the watched playlists");
                tokio::time::sleep(super::IDLE_CHECK).await;
                return None;
            }
        };
        let playlists: Vec<String> = watched
            .into_iter()
            .filter(|p| {
                let ok = is_playlist_id(p);
                if !ok {
                    tracing::warn!(playlist = p, "skipping a playlist id that is not valid");
                }
                ok
            })
            .collect();
        let new = schedule.sync(&playlists);
        let last = self.db.playlist_listing_times().await.unwrap_or_default();
        self.sync_playlists(&playlists, &last).await;
        let plan = first_playlist_listings(
            new,
            &requested,
            &last,
            unix_now(),
            Instant::now(),
            started,
            self.settings.playlist_interval,
        );
        for (p, at) in plan {
            schedule.set(&p, at);
        }
        match schedule.next() {
            Some((p, due)) if due <= Instant::now() + super::IDLE_CHECK => Some((p, due)),
            _ => {
                tokio::time::sleep(super::IDLE_CHECK).await;
                None
            }
        }
    }

    /// Keeps health rows for exactly `playlists`, filling new ones from the store.
    async fn sync_playlists(&self, playlists: &[String], last: &HashMap<String, i64>) {
        let missing: Vec<String> = {
            let mut states = self.lock_playlists();
            states.retain(|p, _| playlists.contains(p));
            playlists
                .iter()
                .filter(|p| !states.contains_key(*p))
                .cloned()
                .collect()
        };
        for p in missing {
            let videos = match self.db.playlist_items(&p).await {
                Ok(items) => items.map(|i| i.len() as u64),
                Err(_) => None,
            };
            self.with_playlist(&p, |h| {
                h.last_listing_at = last.get(&p).copied();
                h.videos = videos;
            });
        }
    }

    fn with_playlist<T>(&self, playlist_id: &str, f: impl FnOnce(&mut PlaylistHealth) -> T) -> T {
        let mut states = self.lock_playlists();
        let state = states
            .entry(playlist_id.to_owned())
            .or_insert_with(|| PlaylistHealth {
                playlist_id: playlist_id.to_owned(),
                ..PlaylistHealth::default()
            });
        f(state)
    }

    pub(super) fn lock_playlists(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<String, PlaylistHealth>> {
        self.playlists.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// First listing times for playlists new to the schedule. One that was never listed goes soon,
/// but never in the first minutes after a start: draft requests first, then the rest a few minutes
/// apart, closer only when a whole interval would not fit them all. A playlist listed before keeps
/// its turn like a channel list, or is spread over the interval when overdue.
fn first_playlist_listings(
    new: Vec<String>,
    requested: &[String],
    last: &HashMap<String, i64>,
    now_unix: i64,
    now: Instant,
    started: Instant,
    interval: Duration,
) -> Vec<(String, Instant)> {
    let soon = now.max(started + LISTING_START_DELAY);
    let (mut unlisted, listed): (Vec<String>, Vec<String>) =
        new.into_iter().partition(|p| !last.contains_key(p));
    unlisted.sort_by_key(|p| !requested.contains(p));
    let n = u32::try_from(unlisted.len()).unwrap_or(u32::MAX).max(1);
    let step = FIRST_LISTING_GAP.min(interval / n);
    let mut plan: Vec<(String, Instant)> = (0u32..)
        .zip(unlisted)
        .map(|(i, p)| (p, soon + step * i))
        .collect();
    plan.extend(schedule::first_listings(
        &listed,
        last,
        now_unix,
        now,
        interval,
        LISTING_START_DELAY,
    ));
    plan
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    fn at(plan: &[(String, Instant)], p: &str, from: Instant) -> Duration {
        plan.iter()
            .find(|(q, _)| q == p)
            .map(|(_, t)| *t - from)
            .unwrap()
    }

    #[tokio::test(start_paused = true)]
    async fn unlisted_playlists_go_soon_and_listed_ones_keep_their_turn() {
        let started = Instant::now();
        let new = ids(&[
            "PLmapped00001",
            "PLasked000000",
            "PLmapped00002",
            "PLrecent00000",
            "PLoverdue0000",
        ]);
        let requested = ids(&["PLasked000000"]);
        let now_unix = 10 * 86_400;
        let last = HashMap::from([
            ("PLrecent00000".to_owned(), now_unix - 3 * 3600),
            ("PLoverdue0000".to_owned(), now_unix - 3 * 86_400),
        ]);

        let plan = first_playlist_listings(
            new,
            &requested,
            &last,
            now_unix,
            started,
            started,
            24 * HOUR,
        );
        let gap = FIRST_LISTING_GAP;
        assert_eq!(at(&plan, "PLasked000000", started), LISTING_START_DELAY);
        assert_eq!(
            at(&plan, "PLmapped00001", started),
            LISTING_START_DELAY + gap
        );
        assert_eq!(
            at(&plan, "PLmapped00002", started),
            LISTING_START_DELAY + 2 * gap
        );
        assert_eq!(at(&plan, "PLrecent00000", started), 21 * HOUR);
        assert_eq!(at(&plan, "PLoverdue0000", started), LISTING_START_DELAY);
    }

    #[tokio::test(start_paused = true)]
    async fn a_playlist_set_on_a_mapping_later_goes_at_once() {
        let started = Instant::now();
        let now_unix = 10 * 86_400;
        tokio::time::advance(HOUR).await;
        let now = Instant::now();
        for p in ["PLsetfirst000", "PLsetsecond00"] {
            let plan = first_playlist_listings(
                ids(&[p]),
                &[],
                &HashMap::new(),
                now_unix + 3600,
                now,
                started,
                24 * HOUR,
            );
            assert_eq!(at(&plan, p, now), Duration::ZERO);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn many_unlisted_playlists_at_start_are_staggered() {
        let started = Instant::now();
        let names: Vec<String> = (0..100).map(|i| format!("PLnew{i:08}")).collect();
        let plan = first_playlist_listings(
            names.clone(),
            &[],
            &HashMap::new(),
            10 * 86_400,
            started,
            started,
            24 * HOUR,
        );
        let mut offsets: Vec<Duration> = plan.iter().map(|(_, t)| *t - started).collect();
        offsets.sort();
        assert_eq!(offsets[0], LISTING_START_DELAY);
        assert!(offsets.windows(2).all(|w| w[1] - w[0] == FIRST_LISTING_GAP));
        assert_eq!(offsets[99], LISTING_START_DELAY + 99 * FIRST_LISTING_GAP);

        // More than an interval holds at that gap: they share the interval evenly instead.
        let names: Vec<String> = (0..1000).map(|i| format!("PLnew{i:08}")).collect();
        let plan = first_playlist_listings(
            names,
            &[],
            &HashMap::new(),
            10 * 86_400,
            started,
            started,
            24 * HOUR,
        );
        let last = plan.iter().map(|(_, t)| *t - started).max().unwrap();
        assert!(last < LISTING_START_DELAY + 24 * HOUR);
        assert!(last > LISTING_START_DELAY + 23 * HOUR);
    }

    #[test]
    fn playlist_ids_are_checked() {
        assert!(is_playlist_id("PLpitchersSeason1"));
        assert!(is_playlist_id("OLAK5uy_kQ1n8Ylq3JGs1s0wFXGs4n2cY7c3dPjGg"));
        assert!(!is_playlist_id("PLx"));
        assert!(!is_playlist_id("PLpitchers&index=2"));
        assert!(!is_playlist_id(&"P".repeat(65)));
    }
}
