//! The Newznab API on `/api`, end to end through the router, with every answer parsed as XML.

mod support;

use std::{io::Write, sync::Arc};

use axum::{body::Body, http::Request, http::StatusCode};
use http_body_util::BodyExt;
use quick_xml::events::Event;
use tower::ServiceExt;
use tubarr::{
    AppState,
    binaries::BinaryManager,
    config::Config,
    store::{Db, MappingKind, NewMapping, NewRelease, PinTarget},
};

const KEY: &str = "test-key";
const NEWZNAB_NS: &str = "http://www.newznab.com/DTD/2010/feeds/attributes/";

struct Fixture {
    app: axum::Router,
    db: Db,
    _dir: tempfile::TempDir,
}

/// The router over a fresh database at `public_url`, with `key` as its API key when given.
async fn build(public_url: &str, key: Option<&str>) -> Fixture {
    build_with(public_url, key, "").await
}

/// [`build`] with more config sections after the top-level keys.
async fn build_with(public_url: &str, key: Option<&str>, sections: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let mut toml = format!(
        "public_url = \"{public_url}\"\ndata_dir = {:?}\n",
        dir.path()
    );
    if let Some(key) = key {
        let key_path = dir.path().join("api_key");
        let mut file = std::fs::File::create(&key_path).unwrap();
        writeln!(file, "{key}").unwrap();
        toml.push_str(&format!("api_key_file = {key_path:?}\n"));
    }
    toml.push_str(sections);
    let config = Config::from_toml(&toml).unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    Fixture {
        app: tubarr::app(AppState::new(config, db.clone(), binaries).expect("app state")),
        db,
        _dir: dir,
    }
}

async fn fixture_with_key(with_key: bool) -> Fixture {
    build("http://localhost:8480", with_key.then_some(KEY)).await
}

async fn empty() -> Fixture {
    fixture_with_key(true).await
}

fn mapping(
    kind: MappingKind,
    tvdb_id: Option<i64>,
    tmdb_id: Option<i64>,
    title: &str,
) -> NewMapping {
    NewMapping {
        kind,
        tvdb_id,
        tmdb_id,
        title: title.to_owned(),
        language: if kind == MappingKind::Film {
            "Hindi"
        } else {
            "Urdu"
        }
        .to_owned(),
        channel_ids: vec![],
        episode_regex: None,
        excludes: vec![],
        min_duration_secs: None,
    }
}

fn release(mapping_id: i64, video_id: &str, target: PinTarget, language: &str) -> NewRelease {
    NewRelease {
        mapping_id,
        video_id: video_id.to_owned(),
        part_ids: Vec::new(),
        target,
        year: None,
        imdb_id: None,
        video_title: "Kabhi Main Kabhi Tum Episode 1 | ARY Digital".to_owned(),
        language: language.to_owned(),
        height: 1080,
        size: Some(1_234_567_890),
        published_at: 1_792_000_000,
    }
}

/// A drama, a daily show and a film, one release each.
async fn seeded() -> Fixture {
    seed(empty().await).await
}

/// [`seeded`] data in `f`.
async fn seed(f: Fixture) -> Fixture {
    let show =
        f.db.insert_mapping(&mapping(
            MappingKind::Show,
            Some(452_001),
            None,
            "Kabhi Main Kabhi Tum",
        ))
        .await
        .unwrap()
        .id;
    let daily =
        f.db.insert_mapping(&mapping(
            MappingKind::Daily,
            Some(9),
            None,
            "Khabarnaak: Live",
        ))
        .await
        .unwrap()
        .id;
    let film =
        f.db.insert_mapping(&mapping(MappingKind::Film, None, Some(352_173), "Theri"))
            .await
            .unwrap()
            .id;
    let ep = PinTarget::Episode {
        season: 1,
        episode: 1,
    };
    f.db.insert_release(&release(show, "0eD-GQCutAU", ep, "Urdu"))
        .await
        .unwrap();
    f.db.insert_release(&NewRelease {
        size: None,
        published_at: 1_792_000_100,
        ..release(
            daily,
            "dailyvid001",
            PinTarget::AirDate("2026-10-14".to_owned()),
            "Urdu",
        )
    })
    .await
    .unwrap();
    f.db.insert_release(&NewRelease {
        year: Some(2016),
        imdb_id: Some(4_598_756),
        published_at: 1_700_000_000,
        ..release(film, "ldmPWRb-XcM", PinTarget::Film, "Hindi")
    })
    .await
    .unwrap();
    f
}

struct Answer {
    status: StatusCode,
    content_type: String,
    headers: axum::http::HeaderMap,
    body: String,
}

impl Answer {
    fn xml(&self) -> El {
        parse(&self.body)
    }
}

async fn get(f: &Fixture, query: &str) -> Answer {
    let response = f
        .app
        .clone()
        .oneshot(
            Request::get(format!("/api?{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let content_type = headers
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    Answer {
        status,
        content_type,
        headers,
        body: String::from_utf8(body.to_vec()).unwrap(),
    }
}

/// A Newznab call with the right key.
async fn api(f: &Fixture, query: &str) -> Answer {
    get(f, &format!("apikey={KEY}&{query}")).await
}

// A small element tree, enough to check structure.
#[derive(Debug, Default)]
struct El {
    name: String,
    attrs: Vec<(String, String)>,
    text: String,
    children: Vec<El>,
}

impl El {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn all(&self, name: &str) -> Vec<&El> {
        self.children.iter().filter(|c| c.name == name).collect()
    }

    fn child(&self, name: &str) -> &El {
        let found = self.all(name);
        assert_eq!(found.len(), 1, "expected one <{name}> in <{}>", self.name);
        found[0]
    }

    fn text_of(&self, name: &str) -> &str {
        &self.child(name).text
    }

    /// Newznab attributes as (name, value), in order.
    fn nz_attrs(&self) -> Vec<(&str, &str)> {
        self.all("newznab:attr")
            .into_iter()
            .map(|a| (a.attr("name").unwrap(), a.attr("value").unwrap()))
            .collect()
    }
}

fn parse(xml: &str) -> El {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut stack = vec![El::default()];
    let start = |e: &quick_xml::events::BytesStart<'_>| El {
        name: e.name().as_ref().to_owned(),
        attrs: e
            .attributes()
            .map(|a| {
                let a = a.unwrap();
                (
                    a.key.as_ref().to_owned(),
                    a.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .unwrap()
                        .into_owned(),
                )
            })
            .collect(),
        ..El::default()
    };
    loop {
        match reader.read_event().unwrap() {
            Event::Start(e) => stack.push(start(&e)),
            Event::Empty(e) => stack.last_mut().unwrap().children.push(start(&e)),
            Event::End(_) => {
                let done = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(done);
            }
            Event::Text(t) => stack.last_mut().unwrap().text.push_str(&t.xml10_content()),
            Event::GeneralRef(r) => {
                let text = match r.resolve_char_ref().unwrap() {
                    Some(c) => c.to_string(),
                    None => match &*r.xml10_content() {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        other => panic!("unknown entity {other}"),
                    }
                    .to_owned(),
                };
                stack.last_mut().unwrap().text.push_str(&text);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    assert_eq!(stack.len(), 1, "unclosed elements");
    let mut doc = stack.pop().unwrap();
    assert_eq!(doc.children.len(), 1, "one root element");
    doc.children.pop().unwrap()
}

/// The items of a result feed, after checking the feed's own shape.
fn items(answer: &Answer) -> Vec<El> {
    assert_eq!(answer.status, StatusCode::OK, "{}", answer.body);
    assert!(answer.content_type.starts_with("application/xml"));
    let mut rss = answer.xml();
    assert_eq!(rss.name, "rss");
    assert_eq!(rss.attr("version"), Some("2.0"));
    assert_eq!(rss.attr("xmlns:newznab"), Some(NEWZNAB_NS));
    let mut channel = rss.children.pop().unwrap();
    assert_eq!(channel.name, "channel");
    let count = channel.all("item").len().to_string();
    assert_eq!(
        channel.child("newznab:response").attr("total"),
        Some(count.as_str())
    );
    channel.children.retain(|c| c.name == "item");
    channel.children
}

fn titles(items: &[El]) -> Vec<&str> {
    items.iter().map(|i| i.text_of("title")).collect()
}

fn error_code(answer: &Answer) -> String {
    let root = answer.xml();
    assert_eq!(root.name, "error", "{}", answer.body);
    assert!(root.attr("description").is_some_and(|d| !d.is_empty()));
    root.attr("code").unwrap().to_owned()
}

#[tokio::test]
async fn caps_advertise_searches_and_categories() {
    let f = empty().await;
    let answer = api(&f, "t=caps").await;
    assert_eq!(answer.status, StatusCode::OK);
    let caps = answer.xml();
    assert_eq!(caps.name, "caps");
    let limits = caps.child("limits");
    assert_eq!(limits.attr("max"), Some("100"));
    assert_eq!(limits.attr("default"), Some("100"));
    let searching = caps.child("searching");
    let params = |name: &str| {
        let s = searching.child(name);
        (
            s.attr("available").unwrap(),
            s.attr("supportedParams").unwrap(),
        )
    };
    assert_eq!(params("search"), ("yes", "q"));
    assert_eq!(params("tv-search"), ("yes", "q,tvdbid,season,ep"));
    assert_eq!(params("movie-search"), ("yes", "q,imdbid,tmdbid"));
    let cats: Vec<(&str, Vec<&str>)> = caps
        .child("categories")
        .all("category")
        .into_iter()
        .map(|c| {
            (
                c.attr("id").unwrap(),
                c.all("subcat")
                    .iter()
                    .map(|s| s.attr("id").unwrap())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(cats, [("2000", vec!["2040"]), ("5000", vec!["5040"])]);
}

#[tokio::test]
async fn wrong_missing_or_unconfigured_key_is_refused() {
    let f = seeded().await;
    for query in ["t=caps", "t=caps&apikey=nope", "t=tvsearch&apikey=TEST-KEY"] {
        let answer = get(&f, query).await;
        assert_eq!(answer.status, StatusCode::UNAUTHORIZED, "{query}");
        assert!(answer.content_type.starts_with("application/xml"));
        assert_eq!(error_code(&answer), "100", "{query}");
    }

    let open = fixture_with_key(false).await;
    let answer = api(&open, "t=caps").await;
    assert_eq!(answer.status, StatusCode::UNAUTHORIZED);
    assert_eq!(error_code(&answer), "100");
}

#[tokio::test]
async fn bad_requests_get_newznab_errors() {
    let f = seeded().await;
    let cases = [
        ("t=nonsense", "202"),
        ("q=neither", "200"),
        ("t=get", "200"),
        ("t=get&id=unknown", "300"),
        ("t=tvsearch&tvdbid=452001&season=one&ep=1", "201"),
        ("t=tvsearch&tvdbid=452001&ep=1", "200"),
        ("t=tvsearch&tvdbid=452001&season=2026&ep=10-14x/1", "201"),
        ("t=tvsearch&tvdbid=abc", "201"),
        ("t=movie&imdbid=ttxyz", "201"),
        ("t=search&limit=-1", "201"),
        ("t=search&cat=TV", "201"),
    ];
    for (query, code) in cases {
        let answer = api(&f, query).await;
        assert_eq!(answer.status, StatusCode::OK, "{query}");
        assert_eq!(error_code(&answer), code, "{query}");
    }
}

#[tokio::test]
async fn tvsearch_by_tvdb_season_and_episode() {
    let f = seeded().await;
    let answer = api(
        &f,
        "t=tvsearch&cat=5030,5040&extended=1&offset=0&limit=100&tvdbid=452001&season=1&ep=1",
    )
    .await;
    let found = items(&answer);
    assert_eq!(found.len(), 1);
    let item = &found[0];
    assert_eq!(
        item.text_of("title"),
        "Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR"
    );
    assert_eq!(item.text_of("guid"), "tubarr-0eD-GQCutAU-S01E01");
    assert_eq!(item.child("guid").attr("isPermaLink"), Some("false"));
    assert_eq!(item.text_of("pubDate"), "Wed, 14 Oct 2026 17:46:40 +0000");
    assert_eq!(item.text_of("category"), "TV > HD");
    let url = "http://localhost:8480/api?t=get&id=0eD-GQCutAU&apikey=test-key";
    assert_eq!(item.text_of("link"), url);
    let enclosure = item.child("enclosure");
    assert_eq!(enclosure.attr("url"), Some(url));
    assert_eq!(enclosure.attr("type"), Some("application/x-nzb"));
    // The size is known but not sent (`[newznab] report_size` is off by default), so the arrs
    // skip their size check: no size attribute, and the enclosure length says 0.
    assert_eq!(enclosure.attr("length"), Some("0"));
    assert_eq!(
        item.nz_attrs(),
        [
            ("category", "5000"),
            ("category", "5040"),
            ("tvdbid", "452001"),
            ("season", "1"),
            ("episode", "1"),
            ("language", "Urdu"),
        ]
    );

    // Another episode, a season Sonarr sends as `00`, a whole season and the whole series.
    let count = async |q: &str| items(&api(&f, q).await).len();
    assert_eq!(count("t=tvsearch&tvdbid=452001&season=1&ep=2").await, 0);
    assert_eq!(count("t=tvsearch&tvdbid=452001&season=00&ep=1").await, 0);
    assert_eq!(count("t=tvsearch&tvdbid=452001&season=1").await, 1);
    assert_eq!(count("t=tvsearch&tvdbid=452001").await, 1);
    assert_eq!(count("t=tvsearch&tvdbid=1&season=1&ep=1").await, 0);
    // Wrong categories, ids Tubarr does not index, a later page.
    assert_eq!(count("t=tvsearch&tvdbid=452001&cat=2000").await, 0);
    assert_eq!(count("t=tvsearch&tvmazeid=5&season=1&ep=1").await, 0);
    assert_eq!(count("t=tvsearch&tvdbid=452001&offset=100").await, 0);
}

#[tokio::test]
async fn tvsearch_for_a_daily_show_by_air_date() {
    let f = seeded().await;
    // What Sonarr sends for a daily episode: season=yyyy, ep=MM/dd.
    let found = items(&api(&f, "t=tvsearch&tvdbid=9&season=2026&ep=10/14").await);
    assert_eq!(
        titles(&found),
        ["Khabarnaak.Live.2026.10.14.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR"]
    );
    let item = &found[0];
    // No size known: no size attribute, and the enclosure length says 0.
    assert_eq!(
        item.nz_attrs(),
        [
            ("category", "5000"),
            ("category", "5040"),
            ("tvdbid", "9"),
            ("tvairdate", "2026-10-14"),
            ("language", "Urdu"),
        ]
    );
    assert_eq!(item.child("enclosure").attr("length"), Some("0"));
    let count = async |q: &str| items(&api(&f, q).await).len();
    assert_eq!(count("t=tvsearch&tvdbid=9&season=2026&ep=10/15").await, 0);
    assert_eq!(count("t=tvsearch&tvdbid=9&season=2026").await, 1);
}

#[tokio::test]
async fn known_sizes_go_out_when_report_size_is_on() {
    let f = seed(
        build_with(
            "http://localhost:8480",
            Some(KEY),
            "[newznab]\nreport_size = true\n",
        )
        .await,
    )
    .await;
    let tv = items(&api(&f, "t=tvsearch&tvdbid=452001&season=1&ep=1").await);
    assert_eq!(tv[0].child("enclosure").attr("length"), Some("1234567890"));
    assert!(tv[0].nz_attrs().contains(&("size", "1234567890")));
    let film = items(&api(&f, "t=movie&tmdbid=352173").await);
    assert!(film[0].nz_attrs().contains(&("size", "1234567890")));
    // Still none for a release whose size is unknown.
    let daily = items(&api(&f, "t=tvsearch&tvdbid=9&season=2026&ep=10/14").await);
    assert_eq!(daily[0].child("enclosure").attr("length"), Some("0"));
    assert!(daily[0].nz_attrs().iter().all(|(name, _)| *name != "size"));
}

#[tokio::test]
async fn movie_by_tmdb_or_imdb() {
    let f = seeded().await;
    for query in [
        "t=movie&cat=2000,2040&tmdbid=352173&imdbid=4598756",
        "t=movie&tmdbid=352173",
        "t=movie&imdbid=4598756",
        "t=movie&imdbid=tt4598756",
    ] {
        let found = items(&api(&f, query).await);
        assert_eq!(
            titles(&found),
            ["Theri.2016.1080p.WEB-DL.AAC2.0.H.264.HINDI-TUBARR"],
            "{query}"
        );
        assert_eq!(found[0].text_of("category"), "Movies > HD");
        assert_eq!(
            found[0].nz_attrs(),
            [
                ("category", "2000"),
                ("category", "2040"),
                ("tmdbid", "352173"),
                ("imdb", "4598756"),
                ("language", "Hindi"),
            ]
        );
    }
    assert!(items(&api(&f, "t=movie&tmdbid=1").await).is_empty());
    assert!(items(&api(&f, "t=movie&tmdbid=352173&cat=5040").await).is_empty());
}

#[tokio::test]
async fn rss_lists_recent_releases_by_kind() {
    let f = seeded().await;
    let tv = items(&api(&f, "t=tvsearch&cat=5030,5040&extended=1").await);
    assert_eq!(
        titles(&tv),
        [
            "Khabarnaak.Live.2026.10.14.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR",
            "Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR",
        ]
    );
    let films = items(&api(&f, "t=movie&cat=2000").await);
    assert_eq!(
        titles(&films),
        ["Theri.2016.1080p.WEB-DL.AAC2.0.H.264.HINDI-TUBARR"]
    );
    assert_eq!(items(&api(&f, "t=search").await).len(), 3);
    assert_eq!(items(&api(&f, "t=search&cat=2040").await).len(), 1);
    assert_eq!(items(&api(&f, "t=search&limit=1").await).len(), 1);
    assert!(items(&api(&f, "t=search&offset=1").await).is_empty());
}

#[tokio::test]
async fn empty_rss_holds_one_placeholder_without_ids() {
    let f = empty().await;
    for query in ["t=tvsearch&cat=5030,5040", "t=movie&cat=2000", "t=search"] {
        let found = items(&api(&f, query).await);
        assert_eq!(
            titles(&found),
            ["Tubarr.Placeholder.No.Releases.Yet"],
            "{query}"
        );
        let attrs = found[0].nz_attrs();
        assert!(
            attrs.iter().all(|(name, _)| *name == "category"),
            "{query}: {attrs:?}"
        );
    }
    // Its NZB does not exist, and searches with ids or words stay empty.
    assert_eq!(
        error_code(&api(&f, "t=get&id=tubarr-placeholder").await),
        "300"
    );
    assert!(items(&api(&f, "t=tvsearch&tvdbid=1&season=1&ep=1").await).is_empty());
    assert!(items(&api(&f, "t=search&q=anything").await).is_empty());
    assert!(items(&api(&f, "t=search&offset=100").await).is_empty());
}

#[tokio::test]
async fn text_search_matches_release_names() {
    let f = seeded().await;
    let search = async |q: &str| {
        let found = items(&api(&f, q).await);
        titles(&found)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(
        search("t=search&q=kabhi+main").await,
        ["Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR"]
    );
    assert_eq!(
        search("t=movie&q=Theri%202016").await,
        ["Theri.2016.1080p.WEB-DL.AAC2.0.H.264.HINDI-TUBARR"]
    );
    assert_eq!(
        search("t=tvsearch&q=Kabhi%20Main%20Kabhi%20Tum&season=1&ep=1")
            .await
            .len(),
        1
    );
    assert!(search("t=tvsearch&q=Kabhi&season=2&ep=1").await.is_empty());
    assert!(search("t=movie&q=Kabhi").await.is_empty());
    assert!(search("t=search&q=Theri+1999").await.is_empty());
}

#[tokio::test]
async fn nzb_download_passes_the_arrs_check() {
    let f = seeded().await;
    let answer = api(&f, "t=get&id=0eD-GQCutAU").await;
    assert_eq!(answer.status, StatusCode::OK);
    assert!(answer.content_type.starts_with("application/x-nzb"));
    assert_eq!(
        answer.headers["content-disposition"],
        "attachment; filename=\"Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR.nzb\""
    );
    let nzb = answer.xml();
    // NzbValidationService: root `nzb`, at least one `file` in the root's namespace.
    assert_eq!(nzb.name, "nzb");
    assert_eq!(
        nzb.attr("xmlns"),
        Some("http://www.newzbin.com/DTD/2003/nzb")
    );
    assert!(!nzb.all("file").is_empty());
    let metas: Vec<(&str, &str)> = nzb
        .child("head")
        .all("meta")
        .into_iter()
        .map(|m| (m.attr("type").unwrap(), m.text.as_str()))
        .collect();
    assert!(
        metas.contains(&("tubarr-video", "0eD-GQCutAU")),
        "{metas:?}"
    );
    let file = nzb.child("file");
    assert!(!file.child("groups").all("group").is_empty());
    assert!(!file.child("segments").all("segment").is_empty());
}

#[tokio::test]
async fn post_reaches_the_same_api() {
    let f = empty().await;
    let response = f
        .app
        .clone()
        .oneshot(
            Request::post(format!("/api?t=caps&apikey={KEY}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

/// The recorded Sonarr and Radarr requests from the test kit, answered over data shaped like the
/// kit's example answers, and compared with those answers element by element. Differences on
/// purpose: Tubarr's guids also name the target (`tubarr-<id>-S01E01`), and its NZB links are
/// `/api?t=get&id=<id>` (the kit's answers were changed from `/nzb/<id>` to match).
#[tokio::test]
async fn recorded_arr_requests_get_the_good_answers() {
    let key = support::fixture_json("arr/requests.json")["api_key"]
        .as_str()
        .unwrap()
        .to_owned();
    let f = build("http://tubarr:9797", Some(&key)).await;
    let show =
        f.db.insert_mapping(&mapping(
            MappingKind::Show,
            Some(100_001),
            None,
            "Kabhi Main Kabhi Tum",
        ))
        .await
        .unwrap()
        .id;
    let film =
        f.db.insert_mapping(&mapping(MappingKind::Film, None, Some(200_001), "Theri"))
            .await
            .unwrap()
            .id;
    let ep = PinTarget::Episode {
        season: 1,
        episode: 1,
    };
    f.db.insert_release(&NewRelease {
        size: None,
        published_at: 1_719_943_200,
        ..release(show, "0eD-GQCutAU", ep, "Urdu")
    })
    .await
    .unwrap();
    f.db.insert_release(&NewRelease {
        size: None,
        year: Some(2016),
        imdb_id: Some(1),
        published_at: 1_552_651_200,
        ..release(film, "ldmPWRb-XcM", PinTarget::Film, "Hindi")
    })
    .await
    .unwrap();

    let recorded: Vec<_> = support::arr_requests()
        .into_iter()
        .filter(|r| r.param("t").is_some())
        .collect();
    assert_eq!(
        recorded.len(),
        11,
        "every recorded Newznab request is checked"
    );
    for request in recorded {
        assert_eq!(request.method, "GET", "{}", request.name);
        let response = f
            .app
            .clone()
            .oneshot(Request::get(request.uri()).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{}", request.name);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let got = parse(std::str::from_utf8(&body).unwrap());
        let want = parse(&String::from_utf8(request.response_bytes()).unwrap());
        same_shape(&got, &want, &request.name);
    }
}

/// `got` has the elements of `want`, in order, with the same attributes and text. Only element
/// names are compared for caps' subcategory labels, NZB file details and guids, which carry no
/// meaning for the arrs.
fn same_shape(got: &El, want: &El, at: &str) {
    let at = format!("{at} > {}", want.name);
    assert_eq!(got.name, want.name, "{at}");
    let loose = matches!(
        want.name.as_str(),
        "guid" | "server" | "subcat" | "file" | "segment" | "description"
    ) || at.contains("> file >");
    if !loose {
        for (name, value) in &want.attrs {
            assert_eq!(got.attr(name), Some(value.as_str()), "{at} @{name}");
        }
        assert_eq!(got.text.trim(), want.text.trim(), "{at} text");
    }
    let names = |e: &El| {
        e.children
            .iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>()
    };
    // Tubarr may add elements the example lacks (item descriptions, more search types), never
    // drop or reorder the example's.
    let mut got_children = got.children.iter();
    for child in &want.children {
        let found = got_children
            .by_ref()
            .find(|g| g.name == child.name)
            .unwrap_or_else(|| panic!("{at}: missing <{}> in {:?}", child.name, names(got)));
        same_shape(found, child, &at);
    }
}
