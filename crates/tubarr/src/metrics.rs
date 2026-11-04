//! `GET /metrics`: counters and gauges in the Prometheus text format, off until `[metrics]
//! enabled = true`. It takes `Authorization: Bearer` with `[metrics] token_file`, or else the API
//! key. With neither set, only clients `auth.play_allowed_networks` lets through may read it.
//!
//! The counters that no other part keeps live in [`METRICS`], one per process. Everything else
//! (lookups, the limiter, the matcher's last run, store sizes) is read where it is kept, on each
//! scrape. Labels stay few and fixed: no video ids, titles or paths.

use std::{
    fmt::Write as _,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use axum::{
    Router,
    extract::State,
    http::{StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
    routing::get,
};

use crate::{AppState, api, auth::PlayAccess, health::HealthReport, store::StoreSizes};

/// The counters Tubarr adds to as it works.
pub static METRICS: Metrics = Metrics::new();

/// A count that only goes up.
#[derive(Debug)]
pub struct Counter(AtomicU64);

impl Counter {
    const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }

    pub fn inc(&self) {
        self.add(1);
    }

    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Durations in fixed buckets, in seconds.
#[derive(Debug)]
pub struct Histogram<const N: usize> {
    /// Upper bounds, rising. The `+Inf` bucket is `count`.
    bounds: [f64; N],
    buckets: [AtomicU64; N],
    count: AtomicU64,
    sum_micros: AtomicU64,
}

impl<const N: usize> Histogram<N> {
    const fn new(bounds: [f64; N]) -> Self {
        Self {
            bounds,
            buckets: [const { AtomicU64::new(0) }; N],
            count: AtomicU64::new(0),
            sum_micros: AtomicU64::new(0),
        }
    }

    pub fn observe(&self, took: Duration) {
        let secs = took.as_secs_f64();
        for (bound, bucket) in self.bounds.iter().zip(&self.buckets) {
            if secs <= *bound {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.count.fetch_add(1, Ordering::Relaxed);
        let micros = u64::try_from(took.as_micros()).unwrap_or(u64::MAX);
        self.sum_micros.fetch_add(micros, Ordering::Relaxed);
    }

    pub fn count(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }
}

/// `/play` answers by status class: 2xx, 3xx, 4xx, 5xx.
const STATUS_CLASSES: [&str; 4] = ["2xx", "3xx", "4xx", "5xx"];
/// The arrs, in the order the per-arr counters use.
pub const APPS: [&str; 2] = ["sonarr", "radarr"];
/// What a push to an arr came to, as [`crate::arr::push`] names it.
pub const PUSH_OUTCOMES: [&str; 4] = ["grabbed", "pending", "rejected", "failed"];

#[derive(Debug)]
pub struct Metrics {
    /// Fresh lookups from the limiter token to the answer, every fallback step included.
    pub lookup_seconds: Histogram<9>,
    /// Lookups that had to wait for a limiter token, and how long they waited in all.
    pub limiter_waits: Counter,
    pub limiter_wait_micros: Counter,
    play_requests: [Counter; 4],
    /// Requests that start playback (no `Range`, or one from byte 0).
    pub play_starts: Counter,
    pub play_bytes: Counter,
    /// From a start request arriving to its first body bytes leaving.
    pub play_first_byte_seconds: Histogram<9>,
    /// Sample tables read from the moofs, and ones found on disk.
    pub table_builds: Counter,
    pub table_cache_hits: Counter,
    pub matcher_seconds: Histogram<8>,
    pub matcher_failures: Counter,
    /// Releases the matcher made, shows and films together.
    pub matcher_released: Counter,
    arr_syncs: [[Counter; 2]; 2],
    arr_pushes: [[Counter; 4]; 2],
}

impl Metrics {
    const fn new() -> Self {
        Self {
            lookup_seconds: Histogram::new([0.5, 1.0, 2.0, 3.0, 5.0, 10.0, 20.0, 30.0, 60.0]),
            limiter_waits: Counter::new(),
            limiter_wait_micros: Counter::new(),
            play_requests: [const { Counter::new() }; 4],
            play_starts: Counter::new(),
            play_bytes: Counter::new(),
            play_first_byte_seconds: Histogram::new([
                0.1, 0.25, 0.5, 1.0, 2.0, 3.0, 5.0, 10.0, 30.0,
            ]),
            table_builds: Counter::new(),
            table_cache_hits: Counter::new(),
            matcher_seconds: Histogram::new([1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0]),
            matcher_failures: Counter::new(),
            matcher_released: Counter::new(),
            arr_syncs: [const { [const { Counter::new() }; 2] }; 2],
            arr_pushes: [const { [const { Counter::new() }; 4] }; 2],
        }
    }

    /// Counts one `/play` answer. Informational codes are not counted.
    pub fn play_request(&self, status: StatusCode) {
        if let Some(c) = (status.as_u16() / 100)
            .checked_sub(2)
            .and_then(|i| self.play_requests.get(usize::from(i)))
        {
            c.inc();
        }
    }

    /// Counts one sync of `app` (an entry of [`APPS`]).
    pub fn arr_sync(&self, app: &str, ok: bool) {
        if let Some(i) = APPS.iter().position(|a| a.eq_ignore_ascii_case(app)) {
            self.arr_syncs[i][usize::from(!ok)].inc();
        }
    }

    /// Counts one push to `app` that came to `outcome` (an entry of [`PUSH_OUTCOMES`]).
    pub fn arr_push(&self, app: &str, outcome: &str) {
        let a = APPS.iter().position(|a| a.eq_ignore_ascii_case(app));
        let o = PUSH_OUTCOMES.iter().position(|o| *o == outcome);
        if let (Some(a), Some(o)) = (a, o) {
            self.arr_pushes[a][o].inc();
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route("/metrics", get(handler))
}

async fn handler(State(state): State<AppState>, parts: Parts) -> Response {
    let config = &state.config.metrics;
    if !config.enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let token = config.token.as_ref().or(state.config.api_key.as_ref());
    let allowed = match token {
        Some(token) => parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .is_some_and(|given| {
                api::constant_time_eq(token.expose().as_bytes(), given.trim().as_bytes())
            }),
        None => PlayAccess::from_config(&state.config.auth)
            .allows_request(&parts.extensions, &parts.headers),
    };
    if !allowed {
        let status = if token.is_some() {
            StatusCode::UNAUTHORIZED
        } else {
            StatusCode::FORBIDDEN
        };
        return status.into_response();
    }
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        render(&state).await,
    )
        .into_response()
}

/// Every metric, in the Prometheus text format.
pub async fn render(state: &AppState) -> String {
    let m = &METRICS;
    let mut out = Text(String::new());

    let r = state.resolver.stats();
    out.counter(
        "tubarr_lookups_total",
        "Stream lookups by outcome: cache_hit needed no yt-dlp run.",
        &[
            ("outcome=\"cache_hit\"", r.cache_hits),
            ("outcome=\"ok\"", r.lookups.saturating_sub(r.failures)),
            ("outcome=\"failed\"", r.failures),
        ],
    );
    out.histogram(
        "tubarr_lookup_duration_seconds",
        "Fresh lookups from the limiter token to the answer.",
        &m.lookup_seconds,
    );
    let limiter = state.limiter.status();
    out.gauge(
        "tubarr_lookup_budget_tokens",
        "Lookup budget tokens ready now.",
        limiter.available,
    );
    out.gauge(
        "tubarr_lookup_budget_queued",
        "Lookups waiting for a budget token.",
        f64::from(limiter.queued),
    );
    out.gauge(
        "tubarr_lookup_budget_paused_seconds",
        "Time left on a pause after YouTube pushed back, 0 when not paused.",
        limiter.paused_for.unwrap_or_default().as_secs_f64(),
    );
    out.counter(
        "tubarr_lookup_budget_waits_total",
        "Lookups that had to wait for a budget token.",
        &[("", m.limiter_waits.get())],
    );
    out.gauge_counter(
        "tubarr_lookup_budget_wait_seconds_total",
        "Time lookups spent waiting for a budget token.",
        m.limiter_wait_micros.get() as f64 / 1e6,
    );
    out.gauge(
        "tubarr_ytdlp_processes",
        "yt-dlp processes running now.",
        state.resolver.processes_running() as f64,
    );

    let requests: Vec<(String, u64)> = STATUS_CLASSES
        .iter()
        .zip(&m.play_requests)
        .map(|(class, c)| (format!("status=\"{class}\""), c.get()))
        .collect();
    let requests: Vec<(&str, u64)> = requests.iter().map(|(l, v)| (l.as_str(), *v)).collect();
    out.counter(
        "tubarr_play_requests_total",
        "Answers to /play requests by status class.",
        &requests,
    );
    out.counter(
        "tubarr_play_starts_total",
        "/play requests that start playback.",
        &[("", m.play_starts.get())],
    );
    out.histogram(
        "tubarr_play_first_byte_seconds",
        "From a playback start request to its first body bytes.",
        &m.play_first_byte_seconds,
    );
    out.counter(
        "tubarr_play_bytes_total",
        "Media bytes /play has sent in its 2xx answers.",
        &[("", m.play_bytes.get())],
    );
    out.counter(
        "tubarr_play_table_builds_total",
        "Sample tables read from YouTube's moofs.",
        &[("", m.table_builds.get())],
    );
    out.counter(
        "tubarr_play_table_cache_hits_total",
        "Sample tables found in the on-disk cache.",
        &[("", m.table_cache_hits.get())],
    );

    let matcher = state.matcher.health();
    out.histogram(
        "tubarr_matcher_run_duration_seconds",
        "Matcher runs, good and failed.",
        &m.matcher_seconds,
    );
    out.counter(
        "tubarr_matcher_failures_total",
        "Matcher runs that failed.",
        &[("", m.matcher_failures.get())],
    );
    out.counter(
        "tubarr_matcher_released_total",
        "Releases the matcher made, shows and films together.",
        &[("", m.matcher_released.get())],
    );
    out.gauge(
        "tubarr_matcher_matched",
        "Mappings the last matcher run matched.",
        matcher.matched as f64,
    );
    out.gauge(
        "tubarr_matcher_unchanged",
        "Mappings the last matcher run left alone since nothing they read changed.",
        matcher.unchanged as f64,
    );

    let rows = state.db.store_sizes().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "cannot count the stores for the metrics");
        StoreSizes::default()
    });
    let index = HealthReport::play_index_usage(state).await;
    out.gauge(
        "tubarr_unmatched_notes",
        "Unmatched video notes kept.",
        rows.notes as f64,
    );
    out.gauge(
        "tubarr_channel_videos",
        "Channel listing rows kept.",
        rows.channel_videos as f64,
    );
    out.gauge("tubarr_grabs", "Grab rows kept.", rows.grabs as f64);
    out.gauge(
        "tubarr_activity_events",
        "Searches, arr pushes and play starts kept for the Activity page.",
        rows.activity_events as f64,
    );
    out.gauge(
        "tubarr_play_index_files",
        "Sample table files in [play] index_dir.",
        index.files as f64,
    );
    out.gauge(
        "tubarr_play_index_bytes",
        "Bytes of sample table files in [play] index_dir.",
        index.bytes as f64,
    );

    let syncs: Vec<(String, u64)> = APPS
        .iter()
        .zip(&m.arr_syncs)
        .flat_map(|(app, by)| {
            ["ok", "failed"]
                .iter()
                .zip(by)
                .map(move |(result, c)| (format!("app=\"{app}\",result=\"{result}\""), c.get()))
        })
        .collect();
    let syncs: Vec<(&str, u64)> = syncs.iter().map(|(l, v)| (l.as_str(), *v)).collect();
    out.counter(
        "tubarr_arr_syncs_total",
        "Syncs with Sonarr and Radarr by result.",
        &syncs,
    );
    let pushes: Vec<(String, u64)> = APPS
        .iter()
        .zip(&m.arr_pushes)
        .flat_map(|(app, by)| {
            PUSH_OUTCOMES
                .iter()
                .zip(by)
                .map(move |(o, c)| (format!("app=\"{app}\",outcome=\"{o}\""), c.get()))
        })
        .collect();
    let pushes: Vec<(&str, u64)> = pushes.iter().map(|(l, v)| (l.as_str(), *v)).collect();
    out.counter(
        "tubarr_arr_pushes_total",
        "Releases pushed to Sonarr and Radarr by outcome.",
        &pushes,
    );

    if let Some(rss) = resident_bytes() {
        out.gauge(
            "process_resident_memory_bytes",
            "Resident memory size in bytes.",
            rss as f64,
        );
    }
    out.0
}

/// The process's resident memory, from `/proc/self/status`. `None` off Linux.
fn resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// The text format, one metric family at a time.
struct Text(String);

impl Text {
    fn head(&mut self, name: &str, help: &str, kind: &str) {
        let _ = writeln!(self.0, "# HELP {name} {help}\n# TYPE {name} {kind}");
    }

    fn sample(&mut self, name: &str, labels: &str, value: impl std::fmt::Display) {
        if labels.is_empty() {
            let _ = writeln!(self.0, "{name} {value}");
        } else {
            let _ = writeln!(self.0, "{name}{{{labels}}} {value}");
        }
    }

    fn counter(&mut self, name: &str, help: &str, samples: &[(&str, u64)]) {
        self.head(name, help, "counter");
        for (labels, value) in samples {
            self.sample(name, labels, value);
        }
    }

    /// A counter kept as a fraction, such as seconds.
    fn gauge_counter(&mut self, name: &str, help: &str, value: f64) {
        self.head(name, help, "counter");
        self.sample(name, "", value);
    }

    fn gauge(&mut self, name: &str, help: &str, value: f64) {
        self.head(name, help, "gauge");
        self.sample(name, "", value);
    }

    fn histogram<const N: usize>(&mut self, name: &str, help: &str, h: &Histogram<N>) {
        self.head(name, help, "histogram");
        let bucket = format!("{name}_bucket");
        for (bound, count) in h.bounds.iter().zip(&h.buckets) {
            let le = format!("le=\"{bound}\"");
            self.sample(&bucket, &le, count.load(Ordering::Relaxed));
        }
        let count = h.count();
        self.sample(&bucket, "le=\"+Inf\"", count);
        let sum = h.sum_micros.load(Ordering::Relaxed) as f64 / 1e6;
        self.sample(&format!("{name}_sum"), "", sum);
        self.sample(&format!("{name}_count"), "", count);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn histogram_buckets_are_cumulative() {
        let h = Histogram::new([1.0, 5.0]);
        h.observe(Duration::from_millis(500));
        h.observe(Duration::from_secs(3));
        h.observe(Duration::from_secs(9));
        let mut out = Text(String::new());
        out.histogram("t", "Test.", &h);
        assert!(out.0.contains("t_bucket{le=\"1\"} 1\n"), "{}", out.0);
        assert!(out.0.contains("t_bucket{le=\"5\"} 2\n"), "{}", out.0);
        assert!(out.0.contains("t_bucket{le=\"+Inf\"} 3\n"), "{}", out.0);
        assert!(out.0.contains("t_sum 12.5\n"), "{}", out.0);
        assert!(out.0.contains("t_count 3\n"), "{}", out.0);
    }

    #[test]
    fn unknown_apps_and_outcomes_are_not_counted() {
        let m = Metrics::new();
        m.arr_push("Lidarr", "grabbed");
        m.arr_push("Sonarr", "maybe");
        m.arr_push("Sonarr", "grabbed");
        m.arr_sync("Radarr", false);
        m.play_request(StatusCode::CONTINUE);
        m.play_request(StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(m.arr_pushes[0][0].get(), 1);
        assert_eq!(
            m.arr_pushes.iter().flatten().map(Counter::get).sum::<u64>(),
            1
        );
        assert_eq!(m.arr_syncs[1][1].get(), 1);
        assert_eq!(m.play_requests[3].get(), 1);
        assert_eq!(m.play_requests.iter().map(Counter::get).sum::<u64>(), 1);
    }
}
