//! `GET /api/health`: everything the Health page shows, as JSON. It takes the same API key as
//! `/api` (`apikey=` or an `X-Api-Key` header) or a signed-in session, since it names channels and
//! the canary video.
//! `/healthz` stays the cheap liveness probe without a key.

use axum::{
    Json, Router,
    extract::State,
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Serialize;

use super::{FAILURES_BEFORE_UPDATE, canary::CanaryReport, dirs};
use crate::{
    AppState, api, auth,
    binaries::{Tool, ToolStatus},
    jellyfin::JellyfinHealth,
    matcher::MatcherHealth,
    play::{
        PlayLoad,
        table::{self, IndexUsage},
    },
    resolver::{ResolverStats, Step, StepCounts, StickyStatus},
    store::StoreSizes,
    watcher::{WatcherHealth, backfill::BackfillHealth},
};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/health", get(handler))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Overall {
    Ok,
    /// Working, but YouTube is pushing back: the limiter is paused or the canary failed lately.
    Degraded,
    /// Lookups are not working: the canary is failing or yt-dlp does not run.
    Failing,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub version: BuildInfo,
    pub status: Overall,
    /// Why the status is not ok, one short line each. Empty when it is ok.
    pub reasons: Vec<String>,
    pub binaries: Vec<ToolStatus>,
    pub limiter: LimiterReport,
    pub canary: CanaryReport,
    pub fallback: Vec<StepReport>,
    /// The fallback step lookups start at for now, null while they start plain.
    pub sticky: Option<StickyStatus>,
    pub watcher: WatcherHealth,
    /// The channel search backfills of shows without a playlist.
    pub backfills: Vec<BackfillHealth>,
    pub resolver: ResolverStats,
    pub matcher: MatcherHealth,
    /// The Jellyfin version merges, null without `[jellyfin]`.
    pub jellyfin: Option<JellyfinHealth>,
    /// How big the stores that grow with a library's history are.
    pub storage: StorageReport,
    /// First plays building and plays streaming right now.
    pub play: PlayLoad,
}

#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct StorageReport {
    #[serde(flatten)]
    pub rows: StoreSizes,
    /// The sample tables in `[play] index_dir`.
    pub play_index: IndexUsage,
}

#[derive(Debug, Clone, Serialize)]
pub struct BuildInfo {
    pub version: &'static str,
    /// The commit, 12 characters, null for builds that did not pass one in.
    pub git_sha: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LimiterReport {
    pub available: f64,
    pub queued: u32,
    pub next_in_secs: f64,
    /// Time left on a slow-down after a 429 or bot check.
    pub paused_for_secs: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepReport {
    pub step: Step,
    pub configured: bool,
    #[serde(flatten)]
    pub counts: StepCounts,
}

fn canary_reason(failures: u32) -> String {
    if failures == 1 {
        "the canary failed its last check".to_owned()
    } else {
        format!("the canary failed {failures} checks in a row")
    }
}

impl HealthReport {
    /// The files and bytes in `[play] index_dir`, read off the async threads.
    pub async fn play_index_usage(state: &AppState) -> IndexUsage {
        let dir = state.config.play_index_dir();
        tokio::task::spawn_blocking(move || table::usage(&dir))
            .await
            .unwrap_or_default()
    }

    /// The folders Tubarr cannot write to, probed off the async threads.
    async fn unwritable_dirs(state: &AppState) -> Vec<(dirs::WritableDir, std::io::Error)> {
        let config = state.config.clone();
        tokio::task::spawn_blocking(move || dirs::check(&config))
            .await
            .unwrap_or_default()
    }

    pub async fn collect(state: &AppState) -> Self {
        let binaries = state.binaries.status().await;
        let limiter = state.limiter.status();
        let canary = state.health.canary().report();
        let configured = state.resolver.fallback().steps();
        let fallback = Step::ALL
            .into_iter()
            .map(|step| StepReport {
                step,
                configured: configured.contains(&step),
                counts: state.resolver.fallback_stats().get(step),
            })
            .collect();

        let yt_dlp_runs = binaries
            .iter()
            .any(|b| b.tool == Tool::YtDlp && b.installed.is_some());
        let mut failing = Vec::new();
        if let Some(arch) = state.binaries.unsupported_arch() {
            failing.push(format!(
                "no managed yt-dlp or deno build for {arch}: set their paths under [binaries]"
            ));
        }
        if !yt_dlp_runs {
            failing.push("yt-dlp is not installed or does not run".to_owned());
        }
        if canary.failures >= FAILURES_BEFORE_UPDATE {
            failing.push(canary_reason(canary.failures));
        }
        let mut degraded = Vec::new();
        for (dir, error) in Self::unwritable_dirs(state).await {
            let reason = dirs::reason(&dir, &error);
            if dir.critical {
                failing.push(reason);
            } else {
                degraded.push(reason);
            }
        }
        if canary.failures > 0 && canary.failures < FAILURES_BEFORE_UPDATE {
            degraded.push(canary_reason(canary.failures));
        }
        if limiter.paused_for.is_some() {
            degraded.push("lookups are paused after YouTube pushed back".to_owned());
        }
        let (status, reasons) = if !failing.is_empty() {
            failing.extend(degraded);
            (Overall::Failing, failing)
        } else if !degraded.is_empty() {
            (Overall::Degraded, degraded)
        } else {
            (Overall::Ok, Vec::new())
        };

        let backfills = state.watcher.backfill_health().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cannot read the backfills for the health report");
            Vec::new()
        });

        let jellyfin = match &state.jellyfin {
            Some(j) => Some(j.health().await),
            None => None,
        };

        let rows = state.db.store_sizes().await.unwrap_or_else(|e| {
            tracing::warn!(error = %e, "cannot count the stores for the health report");
            StoreSizes::default()
        });
        let play_index = Self::play_index_usage(state).await;

        Self {
            version: BuildInfo {
                version: crate::VERSION,
                git_sha: crate::GIT_SHA,
            },
            status,
            reasons,
            binaries,
            limiter: LimiterReport {
                available: limiter.available,
                queued: limiter.queued,
                next_in_secs: limiter.next_in.as_secs_f64(),
                paused_for_secs: limiter.paused_for.map(|d| d.as_secs_f64()),
            },
            canary,
            fallback,
            sticky: state.resolver.sticky(),
            watcher: state.watcher.health(),
            backfills,
            resolver: state.resolver.stats(),
            matcher: state.matcher.health(),
            jellyfin,
            storage: StorageReport { rows, play_index },
            play: state.play.load(),
        }
    }
}

async fn handler(State(state): State<AppState>, parts: Parts) -> Response {
    let params = api::query_params(parts.uri.query());
    let by_query = params.contains_key("apikey") && api::checked_key(&state, &params).is_some();
    if !by_query
        && auth::authenticate(&state, &parts.headers, &parts.extensions)
            .await
            .is_err()
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "missing or wrong API key"})),
        )
            .into_response();
    }
    Json(HealthReport::collect(&state).await).into_response()
}
