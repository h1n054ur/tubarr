//! Sonarr and Radarr connections saved from the Settings page, and the wiring check, against a
//! small fake arr.

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
use tubarr::{
    AppState,
    arr::{App, ArrSync, connections},
    binaries::BinaryManager,
    config::{Config, ConnectionSource},
    store::Db,
};

const KEY: &str = "test-admin-key";
const ARR_KEY: &str = "arr-secret-key-0123456789";
const FILE_KEY: &str = "file-secret-key-9876543210";

/// What the fake arr answers. Tests change it between calls.
struct FakeState {
    app_name: &'static str,
    tags: Value,
    indexers: Value,
    clients: Value,
    media: Value,
    notifications: Value,
    series: Value,
    calls: usize,
    /// Calls to the series list, which only the arr sync makes.
    series_calls: usize,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<FakeState>>);

impl Fake {
    /// An arr with everything Tubarr needs in place.
    fn wired(app_name: &'static str) -> Self {
        Self(Arc::new(Mutex::new(FakeState {
            app_name,
            tags: json!([{"id": 3, "label": "anime"}, {"id": 7, "label": "youtube"}]),
            indexers: json!([{
                "name": "Tubarr", "implementation": "Newznab", "enableRss": true, "tags": [7],
                "fields": [{"name": "baseUrl", "value": "http://tubarr:8480"}, {"name": "apiPath", "value": "/api"}]
            }]),
            clients: json!([{
                "name": "tubarr", "implementation": "Sabnzbd", "enable": true, "protocol": "usenet",
                "tags": [7], "fields": []
            }]),
            media: json!({"id": 1, "importExtraFiles": true, "extraFileExtensions": "nfo, .srt"}),
            notifications: json!([{
                "name": "Tubarr", "implementation": "Webhook", "tags": [7],
                "fields": [{"name": "url", "value": "http://tubarr:8480/hooks/sonarr"}]
            }]),
            series: json!([]),
            calls: 0,
            series_calls: 0,
        })))
    }

    /// A wired Sonarr with one series tagged for Tubarr, titled `title`.
    fn with_series(title: &str) -> Self {
        let fake = Self::wired("Sonarr");
        fake.edit(|st| {
            st.series = json!([{
                "id": 1, "title": title, "year": 2024, "tvdbId": 4242,
                "seriesType": "standard", "monitored": true, "tags": [7]
            }]);
        });
        fake
    }

    fn series_calls(&self) -> usize {
        self.0.lock().unwrap().series_calls
    }

    fn edit(&self, f: impl FnOnce(&mut FakeState)) {
        f(&mut self.0.lock().unwrap());
    }

    fn calls(&self) -> usize {
        self.0.lock().unwrap().calls
    }

    async fn serve(&self) -> String {
        let route = |pick: fn(&FakeState) -> Value| {
            get(move |s: State<Fake>, h: HeaderMap| answer(s, h, pick))
        };
        let app = Router::new()
            .route(
                "/api/v3/system/status",
                route(|st| json!({"appName": st.app_name, "version": "4.0.20"})),
            )
            .route("/api/v3/tag", route(|st| st.tags.clone()))
            .route("/api/v3/indexer", route(|st| st.indexers.clone()))
            .route("/api/v3/downloadclient", route(|st| st.clients.clone()))
            .route(
                "/api/v3/config/mediamanagement",
                route(|st| st.media.clone()),
            )
            .route("/api/v3/notification", route(|st| st.notifications.clone()))
            .route(
                "/api/v3/series",
                get(|s: State<Fake>, h: HeaderMap| async move {
                    s.0.0.lock().unwrap().series_calls += 1;
                    answer(s, h, |st| st.series.clone()).await
                }),
            )
            .with_state(self.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }
}

async fn answer(
    State(fake): State<Fake>,
    headers: HeaderMap,
    pick: fn(&FakeState) -> Value,
) -> Response {
    let mut st = fake.0.lock().unwrap();
    st.calls += 1;
    if headers.get("x-api-key").and_then(|v| v.to_str().ok()) != Some(ARR_KEY) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(pick(&st)).into_response()
}

struct Kit {
    app: Router,
    db: Db,
    state: AppState,
    _dir: tempfile::TempDir,
}

/// A Tubarr whose config gets `extra`, with the saved connections already in `db` loaded the way
/// `main` loads them. `seed` runs on the database before the app is built.
async fn kit_with(extra: &str, seed: impl AsyncFnOnce(&Db)) -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("key");
    std::fs::write(&key_file, KEY).unwrap();
    let arr_key = dir.path().join("arr-key");
    std::fs::write(&arr_key, FILE_KEY).unwrap();
    let extra = extra.replace("ARR_KEY_FILE", &format!("{arr_key:?}"));
    let toml = format!(
        "public_url = \"http://tubarr:8480\"\ndata_dir = {:?}\napi_key_file = {key_file:?}\n\
         [binaries]\nyt_dlp = {:?}\ndeno = {:?}\n{extra}",
        dir.path(),
        dir.path().join("yt-dlp"),
        dir.path().join("deno"),
    );
    let config = Config::from_toml(&toml).unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    seed(&db).await;
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let state = AppState::new(config, db.clone(), binaries).unwrap();
    state.arrs.load_saved(&db).await.unwrap();
    Kit {
        app: tubarr::app(state.clone()),
        db,
        state,
        _dir: dir,
    }
}

async fn kit(extra: &str) -> Kit {
    kit_with(extra, async |_| {}).await
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("x-api-key", KEY)
        .header("x-requested-with", "tubarr");
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

fn no_secret(body: &Value) {
    let text = body.to_string();
    for secret in [ARR_KEY, FILE_KEY, KEY] {
        assert!(!text.contains(secret), "answer leaks a key: {text}");
    }
}

#[tokio::test]
async fn a_connection_is_tested_then_saved_and_the_key_never_comes_back() {
    let fake = Fake::wired("Sonarr");
    let url = fake.serve().await;
    let k = kit("").await;

    let (status, body) = call(&k.app, "GET", "/admin/api/arr/connections", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["sonarr"]["source"].is_null());
    assert_eq!(body["sonarr"]["api_key"], "not set");

    let (status, body) = call(
        &k.app,
        "PUT",
        "/admin/api/arr/sonarr/connection",
        Some(json!({"url": url, "api_key": format!("  {ARR_KEY}\n")})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    no_secret(&body);
    assert_eq!(body["source"], "saved");
    assert_eq!(body["api_key"], "set");
    assert_eq!(body["version"], "4.0.20");
    // In use at once, no restart.
    assert_eq!(
        k.state.arrs.sonarr().unwrap().url.as_str(),
        format!("{url}/")
    );
    let saved = k.db.saved_arr("sonarr").await.unwrap().unwrap();
    assert_eq!(saved.api_key, ARR_KEY);
    assert_eq!(saved.url, format!("{url}/"));
    assert!(!format!("{saved:?}").contains(ARR_KEY));

    let (_, body) = call(&k.app, "GET", "/admin/api/arr/connections", None).await;
    no_secret(&body);
    assert_eq!(body["sonarr"]["source"], "saved");

    let (status, body) = call(&k.app, "POST", "/admin/api/arr/sonarr/test", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!({"ok": true, "tag": "youtube", "tag_id": 7}));

    // A new URL without a key keeps the saved key.
    let (status, body) = call(
        &k.app,
        "PUT",
        "/admin/api/arr/sonarr/connection",
        Some(json!({"url": format!("{url}/")})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        k.db.saved_arr("sonarr").await.unwrap().unwrap().api_key,
        ARR_KEY
    );

    let (status, body) = call(&k.app, "DELETE", "/admin/api/arr/sonarr/connection", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["source"].is_null());
    assert!(k.db.saved_arr("sonarr").await.unwrap().is_none());
    assert!(k.state.arrs.sonarr().is_none());
    let (status, _) = call(&k.app, "DELETE", "/admin/api/arr/sonarr/connection", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn nothing_is_saved_when_the_test_fails() {
    let fake = Fake::wired("Radarr");
    let url = fake.serve().await;
    let k = kit("").await;

    let (status, body) = call(
        &k.app,
        "PUT",
        "/admin/api/arr/radarr/connection",
        Some(json!({"url": url, "api_key": "wrong-key"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    let error = body["error"].as_str().unwrap();
    assert!(
        error.starts_with("Not saved: Radarr refused the API key"),
        "{error}"
    );
    assert!(!error.contains("wrong-key"));
    assert!(k.db.saved_arr("radarr").await.unwrap().is_none());

    // A Radarr address in the Sonarr slot is refused.
    let (status, body) = call(
        &k.app,
        "PUT",
        "/admin/api/arr/sonarr/connection",
        Some(json!({"url": url, "api_key": ARR_KEY})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        body["error"],
        "Not saved: that address answers as Radarr, not Sonarr"
    );
    assert!(k.db.saved_arr("sonarr").await.unwrap().is_none());

    for (body, want) in [
        (
            json!({"url": "ftp://radarr", "api_key": ARR_KEY}),
            "The URL must start with http:// or https://",
        ),
        (
            json!({"url": "radarr:7878", "api_key": ARR_KEY}),
            "The URL must start with http:// or https://",
        ),
        (
            json!({"url": "not a url", "api_key": ARR_KEY}),
            "The URL is not valid, e.g. http://sonarr:8989",
        ),
        (
            json!({"url": url, "api_key": "  "}),
            "Enter the API key from Radarr, Settings > General",
        ),
    ] {
        let calls = fake.calls();
        let (status, answer) = call(
            &k.app,
            "PUT",
            "/admin/api/arr/radarr/connection",
            Some(body),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(answer["error"], want);
        assert_eq!(fake.calls(), calls, "a refused form never reaches the arr");
    }
    let (status, _) = call(
        &k.app,
        "PUT",
        "/admin/api/arr/lidarr/connection",
        Some(json!({"url": url})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_config_file_wins_and_stays_read_only() {
    let fake = Fake::wired("Sonarr");
    let url = fake.serve().await;
    let extra = "[sonarr]\nurl = \"http://sonarr-from-file:8989\"\napi_key_file = ARR_KEY_FILE\n";
    let k = kit_with(extra, async |db| {
        let saved = tubarr::store::SavedArr {
            url: format!("{url}/"),
            api_key: ARR_KEY.to_owned(),
        };
        db.save_arr("sonarr", &saved).await.unwrap();
        db.save_arr("radarr", &saved).await.unwrap();
    })
    .await;

    // Sonarr from the file, Radarr from the database.
    let sonarr = k.state.arrs.sonarr().unwrap();
    assert_eq!(sonarr.url.as_str(), "http://sonarr-from-file:8989/");
    assert_eq!(sonarr.api_key.expose(), FILE_KEY);
    assert_eq!(
        k.state.arrs.radarr().unwrap().url.as_str(),
        format!("{url}/")
    );

    let (_, body) = call(&k.app, "GET", "/admin/api/arr/connections", None).await;
    no_secret(&body);
    assert_eq!(body["sonarr"]["source"], "config");
    assert_eq!(body["sonarr"]["url"], "http://sonarr-from-file:8989/");
    assert_eq!(body["radarr"]["source"], "saved");

    for method in ["PUT", "DELETE"] {
        let (status, body) = call(
            &k.app,
            method,
            "/admin/api/arr/sonarr/connection",
            Some(json!({"url": url, "api_key": ARR_KEY})),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body["error"],
            "Sonarr is set in the config file ([sonarr]): change it there"
        );
    }
    let saved = k.db.saved_arr("sonarr").await.unwrap().unwrap();
    assert_eq!(saved.url, format!("{url}/"), "the saved row is left alone");

    // Nothing running can swap a connection from the config file either.
    let other = connections::saved_connection(&tubarr::store::SavedArr {
        url: format!("{url}/"),
        api_key: ARR_KEY.to_owned(),
    })
    .unwrap();
    assert!(!k.state.arrs.set(App::Sonarr, Some(other)));
    assert!(!k.state.arrs.set(App::Sonarr, None));
    let sonarr = k.state.arrs.sonarr().unwrap();
    assert_eq!(sonarr.url.as_str(), "http://sonarr-from-file:8989/");
    assert_eq!(sonarr.source, ConnectionSource::Config);
}

fn sync(k: &Kit) -> ArrSync {
    ArrSync::new(&k.state.config, k.state.arrs.clone(), k.db.clone()).unwrap()
}

async fn active_titles(db: &Db) -> Vec<String> {
    db.mappings()
        .await
        .unwrap()
        .into_iter()
        .filter(|m| m.active)
        .map(|m| m.title)
        .collect()
}

async fn save_sonarr(k: &Kit, url: &str) {
    let (status, body) = call(
        &k.app,
        "PUT",
        "/admin/api/arr/sonarr/connection",
        Some(json!({"url": url, "api_key": ARR_KEY})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn the_arr_sync_follows_connections_saved_changed_and_removed() {
    let first = Fake::with_series("Dar E Nijaat");
    let second = Fake::with_series("Tere Bin");
    let (first_url, second_url) = (first.serve().await, second.serve().await);
    let k = kit("").await;
    let sync = sync(&k);

    // No arr at start: the sync has nothing to read.
    sync.sync_once().await;
    assert!(active_titles(&k.db).await.is_empty());

    save_sonarr(&k, &first_url).await;
    sync.sync_once().await;
    assert_eq!(first.series_calls(), 1);
    assert_eq!(active_titles(&k.db).await, ["Dar E Nijaat"]);

    // A new URL: the next round asks the new arr only.
    save_sonarr(&k, &second_url).await;
    sync.sync_once().await;
    assert_eq!(first.series_calls(), 1);
    assert_eq!(second.series_calls(), 1);
    assert!(active_titles(&k.db).await.contains(&"Tere Bin".to_owned()));

    // Removed: no arr is asked any more.
    let (status, _) = call(&k.app, "DELETE", "/admin/api/arr/sonarr/connection", None).await;
    assert_eq!(status, StatusCode::OK);
    let (first_calls, second_calls) = (first.calls(), second.calls());
    sync.sync_once().await;
    assert_eq!((first.calls(), second.calls()), (first_calls, second_calls));
}

#[tokio::test]
async fn saving_a_connection_wakes_the_idle_sync_loop() {
    let fake = Fake::with_series("Dar E Nijaat");
    let url = fake.serve().await;
    // The interval is far longer than the test: only the save can start this sync.
    let k = kit("[arr]\nsync_interval = \"6h\"\n").await;
    let running = tokio::spawn(sync(&k).run());
    tokio::task::yield_now().await;

    save_sonarr(&k, &url).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while fake.series_calls() == 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the sync did not start after the save");
    running.abort();
}

fn states(body: &Value) -> Vec<(String, String)> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["key"].as_str().unwrap().to_owned(),
                i["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn detail(body: &Value, key: &str) -> String {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == key)
        .unwrap()["detail"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn saved_kit(app: &str, url: &str, extra: &str) -> Kit {
    let app = app.to_owned();
    let url = url.to_owned();
    kit_with(extra, async move |db| {
        let saved = tubarr::store::SavedArr {
            url: format!("{url}/"),
            api_key: ARR_KEY.to_owned(),
        };
        db.save_arr(&app, &saved).await.unwrap();
    })
    .await
}

#[tokio::test]
async fn wiring_check_passes_on_a_wired_sonarr() {
    let fake = Fake::wired("Sonarr");
    let url = fake.serve().await;
    let k = saved_kit("sonarr", &url, "").await;
    let (status, body) = call(&k.app, "POST", "/admin/api/arr/sonarr/wiring", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
    let ok = |k: &str| (k.to_owned(), "ok".to_owned());
    assert_eq!(
        states(&body),
        vec![
            ok("tag"),
            ok("indexer"),
            ok("download_client"),
            ok("extra_files"),
            ok("webhook")
        ]
    );
    assert_eq!(
        detail(&body, "indexer"),
        "Found, pointing at http://tubarr:8480."
    );
}

#[tokio::test]
async fn wiring_check_says_what_is_missing() {
    let fake = Fake::wired("Sonarr");
    fake.edit(|st| {
        st.tags = json!([{"id": 3, "label": "anime"}]);
        st.indexers = json!([]);
        st.clients =
            json!([{"name": "Tubarr", "implementation": "Sabnzbd", "enable": false, "tags": []}]);
        st.media = json!({"importExtraFiles": true, "extraFileExtensions": "nfo"});
        st.notifications = json!([]);
    });
    let url = fake.serve().await;
    let k = saved_kit("sonarr", &url, "").await;
    let (_, body) = call(&k.app, "POST", "/admin/api/arr/sonarr/wiring", None).await;
    let missing = |k: &str| (k.to_owned(), "missing".to_owned());
    assert_eq!(
        states(&body),
        vec![
            missing("tag"),
            missing("indexer"),
            missing("download_client"),
            missing("extra_files"),
            ("webhook".to_owned(), "optional".to_owned()),
        ]
    );
    assert_eq!(
        detail(&body, "tag"),
        "Add the tag \"youtube\" in Sonarr under Settings > Tags, and put it on the series Tubarr should serve."
    );
    assert_eq!(
        detail(&body, "indexer"),
        "Add a Newznab indexer named \"Tubarr\" in Sonarr under Settings > Indexers: URL http://tubarr:8480/, API path /api, Tubarr's API key, tag \"youtube\"."
    );
    assert_eq!(
        detail(&body, "download_client"),
        "Turn on the \"Tubarr\" download client: every push is sent to it."
    );
    assert!(detail(&body, "extra_files").starts_with("Turn on Import Extra Files in Sonarr"));
    assert!(detail(&body, "webhook").starts_with("Needed only for audio only versions"));
    for item in body["items"].as_array().unwrap() {
        let text = item.to_string();
        assert!(
            !text.contains('\u{2013}') && !text.contains('\u{2014}'),
            "{text}"
        );
    }

    // With the tag in place but not on the indexer and client, each says so.
    fake.edit(|st| {
        st.tags = json!([{"id": 7, "label": "YouTube"}]);
        st.indexers = json!([{"name": "Tubarr", "implementation": "Newznab", "tags": []}]);
        st.clients =
            json!([{"name": "Tubarr", "implementation": "Sabnzbd", "enable": true, "tags": [3]}]);
    });
    let (_, body) = call(&k.app, "POST", "/admin/api/arr/sonarr/wiring", None).await;
    assert_eq!(
        detail(&body, "indexer"),
        "Put the tag \"youtube\" on the \"Tubarr\" indexer."
    );
    assert_eq!(
        detail(&body, "download_client"),
        "Put the tag \"youtube\" on the \"Tubarr\" download client."
    );
}

#[tokio::test]
async fn the_webhook_is_required_with_hooks_set_and_radarr_has_none() {
    let fake = Fake::wired("Sonarr");
    fake.edit(|st| st.notifications = json!([]));
    let url = fake.serve().await;
    let extra =
        "[hooks]\nsonarr_token_file = ARR_KEY_FILE\nlibrary_paths = [[\"/media\", \"/media\"]]\n";
    let k = saved_kit("sonarr", &url, extra).await;
    let (_, body) = call(&k.app, "POST", "/admin/api/arr/sonarr/wiring", None).await;
    assert_eq!(
        states(&body).last().unwrap(),
        &("webhook".to_owned(), "missing".to_owned())
    );
    assert!(detail(&body, "webhook").contains("URL http://tubarr:8480/hooks/sonarr"));

    let radarr = Fake::wired("Radarr");
    let url = radarr.serve().await;
    let k = saved_kit("radarr", &url, "").await;
    let (_, body) = call(&k.app, "POST", "/admin/api/arr/radarr/wiring", None).await;
    let keys: Vec<String> = states(&body).into_iter().map(|(k, _)| k).collect();
    assert_eq!(keys, ["tag", "indexer", "download_client", "extra_files"]);
}

#[tokio::test]
async fn wiring_check_reports_an_unreadable_arr_and_needs_a_connection() {
    let k = kit("").await;
    let (status, body) = call(&k.app, "POST", "/admin/api/arr/radarr/wiring", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "Radarr is not connected");

    let fake = Fake::wired("Radarr");
    let url = fake.serve().await;
    let k = kit_with("", async |db| {
        let saved = tubarr::store::SavedArr {
            url: format!("{url}/"),
            api_key: "stale-key".to_owned(),
        };
        db.save_arr("radarr", &saved).await.unwrap();
    })
    .await;
    let (status, body) = call(&k.app, "POST", "/admin/api/arr/radarr/wiring", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], false);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("refused the API key")
    );
    assert!(!body.to_string().contains("stale-key"));
}
