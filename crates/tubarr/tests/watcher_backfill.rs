//! Channel search backfill: the watcher with a fake yt-dlp, the store, and the matcher reading
//! what a search stored through the real Db. Nothing here reaches YouTube.

use std::{collections::HashMap, sync::Arc, time::Duration};

mod support;

use tubarr::{
    config,
    matcher::{Wanted, WantedEpisode, WantedSeries, match_shows},
    ratelimit::{RateLimitConfig, RateLimiter},
    resolver::YtDlp,
    store::{Db, EpisodeFilter, Mapping, MappingKind, NewMapping, StoreError, VideoSource},
    watcher::{ListingRun, Watcher, backfill::BackfillTurn},
};

const CHANNEL: &str = "UC4JCksJF76g_MdzPVBJoC3Q";
const HOUR: Duration = Duration::from_secs(3600);

struct Setup {
    watcher: Watcher,
    db: Db,
    limiter: Arc<RateLimiter>,
    _dir: tempfile::TempDir,
}

/// A watcher over a fresh database. The limiter refills one token an hour, so a test sees exactly
/// how many lookups were made.
async fn setup(ytdlp: &std::path::Path, settings: config::Watcher) -> Setup {
    let dir = support::temp_dir();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    let limiter = Arc::new(RateLimiter::new(RateLimitConfig {
        interval: HOUR,
        burst: 3,
        max_jitter: Duration::ZERO,
    }));
    let watcher = Watcher::new(
        settings,
        db.clone(),
        limiter.clone(),
        YtDlp::new(ytdlp),
        Duration::from_secs(900),
        None,
    )
    .unwrap();
    Setup {
        watcher,
        db,
        limiter,
        _dir: dir,
    }
}

async fn drama(db: &Db) -> Mapping {
    db.insert_mapping(&NewMapping {
        kind: MappingKind::Show,
        tvdb_id: Some(460_100),
        tmdb_id: None,
        title: "Dar E Nijaat".to_owned(),
        language: "Urdu".to_owned(),
        channel_ids: vec![CHANNEL.to_owned()],
        episode_regex: None,
        excludes: vec!["Promo".to_owned()],
        min_duration_secs: None,
    })
    .await
    .unwrap()
}

fn tokens(limiter: &RateLimiter) -> f64 {
    limiter.status().available
}

#[tokio::test]
async fn a_backfill_searches_the_channel_once_and_starts_over_on_a_new_alias() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "flat_channel_search");
    let s = setup(&fake.path, config::Watcher::default()).await;
    let mapping = drama(&s.db).await;

    let turn = s.watcher.backfill_turn().await.unwrap();
    let BackfillTurn::Searched { mapping_id, result } = turn else {
        panic!("{turn:?}");
    };
    assert_eq!(mapping_id, mapping.id);
    // Five uploads of the channel: the playlist result and the other channel's video are skipped.
    assert_eq!(result.unwrap(), ListingRun { seen: 5, new: 5 });
    assert!((tokens(&s.limiter) - 2.0).abs() < 0.01);
    let args = fake.last_args();
    assert_eq!(
        args.last().unwrap(),
        &format!("https://www.youtube.com/channel/{CHANNEL}/search?query=Dar+E+Nijaat")
    );
    let at = args.iter().position(|a| a == "--playlist-end").unwrap();
    assert_eq!(args[at + 1], "400");

    let ep = s.db.channel_video("DenEp01aaaa").await.unwrap().unwrap();
    assert_eq!(ep.channel_id, CHANNEL);
    assert_eq!(ep.source, VideoSource::Listing);
    assert_eq!(ep.duration_secs, Some(2310));
    assert_eq!(ep.published_at, Some(1_700_006_400));
    assert!(s.db.channel_video("OtherChan01").await.unwrap().is_none());

    // Once a day: the next turn has nothing to do.
    assert!(matches!(
        s.watcher.backfill_turn().await.unwrap(),
        BackfillTurn::Idle
    ));

    // A new alias starts over and searches both names.
    s.db.set_aliases(mapping.id, &["Darr E Nijaat".to_owned()])
        .await
        .unwrap();
    let turn = s.watcher.backfill_turn().await.unwrap();
    assert!(
        matches!(turn, BackfillTurn::Searched { result: Ok(_), .. }),
        "{turn:?}"
    );
    assert!(tokens(&s.limiter) < 0.01);
    assert!(
        fake.last_args()
            .last()
            .unwrap()
            .ends_with("query=Darr+E+Nijaat")
    );
}

#[tokio::test]
async fn a_full_search_is_followed_by_one_for_the_episodes() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "flat_channel_search");
    // The fixture lists 7 results, so a search reading 5 comes back full.
    let small = config::Watcher {
        backfill_size: 5,
        ..config::Watcher::default()
    };
    let s = setup(&fake.path, small).await;
    drama(&s.db).await;

    let turn = s.watcher.backfill_turn().await.unwrap();
    let BackfillTurn::Searched { result, .. } = turn else {
        panic!("{turn:?}");
    };
    // Both searches return the same five uploads: new only the first time.
    assert_eq!(result.unwrap(), ListingRun { seen: 10, new: 5 });
    assert!((tokens(&s.limiter) - 1.0).abs() < 0.01);
    let args = fake.last_args();
    assert_eq!(
        args.last().unwrap(),
        &format!("https://www.youtube.com/channel/{CHANNEL}/search?query=Dar+E+Nijaat+Episode")
    );
    let at = args.iter().position(|a| a == "--playlist-end").unwrap();
    assert_eq!(args[at + 1], "5");
}

#[tokio::test]
async fn a_backfill_holds_off_while_lookups_are_paused() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "flat_channel_search");
    let s = setup(&fake.path, config::Watcher::default()).await;
    drama(&s.db).await;
    s.limiter.penalize(Duration::from_secs(900));

    let turn = s.watcher.backfill_turn().await.unwrap();
    assert!(
        matches!(turn, BackfillTurn::Held(p) if p > Duration::from_secs(890)),
        "{turn:?}"
    );
    assert!(fake.last_args().is_empty(), "yt-dlp ran while paused");
    // Still due once the pause is over.
    assert_eq!(
        s.db.due_backfills(tubarr::store::unix_now(), 86_400, 14)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn backfill_days_zero_turns_it_off() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "flat_channel_search");
    let off = config::Watcher {
        backfill_days: 0,
        ..config::Watcher::default()
    };
    let s = setup(&fake.path, off).await;
    drama(&s.db).await;
    assert!(matches!(
        s.watcher.backfill_turn().await.unwrap(),
        BackfillTurn::Idle
    ));
    assert!(fake.last_args().is_empty());
}

struct FakeWanted(Vec<WantedSeries>);

impl Wanted for FakeWanted {
    async fn series(&self) -> Result<Vec<WantedSeries>, StoreError> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn the_matcher_releases_back_episodes_a_search_found_and_ends_the_backfill() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "flat_channel_search");
    let s = setup(&fake.path, config::Watcher::default()).await;
    let mapping = drama(&s.db).await;
    s.watcher.backfill_turn().await.unwrap();
    for id in ["DenEp01aaaa", "DenEp02bbbb", "DenEp03Pt1c", "DenEp03Pt2c"] {
        sqlx::query(
            "INSERT INTO videos (video_id, title, duration_secs, upload_date, channel_id, \
             updated_at, height) VALUES (?, ?, 2200, '20231115', ?, 0, 1080)",
        )
        .bind(id)
        .bind(format!("Dar E Nijaat {id}"))
        .bind(CHANNEL)
        .execute(s.db.pool())
        .await
        .unwrap();
    }
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
    let report = match_shows(&s.db, &s.db, &wanted, &s.db).await.unwrap();
    assert_eq!(report.released, 3, "{report:?}");
    let mut got: HashMap<String, Vec<String>> = HashMap::new();
    for r in
        s.db.releases_for_mappings(&[mapping.id], &EpisodeFilter::Any, 100)
            .await
            .unwrap()
    {
        got.insert(r.video_id, r.part_ids);
    }
    assert!(got.contains_key("DenEp01aaaa"));
    assert!(got.contains_key("DenEp02bbbb"));
    assert_eq!(got["DenEp03Pt1c"], ["DenEp03Pt1c", "DenEp03Pt2c"]);

    // Every wanted episode is out, so the backfill is done even after the day has passed.
    let tomorrow = tubarr::store::unix_now() + 2 * 86_400;
    assert!(
        s.db.due_backfills(tomorrow, 86_400, 14)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_channel_listing_size_overrides_the_default() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "flat_listing");
    let s = setup(&fake.path, config::Watcher::default()).await;
    let watcher = s
        .watcher
        .with_listing_sizes(HashMap::from([(CHANNEL.to_owned(), 600)]));
    watcher.list_channel(CHANNEL).await.unwrap();
    let args = fake.last_args();
    let at = args.iter().position(|a| a == "--playlist-end").unwrap();
    assert_eq!(args[at + 1], "600");
}

/// Runs one backfill turn that fails, and returns its health row.
async fn failed_turn(s: &Setup) -> tubarr::watcher::backfill::BackfillHealth {
    let turn = s.watcher.backfill_turn().await.unwrap();
    assert!(
        matches!(turn, BackfillTurn::Searched { result: Err(_), .. }),
        "{turn:?}"
    );
    s.watcher.backfill_health().await.unwrap().remove(0)
}

#[tokio::test]
async fn a_5xx_retries_once_after_an_hour_and_then_waits_the_day() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "channel_search_503");
    let s = setup(&fake.path, config::Watcher::default()).await;
    let mapping = drama(&s.db).await;

    let row = failed_turn(&s).await;
    assert_eq!(row.mapping_id, mapping.id);
    assert_eq!(row.title, "Dar E Nijaat");
    assert!(!row.done);
    assert!(
        row.last_error
            .as_deref()
            .unwrap()
            .contains("HTTP Error 503")
    );
    let last = row.last_run_at.unwrap();
    assert_eq!(row.next_run_at, Some(last + 3600));
    assert!(
        s.db.due_backfills(last + 3599, 86_400, 14)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        s.db.due_backfills(last + 3600, 86_400, 14)
            .await
            .unwrap()
            .len(),
        1
    );

    // The retry fails as well: a full day, no second retry.
    sqlx::query("UPDATE mapping_backfill SET retry_at = 0")
        .execute(s.db.pool())
        .await
        .unwrap();
    let row = failed_turn(&s).await;
    let last = row.last_run_at.unwrap();
    assert_eq!(row.next_run_at, Some(last + 86_400));
}

#[tokio::test]
async fn a_429_waits_the_full_day() {
    let dir = support::temp_dir();
    let fake = support::FakeYtDlp::from_index(dir.path(), "channel_search_429");
    let s = setup(&fake.path, config::Watcher::default()).await;
    drama(&s.db).await;
    let row = failed_turn(&s).await;
    assert_eq!(row.next_run_at, Some(row.last_run_at.unwrap() + 86_400));
    assert!(s.limiter.status().paused_for.is_some());
}
