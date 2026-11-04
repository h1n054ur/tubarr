//! The resolver against a fake yt-dlp: a shell script that prints recorded answers.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod support;

use tubarr::{
    ratelimit::{RateLimitConfig, RateLimiter},
    resolver::{CachePolicy, MemoryCache, ResolveError, Resolver, YtDlp},
};

const ID: &str = "0eD-GQCutAU";
const HOUR: Duration = Duration::from_secs(3600);

/// A trimmed `yt-dlp -J` answer for the ARY test episode. `EXPIRE` is replaced per test.
const INFO: &str = r#"{
  "id": "0eD-GQCutAU",
  "title": "Kabhi Main Kabhi Tum Episode 1",
  "duration": 2101,
  "upload_date": "20240701",
  "channel_id": "UC4JCksJF76g_MdzPVBJoC3Q",
  "formats": [
    {"format_id": "251", "ext": "webm", "vcodec": "none", "acodec": "opus", "protocol": "https",
     "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire=EXPIRE&itag=251"},
    {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2", "protocol": "https",
     "filesize": 34012345,
     "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire=EXPIRE&itag=140",
     "http_headers": {"User-Agent": "Mozilla/5.0", "Accept-Language": "en-us,en;q=0.5"}},
    {"format_id": "136", "ext": "mp4", "vcodec": "avc1.4d401f", "acodec": "none", "protocol": "https",
     "width": 1280, "height": 720, "filesize_approx": 200000000,
     "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire=EXPIRE&itag=136"},
    {"format_id": "137", "ext": "mp4", "vcodec": "avc1.640028", "acodec": "none", "protocol": "https",
     "width": 1920, "height": 1080, "filesize_approx": 412345678,
     "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire=EXPIRE&itag=137",
     "http_headers": {"User-Agent": "Mozilla/5.0"}},
    {"format_id": "248", "ext": "webm", "vcodec": "vp9", "acodec": "none", "protocol": "https",
     "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire=EXPIRE&itag=248"}
  ],
  "subtitles": {
    "ur": [{"ext": "json3", "url": "https://www.youtube.com/api/timedtext?lang=ur&fmt=json3"},
           {"ext": "vtt", "url": "https://www.youtube.com/api/timedtext?lang=ur&fmt=vtt"}]
  },
  "automatic_captions": {
    "ur-orig": [{"ext": "vtt", "url": "https://www.youtube.com/api/timedtext?lang=ur&kind=asr&fmt=vtt"}],
    "en": [{"ext": "srv3", "url": "https://www.youtube.com/api/timedtext?lang=ur&kind=asr&tlang=en&fmt=srv3"}]
  }
}"#;

struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    /// Writes a fake yt-dlp that logs its arguments and each call, then runs `body`.
    fn new(body: &str) -> Self {
        let fake = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        fake.set_body(body);
        fake
    }

    /// Replaces what the fake runs after logging the call.
    fn set_body(&self, body: &str) {
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\n{body}\n",
            log = self.dir.path().join("calls.log").display()
        );
        support::install_executable(&self.binary(), script.as_bytes());
    }

    /// A fake that prints `INFO` with the given expiry after `delay` seconds.
    fn answering(expire: SystemTime, delay: &str) -> Self {
        let secs = expire.duration_since(UNIX_EPOCH).unwrap().as_secs();
        let json = INFO.replace("EXPIRE", &secs.to_string());
        let fake = Self::new("");
        let json_path = fake.dir.path().join("info.json");
        std::fs::write(&json_path, json).unwrap();
        fake.set_body(&format!("sleep {delay}\ncat '{}'", json_path.display()));
        fake
    }

    fn binary(&self) -> PathBuf {
        self.dir.path().join("yt-dlp")
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn ytdlp(&self) -> YtDlp {
        YtDlp::new(self.binary())
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

fn fast_limiter() -> Arc<RateLimiter> {
    Arc::new(RateLimiter::new(RateLimitConfig {
        interval: Duration::from_millis(1),
        burst: 100,
        max_jitter: Duration::ZERO,
    }))
}

/// A wall clock the test moves by hand.
struct ManualClock(Arc<Mutex<SystemTime>>);

impl ManualClock {
    fn new(start: SystemTime) -> Self {
        Self(Arc::new(Mutex::new(start)))
    }

    fn advance(&self, by: Duration) {
        *self.0.lock().unwrap() += by;
    }

    fn clock(&self) -> tubarr::resolver::Clock {
        let now = self.0.clone();
        Arc::new(move || *now.lock().unwrap())
    }
}

fn resolver(ytdlp: YtDlp, clock: &ManualClock) -> Resolver<MemoryCache> {
    Resolver::new(ytdlp, fast_limiter(), MemoryCache::new()).with_clock(clock.clock())
}

fn t0() -> SystemTime {
    UNIX_EPOCH + 500_000 * HOUR
}

#[tokio::test]
async fn resolves_formats_metadata_and_subtitles() {
    let fake = Fake::answering(t0() + 6 * HOUR, "0");
    let mut ytdlp = fake.ytdlp();
    ytdlp.deno = Some(PathBuf::from("/config/bin/deno"));
    let clock = ManualClock::new(t0());
    let r = resolver(ytdlp, &clock).resolve(ID).await.unwrap();

    assert_eq!(r.meta.id, ID);
    assert_eq!(r.meta.title, "Kabhi Main Kabhi Tum Episode 1");
    assert_eq!(r.meta.duration, Some(2101));
    assert_eq!(r.meta.upload_date.as_deref(), Some("20240701"));
    assert_eq!(
        r.meta.channel_id.as_deref(),
        Some("UC4JCksJF76g_MdzPVBJoC3Q")
    );
    assert_eq!(r.video.format_id, "137");
    assert_eq!((r.video.width, r.video.height), (Some(1920), Some(1080)));
    assert_eq!(r.video.size(), Some(412_345_678));
    assert!(r.video.url.contains("itag=137"));
    assert_eq!(r.audio.format_id, "140");
    assert_eq!(r.audio.size(), Some(34_012_345));
    assert_eq!(
        r.audio.http_headers.get("User-Agent").map(String::as_str),
        Some("Mozilla/5.0")
    );
    assert_eq!(r.expires_at, Some(t0() + 6 * HOUR));

    let subs: Vec<(&str, bool, &str)> = r
        .subtitles
        .iter()
        .map(|s| (s.lang.as_str(), s.automatic, s.ext.as_str()))
        .collect();
    assert_eq!(
        subs,
        vec![
            ("ur", false, "vtt"),
            ("en", true, "srv3"),
            ("ur-orig", true, "vtt")
        ]
    );

    let calls = fake.calls();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].contains("-J"), "{}", calls[0]);
    assert!(calls[0].contains("--js-runtimes deno:/config/bin/deno"));
    assert!(calls[0].ends_with(&format!("https://www.youtube.com/watch?v={ID}")));
}

#[tokio::test]
async fn concurrent_requests_share_one_lookup() {
    let fake = Fake::answering(t0() + 6 * HOUR, "0.5");
    let clock = ManualClock::new(t0());
    let resolver = Arc::new(resolver(fake.ytdlp(), &clock));

    let mut tasks = Vec::new();
    for _ in 0..5 {
        let resolver = resolver.clone();
        tasks.push(tokio::spawn(async move { resolver.resolve(ID).await }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().unwrap().video.format_id, "137");
    }
    assert_eq!(fake.calls().len(), 1);

    // The play that follows the probe is served from the cache.
    resolver.resolve(ID).await.unwrap();
    assert_eq!(fake.calls().len(), 1);
}

#[tokio::test]
async fn cache_ends_after_the_ttl() {
    let fake = Fake::answering(t0() + 24 * HOUR, "0");
    let clock = ManualClock::new(t0());
    let resolver = resolver(fake.ytdlp(), &clock);

    resolver.resolve(ID).await.unwrap();
    clock.advance(5 * HOUR - Duration::from_secs(1));
    resolver.resolve(ID).await.unwrap();
    assert_eq!(fake.calls().len(), 1);
    clock.advance(Duration::from_secs(1));
    resolver.resolve(ID).await.unwrap();
    assert_eq!(fake.calls().len(), 2);
}

#[tokio::test]
async fn cache_never_outlives_the_url_expiry() {
    let fake = Fake::answering(t0() + HOUR, "0");
    let clock = ManualClock::new(t0());
    let resolver = resolver(fake.ytdlp(), &clock).with_policy(CachePolicy {
        ttl: 5 * HOUR,
        expiry_margin: Duration::from_secs(30 * 60),
    });

    resolver.resolve(ID).await.unwrap();
    clock.advance(Duration::from_secs(29 * 60));
    resolver.resolve(ID).await.unwrap();
    assert_eq!(fake.calls().len(), 1);
    clock.advance(Duration::from_secs(60));
    resolver.resolve(ID).await.unwrap();
    assert_eq!(fake.calls().len(), 2);
}

#[tokio::test]
async fn failures_are_classified_and_not_cached() {
    let fake = Fake::new(
        "echo 'ERROR: [youtube] 0eD-GQCutAU: Video unavailable. The uploader has not made this video available in your country' >&2\nexit 1",
    );
    let clock = ManualClock::new(t0());
    let resolver = resolver(fake.ytdlp(), &clock);
    for _ in 0..2 {
        match resolver.resolve(ID).await {
            Err(ResolveError::GeoBlocked(message)) => {
                assert!(message.starts_with("ERROR: [youtube]"), "{message}")
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(fake.calls().len(), 2);
    assert_eq!(resolver.limiter().status().paused_for, None);
}

#[tokio::test]
async fn bot_check_pauses_the_limiter() {
    let fake = Fake::new(
        "echo \"ERROR: [youtube] 0eD-GQCutAU: Sign in to confirm you're not a bot. Use --cookies-from-browser or --cookies for the authentication.\" >&2\nexit 1",
    );
    let clock = ManualClock::new(t0());
    let resolver = resolver(fake.ytdlp(), &clock).with_throttle_pause(Duration::from_secs(600));
    let err = resolver.resolve(ID).await.unwrap_err();
    assert!(matches!(err, ResolveError::BotCheck(_)), "{err:?}");
    let paused = resolver.limiter().status().paused_for.unwrap();
    assert!(paused > Duration::from_secs(590), "{paused:?}");
}

#[tokio::test]
async fn rate_limit_is_recognised() {
    let fake = Fake::new(
        "echo 'ERROR: [youtube] 0eD-GQCutAU: Unable to download API page: HTTP Error 429: Too Many Requests' >&2\nexit 1",
    );
    let clock = ManualClock::new(t0());
    let resolver = resolver(fake.ytdlp(), &clock);
    let err = resolver.resolve(ID).await.unwrap_err();
    assert!(matches!(err, ResolveError::RateLimited(_)), "{err:?}");
    assert!(resolver.limiter().status().paused_for.is_some());
}

#[tokio::test]
async fn invalid_ids_never_reach_yt_dlp() {
    let fake = Fake::new("exit 0");
    let clock = ManualClock::new(t0());
    let resolver = resolver(fake.ytdlp(), &clock);
    for id in ["", "--exec=rm -rf", "0eD-GQCutAU&list=x", "../../etc/pa"] {
        assert!(matches!(
            resolver.resolve(id).await,
            Err(ResolveError::InvalidId(_))
        ));
    }
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn garbage_output_is_a_parse_error() {
    let fake = Fake::new("echo 'not json'");
    let clock = ManualClock::new(t0());
    let err = resolver(fake.ytdlp(), &clock)
        .resolve(ID)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::Parse(_)), "{err:?}");
}

#[tokio::test]
async fn missing_binary_is_a_spawn_error() {
    let clock = ManualClock::new(t0());
    let err = resolver(YtDlp::new("/nonexistent/yt-dlp"), &clock)
        .resolve(ID)
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::Spawn { .. }), "{err:?}");
}

/// True once the process is gone or only a zombie waiting to be reaped.
fn is_dead(pid: &str) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(')')
            .is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        Err(_) => true,
    }
}

fn wait_for_file(path: &Path) -> String {
    let start = Instant::now();
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && !text.trim().is_empty()
        {
            return text.trim().to_owned();
        }
        assert!(start.elapsed() < Duration::from_secs(5), "no {path:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[tokio::test]
async fn timeout_kills_the_whole_process_group() {
    // Stands in for the PyInstaller bootloader: the real work runs in a child process.
    let fake = Fake::new("");
    let pid_file = fake.path("child.pid");
    fake.set_body(&format!(
        "sleep 30 &\necho $! > '{}'\nwait",
        pid_file.display()
    ));

    let mut ytdlp = fake.ytdlp();
    ytdlp.timeout = Duration::from_millis(500);
    let clock = ManualClock::new(t0());
    let started = Instant::now();
    let err = resolver(ytdlp, &clock).resolve(ID).await.unwrap_err();
    assert_eq!(err, ResolveError::Timeout(Duration::from_millis(500)));
    assert!(started.elapsed() < Duration::from_secs(5));

    let child = wait_for_file(&pid_file);
    let start = Instant::now();
    while !is_dead(&child) {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "child {child} survived"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn refresh_replaces_a_stale_entry_once() {
    let fake = Fake::answering(t0() + 6 * HOUR, "0");
    let clock = ManualClock::new(t0());
    let resolver = resolver(fake.ytdlp(), &clock);
    let first = resolver.resolve(ID).await.unwrap();
    assert_eq!(fake.calls().len(), 1);

    // The stale entry is what the cache holds, so refresh runs yt-dlp and caches the answer.
    let fresh = resolver.refresh(&first, HOUR).await.unwrap();
    assert_eq!(fake.calls().len(), 2);
    assert_eq!(resolver.resolve(ID).await.unwrap(), fresh);
    assert_eq!(fake.calls().len(), 2);

    // A caller holding an older copy than the cache gets the cached entry without a lookup.
    let mut older = first.clone();
    older.video.url.push_str("&old=1");
    assert_eq!(resolver.refresh(&older, HOUR).await.unwrap(), fresh);
    assert_eq!(fake.calls().len(), 2);
}

#[test]
fn recorded_film_lookup_picks_1080p_h264_and_aac() {
    let json = support::fixture_bytes("youtube/lookup-film-1080p.json");
    let r = tubarr::resolver::parse(&json).unwrap();
    assert_eq!(r.meta.id, "ldmPWRb-XcM");
    assert_eq!(r.video.format_id, "137");
    assert_eq!(r.video.height, Some(1080));
    assert_eq!(r.audio.format_id, "140");
    assert!(r.expires_at.is_some());
}

#[test]
fn recorded_dubbed_lookup_picks_the_original_audio_track() {
    let json = support::fixture_bytes("youtube/lookup-dubbed-audio.json");
    let r = tubarr::resolver::parse(&json).unwrap();
    assert_eq!(r.meta.id, "wykbxHM2Ch4");
    assert_eq!(
        r.meta.channel_id.as_deref(),
        Some("UCe9JSDmyqNgA_l2BzGHq1Ug")
    );
    assert_eq!(r.video.format_id, "137");
    assert_eq!(r.audio.format_id, "140-11");
}

#[test]
fn recorded_lookup_without_1080p_has_no_format_to_serve() {
    let json = support::fixture_bytes("youtube/lookup-episode-no-1080p.json");
    let err = tubarr::resolver::parse(&json).unwrap_err();
    assert!(matches!(err, ResolveError::NoFormats { .. }), "{err:?}");
}

#[test]
fn recorded_unavailable_id_is_classified_as_unavailable() {
    let stderr = support::fixture_string("youtube/stderr-unavailable.txt");
    let err = tubarr::resolver::classify(&stderr, Some(1));
    assert!(matches!(err, ResolveError::Unavailable(_)), "{err:?}");
}

/// A fake that prints a lookup whose only audio is WebM Opus.
fn opus_only() -> Fake {
    let fake = Fake::new("");
    let json = r#"{"id": "0eD-GQCutAU", "title": "T", "formats": [
      {"format_id": "251", "vcodec": "none", "acodec": "opus", "protocol": "https",
       "url": "https://rr3---sn-a.googlevideo.com/videoplayback?itag=251"},
      {"format_id": "137", "vcodec": "avc1.640028", "acodec": "none", "protocol": "https",
       "url": "https://rr3---sn-a.googlevideo.com/videoplayback?itag=137"}]}"#;
    let path = fake.path("opus.json");
    std::fs::write(&path, json).unwrap();
    fake.set_body(&format!("cat '{}'", path.display()));
    fake
}

#[tokio::test]
async fn settled_failures_are_answered_from_memory_until_they_run_out() {
    let fake = opus_only();
    let clock = ManualClock::new(t0());
    let resolver = resolver(fake.ytdlp(), &clock).with_settled_for(HOUR);

    for _ in 0..3 {
        let err = resolver.resolve(ID).await.unwrap_err();
        assert!(matches!(err, ResolveError::NoFormats { .. }), "{err:?}");
    }
    assert_eq!(fake.calls().len(), 1, "repeat plays cost no lookup");

    clock.advance(HOUR + Duration::from_secs(1));
    resolver.resolve(ID).await.unwrap_err();
    assert_eq!(fake.calls().len(), 2);
}

#[tokio::test]
async fn throttles_are_not_remembered_as_settled() {
    let fake = Fake::new(
        "echo 'ERROR: [youtube] x: Unable to download API page: HTTP Error 429: Too Many Requests' >&2\nexit 1",
    );
    let clock = ManualClock::new(t0());
    let resolver = resolver(fake.ytdlp(), &clock).with_throttle_pause(Duration::ZERO);
    resolver.resolve(ID).await.unwrap_err();
    resolver.resolve(ID).await.unwrap_err();
    assert_eq!(fake.calls().len(), 2);
}

#[tokio::test]
async fn resolve_for_play_answers_busy_at_once_when_the_wait_is_long() {
    let fake = Fake::answering(t0() + 6 * HOUR, "0");
    let clock = ManualClock::new(t0());
    let limiter = Arc::new(RateLimiter::new(RateLimitConfig {
        interval: Duration::from_secs(600),
        burst: 1,
        max_jitter: Duration::ZERO,
    }));
    let resolver =
        Resolver::new(fake.ytdlp(), limiter.clone(), MemoryCache::new()).with_clock(clock.clock());
    // Someone else holds the only token, so the next lookup would wait ten minutes.
    limiter.acquire().await;

    let started = Instant::now();
    let err = resolver
        .resolve_for_play(ID, Duration::from_secs(15))
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(1));
    match err {
        ResolveError::Busy(wait) => assert!(wait > Duration::from_secs(500), "{wait:?}"),
        other => panic!("{other:?}"),
    }
    assert!(fake.calls().is_empty());
    assert_eq!(limiter.status().queued, 0, "no token was reserved");
}

#[tokio::test]
async fn resolve_for_play_goes_ahead_of_queued_background_lookups() {
    let fake = Fake::answering(t0() + 6 * HOUR, "0");
    let clock = ManualClock::new(t0());
    let limiter = Arc::new(RateLimiter::new(RateLimitConfig {
        interval: Duration::from_millis(500),
        burst: 1,
        max_jitter: Duration::ZERO,
    }));
    let resolver =
        Resolver::new(fake.ytdlp(), limiter.clone(), MemoryCache::new()).with_clock(clock.clock());
    limiter.acquire().await;
    // Six background lookups hold the next three seconds of slots.
    let background: Vec<_> = (0..6)
        .map(|_| {
            let limiter = limiter.clone();
            tokio::spawn(async move { limiter.acquire().await })
        })
        .collect();
    tokio::task::yield_now().await;
    assert_eq!(limiter.status().queued, 6);

    let started = Instant::now();
    let r = resolver
        .resolve_for_play(ID, Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(r.audio.format_id, "140");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(fake.calls().len(), 1);
    assert!(
        background.iter().all(|t| !t.is_finished()),
        "background waits behind the play"
    );
    for task in background {
        task.abort();
    }
}

#[tokio::test]
async fn resolve_for_play_answers_busy_during_a_pause() {
    let fake = Fake::answering(t0() + 6 * HOUR, "0");
    let clock = ManualClock::new(t0());
    let limiter = Arc::new(RateLimiter::new(RateLimitConfig::default()));
    let resolver =
        Resolver::new(fake.ytdlp(), limiter.clone(), MemoryCache::new()).with_clock(clock.clock());
    limiter.penalize(Duration::from_secs(900));

    let err = resolver
        .resolve_for_play(ID, Duration::from_secs(20))
        .await
        .unwrap_err();
    match err {
        ResolveError::Busy(wait) => assert!(wait > Duration::from_secs(890), "{wait:?}"),
        other => panic!("{other:?}"),
    }
    assert!(fake.calls().is_empty());
}

#[tokio::test]
async fn refresh_answers_busy_during_a_pause_unless_the_cache_has_newer_urls() {
    let fake = Fake::answering(t0() + 6 * HOUR, "0");
    let clock = ManualClock::new(t0());
    let limiter = Arc::new(RateLimiter::new(RateLimitConfig::default()));
    let resolver =
        Resolver::new(fake.ytdlp(), limiter.clone(), MemoryCache::new()).with_clock(clock.clock());
    let cached = resolver.resolve(ID).await.unwrap();
    limiter.penalize(Duration::from_secs(900));

    let err = resolver
        .refresh(&cached, Duration::from_secs(20))
        .await
        .unwrap_err();
    assert!(matches!(err, ResolveError::Busy(_)), "{err:?}");
    assert_eq!(fake.calls().len(), 1);

    let mut older = cached.clone();
    older.audio.url.push_str("&old=1");
    assert_eq!(
        resolver
            .refresh(&older, Duration::from_secs(20))
            .await
            .unwrap(),
        cached
    );
    assert_eq!(fake.calls().len(), 1);
}

#[tokio::test]
async fn resolve_for_play_still_answers_from_the_cache_when_busy() {
    let fake = Fake::answering(t0() + 6 * HOUR, "0");
    let clock = ManualClock::new(t0());
    let limiter = Arc::new(RateLimiter::new(RateLimitConfig {
        interval: Duration::from_secs(600),
        burst: 1,
        max_jitter: Duration::ZERO,
    }));
    let resolver =
        Resolver::new(fake.ytdlp(), limiter, MemoryCache::new()).with_clock(clock.clock());
    resolver.resolve(ID).await.unwrap();
    let r = resolver
        .resolve_for_play(ID, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(r.audio.format_id, "140");
}

/// Lookups never run more yt-dlp processes at once than allowed (one by default), and a play
/// waiting for the process slot goes ahead of background lookups that waited longer.
#[tokio::test]
async fn one_ytdlp_at_a_time_and_a_play_takes_the_next_one() {
    let fake = Fake::answering(t0() + 6 * HOUR, "0");
    let running = fake.path("running.log");
    let json = fake.path("info.json");
    fake.set_body(&format!(
        "echo start >> '{log}'\nsleep 0.3\necho end >> '{log}'\ncat '{json}'",
        log = running.display(),
        json = json.display()
    ));
    let clock = ManualClock::new(t0());
    let resolver = Arc::new(resolver(fake.ytdlp(), &clock));
    let lookup = |id: &'static str, play: bool| {
        let resolver = resolver.clone();
        tokio::spawn(async move {
            if play {
                resolver.resolve_for_play(id, Duration::from_secs(20)).await
            } else {
                resolver.resolve(id).await
            }
        })
    };

    let mut tasks = vec![lookup("aaaaaaaaaaa", false)];
    while fake.calls().is_empty() {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    for (id, play) in [
        ("bbbbbbbbbbb", false),
        ("ccccccccccc", false),
        ("ddddddddddd", true),
    ] {
        tasks.push(lookup(id, play));
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    for task in tasks {
        task.await.unwrap().unwrap();
    }

    let order: Vec<String> = fake
        .calls()
        .iter()
        .map(|c| c.rsplit('=').next().unwrap().to_owned())
        .collect();
    assert_eq!(
        order,
        ["aaaaaaaaaaa", "ddddddddddd", "bbbbbbbbbbb", "ccccccccccc"]
    );
    let log = std::fs::read_to_string(&running).unwrap();
    assert_eq!(log, "start\nend\n".repeat(4), "never two at once");
}
