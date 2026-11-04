use axum::{body::Body, http::Request, http::StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;
use tubarr::{AppState, binaries::BinaryManager, config::Config, store::Db};

/// The router over a fresh database. Keep the folder alive as long as the router.
async fn app() -> (axum::Router, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let config = Config::from_toml(&format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {:?}",
        dir.path()
    ))
    .unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = std::sync::Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    (
        tubarr::app(AppState::new(config, db, binaries).unwrap()),
        dir,
    )
}

#[tokio::test]
async fn healthz_returns_ok() {
    let (app, _dir) = app().await;
    let response = app
        .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"ok");
}

#[tokio::test]
async fn unknown_path_is_not_found() {
    let (app, _dir) = app().await;
    // Unknown GETs go to the web UI's router, so only other methods get the plain 404.
    let response = app
        .oneshot(Request::post("/nope").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn play_is_mounted() {
    let (app, _dir) = app().await;
    // A loopback peer, which the default play_allowed_networks lets through.
    let mut request = Request::get("/play/not-an-id").body(Body::empty()).unwrap();
    let peer: std::net::SocketAddr = "127.0.0.1:40000".parse().unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    let response = app.oneshot(request).await.unwrap();

    // The play handler's own answer for a bad id, not the router's empty 404.
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(String::from_utf8_lossy(&body).contains("not a YouTube video id"));
}

#[test]
fn healthcheck_does_not_repeat_the_play_networks_warning() {
    let dir = tempfile::tempdir().unwrap();
    // A port nobody listens on, so the probe fails fast without a server.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = dir.path().join("tubarr.toml");
    std::fs::write(
        &config,
        format!(
            "public_url = \"http://localhost:8480\"\nlisten = \"127.0.0.1:{port}\"\ndata_dir = {:?}",
            dir.path()
        ),
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_tubarr"))
        .arg("--healthcheck")
        .arg("--config")
        .arg(&config)
        .env("RUST_LOG", "info")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let all = [output.stdout, output.stderr].concat();
    assert!(!String::from_utf8_lossy(&all).contains("play_allowed_networks is not set"));
}
