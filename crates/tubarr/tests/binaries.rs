//! The binary manager against a fake GitHub: release JSON with ETags, assets and checksum files, all local.

use std::{
    collections::HashMap,
    io::Write as _,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::{Arc, Mutex},
};

mod support;

use axum::{
    Router,
    extract::{Path as UrlPath, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use sha2::{Digest, Sha256};
use tubarr::{
    binaries::{BinaryError, BinaryManager, Outcome, Tool},
    config::Binaries,
};

const YT: &str = "yt-dlp/yt-dlp";
const DENO: &str = "denoland/deno";

#[derive(Default)]
struct Fake {
    base: String,
    /// repo -> (release JSON, ETag)
    releases: HashMap<String, (serde_json::Value, String)>,
    /// path under /dl -> body, None answers 500
    files: HashMap<String, Option<Vec<u8>>>,
    api_calls: usize,
    not_modified: usize,
    downloads: usize,
    /// Answer every API call with GitHub's exhausted rate limit reply.
    rate_limited: bool,
}

type Shared = Arc<Mutex<Fake>>;

async fn latest(
    State(fake): State<Shared>,
    UrlPath((owner, repo)): UrlPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let mut fake = fake.lock().unwrap();
    fake.api_calls += 1;
    if fake.rate_limited {
        return (
            StatusCode::FORBIDDEN,
            [
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", "4000000000"),
            ],
        )
            .into_response();
    }
    let Some((json, etag)) = fake.releases.get(&format!("{owner}/{repo}")).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        == Some(etag.as_str())
    {
        fake.not_modified += 1;
        return StatusCode::NOT_MODIFIED.into_response();
    }
    ([(header::ETAG, etag)], axum::Json(json)).into_response()
}

async fn download(State(fake): State<Shared>, UrlPath(path): UrlPath<String>) -> Response {
    let mut fake = fake.lock().unwrap();
    fake.downloads += 1;
    match fake.files.get(&path) {
        Some(Some(body)) => body.clone().into_response(),
        Some(None) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn start() -> Shared {
    let fake: Shared = Arc::default();
    let app = Router::new()
        .route("/repos/{owner}/{repo}/releases/latest", get(latest))
        .route("/dl/{*path}", get(download))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    fake.lock().unwrap().base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    fake
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn script(stdout: &str) -> Vec<u8> {
    format!("#!/bin/sh\necho '{stdout}'\n").into_bytes()
}

/// Publishes a release with one binary asset and its checksum file.
fn publish(fake: &Shared, repo: &str, tag: &str, asset: &str, body: Vec<u8>, sums_name: &str) {
    let mut f = fake.lock().unwrap();
    let sums = format!("{}  {asset}\n{}  other-file\n", sha(&body), sha(b"x"));
    let asset_path = format!("{tag}/{asset}");
    let sums_path = format!("{tag}/{sums_name}");
    let json = serde_json::json!({
        "tag_name": tag,
        "assets": [
            { "name": asset, "browser_download_url": format!("{}/dl/{asset_path}", f.base) },
            { "name": sums_name, "browser_download_url": format!("{}/dl/{sums_path}", f.base) },
        ],
    });
    f.files.insert(asset_path, Some(body));
    f.files.insert(sums_path, Some(sums.into_bytes()));
    f.releases
        .insert(repo.to_owned(), (json, format!("\"{repo}@{tag}\"")));
}

fn publish_yt_dlp(fake: &Shared, tag: &str) {
    publish(fake, YT, tag, "yt-dlp_linux", script(tag), "SHA2-256SUMS");
}

fn publish_deno(fake: &Shared, version: &str) {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("deno", options).unwrap();
    zip.write_all(&script(&format!(
        "deno {version} (stable, release, x86_64-unknown-linux-gnu)"
    )))
    .unwrap();
    let body = zip.finish().unwrap().into_inner();
    let asset = "deno-x86_64-unknown-linux-gnu.zip";
    publish(
        fake,
        DENO,
        &format!("v{version}"),
        asset,
        body,
        &format!("{asset}.sha256sum"),
    );
}

fn manager(fake: &Shared, dir: &Path) -> BinaryManager {
    BinaryManager::new(Binaries {
        bin_dir: dir.to_path_buf(),
        github_api: fake.lock().unwrap().base.parse().unwrap(),
        ..Binaries::default()
    })
    .unwrap()
}

fn outcome(
    results: &[(Tool, Result<Outcome, BinaryError>)],
    tool: Tool,
) -> &Result<Outcome, BinaryError> {
    &results.iter().find(|(t, _)| *t == tool).unwrap().1
}

/// Only the binaries and the state file are left: no downloads, no staged files.
fn assert_clean(dir: &Path) {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    names.sort();
    assert!(names.is_empty(), "leftover temp files: {names:?}");
}

#[tokio::test]
async fn fresh_install_gets_checked_binaries() {
    let fake = start().await;
    publish_yt_dlp(&fake, "2026.08.19");
    publish_deno(&fake, "2.9.7");
    let dir = tempfile::tempdir().unwrap();
    let bins = manager(&fake, dir.path());

    bins.ensure().await.unwrap();

    let smoke = bins.smoke().await;
    assert_eq!(smoke[0].1.as_deref().unwrap(), "2026.08.19");
    assert_eq!(smoke[1].1.as_deref().unwrap(), "2.9.7");
    let mode = std::fs::metadata(bins.path(Tool::YtDlp))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o755, 0o755);
    assert_clean(dir.path());

    let status = bins.status().await;
    assert_eq!(status[0].installed.as_deref(), Some("2026.08.19"));
    assert_eq!(status[0].latest.as_deref(), Some("2026.08.19"));
    assert_eq!(status[1].latest.as_deref(), Some("v2.9.7"));
    assert!(status[0].last_check.is_some());
    assert_eq!(
        status[0].last_result.as_deref(),
        Some("installed 2026.08.19")
    );

    // A second ensure finds both working and asks nobody.
    let calls = fake.lock().unwrap().api_calls;
    bins.ensure().await.unwrap();
    assert_eq!(fake.lock().unwrap().api_calls, calls);
}

#[tokio::test]
async fn current_binaries_use_the_etag_and_download_nothing() {
    let fake = start().await;
    publish_yt_dlp(&fake, "2026.08.19");
    publish_deno(&fake, "2.9.7");
    let dir = tempfile::tempdir().unwrap();
    let bins = manager(&fake, dir.path());
    bins.ensure().await.unwrap();
    let downloads = fake.lock().unwrap().downloads;

    let results = bins.check(true).await;

    assert_eq!(
        outcome(&results, Tool::YtDlp).as_ref().unwrap(),
        &Outcome::Current("2026.08.19".to_owned())
    );
    assert_eq!(
        outcome(&results, Tool::Deno).as_ref().unwrap(),
        &Outcome::Current("2.9.7".to_owned())
    );
    let f = fake.lock().unwrap();
    assert_eq!(f.not_modified, 2);
    assert_eq!(f.downloads, downloads);
}

#[tokio::test]
async fn checks_at_most_once_per_interval() {
    let fake = start().await;
    publish_yt_dlp(&fake, "2026.08.19");
    publish_deno(&fake, "2.9.7");
    let dir = tempfile::tempdir().unwrap();
    manager(&fake, dir.path()).ensure().await.unwrap();
    publish_yt_dlp(&fake, "2026.10.10");

    // A new manager reads the last check from the state file, as after a restart.
    let bins = manager(&fake, dir.path());
    let calls = fake.lock().unwrap().api_calls;
    let results = bins.check(false).await;
    assert_eq!(
        outcome(&results, Tool::YtDlp).as_ref().unwrap(),
        &Outcome::Skipped
    );
    assert_eq!(fake.lock().unwrap().api_calls, calls);
    assert_eq!(bins.version(Tool::YtDlp).await.unwrap(), "2026.08.19");
}

#[tokio::test]
async fn newer_release_is_installed_and_old_one_kept() {
    let fake = start().await;
    publish_yt_dlp(&fake, "2026.08.19");
    publish_deno(&fake, "2.9.7");
    let dir = tempfile::tempdir().unwrap();
    let bins = manager(&fake, dir.path());
    bins.ensure().await.unwrap();
    publish_yt_dlp(&fake, "2026.10.10");

    let results = bins.check(true).await;

    assert_eq!(
        outcome(&results, Tool::YtDlp).as_ref().unwrap(),
        &Outcome::Updated {
            from: Some("2026.08.19".to_owned()),
            to: "2026.10.10".to_owned()
        }
    );
    assert_eq!(bins.version(Tool::YtDlp).await.unwrap(), "2026.10.10");
    let prev = dir.path().join("yt-dlp.prev");
    assert_eq!(std::fs::read(&prev).unwrap(), script("2026.08.19"));
    assert_clean(dir.path());

    assert!(bins.rollback(Tool::YtDlp).await.unwrap());
    assert_eq!(bins.version(Tool::YtDlp).await.unwrap(), "2026.08.19");
}

#[tokio::test]
async fn checksum_mismatch_is_rejected() {
    let fake = start().await;
    publish_yt_dlp(&fake, "2026.08.19");
    publish_deno(&fake, "2.9.7");
    let dir = tempfile::tempdir().unwrap();
    let bins = manager(&fake, dir.path());
    bins.ensure().await.unwrap();

    publish_yt_dlp(&fake, "2026.10.10");
    fake.lock().unwrap().files.insert(
        "2026.10.10/yt-dlp_linux".to_owned(),
        Some(script("tampered")),
    );

    let results = bins.check(true).await;

    assert!(matches!(
        outcome(&results, Tool::YtDlp),
        Err(BinaryError::ChecksumMismatch { .. })
    ));
    assert_eq!(bins.version(Tool::YtDlp).await.unwrap(), "2026.08.19");
    assert_clean(dir.path());
    let status = bins.status().await;
    assert!(
        status[0]
            .last_result
            .as_deref()
            .unwrap()
            .contains("checksum mismatch")
    );
}

#[tokio::test]
async fn checksum_mismatch_on_fresh_install_leaves_no_binary() {
    let fake = start().await;
    publish_yt_dlp(&fake, "2026.08.19");
    publish_deno(&fake, "2.9.7");
    fake.lock().unwrap().files.insert(
        "2026.08.19/yt-dlp_linux".to_owned(),
        Some(script("tampered")),
    );
    let dir = tempfile::tempdir().unwrap();
    let bins = manager(&fake, dir.path());

    let err = bins.ensure().await.unwrap_err();

    assert!(matches!(err, BinaryError::ChecksumMismatch { .. }));
    assert!(!bins.path(Tool::YtDlp).exists());
    assert_clean(dir.path());
}

#[tokio::test]
async fn failed_download_keeps_the_old_binary() {
    let fake = start().await;
    publish_yt_dlp(&fake, "2026.08.19");
    publish_deno(&fake, "2.9.7");
    let dir = tempfile::tempdir().unwrap();
    let bins = manager(&fake, dir.path());
    bins.ensure().await.unwrap();

    publish_yt_dlp(&fake, "2026.10.10");
    fake.lock()
        .unwrap()
        .files
        .insert("2026.10.10/yt-dlp_linux".to_owned(), None);

    let results = bins.check(true).await;

    assert!(matches!(
        outcome(&results, Tool::YtDlp),
        Err(BinaryError::Status { status, .. }) if *status == StatusCode::INTERNAL_SERVER_ERROR
    ));
    assert_eq!(bins.version(Tool::YtDlp).await.unwrap(), "2026.08.19");
    assert_clean(dir.path());
}

#[tokio::test]
async fn broken_new_binary_is_rolled_back() {
    let fake = start().await;
    publish_yt_dlp(&fake, "2026.08.19");
    publish_deno(&fake, "2.9.7");
    let dir = tempfile::tempdir().unwrap();
    let bins = manager(&fake, dir.path());
    bins.ensure().await.unwrap();

    publish(
        &fake,
        YT,
        "2026.10.10",
        "yt-dlp_linux",
        b"#!/bin/sh\nexit 1\n".to_vec(),
        "SHA2-256SUMS",
    );

    let results = bins.check(true).await;

    assert!(matches!(
        outcome(&results, Tool::YtDlp),
        Err(BinaryError::Smoke { .. })
    ));
    assert_eq!(bins.version(Tool::YtDlp).await.unwrap(), "2026.08.19");
}

#[tokio::test]
async fn rate_limit_is_respected_until_reset() {
    let fake = start().await;
    publish_yt_dlp(&fake, "2026.08.19");
    publish_deno(&fake, "2.9.7");
    let dir = tempfile::tempdir().unwrap();
    let bins = manager(&fake, dir.path());
    bins.ensure().await.unwrap();
    fake.lock().unwrap().rate_limited = true;

    let results = bins.check(true).await;
    assert!(matches!(
        outcome(&results, Tool::YtDlp),
        Err(BinaryError::RateLimited {
            until: 4_000_000_000
        })
    ));
    // The deno check and any later one wait for the reset instead of asking again.
    assert_eq!(fake.lock().unwrap().api_calls, 3);
    let results = bins.check(true).await;
    assert!(matches!(
        outcome(&results, Tool::Deno),
        Err(BinaryError::RateLimited { .. })
    ));
    assert_eq!(fake.lock().unwrap().api_calls, 3);
    assert_eq!(bins.version(Tool::YtDlp).await.unwrap(), "2026.08.19");
}

#[tokio::test]
async fn explicit_path_is_used_and_left_alone() {
    let fake = start().await;
    publish_yt_dlp(&fake, "2026.08.19");
    publish_deno(&fake, "2.9.7");
    let dir = tempfile::tempdir().unwrap();
    let system = dir.path().join("system-yt-dlp");
    support::install_executable(&system, &script("2025.01.01"));
    let bins = BinaryManager::new(Binaries {
        bin_dir: dir.path().join("bin"),
        yt_dlp: Some(system.clone()),
        github_api: fake.lock().unwrap().base.parse().unwrap(),
        ..Binaries::default()
    })
    .unwrap();

    bins.ensure().await.unwrap();
    let results = bins.check(true).await;

    assert_eq!(bins.path(Tool::YtDlp), system);
    assert_eq!(bins.version(Tool::YtDlp).await.unwrap(), "2025.01.01");
    assert!(results.iter().all(|(tool, _)| *tool == Tool::Deno));
    assert!(!dir.path().join("bin/yt-dlp").exists());
    assert_eq!(bins.version(Tool::Deno).await.unwrap(), "2.9.7");
}
