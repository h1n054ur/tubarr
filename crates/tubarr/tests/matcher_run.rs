//! The show matcher on its real inputs: watcher rows, Sonarr's episodes (a fake Sonarr serving
//! the T16 fixture) and resolver lookups through a fake yt-dlp.

mod support;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{Json, Router, extract::Query, routing::get};
use serde_json::Value;
use tokio::sync::Notify;
use tubarr::{
    arr::ArrConnections,
    config::Config,
    matcher::{Metadata, ShowMatcher},
    newznab::Release,
    ratelimit::{RateLimitConfig, RateLimiter},
    resolver::{Resolver, YtDlp},
    store::{
        ArrEntry, ArrSource, Availability, Db, EpisodeFilter, MappingKind, MatchState, NewMapping,
        PinTarget, SeenVideo, UnmatchedState, VideoSource,
    },
};

const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";
/// 2024-07-02 00:00 UTC, the day Sonarr's episode 1 aired.
const AIRED: i64 = 1_719_878_400;
const DAY: i64 = 86_400;
/// Episodes in the fake Sonarr's podcast, series 12.
const PODCAST_EPISODES: i64 = 12;

/// A trimmed `yt-dlp -J` answer, 38 minutes at 720p. `VIDEO_ID` is replaced with the id asked for.
const INFO: &str = r#"{
  "id": "VIDEO_ID",
  "title": "Kabhi Main Kabhi Tum Episode",
  "duration": 2280,
  "upload_date": "20240702",
  "channel_id": "UC4JCksJF76g_MdzPVBJoC3Q",
  "formats": [
    {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2", "protocol": "https",
     "filesize": 34012345, "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire=EXPIRE&itag=140"},
    {"format_id": "137", "ext": "mp4", "vcodec": "avc1.640028", "acodec": "none", "protocol": "https",
     "width": 1280, "height": 720, "filesize": 212345678,
     "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire=EXPIRE&itag=137"}
  ]
}"#;

/// A fake yt-dlp in `dir` that logs each video id it is asked for and answers `INFO` for it.
fn fake_ytdlp(dir: &Path) -> PathBuf {
    let expire = SystemTime::now() + Duration::from_secs(6 * 3600);
    let secs = expire.duration_since(UNIX_EPOCH).unwrap().as_secs();
    let json = dir.join("info.json");
    std::fs::write(&json, INFO.replace("EXPIRE", &secs.to_string())).unwrap();
    let path = dir.join("yt-dlp");
    let script = format!(
        "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\nid=\"${{last##*v=}}\"\n\
         echo \"$id\" >> '{log}'\nsed \"s/VIDEO_ID/$id/\" '{json}'\n",
        log = dir.join("calls.log").display(),
        json = json.display(),
    );
    support::install_executable(&path, script.as_bytes());
    path
}

/// The ids the fake was asked for, in order.
fn calls(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(dir.join("calls.log"))
        .map(|s| s.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

/// Sonarr's episode list for series 11 (T16's fixture): E1 has a file, E2 is missing, E3 is not
/// monitored, E4 has no air date yet. Series 12 is a podcast of [`PODCAST_EPISODES`] monitored
/// episodes without air dates. Any other series has no episodes.
async fn fake_sonarr() -> String {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Q {
        series_id: i64,
    }
    let app = Router::new().route(
        "/api/v3/episode",
        get(|Query(q): Query<Q>| async move {
            Json(match q.series_id {
                11 => support::fixture_json("arr/api/sonarr/episode-11.json"),
                12 => Value::Array(
                    (1..=PODCAST_EPISODES)
                        .map(|n| {
                            serde_json::json!({
                                "id": 1200 + n, "seriesId": 12, "seasonNumber": 1,
                                "episodeNumber": n, "title": format!("Episode {n}"),
                                "hasFile": false, "monitored": true,
                            })
                        })
                        .collect(),
                ),
                _ => Value::Array(vec![]),
            })
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}/")
}

struct Setup {
    db: Db,
    matcher: ShowMatcher,
    resolver: Arc<Resolver<Db>>,
    mapping_id: i64,
    dir: tempfile::TempDir,
}

/// A database with the series mapped from Sonarr (id 11) on the ARY channel, and a matcher that
/// may make `lookups` lookups per run.
async fn setup(lookups: u32) -> Setup {
    setup_with(lookups, "").await
}

/// [`setup`] with more `[matcher]` keys.
async fn setup_with(lookups: u32, matcher: &str) -> Setup {
    setup_series(11, lookups, matcher).await
}

/// [`setup_with`] for Sonarr series `series_id`.
async fn setup_series(series_id: i64, lookups: u32, matcher: &str) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    db.sync_arr_mappings(
        ArrSource::Sonarr,
        &[ArrEntry {
            kind: MappingKind::Show,
            key: 452_001,
            arr_id: series_id,
            title: "Kabhi Main Kabhi Tum".to_owned(),
            language: "Urdu".to_owned(),
            year: Some(2024),
            imdb_id: None,
            aliases: vec![],
        }],
    )
    .await
    .unwrap();
    let mapping = db.mapping_by_tvdb(452_001).await.unwrap().unwrap();
    db.update_mapping(
        mapping.id,
        &NewMapping {
            kind: MappingKind::Show,
            tvdb_id: Some(452_001),
            tmdb_id: None,
            title: mapping.title.clone(),
            language: mapping.language.clone(),
            channel_ids: vec![ARY.to_owned()],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        },
    )
    .await
    .unwrap();

    let (matcher, resolver) = matcher_for(&db, dir.path(), lookups, matcher).await;
    Setup {
        db,
        matcher,
        resolver,
        mapping_id: mapping.id,
        dir,
    }
}

/// A matcher on `db` with its own resolver over the fake yt-dlp in `dir`, as after a restart.
async fn matcher_for(
    db: &Db,
    dir: &Path,
    lookups: u32,
    matcher: &str,
) -> (ShowMatcher, Arc<Resolver<Db>>) {
    let key = dir.join("sonarr.key");
    std::fs::write(&key, "0123456789abcdef0123456789abcdef").unwrap();
    let config = Config::from_toml(&format!(
        "public_url = \"http://tubarr.test:8480\"\n\
         [sonarr]\nurl = \"{}\"\napi_key_file = \"{}\"\n\
         [matcher]\nlookups_per_run = {lookups}\n{matcher}\n",
        fake_sonarr().await,
        key.display()
    ))
    .unwrap();
    let limiter = Arc::new(RateLimiter::new(RateLimitConfig {
        interval: Duration::from_millis(1),
        burst: 100,
        max_jitter: Duration::ZERO,
    }));
    let resolver = Arc::new(Resolver::new(
        YtDlp::new(fake_ytdlp(dir)),
        limiter,
        db.clone(),
    ));
    let matcher = ShowMatcher::new(
        db.clone(),
        resolver.clone(),
        ArrConnections::from_config(&config),
        config.matcher.clone(),
        Arc::new(Notify::new()),
    );
    (matcher, resolver)
}

async fn seen(db: &Db, id: &str, title: &str, published_at: i64, mins: Option<i64>) {
    db.upsert_seen_video(&SeenVideo {
        video_id: id.to_owned(),
        channel_id: ARY.to_owned(),
        title: title.to_owned(),
        published_at: Some(published_at),
        published_approx: false,
        duration_secs: mins.map(|m| m * 60),
        source: VideoSource::Rss,
    })
    .await
    .unwrap();
}

async fn released(s: &Setup) -> Vec<(i64, i64, String, i64)> {
    let mut out: Vec<_> =
        s.db.releases_for_mappings(&[s.mapping_id], &EpisodeFilter::Any, 100)
            .await
            .unwrap()
            .into_iter()
            .map(|r| match r.target {
                PinTarget::Episode { season, episode } => (season, episode, r.video_id, r.height),
                other => panic!("not an episode release: {other:?}"),
            })
            .collect();
    out.sort();
    out
}

async fn state(db: &Db, id: &str) -> MatchState {
    db.channel_video(id).await.unwrap().unwrap().match_state
}

#[tokio::test]
async fn a_run_releases_every_monitored_episode_and_fills_missing_metadata() {
    let s = setup(3).await;
    let title = |n: u32| format!("Kabhi Main Kabhi Tum Episode {n} | ARY Digital");
    seen(&s.db, "kmkt-ep0001", &title(1), AIRED + 16 * 3600, Some(38)).await;
    seen(
        &s.db,
        "kmkt-ep0002",
        &title(2),
        AIRED + DAY + 16 * 3600,
        Some(37),
    )
    .await;
    seen(&s.db, "kmkt-ep0003", &title(3), AIRED + 7 * DAY, Some(38)).await;
    seen(&s.db, "kmkt-ep0004", &title(4), AIRED + 8 * DAY, None).await;
    seen(
        &s.db,
        "kmkt-promo2",
        "Kabhi Main Kabhi Tum Episode 2 Promo",
        AIRED,
        Some(1),
    )
    .await;

    // Episode 1 was played once: its streams expired since, its height must still be known.
    s.resolver.resolve("kmkt-ep0001").await.unwrap();
    s.db.purge_resolved(i64::MAX).await.unwrap();
    assert!(
        s.db.cached_resolved("kmkt-ep0001", 0)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        s.db.known("kmkt-ep0001").await.unwrap().height,
        Some(720),
        "height lost with the cache"
    );

    let summary = s.matcher.run_once().await.unwrap();
    // E1 already has a file in Sonarr and keeps its release. E3 is not monitored. E2's duration
    // came from the channel list, so it goes out at the assumed 1080p without a lookup, and the
    // budget left over verifies it, which corrects the height.
    assert_eq!(
        released(&s).await,
        [
            (1, 1, "kmkt-ep0001".to_owned(), 720),
            (1, 2, "kmkt-ep0002".to_owned(), 720),
            (1, 4, "kmkt-ep0004".to_owned(), 720),
        ]
    );
    // E4 lacked its duration, E2 was released unverified: two lookups, E1 none.
    assert_eq!(
        calls(s.dir.path()),
        ["kmkt-ep0001", "kmkt-ep0004", "kmkt-ep0002"]
    );
    assert_eq!(summary.lookups, 2);
    assert_eq!(summary.report.released, 3);
    assert_eq!(summary.counts.released, 3);
    assert_eq!(summary.counts.pending, 0);
    assert!(s.db.unmatched(s.mapping_id).await.unwrap().is_empty());

    for id in ["kmkt-ep0001", "kmkt-ep0002", "kmkt-ep0004"] {
        assert_eq!(state(&s.db, id).await, MatchState::Matched, "{id}");
    }
    for id in ["kmkt-ep0003", "kmkt-promo2"] {
        assert_eq!(state(&s.db, id).await, MatchState::Ignored, "{id}");
    }

    let health = s.matcher.health();
    assert!(health.last_run_at.is_some());
    assert_eq!(health.last_error, None);
    assert_eq!(health.counts, summary.counts);
    assert_eq!(health.lookups, 2);

    // Again: nothing changes and nothing is looked up.
    let again = s.matcher.run_once().await.unwrap();
    assert_eq!(again.report.released, 0);
    assert_eq!(again.report.replaced, 0);
    assert_eq!(again.report.notes_changed, 0);
    assert_eq!(again.lookups, 0);
    assert_eq!(calls(s.dir.path()).len(), 3);
    assert_eq!(released(&s).await.len(), 3);
}

#[tokio::test]
async fn an_assumed_height_releases_at_once_and_the_real_one_corrects_it() {
    // No lookup budget at all: only the assumed height can release anything.
    let s = setup_with(0, "assumed_height = 480").await;
    let title = |n: u32| format!("Kabhi Main Kabhi Tum Episode {n} | ARY Digital");
    seen(
        &s.db,
        "kmkt-ep0002",
        &title(2),
        AIRED + DAY + 16 * 3600,
        Some(37),
    )
    .await;
    seen(&s.db, "kmkt-ep0004", &title(4), AIRED + 8 * DAY, None).await;

    let first = s.matcher.run_once().await.unwrap();
    assert_eq!(first.lookups, 0);
    assert!(calls(s.dir.path()).is_empty());
    assert_eq!(released(&s).await, [(1, 2, "kmkt-ep0002".to_owned(), 480)]);
    // E4's duration is still unknown: it waits for a lookup, height or not.
    let notes = s.db.unmatched(s.mapping_id).await.unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].video_id, "kmkt-ep0004");
    assert_eq!(notes[0].state, UnmatchedState::Pending);
    assert!(notes[0].reason.contains("duration"), "{}", notes[0].reason);
    let before =
        s.db.releases_for_mappings(&[s.mapping_id], &EpisodeFilter::Any, 100)
            .await
            .unwrap();

    // A grab or a play looks E2 up (720p): the next run names the release with the real height
    // and keeps the same row, so its guid does not change.
    s.resolver.resolve("kmkt-ep0002").await.unwrap();
    let again = s.matcher.run_once().await.unwrap();
    assert_eq!(again.report.released, 0);
    assert_eq!(again.report.replaced, 0);
    assert_eq!(released(&s).await, [(1, 2, "kmkt-ep0002".to_owned(), 720)]);
    let after =
        s.db.releases_for_mappings(&[s.mapping_id], &EpisodeFilter::Any, 100)
            .await
            .unwrap();
    assert_eq!(after[0].id, before[0].id);
    assert_eq!(before[0].size, None);
    assert!(after[0].size.is_some());
    assert_eq!(after[0].size, s.db.known("kmkt-ep0002").await.unwrap().size);
    let r = Release::from_stored(after[0].clone()).unwrap();
    assert_eq!(r.guid(), "tubarr-kmkt-ep0002-S01E02");
    assert!(r.title.contains(".720p."), "{}", r.title);
}

#[tokio::test]
async fn metadata_lookups_are_capped_oldest_first_and_wait_out_a_pause() {
    let s = setup(1).await;
    // E2 and E4 both wait on their duration. E4 was seen first.
    seen(
        &s.db,
        "kmkt-ep0002",
        "Kabhi Main Kabhi Tum Episode 2",
        AIRED + DAY,
        None,
    )
    .await;
    seen(
        &s.db,
        "kmkt-ep0004",
        "Kabhi Main Kabhi Tum Episode 4",
        AIRED + 3 * DAY,
        None,
    )
    .await;
    sqlx::query("UPDATE channel_videos SET first_seen = 100 WHERE video_id = 'kmkt-ep0004'")
        .execute(s.db.pool())
        .await
        .unwrap();

    let first = s.matcher.run_once().await.unwrap();
    assert_eq!(calls(s.dir.path()), ["kmkt-ep0004"]);
    assert_eq!(first.lookups, 1);
    assert_eq!(first.counts.released, 1);
    assert_eq!(first.counts.pending, 1);
    let notes = s.db.unmatched(s.mapping_id).await.unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].video_id, "kmkt-ep0002");
    assert_eq!(notes[0].state, UnmatchedState::Pending);
    // Still being looked at, so not ignored.
    assert_eq!(state(&s.db, "kmkt-ep0002").await, MatchState::New);
    assert_eq!(state(&s.db, "kmkt-ep0004").await, MatchState::Matched);

    // YouTube pushed back: no lookups until the pause is over.
    s.resolver.limiter().penalize(Duration::from_secs(3600));
    let paused = s.matcher.run_once().await.unwrap();
    assert_eq!(paused.lookups, 0);
    assert_eq!(calls(s.dir.path()).len(), 1);
    assert_eq!(paused.counts.pending, 1);
}

#[tokio::test]
async fn a_failed_lookup_does_not_hold_the_budget() {
    let s = setup(1).await;
    // Not a valid video id: the resolver refuses it without running yt-dlp.
    seen(
        &s.db,
        "kmkt-bad",
        "Kabhi Main Kabhi Tum Episode 2",
        AIRED + DAY,
        None,
    )
    .await;
    seen(
        &s.db,
        "kmkt-ep0004",
        "Kabhi Main Kabhi Tum Episode 4",
        AIRED + 3 * DAY,
        None,
    )
    .await;
    sqlx::query("UPDATE channel_videos SET first_seen = 100 WHERE video_id = 'kmkt-bad'")
        .execute(s.db.pool())
        .await
        .unwrap();

    let first = s.matcher.run_once().await.unwrap();
    assert_eq!(first.lookups, 1);
    assert!(calls(s.dir.path()).is_empty());
    // The failed one waits for its retry time, so the next run gets to the other video.
    let second = s.matcher.run_once().await.unwrap();
    assert_eq!(second.lookups, 1);
    assert_eq!(calls(s.dir.path()), ["kmkt-ep0004"]);
    assert_eq!(second.counts.released, 1);
}

#[tokio::test]
async fn without_sonarr_nothing_is_matched_or_looked_up() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    let config = Config::from_toml("public_url = \"http://tubarr.test:8480\"").unwrap();
    let resolver = Arc::new(Resolver::new(
        YtDlp::new(fake_ytdlp(dir.path())),
        Arc::new(RateLimiter::new(RateLimitConfig::default())),
        db.clone(),
    ));
    let matcher = ShowMatcher::new(
        db,
        resolver,
        ArrConnections::new(None, None),
        config.matcher.clone(),
        Arc::new(Notify::new()),
    );
    let summary = matcher.run_once().await.unwrap();
    assert_eq!(summary.report.released, 0);
    assert_eq!(summary.lookups, 0);
    assert!(calls(dir.path()).is_empty());
}

#[tokio::test]
async fn an_arr_sync_wakes_the_matcher() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    let key = dir.path().join("sonarr.key");
    std::fs::write(&key, "0123456789abcdef0123456789abcdef").unwrap();
    let config = Config::from_toml(&format!(
        "public_url = \"http://tubarr.test:8480\"\n[sonarr]\nurl = \"{}\"\napi_key_file = \"{}\"\n",
        fake_sonarr().await,
        key.display()
    ))
    .unwrap();
    let wake = Arc::new(Notify::new());
    let sync = tubarr::arr::ArrSync::from_config(&config, db)
        .unwrap()
        .with_synced(wake.clone());
    // The fake has no tag list, so the sync fails. The matcher is woken all the same.
    sync.sync_once().await;
    tokio::time::timeout(Duration::from_secs(1), wake.notified())
        .await
        .expect("the sync did not wake the matcher");
}

#[tokio::test]
async fn candidates_say_when_the_upload_time_is_a_guess() {
    use tubarr::matcher::Candidates;
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    let base = SeenVideo {
        video_id: "exact".to_owned(),
        channel_id: ARY.to_owned(),
        title: "Show".to_owned(),
        published_at: Some(AIRED),
        published_approx: false,
        duration_secs: None,
        source: VideoSource::Rss,
    };
    let guessed = SeenVideo {
        video_id: "guessed".to_owned(),
        published_approx: true,
        source: VideoSource::Listing,
        ..base.clone()
    };
    let undated = SeenVideo {
        video_id: "undated".to_owned(),
        published_at: None,
        source: VideoSource::Listing,
        ..base.clone()
    };
    for v in [&base, &guessed, &undated] {
        db.upsert_seen_video(v).await.unwrap();
    }
    let approx: Vec<(String, bool)> = db
        .on_channels(&[ARY.to_owned()])
        .await
        .unwrap()
        .into_iter()
        .map(|c| (c.video_id, c.published_approx))
        .collect();
    assert_eq!(
        approx,
        [
            ("exact".to_owned(), false),
            ("guessed".to_owned(), true),
            ("undated".to_owned(), true)
        ]
    );
}

/// Makes the fake answer with WebM Opus as the only audio for `id`, like an upload Tubarr cannot
/// serve through its virtual file.
fn make_unplayable(dir: &Path, id: &str) {
    answer_with(
        dir,
        id,
        r#"{"id": "X", "title": "T", "duration": 2280, "formats": [
          {"format_id": "251", "vcodec": "none", "acodec": "opus", "protocol": "https",
           "url": "https://rr3---sn-a.googlevideo.com/videoplayback?itag=251"},
          {"format_id": "137", "vcodec": "avc1.640028", "acodec": "none", "protocol": "https",
           "height": 1080, "url": "https://rr3---sn-a.googlevideo.com/videoplayback?itag=137"}]}"#,
    );
}

/// Makes the fake answer `json` for `id` instead of `INFO`.
fn answer_with(dir: &Path, id: &str, json: &str) {
    let path = dir.join("yt-dlp");
    let script = std::fs::read_to_string(&path).unwrap();
    let answer = dir.join(format!("{id}.json"));
    std::fs::write(&answer, json).unwrap();
    let script = script.replacen(
        "sed ",
        &format!(
            "[ \"$id\" = '{id}' ] && exec cat '{}'\nsed ",
            answer.display()
        ),
        1,
    );
    support::install_executable(&path, script.as_bytes());
}

#[tokio::test]
async fn an_unverified_release_found_unplayable_leaves_the_feed_and_is_not_released_again() {
    let s = setup(1).await;
    make_unplayable(s.dir.path(), "kmkt-ep0002");
    let title = |n: u32| format!("Kabhi Main Kabhi Tum Episode {n} | ARY Digital");
    seen(
        &s.db,
        "kmkt-ep0002",
        &title(2),
        AIRED + DAY + 16 * 3600,
        Some(37),
    )
    .await;

    let summary = s.matcher.run_once().await.unwrap();
    // Released at the assumed height, then the verification lookup found it unplayable and the
    // rerun took the release back out before any arr saw it.
    assert_eq!(summary.lookups, 1);
    assert!(released(&s).await.is_empty());
    let marked = s.db.unplayable_videos().await.unwrap();
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0].video_id, "kmkt-ep0002");
    assert!(marked[0].reason.contains("AAC"), "{}", marked[0].reason);
    assert!(s.db.release_checks().await.unwrap().is_empty());

    // Later runs leave it out without asking YouTube again.
    let again = s.matcher.run_once().await.unwrap();
    assert_eq!(again.lookups, 0);
    assert!(released(&s).await.is_empty());
    assert_eq!(calls(s.dir.path()), ["kmkt-ep0002"]);

    // Clearing the mark lets the matcher release it again.
    assert!(s.db.clear_unplayable("kmkt-ep0002").await.unwrap());
    s.matcher.run_once().await.unwrap();
    assert_eq!(released(&s).await.len(), 1);
    let checks = s.db.release_checks().await.unwrap();
    assert_eq!(checks.len(), 1);
    assert_eq!(checks[0].status, "unresolved");
}

#[tokio::test]
async fn a_video_without_h264_yet_is_looked_up_again_and_released_at_the_height_served() {
    let s = setup(1).await;
    // A brand-new upload: VP9 and AV1 at 1080p, no H.264 video at all.
    answer_with(
        s.dir.path(),
        "kmkt-ep0002",
        r#"{"id": "kmkt-ep0002", "title": "T", "duration": 2280, "formats": [
          {"format_id": "140", "vcodec": "none", "acodec": "mp4a.40.2", "protocol": "https",
           "url": "https://rr3---sn-a.googlevideo.com/videoplayback?itag=140"},
          {"format_id": "248", "vcodec": "vp09.00.40.08", "acodec": "none", "protocol": "https",
           "width": 1920, "height": 1080, "url": "https://rr3---sn-a.googlevideo.com/videoplayback?itag=248"},
          {"format_id": "399", "vcodec": "av01.0.08M.08", "acodec": "none", "protocol": "https",
           "width": 1920, "height": 1080, "url": "https://rr3---sn-a.googlevideo.com/videoplayback?itag=399"}]}"#,
    );
    seen(
        &s.db,
        "kmkt-ep0002",
        "Kabhi Main Kabhi Tum Episode 2 | ARY Digital",
        AIRED + DAY + 16 * 3600,
        Some(37),
    )
    .await;
    s.matcher.run_once().await.unwrap();
    assert!(released(&s).await.is_empty());
    let marked = s.db.unplayable_videos().await.unwrap();
    assert!(marked[0].reason.contains("H.264"), "{}", marked[0].reason);
    assert!(marked[0].retry);
    // A fresh mark is not looked up again.
    assert_eq!(s.matcher.run_once().await.unwrap().lookups, 0);

    // Hours later, after a restart (which rewrites the fake), YouTube offers H.264 at 720p only.
    let (matcher, _) = matcher_for(&s.db, s.dir.path(), 1, "").await;
    let expire = SystemTime::now() + Duration::from_secs(6 * 3600);
    let expire = expire.duration_since(UNIX_EPOCH).unwrap().as_secs();
    answer_with(
        s.dir.path(),
        "kmkt-ep0002",
        &format!(
            r#"{{"id": "kmkt-ep0002", "title": "T", "duration": 2280, "upload_date": "20240703", "formats": [
              {{"format_id": "140", "vcodec": "none", "acodec": "mp4a.40.2", "protocol": "https",
               "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire={expire}&itag=140"}},
              {{"format_id": "248", "vcodec": "vp09.00.40.08", "acodec": "none", "protocol": "https",
               "width": 1920, "height": 1080, "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire={expire}&itag=248"}},
              {{"format_id": "136", "vcodec": "avc1.4d401f", "acodec": "none", "protocol": "https",
               "width": 1280, "height": 720, "url": "https://rr3---sn-a.googlevideo.com/videoplayback?expire={expire}&itag=136"}}]}}"#
        ),
    );
    sqlx::query("UPDATE unplayable_videos SET checked_at = checked_at - 7 * 3600")
        .execute(s.db.pool())
        .await
        .unwrap();
    assert_eq!(matcher.run_once().await.unwrap().lookups, 1);
    assert!(s.db.unplayable_videos().await.unwrap().is_empty());
    let releases = released(&s).await;
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].2, "kmkt-ep0002");
    assert_eq!(releases[0].3, 720);
}

/// Makes the fake fail for `id` the way yt-dlp does for a video only channel members may watch.
fn make_members_only(dir: &Path, id: &str) {
    let path = dir.join("yt-dlp");
    let script = std::fs::read_to_string(&path).unwrap();
    let script = script.replacen(
        "sed ",
        &format!(
            "[ \"$id\" = '{id}' ] && {{ echo 'ERROR: [youtube] {id}: Join this channel to get \
             access to members-only content like this video, and other exclusive perks.' >&2; \
             exit 1; }}\nsed "
        ),
        1,
    );
    support::install_executable(&path, script.as_bytes());
}

/// An early-access episode the feed showed while it was for members only, marked by its lookup.
async fn members_only_episode(s: &Setup) {
    make_members_only(s.dir.path(), "kmkt-ep0002");
    seen(
        &s.db,
        "kmkt-ep0002",
        "Kabhi Main Kabhi Tum Episode 2 | ARY Digital",
        AIRED + DAY + 16 * 3600,
        Some(37),
    )
    .await;
    s.matcher.run_once().await.unwrap();
    assert!(released(s).await.is_empty());
    let marked = s.db.unplayable_videos().await.unwrap();
    assert!(
        marked[0].reason.contains("members-only"),
        "{}",
        marked[0].reason
    );
    assert!(marked[0].retry);
}

/// Moves every unplayable mark's last check `hours` back.
async fn age_marks(db: &Db, hours: i64) {
    sqlx::query("UPDATE unplayable_videos SET checked_at = checked_at - ?")
        .bind(hours * 3600)
        .execute(db.pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn a_members_only_episode_a_channel_list_shows_open_is_released() {
    let s = setup(1).await;
    members_only_episode(&s).await;
    // Hours later, after a restart, it is not looked up again: members-only marks wait a day.
    age_marks(&s.db, 7).await;
    let (matcher, _) = matcher_for(&s.db, s.dir.path(), 1, "").await;
    assert_eq!(matcher.run_once().await.unwrap().lookups, 0);

    // The channel opens it to everyone and the next channel list shows it so.
    s.db.record_sighting(
        &SeenVideo {
            video_id: "kmkt-ep0002".to_owned(),
            channel_id: ARY.to_owned(),
            title: "Kabhi Main Kabhi Tum Episode 2 | ARY Digital".to_owned(),
            published_at: Some(AIRED + DAY + 16 * 3600),
            published_approx: false,
            duration_secs: Some(37 * 60),
            source: VideoSource::Listing,
        },
        Availability::Open,
    )
    .await
    .unwrap();
    assert!(s.db.unplayable_videos().await.unwrap().is_empty());
    matcher.run_once().await.unwrap();
    let releases = released(&s).await;
    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].2, "kmkt-ep0002");
}

#[tokio::test]
async fn a_members_only_episode_is_looked_up_again_daily_for_two_weeks() {
    let s = setup(1).await;
    members_only_episode(&s).await;
    // A day on, after a restart that also brings the episode out for everyone.
    age_marks(&s.db, 25).await;
    let (matcher, _) = matcher_for(&s.db, s.dir.path(), 1, "").await;
    // First seen over two weeks ago: no more daily lookups, only a channel list clears it.
    sqlx::query("UPDATE channel_videos SET first_seen = first_seen - 15 * 86400")
        .execute(s.db.pool())
        .await
        .unwrap();
    assert_eq!(matcher.run_once().await.unwrap().lookups, 0);
    sqlx::query("UPDATE channel_videos SET first_seen = first_seen + 15 * 86400")
        .execute(s.db.pool())
        .await
        .unwrap();
    assert_eq!(matcher.run_once().await.unwrap().lookups, 1);
    assert!(s.db.unplayable_videos().await.unwrap().is_empty());
    assert_eq!(released(&s).await.len(), 1);
}

#[tokio::test]
async fn verification_waits_out_a_limiter_pause() {
    let s = setup(3).await;
    seen(
        &s.db,
        "kmkt-ep0002",
        "Kabhi Main Kabhi Tum Episode 2 | ARY Digital",
        AIRED + DAY + 16 * 3600,
        Some(37),
    )
    .await;
    s.resolver.limiter().penalize(Duration::from_secs(600));
    let summary = s.matcher.run_once().await.unwrap();
    assert_eq!(summary.lookups, 0);
    assert!(calls(s.dir.path()).is_empty());
    // Released unverified, listed for the lead.
    assert_eq!(released(&s).await.len(), 1);
    assert_eq!(s.db.release_checks().await.unwrap()[0].status, "unresolved");
}

/// The podcast's episode `n` as a video id.
fn pod(n: i64) -> String {
    format!("kmkt-ep{n:04}")
}

/// Episodes 1 to 10 uploaded a day apart before the mapping was added, 11 and 12 after it.
/// Episodes 1, 2 and 9 lack their duration, so each needs a lookup before it can go out.
async fn podcast(s: &Setup) {
    let added =
        s.db.mapping(s.mapping_id)
            .await
            .unwrap()
            .unwrap()
            .created_at;
    for n in 1..=PODCAST_EPISODES {
        let at = if n <= 10 {
            added - (11 - n) * DAY
        } else {
            added + (n - 10) * 3600
        };
        let mins = (![1, 2, 9].contains(&n)).then_some(60);
        let title = format!("Kabhi Main Kabhi Tum Episode {n}");
        seen(&s.db, &pod(n), &title, at, mins).await;
    }
}

async fn released_episodes(s: &Setup) -> Vec<i64> {
    released(s).await.into_iter().map(|r| r.1).collect()
}

#[tokio::test]
async fn a_backfill_limit_releases_the_newest_back_episodes_and_every_new_one() {
    let s = setup_series(12, 20, "assumed_height = 720").await;
    podcast(&s).await;
    s.db.set_backfill_limit(s.mapping_id, Some(5))
        .await
        .unwrap();
    s.db.sync_backfills(&s.db.backfill_jobs().await.unwrap(), 0)
        .await
        .unwrap();

    s.matcher.run_once().await.unwrap();
    // The five newest back episodes (6 to 10, 9 after its lookup) and both new ones.
    assert_eq!(released_episodes(&s).await, [6, 7, 8, 9, 10, 11, 12]);
    let looked = calls(s.dir.path());
    assert!(looked.contains(&pod(9)), "{looked:?}");
    for n in 1..=5 {
        assert!(!looked.contains(&pod(n)), "{} was looked up", pod(n));
    }
    // The rest stay visible with the reason, and count as ignored on the channel.
    let notes = s.db.unmatched(s.mapping_id).await.unwrap();
    let capped: Vec<&str> = notes.iter().map(|n| n.video_id.as_str()).collect();
    assert_eq!(capped, (1..=5).map(pod).collect::<Vec<_>>());
    for note in &notes {
        assert_eq!(note.state, UnmatchedState::Capped);
        assert!(
            note.reason.contains("newest 5 back episodes"),
            "{}",
            note.reason
        );
        assert_eq!(state(&s.db, &note.video_id).await, MatchState::Ignored);
    }
    // Every kept back episode is out: the channel search has nothing left to find.
    assert!(s.db.backfill_rows().await.unwrap()[0].done);

    let lookups = looked.len();
    let again = s.matcher.run_once().await.unwrap();
    assert_eq!(again.report.released, 0);
    assert_eq!(again.report.notes_changed, 0);
    assert_eq!(calls(s.dir.path()).len(), lookups);

    // Without the limit everything goes out as before, the capped ones after their lookups.
    s.db.set_backfill_limit(s.mapping_id, None).await.unwrap();
    s.matcher.run_once().await.unwrap();
    assert_eq!(
        released_episodes(&s).await,
        (1..=PODCAST_EPISODES).collect::<Vec<_>>()
    );
    assert!(s.db.unmatched(s.mapping_id).await.unwrap().is_empty());
}

#[tokio::test]
async fn without_a_limit_every_back_episode_goes_out_and_a_new_limit_takes_the_older_back() {
    let s = setup_series(12, 20, "assumed_height = 720").await;
    podcast(&s).await;
    s.matcher.run_once().await.unwrap();
    assert_eq!(
        released_episodes(&s).await,
        (1..=PODCAST_EPISODES).collect::<Vec<_>>()
    );

    s.db.set_backfill_limit(s.mapping_id, Some(5))
        .await
        .unwrap();
    let summary = s.matcher.run_once().await.unwrap();
    assert_eq!(summary.report.replaced, 5);
    assert_eq!(released_episodes(&s).await, [6, 7, 8, 9, 10, 11, 12]);
    let notes = s.db.unmatched(s.mapping_id).await.unwrap();
    assert_eq!(notes.len(), 5);
    assert!(notes.iter().all(|n| n.state == UnmatchedState::Capped));
}

/// The show's mapping with `excludes`, as the admin UI saves it.
async fn set_excludes(s: &Setup, excludes: &[&str]) {
    let mapping = s.db.mapping(s.mapping_id).await.unwrap().unwrap();
    s.db.update_mapping(
        s.mapping_id,
        &NewMapping {
            kind: MappingKind::Show,
            tvdb_id: mapping.tvdb_id,
            tmdb_id: None,
            title: mapping.title.clone(),
            language: mapping.language.clone(),
            channel_ids: mapping.channel_ids.clone(),
            episode_regex: None,
            excludes: excludes.iter().map(|e| (*e).to_owned()).collect(),
            min_duration_secs: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn a_rerun_over_unchanged_data_evaluates_no_mapping_and_a_change_only_its_own() {
    // The fixture's uploads are from 2024: keep their notes.
    let s = setup_with(0, "note_retention = \"0s\"").await;
    // A second show from Sonarr on another channel, with nothing uploaded.
    let other =
        s.db.insert_mapping(&NewMapping {
            kind: MappingKind::Show,
            tvdb_id: Some(452_002),
            tmdb_id: None,
            title: "Another Show".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec!["UCanotherchannelxxxxxxxx".to_owned()],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        })
        .await
        .unwrap()
        .id;
    sqlx::query("UPDATE mappings SET arr_id = 12 WHERE id = ?")
        .bind(other)
        .execute(s.db.pool())
        .await
        .unwrap();
    let title = "Kabhi Main Kabhi Tum Episode 1 | ARY Digital";
    seen(&s.db, "kmkt-ep0001", title, AIRED + 3600, Some(38)).await;
    seen(&s.db, "kmkt-ep001b", title, AIRED + 7200, Some(38)).await;
    let special = "Kabhi Main Kabhi Tum Special";
    seen(&s.db, "kmkt-specl", special, AIRED + 3600, Some(38)).await;
    let notes = |s: &Setup| {
        let db = s.db.clone();
        let id = s.mapping_id;
        async move {
            let mut notes: Vec<(String, UnmatchedState)> = db
                .unmatched(id)
                .await
                .unwrap()
                .into_iter()
                .map(|n| (n.video_id, n.state))
                .collect();
            notes.sort_by(|a, b| a.0.cmp(&b.0));
            notes
        }
    };

    let first = s.matcher.run_once().await.unwrap();
    assert_eq!((first.report.matched, first.report.unchanged), (2, 0));
    assert_eq!(
        notes(&s).await,
        [
            ("kmkt-ep001b".to_owned(), UnmatchedState::Duplicate),
            ("kmkt-specl".to_owned(), UnmatchedState::Unsure),
        ]
    );
    let released_first = released(&s).await;
    assert_eq!(released_first.len(), 1);

    // Nothing changed: no mapping is matched, so no note is looked at again.
    let again = s.matcher.run_once().await.unwrap();
    assert_eq!((again.report.matched, again.report.unchanged), (0, 2));
    assert_eq!(again.report.notes_changed, 0);
    // The watcher seeing the same videos again only moves columns the matcher does not read.
    seen(&s.db, "kmkt-ep0001", title, AIRED + 3600, Some(38)).await;
    s.db.refresh_match_states(&[ARY.to_owned()]).await.unwrap();
    let seen_again = s.matcher.run_once().await.unwrap();
    assert_eq!(seen_again.report.matched, 0);
    // So does hiding a note.
    let special_note = s.db.unmatched(s.mapping_id).await.unwrap()[1].id;
    s.db.set_unmatched_ignored(special_note, true)
        .await
        .unwrap();
    assert_eq!(s.matcher.run_once().await.unwrap().report.matched, 0);

    // A rules change re-matches that mapping only.
    set_excludes(&s, &["Special"]).await;
    let rules = s.matcher.run_once().await.unwrap();
    assert_eq!((rules.report.matched, rules.report.unchanged), (1, 1));
    assert_eq!(
        notes(&s).await,
        [("kmkt-ep001b".to_owned(), UnmatchedState::Duplicate)]
    );
    assert_eq!(s.matcher.run_once().await.unwrap().report.matched, 0);

    // A pin re-matches it, and so does taking the pin away.
    let pin =
        s.db.set_pin(
            s.mapping_id,
            &PinTarget::Episode {
                season: 1,
                episode: 1,
            },
            "kmkt-ep001b",
        )
        .await
        .unwrap();
    let pinned = s.matcher.run_once().await.unwrap();
    assert_eq!((pinned.report.matched, pinned.report.unchanged), (1, 1));
    assert_eq!(released(&s).await[0].2, "kmkt-ep001b");
    assert_eq!(s.matcher.run_once().await.unwrap().report.matched, 0);
    assert!(s.db.delete_pin(pin.id).await.unwrap());
    let unpinned = s.matcher.run_once().await.unwrap();
    assert_eq!((unpinned.report.matched, unpinned.report.unchanged), (1, 1));
    assert_eq!(released(&s).await, released_first);

    // A new upload re-matches its mapping.
    let next = "Kabhi Main Kabhi Tum Episode 2 | ARY Digital";
    seen(&s.db, "kmkt-ep0002", next, AIRED + DAY + 3600, Some(37)).await;
    let upload = s.matcher.run_once().await.unwrap();
    assert_eq!((upload.report.matched, upload.report.unchanged), (1, 1));
    assert_eq!(released(&s).await.len(), 2);
    assert_eq!(s.matcher.run_once().await.unwrap().report.matched, 0);
}

#[tokio::test]
async fn a_restarted_matcher_keeps_the_digests() {
    let s = setup(0).await;
    let title = "Kabhi Main Kabhi Tum Episode 1 | ARY Digital";
    seen(&s.db, "kmkt-ep0001", title, AIRED + 3600, Some(38)).await;
    seen(&s.db, "kmkt-ep001b", title, AIRED + 7200, Some(38)).await;
    assert_eq!(s.matcher.run_once().await.unwrap().report.matched, 1);
    // The default retention keeps no note for an upload from 2024, the release stays.
    assert!(s.db.unmatched(s.mapping_id).await.unwrap().is_empty());
    assert_eq!(released(&s).await.len(), 1);
    let (restarted, _) = matcher_for(&s.db, s.dir.path(), 0, "").await;
    let after = restarted.run_once().await.unwrap();
    assert_eq!((after.report.matched, after.report.unchanged), (0, 1));
}

#[tokio::test]
async fn pruning_unused_channel_videos_re_matches_the_mapping_once() {
    let s = setup(0).await;
    let title = "Kabhi Main Kabhi Tum Episode 1 | ARY Digital";
    seen(&s.db, "kmkt-ep0001", title, AIRED + 3600, Some(38)).await;
    seen(&s.db, "kmkt-ep001b", title, AIRED + 7200, Some(38)).await;
    let special = "Kabhi Main Kabhi Tum Special";
    seen(&s.db, "kmkt-specl", special, AIRED + 3600, Some(38)).await;
    assert_eq!(s.matcher.run_once().await.unwrap().report.matched, 1);
    let released_first = released(&s).await;
    assert_eq!(s.matcher.run_once().await.unwrap().report.matched, 0);

    // The 2024 uploads have not been seen for a year either: the two without a release go.
    sqlx::query("UPDATE channel_videos SET last_seen = last_seen - 365 * 86400")
        .execute(s.db.pool())
        .await
        .unwrap();
    let year = std::time::Duration::from_secs(180 * 86_400);
    assert_eq!(s.db.prune_channel_videos(year).await.unwrap(), 2);
    assert!(s.db.channel_video("kmkt-ep0001").await.unwrap().is_some());

    let after = s.matcher.run_once().await.unwrap();
    assert_eq!((after.report.matched, after.report.unchanged), (1, 0));
    assert_eq!(released(&s).await, released_first);
    let health = s.matcher.health();
    assert_eq!((health.matched, health.unchanged), (1, 0));
    let settled = s.matcher.run_once().await.unwrap();
    assert_eq!((settled.report.matched, settled.report.unchanged), (0, 1));
    assert_eq!(s.matcher.health().unchanged, 1);
}
