//! The draft helper: the existence check through Sonarr and Radarr (and TVDB and TMDB directly
//! when keys are set), and show and film drafts from stored videos. Everything runs against local
//! fakes.

mod support;

use std::{
    io::Write,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use tubarr::{
    AppState,
    binaries::BinaryManager,
    config::Config,
    store::{Db, PlaylistItem, SeenVideo, VideoSource},
};

const ADMIN_KEY: &str = "test-admin-key";
const ARR_KEY: &str = "0123456789abcdef0123456789abcdef";
const TVDB_KEY: &str = "tvdb-project-key-5f2a";
const TVDB_PIN: &str = "tvdb-pin-9931";
const TMDB_TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.tmdb-read-token";
const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";
/// 2026-10-05 19:00 UTC, a Monday evening in Pakistan.
const T0: i64 = 1_791_226_800;
const DAY: i64 = 86_400;

/// Held by the tests whose failed checks log a warning, so the log test sees its own warnings:
/// tracing caches per callsite whether anyone listens, and a test without a subscriber hitting
/// the same callsite at the same time spoils that for the log test.
static WARNS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Default)]
struct FakeState {
    /// Every search answers 429 while set.
    rate_limited: bool,
    tvdb_logins: usize,
    /// Every term the lookups were asked for, as `source:term`.
    terms: Vec<String>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeState>>);

#[derive(serde::Deserialize)]
struct Term {
    #[serde(alias = "query")]
    term: String,
}

/// The fixture when the term names its title, else an empty answer, like the real searches.
fn answer_for(term: &str, title: &str, fixture: &str, empty: Value) -> Value {
    if term.eq_ignore_ascii_case(title) {
        support::fixture_json(&format!("drafts/{fixture}"))
    } else {
        empty
    }
}

impl Fake {
    fn record(&self, source: &str, term: &str) -> bool {
        let mut st = self.0.lock().unwrap();
        st.terms.push(format!("{source}:{term}"));
        st.rate_limited
    }

    fn set_rate_limited(&self, on: bool) {
        self.0.lock().unwrap().rate_limited = on;
    }

    fn terms(&self) -> Vec<String> {
        self.0.lock().unwrap().terms.clone()
    }

    /// Sonarr, Radarr, TVDB and TMDB on one port, each under its own prefix.
    async fn serve(&self) -> String {
        let app = Router::new()
            .route("/sonarr/api/v3/series/lookup", get(sonarr_lookup))
            .route("/radarr/api/v3/movie/lookup", get(radarr_lookup))
            .route("/tvdb/login", post(tvdb_login))
            .route("/tvdb/search", get(tvdb_search))
            .route("/tmdb/search/movie", get(tmdb_movie))
            .route("/tmdb/search/tv", get(tmdb_tv))
            .with_state(self.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers
        .get(name)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
}

fn too_many() -> Response {
    (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "30")]).into_response()
}

async fn sonarr_lookup(
    State(fake): State<Fake>,
    headers: HeaderMap,
    Query(q): Query<Term>,
) -> Response {
    if header(&headers, "x-api-key") != ARR_KEY {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if fake.record("sonarr", &q.term) {
        return too_many();
    }
    Json(answer_for(
        &q.term,
        "Tere Bin",
        "sonarr-series-lookup.json",
        json!([]),
    ))
    .into_response()
}

async fn radarr_lookup(
    State(fake): State<Fake>,
    headers: HeaderMap,
    Query(q): Query<Term>,
) -> Response {
    if header(&headers, "x-api-key") != ARR_KEY {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if fake.record("radarr", &q.term) {
        return too_many();
    }
    Json(answer_for(
        &q.term,
        "Ishq Hua",
        "radarr-movie-lookup.json",
        json!([]),
    ))
    .into_response()
}

async fn tvdb_login(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
    fake.0.lock().unwrap().tvdb_logins += 1;
    if body["apikey"] == TVDB_KEY && body["pin"] == TVDB_PIN {
        Json(support::fixture_json("drafts/tvdb-login.json")).into_response()
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"status": "failure", "message": "Unauthorized", "data": null})),
        )
            .into_response()
    }
}

async fn tvdb_search(
    State(fake): State<Fake>,
    headers: HeaderMap,
    Query(q): Query<Term>,
) -> Response {
    if header(&headers, "authorization") != "Bearer tvdb-session-token" {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    if fake.record("tvdb", &q.term) {
        return too_many();
    }
    let empty = json!({"status": "success", "data": [], "links": {"total_items": 0}});
    Json(answer_for(&q.term, "Tere Bin", "tvdb-search.json", empty)).into_response()
}

async fn tmdb(fake: &Fake, headers: &HeaderMap, q: &Term, source: &str, title: &str) -> Response {
    if header(headers, "authorization") != format!("Bearer {TMDB_TOKEN}") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"status_code": 7, "status_message": "Invalid API key", "success": false})),
        )
            .into_response();
    }
    if fake.record(source, &q.term) {
        return too_many();
    }
    let empty = json!({"page": 1, "results": [], "total_pages": 1, "total_results": 0});
    Json(answer_for(&q.term, title, &format!("{source}.json"), empty)).into_response()
}

async fn tmdb_movie(
    State(fake): State<Fake>,
    headers: HeaderMap,
    Query(q): Query<Term>,
) -> Response {
    tmdb(&fake, &headers, &q, "tmdb-search-movie", "Ishq Hua").await
}

async fn tmdb_tv(State(fake): State<Fake>, headers: HeaderMap, Query(q): Query<Term>) -> Response {
    tmdb(&fake, &headers, &q, "tmdb-search-tv", "Tere Bin").await
}

struct Kit {
    app: Router,
    db: Db,
    _dir: tempfile::TempDir,
}

/// What the kit connects: Sonarr and Radarr, and direct keys for TVDB and TMDB.
#[derive(Default)]
struct Setup {
    arrs: bool,
    keys: bool,
    /// Key files named but missing on disk.
    missing_keys: bool,
}

async fn kit(fake_url: Option<&str>, setup: Setup) -> Kit {
    let dir = support::temp_dir();
    let write = |name: &str, value: &str| {
        let path = dir.path().join(name);
        std::fs::write(&path, value).unwrap();
        path
    };
    let admin = write("admin-key", ADMIN_KEY);
    let mut toml = format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\napi_key_file = {admin:?}\n",
        dir.path()
    );
    let url = fake_url.unwrap_or("http://127.0.0.1:9");
    if setup.arrs {
        let arr = write("arr-key", ARR_KEY);
        for name in ["sonarr", "radarr"] {
            toml.push_str(&format!(
                "\n[{name}]\nurl = \"{url}/{name}\"\napi_key_file = {arr:?}\n"
            ));
        }
    }
    toml.push_str(&format!(
        "\n[drafts]\ntvdb_url = \"{url}/tvdb\"\ntmdb_url = \"{url}/tmdb\"\n"
    ));
    if setup.keys {
        let tvdb = write("tvdb-key", TVDB_KEY);
        let pin = write("tvdb-pin", TVDB_PIN);
        let tmdb = write("tmdb-token", TMDB_TOKEN);
        toml.push_str(&format!(
            "tvdb_api_key_file = {tvdb:?}\ntvdb_pin_file = {pin:?}\ntmdb_read_token_file = {tmdb:?}\n"
        ));
    }
    if setup.missing_keys {
        let gone = dir.path().join("not-there");
        toml.push_str(&format!(
            "tvdb_api_key_file = {gone:?}\ntmdb_read_token_file = {gone:?}\n"
        ));
    }
    toml.push_str(&format!(
        "\n[drafts.channels.{ARY}]\nnetwork = \"ARY Digital\"\ncountry = \"Pakistan\"\nlanguage = \"Urdu\"\nutc_offset_mins = 300\n"
    ));
    let config = Config::from_toml(&toml).unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let app = tubarr::app(AppState::new(config, db.clone(), binaries).unwrap());
    Kit { app, db, _dir: dir }
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("x-api-key", ADMIN_KEY);
    let body = match body {
        Some(v) => {
            req = req.header("content-type", "application/json");
            Body::from(v.to_string())
        }
        None => Body::empty(),
    };
    let res = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn source<'a>(result: &'a Value, name: &str) -> &'a Value {
    result["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["source"] == name)
        .unwrap_or_else(|| panic!("no {name} in {result}"))
}

#[tokio::test]
async fn sonarr_decides_a_show_without_any_key() {
    let fake = Fake::default();
    let url = fake.serve().await;
    let kit = kit(
        Some(&url),
        Setup {
            arrs: true,
            ..Setup::default()
        },
    )
    .await;

    let (status, found) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/check",
        Some(json!({"kind": "show", "title": "Tere Bin"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["verdict"], "found");
    assert_eq!(found["best"]["source"], "sonarr");
    assert_eq!(found["best"]["id"], "424242");
    assert_eq!(found["best"]["year"], 2022);
    assert_eq!(found["best"]["in_arr"], false);
    assert_eq!(
        found["best"]["language"],
        Value::Null,
        "Unknown is no language"
    );
    let sonarr = source(&found, "sonarr");
    assert_eq!(sonarr["state"], "checked");
    // The entry without a TVDB id is not on TVDB.
    assert_eq!(sonarr["candidates"].as_array().unwrap().len(), 2);
    assert_eq!(source(&found, "tvdb_series")["state"], "not_checked");
    assert!(
        found["summary"]
            .as_str()
            .unwrap()
            .starts_with("Already on TVDB")
    );

    let (_, missing) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/check",
        Some(json!({"kind": "show", "title": "Kabhi Main Kabhi Tum"})),
    )
    .await;
    assert_eq!(missing["verdict"], "probably_missing");
    assert_eq!(missing["best"], Value::Null);
    assert!(
        fake.terms()
            .contains(&"sonarr:Kabhi Main Kabhi Tum".to_owned())
    );
}

#[tokio::test]
async fn radarr_decides_a_film_and_flags_tv_films() {
    let fake = Fake::default();
    let url = fake.serve().await;
    let kit = kit(
        Some(&url),
        Setup {
            arrs: true,
            ..Setup::default()
        },
    )
    .await;
    let (status, found) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/check",
        Some(json!({"kind": "film", "title": "Ishq Hua", "year": 2019})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["verdict"], "found");
    assert_eq!(found["best"]["source"], "radarr");
    assert_eq!(found["best"]["id"], "612345");
    assert_eq!(found["best"]["tv_film"], true);
    assert_eq!(found["best"]["original_name"], "عشق ہوا");
    let radarr = source(&found, "radarr");
    let theri = radarr["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Theri")
        .unwrap();
    assert_eq!(theri["in_arr"], true);
    assert_eq!(theri["year_differs"], true);
}

#[tokio::test]
async fn direct_keys_log_in_once_and_search() {
    let _warns = WARNS.lock().await;
    let fake = Fake::default();
    let url = fake.serve().await;
    let kit = kit(
        Some(&url),
        Setup {
            keys: true,
            ..Setup::default()
        },
    )
    .await;
    for _ in 0..2 {
        let (status, found) = call(
            &kit.app,
            "POST",
            "/admin/api/drafts/check",
            Some(json!({"kind": "show", "title": "Tere Bin"})),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{found}");
        assert_eq!(found["verdict"], "found");
        assert_eq!(found["best"]["source"], "tvdb_series");
        assert_eq!(found["best"]["original_name"], "تیرے بن");
        assert_eq!(source(&found, "sonarr")["state"], "not_checked");
        let tmdb_tv = source(&found, "tmdb_tv");
        assert_eq!(tmdb_tv["candidates"][0]["country"], "PK");
    }
    assert_eq!(fake.0.lock().unwrap().tvdb_logins, 1, "the token is kept");

    let (_, film) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/check",
        Some(json!({"kind": "film", "title": "Ishq Hua"})),
    )
    .await;
    assert_eq!(film["verdict"], "found");
    assert_eq!(film["best"]["source"], "tmdb_movie");
    assert_eq!(film["best"]["tv_film"], true);

    // Rate limited everywhere: the answer says so instead of claiming the title is missing.
    fake.set_rate_limited(true);
    let (status, limited) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/check",
        Some(json!({"kind": "show", "title": "Tere Bin"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(limited["verdict"], "unknown");
    let tvdb = source(&limited, "tvdb_series");
    assert_eq!(tvdb["state"], "failed");
    assert_eq!(tvdb["reason"], "rate limited, retry after 30s");
}

#[tokio::test]
async fn missing_key_files_leave_the_check_undone_but_drafts_work() {
    let kit = kit(
        None,
        Setup {
            missing_keys: true,
            ..Setup::default()
        },
    )
    .await;
    let (status, result) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/check",
        Some(json!({"kind": "film", "title": "Ishq Hua"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(result["verdict"], "not_checked");
    let tmdb = source(&result, "tmdb_movie");
    assert_eq!(tmdb["state"], "not_checked");
    assert!(
        tmdb["reason"]
            .as_str()
            .unwrap()
            .contains("drafts.tmdb_read_token_file")
    );

    seed_film(&kit.db).await;
    let (status, draft) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/film",
        Some(json!({"video_id": "fIlm0000001"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{draft}");
    assert!(
        draft["text"]
            .as_str()
            .unwrap()
            .starts_with("Not checked against TMDB")
    );

    // The list says why the check cannot ask, and a draft can be deleted.
    let (status, list) = call(&kit.app, "GET", "/admin/api/drafts?q=ishq", None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["title"], "Ishq Hua");
    assert_eq!(list["items"][0]["kind"], "film");
    assert_eq!(list["items"][0]["verdict"], "not_checked");
    assert_eq!(list["keys"]["tmdb"]["ready"], false);
    assert!(
        list["keys"]["tvdb"]["reason"]
            .as_str()
            .unwrap()
            .contains("drafts.tvdb_api_key_file")
    );
    let (_, none) = call(&kit.app, "GET", "/admin/api/drafts?kind=show", None).await;
    assert_eq!(none["total"], 0);
    let (status, _) = call(&kit.app, "GET", "/admin/api/drafts?limit=0", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let path = format!("/admin/api/drafts/{}", draft["id"]);
    assert_eq!(
        call(&kit.app, "DELETE", &path, None).await.0,
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        call(&kit.app, "GET", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        call(&kit.app, "DELETE", &path, None).await.0,
        StatusCode::NOT_FOUND
    );
}

async fn seen(db: &Db, id: &str, title: &str, mins: i64, at: i64) {
    seen_on(db, ARY, id, title, mins, at).await;
}

async fn seen_on(db: &Db, channel: &str, id: &str, title: &str, mins: i64, at: i64) {
    db.upsert_seen_video(&SeenVideo {
        video_id: id.to_owned(),
        channel_id: channel.to_owned(),
        title: title.to_owned(),
        published_at: Some(at),
        published_approx: false,
        duration_secs: Some(mins * 60),
        source: VideoSource::Rss,
    })
    .await
    .unwrap();
}

/// A drama on ARY with the usual noise: a subtitled copy, an episode in two parts, extras, a
/// short clip, a last episode without a number and another drama on the same channel.
async fn seed_drama(db: &Db) {
    let show = "Kabhi Main Kabhi Tum";
    seen(
        db,
        "ep1clean000",
        &format!("{show} Episode 1 | کبھی میں کبھی تم | ARY Digital"),
        38,
        T0,
    )
    .await;
    seen(
        db,
        "ep1engsub00",
        &format!("{show} Episode 1 [Eng Sub] | ARY Digital"),
        38,
        T0 + 3600,
    )
    .await;
    seen(
        db,
        "ep2part1000",
        &format!("{show} Episode 2 Part 1 | ARY Digital"),
        20,
        T0 + 7 * DAY,
    )
    .await;
    seen(
        db,
        "ep2part2000",
        &format!("{show} Episode 2 Part 2 | ARY Digital"),
        19,
        T0 + 7 * DAY + 60,
    )
    .await;
    seen(
        db,
        "ep3wedding0",
        &format!("{show} Episode 3 | The Wedding | ARY Digital"),
        40,
        T0 + 14 * DAY,
    )
    .await;
    seen(
        db,
        "ep4promo000",
        &format!("{show} Episode 4 Promo | ARY Digital"),
        1,
        T0 + 15 * DAY,
    )
    .await;
    seen(
        db,
        "teaser10000",
        &format!("{show} Teaser 1 | ARY Digital"),
        1,
        T0 - 7 * DAY,
    )
    .await;
    seen(
        db,
        "ep3clip0000",
        &format!("{show} Episode 3 | Best Moments"),
        12,
        T0 + 15 * DAY,
    )
    .await;
    seen(
        db,
        "lastep00000",
        &format!("{show} Last Episode | ARY Digital"),
        42,
        T0 + 21 * DAY,
    )
    .await;
    seen(
        db,
        "terebin0005",
        "Tere Bin Episode 5 | ARY Digital",
        40,
        T0,
    )
    .await;
}

#[tokio::test]
async fn show_draft_from_stored_channel_videos() {
    let fake = Fake::default();
    let url = fake.serve().await;
    let kit = kit(
        Some(&url),
        Setup {
            arrs: true,
            ..Setup::default()
        },
    )
    .await;
    seed_drama(&kit.db).await;
    let before = fake.terms().len();

    let (status, stored) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/show",
        Some(json!({"channel_id": ARY, "title": "Kabhi Main Kabhi Tum"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stored}");
    assert_eq!(stored["kind"], "show");
    let draft = &stored["draft"];
    assert_eq!(draft["series_name"], "Kabhi Main Kabhi Tum");
    assert_eq!(draft["original_title"], "کبھی میں کبھی تم");
    assert_eq!(draft["network"], "ARY Digital");
    assert_eq!(draft["country"], "Pakistan");
    assert_eq!(draft["original_language"], "Urdu");
    assert_eq!(draft["first_aired"], "2026-10-05");
    assert_eq!(draft["status"], "Ended");
    assert_eq!(draft["numbering"], "title");
    assert_eq!(draft["check"]["verdict"], "probably_missing");
    // Only the existence check asked anything, and only Sonarr (no YouTube lookups).
    assert_eq!(
        fake.terms()[before..],
        ["sonarr:Kabhi Main Kabhi Tum".to_owned()]
    );

    let episodes = draft["episodes"].as_array().unwrap();
    let rows: Vec<(i64, &str, &str, i64, usize)> = episodes
        .iter()
        .map(|e| {
            (
                e["number"].as_i64().unwrap(),
                e["name"].as_str().unwrap(),
                e["aired"].as_str().unwrap(),
                e["runtime_mins"].as_i64().unwrap(),
                e["video_ids"].as_array().unwrap().len(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (1, "Episode 1", "2026-10-05", 38, 1),
            (2, "Episode 2", "2026-10-12", 39, 2),
            (3, "The Wedding", "2026-10-19", 40, 1),
        ]
    );
    assert_eq!(
        episodes[0]["video_ids"][0], "ep1clean000",
        "the clean upload wins"
    );

    let skipped: Vec<(&str, &str)> = draft["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["video_id"].as_str().unwrap(),
                s["reason"].as_str().unwrap(),
            )
        })
        .collect();
    let reason = |id: &str| skipped.iter().find(|s| s.0 == id).map(|s| s.1);
    assert!(reason("ep4promo000").unwrap().starts_with("an extra"));
    assert!(reason("teaser10000").unwrap().starts_with("an extra"));
    assert!(
        reason("ep3clip0000")
            .unwrap()
            .starts_with("shorter than 25")
    );
    assert!(
        reason("lastep00000")
            .unwrap()
            .starts_with("no episode number")
    );
    assert_eq!(reason("terebin0005"), None, "other dramas are not listed");

    let text = stored["text"].as_str().unwrap();
    let order = [
        "Probably missing on TVDB",
        "TVDB: add series",
        "Series name: Kabhi Main Kabhi Tum",
        "Original name (Urdu): کبھی میں کبھی تم",
        "Country of origin: Pakistan",
        "Original language: Urdu",
        "Network: ARY Digital",
        "First aired: 2026-10-05",
        "Status: Ended",
        "TVDB: add episodes",
        "1\t1\tEpisode 1\t2026-10-05\t38",
        "1\t2\tEpisode 2\t2026-10-12\t39",
        "1\t3\tThe Wedding\t2026-10-19\t40",
        "Left out:",
    ];
    let mut at = 0;
    for line in order {
        let found = text[at..]
            .find(line)
            .unwrap_or_else(|| panic!("{line:?} missing or out of order in:\n{text}"));
        at += found + line.len();
    }

    let id = stored["id"].as_i64().unwrap();
    let (status, again) = call(&kit.app, "GET", &format!("/admin/api/drafts/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again, stored);
    let (status, _) = call(&kit.app, "GET", "/admin/api/drafts/999", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_podcast_draft_numbers_by_hash_and_needs_an_offset() {
    const JRE: &str = "UCzQUP1qoWDoEbmsQxvdjxgQ";
    let kit = kit(None, Setup::default()).await;
    // 2026-10-05 03:00 UTC: still the 4th in Texas (-300 in October).
    let at = 1_791_169_200;
    for (id, title, mins) in [
        (
            "jre2569aaaa",
            "Joe Rogan Experience #2569 - Bill Thompson",
            170,
        ),
        (
            "jre2568aaaa",
            "Joe Rogan Experience #2568 - Mike Baker",
            160,
        ),
        (
            "jreclipaaaa",
            "Bill Thompson on Bigfoot | JRE Clip #2569",
            12,
        ),
        ("jreshortaaa", "Joe Rogan Experience #2567 - Short Cut", 15),
    ] {
        seen_on(&kit.db, JRE, id, title, mins, at).await;
    }
    let request = |extra: Value| {
        let mut body =
            json!({"channel_id": JRE, "title": "Joe Rogan Experience", "preset": "podcast"});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };

    let (status, body) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/show",
        Some(request(json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"].as_str().unwrap().contains("utc_offset_mins"),
        "{body}"
    );

    let (status, stored) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/show",
        Some(request(json!({"utc_offset_mins": -300}))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stored}");
    let draft = &stored["draft"];
    let rows: Vec<(i64, &str, &str)> = draft["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["number"].as_i64().unwrap(),
                e["name"].as_str().unwrap(),
                e["aired"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (2568, "#2568 - Mike Baker", "2026-10-04"),
            (2569, "#2569 - Bill Thompson", "2026-10-04"),
        ]
    );
    let skipped: Vec<(&str, &str)> = draft["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["video_id"].as_str().unwrap(),
                s["reason"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(
        skipped.contains(&("jreshortaaa", "shorter than 20 minutes, likely a highlight")),
        "{skipped:?}"
    );
}

#[tokio::test]
async fn show_draft_requests_are_checked() {
    let kit = kit(None, Setup::default()).await;
    let (status, body) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/show",
        Some(json!({"channel_id": "ARY", "title": "Kabhi Main Kabhi Tum"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"].as_str().unwrap().contains("not a channel id"));

    let (status, body) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/show",
        Some(json!({"channel_id": ARY, "title": "Kabhi Main Kabhi Tum"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"].as_str().unwrap().contains("no videos stored"));

    seed_drama(&kit.db).await;
    let (status, body) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/show",
        Some(json!({"channel_id": ARY, "title": "Kabhi Main Kabhi Tum", "playlist_id": "PLx"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("not a playlist id")
    );

    let (status, body) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/show",
        Some(json!({"channel_id": ARY, "title": "X", "regex": "Episode (\\d+)"})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"].as_str().unwrap().contains("named group"));

    let req = Request::builder()
        .method("POST")
        .uri("/admin/api/drafts/check")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({"kind": "show", "title": "x"}).to_string(),
        ))
        .unwrap();
    let res = kit.app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_playlist_draft_asks_the_watcher_then_numbers_by_place() {
    const PLAYLIST: &str = "PLkmktOfficial01";
    let kit = kit(None, Setup::default()).await;
    seed_drama(&kit.db).await;
    let request =
        json!({"channel_id": ARY, "title": "Kabhi Main Kabhi Tum", "playlist_id": PLAYLIST});

    let (status, body) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/show",
        Some(request.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("not been listed yet"),
        "{body}"
    );
    // The request is left for the watcher, which lists the playlist once.
    assert_eq!(kit.db.requested_playlists().await.unwrap(), [PLAYLIST]);
    assert_eq!(kit.db.watched_playlists().await.unwrap(), [PLAYLIST]);

    // What the watcher stores after listing it: a promo the excludes catch, then the episodes.
    let items: Vec<PlaylistItem> = ["ep4promo000", "ep1clean000", "ep3wedding0", "lastep00000"]
        .iter()
        .map(|id| PlaylistItem {
            video_id: (*id).to_owned(),
            title: String::new(),
            duration_secs: None,
        })
        .collect();
    kit.db.replace_playlist(PLAYLIST, &items).await.unwrap();
    assert!(kit.db.requested_playlists().await.unwrap().is_empty());

    let (status, stored) = call(&kit.app, "POST", "/admin/api/drafts/show", Some(request)).await;
    assert_eq!(status, StatusCode::OK, "{stored}");
    let draft = &stored["draft"];
    assert_eq!(draft["numbering"], "playlist");
    let episodes: Vec<(i64, String)> = draft["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["number"].as_i64().unwrap(),
                e["video_ids"][0].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        episodes,
        [
            (1, "ep1clean000".to_owned()),
            (2, "ep3wedding0".to_owned()),
            (3, "lastep00000".to_owned())
        ]
    );
}

async fn seed_film(db: &Db) {
    sqlx::query(
        "INSERT INTO videos (video_id, title, duration_secs, upload_date, channel_id, updated_at) \
         VALUES (?, ?, ?, ?, ?, 0)",
    )
    .bind("fIlm0000001")
    .bind("Ishq Hua | کیا عشق ہوا | ARY Digital")
    .bind(5_390_i64)
    .bind("20250214")
    .bind(ARY)
    .execute(db.pool())
    .await
    .unwrap();
}

#[tokio::test]
async fn film_draft_from_a_stored_video() {
    let fake = Fake::default();
    let url = fake.serve().await;
    let kit = kit(
        Some(&url),
        Setup {
            arrs: true,
            ..Setup::default()
        },
    )
    .await;
    seed_film(&kit.db).await;
    let (status, stored) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/film",
        Some(json!({"video_id": "fIlm0000001"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stored}");
    let draft = &stored["draft"];
    assert_eq!(draft["title"], "Ishq Hua");
    assert_eq!(draft["original_title"], "کیا عشق ہوا");
    assert_eq!(draft["year"], 2025);
    assert_eq!(draft["year_from_upload"], true);
    assert_eq!(draft["runtime_mins"], 90);
    assert_eq!(draft["language"], "Urdu");
    assert_eq!(draft["country"], "Pakistan");
    assert_eq!(draft["channel_id"], ARY);
    assert_eq!(draft["tv_film"], true, "a TV network's film is a telefilm");
    assert_eq!(
        draft["youtube_url"],
        "https://www.youtube.com/watch?v=fIlm0000001"
    );
    // TMDB has a 2019 "Ishq Hua": close in name, but the year is far off, so look first.
    assert_eq!(draft["check"]["verdict"], "possible");

    let text = stored["text"].as_str().unwrap();
    let order = [
        "Maybe on TMDB, check first",
        "TMDB: add movie",
        "Original language: Urdu",
        "Original title: کیا عشق ہوا",
        "English title: Ishq Hua",
        "Year: 2025 (upload year, check it)",
        "Runtime: 90 min",
        "Country: Pakistan",
        "Genre: TV Movie",
        "Watch: https://www.youtube.com/watch?v=fIlm0000001",
    ];
    let mut at = 0;
    for line in order {
        let found = text[at..]
            .find(line)
            .unwrap_or_else(|| panic!("{line:?} missing or out of order in:\n{text}"));
        at += found + line.len();
    }

    let (status, _) = call(
        &kit.app,
        "POST",
        "/admin/api/drafts/film",
        Some(json!({"video_id": "nope0000000"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
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

#[tokio::test]
async fn keys_are_never_logged_or_returned() {
    let _warns = WARNS.lock().await;
    let logs = LogBuffer::default();
    let writer = logs.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);
    // Other tests in this binary run without a subscriber; without this their cached "nobody
    // listens" verdict can hide the warnings from this thread.
    tracing::callsite::rebuild_interest_cache();

    let fake = Fake::default();
    let url = fake.serve().await;
    let kit = kit(
        Some(&url),
        Setup {
            arrs: true,
            keys: true,
            ..Setup::default()
        },
    )
    .await;
    let mut bodies = String::new();
    for limited in [false, true] {
        fake.set_rate_limited(limited);
        for (kind, title) in [("show", "Tere Bin"), ("film", "Ishq Hua")] {
            let (_, body) = call(
                &kit.app,
                "POST",
                "/admin/api/drafts/check",
                Some(json!({"kind": kind, "title": title})),
            )
            .await;
            bodies.push_str(&body.to_string());
        }
    }
    let text = logs.text();
    assert!(text.contains("draft check failed"), "{text}");
    for secret in [
        ARR_KEY,
        TVDB_KEY,
        TVDB_PIN,
        TMDB_TOKEN,
        "tvdb-session-token",
    ] {
        assert!(!text.contains(secret), "a secret leaked into the logs");
        assert!(!bodies.contains(secret), "a secret leaked into an answer");
    }
}

impl LogBuffer {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}
