//! Official playlist listings: the watcher with a fake yt-dlp, the store, the matcher reading the
//! order through the real Db. Nothing here reaches YouTube.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

mod support;

use tokio::sync::Notify;
use tubarr::{
    config,
    matcher::{Wanted, WantedEpisode, WantedSeries, match_shows},
    ratelimit::{RateLimitConfig, RateLimiter},
    resolver::{ResolveError, YtDlp},
    store::{
        Db, EpisodeFilter, Mapping, MappingKind, MatchOptions, NewMapping, PinTarget, PlaylistMode,
        StoreError, VideoSource,
    },
    watcher::{ListingRun, WatchError, Watcher, playlists::PlaylistTurn},
};

const CHANNEL: &str = "UC4JCksJF76g_MdzPVBJoC3Q";
const PLAYLIST: &str = "PLkmktOfficialPlaylist0000000001";
const HOUR: Duration = Duration::from_secs(3600);

struct Setup {
    watcher: Arc<Watcher>,
    db: Db,
    limiter: Arc<RateLimiter>,
    found: Arc<Notify>,
    _dir: tempfile::TempDir,
}

/// A watcher over a fresh database. The limiter refills one token an hour, so a test sees exactly
/// how many tokens it used.
async fn setup(ytdlp: &std::path::Path) -> Setup {
    let dir = support::temp_dir();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    let limiter = Arc::new(RateLimiter::new(RateLimitConfig {
        interval: HOUR,
        burst: 3,
        max_jitter: Duration::ZERO,
    }));
    let found = Arc::new(Notify::new());
    let watcher = Watcher::new(
        config::Watcher {
            feed_url: "http://127.0.0.1:9/feeds/videos.xml".parse().unwrap(),
            ..config::Watcher::default()
        },
        db.clone(),
        limiter.clone(),
        YtDlp::new(ytdlp),
        Duration::from_secs(900),
    )
    .with_found(found.clone());
    Setup {
        watcher: Arc::new(watcher),
        db,
        limiter,
        found,
        _dir: dir,
    }
}

async fn order(db: &Db) -> Vec<String> {
    db.playlist_items(PLAYLIST)
        .await
        .unwrap()
        .expect("playlist listed")
        .into_iter()
        .map(|i| i.video_id)
        .collect()
}

#[tokio::test]
async fn a_playlist_listing_keeps_the_order_and_feeds_the_matcher() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "flat_playlist");
    let s = setup(&fake.path).await;

    assert_eq!(
        s.watcher.list_playlist(PLAYLIST).await.unwrap(),
        ListingRun { seen: 4, new: 4 }
    );
    assert!((s.limiter.status().available - 2.0).abs() < 0.01);
    let args = fake.last_args();
    assert_eq!(
        args.last().unwrap(),
        &format!("https://www.youtube.com/playlist?list={PLAYLIST}")
    );
    assert!(args.contains(&"--flat-playlist".to_owned()));

    // The private video is left out, the rest keep the playlist's order.
    assert_eq!(
        order(&s.db).await,
        ["KmktTeaser1", "KmktEp01aaa", "KmktEp02bbb", "KmktEp03ccc"]
    );
    let items = s.db.playlist_items(PLAYLIST).await.unwrap().unwrap();
    assert_eq!(items[1].duration_secs, Some(2290));
    assert!(items[1].title.starts_with("Kabhi Main Kabhi Tum Episode 1"));

    // They are channel uploads too, so the matcher sees them.
    let ep = s.db.channel_video("KmktEp01aaa").await.unwrap().unwrap();
    assert_eq!(ep.channel_id, CHANNEL);
    assert_eq!(ep.source, VideoSource::Listing);
    assert_eq!(ep.duration_secs, Some(2290));
    assert_eq!(ep.published_at, None);
    assert!(s.db.channel_video("KmktPrivate").await.unwrap().is_none());

    tokio::time::timeout(Duration::from_secs(1), s.found.notified())
        .await
        .expect("the matcher is woken after a listing");

    let health = s.watcher.health();
    assert_eq!(health.playlists.len(), 1);
    let p = &health.playlists[0];
    assert_eq!(p.playlist_id, PLAYLIST);
    assert_eq!(p.videos, Some(4));
    assert!(p.last_listing_at.is_some());
    assert_eq!(p.last_listing_error, None);

    // The next listing replaces the stored order in full.
    let json = dir.path().join("shorter.json");
    std::fs::write(
        &json,
        format!(
            r#"{{"_type": "playlist", "id": "{PLAYLIST}", "channel_id": "{CHANNEL}", "entries": [
                {{"_type": "url", "ie_key": "Youtube", "id": "KmktEp02bbb", "title": "Kabhi Main Kabhi Tum Episode 2", "duration": 2205.0, "channel_id": null}},
                {{"_type": "url", "ie_key": "Youtube", "id": "KmktEp01aaa", "title": "Kabhi Main Kabhi Tum Episode 1", "duration": 2290.0, "channel_id": "{CHANNEL}"}}
            ]}}"#
        ),
    )
    .unwrap();
    let fake = support::FakeYtDlp::new(dir.path(), Some(json.to_str().unwrap()), None, 0);
    let s2 = Watcher::new(
        config::Watcher::default(),
        s.db.clone(),
        s.limiter.clone(),
        YtDlp::new(&fake.path),
        Duration::from_secs(900),
    );
    assert_eq!(
        s2.list_playlist(PLAYLIST).await.unwrap(),
        ListingRun { seen: 2, new: 0 }
    );
    assert_eq!(order(&s.db).await, ["KmktEp02bbb", "KmktEp01aaa"]);
}

#[tokio::test]
async fn a_bot_check_on_a_playlist_pauses_lookups_and_keeps_the_old_order() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "bot_check");
    let s = setup(&fake.path).await;
    s.db.replace_playlist(PLAYLIST, &[]).await.unwrap();

    let err = s.watcher.list_playlist(PLAYLIST).await.unwrap_err();
    assert!(
        matches!(err, WatchError::Listing(ResolveError::BotCheck(_))),
        "{err}"
    );
    let paused = s.limiter.status().paused_for.expect("limiter paused");
    assert!(paused > Duration::from_secs(890), "{paused:?}");
    assert_eq!(s.db.playlist_items(PLAYLIST).await.unwrap(), Some(vec![]));
    let health = s.watcher.health();
    assert!(health.playlists[0].last_listing_error.is_some());
    assert_eq!(health.playlists[0].last_listing_at, None);
}

#[tokio::test]
async fn bad_playlist_ids_never_reach_youtube() {
    let s = setup("/nonexistent/yt-dlp".as_ref()).await;
    for id in ["PLx", "PLkmktOfficial&index=2", "../../etc/passwd00"] {
        assert!(matches!(
            s.watcher.list_playlist(id).await,
            Err(WatchError::InvalidPlaylist(_))
        ));
    }
    assert_eq!(s.limiter.status().available, 3.0);
}

#[tokio::test]
async fn a_turn_holds_off_while_lookups_are_paused() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "flat_playlist");
    let s = setup(&fake.path).await;
    s.db.request_playlist(PLAYLIST).await.unwrap();
    s.limiter.penalize(Duration::from_secs(900));

    let turn = s.watcher.playlist_turn(PLAYLIST).await;
    assert!(
        matches!(turn, PlaylistTurn::Held(p) if p > Duration::from_secs(890)),
        "{turn:?}"
    );
    assert!(fake.last_args().is_empty(), "yt-dlp ran while paused");
    assert_eq!(s.db.playlist_items(PLAYLIST).await.unwrap(), None);
    assert_eq!(s.db.requested_playlists().await.unwrap(), [PLAYLIST]);

    // Once the pause is over the same turn lists it and settles the request.
    let fresh = Arc::new(RateLimiter::new(RateLimitConfig {
        interval: HOUR,
        burst: 3,
        max_jitter: Duration::ZERO,
    }));
    let watcher = Watcher::new(
        config::Watcher::default(),
        s.db.clone(),
        fresh,
        YtDlp::new(&fake.path),
        Duration::from_secs(900),
    );
    let turn = watcher.playlist_turn(PLAYLIST).await;
    assert!(matches!(turn, PlaylistTurn::Listed(Ok(_))), "{turn:?}");
    assert!(s.db.requested_playlists().await.unwrap().is_empty());
    assert_eq!(order(&s.db).await.len(), 4);
}

struct FakeWanted(Vec<WantedSeries>);

impl Wanted for FakeWanted {
    async fn series(&self) -> Result<Vec<WantedSeries>, StoreError> {
        Ok(self.0.clone())
    }
}

async fn playlist_mapping(db: &Db) -> Mapping {
    let mapping = db
        .insert_mapping(&NewMapping {
            kind: MappingKind::Show,
            tvdb_id: Some(452_001),
            tmdb_id: None,
            title: "Kabhi Main Kabhi Tum".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec![CHANNEL.to_owned()],
            episode_regex: None,
            excludes: vec!["Teaser".to_owned()],
            min_duration_secs: None,
        })
        .await
        .unwrap();
    db.set_match_options(
        mapping.id,
        &MatchOptions {
            playlist_id: Some(PLAYLIST.to_owned()),
            playlist_season: 1,
            playlist_offset: 1,
            playlist_mode: PlaylistMode::Order,
            title_number: false,
            ..MatchOptions::default()
        },
    )
    .await
    .unwrap()
    .unwrap()
}

#[tokio::test]
async fn the_matcher_releases_playlist_episodes_from_a_real_listing() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "flat_playlist");
    let s = setup(&fake.path).await;
    let mapping = playlist_mapping(&s.db).await;
    assert_eq!(s.db.watched_playlists().await.unwrap(), [PLAYLIST]);

    let wanted = FakeWanted(vec![WantedSeries {
        mapping: mapping.clone(),
        episodes: (1..=3)
            .map(|episode| WantedEpisode {
                season: 1,
                episode,
                air_date: None,
                title: None,
                monitored: true,
            })
            .collect(),
    }]);
    // Before a listing the mapping waits.
    let report = match_shows(&s.db, &s.db, &wanted, &s.db).await.unwrap();
    assert!(report.skipped[0].1.contains("not listed yet"), "{report:?}");

    s.watcher.list_playlist(PLAYLIST).await.unwrap();
    // The 58 second teaser is a Short: it takes no place in the numbering.
    let teaser = s.db.channel_video("KmktTeaser1").await.unwrap().unwrap();
    assert_eq!(teaser.skip_reason.as_deref(), Some("a YouTube Short"));
    assert_eq!(
        s.db.playlist_order(PLAYLIST).await.unwrap().unwrap(),
        ["KmktEp01aaa", "KmktEp02bbb", "KmktEp03ccc"]
    );
    // The resolver's earlier lookups know the heights.
    for id in ["KmktEp01aaa", "KmktEp02bbb", "KmktEp03ccc"] {
        sqlx::query(
            "INSERT INTO videos (video_id, title, duration_secs, upload_date, channel_id, \
             updated_at, height) VALUES (?, ?, 2200, '20240702', ?, 0, 1080)",
        )
        .bind(id)
        .bind(format!("Kabhi Main Kabhi Tum {id}"))
        .bind(CHANNEL)
        .execute(s.db.pool())
        .await
        .unwrap();
    }
    let report = match_shows(&s.db, &s.db, &wanted, &s.db).await.unwrap();
    assert_eq!(report.released, 3, "{report:?}");
    let mut got: BTreeMap<String, String> = BTreeMap::new();
    for r in
        s.db.releases_for_mappings(&[mapping.id], &EpisodeFilter::Any, 100)
            .await
            .unwrap()
    {
        if let PinTarget::Episode { season, episode } = r.target {
            got.insert(format!("S{season:02}E{episode:02}"), r.video_id);
        }
    }
    assert_eq!(
        got,
        BTreeMap::from([
            ("S01E01".to_owned(), "KmktEp01aaa".to_owned()),
            ("S01E02".to_owned(), "KmktEp02bbb".to_owned()),
            ("S01E03".to_owned(), "KmktEp03ccc".to_owned()),
        ])
    );
}
