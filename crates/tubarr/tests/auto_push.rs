//! Auto push: the matcher's first releases (and a catch-up) become paced `release/push` calls to a
//! fake Sonarr and Radarr, which record each body and answer like the real ones.

mod support;

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{Path, Query, State},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use tubarr::{
    AppState,
    activity::Activity,
    arr::{App, AutoPush, Outcome, PushTarget},
    binaries::BinaryManager,
    config::Config,
    matcher::{Known, Metadata, Wanted, WantedEpisode, WantedSeries, match_shows},
    store::{
        Db, MappingKind, NewMapping, NewRelease, PinTarget, SeenVideo, StoreError, VideoSource,
    },
};

const KEY: &str = "0123456789abcdef0123456789abcdef";
const SECOND: Duration = Duration::from_secs(1);
const SERIES: i64 = 11;
const DAILY: i64 = 12;
const TVDB: i64 = 460_100;

#[derive(Default)]
struct FakeState {
    pushes: Vec<Value>,
    client_reads: usize,
    /// Answer every push with this status instead of a decision.
    fail: Option<StatusCode>,
}

/// Series 11: episodes 1 to 30 wanted, 31 has a file, 32 is not monitored. Series 12 is daily.
/// Film 7 is wanted, film 8 has a file. A push of episode 2 is rejected.
#[derive(Clone, Default)]
struct Fake(Arc<Mutex<FakeState>>);

impl Fake {
    fn pushes(&self) -> Vec<Value> {
        self.0.lock().unwrap().pushes.clone()
    }

    fn fail(&self, status: Option<StatusCode>) {
        self.0.lock().unwrap().fail = status;
    }

    async fn serve(&self) -> String {
        let api = Router::new()
            .route("/api/v3/episode", get(episodes))
            .route("/api/v3/movie/{id}", get(movie))
            .route("/api/v3/downloadclient", get(download_clients))
            .route("/api/v3/release/push", post(push))
            .with_state(self.clone());
        let app = Router::new().nest("/base", api);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/base")
    }
}

fn episode(series: i64, n: i64, air_date: Option<&str>, monitored: bool, has_file: bool) -> Value {
    json!({
        "id": series * 100 + n, "seriesId": series, "seasonNumber": 1, "episodeNumber": n,
        "airDate": air_date, "monitored": monitored, "hasFile": has_file
    })
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct EpisodeQuery {
    series_id: i64,
}

async fn episodes(Query(q): Query<EpisodeQuery>) -> Json<Value> {
    let list: Vec<Value> = match q.series_id {
        SERIES => (1..=32)
            .map(|n| episode(SERIES, n, None, n != 32, n == 31))
            .collect(),
        DAILY => vec![
            episode(DAILY, 1, Some("2026-10-01"), true, false),
            episode(DAILY, 2, Some("2026-10-02"), true, true),
        ],
        _ => Vec::new(),
    };
    Json(Value::Array(list))
}

async fn movie(Path(id): Path<i64>) -> Response {
    match id {
        7 | 8 => Json(json!({
            "id": id, "title": "Film", "tmdbId": id * 1000, "monitored": true, "hasFile": id == 8
        }))
        .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A disabled "Tubarr" first, the real SABnzbd, then Tubarr's client in lower case.
async fn download_clients(State(fake): State<Fake>) -> Json<Value> {
    fake.0.lock().unwrap().client_reads += 1;
    Json(json!([
        {"id": 9, "name": "Tubarr", "protocol": "usenet", "enable": false, "implementation": "Sabnzbd"},
        {"id": 1, "name": "qBittorrent", "protocol": "torrent", "enable": true, "implementation": "QBittorrent"},
        {"id": 2, "name": "SABnzbd", "protocol": "usenet", "enable": true, "implementation": "Sabnzbd"},
        {"id": 4, "name": "tubarr", "protocol": "usenet", "enable": true, "implementation": "Sabnzbd"}
    ]))
}

async fn push(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
    let mut state = fake.0.lock().unwrap();
    state.pushes.push(body.clone());
    match state.fail {
        Some(StatusCode::BAD_REQUEST) => (
            StatusCode::BAD_REQUEST,
            Json(json!([{"propertyName": "Title", "errorMessage": "Unable to parse"}])),
        )
            .into_response(),
        Some(status) => status.into_response(),
        None => {
            let title = body["title"].as_str().unwrap_or_default();
            let decision = if title.contains("S01E02") {
                json!({"title": title, "approved": false, "temporarilyRejected": false,
                       "rejected": true, "rejections": ["Not an upgrade for existing episode file(s)"]})
            } else {
                json!({"title": title, "approved": true, "temporarilyRejected": false,
                       "rejected": false, "rejections": []})
            };
            Json(json!([decision])).into_response()
        }
    }
}

struct Kit {
    state: AppState,
    push: Arc<AutoPush>,
    fake: Fake,
    show: i64,
    daily: i64,
    film: i64,
    _dir: tempfile::TempDir,
}

fn config(url: &str, extra: &str, with_key: bool) -> (Config, tempfile::TempDir) {
    let dir = support::temp_dir();
    let key_file = dir.path().join("key");
    std::fs::write(&key_file, KEY).unwrap();
    let api_key = if with_key {
        format!("api_key_file = \"{}\"\n", key_file.display())
    } else {
        String::new()
    };
    let toml = format!(
        "public_url = \"http://tubarr:8480\"\ndata_dir = {data:?}\n{api_key}{extra}\n\
         [sonarr]\nurl = \"{url}\"\napi_key_file = \"{key}\"\n\
         [radarr]\nurl = \"{url}\"\napi_key_file = \"{key}\"\n",
        data = dir.path(),
        key = key_file.display()
    );
    (Config::from_toml(&toml).unwrap(), dir)
}

async fn state(config: Config) -> AppState {
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    AppState::new(config, db, binaries).unwrap()
}

async fn mapping(db: &Db, kind: MappingKind, arr_id: Option<i64>, title: &str) -> i64 {
    let film = kind == MappingKind::Film;
    let m = db
        .insert_mapping(&NewMapping {
            kind,
            tvdb_id: (!film).then_some(TVDB + arr_id.unwrap_or(99)),
            tmdb_id: film.then_some(7000),
            title: title.to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec![CHANNEL.to_owned()],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        })
        .await
        .unwrap();
    sqlx::query("UPDATE mappings SET arr_id = ? WHERE id = ?")
        .bind(arr_id)
        .bind(m.id)
        .execute(db.pool())
        .await
        .unwrap();
    m.id
}

async fn kit(extra: &str) -> Kit {
    let fake = Fake::default();
    let (config, dir) = config(&fake.serve().await, extra, true);
    let state = state(config).await;
    let push = state.push.clone().expect("auto push is on");
    let show = mapping(&state.db, MappingKind::Show, Some(SERIES), "Dar E Nijaat").await;
    let daily = mapping(
        &state.db,
        MappingKind::Daily,
        Some(DAILY),
        "Good Morning Pakistan",
    )
    .await;
    let film = mapping(&state.db, MappingKind::Film, Some(7), "Parey Hut Love").await;
    Kit {
        state,
        push,
        fake,
        show,
        daily,
        film,
        _dir: dir,
    }
}

impl Kit {
    async fn release(&self, mapping_id: i64, target: PinTarget, video_id: &str) {
        self.state
            .db
            .insert_release(&NewRelease {
                mapping_id,
                video_id: video_id.to_owned(),
                part_ids: vec![],
                target,
                year: None,
                imdb_id: None,
                video_title: format!("{video_id} | ARY Digital"),
                language: "Urdu".to_owned(),
                height: 1080,
                size: Some(1_234_567_890),
                published_at: 1_700_000_000,
            })
            .await
            .unwrap();
    }

    async fn episode(&self, n: i64) {
        let id = format!("DenEp{n:02}aaaa");
        self.release(
            self.show,
            PinTarget::Episode {
                season: 1,
                episode: n,
            },
            &id,
        )
        .await;
    }

    /// The push events, oldest first (the feed reads in a test log searches too).
    async fn events(&self) -> Vec<Activity> {
        let (events, _) = self.state.activity.page(None, 100, 0).await.unwrap();
        events
            .into_iter()
            .rev()
            .map(|e| e.activity)
            .filter(|a| matches!(a, Activity::ArrPush { .. }))
            .collect()
    }

    async fn call(&self, method: &str, uri: &str, key: Option<&str>) -> (StatusCode, Vec<u8>) {
        let mut request = Request::builder().method(method).uri(uri);
        if let Some(key) = key {
            request = request.header("X-Api-Key", key);
        }
        let response = tubarr::app(self.state.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, body.to_vec())
    }

    /// The first item's title and NZB link in Tubarr's own feed for `query`.
    async fn feed_item(&self, query: &str) -> (String, String) {
        let (status, body) = self
            .call("GET", &format!("/api?{query}&apikey={KEY}"), None)
            .await;
        assert_eq!(status, StatusCode::OK);
        let xml = String::from_utf8(body).unwrap();
        let item = &xml[xml.find("<item>").expect("an item")..];
        let between = |from: &str, to: &str| {
            let start = item.find(from).unwrap() + from.len();
            let end = start + item[start..].find(to).unwrap();
            item[start..end].replace("&amp;", "&")
        };
        (
            between("<title>", "</title>"),
            between("<enclosure url=\"", "\""),
        )
    }
}

fn ep(series_id: i64, episode: i64) -> PushTarget {
    PushTarget::Episode {
        series_id,
        season: 1,
        episode,
    }
}

#[tokio::test]
async fn a_push_carries_the_feed_release_pinned_to_tubarrs_client() {
    let kit = kit("").await;
    kit.episode(1).await;
    kit.release(kit.film, PinTarget::Film, "PareyHutLov").await;
    kit.release(
        kit.daily,
        PinTarget::AirDate("2026-10-01".to_owned()),
        "GmpOct01aaa",
    )
    .await;
    kit.push.enqueue([
        ep(SERIES, 1),
        PushTarget::Film { movie_id: 7 },
        PushTarget::AirDate {
            series_id: DAILY,
            date: "2026-10-01".to_owned(),
        },
    ]);

    let t0 = Instant::now();
    let sent = kit.push.flush_at(t0).await;
    // One push per arr per interval: the daily episode waits for Sonarr's next turn.
    assert_eq!(sent.len(), 2);
    assert_eq!(
        (sent[0].app, &sent[0].outcome),
        (App::Sonarr, &Outcome::Grabbed)
    );
    assert_eq!(
        (sent[1].app, &sent[1].outcome),
        (App::Radarr, &Outcome::Grabbed)
    );
    let sent = kit.push.flush_at(t0 + 5 * SECOND).await;
    assert_eq!(sent.len(), 1);

    let pushes = kit.fake.pushes();
    assert_eq!(pushes.len(), 3);
    for (push, query) in pushes.iter().zip([
        format!("t=tvsearch&tvdbid={}&season=1&ep=1", TVDB + SERIES),
        "t=movie&tmdbid=7000".to_owned(),
        format!("t=tvsearch&tvdbid={}&season=2026&ep=10/01", TVDB + DAILY),
    ]) {
        let (title, url) = kit.feed_item(&query).await;
        assert_eq!(push["title"], title.as_str(), "{query}");
        assert_eq!(push["downloadUrl"], url.as_str(), "{query}");
        assert_eq!(push["protocol"], "usenet");
        assert_eq!(push["publishDate"], "2023-11-14T22:13:20Z");
        assert_eq!(push["indexer"], "Tubarr");
        // Pinned to Tubarr's enabled client, never the real SABnzbd (2) or the disabled one (9).
        assert_eq!(push["downloadClientId"], 4);
        // `[newznab] report_size` is off by default: no size, like the feed.
        assert!(push.get("size").is_none(), "{push}");
    }
    assert!(
        pushes[0]["downloadUrl"]
            .as_str()
            .unwrap()
            .starts_with("http://tubarr:8480/api?t=get&id=DenEp01aaaa&apikey=")
    );
    // The client id was read once per arr and kept.
    assert_eq!(kit.fake.0.lock().unwrap().client_reads, 2);

    let events = kit.events().await;
    assert_eq!(events.len(), 3);
    assert!(matches!(
        &events[0],
        Activity::ArrPush { app, outcome, reasons, .. }
            if app == "Sonarr" && outcome == "grabbed" && reasons.is_empty()
    ));
}

#[tokio::test]
async fn a_reported_size_goes_with_the_push() {
    let kit = kit("[newznab]\nreport_size = true").await;
    kit.episode(1).await;
    kit.push.enqueue([ep(SERIES, 1)]);
    kit.push.flush_at(Instant::now()).await;
    assert_eq!(kit.fake.pushes()[0]["size"], 1_234_567_890_i64);
}

#[tokio::test]
async fn pushes_are_paced_and_only_for_what_the_arr_wants() {
    let kit = kit("").await;
    for n in [1, 2, 3, 31] {
        kit.episode(n).await;
    }
    // 31 has a file, 5 has no release in Tubarr: both are skipped on the way.
    kit.push.enqueue([1, 31, 2, 5, 3].map(|n| ep(SERIES, n)));
    assert_eq!(kit.push.enqueue([ep(SERIES, 1)]), 0);
    assert_eq!(kit.push.queued().len(), 5);

    let t0 = Instant::now();
    let titles = |sent: Vec<tubarr::arr::Pushed>| -> Vec<String> {
        sent.into_iter().map(|p| p.title).collect()
    };
    let first = titles(kit.push.flush_at(t0).await);
    assert_eq!(first.len(), 1);
    assert!(first[0].contains("S01E01"), "{first:?}");
    assert!(kit.push.flush_at(t0 + 4 * SECOND).await.is_empty());

    let second = kit.push.flush_at(t0 + 5 * SECOND).await;
    assert!(second[0].title.contains("S01E02"));
    assert_eq!(
        second[0].outcome,
        Outcome::Rejected(vec![
            "Not an upgrade for existing episode file(s)".to_owned()
        ])
    );
    let third = titles(kit.push.flush_at(t0 + 10 * SECOND).await);
    assert!(third[0].contains("S01E03"), "{third:?}");
    assert!(kit.push.queued().is_empty());
    assert_eq!(kit.fake.pushes().len(), 3);

    let events = kit.events().await;
    assert!(matches!(
        &events[1],
        Activity::ArrPush { outcome, reasons, .. }
            if outcome == "rejected" && reasons == &["Not an upgrade for existing episode file(s)"]
    ));
}

#[tokio::test]
async fn an_unavailable_arr_keeps_the_target_and_other_errors_drop_it() {
    let kit = kit("").await;
    kit.episode(1).await;
    kit.episode(3).await;
    kit.push.enqueue([ep(SERIES, 1)]);

    kit.fake.fail(Some(StatusCode::SERVICE_UNAVAILABLE));
    let t0 = Instant::now();
    let sent = kit.push.flush_at(t0).await;
    assert!(matches!(&sent[0].outcome, Outcome::Failed(e) if e.contains("503")));
    assert_eq!(kit.push.queued(), [ep(SERIES, 1)]);

    kit.fake.fail(Some(StatusCode::TOO_MANY_REQUESTS));
    kit.push.flush_at(t0 + 5 * SECOND).await;
    assert_eq!(kit.push.queued(), [ep(SERIES, 1)]);

    kit.fake.fail(None);
    let sent = kit.push.flush_at(t0 + 10 * SECOND).await;
    assert_eq!(sent[0].outcome, Outcome::Grabbed);
    assert!(kit.push.queued().is_empty());

    // A 400 (the arr cannot parse the title) is not tried again.
    kit.fake.fail(Some(StatusCode::BAD_REQUEST));
    kit.push.enqueue([ep(SERIES, 3)]);
    let sent = kit.push.flush_at(t0 + 15 * SECOND).await;
    assert!(
        matches!(&sent[0].outcome, Outcome::Failed(e) if e.contains("Unable to parse")),
        "{sent:?}"
    );
    assert!(kit.push.queued().is_empty());
    let outcomes: Vec<String> = kit
        .events()
        .await
        .into_iter()
        .map(|e| match e {
            Activity::ArrPush { outcome, .. } => outcome,
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(outcomes, ["failed", "failed", "grabbed", "failed"]);
}

#[tokio::test]
async fn without_tubarrs_download_client_nothing_is_pushed() {
    let kit = kit("[arr]\ndownload_client = \"Tubarr Somewhere\"").await;
    kit.episode(1).await;
    kit.push.enqueue([ep(SERIES, 1)]);
    let sent = kit.push.flush_at(Instant::now()).await;
    assert!(
        matches!(&sent[0].outcome, Outcome::Failed(e) if e.contains("Tubarr Somewhere")),
        "{sent:?}"
    );
    assert!(kit.fake.pushes().is_empty());
    assert!(kit.push.queued().is_empty());
}

#[tokio::test]
async fn auto_push_follows_the_config_switch() {
    let fake = Fake::default();
    let url = fake.serve().await;
    for (extra, with_key, on) in [
        ("", true, true),
        ("[arr]\nauto_push = false", true, false),
        // The old name still works.
        ("[arr]\nauto_search = false", true, false),
        // The feed's links need the key.
        ("", false, false),
    ] {
        let (config, _dir) = config(&url, extra, with_key);
        assert_eq!(state(config).await.push.is_some(), on, "{extra} {with_key}");
    }
}

#[tokio::test]
async fn catch_up_queues_every_released_target_and_needs_auth() {
    let kit = kit("").await;
    kit.episode(1).await;
    kit.episode(31).await;
    kit.release(kit.film, PinTarget::Film, "PareyHutLov").await;
    // A mapping made by hand has no arr id: nothing to push.
    let manual = mapping(&kit.state.db, MappingKind::Show, None, "By Hand").await;
    kit.release(
        manual,
        PinTarget::Episode {
            season: 1,
            episode: 1,
        },
        "ByHandEp01a",
    )
    .await;

    let (status, _) = kit.call("POST", "/admin/api/arr/push-released", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(kit.push.queued().is_empty());

    let (status, body) = kit
        .call("POST", "/admin/api/arr/push-released", Some(KEY))
        .await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body, json!({"queued": 3, "waiting": 3}));
    assert_eq!(
        kit.push.queued(),
        [
            ep(SERIES, 1),
            ep(SERIES, 31),
            PushTarget::Film { movie_id: 7 }
        ]
    );

    // Episode 31 has a file and is skipped. Film 7 is pushed.
    let sent = kit.push.flush_at(Instant::now()).await;
    assert_eq!(sent.len(), 2);
    let sent = kit.push.flush_at(Instant::now() + 5 * SECOND).await;
    assert!(sent.is_empty());
    assert!(kit.push.queued().is_empty());
    assert_eq!(kit.fake.pushes().len(), 2);

    // Queued again, nothing new is added twice.
    kit.push.enqueue([ep(SERIES, 2)]);
    let (_, body) = kit
        .call("POST", "/admin/api/arr/push-released", Some(KEY))
        .await;
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["waiting"], 4);
}

#[tokio::test]
async fn catch_up_is_refused_with_auto_push_off() {
    let fake = Fake::default();
    let (config, _dir) = config(&fake.serve().await, "[arr]\nauto_push = false", true);
    let state = state(config).await;
    let response = tubarr::app(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/admin/api/arr/push-released")
                .header("X-Api-Key", KEY)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

struct FakeWanted(Vec<WantedSeries>);

impl Wanted for FakeWanted {
    async fn series(&self) -> Result<Vec<WantedSeries>, StoreError> {
        Ok(self.0.clone())
    }
}

/// Every video is known at 1080p.
struct Tall;

impl Metadata for Tall {
    async fn known(&self, _video_id: &str) -> Result<Known, StoreError> {
        Ok(Known {
            height: Some(1080),
            ..Known::default()
        })
    }
}

const CHANNEL: &str = "UC4JCksJF76g_MdzPVBJoC3Q";

async fn upload(db: &Db, id: &str, episode: i64, at: i64) {
    db.upsert_seen_video(&SeenVideo {
        video_id: id.to_owned(),
        channel_id: CHANNEL.to_owned(),
        title: format!("Dar E Nijaat Episode {episode} | ARY Digital"),
        published_at: Some(at),
        published_approx: false,
        duration_secs: Some(38 * 60),
        source: VideoSource::Listing,
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn only_a_first_release_is_queued_not_a_rematch() {
    let dir = support::temp_dir();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    let mapping = db
        .insert_mapping(&NewMapping {
            kind: MappingKind::Show,
            tvdb_id: Some(460_100),
            tmdb_id: None,
            title: "Dar E Nijaat".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec![CHANNEL.to_owned()],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
        })
        .await
        .unwrap();
    sqlx::query("UPDATE mappings SET arr_id = ? WHERE id = ?")
        .bind(SERIES)
        .bind(mapping.id)
        .execute(db.pool())
        .await
        .unwrap();
    let mapping = db.mapping(mapping.id).await.unwrap().unwrap();
    let wanted = FakeWanted(vec![WantedSeries {
        mapping,
        episodes: (1..=2)
            .map(|episode| WantedEpisode {
                season: 1,
                episode,
                air_date: None,
                title: None,
                monitored: true,
            })
            .collect(),
    }]);

    upload(&db, "DenEp01late", 1, 1_700_100_000).await;
    upload(&db, "DenEp02aaaa", 2, 1_700_200_000).await;
    let report = match_shows(&db, &db, &wanted, &Tall).await.unwrap();
    assert_eq!(report.released, 2, "{report:?}");
    assert_eq!(report.first_released, [ep(SERIES, 1), ep(SERIES, 2)]);

    // An earlier upload of episode 1 takes its place: a re-match, not a first release.
    upload(&db, "DenEp01orig", 1, 1_700_000_000).await;
    let report = match_shows(&db, &db, &wanted, &Tall).await.unwrap();
    assert_eq!((report.released, report.replaced), (1, 1), "{report:?}");
    assert!(report.first_released.is_empty(), "{report:?}");

    let report = match_shows(&db, &db, &wanted, &Tall).await.unwrap();
    assert_eq!(report.released, 0);
    assert!(report.first_released.is_empty());
}
