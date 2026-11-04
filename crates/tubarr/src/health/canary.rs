//! The canary: a lookup of a known good video every hour or so. Failing three times in a row
//! forces a yt-dlp update, at most once a day, since a stale yt-dlp is the usual cause.

use std::{collections::VecDeque, future::Future, sync::Mutex, time::Duration};

use serde::Serialize;
use tokio::time::Instant;

use super::{Events, HealthEvent};
use crate::{
    binaries::{BinaryError, BinaryManager, Outcome, Tool, now_unix},
    config,
    ratelimit::RateLimitStatus,
    resolver::{ProbeFailure, ResolveCache, Resolver, Step},
};

/// Failures in a row that count as broken: the failing event goes out and yt-dlp gets updated.
pub const FAILURES_BEFORE_UPDATE: u32 = 3;
/// Runs kept for the Health page.
pub const CANARY_HISTORY: usize = 24;
/// Least time between two forced updates. The daily check runs on its own as well.
const FORCED_UPDATE_SPACING: Duration = Duration::from_secs(24 * 60 * 60);

/// Something that can update yt-dlp on demand. [`BinaryManager`] in Tubarr, a fake in tests.
pub trait Updater: Send + Sync {
    fn force_update(
        &self,
    ) -> impl Future<Output = Vec<(Tool, Result<Outcome, BinaryError>)>> + Send;
}

impl Updater for BinaryManager {
    async fn force_update(&self) -> Vec<(Tool, Result<Outcome, BinaryError>)> {
        self.check(true).await
    }
}

/// One canary lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CanaryRun {
    /// Unix seconds.
    pub at: u64,
    pub ok: bool,
    /// The fallback step that worked.
    pub step: Option<Step>,
    pub error: Option<String>,
}

/// What one tick did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tick {
    /// The limiter was paused or busy, so the canary left the lookup to real work.
    Skipped,
    Ok(Step),
    Failed {
        failures: u32,
        forced_update: bool,
    },
}

#[derive(Debug)]
pub struct Canary {
    settings: config::Canary,
    state: Mutex<State>,
}

#[derive(Debug, Default)]
struct State {
    history: VecDeque<CanaryRun>,
    failures: u32,
    /// Whether this streak has sent its failing event.
    announced: bool,
    skipped: u64,
    last_forced: Option<Instant>,
    last_forced_at: Option<u64>,
}

/// The canary part of the Health report.
#[derive(Debug, Clone, Serialize)]
pub struct CanaryReport {
    pub enabled: bool,
    pub video_id: Option<String>,
    pub interval_secs: u64,
    /// Failures in a row right now.
    pub failures: u32,
    /// Ticks left out because the limiter was paused or busy.
    pub skipped: u64,
    /// Unix seconds of the last update the canary forced.
    pub last_forced_update: Option<u64>,
    /// Newest last.
    pub history: Vec<CanaryRun>,
}

impl Canary {
    pub fn new(settings: config::Canary) -> Self {
        Self {
            settings,
            state: Mutex::new(State::default()),
        }
    }

    pub fn failures(&self) -> u32 {
        self.lock().failures
    }

    pub fn report(&self) -> CanaryReport {
        let state = self.lock();
        CanaryReport {
            enabled: self.settings.video_id.is_some(),
            video_id: self.settings.video_id.clone(),
            interval_secs: self.settings.interval.as_secs(),
            failures: state.failures,
            skipped: state.skipped,
            last_forced_update: state.last_forced_at,
            history: state.history.iter().cloned().collect(),
        }
    }

    /// Waits a jittered interval, then ticks, forever. Returns at once without a canary video.
    pub async fn run<C: ResolveCache, U: Updater>(
        &self,
        resolver: &Resolver<C>,
        updater: &U,
        events: &Events,
    ) {
        let Some(video_id) = self.settings.video_id.clone() else {
            tracing::info!("no canary video configured, the canary is off");
            return;
        };
        loop {
            tokio::time::sleep(self.settings.interval + self.jitter()).await;
            let status = resolver.limiter().status();
            self.tick(
                &video_id,
                status,
                resolver.probe(&video_id),
                updater,
                events,
            )
            .await;
        }
    }

    /// One canary round: `probe` is the lookup and only runs when the limiter has a token ready
    /// and nothing paused, since the canary must never hold up a play.
    pub async fn tick<U: Updater>(
        &self,
        video_id: &str,
        limiter: RateLimitStatus,
        probe: impl Future<Output = Result<Step, ProbeFailure>>,
        updater: &U,
        events: &Events,
    ) -> Tick {
        if limiter.paused_for.is_some() || limiter.queued > 0 || limiter.available < 1.0 {
            self.lock().skipped += 1;
            tracing::debug!(video_id, "limiter paused or busy, skipping the canary");
            return Tick::Skipped;
        }
        let result = probe.await;
        let now = now_unix();
        match result {
            Ok(step) => {
                let (failures, announced) = {
                    let mut state = self.lock();
                    state.push(CanaryRun {
                        at: now,
                        ok: true,
                        step: Some(step),
                        error: None,
                    });
                    let streak = (state.failures, state.announced);
                    state.failures = 0;
                    state.announced = false;
                    streak
                };
                if announced {
                    tracing::info!(video_id, ?step, after = failures, "canary works again");
                    events.publish(HealthEvent::CanaryRecovered {
                        video_id: video_id.to_owned(),
                        step,
                        after_failures: failures,
                    });
                }
                Tick::Ok(step)
            }
            Err(failure) => {
                let e = &failure.error;
                let error = e.to_string();
                let (failures, announce, force) = {
                    let mut state = self.lock();
                    state.push(CanaryRun {
                        at: now,
                        ok: false,
                        step: None,
                        error: Some(error.clone()),
                    });
                    state.failures += 1;
                    let broken = state.failures >= FAILURES_BEFORE_UPDATE;
                    let announce = broken && !state.announced;
                    state.announced |= broken;
                    let force = broken
                        && state
                            .last_forced
                            .is_none_or(|t| t.elapsed() >= FORCED_UPDATE_SPACING);
                    if force {
                        state.last_forced = Some(Instant::now());
                        state.last_forced_at = Some(now);
                    }
                    (state.failures, announce, force)
                };
                tracing::warn!(video_id, failures, error = %e, "canary lookup failed");
                if announce {
                    events.publish(HealthEvent::CanaryFailing {
                        video_id: video_id.to_owned(),
                        failures,
                        error,
                        kind: e.kind(),
                        step: failure.step,
                        forced_update: force,
                    });
                }
                if force {
                    tracing::warn!(failures, "canary keeps failing, forcing a yt-dlp update");
                    let results = updater.force_update().await;
                    for (tool, result) in &results {
                        match result {
                            Ok(outcome) => {
                                tracing::info!(tool = tool.name(), %outcome, "forced binary check")
                            }
                            Err(e) => {
                                tracing::warn!(tool = tool.name(), error = %e, "forced binary check failed")
                            }
                        }
                    }
                    events.publish_binary_results(&results);
                }
                Tick::Failed {
                    failures,
                    forced_update: force,
                }
            }
        }
    }

    fn jitter(&self) -> Duration {
        let max = u64::try_from(self.settings.jitter.as_millis()).unwrap_or(u64::MAX);
        Duration::from_millis(if max == 0 {
            0
        } else {
            rand::random_range(0..=max)
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl State {
    fn push(&mut self, run: CanaryRun) {
        if self.history.len() == CANARY_HISTORY {
            self.history.pop_front();
        }
        self.history.push_back(run);
    }
}
