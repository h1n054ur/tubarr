//! The watcher against a local fake feed server and a fake yt-dlp. Nothing here reaches YouTube.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

mod support;

use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use tubarr::{
    config,
    ratelimit::{RateLimitConfig, RateLimiter},
    resolver::{ResolveError, YtDlp},
    store::{Db, MappingKind, MatchState, NewMapping, VideoSource},
    watcher::{FeedPoll, ListingRun, WatchError, Watcher},
};

const CHANNEL: &str = "UC4JCksJF76g_MdzPVBJoC3Q";
const HOUR: Duration = Duration::from_secs(3600);

/// What the fake feed answers.
#[derive(Default)]
struct Feed {
    /// (status, body, ETag, Last-Modified)
    answer: Option<(u16, String, Option<String>, Option<String>)>,
    requests: Vec<(Option<String>, Option<String>, Option<String>)>,
    not_modified: usize,
}

type Shared = Arc<Mutex<Feed>>;

async fn feed(
    State(feed): State<Shared>,
    Query(query): Query<Vec<(String, String)>>,
    headers: HeaderMap,
) -> Response {
    let mut feed = feed.lock().unwrap();
    let get = |name| {
        headers
            .get(name)
            .and_then(|v: &header::HeaderValue| v.to_str().ok())
            .map(str::to_owned)
    };
    let channel = query
        .iter()
        .find(|(k, _)| k == "channel_id")
        .map(|(_, v)| v.clone());
    let (inm, ims) = (get(header::IF_NONE_MATCH), get(header::IF_MODIFIED_SINCE));
    feed.requests.push((channel, inm.clone(), ims.clone()));
    let Some((status, body, etag, modified)) = feed.answer.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let fresh_etag = etag.is_some() && inm == etag;
    let fresh_date = etag.is_none() && modified.is_some() && ims == modified;
    if status == 200 && (fresh_etag || fresh_date) {
        feed.not_modified += 1;
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let mut response = (StatusCode::from_u16(status).unwrap(), body).into_response();
    if let Some(etag) = etag {
        response
            .headers_mut()
            .insert(header::ETAG, etag.parse().unwrap());
    }
    if let Some(modified) = modified {
        response
            .headers_mut()
            .insert(header::LAST_MODIFIED, modified.parse().unwrap());
    }
    response
}

async fn start_feed() -> (Shared, String) {
    let shared: Shared = Arc::default();
    let app = Router::new()
        .route("/feeds/videos.xml", get(feed))
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (shared, format!("http://{addr}/feeds/videos.xml"))
}

fn answer(feed: &Shared, status: u16, fixture: &str, etag: Option<&str>, modified: Option<&str>) {
    feed.lock().unwrap().answer = Some((
        status,
        support::fixture_string(fixture),
        etag.map(str::to_owned),
        modified.map(str::to_owned),
    ));
}

struct Setup {
    watcher: Arc<Watcher>,
    db: Db,
    limiter: Arc<RateLimiter>,
    _dir: tempfile::TempDir,
}

/// A watcher over a fresh database. The limiter refills one token an hour, so a test sees exactly
/// how many tokens it used.
async fn setup(feed_url: &str, ytdlp: &std::path::Path) -> Setup {
    let dir = support::temp_dir();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    let limiter = Arc::new(RateLimiter::new(RateLimitConfig {
        interval: HOUR,
        burst: 3,
        max_jitter: Duration::ZERO,
    }));
    let settings = config::Watcher {
        feed_url: feed_url.parse().unwrap(),
        rss_spacing: Duration::from_secs(1),
        ..config::Watcher::default()
    };
    let watcher = Arc::new(
        Watcher::new(
            settings,
            db.clone(),
            limiter.clone(),
            YtDlp::new(ytdlp),
            Duration::from_secs(900),
            None,
        )
        .unwrap(),
    );
    Setup {
        watcher,
        db,
        limiter,
        _dir: dir,
    }
}

fn mapping() -> NewMapping {
    NewMapping {
        kind: MappingKind::Show,
        tvdb_id: Some(452_001),
        tmdb_id: None,
        title: "Kabhi Main Kabhi Tum".to_owned(),
        language: "Urdu".to_owned(),
        channel_ids: vec![CHANNEL.to_owned()],
        episode_regex: None,
        excludes: vec![],
        min_duration_secs: None,
    }
}

#[tokio::test]
async fn a_new_upload_in_the_feed_is_picked_up() {
    let (feed, url) = start_feed().await;
    let s = setup(&url, "/nonexistent/yt-dlp".as_ref()).await;
    answer(&feed, 200, "youtube/feed-channel.xml", Some("\"v1\""), None);

    assert_eq!(
        s.watcher.poll_feed(CHANNEL).await.unwrap(),
        FeedPoll::Read { seen: 3, new: 3 }
    );
    // Unchanged: the ETag comes back and the answer is a 304.
    assert_eq!(
        s.watcher.poll_feed(CHANNEL).await.unwrap(),
        FeedPoll::NotModified
    );
    answer(
        &feed,
        200,
        "youtube/feed-channel-new-upload.xml",
        Some("\"v2\""),
        None,
    );
    assert_eq!(
        s.watcher.poll_feed(CHANNEL).await.unwrap(),
        FeedPoll::Read { seen: 4, new: 1 }
    );

    {
        let feed = feed.lock().unwrap();
        assert_eq!(feed.not_modified, 1);
        let inm: Vec<_> = feed.requests.iter().map(|r| r.1.as_deref()).collect();
        assert_eq!(inm, [None, Some("\"v1\""), Some("\"v1\"")]);
        assert!(
            feed.requests
                .iter()
                .all(|r| r.0.as_deref() == Some(CHANNEL))
        );
    }

    let video = s.db.channel_video("Zq8uV4hNs1M").await.unwrap().unwrap();
    assert_eq!(video.channel_id, CHANNEL);
    assert!(video.title.starts_with("Kabhi Main Kabhi Tum Episode 4"));
    // 2024-07-16T15:00:04+00:00
    assert_eq!(video.published_at, Some(1_721_142_004));
    assert!(!video.published_approx);
    assert_eq!(video.source, VideoSource::Rss);
    assert_eq!(video.match_state, MatchState::New);
    assert_eq!(s.db.unmatched_videos(0).await.unwrap().len(), 4);
    // Feeds are not lookups.
    assert_eq!(s.limiter.status().available, 3.0);

    let health = s.watcher.health();
    let ch = &health.channels[0];
    assert_eq!(ch.channel_id, CHANNEL);
    assert_eq!((ch.videos_seen, ch.videos_new), (7, 4));
    assert!(ch.last_feed_at.is_some());
    assert_eq!(ch.last_feed_error, None);
}

#[tokio::test]
async fn last_modified_is_sent_back_when_there_is_no_etag() {
    let (feed, url) = start_feed().await;
    let s = setup(&url, "/nonexistent/yt-dlp".as_ref()).await;
    let date = "Thu, 15 Oct 2026 08:12:01 GMT";
    answer(&feed, 200, "youtube/feed-channel.xml", None, Some(date));
    s.watcher.poll_feed(CHANNEL).await.unwrap();
    assert_eq!(
        s.watcher.poll_feed(CHANNEL).await.unwrap(),
        FeedPoll::NotModified
    );
    assert_eq!(feed.lock().unwrap().requests[1].2.as_deref(), Some(date));
}

#[tokio::test]
async fn server_errors_count_as_failures_and_keep_the_validators() {
    let (feed, url) = start_feed().await;
    let s = setup(&url, "/nonexistent/yt-dlp".as_ref()).await;
    answer(&feed, 200, "youtube/feed-channel.xml", Some("\"v1\""), None);
    s.watcher.poll_feed(CHANNEL).await.unwrap();

    // The Google 500 page recorded on 2026-10-16.
    answer(&feed, 500, "youtube/feed-server-error.html", None, None);
    let err = s.watcher.poll_feed(CHANNEL).await.unwrap_err();
    assert!(matches!(err, WatchError::Status(500)), "{err}");
    s.watcher.poll_feed(CHANNEL).await.unwrap_err();
    let health = s.watcher.health();
    assert_eq!(health.channels[0].feed_failures, 2);
    assert!(
        health.channels[0]
            .last_feed_error
            .as_deref()
            .unwrap()
            .contains("500")
    );
    // Backoff for two failures in a row is four times the interval.
    assert_eq!(
        tubarr::watcher::schedule::feed_delay(30 * Duration::from_secs(60), 2, Duration::ZERO),
        2 * HOUR
    );

    // A 200 that is an HTML page is a failure too, and nothing is stored from it.
    answer(&feed, 200, "youtube/feed-server-error.html", None, None);
    let err = s.watcher.poll_feed(CHANNEL).await.unwrap_err();
    assert!(matches!(err, WatchError::Feed(_)), "{err}");
    assert_eq!(s.watcher.health().channels[0].feed_failures, 3);

    // A 404 for a feed that exists is the same kind of passing failure.
    feed.lock().unwrap().answer = None;
    let err = s.watcher.poll_feed(CHANNEL).await.unwrap_err();
    assert!(matches!(err, WatchError::Status(404)), "{err}");

    // Recovery: the old ETag is still sent, and a success clears the failures.
    answer(&feed, 200, "youtube/feed-channel.xml", Some("\"v1\""), None);
    assert_eq!(
        s.watcher.poll_feed(CHANNEL).await.unwrap(),
        FeedPoll::NotModified
    );
    let health = s.watcher.health();
    assert_eq!(health.channels[0].feed_failures, 0);
    assert_eq!(health.channels[0].last_feed_error, None);
    assert_eq!(s.db.unmatched_videos(0).await.unwrap().len(), 3);
}

#[tokio::test]
async fn a_channel_listing_goes_through_the_limiter() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "flat_listing");
    let s = setup("http://localhost:9/unused", &fake.path).await;

    assert_eq!(
        s.watcher.list_channel(CHANNEL).await.unwrap(),
        ListingRun { seen: 5, new: 5 }
    );
    assert!((s.limiter.status().available - 2.0).abs() < 0.01);
    assert_eq!(
        s.watcher.list_channel(CHANNEL).await.unwrap(),
        ListingRun { seen: 5, new: 0 }
    );
    assert!((s.limiter.status().available - 1.0).abs() < 0.01);

    let args = fake.last_args();
    assert_eq!(
        args.last().unwrap(),
        &format!("https://www.youtube.com/channel/{CHANNEL}/videos")
    );
    assert!(args.contains(&"--flat-playlist".to_owned()));
    let at = args.iter().position(|a| a == "--playlist-end").unwrap();
    assert_eq!(args[at + 1], "100");

    let episode = s.db.channel_video("Zq8uV4hNs1M").await.unwrap().unwrap();
    assert_eq!(episode.duration_secs, Some(2131));
    assert_eq!(episode.published_at, Some(1_721_127_600));
    assert!(episode.published_approx);
    assert_eq!(episode.source, VideoSource::Listing);
    let old = s.db.channel_video("Lm2pQ9rTt0B").await.unwrap().unwrap();
    assert_eq!(old.published_at, None);
    assert!(!old.published_approx);
    assert!(
        s.db.channel_listing_times().await.unwrap()[CHANNEL] > 0,
        "listing time is kept for the next start"
    );
    assert!(s.watcher.health().channels[0].last_listing_at.is_some());
}

#[tokio::test]
async fn a_bot_check_pauses_lookups_and_stores_nothing() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "bot_check");
    let s = setup("http://localhost:9/unused", &fake.path).await;

    let err = s.watcher.list_channel(CHANNEL).await.unwrap_err();
    assert!(
        matches!(err, WatchError::Listing(ResolveError::BotCheck(_))),
        "{err}"
    );
    let paused = s.limiter.status().paused_for.expect("limiter paused");
    assert!(paused > Duration::from_secs(890), "{paused:?}");
    assert!(s.db.unmatched_videos(0).await.unwrap().is_empty());
    assert!(s.db.channel_listing_times().await.unwrap().is_empty());
    let health = s.watcher.health();
    assert!(health.channels[0].last_listing_error.is_some());
    assert_eq!(health.channels[0].last_listing_at, None);
}

#[tokio::test]
async fn bad_channel_ids_never_reach_youtube() {
    let s = setup("http://localhost:9/unused", "/nonexistent/yt-dlp".as_ref()).await;
    for id in ["@arydigital", "UC4JCksJF76g_MdzPVBJoC3Q&x=1"] {
        assert!(matches!(
            s.watcher.poll_feed(id).await,
            Err(WatchError::InvalidChannel(_))
        ));
        assert!(matches!(
            s.watcher.list_channel(id).await,
            Err(WatchError::InvalidChannel(_))
        ));
    }
    assert_eq!(s.limiter.status().available, 3.0);
}

#[tokio::test]
async fn the_loops_stay_idle_without_mappings_and_start_on_one() {
    let (feed, url) = start_feed().await;
    answer(&feed, 200, "youtube/feed-channel.xml", Some("\"v1\""), None);
    let s = setup(&url, "/nonexistent/yt-dlp".as_ref()).await;
    let task = tokio::spawn(s.watcher.clone().run());

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!s.watcher.health().active);
    assert!(feed.lock().unwrap().requests.is_empty());
    task.abort();

    // With a mapping the feed is read at once. The first listing waits two minutes, so the fake
    // yt-dlp path is never run here.
    s.db.insert_mapping(&mapping()).await.unwrap();
    let task = tokio::spawn(s.watcher.clone().run());
    tokio::time::timeout(Duration::from_secs(5), async {
        while s.db.unmatched_videos(0).await.unwrap().len() < 3 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("feed was read");
    task.abort();
    let health = s.watcher.health();
    assert!(health.active);
    assert_eq!(health.channels.len(), 1);
    assert_eq!(feed.lock().unwrap().requests.len(), 1);
    assert_eq!(s.limiter.status().available, 3.0);
}

#[tokio::test]
async fn extra_channels_are_watched_without_a_mapping() {
    let (feed, url) = start_feed().await;
    answer(&feed, 200, "youtube/feed-channel.xml", Some("\"v1\""), None);
    let s = setup(&url, "/nonexistent/yt-dlp".as_ref()).await;
    // The global film channels from `films.channels`.
    let watcher = Arc::new(
        Watcher::new(
            config::Watcher {
                feed_url: url.parse().unwrap(),
                rss_spacing: Duration::from_secs(1),
                ..config::Watcher::default()
            },
            s.db.clone(),
            s.limiter.clone(),
            YtDlp::new("/nonexistent/yt-dlp"),
            Duration::from_secs(900),
            None,
        )
        .unwrap()
        .with_channels(vec![CHANNEL.to_owned()]),
    );
    let task = tokio::spawn(watcher.clone().run());
    tokio::time::timeout(Duration::from_secs(5), async {
        while s.db.unmatched_videos(0).await.unwrap().len() < 3 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("feed was read");
    task.abort();
    assert!(watcher.health().active);
    assert_eq!(watcher.health().channels.len(), 1);
}

/// Waits up to five seconds for `done`.
async fn until(what: &str, done: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting: {what}"));
}

#[tokio::test]
async fn a_feed_failing_past_one_interval_falls_back_to_short_listings_until_it_works() {
    let (feed, url) = start_feed().await;
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "flat_listing");
    let s = setup(&url, &fake.path).await;
    s.db.insert_mapping(&mapping()).await.unwrap();
    let interval = Duration::from_millis(300);
    let watcher = Arc::new(
        Watcher::new(
            config::Watcher {
                feed_url: url.parse().unwrap(),
                rss_interval: interval,
                rss_jitter: Duration::ZERO,
                rss_spacing: Duration::from_millis(10),
                // 100 ms apart, so the cap never holds this test up.
                fallback_per_hour: 36_000,
                ..config::Watcher::default()
            },
            s.db.clone(),
            s.limiter.clone(),
            YtDlp::new(&fake.path),
            Duration::from_secs(900),
            None,
        )
        .unwrap(),
    );
    let used = || (3.0 - s.limiter.status().available).round();

    // Every feed answers 404. Within the first interval only the feed is tried.
    let task = tokio::spawn(watcher.clone().run());
    until("feed tried", || {
        watcher
            .health()
            .channels
            .first()
            .is_some_and(|c| c.feed_failures > 0)
    })
    .await;
    let ch = watcher.health().channels[0].clone();
    assert!(ch.feed_failing_since.is_some());
    assert!(!ch.feed_fallback);
    assert_eq!(used(), 0.0);

    // Past one interval the channel is listed instead: one short list through the limiter.
    until("fallback list ran", || {
        watcher.health().channels[0].last_fallback_at.is_some()
    })
    .await;
    let ch = watcher.health().channels[0].clone();
    assert!(ch.feed_fallback);
    assert_eq!(ch.last_fallback_error, None);
    assert!(used() >= 1.0);
    let args = fake.last_args();
    let at = args.iter().position(|a| a == "--playlist-end").unwrap();
    assert_eq!(args[at + 1], "15");
    assert_eq!(
        args.last().unwrap(),
        &format!("https://www.youtube.com/channel/{CHANNEL}/videos")
    );
    let episode = s.db.channel_video("Zq8uV4hNs1M").await.unwrap().unwrap();
    assert_eq!(episode.source, VideoSource::Listing);
    assert!(
        s.db.channel_listing_times().await.unwrap().is_empty(),
        "a fallback list is not the daily full listing"
    );

    // The feed works again: the fallback stops at the next feed read.
    answer(&feed, 200, "youtube/feed-channel.xml", Some("\"v1\""), None);
    until("feed recovered", || {
        watcher.health().channels[0].feed_failures == 0
    })
    .await;
    let ch = watcher.health().channels[0].clone();
    assert!(!ch.feed_fallback);
    assert_eq!(ch.feed_failing_since, None);
    let after = used();
    tokio::time::sleep(4 * interval).await;
    assert_eq!(used(), after, "no fallback list once the feed works");
    task.abort();
}
