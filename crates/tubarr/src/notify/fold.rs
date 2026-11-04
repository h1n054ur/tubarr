//! Turns events into cards. Noisy events fold: cookies uses into at most one card an hour, grabs
//! into one card per 10 minutes. Unmatched videos come as a daily digest read from the store.

use std::{future::Future, time::Duration};

use tokio::{
    sync::{broadcast, mpsc},
    time::{Instant, MissedTickBehavior},
};

use super::{
    card::Card,
    cards::{self, GRABS_LISTED, GrabLine},
};
use crate::{
    binaries::now_unix,
    health::HealthEvent,
    store::{Db, UnmatchedState},
};

/// After a cookies card, further uses wait this long and go out as one card with a count.
pub const COOKIES_WINDOW: Duration = Duration::from_secs(60 * 60);
/// Grabs within this long of the first one share a card.
pub const GRABS_WINDOW: Duration = Duration::from_secs(10 * 60);
/// How often the unmatched digest goes out. Skipped when nothing waits.
pub const DIGEST_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// What the folder reads from the store. [`Db`] in Tubarr, a fake in tests.
pub trait Lookups: Send + Sync {
    /// The series or film title of the release for `video_id`, if Tubarr knows it.
    fn title(&self, video_id: &str) -> impl Future<Output = Option<String>> + Send;
    /// Videos waiting for a match per reason, most common first.
    fn unmatched_waiting(&self) -> impl Future<Output = Vec<(UnmatchedState, i64)>> + Send;
}

impl Lookups for Db {
    async fn title(&self, video_id: &str) -> Option<String> {
        match self.release_by_video(video_id).await {
            Ok(release) => release.map(|r| r.arr_title),
            Err(e) => {
                tracing::debug!(video_id, error = %e, "cannot read the release for a grab card");
                None
            }
        }
    }

    async fn unmatched_waiting(&self) -> Vec<(UnmatchedState, i64)> {
        Db::unmatched_waiting(self).await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cannot count unmatched videos for the digest");
            Vec::new()
        })
    }
}

pub struct Folder<L> {
    db: L,
    out: mpsc::Sender<Card>,
    cookies: CookieFold,
    grabs: GrabFold,
}

#[derive(Default)]
struct CookieFold {
    last_card: Option<Instant>,
    pending: u32,
    latest_video: String,
}

#[derive(Default)]
struct GrabFold {
    opened: Option<Instant>,
    lines: Vec<GrabLine>,
    total: usize,
}

impl<L: Lookups> Folder<L> {
    /// Cards go to `out`, the delivery queue. A full queue drops the card rather than wait.
    pub fn new(db: L, out: mpsc::Sender<Card>) -> Self {
        Self {
            db,
            out,
            cookies: CookieFold::default(),
            grabs: GrabFold::default(),
        }
    }

    /// Runs until the event channel closes, then sends what is still folded.
    pub async fn run(mut self, mut events: broadcast::Receiver<HealthEvent>) {
        let mut digest = tokio::time::interval_at(Instant::now() + DIGEST_EVERY, DIGEST_EVERY);
        digest.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            let cookies_due = self.cookies.due();
            let grabs_due = self.grabs.due();
            tokio::select! {
                event = events.recv() => match event {
                    Ok(event) => self.on_event(event).await,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "notices fell behind, some events got no card");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                () = sleep_until(cookies_due) => self.flush_cookies(),
                () = sleep_until(grabs_due) => self.flush_grabs(),
                _ = digest.tick() => self.digest().await,
            }
        }
        self.flush_cookies();
        self.flush_grabs();
    }

    async fn on_event(&mut self, event: HealthEvent) {
        let at = now_unix();
        match event {
            HealthEvent::CanaryFailing {
                video_id,
                failures,
                error,
                kind,
                step,
                forced_update,
            } => self.send(cards::canary_failing(
                &cards::CanaryFailing {
                    video_id: &video_id,
                    failures,
                    error: &error,
                    kind,
                    step,
                    forced_update,
                },
                at,
            )),
            HealthEvent::CanaryRecovered {
                video_id,
                step,
                after_failures,
            } => self.send(cards::canary_recovered(&video_id, step, after_failures, at)),
            HealthEvent::CookiesUsed { video_id } => {
                let now = Instant::now();
                self.cookies.latest_video = video_id;
                match self.cookies.last_card {
                    Some(last) if now.duration_since(last) < COOKIES_WINDOW => {
                        self.cookies.pending += 1;
                    }
                    _ => {
                        self.cookies.last_card = Some(now);
                        let card = cards::cookies_used(1, &self.cookies.latest_video, true, at);
                        self.send(card);
                    }
                }
            }
            HealthEvent::BinaryUpdated { tool, from, to } => {
                self.send(cards::binary_updated(tool, from.as_deref(), &to, at));
            }
            HealthEvent::BinaryUpdateFailed { tool, error } => {
                self.send(cards::binary_update_failed(tool, &error, at));
            }
            HealthEvent::GrabCompleted {
                video_id,
                release_name,
                category,
            } => {
                let title = self.db.title(&video_id).await;
                let grabs = &mut self.grabs;
                grabs.opened.get_or_insert_with(Instant::now);
                grabs.total += 1;
                if grabs.lines.len() < GRABS_LISTED {
                    grabs.lines.push(GrabLine {
                        release_name,
                        title,
                        category,
                    });
                }
            }
        }
    }

    fn flush_cookies(&mut self) {
        if self.cookies.pending == 0 {
            return;
        }
        let card = cards::cookies_used(
            self.cookies.pending,
            &self.cookies.latest_video,
            false,
            now_unix(),
        );
        self.cookies.pending = 0;
        self.cookies.last_card = Some(Instant::now());
        self.send(card);
    }

    fn flush_grabs(&mut self) {
        let grabs = std::mem::take(&mut self.grabs);
        if grabs.total > 0 {
            self.send(cards::grabs(&grabs.lines, grabs.total, now_unix()));
        }
    }

    async fn digest(&mut self) {
        let counts = self.db.unmatched_waiting().await;
        if !counts.is_empty() {
            self.send(cards::unmatched(&counts, now_unix()));
        }
    }

    fn send(&self, card: Card) {
        match self.out.try_send(card) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(card)) => {
                tracing::warn!(event = card.event, "notice queue is full, dropping a card");
            }
            Err(mpsc::error::TrySendError::Closed(card)) => {
                tracing::debug!(event = card.event, "notice delivery has stopped");
            }
        }
    }
}

impl CookieFold {
    fn due(&self) -> Option<Instant> {
        if self.pending == 0 {
            return None;
        }
        self.last_card.map(|last| last + COOKIES_WINDOW)
    }
}

impl GrabFold {
    fn due(&self) -> Option<Instant> {
        self.opened.map(|opened| opened + GRABS_WINDOW)
    }
}

async fn sleep_until(at: Option<Instant>) {
    match at {
        Some(at) => tokio::time::sleep_until(at).await,
        None => std::future::pending().await,
    }
}
