//! The Sonarr webhook and the audio only versions it keeps next to imported episodes, with
//! payloads shaped like Sonarr 4's (tests/fixtures/sonarr_hooks) and a small fake Sonarr for the
//! backfill when a mapping's toggle changes. Episode thumbnails come from a small fake of
//! YouTube's image host.

mod support;

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use axum::{
    Json, Router,
    body::Body,
    extract::Path as UrlPath,
    http::{HeaderMap, Request, StatusCode},
    response::IntoResponse,
    routing::get,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use tubarr::{
    AppState,
    binaries::BinaryManager,
    config::Config,
    store::{Db, MappingKind, NewMapping},
};

const TOKEN: &str = "sonarr-hook-token";
const ADMIN_KEY: &str = "test-admin-key";
const SONARR_KEY: &str = "0123456789abcdef0123456789abcdef";
const SERIES_ID: i64 = 17;
const PLAY: &str = "http://tubarr:8480/play/0eD-GQCutAU";
const AUDIO: &str = "http://tubarr:8480/play/0eD-GQCutAU/audio\n";
const SHOW: &str = "pk-dramas/Kabhi Main Kabhi Tum (2024)";
const E2_1080: &str = "Season 1/Kabhi Main Kabhi Tum - S01E02 - Episode 2 WEBDL-1080p";
const E2_720: &str = "Season 1/Kabhi Main Kabhi Tum - S01E02 - Episode 2 WEBDL-720p";

struct Kit {
    app: Router,
    db: Db,
    mapping: i64,
    /// What Sonarr calls `/mnt/fk/data/media`.
    media: PathBuf,
    images: Images,
    _dir: tempfile::TempDir,
}

/// The fake image host: `0eD-GQCutAU` has a `maxresdefault`, `ldmPWRb-XcM` only an `hqdefault`,
/// and every request fails while `down` is set.
#[derive(Clone, Default)]
struct Images {
    url: String,
    hits: Arc<AtomicUsize>,
    down: Arc<AtomicBool>,
}

impl Images {
    async fn start() -> Self {
        let images = Self::default();
        let state = images.clone();
        let app = Router::new().route(
            "/vi/{id}/{file}",
            get(move |UrlPath((id, file)): UrlPath<(String, String)>| {
                let state = state.clone();
                async move {
                    state.hits.fetch_add(1, Ordering::SeqCst);
                    if state.down.load(Ordering::SeqCst) {
                        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                    }
                    let image = match (id.as_str(), file.as_str()) {
                        ("0eD-GQCutAU", "maxresdefault.jpg") => "maxres 0eD",
                        ("0eD-GQCutAU" | "ldmPWRb-XcM", "hqdefault.jpg") => "hq",
                        _ => return StatusCode::NOT_FOUND.into_response(),
                    };
                    ([("content-type", "image/jpeg")], image).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, ..images }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl Kit {
    /// `<media>/<show>/<name>.strm`.
    fn main(&self, name: &str) -> PathBuf {
        self.media.join(SHOW).join(format!("{name}.strm"))
    }

    fn audio(&self, name: &str) -> PathBuf {
        self.media
            .join(SHOW)
            .join(format!("{name} - Audio only.strm"))
    }

    fn put(&self, path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn thumb(&self, name: &str) -> PathBuf {
        self.media.join(SHOW).join(format!("{name}-thumb.jpg"))
    }

    async fn audio_version(&self, on: bool) {
        self.db.set_audio_version(self.mapping, on).await.unwrap();
    }

    async fn thumbnails(&self, on: bool) {
        self.db
            .set_episode_thumbnails(self.mapping, on)
            .await
            .unwrap();
    }

    async fn post(&self, path: &str) -> (StatusCode, Value) {
        let req = Request::post(path)
            .header("x-api-key", ADMIN_KEY)
            .body(Body::empty())
            .unwrap();
        send(&self.app, req).await
    }

    async fn hook(&self, payload: &Value, token: Option<&str>) -> (StatusCode, Value) {
        let mut req = Request::post("/hooks/sonarr").header("content-type", "application/json");
        if let Some(token) = token {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        send(
            &self.app,
            req.body(Body::from(payload.to_string())).unwrap(),
        )
        .await
    }

    async fn fixture(&self, name: &str) -> (StatusCode, Value) {
        let payload = support::fixture_json(&format!("sonarr_hooks/{name}.json"));
        self.hook(&payload, Some(TOKEN)).await
    }

    async fn patch(&self, id: i64, body: Value) -> (StatusCode, Value) {
        let req = Request::patch(format!("/admin/api/mappings/{id}"))
            .header("x-api-key", ADMIN_KEY)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        send(&self.app, req).await
    }
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// `hooks` is the `[hooks]` section, `sonarr` the fake's URL when a test needs one.
async fn kit_with(hooks: Option<&str>, sonarr: Option<&str>) -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let media = dir.path().join("media");
    std::fs::create_dir_all(&media).unwrap();
    let write = |name: &str, value: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, value).unwrap();
        path
    };
    let images = Images::start().await;
    let mut toml = format!(
        "public_url = \"http://tubarr:8480\"\ndata_dir = {:?}\napi_key_file = {:?}\n",
        dir.path(),
        write("admin_key", ADMIN_KEY)
    );
    if let Some(url) = sonarr {
        toml.push_str(&format!(
            "[sonarr]\nurl = \"{url}\"\napi_key_file = {:?}\n",
            write("sonarr_key", SONARR_KEY)
        ));
    }
    match hooks {
        Some(section) => toml.push_str(section),
        None => toml.push_str(&format!(
            "[hooks]\nsonarr_token_file = {:?}\nlibrary_paths = [[\"/mnt/fk/data/media\", {:?}]]\n\
             thumbnail_url = \"{}\"\n",
            write("hook_token", TOKEN),
            media,
            images.url
        )),
    }
    let config = Config::from_toml(&toml).unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let mapping = db
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
        .execute(db.pool())
        .await
        .unwrap();
    let binaries = std::sync::Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let app = tubarr::app(AppState::new(config, db.clone(), binaries).unwrap());
    Kit {
        app,
        db,
        mapping,
        media,
        images,
        _dir: dir,
    }
}

async fn kit() -> Kit {
    kit_with(None, None).await
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

#[tokio::test]
async fn download_writes_the_audio_version_only_when_on() {
    let k = kit().await;
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));

    let (status, body) = k.fixture("download").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["written"], 0);
    assert!(!k.audio(E2_1080).exists());

    k.audio_version(true).await;
    let (status, body) = k.fixture("download").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["written"], 1);
    assert_eq!(read(&k.audio(E2_1080)).as_deref(), Some(AUDIO));
    // The same import again changes nothing.
    let (_, body) = k.fixture("download").await;
    assert_eq!(body["written"], 0);
}

#[tokio::test]
async fn joined_parts_get_the_audio_of_the_same_parts() {
    let k = kit().await;
    k.audio_version(true).await;
    let parts = "http://tubarr:8480/play/0eD-GQCutAU,ldmPWRb-XcM";
    k.put(&k.main(E2_1080), &format!("{parts}\n"));
    k.fixture("download").await;
    assert_eq!(read(&k.audio(E2_1080)), Some(format!("{parts}/audio\n")));
}

#[tokio::test]
async fn no_audio_version_without_the_main_file_or_for_other_files() {
    let k = kit().await;
    k.audio_version(true).await;
    // Missing main file.
    let (status, body) = k.fixture("download").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["written"], 0);
    assert!(!k.audio(E2_1080).exists());
    // A .strm that is not Tubarr's.
    k.put(&k.main(E2_1080), "http://jellyfin.example/videos/1.mp4\n");
    let (_, body) = k.fixture("download").await;
    assert_eq!(body["written"], 0);
    assert!(!k.audio(E2_1080).exists());
    // A file someone else put where the audio version goes is left alone.
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));
    k.put(&k.audio(E2_1080), "mine\n");
    k.fixture("download").await;
    assert_eq!(read(&k.audio(E2_1080)).as_deref(), Some("mine\n"));
}

#[tokio::test]
async fn upgrade_replaces_the_old_audio_version() {
    let k = kit().await;
    k.audio_version(true).await;
    // Sonarr already removed the 720p file it replaced.
    k.put(&k.audio(E2_720), AUDIO);
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));
    let (status, body) = k.fixture("upgrade").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        (body["written"].clone(), body["removed"].clone()),
        (json!(1), json!(1))
    );
    assert!(!k.audio(E2_720).exists());
    assert_eq!(read(&k.audio(E2_1080)).as_deref(), Some(AUDIO));
}

#[tokio::test]
async fn upgrade_under_the_same_name_keeps_the_audio_version() {
    let k = kit().await;
    k.audio_version(true).await;
    let mut payload = support::fixture_json("sonarr_hooks/upgrade.json");
    payload["deletedFiles"][0]["path"] = payload["episodeFile"]["path"].clone();
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));
    k.put(&k.audio(E2_1080), AUDIO);
    let (status, _) = k.hook(&payload, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read(&k.audio(E2_1080)).as_deref(), Some(AUDIO));
}

#[tokio::test]
async fn rename_moves_the_audio_version() {
    let k = kit().await;
    k.audio_version(true).await;
    k.put(&k.audio(E2_720), AUDIO);
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));
    let (status, body) = k.fixture("rename").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!k.audio(E2_720).exists());
    assert_eq!(read(&k.audio(E2_1080)).as_deref(), Some(AUDIO));

    // With the option off a rename still takes the old one away and writes no new one.
    k.audio_version(false).await;
    k.put(&k.audio(E2_720), AUDIO);
    std::fs::remove_file(k.audio(E2_1080)).unwrap();
    k.fixture("rename").await;
    assert!(!k.audio(E2_720).exists() && !k.audio(E2_1080).exists());
}

#[tokio::test]
async fn deleting_the_episode_file_deletes_its_audio_version() {
    let k = kit().await;
    k.audio_version(true).await;
    k.put(&k.audio(E2_1080), AUDIO);
    let (status, body) = k.fixture("episode_file_delete").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["removed"], 1);
    assert!(!k.audio(E2_1080).exists());
}

#[tokio::test]
async fn deleting_the_series_clears_every_audio_version() {
    let k = kit().await;
    k.audio_version(true).await;
    let s2 = "Season 2/Kabhi Main Kabhi Tum - S02E01 - Episode 1 WEBDL-1080p";
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));
    k.put(&k.audio(E2_1080), AUDIO);
    k.put(&k.audio(s2), AUDIO);
    k.put(&k.audio(E2_720), "not ours\n");
    let (status, body) = k.fixture("series_delete").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["removed"], 2);
    assert!(!k.audio(E2_1080).exists() && !k.audio(s2).exists());
    assert!(k.audio(E2_720).exists());
    // Only audio versions go: the episode file is Sonarr's to delete.
    assert!(k.main(E2_1080).exists());
}

#[tokio::test]
async fn test_and_other_events_answer_ok_and_change_nothing() {
    let k = kit().await;
    let (status, body) = k.fixture("test").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ignored"], true);
    let (status, _) = k
        .hook(
            &json!({ "eventType": "Grab", "series": { "id": 17 } }),
            Some(TOKEN),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_bad_or_missing_token_is_refused() {
    let k = kit().await;
    k.audio_version(true).await;
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));
    let payload = support::fixture_json("sonarr_hooks/download.json");
    for token in [None, Some("wrong"), Some("")] {
        let (status, _) = k.hook(&payload, token).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{token:?}");
    }
    assert!(!k.audio(E2_1080).exists());

    // No token configured: the endpoint is off.
    let off = kit_with(Some(""), None).await;
    let (status, _) = off.hook(&payload, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn paths_outside_the_library_are_refused_and_nothing_changes() {
    let k = kit().await;
    k.audio_version(true).await;
    k.put(&k.audio(E2_720), AUDIO);
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));
    let mut payload = support::fixture_json("sonarr_hooks/upgrade.json");
    payload["episodeFile"]["path"] = json!("/mnt/other/Show/S01E02.strm");
    let (status, body) = k.hook(&payload, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["error"].as_str().unwrap().contains("/mnt/other"),
        "{body}"
    );
    // The replaced file listed first was not touched either.
    assert!(k.audio(E2_720).exists());

    payload["episodeFile"]["path"] = json!("/mnt/fk/data/media/../../etc/S01E02.strm");
    let (status, _) = k.hook(&payload, Some(TOKEN)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// A fake Sonarr answering `/api/v3/episodefile` for series 17 with the 1080p file and one outside
/// the library.
async fn fake_sonarr() -> String {
    async fn files(headers: HeaderMap) -> axum::response::Response {
        if headers.get("x-api-key").and_then(|v| v.to_str().ok()) != Some(SONARR_KEY) {
            return StatusCode::UNAUTHORIZED.into_response();
        }
        let root = format!("/mnt/fk/data/media/{SHOW}");
        Json(json!([
            { "id": 5502, "seriesId": 17, "seasonNumber": 1,
              "relativePath": format!("{E2_1080}.strm"), "path": format!("{root}/{E2_1080}.strm"),
              "size": 64, "dateAdded": "2026-10-24T06:00:00Z", "quality": { "quality": { "id": 3, "name": "WEBDL-1080p" } } },
            { "id": 5503, "seriesId": 17, "seasonNumber": 1,
              "relativePath": "Season 1/x.strm", "path": "/elsewhere/x.strm",
              "size": 64, "dateAdded": "2026-10-24T06:00:00Z" }
        ]))
        .into_response()
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().route("/api/v3/episodefile", get(files));
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test]
async fn turning_the_option_on_or_off_backfills_existing_episodes() {
    let sonarr = fake_sonarr().await;
    let k = kit_with(None, Some(&sonarr)).await;
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));

    let (status, body) = k.patch(k.mapping, json!({ "audio_version": true })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["audio_version"], true);
    assert_eq!(read(&k.audio(E2_1080)).as_deref(), Some(AUDIO));

    let (status, body) = k.patch(k.mapping, json!({ "audio_version": false })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["audio_version"], false);
    assert!(!k.audio(E2_1080).exists());
    assert!(k.main(E2_1080).exists());
}

#[tokio::test]
async fn a_film_cannot_have_an_audio_version() {
    let k = kit().await;
    let film =
        k.db.insert_mapping(&NewMapping {
            kind: MappingKind::Film,
            tvdb_id: None,
            tmdb_id: Some(352_173),
            title: "Theri".to_owned(),
            language: "Tamil".to_owned(),
            channel_ids: vec![],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        })
        .await
        .unwrap();
    let (status, body) = k.patch(film.id, json!({ "audio_version": true })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, body) = k
        .patch(film.id, json!({ "episode_thumbnails": true }))
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
}

#[tokio::test]
async fn download_saves_the_thumbnail_only_when_on_and_once() {
    let k = kit().await;
    k.audio_version(true).await;
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));

    let (status, body) = k.fixture("download").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["thumbnails"], 0);
    assert!(!k.thumb(E2_1080).exists());
    assert_eq!(k.images.hits(), 0);

    k.thumbnails(true).await;
    let (status, body) = k.fixture("download").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["thumbnails"], 1);
    // Named after the main file, so the audio only version has none of its own.
    assert_eq!(read(&k.thumb(E2_1080)).as_deref(), Some("maxres 0eD"));
    assert!(!k.thumb(&format!("{E2_1080} - Audio only")).exists());
    let hits = k.images.hits();

    // In place: never fetched again, and turning the option off leaves it.
    let (_, body) = k.fixture("download").await;
    assert_eq!(body["thumbnails"], 0);
    assert_eq!(k.images.hits(), hits);
    k.thumbnails(false).await;
    k.fixture("download").await;
    assert!(k.thumb(E2_1080).exists());
}

#[tokio::test]
async fn joined_parts_show_the_first_part_and_fall_back_to_hqdefault() {
    let k = kit().await;
    k.thumbnails(true).await;
    let parts = "http://tubarr:8480/play/ldmPWRb-XcM,0eD-GQCutAU";
    k.put(&k.main(E2_1080), &format!("{parts}\n"));
    let (status, body) = k.fixture("download").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(read(&k.thumb(E2_1080)).as_deref(), Some("hq"));
}

#[tokio::test]
async fn a_failed_fetch_never_fails_the_import_and_is_tried_again() {
    let k = kit().await;
    k.audio_version(true).await;
    k.thumbnails(true).await;
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));
    k.images.down.store(true, Ordering::SeqCst);

    let (status, body) = k.fixture("download").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["written"], 1);
    assert_eq!(body["thumbnails"], 0);
    assert!(!k.thumb(E2_1080).exists());
    // No half file or temporary file left behind.
    let names: Vec<_> = std::fs::read_dir(k.main(E2_1080).parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert!(names.iter().all(|n| !n.contains("thumb")), "{names:?}");

    k.images.down.store(false, Ordering::SeqCst);
    let (_, body) = k.fixture("download").await;
    assert_eq!(body["thumbnails"], 1);
    assert_eq!(read(&k.thumb(E2_1080)).as_deref(), Some("maxres 0eD"));
}

#[tokio::test]
async fn a_thumbnail_goes_with_its_file_and_moves_with_a_rename() {
    let k = kit().await;
    k.thumbnails(true).await;
    k.put(&k.thumb(E2_720), "maxres 0eD");
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));
    let (status, body) = k.fixture("rename").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!k.thumb(E2_720).exists());
    assert_eq!(read(&k.thumb(E2_1080)).as_deref(), Some("maxres 0eD"));
    // Moved, not fetched again.
    assert_eq!(k.images.hits(), 0);

    // Deleted by Sonarr: the thumbnail goes too, with the option on or off.
    k.thumbnails(false).await;
    std::fs::remove_file(k.main(E2_1080)).unwrap();
    let (status, body) = k.fixture("episode_file_delete").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!k.thumb(E2_1080).exists());
}

#[tokio::test]
async fn deleting_the_series_clears_thumbnails_whose_file_is_gone() {
    let k = kit().await;
    let s2 = "Season 2/Kabhi Main Kabhi Tum - S02E01 - Episode 1 WEBDL-1080p";
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));
    k.put(&k.thumb(E2_1080), "maxres 0eD");
    k.put(&k.thumb(s2), "maxres 0eD");
    let (status, body) = k.fixture("series_delete").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(k.thumb(E2_1080).exists());
    assert!(!k.thumb(s2).exists());
}

#[tokio::test]
async fn turning_thumbnails_on_and_the_endpoint_backfill_existing_episodes() {
    let sonarr = fake_sonarr().await;
    let k = kit_with(None, Some(&sonarr)).await;
    k.put(&k.main(E2_1080), &format!("{PLAY}\n"));
    let backfill = format!("/admin/api/mappings/{}/thumbnails", k.mapping);

    // Off: the endpoint refuses and fetches nothing.
    let (status, body) = k.post(&backfill).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(k.images.hits(), 0);

    let (status, body) = k
        .patch(k.mapping, json!({ "episode_thumbnails": true }))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["episode_thumbnails"], true);
    assert_eq!(read(&k.thumb(E2_1080)).as_deref(), Some("maxres 0eD"));

    // The endpoint fetches only what is missing.
    let (status, body) = k.post(&backfill).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!({ "written": 0, "failed": 0 }));
    std::fs::remove_file(k.thumb(E2_1080)).unwrap();
    k.images.down.store(true, Ordering::SeqCst);
    let (_, body) = k.post(&backfill).await;
    assert_eq!(body, json!({ "written": 0, "failed": 1 }));
    k.images.down.store(false, Ordering::SeqCst);
    let (_, body) = k.post(&backfill).await;
    assert_eq!(body, json!({ "written": 1, "failed": 0 }));
    assert!(k.thumb(E2_1080).exists());

    // Off again: the images stay.
    let (status, _) = k
        .patch(k.mapping, json!({ "episode_thumbnails": false }))
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(k.thumb(E2_1080).exists());
}
