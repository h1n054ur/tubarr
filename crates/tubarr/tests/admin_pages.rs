//! The admin calls behind the Unmatched, Activity, Health and Settings pages.

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
    routing::get,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tower::ServiceExt;
use tubarr::{
    AppState,
    binaries::BinaryManager,
    config::Config,
    store::{
        Db, GrabStatus, MappingKind, NewGrab, NewMapping, NewUnmatched, SeenVideo, UnmatchedState,
        VideoSource,
    },
};

const KEY: &str = "test-admin-key";
const SONARR_KEY: &str = "sonarr-secret-value-123";
const WEBHOOK: &str = "https://discord.com/api/webhooks/1/secret-webhook-token";
const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";

struct Kit {
    app: Router,
    db: Db,
    wake: Arc<Notify>,
    _dir: tempfile::TempDir,
}

/// `extra` is appended to the config. Binaries get explicit paths so a forced check never asks
/// GitHub.
async fn kit(extra: &str) -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("key");
    std::fs::write(&key_file, KEY).unwrap();
    let toml = format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\napi_key_file = {key_file:?}\n\
         [binaries]\nyt_dlp = {:?}\ndeno = {:?}\n{extra}",
        dir.path(),
        dir.path().join("yt-dlp"),
        dir.path().join("deno"),
    );
    let config = Config::from_toml(&toml).unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let state = AppState::new(config, db.clone(), binaries).unwrap();
    let wake = state.matcher.waker();
    Kit {
        app: tubarr::app(state),
        db,
        wake,
        _dir: dir,
    }
}

fn show() -> NewMapping {
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

fn note(video_id: &str, state: UnmatchedState) -> NewUnmatched {
    NewUnmatched {
        video_id: video_id.to_owned(),
        state,
        season: Some(1),
        episode: Some(3),
        part: None,
        reason: "two uploads say episode 3".to_owned(),
    }
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    call_with_key(app, method, path, body, Some(KEY)).await
}

async fn call_with_key(
    app: &Router,
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn woke(wake: &Notify) -> bool {
    tokio::time::timeout(Duration::from_millis(200), wake.notified())
        .await
        .is_ok()
}

#[tokio::test]
async fn every_new_call_refuses_a_wrong_key() {
    let k = kit("").await;
    let calls = [
        ("GET", "/admin/api/unmatched"),
        ("POST", "/admin/api/unmatched/1/ignore"),
        ("GET", "/admin/api/pins?mapping_id=1"),
        ("POST", "/admin/api/pins"),
        ("DELETE", "/admin/api/pins/1"),
        ("GET", "/admin/api/activity"),
        ("GET", "/admin/api/activity/events"),
        ("POST", "/admin/api/binaries/check"),
        ("POST", "/admin/api/binaries/yt-dlp/rollback"),
        ("GET", "/admin/api/settings"),
        ("POST", "/admin/api/arr/sonarr/test"),
        ("GET", "/admin/api/mappings/1/aliases"),
        ("PUT", "/admin/api/mappings/1/aliases"),
    ];
    for (method, path) in calls {
        for key in [None, Some("wrong")] {
            let (status, body) = call_with_key(&k.app, method, path, Some(json!({})), key).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{method} {path} {key:?}");
            assert!(body["error"].is_string());
        }
    }
}

#[tokio::test]
async fn unmatched_lists_with_video_facts_filters_and_ignores() {
    let k = kit("").await;
    let m = k.db.insert_mapping(&show()).await.unwrap().id;
    k.db.upsert_seen_video(&SeenVideo {
        video_id: "aaaaaaaaaaa".to_owned(),
        channel_id: ARY.to_owned(),
        title: "Kabhi Main Kabhi Tum Episode 3".to_owned(),
        published_at: Some(1_760_000_000),
        published_approx: false,
        duration_secs: Some(2100),
        source: VideoSource::Rss,
    })
    .await
    .unwrap();
    k.db.replace_unmatched(
        m,
        &[
            note("aaaaaaaaaaa", UnmatchedState::Unsure),
            note("bbbbbbbbbbb", UnmatchedState::Pending),
        ],
    )
    .await
    .unwrap();

    let (status, body) = call(&k.app, "GET", "/admin/api/unmatched", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert_eq!(body["total"], 2);
    let (_, body) = call(&k.app, "GET", "/admin/api/unmatched?state=unsure", None).await;
    let rows = body["items"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["video_title"], "Kabhi Main Kabhi Tum Episode 3");
    assert_eq!(row["mapping_title"], "Kabhi Main Kabhi Tum");
    assert_eq!(row["mapping_kind"], "show");
    assert_eq!(row["channel_id"], ARY);
    assert_eq!(row["duration_secs"], 2100);
    assert_eq!(row["published_at"], 1_760_000_000);
    assert_eq!(row["state"], "unsure");
    assert_eq!(row["ignored"], false);
    let id = row["id"].as_i64().unwrap();

    let (status, _) = call(
        &k.app,
        "GET",
        &format!("/admin/api/unmatched?mapping_id={}", m + 1),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(&k.app, "GET", "/admin/api/unmatched?state=nope", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let path = format!("/admin/api/unmatched/{id}/ignore");
    let (status, _) = call(&k.app, "POST", &path, Some(json!({"ignored": true}))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, body) = call(&k.app, "GET", "/admin/api/unmatched", None).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert_eq!(body["total"], 1);
    let (_, body) = call(&k.app, "GET", "/admin/api/unmatched?ignored=true", None).await;
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    let (status, _) = call(
        &k.app,
        "POST",
        "/admin/api/unmatched/9999/ignore",
        Some(json!({"ignored": true})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_pin_and_an_unpin_wake_the_matcher() {
    let k = kit("").await;
    let m = k.db.insert_mapping(&show()).await.unwrap().id;
    assert!(!woke(&k.wake).await);

    let (status, body) = call(
        &k.app,
        "POST",
        "/admin/api/pins",
        Some(json!({"mapping_id": m, "video_id": "0eD-GQCutAU", "season": 1, "episode": 3})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["season"], 1);
    assert_eq!(body["episode"], 3);
    assert!(woke(&k.wake).await);

    let (_, pins) = call(
        &k.app,
        "GET",
        &format!("/admin/api/pins?mapping_id={m}"),
        None,
    )
    .await;
    assert_eq!(pins.as_array().unwrap().len(), 1);
    assert_eq!(pins[0]["video_id"], "0eD-GQCutAU");

    let id = body["id"].as_i64().unwrap();
    let (status, _) = call(&k.app, "DELETE", &format!("/admin/api/pins/{id}"), None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(woke(&k.wake).await);
    let (status, _) = call(&k.app, "DELETE", &format!("/admin/api/pins/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!woke(&k.wake).await);
}

#[tokio::test]
async fn pins_must_fit_the_mapping() {
    let k = kit("").await;
    let show = k.db.insert_mapping(&show()).await.unwrap().id;
    let film = k.db.insert_mapping(&film()).await.unwrap().id;
    let refused = [
        json!({"mapping_id": show, "video_id": "0eD-GQCutAU"}),
        json!({"mapping_id": show, "video_id": "not a video", "season": 1, "episode": 1}),
        json!({"mapping_id": film, "video_id": "ldmPWRb-XcM", "air_date": "2026-10-14"}),
    ];
    for body in refused {
        let (status, _) = call(&k.app, "POST", "/admin/api/pins", Some(body.clone())).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    }
    assert!(!woke(&k.wake).await);
    let (status, _) = call(
        &k.app,
        "POST",
        "/admin/api/pins",
        Some(json!({"mapping_id": 999, "video_id": "ldmPWRb-XcM"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, body) = call(
        &k.app,
        "POST",
        "/admin/api/pins",
        Some(json!({"mapping_id": film, "video_id": "ldmPWRb-XcM"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["season"].is_null() && body["air_date"].is_null());
}

#[tokio::test]
async fn activity_shows_grabs_searches_and_play_starts() {
    let k = kit("").await;
    k.db.insert_grab(&NewGrab {
        nzo_id: "SABnzbd_nzo_1".to_owned(),
        release_name: "Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR"
            .to_owned(),
        video_id: "0eD-GQCutAU".to_owned(),
        category: "tv".to_owned(),
        status: GrabStatus::Completed,
        storage_path: "/downloads/tubarr/tv/x".to_owned(),
        fail_message: None,
    })
    .await
    .unwrap();

    let search = Request::get(format!(
        "/api?t=tvsearch&apikey={KEY}&tvdbid=452001&season=1&ep=1"
    ))
    .body(Body::empty())
    .unwrap();
    assert_eq!(
        k.app.clone().oneshot(search).await.unwrap().status(),
        StatusCode::OK
    );
    // A loopback peer, which the default play_allowed_networks lets through.
    let peer: std::net::SocketAddr = "127.0.0.1:40000".parse().unwrap();
    let mut play = Request::get("/play/not-a-video-id")
        .header("user-agent", "Jellyfin-Server/10.11.11")
        .body(Body::empty())
        .unwrap();
    play.extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    let played = k.app.clone().oneshot(play).await.unwrap().status();
    let mut ranged = Request::get("/play/not-a-video-id")
        .header("range", "bytes=5000-")
        .body(Body::empty())
        .unwrap();
    ranged
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    k.app.clone().oneshot(ranged).await.unwrap();

    let (status, body) = call(&k.app, "GET", "/admin/api/activity", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["grabs"][0]["status"], "completed");
    assert_eq!(body["grabs"][0]["storage_path"], "/downloads/tubarr/tv/x");
    assert!(body.get("events").is_none());
    let (status, body) = call(&k.app, "GET", "/admin/api/activity/events", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 2);
    assert_eq!(body["retention_secs"], 30 * 86_400);
    let events = body["items"].as_array().unwrap();
    assert_eq!(events.len(), 2, "{events:?}");
    assert_eq!(events[0]["kind"], "play");
    assert_eq!(events[0]["video_ids"], "not-a-video-id");
    assert_eq!(events[0]["client"], "Jellyfin-Server/10.11.11");
    assert_eq!(events[0]["status"], played.as_u16());
    assert_eq!(events[1]["kind"], "search");
    assert_eq!(events[1]["function"], "tvsearch");
    assert_eq!(events[1]["query"], "tvdbid=452001 season=1 ep=1");
    assert_eq!(events[1]["results"], 0);
    assert!(!body.to_string().contains(KEY));
    assert!(!body.to_string().contains("127.0.0.1"), "no peer address");

    let (_, body) = call(&k.app, "GET", "/admin/api/activity?category=movies", None).await;
    assert!(body["grabs"].as_array().unwrap().is_empty());
    assert_eq!(body["total"], 0);
}

/// `count` notes on one show: video ids `vid00000000` up, titles "Episode 0" up, every fifth
/// note unsure and the rest pending. All share one `updated_at`, so the id breaks the tie.
async fn many_notes(k: &Kit, count: usize) -> i64 {
    let m = k.db.insert_mapping(&show()).await.unwrap().id;
    let mut notes = Vec::new();
    for i in 0..count {
        let video_id = format!("vid{i:08}");
        k.db.upsert_seen_video(&SeenVideo {
            video_id: video_id.clone(),
            channel_id: ARY.to_owned(),
            title: format!("Episode {i}"),
            published_at: None,
            published_approx: false,
            duration_secs: None,
            source: VideoSource::Rss,
        })
        .await
        .unwrap();
        let state = if i % 5 == 0 {
            UnmatchedState::Unsure
        } else {
            UnmatchedState::Pending
        };
        notes.push(note(&video_id, state));
    }
    k.db.replace_unmatched(m, &notes).await.unwrap();
    m
}

fn ids(body: &Value) -> Vec<i64> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn unmatched_pages_in_a_stable_order() {
    let k = kit("").await;
    many_notes(&k, 60).await;

    let (status, first) = call(&k.app, "GET", "/admin/api/unmatched", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(first["total"], 60);
    let first = ids(&first);
    assert_eq!(first.len(), 25, "default limit");
    assert!(
        first.windows(2).all(|w| w[0] > w[1]),
        "newest id first on a tie"
    );

    let mut seen = first.clone();
    for offset in [25, 50] {
        let path = format!("/admin/api/unmatched?limit=25&offset={offset}");
        let (_, body) = call(&k.app, "GET", &path, None).await;
        assert_eq!(body["total"], 60);
        seen.extend(ids(&body));
    }
    assert_eq!(seen.len(), 60);
    let mut sorted = seen.clone();
    sorted.sort_unstable_by(|a, b| b.cmp(a));
    sorted.dedup();
    assert_eq!(seen, sorted, "pages neither overlap nor skip");

    let (_, again) = call(&k.app, "GET", "/admin/api/unmatched", None).await;
    assert_eq!(ids(&again), first);
    let (_, past) = call(&k.app, "GET", "/admin/api/unmatched?offset=100", None).await;
    assert!(ids(&past).is_empty());
    assert_eq!(past["total"], 60);

    for bad in ["limit=0", "limit=101", "offset=-1", "limit=x"] {
        let (status, _) = call(&k.app, "GET", &format!("/admin/api/unmatched?{bad}"), None).await;
        assert!(status.is_client_error(), "{bad}: {status}");
    }
}

#[tokio::test]
async fn unmatched_total_and_search_follow_the_filters() {
    let k = kit("").await;
    let m = many_notes(&k, 30).await;
    k.db.upsert_seen_video(&SeenVideo {
        video_id: "vid00000001".to_owned(),
        channel_id: ARY.to_owned(),
        title: "Kabhi 100% Sach".to_owned(),
        published_at: None,
        published_approx: false,
        duration_secs: None,
        source: VideoSource::Rss,
    })
    .await
    .unwrap();

    let (_, body) = call(
        &k.app,
        "GET",
        "/admin/api/unmatched?state=unsure&limit=2",
        None,
    )
    .await;
    assert_eq!(body["total"], 6);
    assert_eq!(ids(&body).len(), 2);
    let (_, body) = call(
        &k.app,
        "GET",
        &format!("/admin/api/unmatched?mapping_id={}&state=pending", m + 1),
        None,
    )
    .await;
    assert_eq!(body["total"], 0);

    // Note 1 was renamed, so "Episode 1" matches 10 to 19: 10 notes, 2 of them unsure.
    let (_, body) = call(&k.app, "GET", "/admin/api/unmatched?q=episode%201", None).await;
    assert_eq!(body["total"], 10);
    let (_, body) = call(
        &k.app,
        "GET",
        "/admin/api/unmatched?q=EPISODE%201&state=unsure",
        None,
    )
    .await;
    assert_eq!(body["total"], 2);

    // A literal %, not a wildcard: only the one title with "100%" in it.
    let (_, body) = call(&k.app, "GET", "/admin/api/unmatched?q=100%25", None).await;
    assert_eq!(body["total"], 1);
    assert_eq!(body["items"][0]["video_title"], "Kabhi 100% Sach");
    let (_, body) = call(&k.app, "GET", "/admin/api/unmatched?q=%25", None).await;
    assert_eq!(body["total"], 1);
    // Nor is _: as a wildcard it would match "Episode 1".
    let (_, body) = call(&k.app, "GET", "/admin/api/unmatched?q=episode_1", None).await;
    assert_eq!(body["total"], 0);

    // The show title, a video id and the channel id match too; a blank q is no filter.
    let (_, body) = call(&k.app, "GET", "/admin/api/unmatched?q=kabhi%20main", None).await;
    assert_eq!(body["total"], 30);
    let (_, body) = call(&k.app, "GET", "/admin/api/unmatched?q=vid00000029", None).await;
    assert_eq!(body["total"], 1);
    let path = format!("/admin/api/unmatched?q={ARY}&limit=100");
    let (_, body) = call(&k.app, "GET", &path, None).await;
    assert_eq!(body["total"], 30);
    assert_eq!(ids(&body).len(), 30);
    let (_, body) = call(&k.app, "GET", "/admin/api/unmatched?q=%20%20", None).await;
    assert_eq!(body["total"], 30);
}

#[tokio::test]
async fn activity_pages_grabs_within_a_category() {
    let k = kit("").await;
    for i in 0..40 {
        let category = if i % 4 == 0 { "movies" } else { "tv" };
        k.db.insert_grab(&NewGrab {
            nzo_id: format!("SABnzbd_nzo_{i:02}"),
            release_name: format!("Kabhi.Main.Kabhi.Tum.S01E{i:02}.1080p.WEB-DL-TUBARR"),
            video_id: "0eD-GQCutAU".to_owned(),
            category: category.to_owned(),
            status: GrabStatus::Completed,
            storage_path: format!("/downloads/tubarr/{category}/{i}"),
            fail_message: None,
        })
        .await
        .unwrap();
    }
    let names = |body: &Value| -> Vec<String> {
        body["grabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["nzo_id"].as_str().unwrap().to_owned())
            .collect()
    };

    let (status, body) = call(&k.app, "GET", "/admin/api/activity", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 40);
    let first = names(&body);
    assert_eq!(first.len(), 25, "default limit");
    assert_eq!(
        first[0], "SABnzbd_nzo_39",
        "newest first, rowid breaks the tie"
    );

    let (_, body) = call(&k.app, "GET", "/admin/api/activity?offset=25", None).await;
    let rest = names(&body);
    assert_eq!(rest.len(), 15);
    assert_eq!(rest.last().unwrap(), "SABnzbd_nzo_00");
    assert!(rest.iter().all(|n| !first.contains(n)));

    let (_, body) = call(
        &k.app,
        "GET",
        "/admin/api/activity?category=movies&limit=4&offset=4",
        None,
    )
    .await;
    assert_eq!(body["total"], 10);
    assert_eq!(
        names(&body),
        [
            "SABnzbd_nzo_20",
            "SABnzbd_nzo_16",
            "SABnzbd_nzo_12",
            "SABnzbd_nzo_08"
        ]
    );
    let (status, _) = call(&k.app, "GET", "/admin/api/activity?limit=500", None).await;
    assert!(status.is_client_error());
}

#[tokio::test]
async fn activity_events_page_by_kind_and_survive_a_restart() {
    let k = kit("").await;
    for i in 0..30 {
        let search = Request::get(format!("/api?t=tvsearch&apikey={KEY}&tvdbid=452001&ep={i}"))
            .body(Body::empty())
            .unwrap();
        k.app.clone().oneshot(search).await.unwrap();
    }
    let peer: std::net::SocketAddr = "127.0.0.1:40000".parse().unwrap();
    let mut play = Request::get("/play/not-a-video-id")
        .body(Body::empty())
        .unwrap();
    play.extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    k.app.clone().oneshot(play).await.unwrap();

    let (_, body) = call(&k.app, "GET", "/admin/api/activity/events", None).await;
    assert_eq!(body["total"], 31);
    assert_eq!(body["items"].as_array().unwrap().len(), 25, "default limit");
    assert_eq!(body["items"][0]["kind"], "play");

    // A new state on the same database, as after a restart.
    let config = Config::from_toml(&format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\napi_key_file = {:?}\n\
         [binaries]\nyt_dlp = \"/nonexistent/yt-dlp\"\ndeno = \"/nonexistent/deno\"\n",
        k._dir.path(),
        k._dir.path().join("key"),
    ))
    .unwrap();
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let restarted = tubarr::app(AppState::new(config, k.db.clone(), binaries).unwrap());
    let (_, body) = call(
        &restarted,
        "GET",
        "/admin/api/activity/events?kind=search&limit=4&offset=26",
        None,
    )
    .await;
    assert_eq!(body["total"], 30);
    let queries: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["query"].as_str().unwrap())
        .collect();
    assert_eq!(
        queries,
        [
            "tvdbid=452001 ep=3",
            "tvdbid=452001 ep=2",
            "tvdbid=452001 ep=1",
            "tvdbid=452001 ep=0"
        ]
    );
    let (status, _) = call(
        &restarted,
        "GET",
        "/admin/api/activity/events?offset=-1",
        None,
    )
    .await;
    assert!(status.is_client_error());
}

#[tokio::test]
async fn settings_never_carry_secret_values() {
    let dir = tempfile::tempdir().unwrap();
    let sonarr_key = dir.path().join("sonarr");
    let webhook = dir.path().join("webhook");
    std::fs::write(&sonarr_key, SONARR_KEY).unwrap();
    std::fs::write(&webhook, WEBHOOK).unwrap();
    let k = kit(&format!(
        "[sonarr]\nurl = \"http://sonarr:8989\"\napi_key_file = {sonarr_key:?}\n\
         [notify]\ndiscord_webhook_file = {webhook:?}\n[films]\nchannels = [\"{ARY}\"]\n"
    ))
    .await;
    let (status, body) = call(&k.app, "GET", "/admin/api/settings", None).await;
    assert_eq!(status, StatusCode::OK);
    let text = body.to_string();
    for secret in [KEY, SONARR_KEY, WEBHOOK, "secret-webhook-token"] {
        assert!(!text.contains(secret), "settings leak {secret}");
    }
    assert_eq!(body["api_key"], "set");
    assert_eq!(body["sonarr"]["api_key"], "set");
    assert_eq!(body["sonarr"]["url"], "http://sonarr:8989/");
    assert!(body["radarr"].is_null());
    assert_eq!(body["notify"]["webhook"], "set");
    assert_eq!(body["films"]["channels"], json!([ARY]));
    assert_eq!(body["fallback"]["cookies_file"], "not set");
    assert_eq!(body["auth"]["password"], true);
    assert_eq!(body["auth"]["oidc"], false);
    assert_eq!(body["auth"]["local_bypass"], false);
    assert!(body["auth"]["oidc_issuer"].is_null());
}

/// A Sonarr that knows one tag.
async fn fake_sonarr() -> String {
    let app = Router::new().route(
        "/api/v3/tag",
        get(|| async { axum::Json(json!([{"id": 7, "label": "youtube"}])) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn arr_test_looks_up_the_tag() {
    let url = fake_sonarr().await;
    let dir = tempfile::tempdir().unwrap();
    let sonarr_key = dir.path().join("sonarr");
    std::fs::write(&sonarr_key, SONARR_KEY).unwrap();
    let k = kit(&format!(
        "[sonarr]\nurl = \"{url}\"\napi_key_file = {sonarr_key:?}\n"
    ))
    .await;
    let (status, body) = call(&k.app, "POST", "/admin/api/arr/sonarr/test", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"ok": true, "tag": "youtube", "tag_id": 7}));
    let (status, _) = call(&k.app, "POST", "/admin/api/arr/radarr/test", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = call(&k.app, "POST", "/admin/api/arr/lidarr/test", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn binaries_check_and_rollback() {
    let k = kit("").await;
    // Both binaries have explicit paths, so nothing is managed and GitHub is not asked.
    let (status, body) = call(&k.app, "POST", "/admin/api/binaries/check", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));
    let (status, body) = call(&k.app, "POST", "/admin/api/binaries/yt-dlp/rollback", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"tool": "yt-dlp", "rolled_back": false}));
    let (status, _) = call(&k.app, "POST", "/admin/api/binaries/ffmpeg/rollback", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn aliases_for_films_and_shows_wake_the_matcher() {
    let k = kit("").await;
    let film = k.db.insert_mapping(&film()).await.unwrap().id;
    let show = k.db.insert_mapping(&show()).await.unwrap().id;
    let path = format!("/admin/api/mappings/{film}/aliases");
    let (status, body) = call(&k.app, "GET", &path, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));
    let (status, body) = call(
        &k.app,
        "PUT",
        &path,
        Some(json!({"aliases": ["Policeodu", " Theri ", ""]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body,
        json!([
            {"alias": "Policeodu", "source": "user"},
            {"alias": "Theri", "source": "user"},
        ])
    );
    assert!(woke(&k.wake).await);
    // Shows take aliases too, for YouTube titles that differ from TVDB's.
    let path = format!("/admin/api/mappings/{show}/aliases");
    let (status, body) = call(&k.app, "PUT", &path, Some(json!({"aliases": ["Dastaan"]}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([{"alias": "Dastaan", "source": "user"}]));
    let (status, _) = call(&k.app, "GET", "/admin/api/mappings/9999/aliases", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn health_reports_the_matcher() {
    let k = kit("").await;
    let (status, body) = call(&k.app, "GET", "/api/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["matcher"]["counts"]["pending"], 0);
    assert!(body["matcher"]["films"]["radarr_error"].is_null());
}
