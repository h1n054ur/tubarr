//! The Sonarr and Radarr client and the mappings sync, against a small fake arr serving
//! trimmed answers shaped like Sonarr 4.0.20 and Radarr 6.4.4 (tests/fixtures/arr/api).

mod support;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::Value;
use tubarr::{
    arr::{App, ArrError, ArrSync, Radarr, Sonarr, SyncError, sync_radarr, sync_sonarr},
    config::Config,
    store::{Db, MappingKind, NewMapping},
};

const KEY: &str = "0123456789abcdef0123456789abcdef";

/// What the fake arr serves. Tests change it between syncs.
struct FakeState {
    tags: Value,
    series: Value,
    movies: Value,
    /// The X-Api-Key header of every request, in order.
    keys_seen: Vec<Option<String>>,
}

#[derive(Clone)]
struct Fake(Arc<Mutex<FakeState>>);

impl Fake {
    fn sonarr() -> Self {
        Self::new(support::fixture_json("arr/api/sonarr/tag.json"))
    }

    fn radarr() -> Self {
        Self::new(support::fixture_json("arr/api/radarr/tag.json"))
    }

    fn new(tags: Value) -> Self {
        Self(Arc::new(Mutex::new(FakeState {
            tags,
            series: support::fixture_json("arr/api/sonarr/series.json"),
            movies: support::fixture_json("arr/api/radarr/movie.json"),
            keys_seen: Vec::new(),
        })))
    }

    fn edit(&self, f: impl FnOnce(&mut FakeState)) {
        f(&mut self.0.lock().unwrap());
    }

    fn keys_seen(&self) -> Vec<Option<String>> {
        self.0.lock().unwrap().keys_seen.clone()
    }

    /// Serves the fake under `/base/`, so the client must keep a URL base path. Returns that URL.
    async fn serve(&self) -> String {
        let api = Router::new()
            .route(
                "/api/v3/tag",
                get(|s, h| answer(s, h, |st| st.tags.clone())),
            )
            .route(
                "/api/v3/series",
                get(|s, h| answer(s, h, |st| st.series.clone())),
            )
            .route(
                "/api/v3/movie",
                get(|s, h| answer(s, h, |st| st.movies.clone())),
            )
            .route("/api/v3/episode", get(episodes))
            .with_state(self.clone());
        let app = Router::new().nest("/base", api);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/base")
    }
}

/// Records the key. The handlers answer 401 on a wrong one, like the arrs do.
fn key_ok(fake: &Fake, headers: &HeaderMap) -> bool {
    let key = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let ok = key.as_deref() == Some(KEY);
    fake.0.lock().unwrap().keys_seen.push(key);
    ok
}

async fn answer(
    State(fake): State<Fake>,
    headers: HeaderMap,
    body: impl FnOnce(&FakeState) -> Value,
) -> Response {
    if !key_ok(&fake, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let value = body(&fake.0.lock().unwrap());
    Json(value).into_response()
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct EpisodeQuery {
    series_id: i64,
}

async fn episodes(
    State(fake): State<Fake>,
    headers: HeaderMap,
    Query(q): Query<EpisodeQuery>,
) -> Response {
    if !key_ok(&fake, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if q.series_id == 11 {
        Json(support::fixture_json("arr/api/sonarr/episode-11.json")).into_response()
    } else {
        Json(Value::Array(vec![])).into_response()
    }
}

/// A config pointing Sonarr and/or Radarr at the given URLs, with `key` in the key files.
fn config(
    sonarr: Option<&str>,
    radarr: Option<&str>,
    key: &str,
    extra: &str,
) -> (Config, tempfile::TempDir) {
    let dir = support::temp_dir();
    let key_file = dir.path().join("key");
    std::fs::write(&key_file, key).unwrap();
    let mut toml = String::from("public_url = \"http://tubarr:8480\"\n");
    toml.push_str(extra);
    for (name, url) in [("sonarr", sonarr), ("radarr", radarr)] {
        if let Some(url) = url {
            toml.push_str(&format!(
                "\n[{name}]\nurl = \"{url}\"\napi_key_file = \"{}\"\n",
                key_file.display()
            ));
        }
    }
    (Config::from_toml(&toml).unwrap(), dir)
}

async fn fresh_db() -> (Db, tempfile::TempDir) {
    let dir = support::temp_dir();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    (db, dir)
}

async fn sonarr_at(url: &str) -> (Sonarr, tempfile::TempDir) {
    let (config, dir) = config(Some(url), None, KEY, "");
    (Sonarr::new(config.sonarr.as_ref().unwrap()).unwrap(), dir)
}

async fn radarr_at(url: &str) -> (Radarr, tempfile::TempDir) {
    let (config, dir) = config(None, Some(url), KEY, "");
    (Radarr::new(config.radarr.as_ref().unwrap()).unwrap(), dir)
}

/// A URL where nothing listens.
async fn dead_url() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

#[tokio::test]
async fn tag_lookup_ignores_case_and_sends_the_key() {
    let fake = Fake::sonarr();
    let (sonarr, _k) = sonarr_at(&fake.serve().await).await;
    assert_eq!(sonarr.tag_id("youtube").await.unwrap(), 3);
    assert_eq!(sonarr.tag_id("YouTube").await.unwrap(), 3);
    assert!(fake.keys_seen().iter().all(|k| k.as_deref() == Some(KEY)));
}

#[tokio::test]
async fn missing_tag_says_what_to_do() {
    let fake = Fake::radarr();
    let (radarr, _k) = radarr_at(&fake.serve().await).await;
    let err = radarr.tag_id("tubarr").await.unwrap_err();
    assert!(matches!(
        err,
        ArrError::TagMissing {
            app: App::Radarr,
            ..
        }
    ));
    let text = err.to_string();
    assert!(
        text.contains("Radarr has no tag \"tubarr\"") && text.contains("films"),
        "{text}"
    );
}

#[tokio::test]
async fn only_tagged_titles_are_listed() {
    let fake = Fake::sonarr();
    let url = fake.serve().await;
    let (sonarr, _k) = sonarr_at(&url).await;
    let titles: Vec<_> = sonarr
        .tagged_series("youtube")
        .await
        .unwrap()
        .into_iter()
        .map(|s| s.title)
        .collect();
    assert_eq!(titles, ["Kabhi Main Kabhi Tum", "Khabarhar"]);

    let (radarr, _k) = radarr_at(&Fake::radarr().serve().await).await;
    let ids: Vec<_> = radarr
        .tagged_movies("youtube")
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.tmdb_id)
        .collect();
    assert_eq!(ids, [352_173, 471_507]);
    assert_eq!(radarr.tagged_movies("4k").await.unwrap().len(), 2);
}

#[tokio::test]
async fn episodes_and_wanted() {
    let fake = Fake::sonarr();
    let url = fake.serve().await;
    let (sonarr, _k) = sonarr_at(&url).await;
    let all = sonarr.episodes(11).await.unwrap();
    assert_eq!(all.len(), 4);
    assert_eq!(all[0].air_date.as_deref(), Some("2024-07-02"));
    assert!(all[3].air_date.is_none());

    let wanted: Vec<_> = sonarr
        .wanted_episodes(11)
        .await
        .unwrap()
        .into_iter()
        .map(|e| (e.season_number, e.episode_number))
        .collect();
    assert_eq!(wanted, [(1, 2), (1, 4)]);

    let (radarr, _k) = radarr_at(&Fake::radarr().serve().await).await;
    let wanted: Vec<_> = radarr
        .wanted_movies("youtube")
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.title)
        .collect();
    assert_eq!(wanted, ["Theri"]);
}

#[tokio::test]
async fn sync_creates_prefilled_mappings() {
    let (db, _d) = fresh_db().await;
    let sonarr_fake = Fake::sonarr();
    let (sonarr, _k) = sonarr_at(&sonarr_fake.serve().await).await;
    let report = sync_sonarr(&db, &sonarr, "youtube").await.unwrap();
    assert_eq!(report.tagged, 2);
    assert_eq!(report.counts.changed, 2);

    let drama = db.mapping_by_tvdb(452_001).await.unwrap().unwrap();
    assert_eq!(drama.title, "Kabhi Main Kabhi Tum");
    assert_eq!(drama.kind, MappingKind::Show);
    assert_eq!(drama.arr_id, Some(11));
    assert_eq!(drama.year, Some(2024));
    assert_eq!(drama.imdb_id, None, "an empty imdbId is no id");
    assert_eq!(drama.language, "English");
    assert_eq!(drama.tmdb_id, None, "TV and film TMDB ids must not mix");
    assert!(drama.active);
    assert!(drama.channel_ids.is_empty());

    let daily = db.mapping_by_tvdb(389_450).await.unwrap().unwrap();
    assert_eq!(daily.kind, MappingKind::Daily);
    assert_eq!(daily.year, None);
    assert_eq!(daily.language, "Unknown");
    assert!(db.mapping_by_tvdb(371_980).await.unwrap().is_none());

    let (radarr, _k) = radarr_at(&Fake::radarr().serve().await).await;
    sync_radarr(&db, &radarr, "youtube").await.unwrap();
    let theri = db.mapping_by_tmdb(352_173).await.unwrap().unwrap();
    assert_eq!(theri.kind, MappingKind::Film);
    assert_eq!(theri.year, Some(2016));
    assert_eq!(theri.imdb_id.as_deref(), Some("tt4807012"));
    assert_eq!(theri.language, "Tamil");
    assert!(db.mapping_by_tmdb(693_134).await.unwrap().is_none());
    assert_eq!(db.mappings().await.unwrap().len(), 4);
}

#[tokio::test]
async fn tag_removed_or_deleted_marks_inactive_and_retag_revives() {
    let (db, _d) = fresh_db().await;
    let fake = Fake::sonarr();
    let (sonarr, _k) = sonarr_at(&fake.serve().await).await;
    sync_sonarr(&db, &sonarr, "youtube").await.unwrap();

    // The drama loses its tag, the daily show is deleted from Sonarr.
    fake.edit(|st| {
        let list = st.series.as_array_mut().unwrap();
        list[0]["tags"] = serde_json::json!([]);
        list.remove(1);
    });
    let report = sync_sonarr(&db, &sonarr, "youtube").await.unwrap();
    assert_eq!(report.tagged, 0);
    assert_eq!(report.counts.deactivated, 2);
    let drama = db.mapping_by_tvdb(452_001).await.unwrap().unwrap();
    assert!(!drama.active);
    assert!(!db.mapping_by_tvdb(389_450).await.unwrap().unwrap().active);
    assert_eq!(db.mappings().await.unwrap().len(), 2, "rows are kept");

    fake.edit(|st| st.series[0]["tags"] = serde_json::json!([3]));
    let report = sync_sonarr(&db, &sonarr, "youtube").await.unwrap();
    assert_eq!(report.counts.changed, 1);
    let back = db.mapping_by_tvdb(452_001).await.unwrap().unwrap();
    assert!(back.active);
    assert_eq!(back.id, drama.id);
}

#[tokio::test]
async fn tubarr_owned_fields_survive_a_sync() {
    let (db, _d) = fresh_db().await;
    let fake = Fake::sonarr();
    let (sonarr, _k) = sonarr_at(&fake.serve().await).await;
    sync_sonarr(&db, &sonarr, "youtube").await.unwrap();
    let synced = db.mapping_by_tvdb(452_001).await.unwrap().unwrap();

    let edited = NewMapping {
        kind: synced.kind,
        tvdb_id: synced.tvdb_id,
        tmdb_id: None,
        title: synced.title.clone(),
        // Sonarr has no Urdu, so Hani sets it here.
        language: "Urdu".to_owned(),
        channel_ids: vec!["UC4JCksJF76g_MdzPVBJoC3Q".to_owned()],
        episode_regex: Some(r"Ep(?:isode)?\s*(?P<episode>\d+)".to_owned()),
        excludes: vec!["Teaser".to_owned(), "Promo".to_owned()],
        min_duration_secs: Some(25 * 60),
    };
    db.update_mapping(synced.id, &edited)
        .await
        .unwrap()
        .unwrap();

    // Sonarr renames the series and turns it into a daily show.
    fake.edit(|st| {
        st.series[0]["title"] = "Kabhi Main Kabhi Tum (2024)".into();
        st.series[0]["seriesType"] = "daily".into();
    });
    sync_sonarr(&db, &sonarr, "youtube").await.unwrap();
    let after = db.mapping(synced.id).await.unwrap().unwrap();
    assert_eq!(after.title, "Kabhi Main Kabhi Tum (2024)");
    assert_eq!(after.kind, MappingKind::Daily);
    assert_eq!(after.language, "Urdu");
    assert_eq!(after.channel_ids, edited.channel_ids);
    assert_eq!(after.episode_regex, edited.episode_regex);
    assert_eq!(after.excludes, edited.excludes);
    assert_eq!(after.min_duration_secs, Some(25 * 60));
}

#[tokio::test]
async fn alternate_titles_become_aliases_and_user_aliases_survive() {
    let (db, _d) = fresh_db().await;
    let fake = Fake::sonarr();
    let (sonarr, _k) = sonarr_at(&fake.serve().await).await;
    fake.edit(|st| {
        st.series[0]["alternateTitles"] = serde_json::json!([
            { "title": "KMKT", "seasonNumber": -1 },
            { "title": "Kabhi Main Kabhi Tum", "seasonNumber": -1 },
            { "seasonNumber": 1 }
        ]);
    });
    let first = sync_sonarr(&db, &sonarr, "youtube").await.unwrap();
    let drama = db.mapping_by_tvdb(452_001).await.unwrap().unwrap();
    assert_eq!(
        db.aliases(drama.id).await.unwrap(),
        ["KMKT"],
        "the title itself is no alias"
    );

    // The user adds one, Sonarr renames its own on the next sync.
    db.set_aliases(
        drama.id,
        &["KMKT".to_owned(), "Kabhi Mein Kabhi Tum".to_owned()],
    )
    .await
    .unwrap();
    fake.edit(|st| {
        st.series[0]["alternateTitles"] =
            serde_json::json!([{ "title": "K.M.K.T", "seasonNumber": -1 }]);
    });
    let again = sync_sonarr(&db, &sonarr, "youtube").await.unwrap();
    assert_eq!(
        db.aliases(drama.id).await.unwrap(),
        ["K.M.K.T", "Kabhi Mein Kabhi Tum"]
    );
    assert_eq!(first.counts.changed, 2);
    assert_eq!(
        again.counts.changed, 0,
        "alias changes leave the mapping as it was"
    );

    // Sonarr lists none: only the user's stays.
    fake.edit(|st| st.series[0]["alternateTitles"] = serde_json::json!([]));
    sync_sonarr(&db, &sonarr, "youtube").await.unwrap();
    assert_eq!(
        db.aliases(drama.id).await.unwrap(),
        ["Kabhi Mein Kabhi Tum"]
    );
}

#[tokio::test]
async fn arr_down_is_an_error_and_changes_nothing() {
    let (db, _d) = fresh_db().await;
    let (sonarr, _k) = sonarr_at(&Fake::sonarr().serve().await).await;
    sync_sonarr(&db, &sonarr, "youtube").await.unwrap();

    let (down, _k) = sonarr_at(&dead_url().await).await;
    let err = sync_sonarr(&db, &down, "youtube").await.unwrap_err();
    assert!(
        matches!(
            err,
            SyncError::Arr(ArrError::Unreachable {
                app: App::Sonarr,
                ..
            })
        ),
        "{err}"
    );
    assert!(err.to_string().contains("cannot be reached"), "{err}");
    assert!(db.mappings().await.unwrap().iter().all(|m| m.active));
}

#[tokio::test]
async fn missing_tag_changes_nothing() {
    let (db, _d) = fresh_db().await;
    let fake = Fake::sonarr();
    let (sonarr, _k) = sonarr_at(&fake.serve().await).await;
    sync_sonarr(&db, &sonarr, "youtube").await.unwrap();
    // Someone deletes the tag in Sonarr: that must not look like "nothing is tagged".
    fake.edit(|st| st.tags = serde_json::json!([{ "id": 1, "label": "torbox" }]));
    let err = sync_sonarr(&db, &sonarr, "youtube").await.unwrap_err();
    assert!(matches!(err, SyncError::Arr(ArrError::TagMissing { .. })));
    assert!(db.mappings().await.unwrap().iter().all(|m| m.active));
}

/// Collects everything tracing writes while the guard lives.
#[derive(Clone, Default)]
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuffer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

#[tokio::test]
async fn the_api_key_is_never_logged_or_shown() {
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let (db, _d) = fresh_db().await;
    let sonarr_url = Fake::sonarr().serve().await;
    let radarr_url = Fake::radarr().serve().await;

    // Right key on both: a full round at trace level.
    let (good, _k) = config(Some(&sonarr_url), Some(&radarr_url), KEY, "");
    let sync = ArrSync::from_config(&good, db.clone()).unwrap();
    assert!(!format!("{sync:?}").contains(KEY));
    sync.sync_once().await;
    assert_eq!(db.mappings().await.unwrap().len(), 4);

    // A wrong key on Sonarr and a dead Radarr: both fail, both are logged, nothing panics.
    let wrong = "ffffffffffffffffffffffffffffffff";
    let dead = dead_url().await;
    let (bad, _k) = config(Some(&sonarr_url), Some(&dead), wrong, "");
    let sync = ArrSync::from_config(&bad, db.clone()).unwrap();
    sync.sync_once().await;

    let text = logs.text();
    assert!(text.contains("refused the API key"), "{text}");
    assert!(text.contains("Radarr at"), "{text}");
    assert!(!text.contains(KEY), "the key leaked into the logs");
    assert!(!text.contains(wrong), "the key leaked into the logs");
    assert!(db.mappings().await.unwrap().iter().all(|m| m.active));
}

#[tokio::test]
async fn sync_is_idle_without_an_arr_and_reads_the_tag_setting() {
    let (db, _d) = fresh_db().await;
    let (none, _k) = config(None, None, KEY, "");
    ArrSync::from_config(&none, db.clone())
        .unwrap()
        .sync_once()
        .await;
    assert!(db.mappings().await.unwrap().is_empty());

    // A custom tag label from [arr].
    let fake = Fake::new(serde_json::json!([{ "id": 3, "label": "tubarr" }]));
    let (custom, _k) = config(
        Some(&fake.serve().await),
        None,
        KEY,
        "[arr]\ntag = \"Tubarr\"\n",
    );
    ArrSync::from_config(&custom, db.clone())
        .unwrap()
        .sync_once()
        .await;
    assert_eq!(db.mappings().await.unwrap().len(), 2);
}

#[tokio::test]
async fn shows_on_the_arr_default_language_take_their_channel_language() {
    const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";
    let (db, _d) = fresh_db().await;
    let url = Fake::sonarr().serve().await;
    let extra = format!("[channels.{ARY}]\nnetwork = \"ARY Digital\"\nlanguage = \"Urdu\"\n");
    let (config, _k) = config(Some(&url), None, KEY, &extra);
    let sync = ArrSync::from_config(&config, db.clone()).unwrap();
    sync.sync_once().await;

    // Sonarr has no Urdu and reports the drama as English. Without channels it stays so.
    let drama = db.mapping_by_tvdb(452_001).await.unwrap().unwrap();
    assert_eq!(drama.language, "English");

    // Channels set before this version (or outside the admin PATCH): the next sync fixes it. A
    // language the user set on the daily show stays, though its channel is ARY too.
    let daily = db.mapping_by_tvdb(389_450).await.unwrap().unwrap();
    for (id, sql) in [
        (drama.id, "UPDATE mappings SET channel_ids = ? WHERE id = ?"),
        (
            daily.id,
            "UPDATE mappings SET channel_ids = ?, language = 'Punjabi', \
             language_source = 'user' WHERE id = ?",
        ),
    ] {
        sqlx::query(sql)
            .bind(serde_json::json!([ARY]).to_string())
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
    }
    sync.sync_once().await;
    assert_eq!(
        db.mapping(drama.id).await.unwrap().unwrap().language,
        "Urdu"
    );
    assert_eq!(
        db.mapping(daily.id).await.unwrap().unwrap().language,
        "Punjabi"
    );

    // Once set, the language is Tubarr's: Sonarr still says English on every later sync.
    sync.sync_once().await;
    assert_eq!(
        db.mapping(drama.id).await.unwrap().unwrap().language,
        "Urdu"
    );
}
