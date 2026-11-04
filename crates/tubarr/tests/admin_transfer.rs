//! Mapping export and import: one instance's mappings, aliases and pins carried to another.

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
    store::{Db, MappingKind, MatchOptions, NewMapping, PinTarget},
};

const KEY: &str = "test-admin-key";
const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";

struct Kit {
    app: axum::Router,
    db: Db,
    _dir: tempfile::TempDir,
}

async fn kit() -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("key");
    std::fs::write(&key_file, KEY).unwrap();
    let toml = format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\napi_key_file = {key_file:?}\n",
        dir.path()
    );
    let config = Config::from_toml(&toml).unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = std::sync::Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let app = tubarr::app(AppState::new(config, db.clone(), binaries).unwrap());
    Kit { app, db, _dir: dir }
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("x-api-key", KEY);
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

fn drama() -> NewMapping {
    NewMapping {
        kind: MappingKind::Show,
        tvdb_id: Some(452_001),
        tmdb_id: None,
        title: "Kabhi Main Kabhi Tum".to_owned(),
        language: "Urdu".to_owned(),
        channel_ids: vec![ARY.to_owned()],
        episode_regex: Some(r"Ep(?:isode)?\s*(?P<ep>\d+)".to_owned()),
        excludes: vec!["Teaser".to_owned()],
        min_duration_secs: Some(1200),
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

/// An instance with a drama (aliases, two pins, a time zone, a root folder) and a film (a pin).
async fn source() -> Kit {
    let k = kit().await;
    let show = k.db.insert_mapping(&drama()).await.unwrap();
    k.db.set_match_options(
        show.id,
        &MatchOptions {
            timezone: Some("America/New_York".to_owned()),
            ..MatchOptions::default()
        },
    )
    .await
    .unwrap();
    k.db.set_aliases(show.id, &["KMKT".to_owned(), "Kabhi Main".to_owned()])
        .await
        .unwrap();
    let ep = |episode| PinTarget::Episode { season: 1, episode };
    k.db.set_pin(show.id, &ep(1), "0eD-GQCutAU").await.unwrap();
    k.db.set_pin(show.id, &ep(2), "aaaaaaaaaaa,bbbbbbbbbbb")
        .await
        .unwrap();
    k.db.set_root_folder_override(show.id, Some("/data/media/pk-shows"))
        .await
        .unwrap();
    let movie = k.db.insert_mapping(&film()).await.unwrap();
    k.db.set_pin(movie.id, &PinTarget::Film, "ccccccccccc")
        .await
        .unwrap();
    k
}

fn titles(report: &Value, key: &str) -> Vec<String> {
    report[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn export_carries_rules_aliases_and_pins_without_instance_state() {
    let k = source().await;
    let req = Request::builder()
        .uri("/admin/api/mappings/export")
        .header("x-api-key", KEY)
        .body(Body::empty())
        .unwrap();
    let res = k.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert!(
        res.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains("tubarr-mappings.json")
    );

    let (_, file) = call(&k.app, "GET", "/admin/api/mappings/export", None).await;
    assert_eq!(file["format"], 1);
    let mappings = file["mappings"].as_array().unwrap();
    assert_eq!(mappings.len(), 2);
    let show = mappings.iter().find(|m| m["kind"] == "show").unwrap();
    assert_eq!(show["tvdb_id"], 452_001);
    assert_eq!(show["timezone"], "America/New_York");
    assert_eq!(show["root_folder"], "/data/media/pk-shows");
    assert_eq!(show["excludes"], json!(["Teaser"]));
    assert_eq!(
        show["aliases"],
        json!([
            {"alias": "KMKT", "source": "user"},
            {"alias": "Kabhi Main", "source": "user"},
        ])
    );
    assert_eq!(
        show["pins"],
        json!([
            {"season": 1, "episode": 1, "video_id": "0eD-GQCutAU"},
            {"season": 1, "episode": 2, "video_id": "aaaaaaaaaaa,bbbbbbbbbbb"},
        ])
    );
    for key in ["id", "arr_id", "active", "created_at", "updated_at"] {
        assert!(show.get(key).is_none(), "{key} is instance state");
    }
}

#[tokio::test]
async fn a_dry_run_writes_nothing_and_an_import_round_trips() {
    let from = source().await;
    let (_, file) = call(&from.app, "GET", "/admin/api/mappings/export", None).await;

    let to = kit().await;
    let (status, report) = call(
        &to.app,
        "POST",
        "/admin/api/mappings/import?dry_run=true",
        Some(file.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["dry_run"], true);
    assert_eq!(
        titles(&report, "creates"),
        ["Theri", "Kabhi Main Kabhi Tum"],
        "{report}"
    );
    assert!(
        to.db.mappings().await.unwrap().is_empty(),
        "a dry run writes nothing"
    );

    let (status, report) = call(
        &to.app,
        "POST",
        "/admin/api/mappings/import",
        Some(file.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["creates"].as_array().unwrap().len(), 2);
    assert!(report["conflicts"].as_array().unwrap().is_empty());

    let (_, again) = call(&to.app, "GET", "/admin/api/mappings/export", None).await;
    assert_eq!(again["mappings"], file["mappings"]);

    // The same file a second time changes nothing.
    let (_, report) = call(&to.app, "POST", "/admin/api/mappings/import", Some(file)).await;
    assert_eq!(report["unchanged"].as_array().unwrap().len(), 2);
    assert!(report["updates"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn an_import_updates_by_id_keeps_other_pins_and_skips_conflicts() {
    let k = kit().await;
    let show =
        k.db.insert_mapping(&NewMapping {
            language: "English".to_owned(),
            ..drama()
        })
        .await
        .unwrap();
    k.db.set_pin(
        show.id,
        &PinTarget::Episode {
            season: 1,
            episode: 9,
        },
        "ddddddddddd",
    )
    .await
    .unwrap();
    k.db.set_aliases(show.id, &["Old name".to_owned()])
        .await
        .unwrap();
    k.db.insert_mapping(&film()).await.unwrap();

    let file = json!({
        "format": 1,
        "exported_at": 0,
        "mappings": [
            {
                "kind": "show", "tvdb_id": 452_001, "title": "Kabhi Main Kabhi Tum",
                "language": "Urdu", "channel_ids": [ARY],
                "aliases": [{"alias": "KMKT"}],
                "pins": [{"season": 1, "episode": 3, "video_id": "0eD-GQCutAU"}],
            },
            // Theri is a film here.
            {"kind": "daily", "tvdb_id": 1, "tmdb_id": 352_173, "title": "Theri", "language": "Hindi"},
            {"kind": "show", "tvdb_id": 452_001, "title": "Twice", "language": "Urdu"},
            {"kind": "show", "tvdb_id": 7, "title": "Bad channel", "language": "Urdu", "channel_ids": ["nope"]},
            {"kind": "film", "tmdb_id": 99, "title": "New film", "language": "Hindi"},
        ],
    });
    let (status, dry) = call(
        &k.app,
        "POST",
        "/admin/api/mappings/import?dry_run=true",
        Some(file.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{dry}");
    assert_eq!(titles(&dry, "updates"), ["Kabhi Main Kabhi Tum"]);
    assert_eq!(titles(&dry, "creates"), ["New film"]);
    assert_eq!(titles(&dry, "conflicts"), ["Theri", "Twice", "Bad channel"]);
    let reasons: Vec<&str> = dry["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["reason"].as_str().unwrap())
        .collect();
    assert!(
        reasons[0].contains("belongs to another mapping"),
        "{reasons:?}"
    );
    assert!(reasons[1].contains("in the file twice"), "{reasons:?}");
    assert!(
        reasons[2].contains("not a YouTube channel id"),
        "{reasons:?}"
    );

    let (_, report) = call(&k.app, "POST", "/admin/api/mappings/import", Some(file)).await;
    assert_eq!(report["dry_run"], false);
    let updated = k.db.mapping_by_tvdb(452_001).await.unwrap().unwrap();
    assert_eq!(updated.id, show.id);
    assert_eq!(updated.language, "Urdu");
    assert_eq!(k.db.aliases(show.id).await.unwrap(), ["KMKT"]);
    let episodes: Vec<PinTarget> =
        k.db.pins(show.id)
            .await
            .unwrap()
            .into_iter()
            .map(|p| p.target)
            .collect();
    assert_eq!(
        episodes.len(),
        2,
        "the file's pin is added, the old one stays"
    );
    assert!(k.db.mapping_by_tmdb(99).await.unwrap().is_some());
    assert_eq!(k.db.mappings().await.unwrap().len(), 3);
}

#[tokio::test]
async fn a_file_from_a_newer_format_is_refused() {
    let k = kit().await;
    let (status, body) = call(
        &k.app,
        "POST",
        "/admin/api/mappings/import",
        Some(json!({"format": 2, "exported_at": 0, "mappings": []})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"].as_str().unwrap().contains("format 2"));
}
