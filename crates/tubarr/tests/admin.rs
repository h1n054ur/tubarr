//! The admin JSON API behind the web UI: auth, listing, editing and the dry run.

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use tubarr::{
    AppState,
    binaries::BinaryManager,
    config::Config,
    store::{
        ArrEntry, ArrSource, Db, MappingKind, NewMapping, NewRelease, NewUnmatched, PinTarget,
        SeenVideo, UnmatchedState, VideoSource,
    },
};

const KEY: &str = "test-admin-key";
const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";

struct Kit {
    app: axum::Router,
    db: Db,
    _dir: tempfile::TempDir,
}

async fn kit(with_key: bool) -> Kit {
    kit_with(with_key, "").await
}

/// `tables` go after the top-level keys: TOML sections like `[channels.UC...]`.
async fn kit_with(with_key: bool, tables: &str) -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let mut toml = format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\n",
        dir.path()
    );
    if with_key {
        let key_file = dir.path().join("key");
        std::fs::write(&key_file, KEY).unwrap();
        toml.push_str(&format!("api_key_file = {key_file:?}\n"));
    }
    toml.push_str(tables);
    let config = Config::from_toml(&toml).unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = std::sync::Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let app = tubarr::app(AppState::new(config, db.clone(), binaries).unwrap());
    Kit { app, db, _dir: dir }
}

fn drama() -> NewMapping {
    NewMapping {
        kind: MappingKind::Show,
        tvdb_id: Some(452_001),
        tmdb_id: None,
        title: "Kabhi Main Kabhi Tum".to_owned(),
        language: "Urdu".to_owned(),
        channel_ids: vec![ARY.to_owned()],
        episode_regex: None,
        excludes: vec![],
        min_duration_secs: None,
    }
}

fn film() -> NewMapping {
    NewMapping {
        kind: MappingKind::Film,
        tvdb_id: None,
        tmdb_id: Some(352_173),
        title: "Theri".to_owned(),
        language: "Hindi".to_owned(),
        channel_ids: vec![],
        episode_regex: None,
        excludes: vec![],
        min_duration_secs: None,
    }
}

async fn seen(db: &Db, id: &str, title: &str, mins: i64) {
    db.upsert_seen_video(&SeenVideo {
        video_id: id.to_owned(),
        channel_id: ARY.to_owned(),
        title: title.to_owned(),
        published_at: Some(1_760_000_000),
        published_approx: false,
        duration_secs: Some(mins * 60),
        source: VideoSource::Rss,
    })
    .await
    .unwrap();
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    call_with_key(app, method, path, body, Some(KEY)).await
}

async fn call_with_key(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
    key: Option<&str>,
) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(key) = key {
        req = req.header("x-api-key", key);
    }
    let req = match body {
        Some(b) => req
            .header("content-type", "application/json")
            .body(Body::from(b.to_string())),
        None => req.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

#[tokio::test]
async fn auth_is_refused_without_the_right_key() {
    let k = kit(true).await;
    let (status, body) = call_with_key(&k.app, "GET", "/admin/api/mappings", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body["error"].as_str().unwrap().contains("API key"));
    let (status, _) =
        call_with_key(&k.app, "GET", "/admin/api/mappings", None, Some("wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // No key configured: only a signed-in session gets in, whatever key the request sends.
    let open = kit(false).await;
    let (status, _) = call(&open.app, "GET", "/admin/api/mappings", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn list_and_filter_with_counts() {
    let k = kit(true).await;
    let show = k.db.insert_mapping(&drama()).await.unwrap();
    k.db.insert_mapping(&film()).await.unwrap();

    let (status, list) = call(&k.app, "GET", "/admin/api/mappings", None).await;
    assert_eq!(status, StatusCode::OK);
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["title"], "Kabhi Main Kabhi Tum");
    assert_eq!(list[0]["kind"], "show");
    assert_eq!(list[0]["active"], true);
    assert_eq!(list[0]["channel_ids"], json!([ARY]));
    assert_eq!(list[0]["releases"], 0);
    assert_eq!(list[0]["unmatched"], 0);

    // Back episodes the backfill limit left out are not counted as unmatched.
    let note = |video_id: &str, state| NewUnmatched {
        video_id: video_id.to_owned(),
        state,
        season: None,
        episode: None,
        part: None,
        reason: "test".to_owned(),
    };
    k.db.replace_unmatched(
        show.id,
        &[
            note("a", UnmatchedState::Unsure),
            note("b", UnmatchedState::Capped),
            note("c", UnmatchedState::Capped),
        ],
    )
    .await
    .unwrap();
    let (_, list) = call(&k.app, "GET", "/admin/api/mappings", None).await;
    assert_eq!(list[0]["unmatched"], 1);

    let (_, films) = call(&k.app, "GET", "/admin/api/mappings?kind=film", None).await;
    assert_eq!(films.as_array().unwrap().len(), 1);
    assert_eq!(films[0]["tmdb_id"], 352_173);
    let (_, inactive) = call(&k.app, "GET", "/admin/api/mappings?active=false", None).await;
    assert!(inactive.as_array().unwrap().is_empty());
    let (status, _) = call(&k.app, "GET", "/admin/api/mappings?kind=cartoon", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, one) = call(
        &k.app,
        "GET",
        &format!("/admin/api/mappings/{}", show.id),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(one["tvdb_id"], 452_001);
    let (status, _) = call(&k.app, "GET", "/admin/api/mappings/999", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&k.app, "GET", "/admin/api/nothing-here", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn podcasts_are_shows_matched_by_number() {
    let k = kit(true).await;
    k.db.insert_mapping(&drama()).await.unwrap();
    let podcast =
        k.db.insert_mapping(&NewMapping {
            tvdb_id: Some(326_959),
            title: "The Joe Rogan Experience".to_owned(),
            ..drama()
        })
        .await
        .unwrap();
    k.db.insert_mapping(&film()).await.unwrap();
    let (status, _) = call(
        &k.app,
        "PATCH",
        &format!("/admin/api/mappings/{}", podcast.id),
        Some(json!({ "title_number": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let titles = |list: &Value| -> Vec<String> {
        list.as_array()
            .unwrap()
            .iter()
            .map(|m| m["title"].as_str().unwrap().to_owned())
            .collect()
    };
    let (_, all) = call(&k.app, "GET", "/admin/api/mappings", None).await;
    let flags: Vec<bool> = all
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["is_podcast"].as_bool().unwrap())
        .collect();
    assert_eq!(flags, [false, true, false]);
    assert_eq!(all[1]["kind"], "show");

    let (status, podcasts) = call(&k.app, "GET", "/admin/api/mappings?kind=podcast", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(titles(&podcasts), ["The Joe Rogan Experience"]);
    // Shows keep their podcasts, so nothing drops out of the Shows filter.
    let (_, shows) = call(&k.app, "GET", "/admin/api/mappings?kind=show", None).await;
    assert_eq!(
        titles(&shows),
        ["Kabhi Main Kabhi Tum", "The Joe Rogan Experience"]
    );
}

#[tokio::test]
async fn patch_changes_only_tubarr_fields() {
    let k = kit(true).await;
    let show = k.db.insert_mapping(&drama()).await.unwrap();
    let path = format!("/admin/api/mappings/{}", show.id);

    let (status, saved) = call(
        &k.app,
        "PATCH",
        &path,
        Some(json!({
            "excludes": [" Recap ", "Recap", ""],
            "min_duration_secs": 1800,
            "episode_regex": r"Kabhi Main Kabhi Tum.*?Ep(?:isode)?\s*(?P<ep>\d+)",
            "utc_offset_mins": 300,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["excludes"], json!(["Recap"]));
    assert_eq!(saved["min_duration_secs"], 1800);
    assert_eq!(saved["utc_offset_mins"], 300);
    assert_eq!(saved["language"], "Urdu", "missing fields keep their value");
    let stored = k.db.mapping(show.id).await.unwrap().unwrap();
    assert_eq!(stored.min_duration_secs, Some(1800));

    // null clears a nullable field.
    let (_, cleared) = call(
        &k.app,
        "PATCH",
        &path,
        Some(json!({ "episode_regex": null })),
    )
    .await;
    assert_eq!(cleared["episode_regex"], Value::Null);
    assert_eq!(cleared["min_duration_secs"], 1800);

    // A playlist feeds the title matcher unless it is set to number by order.
    assert_eq!(cleared["playlist_mode"], "source");
    let (status, ordered) = call(
        &k.app,
        "PATCH",
        &path,
        Some(json!({ "playlist_id": "PLabcdefghijkl", "playlist_mode": "order" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ordered}");
    assert_eq!(ordered["playlist_mode"], "order");
    assert_eq!(
        k.db.mapping(show.id).await.unwrap().unwrap().playlist_mode,
        tubarr::store::PlaylistMode::Order
    );

    // The backfill limit starts unset (every back episode), takes a number and clears with null.
    assert_eq!(ordered["backfill_limit"], Value::Null);
    let (status, limited) =
        call(&k.app, "PATCH", &path, Some(json!({ "backfill_limit": 5 }))).await;
    assert_eq!(status, StatusCode::OK, "{limited}");
    assert_eq!(limited["backfill_limit"], 5);
    assert_eq!(
        k.db.mapping(show.id).await.unwrap().unwrap().backfill_limit,
        Some(5)
    );
    let (_, unlimited) = call(
        &k.app,
        "PATCH",
        &path,
        Some(json!({ "backfill_limit": null })),
    )
    .await;
    assert_eq!(unlimited["backfill_limit"], Value::Null);
    // A podcast numbered in TVDB's episode titles: off until set.
    assert_eq!(ordered["title_number"], false);
    let (status, numbered) = call(
        &k.app,
        "PATCH",
        &path,
        Some(json!({ "title_number": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{numbered}");
    assert_eq!(numbered["title_number"], true);
    assert!(k.db.mapping(show.id).await.unwrap().unwrap().title_number);

    // Fields the arr owns, bad regexes and bad channel ids are refused, and nothing changes.
    for bad in [
        json!({ "title": "Something else" }),
        json!({ "episode_regex": "Episode (\\d+)" }),
        json!({ "episode_regex": "(" }),
        json!({ "channel_ids": ["not-a-channel"] }),
        json!({ "min_duration_secs": 0 }),
        json!({ "playlist_mode": "position" }),
        json!({ "backfill_limit": -1 }),
    ] {
        let (status, body) = call(&k.app, "PATCH", &path, Some(bad.clone())).await;
        assert!(status.is_client_error(), "{bad} gave {status} {body}");
    }
    assert_eq!(
        k.db.mapping(show.id).await.unwrap().unwrap().title,
        "Kabhi Main Kabhi Tum"
    );
}

#[tokio::test]
async fn manual_mapping_is_added() {
    let k = kit(true).await;
    let (status, made) = call(
        &k.app,
        "POST",
        "/admin/api/mappings",
        Some(json!({
            "kind": "show",
            "title": "Ishq Murshid",
            "tvdb_id": 431_000,
            "language": "Urdu",
            "channel_ids": [ARY],
            "playlist_id": "PLabc",
            "playlist_offset": 0,
            "backfill_limit": 3,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["backfill_limit"], 3);
    assert_eq!(made["title"], "Ishq Murshid");
    assert_eq!(made["playlist_id"], "PLabc");
    assert_eq!(made["playlist_offset"], 0);
    assert_eq!(made["playlist_mode"], "source");
    assert_eq!(made["arr_id"], Value::Null);

    let (status, _) = call(
        &k.app,
        "POST",
        "/admin/api/mappings",
        Some(json!({ "kind": "show", "title": "Again", "tvdb_id": 431_000, "language": "Urdu" })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = call(
        &k.app,
        "POST",
        "/admin/api/mappings",
        Some(json!({ "kind": "film", "title": "No id", "language": "Hindi" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn the_podcast_preset_sets_podcast_rules_and_drama_stays_plain() {
    const JRE: &str = "UCzQUP1qoWDoEbmsQxvdjxgQ";
    let tables = format!("\n[channels.{ARY}]\nnetwork = \"ARY Digital\"\nutc_offset_mins = 300\n");
    let k = kit_with(true, &tables).await;
    let new = |title: &str, tvdb: i64, channel: &str, extra: Value| {
        let mut body = json!({
            "kind": "show", "title": title, "tvdb_id": tvdb, "language": "English",
            "channel_ids": [channel],
        });
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };

    // A channel missing from [channels] has no offset to go by: the user must give one.
    let (status, body) = call(
        &k.app,
        "POST",
        "/admin/api/mappings",
        Some(new(
            "The Joe Rogan Experience",
            1,
            JRE,
            json!({ "preset": "podcast" }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"].as_str().unwrap().contains("utc_offset_mins"),
        "{body}"
    );

    let (status, jre) = call(
        &k.app,
        "POST",
        "/admin/api/mappings",
        Some(new(
            "The Joe Rogan Experience",
            1,
            JRE,
            json!({ "preset": "podcast", "utc_offset_mins": -360 }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{jre}");
    assert_eq!(jre["title_number"], true);
    assert_eq!(jre["is_podcast"], true);
    assert_eq!(jre["backfill_limit"], 10);
    assert_eq!(jre["min_duration_secs"], 1200);
    assert_eq!(jre["utc_offset_mins"], -360);
    assert_eq!(jre["excludes"], json!(["Clip", "Best of"]));
    assert_eq!(jre["episode_thumbnails"], true);

    // On a listed channel the offset comes from the config; given values win over the preset.
    let (status, listed) = call(
        &k.app,
        "POST",
        "/admin/api/mappings",
        Some(new(
            "Some Podcast",
            2,
            ARY,
            json!({ "preset": "podcast", "backfill_limit": 3, "excludes": ["Recap"],
                    "episode_thumbnails": false }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{listed}");
    assert_eq!(listed["utc_offset_mins"], 300);
    assert_eq!(listed["backfill_limit"], 3);
    assert_eq!(listed["excludes"], json!(["Recap", "Clip", "Best of"]));
    assert_eq!(listed["episode_thumbnails"], false);
    // Without thumbnails it is still a podcast: the running number is what makes one.
    assert_eq!(listed["is_podcast"], true);

    let (status, drama) = call(
        &k.app,
        "POST",
        "/admin/api/mappings",
        Some(new("Ishq Murshid", 3, ARY, json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{drama}");
    assert_eq!(drama["title_number"], false);
    assert_eq!(drama["is_podcast"], false);
    assert_eq!(drama["backfill_limit"], Value::Null);
    assert_eq!(drama["min_duration_secs"], Value::Null);
    assert_eq!(drama["utc_offset_mins"], Value::Null);
    assert_eq!(drama["excludes"], json!([]));
    assert_eq!(drama["episode_thumbnails"], false);

    let (status, _) = call(
        &k.app,
        "POST",
        "/admin/api/mappings",
        Some(json!({
            "kind": "film", "title": "F", "tmdb_id": 9, "language": "Urdu", "preset": "podcast",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_time_zone_is_checked_stored_and_wins_for_podcasts() {
    const JRE: &str = "UCzQUP1qoWDoEbmsQxvdjxgQ";
    const THEO: &str = "UC5AQEUAwCh1sGDvkQtkDWUQ";
    let tables = format!("\n[channels.{THEO}]\ntimezone = \"America/Los_Angeles\"\n");
    let k = kit_with(true, &tables).await;
    let podcast = |tvdb: i64, channel: &str, zone: Value| {
        let mut body = json!({
            "kind": "show", "title": "Podcast", "tvdb_id": tvdb, "language": "English",
            "channel_ids": [channel], "preset": "podcast",
        });
        body.as_object_mut()
            .unwrap()
            .extend(zone.as_object().unwrap().clone());
        body
    };

    let (status, body) = call(
        &k.app,
        "POST",
        "/admin/api/mappings",
        Some(podcast(1, JRE, json!({ "timezone": "Central Time" }))),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"].as_str().unwrap().contains("Central Time"),
        "{body}"
    );

    // The time zone wins over an offset given with it, spelled as the database spells it.
    let (status, jre) = call(
        &k.app,
        "POST",
        "/admin/api/mappings",
        Some(podcast(
            1,
            JRE,
            json!({ "timezone": "america/chicago", "utc_offset_mins": -300 }),
        )),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{jre}");
    assert_eq!(jre["timezone"], "America/Chicago");
    assert_eq!(jre["utc_offset_mins"], Value::Null);

    // A listed channel with a time zone gives it to the podcast.
    let (status, theo) = call(
        &k.app,
        "POST",
        "/admin/api/mappings",
        Some(podcast(2, THEO, json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{theo}");
    assert_eq!(theo["timezone"], "America/Los_Angeles");

    // A patch sets and clears it, and refuses an unknown one.
    let path = format!("/admin/api/mappings/{}", jre["id"]);
    let (status, _) = call(
        &k.app,
        "PATCH",
        &path,
        Some(json!({ "timezone": "Mars/Olympus" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, saved) = call(&k.app, "PATCH", &path, Some(json!({ "timezone": "" }))).await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["timezone"], Value::Null);
    let stored =
        k.db.mapping(jre["id"].as_i64().unwrap())
            .await
            .unwrap()
            .unwrap();
    assert_eq!(stored.timezone, None);
}

#[tokio::test]
async fn dry_run_reports_verdicts_and_writes_nothing() {
    let k = kit(true).await;
    let show = k.db.insert_mapping(&drama()).await.unwrap();
    seen(
        &k.db,
        "ep1aaaaaaaa",
        "Kabhi Main Kabhi Tum - Episode 1 | ARY Digital",
        36,
    )
    .await;
    seen(
        &k.db,
        "teaser2aaaa",
        "Kabhi Main Kabhi Tum Episode 2 Teaser",
        1,
    )
    .await;
    seen(&k.db, "short3aaaaa", "Kabhi Main Kabhi Tum Episode 3", 15).await;
    let before = k.db.channel_video("ep1aaaaaaaa").await.unwrap().unwrap();
    let path = format!("/admin/api/mappings/{}/test", show.id);

    let (status, run) = call(&k.app, "POST", &path, None).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(
        run["counts"],
        json!({ "matched": 1, "unsure": 1, "excluded": 1 })
    );
    let verdict = |id: &str| {
        run["videos"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["video_id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(verdict("ep1aaaaaaaa")["verdict"], "matched");
    assert_eq!(verdict("ep1aaaaaaaa")["episode"], 1);
    assert_eq!(verdict("teaser2aaaa")["verdict"], "excluded");
    assert_eq!(verdict("short3aaaaa")["verdict"], "unsure");

    // Unsaved edits in the body are tested, not stored.
    let (_, edited) = call(
        &k.app,
        "POST",
        &path,
        Some(json!({ "min_duration_secs": 600 })),
    )
    .await;
    assert_eq!(edited["counts"]["matched"], 2);
    assert_eq!(
        k.db.mapping(show.id)
            .await
            .unwrap()
            .unwrap()
            .min_duration_secs,
        None
    );

    // Nothing was written: no releases, no notes, match states untouched.
    let releases: i64 = sqlx::query_scalar("SELECT count(*) FROM releases")
        .fetch_one(k.db.pool())
        .await
        .unwrap();
    let notes: i64 = sqlx::query_scalar("SELECT count(*) FROM unmatched")
        .fetch_one(k.db.pool())
        .await
        .unwrap();
    assert_eq!((releases, notes), (0, 0));
    assert_eq!(
        k.db.channel_video("ep1aaaaaaaa").await.unwrap().unwrap(),
        before
    );

    let (status, body) = call(&k.app, "POST", &path, Some(json!({ "episode_regex": "(" }))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"].as_str().unwrap().contains("regex"));
}

#[tokio::test]
async fn dry_run_uses_the_aliases() {
    let k = kit(true).await;
    let mut new = drama();
    new.title = "The Tale".to_owned();
    let show = k.db.insert_mapping(&new).await.unwrap();
    seen(
        &k.db,
        "ep1aaaaaaaa",
        "Kabhi Main Kabhi Tum - Episode 1 | ARY Digital",
        36,
    )
    .await;
    let path = format!("/admin/api/mappings/{}/test", show.id);
    let (_, run) = call(&k.app, "POST", &path, None).await;
    assert_eq!(run["videos"][0]["verdict"], "excluded");
    k.db.set_aliases(show.id, &["Kabhi Main Kabhi Tum".to_owned()])
        .await
        .unwrap();
    let (status, run) = call(&k.app, "POST", &path, None).await;
    assert_eq!(status, StatusCode::OK, "{run}");
    assert_eq!(run["videos"][0]["verdict"], "matched");
    assert_eq!(run["videos"][0]["episode"], 1);
}

#[tokio::test]
async fn ui_is_served_for_unknown_gets() {
    let k = kit(true).await;
    let res = k
        .app
        .clone()
        .oneshot(Request::get("/mappings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let ct = res.headers()["content-type"].to_str().unwrap().to_owned();
    assert!(ct.starts_with("text/html"), "{ct}");
    let expected = if tubarr::admin::ui::built() {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };
    assert_eq!(res.status(), expected);
    let missing = k
        .app
        .clone()
        .oneshot(Request::get("/assets/nope.js").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

fn synced_show(tvdb: i64, title: &str) -> ArrEntry {
    ArrEntry {
        kind: MappingKind::Show,
        key: tvdb,
        arr_id: tvdb / 1000,
        title: title.to_owned(),
        // What Sonarr reports for a Pakistani drama: it has no Urdu.
        language: "English".to_owned(),
        year: Some(2024),
        imdb_id: None,
        aliases: vec![],
    }
}

#[tokio::test]
async fn channels_set_later_give_an_arr_default_show_the_channel_language() {
    let tables = format!("\n[channels.{ARY}]\nnetwork = \"ARY Digital\"\nlanguage = \"Urdu\"\n");
    let k = kit_with(true, &tables).await;
    k.db.sync_arr_mappings(
        ArrSource::Sonarr,
        &[
            synced_show(452_001, "Kabhi Main Kabhi Tum"),
            synced_show(431_000, "Tere Bin"),
        ],
    )
    .await
    .unwrap();
    let drama = k.db.mapping_by_tvdb(452_001).await.unwrap().unwrap();
    let other = k.db.mapping_by_tvdb(431_000).await.unwrap().unwrap();
    let (_, view) = call(
        &k.app,
        "GET",
        &format!("/admin/api/mappings/{}", drama.id),
        None,
    )
    .await;
    assert_eq!(view["language_source"], "arr");

    // A release made while the show was still English follows the language.
    k.db.insert_release(&NewRelease {
        mapping_id: drama.id,
        video_id: "0eD-GQCutAU".to_owned(),
        part_ids: Vec::new(),
        target: PinTarget::Episode {
            season: 1,
            episode: 1,
        },
        year: None,
        imdb_id: None,
        video_title: "Kabhi Main Kabhi Tum Episode 1 | ARY Digital".to_owned(),
        language: "English".to_owned(),
        height: 1080,
        size: None,
        published_at: 1_792_000_000,
    })
    .await
    .unwrap();

    // The UI sends the unchanged language with every save.
    let (status, saved) = call(
        &k.app,
        "PATCH",
        &format!("/admin/api/mappings/{}", drama.id),
        Some(json!({ "language": "English", "channel_ids": [ARY] })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["language"], "Urdu");
    assert_eq!(saved["language_source"], "channel");

    let req = Request::get(format!("/api?t=tvsearch&tvdbid=452001&apikey={KEY}"))
        .body(Body::empty())
        .unwrap();
    let res = k.app.clone().oneshot(req).await.unwrap();
    let xml =
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(
        xml.contains("Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR"),
        "{xml}"
    );

    // A language the user changed is never touched, not even by a later channel change.
    let path = format!("/admin/api/mappings/{}", other.id);
    let (_, saved) = call(
        &k.app,
        "PATCH",
        &path,
        Some(json!({ "language": "Punjabi" })),
    )
    .await;
    assert_eq!(saved["language_source"], "user");
    let (_, saved) = call(
        &k.app,
        "PATCH",
        &path,
        Some(json!({ "language": "Punjabi", "channel_ids": [ARY] })),
    )
    .await;
    assert_eq!(saved["language"], "Punjabi");
    assert_eq!(saved["language_source"], "user");
}

#[tokio::test]
async fn release_checks_list_unresolved_and_unplayable_videos() {
    let k = kit(true).await;
    let mapping = k.db.insert_mapping(&drama()).await.unwrap();
    let release = |video_id: &str, episode: i64, part_ids: Vec<String>| NewRelease {
        mapping_id: mapping.id,
        video_id: video_id.to_owned(),
        part_ids,
        target: PinTarget::Episode { season: 1, episode },
        year: None,
        imdb_id: None,
        video_title: format!("Episode {episode}"),
        language: "Urdu".to_owned(),
        height: 1080,
        size: None,
        published_at: 1_760_000_000,
    };
    k.db.insert_release(&release("aaaaaaaaaaa", 1, vec![]))
        .await
        .unwrap();
    k.db.insert_release(&release(
        "bbbbbbbbbbb",
        2,
        vec!["bbbbbbbbbbb".to_owned(), "ccccccccccc".to_owned()],
    ))
    .await
    .unwrap();

    let (status, body) = call(&k.app, "GET", "/admin/api/releases/check", None).await;
    assert_eq!(status, StatusCode::OK);
    let videos: Vec<(&str, &str)> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["video_id"].as_str().unwrap(),
                r["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        videos,
        [
            ("aaaaaaaaaaa", "unresolved"),
            ("bbbbbbbbbbb", "unresolved"),
            ("ccccccccccc", "unresolved")
        ]
    );

    // A part found unplayable takes its whole release out of the feed.
    assert_eq!(
        k.db.mark_unplayable("ccccccccccc", "no AAC audio", true)
            .await
            .unwrap(),
        1
    );
    let (_, body) = call(&k.app, "GET", "/admin/api/releases/check", None).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
    let (status, body) = call(&k.app, "GET", "/admin/api/unplayable", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body[0]["video_id"], "ccccccccccc");
    assert_eq!(body[0]["reason"], "no AAC audio");

    let (status, _) = call(&k.app, "DELETE", "/admin/api/unplayable/ccccccccccc", None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = call(&k.app, "DELETE", "/admin/api/unplayable/ccccccccccc", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call_with_key(&k.app, "GET", "/admin/api/unplayable", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}
