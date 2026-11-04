//! The resolver over the database cache: lookups survive a restart.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

mod support;

use tubarr::{
    AppState,
    binaries::BinaryManager,
    config::Config,
    ratelimit::{RateLimitConfig, RateLimiter},
    resolver::{Resolver, YtDlp},
    store::Db,
};

const ID: &str = "0eD-GQCutAU";

/// A trimmed `yt-dlp -J` answer. `EXPIRE` is replaced with the URLs' expiry.
const INFO: &str = r#"{
  "id": "0eD-GQCutAU",
  "title": "Kabhi Main Kabhi Tum Episode 1",
  "duration": 2101,
  "upload_date": "20240701",
  "channel_id": "UC4JCksJF76g_MdzPVBJoC3Q",
  "formats": [
    {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2", "protocol": "https",
     "filesize": 34012345, "http_headers": {"User-Agent": "Mozilla/5.0"},
     "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire=EXPIRE&itag=140"},
    {"format_id": "137", "ext": "mp4", "vcodec": "avc1.640028", "acodec": "none", "protocol": "https",
     "width": 1920, "height": 1080, "filesize_approx": 412345678,
     "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire=EXPIRE&itag=137"}
  ],
  "subtitles": {
    "ur": [{"ext": "vtt", "url": "https://www.youtube.com/api/timedtext?lang=ur&fmt=vtt"}]
  }
}"#;

/// Writes a fake yt-dlp into `dir` that counts its calls and prints `INFO`.
fn fake_ytdlp(dir: &Path, expire: SystemTime) -> PathBuf {
    let secs = expire.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let json = dir.join("info.json");
    std::fs::write(&json, INFO.replace("EXPIRE", &secs.to_string())).unwrap();
    let path = dir.join("yt-dlp");
    let script = format!(
        "#!/bin/sh\necho call >> '{}'\ncat '{}'\n",
        dir.join("calls.log").display(),
        json.display()
    );
    support::install_executable(&path, script.as_bytes());
    path
}

fn calls(dir: &Path) -> usize {
    std::fs::read_to_string(dir.join("calls.log"))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

fn limiter() -> Arc<RateLimiter> {
    Arc::new(RateLimiter::new(RateLimitConfig {
        interval: Duration::from_millis(1),
        burst: 100,
        max_jitter: Duration::ZERO,
    }))
}

#[tokio::test]
async fn a_restarted_resolver_serves_the_cached_lookup() {
    let dir = tempfile::tempdir().unwrap();
    let binary = fake_ytdlp(
        dir.path(),
        SystemTime::now() + Duration::from_secs(6 * 3600),
    );
    let db_path = dir.path().join("tubarr.db");

    let first = {
        let db = Db::open(&db_path).await.unwrap();
        let resolver = Resolver::new(YtDlp::new(&binary), limiter(), db.clone());
        let resolved = resolver.resolve(ID).await.unwrap();
        db.pool().close().await;
        resolved
    };
    assert_eq!(calls(dir.path()), 1);

    // A new process: fresh pool, fresh resolver, same file.
    let db = Db::open(&db_path).await.unwrap();
    let resolver = Resolver::new(YtDlp::new(&binary), limiter(), db.clone());
    let again = resolver.resolve(ID).await.unwrap();
    assert_eq!(again, first);
    assert_eq!(calls(dir.path()), 1, "the second resolver ran yt-dlp");
    assert_eq!(again.video.format_id, "137");
    assert_eq!(again.audio.http_headers["User-Agent"], "Mozilla/5.0");
    assert_eq!(again.subtitles.len(), 1);
}

#[tokio::test]
async fn expired_streams_are_looked_up_again_but_metadata_stays() {
    let dir = tempfile::tempdir().unwrap();
    // URLs that expire in 20 minutes are inside the 30 minute margin, so nothing is cached.
    let binary = fake_ytdlp(dir.path(), SystemTime::now() + Duration::from_secs(20 * 60));
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    let resolver = Resolver::new(YtDlp::new(&binary), limiter(), db.clone());

    resolver.resolve(ID).await.unwrap();
    resolver.resolve(ID).await.unwrap();
    assert_eq!(calls(dir.path()), 2);
    // Not cached means not stored at all, so there is no metadata either.
    assert!(db.video(ID).await.unwrap().is_none());

    // With a normal expiry the entry is stored. Once purged, metadata is still there.
    let binary = fake_ytdlp(
        dir.path(),
        SystemTime::now() + Duration::from_secs(6 * 3600),
    );
    let resolver = Resolver::new(YtDlp::new(&binary), limiter(), db.clone());
    resolver.resolve(ID).await.unwrap();
    assert_eq!(calls(dir.path()), 3);
    let far_future = i64::MAX;
    assert_eq!(db.purge_resolved(far_future).await.unwrap(), 1);
    let kept = db.video(ID).await.unwrap().unwrap();
    assert_eq!(kept.meta.title, "Kabhi Main Kabhi Tum Episode 1");
    assert_eq!(kept.meta.duration, Some(2101));
    assert_eq!(kept.subtitles[0].lang, "ur");
    resolver.resolve(ID).await.unwrap();
    assert_eq!(calls(dir.path()), 4);
}

#[tokio::test]
async fn app_state_builds_the_resolver_from_config_and_binaries() {
    let dir = tempfile::tempdir().unwrap();
    let binary = fake_ytdlp(
        dir.path(),
        SystemTime::now() + Duration::from_secs(6 * 3600),
    );
    let config = Config::from_toml(&format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\nlookup_timeout = \"5s\"\n\
         [rate_limit]\nlookups = 2\ninterval = \"10s\"\nburst = 4\njitter = \"1s\"\n\
         [binaries]\nyt_dlp = {:?}\n",
        dir.path(),
        binary
    ))
    .unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let state = AppState::new(config, db, binaries).unwrap();

    let limits = state.limiter.config();
    assert_eq!(limits.interval, Duration::from_secs(5));
    assert_eq!(limits.burst, 4);
    assert!(Arc::ptr_eq(state.resolver.limiter(), &state.limiter));

    // The resolver runs the yt-dlp the binary manager names, through the database cache.
    state.resolver.resolve(ID).await.unwrap();
    state.resolver.resolve(ID).await.unwrap();
    assert_eq!(calls(dir.path()), 1);
    assert!(state.db.video(ID).await.unwrap().is_some());
}
