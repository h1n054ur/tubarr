//! The film matcher in the background run: wanted films from a fake Radarr serving T16's
//! fixture, watcher rows in the real database and resolver lookups through a fake yt-dlp.

mod support;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{Json, Router, routing::get};
use tokio::sync::Notify;
use tubarr::{
    arr::{ArrConnections, Radarr, sync_radarr},
    config::Config,
    matcher::ShowMatcher,
    ratelimit::{RateLimitConfig, RateLimiter},
    resolver::{Resolver, YtDlp},
    store::{
        Db, EpisodeFilter, MatchState, NewMapping, PinTarget, SeenVideo, StoredRelease,
        UnmatchedState, VideoSource,
    },
};

const GOLDMINES: &str = "UCyoXW-Dse7fURq30EWl_CUA";
const SHEMAROO: &str = "UCF1JIbMUs6uqoZEY1Haw0GQ";
/// 2024-07-01 00:00 UTC.
const T0: i64 = 1_719_792_000;

/// A trimmed `yt-dlp -J` answer at 720p. `VIDEO_ID` is replaced with the id asked for.
const INFO: &str = r#"{
  "id": "VIDEO_ID",
  "title": "Full Movie",
  "duration": 9300,
  "upload_date": "20240701",
  "formats": [
    {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2", "protocol": "https",
     "filesize": 34012345, "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire=EXPIRE&itag=140"},
    {"format_id": "137", "ext": "mp4", "vcodec": "avc1.640028", "acodec": "none", "protocol": "https",
     "width": 1280, "height": 720, "filesize": 212345678,
     "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire=EXPIRE&itag=137"}
  ]
}"#;

fn fake_ytdlp(dir: &Path) -> PathBuf {
    let expire = SystemTime::now() + Duration::from_secs(6 * 3600);
    let secs = expire.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let json = dir.join("info.json");
    std::fs::write(&json, INFO.replace("EXPIRE", &secs.to_string())).unwrap();
    let path = dir.join("yt-dlp");
    let script = format!(
        "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\nid=\"${{last##*v=}}\"\n\
         sed \"s/VIDEO_ID/$id/\" '{json}'\n",
        json = json.display(),
    );
    support::install_executable(&path, script.as_bytes());
    path
}

/// Radarr with T16's fixture: Theri (tagged youtube, monitored, no file), Mersal (tagged youtube
/// and 4k, has a file), Dune: Part Two (tagged 4k only).
async fn fake_radarr() -> String {
    let app = Router::new()
        .route(
            "/api/v3/tag",
            get(|| async { Json(support::fixture_json("arr/api/radarr/tag.json")) }),
        )
        .route(
            "/api/v3/movie",
            get(|| async { Json(support::fixture_json("arr/api/radarr/movie.json")) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/")
}

struct Setup {
    db: Db,
    matcher: ShowMatcher,
    _dir: tempfile::TempDir,
}

/// Films synced from Radarr, Theri on Goldmines, Shemaroo as the global film channel, and a
/// matcher with Radarr at `radarr_url` (the fake when `None`) that keeps the notes of the
/// fixture's old uploads.
async fn setup(radarr_url: Option<&str>) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    let key = dir.path().join("radarr.key");
    std::fs::write(&key, "0123456789abcdef0123456789abcdef").unwrap();
    let fake = fake_radarr().await;
    let config = |url: &str| {
        Config::from_toml(&format!(
            "public_url = \"http://tubarr.test:8480\"\n\
             [radarr]\nurl = \"{url}\"\napi_key_file = \"{}\"\n\
             [films]\nchannels = [\"{SHEMAROO}\"]\n\
             [matcher]\nnote_retention = \"0s\"\n",
            key.display()
        ))
        .unwrap()
    };
    let synced = config(&fake);
    let radarr = synced.radarr.clone().unwrap();
    let report = sync_radarr(&db, &Radarr::new(&radarr).unwrap(), &synced.arr.tag)
        .await
        .unwrap();
    assert_eq!(report.tagged, 2);
    let theri = db.mapping_by_tmdb(352_173).await.unwrap().unwrap();
    db.update_mapping(
        theri.id,
        &NewMapping {
            kind: theri.kind,
            tvdb_id: None,
            tmdb_id: theri.tmdb_id,
            title: theri.title.clone(),
            language: theri.language.clone(),
            channel_ids: vec![GOLDMINES.to_owned()],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        },
    )
    .await
    .unwrap();

    let config = config(radarr_url.unwrap_or(&fake));
    let limiter = Arc::new(RateLimiter::new(RateLimitConfig {
        interval: Duration::from_millis(1),
        burst: 100,
        max_jitter: Duration::ZERO,
    }));
    let resolver = Arc::new(Resolver::new(
        YtDlp::new(fake_ytdlp(dir.path())),
        limiter,
        db.clone(),
    ));
    let matcher = ShowMatcher::new(
        db.clone(),
        resolver,
        ArrConnections::from_config(&config),
        config.matcher.clone(),
        Arc::new(Notify::new()),
    )
    .with_films(config.arr.tag.clone(), config.films.clone());
    Setup {
        db,
        matcher,
        _dir: dir,
    }
}

async fn seen(db: &Db, id: &str, channel: &str, title: &str, day: i64, mins: i64) {
    db.upsert_seen_video(&SeenVideo {
        video_id: id.to_owned(),
        channel_id: channel.to_owned(),
        title: title.to_owned(),
        published_at: Some(T0 + day * 86_400),
        published_approx: false,
        duration_secs: Some(mins * 60),
        source: VideoSource::Rss,
    })
    .await
    .unwrap();
}

async fn uploads(db: &Db) {
    seen(
        db,
        "theri-full1",
        GOLDMINES,
        "Theri (2016) Full Hindi Dubbed Movie | Vijay, Samantha | Atlee",
        1,
        155,
    )
    .await;
    seen(
        db,
        "theri-trail",
        GOLDMINES,
        "Theri Official Hindi Trailer | Vijay",
        0,
        2,
    )
    .await;
    seen(
        db,
        "mersal-full",
        SHEMAROO,
        "Mersal Full Movie | Vijay",
        2,
        170,
    )
    .await;
    // Dune is not tagged youtube: never matched, whatever the title says.
    seen(
        db,
        "dune-part02",
        SHEMAROO,
        "Dune: Part Two Full Movie",
        3,
        166,
    )
    .await;
}

async fn releases(db: &Db, tmdb: i64) -> Vec<StoredRelease> {
    let id = db.mapping_by_tmdb(tmdb).await.unwrap().unwrap().id;
    db.releases_for_mappings(&[id], &EpisodeFilter::Any, 100)
        .await
        .unwrap()
}

async fn state(db: &Db, id: &str) -> MatchState {
    db.channel_video(id).await.unwrap().unwrap().match_state
}

#[tokio::test]
async fn a_run_matches_the_tagged_monitored_films() {
    let s = setup(None).await;
    uploads(&s.db).await;

    let summary = s.matcher.run_once().await.unwrap();
    // Durations came from the channel lists, so both films go out at the assumed height without
    // a lookup. The budget left over then verifies the two releases, which corrects the height.
    assert_eq!(summary.lookups, 2);

    let theri = releases(&s.db, 352_173).await;
    assert_eq!(theri.len(), 1);
    let r = &theri[0];
    assert_eq!(r.video_id, "theri-full1");
    assert_eq!(r.target, PinTarget::Film);
    assert_eq!(r.year, Some(2016));
    assert_eq!(r.imdb_id, Some(4_807_012));
    assert_eq!(r.language, "Hindi");
    assert_eq!(r.height, 720);
    // Mersal has a file in Radarr and is still wanted, from the global channel.
    let mersal = releases(&s.db, 471_507).await;
    assert_eq!(mersal.len(), 1);
    assert_eq!(mersal[0].language, "Tamil");
    assert_eq!(mersal[0].imdb_id, None);

    assert_eq!(state(&s.db, "theri-full1").await, MatchState::Matched);
    assert_eq!(state(&s.db, "mersal-full").await, MatchState::Matched);
    assert_eq!(state(&s.db, "theri-trail").await, MatchState::Ignored);
    assert_eq!(state(&s.db, "dune-part02").await, MatchState::Ignored);

    let health = s.matcher.health();
    assert_eq!(health.last_error, None);
    assert_eq!(health.films.wanted, 2);
    assert_eq!(health.films.counts.released, 2);
    assert_eq!(health.films.counts.pending, 0);
    assert_eq!(health.films.radarr_error, None);

    let again = s.matcher.run_once().await.unwrap();
    assert_eq!(again.lookups, 0);
    assert_eq!(again.report.released, 0);
    assert_eq!(again.report.replaced, 0);
    assert_eq!(again.report.notes_changed, 0);
}

#[tokio::test]
async fn the_films_own_channel_beats_a_longer_global_upload() {
    let s = setup(None).await;
    uploads(&s.db).await;
    // A second Theri upload, longer, on the global channel: Goldmines (Theri's own) still wins.
    seen(
        &s.db,
        "theri-shema",
        SHEMAROO,
        "Theri Full Movie | Hindi Dubbed",
        4,
        161,
    )
    .await;
    s.matcher.run_once().await.unwrap();
    let theri = releases(&s.db, 352_173).await;
    assert_eq!(theri[0].video_id, "theri-full1");
    let id = s.db.mapping_by_tmdb(352_173).await.unwrap().unwrap().id;
    let notes = s.db.unmatched(id).await.unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].video_id, "theri-shema");
    assert_eq!(notes[0].state, UnmatchedState::Duplicate);
}

#[tokio::test]
async fn radarr_down_leaves_films_alone_and_says_so() {
    let s = setup(Some("http://127.0.0.1:9/")).await;
    uploads(&s.db).await;
    let summary = s.matcher.run_once().await.unwrap();
    assert_eq!(summary.report.released, 0);
    assert!(releases(&s.db, 352_173).await.is_empty());
    let health = s.matcher.health();
    assert_eq!(health.last_error, None, "shows still ran");
    assert_eq!(health.films.wanted, 0);
    assert!(health.films.radarr_error.is_some());
    // Film channel rows are not judged on a run that could not read Radarr.
    assert_eq!(state(&s.db, "theri-full1").await, MatchState::New);
}
