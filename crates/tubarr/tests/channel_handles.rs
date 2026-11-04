//! Channels entered as handles and links in the admin API, resolved against a local fake YouTube.
//! Nothing here reaches YouTube.

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Body,
    extract::{Query, State},
    http::{Request, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use tubarr::{AppState, binaries::BinaryManager, config::Config, ratelimit::RateLimiter};

const KEY: &str = "test-admin-key";
const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";
const HUM: &str = "UCnHumHumHumHumHumHumHum";

fn channel_page(id: &str, title: &str) -> String {
    format!(
        r#"<html><head><link rel="canonical" href="https://www.youtube.com/channel/{id}">
        <meta property="og:title" content="{title}"></head>
        <body><script>var ytInitialData = {{"metadata":{{"channelMetadataRenderer":{{"externalId":"{id}"}}}}}};</script></body></html>"#
    )
}

/// Paths the fake site was asked for, with their query.
type Asked = Arc<Mutex<Vec<String>>>;

async fn site(
    State(asked): State<Asked>,
    uri: Uri,
    Query(q): Query<Vec<(String, String)>>,
) -> Response {
    asked.lock().unwrap().push(uri.to_string());
    let v = q.iter().find(|(k, _)| k == "v").map(|(_, v)| v.as_str());
    match (uri.path(), v) {
        ("/@arydigital" | "/@ARYDigital", _) => channel_page(ARY, "ARY Digital &amp; More").into_response(),
        ("/c/HumTV", _) => channel_page(HUM, "HUM TV").into_response(),
        ("/user/twochannels", _) => {
            format!("{}{{\"externalId\":\"{HUM}\"}}", channel_page(ARY, "Mixed")).into_response()
        }
        ("/@removed", _) => "<html><body>This channel is not available.</body></html>".into_response(),
        ("/@busy", _) => StatusCode::TOO_MANY_REQUESTS.into_response(),
        ("/watch", Some("0eD-GQCutAU")) => format!(
            r#"{{"responseContext":{{}},"videoDetails":{{"videoId":"0eD-GQCutAU","channelId":"{HUM}","author":"HUM TV"}}}}"#
        )
        .into_response(),
        ("/watch", Some(_)) => r#"{"playabilityStatus":{"status":"ERROR"}}"#.into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

struct Kit {
    app: Router,
    limiter: Arc<RateLimiter>,
    asked: Asked,
    _dir: tempfile::TempDir,
}

/// The lookup budget refills one token an hour from a bucket of five, so a test sees exactly how
/// many tokens it used.
async fn kit() -> Kit {
    let asked: Asked = Arc::default();
    let fake = Router::new().fallback(get(site)).with_state(asked.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, fake).await.unwrap() });

    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("key");
    std::fs::write(&key_file, KEY).unwrap();
    let toml = format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\napi_key_file = {key_file:?}\n\
         [watcher]\nfeed_url = \"http://{addr}/feeds/videos.xml\"\n\
         [rate_limit]\nlookups = 1\ninterval = \"1h\"\nburst = 5\njitter = \"0s\"\nthrottle_pause = \"15m\"\n",
        dir.path()
    );
    let config = Config::from_toml(&toml).unwrap();
    let db = tubarr::store::Db::open(&config.database_path())
        .await
        .unwrap();
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let state = AppState::new(config, db, binaries).unwrap();
    let limiter = state.limiter.clone();
    Kit {
        app: tubarr::app(state),
        limiter,
        asked,
        _dir: dir,
    }
}

impl Kit {
    async fn call(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let req = Request::builder()
            .method(method)
            .uri(path)
            .header("x-api-key", KEY)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn create(&self, channels: &[&str]) -> (StatusCode, Value) {
        self.call(
            "POST",
            "/admin/api/mappings",
            json!({
                "kind": "show",
                "title": "Ishq Murshid",
                "tvdb_id": 431_000,
                "language": "Urdu",
                "channel_ids": channels,
            }),
        )
        .await
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }

    fn tokens(&self) -> f64 {
        self.limiter.status().available
    }
}

#[tokio::test]
async fn every_form_resolves_to_the_channel_id_and_only_ids_are_stored() {
    let k = kit().await;
    let (status, made) = k
        .create(&[
            ARY,
            "@arydigital",
            "https://www.youtube.com/@ARYDigital/videos",
            &format!("https://www.youtube.com/channel/{ARY}"),
            "youtube.com/c/HumTV",
            "https://www.youtube.com/watch?v=0eD-GQCutAU",
        ])
        .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["channel_ids"], json!([ARY, HUM]));
    assert_eq!(
        made["channel_titles"],
        json!({ ARY: "ARY Digital & More", HUM: "HUM TV" })
    );
    // The id and the /channel/ link are read as typed. "@arydigital" and "@ARYDigital" are the
    // same handle, so the second one comes from the cache.
    assert_eq!(
        k.asked(),
        vec!["/@arydigital", "/c/HumTV", "/watch?v=0eD-GQCutAU"]
    );
    assert!((5.0 - k.tokens() - 3.0).abs() < 0.01, "{}", k.tokens());

    // The names stay with the ids for every later read, and a save of plain ids asks nothing.
    let id = made["id"].as_i64().unwrap();
    let (status, saved) = k
        .call(
            "PATCH",
            &format!("/admin/api/mappings/{id}"),
            json!({ "channel_ids": [ARY] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["channel_titles"],
        json!({ ARY: "ARY Digital & More" })
    );
    assert_eq!(k.asked().len(), 3);
}

#[tokio::test]
async fn a_patch_and_the_dry_run_resolve_handles_too() {
    let k = kit().await;
    let (_, made) = k.create(&[]).await;
    let id = made["id"].as_i64().unwrap();
    let (status, run) = k
        .call(
            "POST",
            &format!("/admin/api/mappings/{id}/test"),
            json!({ "channel_ids": ["@arydigital"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    let (status, saved) = k
        .call(
            "PATCH",
            &format!("/admin/api/mappings/{id}"),
            json!({ "channel_ids": ["https://youtu.be/0eD-GQCutAU"] }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(saved["channel_ids"], json!([HUM]));
}

#[tokio::test]
async fn bad_input_and_failed_lookups_say_what_went_wrong_and_save_nothing() {
    let k = kit().await;
    let cases = [
        ("arydigital", StatusCode::UNPROCESSABLE_ENTITY, "@handle"),
        (
            "https://vimeo.com/@ary",
            StatusCode::UNPROCESSABLE_ENTITY,
            "not a channel id",
        ),
        (
            "@nobody",
            StatusCode::UNPROCESSABLE_ENTITY,
            "no channel at @nobody",
        ),
        (
            "@removed",
            StatusCode::UNPROCESSABLE_ENTITY,
            "names no channel",
        ),
        (
            "youtube.com/user/twochannels",
            StatusCode::UNPROCESSABLE_ENTITY,
            "more than one channel",
        ),
        (
            "https://www.youtube.com/watch?v=privatevid1",
            StatusCode::UNPROCESSABLE_ENTITY,
            "private or removed",
        ),
    ];
    for (input, want, says) in cases {
        let (status, body) = k.create(&[ARY, input]).await;
        assert_eq!(status, want, "{input}: {body}");
        let error = body["error"].as_str().unwrap();
        assert!(error.contains(says), "{input}: {error}");
    }
    // Refused input never asks YouTube.
    assert_eq!(k.asked().len(), 4);
    let (_, list) = k.call("GET", "/admin/api/mappings", Value::Null).await;
    assert_eq!(list, json!([]));
}

#[tokio::test]
async fn a_throttle_pauses_every_lookup_and_later_saves_say_so() {
    let k = kit().await;
    let (status, body) = k.create(&["@busy"]).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(
        body["error"].as_str().unwrap().contains("throttling"),
        "{body}"
    );
    assert!(k.limiter.status().paused_for.is_some());

    // While paused, a handle is refused at once without asking YouTube; an id still saves.
    let (status, body) = k.create(&["@arydigital"]).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert!(body["error"].as_str().unwrap().contains("paused"), "{body}");
    assert_eq!(k.asked(), vec!["/@busy"]);
    let (status, body) = k.create(&[ARY]).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}
