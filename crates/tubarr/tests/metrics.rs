//! `GET /metrics`: off by default, behind a token or the play networks, and counting what happens.

mod support;

use std::{io::Write as _, path::Path, sync::Arc};

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use tower::ServiceExt;
use tubarr::{AppState, binaries::BinaryManager, config::Config, store::Db};

fn secret(dir: &Path, name: &str, value: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    writeln!(std::fs::File::create(&path).unwrap(), "{value}").unwrap();
    path
}

/// The state and router for a config with `extra` after the basics.
async fn app(dir: &Path, extra: &str) -> (AppState, axum::Router) {
    let config = Config::from_toml(&format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\n{extra}",
        dir
    ))
    .unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let state = AppState::new(config, db, binaries).unwrap();
    (state.clone(), tubarr::app(state))
}

/// GETs `uri` from `peer` with an optional bearer token.
async fn get(
    app: &axum::Router,
    uri: &str,
    peer: &str,
    bearer: Option<&str>,
) -> (StatusCode, String) {
    get_forwarded(app, uri, peer, bearer, &[]).await
}

async fn get_forwarded(
    app: &axum::Router,
    uri: &str,
    peer: &str,
    bearer: Option<&str>,
    forwarded: &[&str],
) -> (StatusCode, String) {
    let mut request = Request::get(uri);
    for value in forwarded {
        request = request.header("x-forwarded-for", *value);
    }
    if let Some(token) = bearer {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let mut request = request.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<std::net::SocketAddr>().unwrap()));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// The value of the sample `series` (name and labels as written), 0 when absent.
fn sample(text: &str, series: &str) -> f64 {
    text.lines()
        .find_map(|l| l.strip_prefix(series)?.strip_prefix(' ')?.parse().ok())
        .unwrap_or(0.0)
}

const LOCAL: &str = "127.0.0.1:40000";
const PUBLIC: &str = "203.0.113.9:40000";

#[tokio::test]
async fn metrics_are_off_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let (_, app) = app(dir.path(), "").await;
    let (status, _) = get(&app, "/metrics", LOCAL, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn metrics_token_is_required_when_set() {
    let dir = tempfile::tempdir().unwrap();
    let token = secret(dir.path(), "metrics_token", "scrape-me");
    let key = secret(dir.path(), "api_key", "the-api-key");
    let (_, app) = app(
        dir.path(),
        &format!("api_key_file = {key:?}\n[metrics]\nenabled = true\ntoken_file = {token:?}\n"),
    )
    .await;
    // A local peer still needs the token once one is set.
    assert_eq!(
        get(&app, "/metrics", LOCAL, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&app, "/metrics", LOCAL, Some("wrong")).await.0,
        StatusCode::UNAUTHORIZED
    );
    // The metrics token wins over the API key.
    assert_eq!(
        get(&app, "/metrics", LOCAL, Some("the-api-key")).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = get(&app, "/metrics", PUBLIC, Some("scrape-me")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("# TYPE tubarr_lookups_total counter"),
        "{body}"
    );
    assert!(body.contains("tubarr_play_first_byte_seconds_bucket{le=\"+Inf\"}"));
    assert!(body.contains("# TYPE tubarr_activity_events gauge"));
    assert!(!body.contains("scrape-me"));
}

#[tokio::test]
async fn metrics_take_the_api_key_without_their_own_token() {
    let dir = tempfile::tempdir().unwrap();
    let key = secret(dir.path(), "api_key", "the-api-key");
    let (_, app) = app(
        dir.path(),
        &format!("api_key_file = {key:?}\n[metrics]\nenabled = true\n"),
    )
    .await;
    assert_eq!(
        get(&app, "/metrics", LOCAL, None).await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        get(&app, "/metrics", PUBLIC, Some("the-api-key")).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn metrics_without_any_token_follow_the_play_networks() {
    let dir = tempfile::tempdir().unwrap();
    let (_, app) = app(dir.path(), "[metrics]\nenabled = true\n").await;
    assert_eq!(get(&app, "/metrics", LOCAL, None).await.0, StatusCode::OK);
    assert_eq!(
        get(&app, "/metrics", PUBLIC, None).await.0,
        StatusCode::FORBIDDEN
    );
}

const PROXY_METRICS: &str =
    "[metrics]\nenabled = true\n[auth]\ntrusted_proxies = [\"172.16.0.2/32\", \"172.16.0.3/32\"]\n";

#[tokio::test]
async fn tokenless_metrics_require_a_client_behind_trusted_proxies() {
    let dir = tempfile::tempdir().unwrap();
    let (_, app) = app(dir.path(), PROXY_METRICS).await;
    for values in [
        vec![],
        vec!["172.16.0.3"],
        vec!["172.16.0.2", "172.16.0.3"],
        vec!["::ffff:172.16.0.3"],
    ] {
        let (status, body) =
            get_forwarded(&app, "/metrics", "172.16.0.2:4000", None, &values).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{values:?}");
        assert!(body.is_empty(), "refused scrapes expose no metrics");
    }
}

#[tokio::test]
async fn tokenless_metrics_preserve_forwarded_client_rules() {
    let dir = tempfile::tempdir().unwrap();
    let (_, app) = app(dir.path(), PROXY_METRICS).await;
    for values in [
        vec![""],
        vec!["nope"],
        vec!["192.168.1.20,"],
        vec!["192.168.1.20", ""],
        vec!["192.168.1.20, 203.0.113.7", "172.16.0.3"],
    ] {
        assert_eq!(
            get_forwarded(&app, "/metrics", "172.16.0.2:4000", None, &values)
                .await
                .0,
            StatusCode::FORBIDDEN,
            "{values:?}"
        );
    }
    for values in [
        vec!["nope, 192.168.1.20, 172.16.0.3"],
        vec!["203.0.113.7", "192.168.1.20, 172.16.0.3"],
        vec!["::ffff:192.168.1.20, ::ffff:172.16.0.3"],
    ] {
        assert_eq!(
            get_forwarded(&app, "/metrics", "[::ffff:172.16.0.2]:4000", None, &values)
                .await
                .0,
            StatusCode::OK,
            "{values:?}"
        );
    }
    assert_eq!(
        get_forwarded(&app, "/metrics", PUBLIC, None, &["192.168.1.20"])
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        get(&app, "/metrics", "192.168.1.20:4000", None).await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn plays_and_lookups_move_the_counters() {
    let dir = tempfile::tempdir().unwrap();
    let ytdlp = support::FakeYtDlp::from_index(dir.path(), "unavailable");
    let (state, app) = app(
        dir.path(),
        &format!(
            "[binaries]\nyt_dlp = {:?}\ndeno = {:?}\n[metrics]\nenabled = true\n",
            ytdlp.path, ytdlp.path
        ),
    )
    .await;
    let (_, before) = get(&app, "/metrics", LOCAL, None).await;

    // A play start the handler refuses: a 4xx answer, counted as a start.
    let (status, _) = get(&app, "/play/not-an-id", LOCAL, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A fresh lookup that fails on yt-dlp.
    assert!(state.resolver.resolve("dQw4w9WgXcQ").await.is_err());

    let (_, after) = get(&app, "/metrics", LOCAL, None).await;
    let grew = |series: &str| sample(&after, series) - sample(&before, series);
    assert!(
        grew("tubarr_play_requests_total{status=\"4xx\"}") >= 1.0,
        "{after}"
    );
    assert!(grew("tubarr_play_starts_total") >= 1.0);
    assert_eq!(
        sample(&after, "tubarr_lookups_total{outcome=\"failed\"}"),
        1.0
    );
    assert_eq!(sample(&after, "tubarr_lookups_total{outcome=\"ok\"}"), 0.0);
    assert!(grew("tubarr_lookup_duration_seconds_count") >= 1.0);
    assert_eq!(sample(&after, "tubarr_ytdlp_processes"), 0.0);
    assert!(sample(&after, "process_resident_memory_bytes") > 0.0);
}
