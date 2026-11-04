//! How Tubarr is doing with YouTube: the canary lookup, the events other parts (Discord cards)
//! listen to, and the `GET /api/health` report. Grabs go out on the same channel.

mod canary;
pub mod dirs;
mod report;

use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;

pub use canary::{CANARY_HISTORY, Canary, CanaryRun, FAILURES_BEFORE_UPDATE, Tick, Updater};
pub use report::{HealthReport, router};

use crate::{
    binaries::{BinaryError, BinaryManager, Outcome, Tool},
    config,
    resolver::{ErrorKind, ResolveCache, Resolver, Step},
};

/// Something worth telling a person about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum HealthEvent {
    /// The canary has failed [`FAILURES_BEFORE_UPDATE`] times in a row. Sent once per streak.
    CanaryFailing {
        video_id: String,
        failures: u32,
        error: String,
        kind: ErrorKind,
        /// The last fallback step the failing lookup tried.
        step: Option<Step>,
        /// Whether the canary is forcing a yt-dlp update now (at most once a day).
        forced_update: bool,
    },
    /// The canary works again after a streak that sent [`HealthEvent::CanaryFailing`].
    CanaryRecovered {
        video_id: String,
        step: Step,
        after_failures: u32,
    },
    /// A lookup fell back to the spare account's cookies. Sent on every use, so a flagged account
    /// shows early.
    CookiesUsed {
        video_id: String,
    },
    BinaryUpdated {
        tool: Tool,
        from: Option<String>,
        to: String,
    },
    BinaryUpdateFailed {
        tool: Tool,
        error: String,
    },
    /// Sonarr or Radarr grabbed a release and its `.strm` is written.
    GrabCompleted {
        video_id: String,
        release_name: String,
        category: String,
    },
}

/// The broadcast channel for [`HealthEvent`]s. Cheap to clone. Events sent while nobody listens
/// are dropped, and a listener that falls more than the capacity behind loses the oldest ones.
#[derive(Debug, Clone)]
pub struct Events(broadcast::Sender<HealthEvent>);

impl Events {
    const CAPACITY: usize = 64;

    pub fn new() -> Self {
        Self(broadcast::channel(Self::CAPACITY).0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HealthEvent> {
        self.0.subscribe()
    }

    pub fn publish(&self, event: HealthEvent) {
        // An error only means no one is listening right now.
        let _ = self.0.send(event);
    }

    /// One event per binary that was updated or failed to update.
    pub fn publish_binary_results(&self, results: &[(Tool, Result<Outcome, BinaryError>)]) {
        for (tool, result) in results {
            match result {
                Ok(Outcome::Updated { from, to }) => self.publish(HealthEvent::BinaryUpdated {
                    tool: *tool,
                    from: from.clone(),
                    to: to.clone(),
                }),
                Ok(Outcome::Skipped | Outcome::Current(_)) => {}
                Err(e) => self.publish(HealthEvent::BinaryUpdateFailed {
                    tool: *tool,
                    error: e.to_string(),
                }),
            }
        }
    }
}

impl Default for Events {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared behind an `Arc` in `AppState`.
#[derive(Debug)]
pub struct Health {
    events: Events,
    canary: Canary,
}

impl Health {
    pub fn new(canary: config::Canary, events: Events) -> Self {
        Self {
            events,
            canary: Canary::new(canary),
        }
    }

    pub fn events(&self) -> &Events {
        &self.events
    }

    pub fn canary(&self) -> &Canary {
        &self.canary
    }

    /// The canary loop. Returns at once when no canary video is configured.
    pub async fn run_canary<C: ResolveCache>(
        self: Arc<Self>,
        resolver: Arc<Resolver<C>>,
        binaries: Arc<BinaryManager>,
    ) {
        self.canary
            .run(&resolver, binaries.as_ref(), &self.events)
            .await;
    }
}
