//! Root folder routing against a fake Sonarr and Radarr that serve the arr fixtures, list root
//! folders and take the editor PUT (`series/editor`, `movie/editor` with `moveFiles`) like
//! Sonarr 4.0.20 and Radarr 6.4.4.

mod support;

use std::sync::{Arc, Mutex};

use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use tubarr::{AppState, arr::ArrSync, binaries::BinaryManager, config::Config, store::Db};

const KEY: &str = "0123456789abcdef0123456789abcdef";
const ADMIN_KEY: &str = "test-admin-key";
const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";
const GOLDMINES: &str = "UCyoXW-Dse7fURq30EWl_CUA";

struct FakeState {
    tags: Value,
    titles: Value,
    root_folders: Value,
    /// Bodies of every editor PUT, in order.
    moves: Vec<Value>,
    /// Whether a PUT changes the title's rootFolderPath, as the real arr does at once.
    apply_moves: bool,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<FakeState>>);

impl Fake {
    fn sonarr() -> Self {
        Self::new(
            "arr/api/sonarr/tag.json",
            "arr/api/sonarr/series.json",
            json!([
                { "id": 1, "path": "/data/media/tv/", "accessible": true },
                { "id": 2, "path": "/data/media/pk-dramas/", "accessible": true },
                { "id": 3, "path": "/data/media/in-dramas", "accessible": true },
            ]),
        )
    }

    fn radarr() -> Self {
        Self::new(
            "arr/api/radarr/tag.json",
            "arr/api/radarr/movie.json",
            json!([
                { "id": 1, "path": "/data/media/movies/", "accessible": true },
                { "id": 2, "path": "/data/media/in-films/", "accessible": true },
            ]),
        )
    }

    fn new(tags: &str, titles: &str, root_folders: Value) -> Self {
        Self(Arc::new(Mutex::new(FakeState {
            tags: support::fixture_json(tags),
            titles: support::fixture_json(titles),
            root_folders,
            moves: Vec::new(),
            apply_moves: true,
        })))
    }

    fn moves(&self) -> Vec<Value> {
        self.0.lock().unwrap().moves.clone()
    }

    fn edit(&self, f: impl FnOnce(&mut FakeState)) {
        f(&mut self.0.lock().unwrap());
    }

    async fn serve(&self) -> String {
        let read = |pick: fn(&FakeState) -> Value| {
            move |State(fake): State<Fake>, headers: HeaderMap| async move {
                if !key_ok(&headers) {
                    return StatusCode::UNAUTHORIZED.into_response();
                }
                Json(pick(&fake.0.lock().unwrap())).into_response()
            }
        };
        let app = Router::new()
            .route("/api/v3/tag", get(read(|s| s.tags.clone())))
            .route("/api/v3/series", get(read(|s| s.titles.clone())))
            .route("/api/v3/movie", get(read(|s| s.titles.clone())))
            .route("/api/v3/rootfolder", get(read(|s| s.root_folders.clone())))
            .route("/api/v3/series/editor", axum::routing::put(editor))
            .route("/api/v3/movie/editor", axum::routing::put(editor))
            .with_state(self.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }
}

fn key_ok(headers: &HeaderMap) -> bool {
    headers.get("x-api-key").and_then(|v| v.to_str().ok()) == Some(KEY)
}

/// The editor PUT: records the body and, like the arr, sets the new root folder on the titles.
async fn editor(State(fake): State<Fake>, headers: HeaderMap, Json(body): Json<Value>) -> Response {
    if !key_ok(&headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut st = fake.0.lock().unwrap();
    st.moves.push(body.clone());
    if st.apply_moves {
        let ids = body
            .get("seriesIds")
            .or_else(|| body.get("movieIds"))
            .cloned()
            .unwrap_or_default();
        for title in st.titles.as_array_mut().unwrap() {
            if ids.as_array().unwrap().contains(&title["id"]) {
                title["rootFolderPath"] = body["rootFolderPath"].clone();
            }
        }
    }
    (StatusCode::ACCEPTED, Json(json!([]))).into_response()
}

/// A config with Sonarr and Radarr at the given URLs, then `tables`.
fn config(dir: &tempfile::TempDir, sonarr: &str, radarr: &str, tables: &str) -> Config {
    let key_file = dir.path().join("key");
    std::fs::write(&key_file, KEY).unwrap();
    let admin_key = dir.path().join("admin_key");
    std::fs::write(&admin_key, ADMIN_KEY).unwrap();
    let mut toml = format!(
        "public_url = \"http://tubarr:8480\"\ndata_dir = {:?}\napi_key_file = {admin_key:?}\n",
        dir.path()
    );
    toml.push_str(tables);
    for (name, url) in [("sonarr", sonarr), ("radarr", radarr)] {
        toml.push_str(&format!(
            "\n[{name}]\nurl = \"{url}\"\napi_key_file = {key_file:?}\n"
        ));
    }
    Config::from_toml(&toml).unwrap()
}

struct Kit {
    sonarr: Fake,
    radarr: Fake,
    db: Db,
    sync: ArrSync,
    config: Config,
    dir: tempfile::TempDir,
}

async fn kit(tables: &str) -> Kit {
    let (sonarr, radarr) = (Fake::sonarr(), Fake::radarr());
    let dir = support::temp_dir();
    let config = config(&dir, &sonarr.serve().await, &radarr.serve().await, tables);
    let db = Db::open(&config.database_path()).await.unwrap();
    let sync = ArrSync::from_config(&config, db.clone()).unwrap();
    Kit {
        sonarr,
        radarr,
        db,
        sync,
        config,
        dir,
    }
}

impl Kit {
    async fn set_channels(&self, tvdb_or_tmdb: (&str, i64), channels: &[&str]) {
        let sql = format!(
            "UPDATE mappings SET channel_ids = ? WHERE {} = ?",
            tvdb_or_tmdb.0
        );
        sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(json!(channels).to_string())
            .bind(tvdb_or_tmdb.1)
            .execute(self.db.pool())
            .await
            .unwrap();
    }

    async fn state(&self, tvdb_or_tmdb: (&str, i64)) -> (Option<String>, Option<String>) {
        let sql = format!(
            "SELECT s.current, s.result FROM mappings m LEFT JOIN root_folder_state s \
             ON s.mapping_id = m.id WHERE m.{} = ?",
            tvdb_or_tmdb.0
        );
        sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(tvdb_or_tmdb.1)
            .fetch_one(self.db.pool())
            .await
            .unwrap()
    }
}

const DRAMA: (&str, i64) = ("tvdb_id", 452_001);
const DAILY: (&str, i64) = ("tvdb_id", 389_450);
const THERI: (&str, i64) = ("tmdb_id", 352_173);
const MERSAL: (&str, i64) = ("tmdb_id", 471_507);

#[tokio::test]
async fn a_title_moves_once_and_equal_folders_stay() {
    // The daily show is Unknown and already in /data/media/tv/: its wanted folder is equal.
    let tables = format!(
        "[channels.{ARY}]\nnetwork = \"ARY Digital\"\nlanguage = \"Urdu\"\n\
         root_folder = \"/data/media/pk-dramas\"\n\
         [root_folders.shows]\nunknown = \"/data/media/tv\"\n"
    );
    let k = kit(&tables).await;
    k.sync.sync_once().await;
    assert!(k.sonarr.moves().is_empty(), "nothing routed yet");

    k.set_channels(DRAMA, &[ARY]).await;
    k.sync.sync_once().await;
    assert_eq!(
        k.sonarr.moves(),
        [
            json!({ "seriesIds": [11], "rootFolderPath": "/data/media/pk-dramas/", "moveFiles": true })
        ],
        "the arr's own spelling of the folder"
    );
    assert_eq!(
        k.state(DRAMA).await,
        (
            Some("/data/media/pk-dramas/".to_owned()),
            Some("moved".to_owned())
        )
    );

    // Later syncs see the new folder: no second move, and the daily show never moved.
    k.sync.sync_once().await;
    k.sync.sync_once().await;
    assert_eq!(k.sonarr.moves().len(), 1);
    assert_eq!(k.state(DAILY).await.1, None);
    assert!(k.radarr.moves().is_empty());
}

#[tokio::test]
async fn a_move_is_asked_once_per_wanted_folder() {
    let k = kit("[root_folders.shows]\nUnknown = \"/data/media/pk-dramas\"\n").await;
    // The arr keeps the old folder (the user moved it back, or the move did not stick).
    k.sonarr.edit(|s| s.apply_moves = false);
    k.sync.sync_once().await;
    k.sync.sync_once().await;
    k.sync.sync_once().await;
    assert_eq!(k.sonarr.moves().len(), 1, "never in a loop");
    assert_eq!(k.sonarr.moves()[0]["seriesIds"], json!([12]));

    // A new wanted folder is asked for once more.
    let id: i64 = sqlx::query_scalar("SELECT id FROM mappings WHERE tvdb_id = ?")
        .bind(DAILY.1)
        .fetch_one(k.db.pool())
        .await
        .unwrap();
    k.db.set_root_folder_override(id, Some("/data/media/in-dramas"))
        .await
        .unwrap();
    k.sync.sync_once().await;
    k.sync.sync_once().await;
    let moves = k.sonarr.moves();
    assert_eq!(moves.len(), 2);
    assert_eq!(moves[1]["rootFolderPath"], "/data/media/in-dramas");
}

#[tokio::test]
async fn a_missing_root_folder_is_skipped_until_the_arr_has_it() {
    // Both tagged films are Tamil.
    let k = kit("[root_folders.films]\ntamil = \"/data/media/ta-films\"\n").await;
    k.sync.sync_once().await;
    assert!(
        k.radarr.moves().is_empty(),
        "Tubarr never creates root folders"
    );
    assert_eq!(k.state(THERI).await.1.as_deref(), Some("missing"));

    // Once the folder exists in Radarr the next sync moves the film.
    k.radarr.edit(|s| {
        s.root_folders
            .as_array_mut()
            .unwrap()
            .push(json!({ "id": 3, "path": "/data/media/ta-films", "accessible": true }));
    });
    k.sync.sync_once().await;
    assert_eq!(
        k.radarr.moves(),
        [7, 8].map(|id| json!({ "movieIds": [id], "rootFolderPath": "/data/media/ta-films", "moveFiles": true }))
    );
    assert_eq!(k.state(THERI).await.1.as_deref(), Some("moved"));
}

#[tokio::test]
async fn films_and_shows_take_their_own_channel_keys() {
    let tables = format!(
        "[channels.{GOLDMINES}]\nnetwork = \"Goldmines\"\ncountry = \"India\"\nlanguage = \"Hindi\"\n\
         root_folder = \"/data/media/movies\"\n\
         show_root_folder = \"/data/media/in-dramas\"\nfilm_root_folder = \"/data/media/in-films\"\n"
    );
    let k = kit(&tables).await;
    k.sync.sync_once().await;
    k.set_channels(DRAMA, &[GOLDMINES]).await;
    k.set_channels(MERSAL, &[GOLDMINES]).await;
    k.sync.sync_once().await;
    assert_eq!(
        k.sonarr.moves(),
        [
            json!({ "seriesIds": [11], "rootFolderPath": "/data/media/in-dramas", "moveFiles": true })
        ]
    );
    assert_eq!(
        k.radarr.moves(),
        [json!({ "movieIds": [8], "rootFolderPath": "/data/media/in-films/", "moveFiles": true })]
    );
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("x-api-key", ADMIN_KEY)
        .header("content-type", "application/json");
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn the_admin_override_wins_and_none_turns_routing_off() {
    let tables = format!(
        "[channels.{ARY}]\nnetwork = \"ARY Digital\"\nlanguage = \"Urdu\"\n\
         root_folder = \"/data/media/pk-dramas\"\n"
    );
    let k = kit(&tables).await;
    k.sync.sync_once().await;
    let binaries = Arc::new(BinaryManager::new(k.config.binaries.clone()).unwrap());
    let app = tubarr::app(AppState::new(k.config.clone(), k.db.clone(), binaries).unwrap());
    let id: i64 = sqlx::query_scalar("SELECT id FROM mappings WHERE tvdb_id = ?")
        .bind(DRAMA.1)
        .fetch_one(k.db.pool())
        .await
        .unwrap();
    let path = format!("/admin/api/mappings/{id}");

    // "none" first: saving the channel then routes nothing.
    let (status, body) = call(
        &app,
        "PATCH",
        &path,
        Some(json!({ "channel_ids": [ARY], "root_folder": "none" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["root_folder"], "none");
    assert_eq!(body["wanted_root_folder"], Value::Null);
    assert_eq!(body["current_root_folder"], "/data/media/tv/");
    assert!(k.sonarr.moves().is_empty());

    // Back to the config: the channel's folder, moved at once.
    let (_, body) = call(&app, "PATCH", &path, Some(json!({ "root_folder": null }))).await;
    assert_eq!(body["wanted_root_folder"], "/data/media/pk-dramas");
    assert_eq!(body["current_root_folder"], "/data/media/pk-dramas/");
    assert_eq!(body["root_folder_result"], "moved");

    // An override wins over the channel.
    let (_, body) = call(
        &app,
        "PATCH",
        &path,
        Some(json!({ "root_folder": "/data/media/in-dramas" })),
    )
    .await;
    assert_eq!(body["wanted_root_folder"], "/data/media/in-dramas");
    let moves = k.sonarr.moves();
    assert_eq!(moves.len(), 2);
    assert_eq!(moves[1]["rootFolderPath"], "/data/media/in-dramas");

    // A bad override is refused and changes nothing.
    let (status, _) = call(
        &app,
        "PATCH",
        &path,
        Some(json!({ "root_folder": "in-dramas" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // The list shows the same fields, and Settings shows the channel map's folders.
    let (_, list) = call(&app, "GET", "/admin/api/mappings", None).await;
    let drama = list
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id)
        .unwrap();
    assert_eq!(drama["root_folder"], "/data/media/in-dramas");
    let (_, settings) = call(&app, "GET", "/admin/api/settings", None).await;
    assert_eq!(
        settings["channels"][ARY]["root_folder"],
        "/data/media/pk-dramas"
    );
    assert_eq!(settings["root_folders"]["shows"], json!({}));
    drop(k.dir);
}
