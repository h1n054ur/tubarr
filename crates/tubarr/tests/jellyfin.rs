//! Jellyfin version merges: the Sonarr webhook writes an audio only file, queues its merge, and
//! the worker merges it into the episode in a fake Jellyfin 10.11 (tests/support/jellyfin.rs)
//! once both files are scanned, or splits it off again when the file goes.

mod support;

use std::path::{Path, PathBuf};

use axum::{
    Json, Router,
    body::Body,
    http::{Request, StatusCode},
    routing::get,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use support::jellyfin::{API_KEY, FakeJellyfin};
use tower::ServiceExt;
use tubarr::{
    AppState,
    binaries::BinaryManager,
    config::Config,
    store::{Db, MappingKind, NewMapping, VersionAction},
};

const TOKEN: &str = "sonarr-hook-token";
const ADMIN_KEY: &str = "test-admin-key";
const SONARR_KEY: &str = "0123456789abcdef0123456789abcdef";
const SERIES_ID: i64 = 17;
const PLAY: &str = "http://tubarr:8480/play/0eD-GQCutAU\n";
const SHOW: &str = "pk-dramas/Kabhi Main Kabhi Tum (2024)";
const EPISODE: &str = "pk-dramas/Kabhi Main Kabhi Tum (2024)/Season 1/Kabhi Main Kabhi Tum - S01E02 - Episode 2 WEBDL-1080p";
/// Jellyfin's id of the series folder.
const SHOW_ID: &str = "5000000000000000000000000000000a";
/// The ids sort so the real MergeVersions would make the audio file primary without a probe.
const VIDEO_ID: &str = "f0000000000000000000000000000002";
const AUDIO_ID: &str = "10000000000000000000000000000001";

struct Kit {
    state: AppState,
    dir: tempfile::TempDir,
    toml: String,
    mapping: i64,
}

impl Kit {
    /// The episode's `.strm` in Tubarr's container.
    fn main(&self) -> PathBuf {
        self.dir
            .path()
            .join("media")
            .join(format!("{EPISODE}.strm"))
    }

    fn audio(&self) -> PathBuf {
        self.dir
            .path()
            .join("media")
            .join(format!("{EPISODE} - Audio only.strm"))
    }

    async fn hook(&self, name: &str) -> Value {
        let payload = support::fixture_json(&format!("sonarr_hooks/{name}.json"));
        let req = Request::post("/hooks/sonarr")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TOKEN}"))
            .body(Body::from(payload.to_string()))
            .unwrap();
        let res = tubarr::app(self.state.clone()).oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn audio_version(&self, on: bool) {
        self.state
            .db
            .set_audio_version(self.mapping, on)
            .await
            .unwrap();
    }

    async fn queued(&self) -> i64 {
        self.state.db.version_jobs_queued().await.unwrap()
    }

    async fn run(&self) {
        self.state.jellyfin.as_ref().unwrap().run_due().await;
    }

    /// A fresh `AppState` on the same database and files, as after a restart.
    async fn restart(&mut self) {
        self.state = state(&self.toml).await;
    }
}

/// Jellyfin mounts the library at `/jf/media`.
fn jf(rel: &str) -> String {
    format!("/jf/media/{rel}")
}

async fn state(toml: &str) -> AppState {
    let config = Config::from_toml(toml).unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = std::sync::Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    AppState::new(config, db, binaries).unwrap()
}

/// `jellyfin` is the fake's URL, or `None` to leave `[jellyfin]` out.
async fn kit(jellyfin: Option<&str>) -> Kit {
    kit_with(jellyfin, None).await
}

/// `sonarr` is the URL of a fake Sonarr, for the backfill.
async fn kit_with(jellyfin: Option<&str>, sonarr: Option<&str>) -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    let write = |name: &str, value: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, value).unwrap();
        path
    };
    let mut toml = format!(
        "public_url = \"http://tubarr:8480\"\ndata_dir = {:?}\napi_key_file = {:?}\n",
        dir.path(),
        write("admin_key", ADMIN_KEY),
    );
    if let Some(url) = sonarr {
        toml.push_str(&format!(
            "[sonarr]\nurl = \"{url}\"\napi_key_file = {:?}\n",
            write("sonarr_key", SONARR_KEY)
        ));
    }
    toml.push_str(&format!(
        "[hooks]\nsonarr_token_file = {:?}\nlibrary_paths = [[\"/mnt/fk/data/media\", {:?}]]\n",
        write("hook_token", TOKEN),
        media,
    ));
    if let Some(url) = jellyfin {
        toml.push_str(&format!(
            "[jellyfin]\nurl = \"{url}\"\napi_key_file = {:?}\nlibrary_paths = [[{:?}, \"/jf/media\"]]\n\
             merge_retry = \"50ms\"\nmerge_attempts = 4\n",
            write("jellyfin_key", API_KEY),
            media,
        ));
    }
    let state = state(&toml).await;
    let mapping = state
        .db
        .insert_mapping(&NewMapping {
            kind: MappingKind::Show,
            tvdb_id: Some(452_001),
            tmdb_id: None,
            title: "Kabhi Main Kabhi Tum".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec![],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        })
        .await
        .unwrap()
        .id;
    sqlx::query("UPDATE mappings SET arr_id = ? WHERE id = ?")
        .bind(SERIES_ID)
        .bind(mapping)
        .execute(state.db.pool())
        .await
        .unwrap();
    let kit = Kit {
        state,
        dir,
        toml,
        mapping,
    };
    put(&kit.main(), PLAY);
    kit.audio_version(true).await;
    kit
}

fn put(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

/// Both files scanned by the fake under Jellyfin's paths.
fn scanned(fake: &FakeJellyfin) {
    fake.add_series(SHOW_ID, &jf(SHOW));
    fake.add(VIDEO_ID, &jf(&format!("{EPISODE}.strm")));
    fake.add(AUDIO_ID, &jf(&format!("{EPISODE} - Audio only.strm")));
}

#[tokio::test]
async fn an_imported_episode_gets_its_audio_version_merged_episode_first() {
    let fake = FakeJellyfin::start().await;
    let k = kit(Some(&fake.url)).await;
    scanned(&fake);

    let body = k.hook("download").await;
    assert_eq!(body["written"], 1);
    assert!(k.audio().exists());
    assert_eq!(k.queued().await, 1);

    k.run().await;
    assert_eq!(
        fake.calls(),
        [
            "GET /Items",
            "GET /Items",
            "GET /Items",
            "GET /Users",
            &format!("POST /Items/{VIDEO_ID}/PlaybackInfo"),
            "POST /Videos/MergeVersions",
            "GET /Items",
        ]
    );
    narrow_reads_only(&fake);
    // The probe made the episode primary although the audio file has the lower id.
    assert_eq!(fake.sources(VIDEO_ID), [VIDEO_ID, AUDIO_ID]);
    assert_eq!(k.queued().await, 0);
    let health = k.state.jellyfin.as_ref().unwrap().health().await;
    assert_eq!((health.queued, health.last_error), (0, None));
}

/// The listings were the series list and series-scoped episode lists, never every episode on the
/// server, and only `PlaybackInfo` went as a user.
fn narrow_reads_only(fake: &FakeJellyfin) {
    for request in fake.requests() {
        if request.starts_with("GET /Items?") && request.contains("includeItemTypes=Episode") {
            assert!(request.contains("parentId="), "{request}");
        }
        if request.contains("userId=") {
            assert!(request.contains("/PlaybackInfo?"), "{request}");
        }
    }
}

#[tokio::test]
async fn an_item_is_read_by_id_without_a_user() {
    let fake = FakeJellyfin::start().await;
    let k = kit(Some(&fake.url)).await;
    fake.lock().hidden = Some(jf("pk-dramas/"));
    scanned(&fake);

    // Like 10.11, the single item route needs a user.
    let res = reqwest::Client::new()
        .get(format!("{}/Items/{VIDEO_ID}?fields=MediaSources", fake.url))
        .header("authorization", format!("MediaBrowser Token=\"{API_KEY}\""))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), reqwest::StatusCode::BAD_REQUEST);
    fake.clear_calls();

    k.hook("download").await;
    k.run().await;
    assert_eq!(k.queued().await, 0);
    assert_eq!(fake.sources(VIDEO_ID), [VIDEO_ID, AUDIO_ID]);
    let reads: Vec<String> = fake
        .requests()
        .into_iter()
        .filter(|r| r.starts_with(&format!("GET /Items?ids={VIDEO_ID}")))
        .collect();
    assert_eq!(reads.len(), 2, "{:?}", fake.requests());
    assert!(
        reads
            .iter()
            .all(|r| r.contains("MediaSources") && !r.contains("userId=")),
        "{reads:?}"
    );
    narrow_reads_only(&fake);
}

#[tokio::test]
async fn an_audio_file_not_scanned_yet_gets_its_series_refreshed_once() {
    let fake = FakeJellyfin::start().await;
    let k = kit(Some(&fake.url)).await;
    fake.add_series(SHOW_ID, &jf(SHOW));
    fake.add(VIDEO_ID, &jf(&format!("{EPISODE}.strm")));
    // On disk, but Jellyfin sees it only after a refresh.
    fake.lock().unscanned.insert(
        AUDIO_ID.to_owned(),
        jf(&format!("{EPISODE} - Audio only.strm")),
    );
    k.hook("download").await;

    k.run().await;
    assert_eq!(
        refreshes(&fake),
        [format!("POST /Items/{SHOW_ID}/Refresh")],
        "{:?}",
        fake.requests()
    );
    assert!(
        fake.requests()
            .iter()
            .any(|r| r.contains("/Refresh?") && r.contains("recursive=true")),
        "{:?}",
        fake.requests()
    );
    // Looked at again on the usual retry, not at once.
    assert_eq!(k.queued().await, 1);
    let health = k.state.jellyfin.as_ref().unwrap().health().await;
    assert_eq!(health.last_error, None);

    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    k.run().await;
    assert_eq!(k.queued().await, 0);
    assert_eq!(fake.sources(VIDEO_ID), [VIDEO_ID, AUDIO_ID]);
    assert_eq!(refreshes(&fake).len(), 1, "{:?}", fake.requests());
}

#[tokio::test]
async fn audio_files_never_scanned_refresh_their_series_once_in_all() {
    let fake = FakeJellyfin::start().await;
    let k = kit(Some(&fake.url)).await;
    fake.add_series(SHOW_ID, &jf(SHOW));
    fake.add(VIDEO_ID, &jf(&format!("{EPISODE}.strm")));
    let other = "pk-dramas/Kabhi Main Kabhi Tum (2024)/Season 1/Kabhi Main Kabhi Tum - S01E03 - Episode 3 WEBDL-1080p";
    fake.add(
        "f0000000000000000000000000000003",
        &jf(&format!("{other}.strm")),
    );
    k.hook("download").await;
    // A second episode of the same series, due in the same round.
    k.state
        .db
        .queue_version_job(&jf(&format!("{other}.strm")), VersionAction::Merge, 0)
        .await
        .unwrap();

    for _ in 0..4 {
        k.run().await;
        tokio::time::sleep(std::time::Duration::from_millis(450)).await;
    }
    assert_eq!(k.queued().await, 0);
    assert_eq!(refreshes(&fake).len(), 1, "{:?}", fake.requests());
    let error = k
        .state
        .jellyfin
        .as_ref()
        .unwrap()
        .health()
        .await
        .last_error
        .unwrap();
    assert!(error.contains("is not in Jellyfin yet"), "{error}");
}

/// The refresh requests, without their queries. None is ever library wide.
fn refreshes(fake: &FakeJellyfin) -> Vec<String> {
    let calls = fake.calls();
    assert!(
        !calls.iter().any(|c| c.contains("/Library/Refresh")),
        "{calls:?}"
    );
    calls
        .into_iter()
        .filter(|c| c.ends_with("/Refresh"))
        .collect()
}

#[tokio::test]
async fn an_episode_in_a_library_hidden_from_the_user_is_found_through_its_series() {
    let fake = FakeJellyfin::start().await;
    let k = kit(Some(&fake.url)).await;
    // A Podcasts library not shared with the administrator: user views leave it out.
    fake.lock().hidden = Some(jf("pk-dramas/"));
    scanned(&fake);
    k.hook("download").await;

    k.run().await;
    assert_eq!(k.queued().await, 0);
    assert_eq!(fake.sources(VIDEO_ID), [VIDEO_ID, AUDIO_ID]);
    narrow_reads_only(&fake);
    assert!(
        fake.requests()
            .iter()
            .any(|r| r.contains(&format!("parentId={SHOW_ID}"))),
        "{:?}",
        fake.requests()
    );
}

#[tokio::test]
async fn series_ids_are_kept_and_read_again_when_a_series_comes_back_under_a_new_id() {
    let fake = FakeJellyfin::start().await;
    let k = kit(Some(&fake.url)).await;
    scanned(&fake);
    k.hook("download").await;
    k.run().await;
    fake.clear_calls();

    // The next round finds the series by its kept id: no series list.
    k.hook("download").await;
    k.run().await;
    let series_lists = |fake: &FakeJellyfin| {
        fake.requests()
            .iter()
            .filter(|r| r.contains("includeItemTypes=Series"))
            .count()
    };
    assert_eq!(series_lists(&fake), 0, "{:?}", fake.requests());
    assert_eq!(k.queued().await, 0);

    // The series is added again: its old id lists nothing, so the series list is read once more.
    fake.clear_calls();
    {
        let mut lib = fake.lock();
        lib.series.clear();
        lib.versions.clear();
    }
    fake.add_series("6000000000000000000000000000000b", &jf(SHOW));
    k.hook("download").await;
    k.run().await;
    assert_eq!(series_lists(&fake), 1, "{:?}", fake.requests());
    assert_eq!(k.queued().await, 0);
    assert_eq!(fake.sources(VIDEO_ID), [VIDEO_ID, AUDIO_ID]);
}

#[tokio::test]
async fn an_audio_file_merged_outside_tubarr_counts_as_merged() {
    let fake = FakeJellyfin::start().await;
    let k = kit(Some(&fake.url)).await;
    scanned(&fake);
    // Already an alternate of the episode, and still listed on its own without a user.
    fake.lock()
        .versions
        .insert(VIDEO_ID.to_owned(), vec![AUDIO_ID.to_owned()]);
    k.hook("download").await;

    k.run().await;
    assert!(
        !fake.calls().iter().any(|c| c.contains("MergeVersions")),
        "{:?}",
        fake.calls()
    );
    assert_eq!(fake.sources(VIDEO_ID), [VIDEO_ID, AUDIO_ID]);
    assert_eq!(k.queued().await, 0);
}

#[tokio::test]
async fn an_episode_already_merged_is_left_alone() {
    let fake = FakeJellyfin::start().await;
    let k = kit(Some(&fake.url)).await;
    scanned(&fake);
    k.hook("download").await;
    k.run().await;
    fake.clear_calls();

    // The same import again: still merged, so no second merge.
    let body = k.hook("download").await;
    assert_eq!(body["written"], 0);
    assert_eq!(k.queued().await, 1);
    k.run().await;
    assert!(
        !fake.calls().iter().any(|c| c.contains("MergeVersions")),
        "{:?}",
        fake.calls()
    );
    assert_eq!(fake.sources(VIDEO_ID), [VIDEO_ID, AUDIO_ID]);
    assert_eq!(k.queued().await, 0);
}

#[tokio::test]
async fn episodes_not_scanned_yet_are_tried_again_later_then_merged() {
    let fake = FakeJellyfin::start().await;
    let k = kit(Some(&fake.url)).await;
    k.hook("download").await;

    k.run().await;
    assert_eq!(k.queued().await, 1);
    // Waiting for a scan is not an error to show.
    let health = k.state.jellyfin.as_ref().unwrap().health().await;
    assert_eq!((health.queued, health.last_error), (1, None));
    // Not due again yet: no request at all.
    fake.clear_calls();
    k.run().await;
    assert!(fake.calls().is_empty(), "{:?}", fake.calls());

    // Only the episode is scanned: the audio file is still missing.
    fake.add(VIDEO_ID, &jf(&format!("{EPISODE}.strm")));
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    k.run().await;
    assert_eq!(k.queued().await, 1);
    assert!(!fake.calls().iter().any(|c| c.contains("MergeVersions")));

    // The second wait is twice the first.
    scanned(&fake);
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    k.run().await;
    assert_eq!(k.queued().await, 1, "not due before 100ms");
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    k.run().await;
    assert_eq!(k.queued().await, 0);
    assert_eq!(fake.sources(VIDEO_ID), [VIDEO_ID, AUDIO_ID]);
}

#[tokio::test]
async fn a_merge_never_scanned_is_given_up_on_and_shown() {
    let fake = FakeJellyfin::start().await;
    let k = kit(Some(&fake.url)).await;
    k.hook("download").await;
    for _ in 0..4 {
        k.run().await;
        tokio::time::sleep(std::time::Duration::from_millis(450)).await;
    }
    assert_eq!(k.queued().await, 0);
    let health = k.state.jellyfin.as_ref().unwrap().health().await;
    let error = health.last_error.unwrap();
    assert!(error.contains("is not in Jellyfin yet"), "{error}");
}

/// A fake Sonarr listing the episode's file for series 17.
async fn fake_sonarr() -> String {
    async fn files() -> Json<Value> {
        Json(json!([
            { "id": 5502, "seriesId": SERIES_ID, "seasonNumber": 1,
              "relativePath": "Season 1/x.strm",
              "path": format!("/mnt/fk/data/media/{EPISODE}.strm"),
              "size": 64, "dateAdded": "2026-10-24T06:00:00Z" }
        ]))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().route("/api/v3/episodefile", get(files));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test]
async fn a_merge_given_up_on_is_queued_again_from_the_admin_api() {
    let fake = FakeJellyfin::start().await;
    let sonarr = fake_sonarr().await;
    let k = kit_with(Some(&fake.url), Some(&sonarr)).await;
    k.hook("download").await;
    for _ in 0..4 {
        k.run().await;
        tokio::time::sleep(std::time::Duration::from_millis(450)).await;
    }
    assert_eq!(k.queued().await, 0);

    scanned(&fake);
    let req = Request::post(format!("/admin/api/mappings/{}/audio-versions", k.mapping))
        .header("x-api-key", ADMIN_KEY)
        .body(Body::empty())
        .unwrap();
    let res = tubarr::app(k.state.clone()).oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        (body["changed"].as_i64(), body["queued"].as_i64()),
        (Some(0), Some(1))
    );

    k.run().await;
    assert_eq!(k.queued().await, 0);
    assert_eq!(fake.sources(VIDEO_ID), [VIDEO_ID, AUDIO_ID]);
}

#[tokio::test]
async fn the_queue_survives_a_restart() {
    let fake = FakeJellyfin::start().await;
    let mut k = kit(Some(&fake.url)).await;
    k.hook("download").await;
    k.restart().await;
    assert!(fake.calls().is_empty());
    scanned(&fake);
    k.run().await;
    assert_eq!(k.queued().await, 0);
    assert_eq!(fake.sources(VIDEO_ID), [VIDEO_ID, AUDIO_ID]);
}

#[tokio::test]
async fn removing_the_audio_version_splits_it_off() {
    let fake = FakeJellyfin::start().await;
    let k = kit(Some(&fake.url)).await;
    scanned(&fake);
    k.hook("download").await;
    k.run().await;
    fake.clear_calls();

    // The option goes off: the next import removes the audio file and queues the split.
    k.audio_version(false).await;
    let body = k.hook("download").await;
    assert_eq!(body["removed"], 1);
    assert!(!k.audio().exists());
    k.run().await;
    assert!(
        fake.calls()
            .contains(&format!("DELETE /Videos/{VIDEO_ID}/AlternateSources")),
        "{:?}",
        fake.calls()
    );
    assert_eq!(fake.sources(VIDEO_ID), [VIDEO_ID]);
    assert_eq!(k.queued().await, 0);

    // Deleting the episode file splits nothing more: the episode has no versions left, and the
    // call that would fail on it is not made.
    fake.clear_calls();
    put(&k.audio(), "http://tubarr:8480/play/0eD-GQCutAU/audio\n");
    k.hook("episode_file_delete").await;
    k.run().await;
    assert!(!fake.calls().iter().any(|c| c.starts_with("DELETE")));
    assert_eq!(k.queued().await, 0);
}

#[tokio::test]
async fn without_jellyfin_nothing_is_queued() {
    let fake = FakeJellyfin::start().await;
    let k = kit(None).await;
    scanned(&fake);
    let body = k.hook("download").await;
    assert_eq!(body["written"], 1);
    assert!(k.state.jellyfin.is_none());
    assert_eq!(k.queued().await, 0);
    assert!(fake.calls().is_empty());
}
