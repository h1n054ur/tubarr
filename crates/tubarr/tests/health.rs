//! The fallback ladder against a fake yt-dlp, the canary's counting and forced updates, and the
//! `/api/health` report.

use std::{
    io::Write as _,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

mod support;

use axum::{body::Body, http::Request, http::StatusCode};
use http_body_util::BodyExt;
use tokio::sync::broadcast::error::TryRecvError;
use tower::ServiceExt;
use tubarr::{
    AppState,
    binaries::{BinaryError, BinaryManager, Outcome, Tool},
    config::{self, Config},
    health::{CANARY_HISTORY, Canary, Events, HealthEvent, Tick, Updater, dirs},
    ratelimit::{RateLimitConfig, RateLimitStatus, RateLimiter},
    resolver::{
        ErrorKind, Fallback, MemoryCache, PoToken, ProbeFailure, ResolveError, Resolver, Step,
        YtDlp,
    },
    store::{Db, GrabStatus, MappingKind, NewGrab, NewMapping},
};

const ID: &str = "0eD-GQCutAU";
const COOKIES: &str = "# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t0\tSID\tfake\n";

/// A fake yt-dlp that refuses with a bot check unless the `accept` file names an option it got:
/// `pot` for the bgutil extractor argument, `cookies` for `--cookies` (or `all`). It answers with
/// `info.json`, or `cookie-info.json` where that exists and it got cookies. Like the real one, it
/// writes back to the cookies file it is given.
struct Ladder {
    dir: tempfile::TempDir,
}

impl Ladder {
    fn new(accept: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().display().to_string();
        std::fs::write(
            dir.path().join("info.json"),
            info_json(&[("137", "avc1.640028", "none"), ("140", "none", "mp4a.40.2")]),
        )
        .unwrap();
        std::fs::write(dir.path().join("accept"), accept).unwrap();
        let script = format!(
            r##"#!/bin/sh
printf '%s\n' "$*" >> '{d}/calls.log'
cookie=""; pot=""; prev=""
for a in "$@"; do
  if [ "$prev" = "--cookies" ]; then cookie="$a"; fi
  case "$a" in youtubepot-bgutilhttp:base_url=*) pot="$a";; esac
  prev="$a"
done
accept=$(cat '{d}/accept')
if [ -n "$cookie" ]; then
  printf '%s\n' "$cookie" > '{d}/cookie-path'
  cp "$cookie" '{d}/seen-cookies'
  echo "# written back" >> "$cookie"
fi
if [ -n "$cookie" ] && [ -f '{d}/cookie-info.json' ]; then info='{d}/cookie-info.json'; else info='{d}/info.json'; fi
if [ "$accept" = all ] || {{ [ "$accept" = pot ] && [ -n "$pot" ]; }} || {{ [ "$accept" = cookies ] && [ -n "$cookie" ]; }}; then
  cat "$info"
  exit 0
fi
if [ "$accept" = geo ]; then
  echo "ERROR: [youtube] {ID}: Video unavailable. The uploader has not made this video available in your country" >&2
  exit 1
fi
echo "ERROR: [youtube] {ID}: Sign in to confirm you're not a bot. Use --cookies-from-browser or --cookies for the authentication." >&2
exit 1
"##
        );
        support::install_executable(&dir.path().join("yt-dlp"), script.as_bytes());
        // The real file is mounted read-only.
        let cookies = dir.path().join("cookies.txt");
        std::fs::write(&cookies, COOKIES).unwrap();
        std::fs::set_permissions(&cookies, std::fs::Permissions::from_mode(0o444)).unwrap();
        Self { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn fallback(&self, po_token: bool, cookies: bool) -> Fallback {
        Fallback {
            po_token: po_token.then(|| PoToken {
                base_url: "http://bgutil:4416/".parse().unwrap(),
                plugin_dir: Some("/config/yt-dlp-plugins".into()),
            }),
            cookies_file: cookies.then(|| self.path("cookies.txt")),
            ..Fallback::default()
        }
    }

    /// Replaces the answer in `file` with these `(format_id, vcodec, acodec)` formats.
    fn serve(&self, file: &str, formats: &[(&str, &str, &str)]) {
        std::fs::write(self.path(file), info_json(formats)).unwrap();
    }

    fn accept(&self, what: &str) {
        std::fs::write(self.path("accept"), what).unwrap();
    }

    fn resolver(&self, fallback: Fallback, events: &Events) -> Resolver<MemoryCache> {
        let limiter = Arc::new(RateLimiter::new(RateLimitConfig {
            interval: Duration::from_millis(1),
            burst: 100,
            max_jitter: Duration::ZERO,
        }));
        Resolver::new(YtDlp::new(self.path("yt-dlp")), limiter, MemoryCache::new())
            .with_fallback(fallback)
            .with_events(events.clone())
            .with_throttle_pause(Duration::from_secs(600))
    }
}

/// A `yt-dlp -J` answer with these formats, their heights taken from the format ids.
fn info_json(formats: &[(&str, &str, &str)]) -> String {
    let expire = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 6 * 3600;
    let formats: Vec<_> = formats
        .iter()
        .map(|&(id, vcodec, acodec)| {
            let height = match id {
                "137" => Some(1080),
                "136" => Some(720),
                "135" => Some(480),
                "18" => Some(324),
                _ => None,
            };
            serde_json::json!({
                "format_id": id, "vcodec": vcodec, "acodec": acodec, "protocol": "https",
                "height": height,
                "url": format!("https://rr1.googlevideo.com/videoplayback?itag={id}&expire={expire}")
            })
        })
        .collect();
    serde_json::json!({"id": ID, "title": "Episode 1", "duration": 2101, "formats": formats})
        .to_string()
}

/// Every log line of this test binary. A scoped subscriber misses events when other tests run on
/// other threads at the same time (tracing caches callsite interest globally), so the buffer is
/// global and tests look for lines that name their own video id.
#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn logs() -> &'static Logs {
    static LOGS: std::sync::OnceLock<Logs> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Logs::default();
        let writer = logs.clone();
        tracing_subscriber::fmt()
            .with_writer(move || writer.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::INFO)
            .init();
        logs
    })
}

fn log_lines_with(needle: &str) -> Vec<String> {
    String::from_utf8_lossy(&logs().0.lock().unwrap())
        .lines()
        .filter(|l| l.contains(needle))
        .map(str::to_owned)
        .collect()
}

fn drain(rx: &mut tokio::sync::broadcast::Receiver<HealthEvent>) -> Vec<HealthEvent> {
    let mut out = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(e) => out.push(e),
            Err(TryRecvError::Empty) => return out,
            Err(e) => panic!("{e:?}"),
        }
    }
}

#[tokio::test]
async fn bot_check_falls_back_to_the_po_token() {
    let fake = Ladder::new("pot");
    let events = Events::new();
    let mut rx = events.subscribe();
    let resolver = fake.resolver(fake.fallback(true, true), &events);

    let resolved = resolver.resolve(ID).await.unwrap();
    assert_eq!(resolved.video.format_id, "137");
    let calls = fake.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(!calls[0].contains("youtubepot"), "{}", calls[0]);
    assert!(
        calls[1].contains(
            "--extractor-args youtubepot-bgutilhttp:base_url=http://bgutil:4416 --plugin-dirs /config/yt-dlp-plugins"
        ),
        "{}",
        calls[1]
    );
    // Never reached the cookies step.
    assert!(!fake.path("cookie-path").exists());
    assert!(drain(&mut rx).is_empty());

    let stats = resolver.fallback_stats();
    assert_eq!(stats.get(Step::Plain).failed, 1);
    assert_eq!(stats.get(Step::PoToken).succeeded, 1);
    assert_eq!(stats.get(Step::Cookies).attempts, 0);
    assert_eq!(resolver.limiter().status().paused_for, None);
    assert_eq!(resolver.stats().lookups, 1);
}

#[tokio::test]
async fn cookies_come_last_from_a_temporary_copy_with_a_warning() {
    let fake = Ladder::new("cookies");
    let events = Events::new();
    let mut rx = events.subscribe();
    let resolver = fake.resolver(fake.fallback(true, true), &events);
    logs();

    // Its own id, so its log lines stand apart from other tests'.
    let id = "cookieTest1";
    resolver.resolve(id).await.unwrap();

    let calls = fake.calls();
    assert_eq!(calls.len(), 3, "{calls:?}");
    assert!(!calls[1].contains("--cookies"), "{}", calls[1]);
    // The cookies step keeps the PO token too, and asks player clients that support cookies.
    assert!(
        calls[2].contains("youtubepot-bgutilhttp:base_url="),
        "{}",
        calls[2]
    );
    assert!(
        calls[2].contains("--extractor-args youtube:player_client=tv,tv_downgraded,web"),
        "{}",
        calls[2]
    );
    assert!(!calls[1].contains("player_client"), "{}", calls[1]);

    let original = fake.path("cookies.txt");
    let given = PathBuf::from(
        std::fs::read_to_string(fake.path("cookie-path"))
            .unwrap()
            .trim(),
    );
    assert_ne!(given, original);
    assert_eq!(
        std::fs::read_to_string(fake.path("seen-cookies")).unwrap(),
        COOKIES
    );
    // yt-dlp wrote into the copy, the original is untouched and the copy is gone.
    assert_eq!(std::fs::read_to_string(&original).unwrap(), COOKIES);
    assert!(!given.exists(), "{}", given.display());

    let warnings = log_lines_with(id);
    assert!(
        warnings
            .iter()
            .any(|l| l.contains("WARN") && l.contains("spare account's cookies")),
        "{warnings:?}"
    );
    assert_eq!(
        drain(&mut rx),
        [HealthEvent::CookiesUsed {
            video_id: id.to_owned()
        }]
    );
    assert_eq!(resolver.fallback_stats().get(Step::Cookies).succeeded, 1);
    assert_eq!(resolver.fallback_stats().get(Step::PoToken).failed, 1);
    // Cookies working is never sticky.
    assert!(resolver.sticky().is_none());
}

#[tokio::test]
async fn a_cookies_answer_below_the_floor_is_rejected() {
    let fake = Ladder::new("cookies");
    // What yt-dlp's default logged-in clients gave on 2026-10-16: one muxed 324p format.
    fake.serve("cookie-info.json", &[("18", "avc1.42001E", "mp4a.40.2")]);
    let events = Events::new();
    let resolver = fake.resolver(fake.fallback(false, true), &events);
    logs();

    let id = "lowCookies1";
    let err = resolver.resolve(id).await.unwrap_err();
    // The plain refusal stays the reason, so the limiter still pauses.
    assert!(matches!(err, ResolveError::BotCheck(_)), "{err:?}");
    assert!(resolver.limiter().status().paused_for.is_some());
    let calls = fake.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert!(
        calls[1].contains("--extractor-args youtube:player_client=tv,tv_downgraded --cookies"),
        "{}",
        calls[1]
    );
    assert!(resolver.sticky().is_none());
    let stats = resolver.fallback_stats();
    assert_eq!(stats.get(Step::Cookies).failed, 1);
    assert_eq!(stats.get(Step::Cookies).succeeded, 0);
    let warnings = log_lines_with(id);
    assert!(
        warnings.iter().any(|l| l.contains("WARN")
            && l.contains("below the height floor")
            && l.contains("max_height=324")),
        "{warnings:?}"
    );

    // 480p through cookies is below the default floor too, 1080p is taken. Fresh resolvers, since
    // the refusal above paused the limiter.
    fake.serve(
        "cookie-info.json",
        &[("135", "avc1.4d401e", "none"), ("140", "none", "mp4a.40.2")],
    );
    let resolver = fake.resolver(fake.fallback(false, true), &events);
    let failure = resolver.probe(ID).await.unwrap_err();
    assert_eq!(failure.step, Some(Step::Cookies));
    fake.serve(
        "cookie-info.json",
        &[("137", "avc1.640028", "none"), ("140", "none", "mp4a.40.2")],
    );
    let resolver = fake.resolver(fake.fallback(false, true), &events);
    assert_eq!(resolver.probe(ID).await.unwrap(), Step::Cookies);
    assert!(resolver.sticky().is_none());
}

#[tokio::test]
async fn a_po_token_answer_below_the_floor_goes_on_to_cookies() {
    let fake = Ladder::new("all");
    fake.serve(
        "info.json",
        &[("135", "avc1.4d401e", "none"), ("140", "none", "mp4a.40.2")],
    );
    fake.serve(
        "cookie-info.json",
        &[("137", "avc1.640028", "none"), ("140", "none", "mp4a.40.2")],
    );
    let mut fallback = fake.fallback(true, true);
    fallback.min_height = 1080;
    let resolver = fake.resolver(fallback, &Events::new());
    // Plain answers with 480p and that is taken as it is: it is all the video has.
    let resolved = resolver.resolve(ID).await.unwrap();
    assert_eq!(resolved.video.format_id, "135");
    assert_eq!(fake.calls().len(), 1);

    // A sticky PO token step that answers low goes on to the cookies step.
    fake.accept("pot");
    fake.serve(
        "info.json",
        &[("137", "avc1.640028", "none"), ("140", "none", "mp4a.40.2")],
    );
    assert_eq!(resolver.probe(ID).await.unwrap(), Step::PoToken);
    assert_eq!(resolver.sticky().unwrap().step, Step::PoToken);
    fake.accept("all");
    fake.serve(
        "info.json",
        &[("135", "avc1.4d401e", "none"), ("140", "none", "mp4a.40.2")],
    );
    assert_eq!(resolver.probe(ID).await.unwrap(), Step::Cookies);
    let stats = resolver.fallback_stats();
    assert_eq!(stats.get(Step::PoToken).failed, 1);
    assert_eq!(stats.get(Step::Cookies).succeeded, 1);
    // The PO token step stays sticky, the cookies step never becomes it.
    assert_eq!(resolver.sticky().unwrap().step, Step::PoToken);
}

#[tokio::test]
async fn other_failures_do_not_fall_back() {
    let fake = Ladder::new("geo");
    let resolver = fake.resolver(fake.fallback(true, true), &Events::new());
    let err = resolver.resolve(ID).await.unwrap_err();
    assert!(matches!(err, ResolveError::GeoBlocked(_)), "{err:?}");
    assert_eq!(fake.calls().len(), 1);
    assert_eq!(resolver.stats().failures, 1);
}

#[tokio::test]
async fn every_step_refused_pauses_the_limiter() {
    let fake = Ladder::new("nothing");
    let events = Events::new();
    let mut rx = events.subscribe();
    let resolver = fake.resolver(fake.fallback(true, true), &events);
    let err = resolver.resolve(ID).await.unwrap_err();
    assert!(matches!(err, ResolveError::BotCheck(_)), "{err:?}");
    assert_eq!(fake.calls().len(), 3);
    assert!(resolver.limiter().status().paused_for.is_some());
    assert_eq!(drain(&mut rx).len(), 1);
    assert_eq!(
        std::fs::read_to_string(fake.path("cookies.txt")).unwrap(),
        COOKIES
    );
}

#[tokio::test]
async fn unreadable_cookies_skip_the_step() {
    let fake = Ladder::new("cookies");
    let mut fallback = fake.fallback(false, true);
    fallback.cookies_file = Some(fake.path("missing.txt"));
    let resolver = fake.resolver(fallback, &Events::new());
    let err = resolver.resolve(ID).await.unwrap_err();
    assert!(matches!(err, ResolveError::BotCheck(_)), "{err:?}");
    assert_eq!(fake.calls().len(), 1);
}

#[tokio::test]
async fn probe_skips_the_cache_and_names_the_step() {
    let fake = Ladder::new("pot");
    let resolver = fake.resolver(fake.fallback(true, false), &Events::new());
    resolver.resolve(ID).await.unwrap();
    resolver.resolve(ID).await.unwrap();
    assert_eq!(resolver.stats().cache_hits, 1);
    assert_eq!(resolver.probe(ID).await.unwrap(), Step::PoToken);
    // Plain, PO token, then straight to the sticky PO token step.
    assert_eq!(fake.calls().len(), 3);
    assert!(matches!(
        resolver.probe("bad").await,
        Err(ProbeFailure {
            error: ResolveError::InvalidId(_),
            step: None
        })
    ));
}

#[tokio::test]
async fn a_working_step_is_sticky_for_a_while() {
    let fake = Ladder::new("pot");
    let hour = Duration::from_secs(3600);
    let mut fallback = fake.fallback(true, true);
    fallback.sticky_for = hour;
    let resolver = fake.resolver(fallback, &Events::new());
    assert!(resolver.sticky().is_none());

    resolver.resolve(ID).await.unwrap();
    assert_eq!(fake.calls().len(), 2);
    let sticky = resolver.sticky().unwrap();
    assert_eq!(sticky.step, Step::PoToken);
    assert!(sticky.remaining_secs <= 3600.0, "{sticky:?}");

    // Starts at the PO token, no plain lookup first.
    assert_eq!(resolver.probe(ID).await.unwrap(), Step::PoToken);
    let calls = fake.calls();
    assert_eq!(calls.len(), 3);
    assert!(calls[2].contains("youtubepot"), "{}", calls[2]);

    // A failure at the sticky step goes on to the later steps. Cookies working does not become
    // the sticky step.
    fake.accept("cookies");
    assert_eq!(resolver.probe(ID).await.unwrap(), Step::Cookies);
    let calls = fake.calls();
    assert_eq!(calls.len(), 5);
    assert!(calls[3].contains("youtubepot") && !calls[3].contains("--cookies"));
    assert!(calls[4].contains("--cookies"), "{}", calls[4]);
    assert_eq!(resolver.sticky().unwrap().step, Step::PoToken);

    // Once it runs out, plain is tried first again. The clock jumps past the hour instead of
    // sleeping through a short one, which a slow runner could use up before this point.
    tokio::time::pause();
    tokio::time::advance(hour).await;
    tokio::time::resume();
    assert!(resolver.sticky().is_none());
    fake.accept("pot");
    assert_eq!(resolver.probe(ID).await.unwrap(), Step::PoToken);
    let calls = fake.calls();
    assert_eq!(calls.len(), 7);
    assert!(!calls[5].contains("youtubepot"), "{}", calls[5]);

    // Every step refused from the sticky one: the next lookup starts plain.
    fake.accept("nothing");
    let failure = resolver.probe(ID).await.unwrap_err();
    assert_eq!(failure.step, Some(Step::Cookies));
    assert!(matches!(failure.error, ResolveError::BotCheck(_)));
    assert_eq!(fake.calls().len(), 9);
    assert!(resolver.sticky().is_none());
}

/// Counts forced updates and answers with a fixed result.
struct FakeUpdater {
    calls: AtomicU32,
    fail: bool,
}

impl FakeUpdater {
    fn new(fail: bool) -> Self {
        Self {
            calls: AtomicU32::new(0),
            fail,
        }
    }
}

impl Updater for FakeUpdater {
    async fn force_update(&self) -> Vec<(Tool, Result<Outcome, BinaryError>)> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let result = if self.fail {
            Err(BinaryError::RateLimited { until: 0 })
        } else {
            Ok(Outcome::Updated {
                from: Some("2026.08.19".to_owned()),
                to: "2026.10.10".to_owned(),
            })
        };
        vec![(Tool::YtDlp, result)]
    }
}

fn canary() -> Canary {
    Canary::new(config::Canary {
        video_id: Some(ID.to_owned()),
        ..config::Canary::default()
    })
}

fn ready() -> RateLimitStatus {
    RateLimitStatus {
        available: 3.0,
        queued: 0,
        next_in: Duration::ZERO,
        paused_for: None,
    }
}

async fn fail(canary: &Canary, updater: &FakeUpdater, events: &Events) -> Tick {
    let probe = async {
        Err(ProbeFailure {
            error: ResolveError::BotCheck("ERROR: bot".to_owned()),
            step: Some(Step::PoToken),
        })
    };
    canary.tick(ID, ready(), probe, updater, events).await
}

#[tokio::test(start_paused = true)]
async fn three_failures_force_one_update_a_day() {
    let canary = canary();
    let updater = FakeUpdater::new(false);
    let events = Events::new();
    let mut rx = events.subscribe();
    let failed = |failures, forced_update| Tick::Failed {
        failures,
        forced_update,
    };

    assert_eq!(fail(&canary, &updater, &events).await, failed(1, false));
    assert_eq!(fail(&canary, &updater, &events).await, failed(2, false));
    assert!(drain(&mut rx).is_empty());
    assert_eq!(fail(&canary, &updater, &events).await, failed(3, true));
    assert_eq!(updater.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        drain(&mut rx),
        [
            HealthEvent::CanaryFailing {
                video_id: ID.to_owned(),
                failures: 3,
                error: "YouTube asked for a bot check: ERROR: bot".to_owned(),
                kind: ErrorKind::BotCheck,
                step: Some(Step::PoToken),
                forced_update: true,
            },
            HealthEvent::BinaryUpdated {
                tool: Tool::YtDlp,
                from: Some("2026.08.19".to_owned()),
                to: "2026.10.10".to_owned(),
            },
        ]
    );

    // Still failing within the day: no second update, no second failing event.
    assert_eq!(fail(&canary, &updater, &events).await, failed(4, false));
    tokio::time::advance(Duration::from_secs(23 * 3600)).await;
    assert_eq!(fail(&canary, &updater, &events).await, failed(5, false));
    assert!(drain(&mut rx).is_empty());
    tokio::time::advance(Duration::from_secs(3600)).await;
    assert_eq!(fail(&canary, &updater, &events).await, failed(6, true));
    assert_eq!(updater.calls.load(Ordering::SeqCst), 2);
    assert!(matches!(
        drain(&mut rx)[..],
        [HealthEvent::BinaryUpdated { .. }]
    ));

    let ok = canary
        .tick(ID, ready(), async { Ok(Step::PoToken) }, &updater, &events)
        .await;
    assert_eq!(ok, Tick::Ok(Step::PoToken));
    assert_eq!(
        drain(&mut rx),
        [HealthEvent::CanaryRecovered {
            video_id: ID.to_owned(),
            step: Step::PoToken,
            after_failures: 6,
        }]
    );
    let report = canary.report();
    assert_eq!(report.failures, 0);
    assert_eq!(report.history.len(), 7);
    assert_eq!(report.history.last().unwrap().step, Some(Step::PoToken));
    assert!(report.last_forced_update.is_some());

    // A fresh streak announces itself again.
    for _ in 0..3 {
        fail(&canary, &updater, &events).await;
    }
    assert!(matches!(
        drain(&mut rx)[..],
        [HealthEvent::CanaryFailing { failures: 3, .. }]
    ));
}

#[tokio::test(start_paused = true)]
async fn a_failed_update_is_an_event() {
    let canary = canary();
    let updater = FakeUpdater::new(true);
    let events = Events::new();
    let mut rx = events.subscribe();
    for _ in 0..3 {
        fail(&canary, &updater, &events).await;
    }
    let got = drain(&mut rx);
    assert!(
        matches!(
            &got[..],
            [
                HealthEvent::CanaryFailing { .. },
                HealthEvent::BinaryUpdateFailed {
                    tool: Tool::YtDlp,
                    ..
                }
            ]
        ),
        "{got:?}"
    );
}

async fn never() -> Result<Step, ProbeFailure> {
    panic!("the probe must not run")
}

#[tokio::test(start_paused = true)]
async fn canary_leaves_a_paused_or_busy_limiter_alone() {
    let canary = canary();
    let updater = FakeUpdater::new(false);
    let events = Events::new();
    let paused = RateLimitStatus {
        paused_for: Some(Duration::from_secs(60)),
        ..ready()
    };
    let busy = RateLimitStatus {
        available: 0.4,
        ..ready()
    };
    assert_eq!(
        canary.tick(ID, paused, never(), &updater, &events).await,
        Tick::Skipped
    );
    assert_eq!(
        canary.tick(ID, busy, never(), &updater, &events).await,
        Tick::Skipped
    );
    let report = canary.report();
    assert_eq!(report.skipped, 2);
    assert!(report.history.is_empty());
}

#[tokio::test(start_paused = true)]
async fn history_keeps_the_last_runs() {
    let canary = canary();
    let updater = FakeUpdater::new(false);
    let events = Events::new();
    for _ in 0..CANARY_HISTORY + 6 {
        canary
            .tick(ID, ready(), async { Ok(Step::Plain) }, &updater, &events)
            .await;
    }
    assert_eq!(canary.report().history.len(), CANARY_HISTORY);
}

const KEY: &str = "health-test-key";

async fn app(dir: &Path) -> axum::Router {
    app_with_db(dir).await.0
}

async fn app_with_db(dir: &Path) -> (axum::Router, Db) {
    app_with(dir, "").await
}

/// [`app_with_db`] with more config sections after the rest.
async fn app_with(dir: &Path, extra: &str) -> (axum::Router, Db) {
    let key_file = dir.join("api_key");
    let mut f = std::fs::File::create(&key_file).unwrap();
    writeln!(f, "{KEY}").unwrap();
    let config = Config::from_toml(&format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\ncomplete_dir = {:?}\napi_key_file = {:?}\n[binaries]\nbin_dir = {:?}\n[fallback]\npo_token_url = \"http://bgutil:4416\"\n[canary]\nvideo_id = \"{ID}\"\n{extra}",
        dir,
        dir.join("downloads"),
        key_file,
        dir.join("bin"),
    ))
    .unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    (
        tubarr::app(AppState::new(config, db.clone(), binaries).unwrap()),
        db,
    )
}

async fn get(app: &axum::Router, uri: &str, key: Option<&str>) -> (StatusCode, serde_json::Value) {
    let mut request = Request::get(uri);
    if let Some(key) = key {
        request = request.header("X-Api-Key", key);
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn health_report_needs_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path()).await;
    let (status, body) = get(&app, "/api/health", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body["error"].is_string());
    let (status, _) = get(&app, "/api/health", Some("wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = get(&app, &format!("/api/health?apikey={KEY}"), None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn health_report_shape() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path()).await;
    let (status, body) = get(&app, "/api/health", Some(KEY)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert_eq!(body["version"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(
        body["version"]["git_sha"].as_str(),
        tubarr::GIT_SHA,
        "the commit is reported when the build passed one in"
    );

    // No yt-dlp in the empty bin dir, so lookups cannot work.
    assert_eq!(body["status"], "failing");
    let tools: Vec<&str> = body["binaries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["tool"].as_str().unwrap())
        .collect();
    assert_eq!(tools, ["yt-dlp", "deno"]);
    assert!(body["binaries"][0]["installed"].is_null());
    assert_eq!(body["binaries"][0]["can_roll_back"], false);
    assert_eq!(
        body["reasons"],
        serde_json::json!(["yt-dlp is not installed or does not run"])
    );

    assert_eq!(body["limiter"]["available"], 3.0);
    assert_eq!(body["limiter"]["queued"], 0);
    assert!(body["limiter"]["paused_for_secs"].is_null());

    let canary = &body["canary"];
    assert_eq!(canary["enabled"], true);
    assert_eq!(canary["video_id"], ID);
    assert_eq!(canary["interval_secs"], 3600);
    assert_eq!(canary["failures"], 0);
    assert_eq!(canary["history"], serde_json::json!([]));

    let fallback = body["fallback"].as_array().unwrap();
    let steps: Vec<(&str, bool)> = fallback
        .iter()
        .map(|s| {
            (
                s["step"].as_str().unwrap(),
                s["configured"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [("plain", true), ("po_token", true), ("cookies", false)]
    );
    assert_eq!(fallback[0]["attempts"], 0);
    assert_eq!(fallback[0]["succeeded"], 0);
    assert!(body["sticky"].is_null());

    assert_eq!(body["watcher"]["active"], false);
    assert_eq!(body["watcher"]["channels"], serde_json::json!([]));
    assert_eq!(body["backfills"], serde_json::json!([]));
    assert_eq!(
        body["resolver"],
        serde_json::json!({"cache_hits": 0, "lookups": 0, "failures": 0})
    );
    // No run yet: no time, no error, zero counts for shows and films.
    let matcher = &body["matcher"];
    assert!(matcher["last_run_at"].is_null());
    assert!(matcher["last_error"].is_null());
    let zero = serde_json::json!({"released": 0, "pending": 0, "unmatched": 0, "capped": 0});
    assert_eq!(matcher["counts"], zero);
    assert_eq!(matcher["films"]["counts"], zero);
    assert_eq!(matcher["films"]["wanted"], 0);
    assert_eq!(matcher["skipped"], serde_json::json!([]));
    assert_eq!(matcher["matched"], 0);
    assert_eq!(matcher["unchanged"], 0);
}

#[test]
fn write_probe_creates_a_missing_folder_and_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("downloads/tubarr");
    dirs::probe(&downloads).unwrap();
    assert!(downloads.is_dir());
    assert_eq!(std::fs::read_dir(&downloads).unwrap().count(), 0);
}

#[test]
fn write_probe_fails_on_a_read_only_folder() {
    if nix::unistd::geteuid().is_root() {
        eprintln!("skipped: root writes past the mode bits");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("downloads");
    std::fs::create_dir(&downloads).unwrap();
    std::fs::set_permissions(&downloads, std::fs::Permissions::from_mode(0o555)).unwrap();
    let error = dirs::probe(&downloads).unwrap_err();
    // Nor can a missing folder be made inside it.
    let nested = dirs::probe(&downloads.join("tv")).unwrap_err();
    std::fs::set_permissions(&downloads, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(nested.kind(), std::io::ErrorKind::PermissionDenied);
    assert_eq!(std::fs::read_dir(&downloads).unwrap().count(), 0);
}

#[test]
fn writable_dirs_follow_the_config() {
    let config = Config::from_toml(
        "public_url = \"http://localhost:8480\"\ncomplete_dir = \"/downloads/yt\"\n",
    )
    .unwrap();
    let found: Vec<(&str, PathBuf, bool)> = dirs::writable_dirs(&config)
        .into_iter()
        .map(|d| (d.key, d.path, d.critical))
        .collect();
    assert_eq!(
        found,
        [
            ("complete_dir", PathBuf::from("/downloads/yt"), true),
            (
                "play.index_dir",
                PathBuf::from("/downloads/yt/.play-index"),
                false
            ),
            ("binaries.bin_dir", PathBuf::from("/config/bin"), false),
        ]
    );
    // With both binaries set by path, Tubarr never writes to bin_dir.
    let config = Config::from_toml(
        "public_url = \"http://localhost:8480\"\n[binaries]\nyt_dlp = \"/usr/bin/yt-dlp\"\ndeno = \"/usr/bin/deno\"\n",
    )
    .unwrap();
    assert!(
        dirs::writable_dirs(&config)
            .iter()
            .all(|d| d.key != "binaries.bin_dir")
    );
}

#[tokio::test]
async fn health_report_fails_while_the_downloads_folder_is_not_writable() {
    let dir = tempfile::tempdir().unwrap();
    let downloads = dir.path().join("downloads");
    let app = app(dir.path()).await;
    // A file where the folder should be stops writes even for root, which CI runs as.
    std::fs::write(&downloads, "").unwrap();

    let (status, body) = get(&app, "/api/health", Some(KEY)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["status"], "failing");
    let reasons: Vec<&str> = body["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap())
        .collect();
    let owner = format!("{}:{}", nix::unistd::geteuid(), nix::unistd::getegid());
    let expected = format!(
        "cannot write to {} (complete_dir) as {owner}: File exists (os error 17)",
        downloads.display()
    );
    assert!(reasons.contains(&expected.as_str()), "{reasons:?}");
    // The play index lives inside it by default, so it cannot be made either.
    assert!(
        reasons.iter().any(|r| r.contains("(play.index_dir)")),
        "{reasons:?}"
    );

    // Fixed on the host: the next report clears it without a restart.
    std::fs::remove_file(&downloads).unwrap();
    let (_, body) = get(&app, "/api/health", Some(KEY)).await;
    assert_eq!(
        body["reasons"],
        serde_json::json!(["yt-dlp is not installed or does not run"])
    );
    assert!(downloads.is_dir());
}

#[tokio::test]
async fn health_report_shows_the_store_sizes() {
    let dir = tempfile::tempdir().unwrap();
    let index = dir.path().join("index");
    std::fs::create_dir_all(&index).unwrap();
    std::fs::write(index.join("abc.137.10.tbt"), [0u8; 300]).unwrap();
    std::fs::write(index.join("abc.140.10.tbt"), [0u8; 100]).unwrap();
    // A write in progress is not a table yet.
    std::fs::write(index.join("abc.251.10.tbt.9.tmp"), [0u8; 50]).unwrap();
    let (app, db) = app_with(dir.path(), &format!("[play]\nindex_dir = {index:?}\n")).await;
    db.insert_grab(&NewGrab {
        nzo_id: "SABnzbd_nzo_1".to_owned(),
        release_name: "r".to_owned(),
        video_id: "abc".to_owned(),
        category: "tv".to_owned(),
        status: GrabStatus::Completed,
        storage_path: "/downloads/r".to_owned(),
        fail_message: None,
    })
    .await
    .unwrap();

    let (status, body) = get(&app, "/api/health", Some(KEY)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["storage"],
        serde_json::json!({
            "notes": 0,
            "channel_videos": 0,
            "grabs": 1,
            "activity_events": 0,
            "play_index": {"files": 2, "bytes": 400},
        })
    );
}

#[tokio::test]
async fn health_report_shows_the_play_load_against_its_caps() {
    let dir = tempfile::tempdir().unwrap();
    let (app, _db) = app_with(dir.path(), "[play]\nmax_builds = 3\nmax_streams = 5\n").await;
    let (status, body) = get(&app, "/api/health", Some(KEY)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["play"],
        serde_json::json!({
            "builds": 0,
            "builds_waiting": 0,
            "max_builds": 3,
            "streams": 0,
            "max_streams": 5,
        })
    );
}

#[tokio::test]
async fn health_report_lists_the_backfills() {
    let dir = tempfile::tempdir().unwrap();
    let (app, db) = app_with_db(dir.path()).await;
    let mapping = db
        .insert_mapping(&NewMapping {
            kind: MappingKind::Show,
            tvdb_id: Some(460_100),
            tmdb_id: None,
            title: "Dar E Nijaat".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec!["UC4JCksJF76g_MdzPVBJoC3Q".to_owned()],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        })
        .await
        .unwrap();
    db.sync_backfills(&db.backfill_jobs().await.unwrap(), 1000)
        .await
        .unwrap();
    db.record_backfill(mapping.id, 2000, Err("yt-dlp did not finish"), Some(3600))
        .await
        .unwrap();

    let (status, body) = get(&app, "/api/health", Some(KEY)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["backfills"],
        serde_json::json!([{
            "mapping_id": mapping.id,
            "title": "Dar E Nijaat",
            "last_run_at": 2000,
            "next_run_at": 5600,
            "results_found": null,
            "done": false,
            "last_error": "yt-dlp did not finish",
        }])
    );
}

#[tokio::test]
async fn healthz_stays_open() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(dir.path()).await;
    let response = app
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
