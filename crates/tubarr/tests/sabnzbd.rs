//! The SABnzbd API driven with the requests Sonarr 4.0.20 and Radarr 6.4.4 really send.

mod support;

use std::{path::PathBuf, sync::Arc};

use axum::{Router, body::Body, extract::Request, http::header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use support::{ArrRequest, arr_request, fixture_string};
use tower::ServiceExt;
use tubarr::{
    AppState,
    binaries::BinaryManager,
    config::Config,
    health::HealthEvent,
    sabnzbd::{self, StrmError, StrmJob, StrmWriter, Upload},
    store::{Db, GrabStatus, MappingKind, NewGrab, NewMapping, NewRelease, PinTarget},
};

/// The complete_dir the recorded good answers were made with.
const RECORDED_DIR: &str = "/downloads/tubarr";

struct Harness {
    router: Router,
    state: AppState,
    complete: PathBuf,
    _dir: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        Self::with_config(|_| String::new()).await
    }

    /// [`Harness::new`] with more config, written by `extra` from the test folder.
    async fn with_config(extra: impl FnOnce(&std::path::Path) -> String) -> Self {
        let dir = support::temp_dir();
        let complete = dir.path().join("complete");
        let key_file = dir.path().join("api_key");
        std::fs::write(&key_file, format!("{}\n", support::arr_api_key())).unwrap();
        // Every grab looks its video up first: no limiter waits between the grabs here.
        let config = Config::from_toml(&format!(
            "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\ncomplete_dir = {:?}\napi_key_file = {:?}\n\
             [rate_limit]\nlookups = 100\ninterval = \"1s\"\nburst = 100\njitter = \"0s\"\n{}",
            dir.path(),
            complete,
            key_file,
            extra(dir.path())
        ))
        .unwrap();
        let db = Db::open(&config.database_path()).await.unwrap();
        let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
        let state = AppState::new(config, db, binaries).unwrap();
        let router = tubarr::app(state.clone());
        Self {
            router,
            state,
            complete,
            _dir: dir,
        }
    }

    async fn send(&self, req: &ArrRequest) -> Value {
        self.send_uri(req, &req.uri()).await
    }

    async fn send_uri(&self, req: &ArrRequest, uri: &str) -> Value {
        let builder = Request::builder().method(req.method.as_str()).uri(uri);
        let request = match req.multipart_body() {
            Some((content_type, body)) => builder
                .header(header::CONTENT_TYPE, content_type)
                .body(Body::from(body)),
            None => builder.body(Body::empty()),
        }
        .unwrap();
        let response = self.router.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), 200);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&body).unwrap()
    }

    /// The recorded good answer, with the recorded complete_dir swapped for this test's.
    fn expected(&self, req: &ArrRequest) -> Value {
        let text = String::from_utf8(req.response_bytes()).unwrap();
        serde_json::from_str(&text.replace(RECORDED_DIR, self.complete.to_str().unwrap())).unwrap()
    }

    async fn add(&self, name: &str) -> String {
        let req = arr_request(name);
        let answer = self.send(&req).await;
        assert_same_shape(&answer, &self.expected(&req), "addfile");
        assert_eq!(answer["status"], true);
        let id = answer["nzo_ids"][0].as_str().unwrap().to_owned();
        assert!(id.starts_with("SABnzbd_nzo_"), "{id}");
        id
    }

    async fn history(&self, name: &str) -> Vec<Value> {
        let answer = self.send(&arr_request(name)).await;
        answer["history"]["slots"].as_array().unwrap().clone()
    }
}

/// Same keys and value types all the way down, so the arrs' parsers see what they expect.
fn assert_same_shape(actual: &Value, expected: &Value, at: &str) {
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            let mut keys_a: Vec<_> = a.keys().collect();
            let mut keys_e: Vec<_> = e.keys().collect();
            keys_a.sort();
            keys_e.sort();
            assert_eq!(keys_a, keys_e, "keys at {at}");
            for (k, v) in e {
                assert_same_shape(&a[k], v, &format!("{at}.{k}"));
            }
        }
        (Value::Array(a), Value::Array(e)) => {
            assert_eq!(a.len(), e.len(), "length at {at}");
            for (i, (x, y)) in a.iter().zip(e).enumerate() {
                assert_same_shape(x, y, &format!("{at}[{i}]"));
            }
        }
        (a, e) => assert_eq!(
            std::mem::discriminant(a),
            std::mem::discriminant(e),
            "type at {at}: {a} vs {e}"
        ),
    }
}

#[tokio::test]
async fn handshake_answers_match_the_recorded_ones() {
    let h = Harness::new().await;
    for name in [
        "sonarr_sab_version",
        "sonarr_sab_get_config",
        "sonarr_sab_fullstatus",
        "sonarr_sab_queue",
    ] {
        let req = arr_request(name);
        assert_eq!(h.send(&req).await, h.expected(&req), "{name}");
    }
}

#[tokio::test]
async fn addfile_writes_the_strm_and_history_shows_it_completed() {
    let h = Harness::new().await;
    let mut events = h.state.health.events().subscribe();
    let tv = h.add("sonarr_sab_addfile").await;
    let movie = h.add("radarr_sab_addfile").await;

    let release = "Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR";
    // Each grab is announced for the Discord grabs card.
    assert_eq!(
        events.try_recv().unwrap(),
        HealthEvent::GrabCompleted {
            video_id: "0eD-GQCutAU".to_owned(),
            release_name: release.to_owned(),
            category: "tv".to_owned(),
        }
    );
    assert!(matches!(
        events.try_recv().unwrap(),
        HealthEvent::GrabCompleted { category, .. } if category == "movies"
    ));
    let folder = h.complete.join("tv").join(release);
    let strm = std::fs::read_to_string(folder.join(format!("{release}.strm"))).unwrap();
    assert_eq!(strm, "http://localhost:8480/play/0eD-GQCutAU\n");
    let film = "Theri.2016.1080p.WEB-DL.AAC2.0.H.264.HINDI-TUBARR";
    let strm = std::fs::read_to_string(h.complete.join(format!("movies/{film}/{film}.strm")));
    assert_eq!(strm.unwrap(), "http://localhost:8480/play/ldmPWRb-XcM\n");

    // Each arr sees only its own category, and the slot matches the recorded one but for the id
    // and the real file size.
    for (history, nzo_id, bytes) in [
        ("sonarr_sab_history", &tv, 39),
        ("radarr_sab_history", &movie, 39),
    ] {
        let slots = h.history(history).await;
        let mut expected = h.expected(&arr_request(history));
        let slot = &mut expected["history"]["slots"][0];
        slot["nzo_id"] = json!(nzo_id);
        slot["bytes"] = json!(bytes);
        assert_eq!(
            slots,
            expected["history"]["slots"].as_array().unwrap().clone()
        );
    }

    // Each grab queues its video for a background read of its sample tables.
    assert_eq!(h.state.play.prebuild_stats().queued, 2);

    // Nothing is ever in progress.
    let queue = h.send(&arr_request("sonarr_sab_queue")).await;
    assert_eq!(queue["queue"]["slots"], json!([]));
}

#[tokio::test]
async fn no_background_table_reads_when_switched_off() {
    let h = Harness::with_config(|_| "[play]\nprebuild_tables = false\n".to_owned()).await;
    h.add("sonarr_sab_addfile").await;
    assert_eq!(h.state.play.prebuild_stats().queued, 0);
}

#[tokio::test]
async fn history_delete_removes_the_record_and_its_files() {
    let h = Harness::new().await;
    let id = h.add("sonarr_sab_addfile").await;
    let req = arr_request("sonarr_sab_history_delete");
    let uri = req.uri().replace("SABnzbd_nzo_tubarr_0eD-GQCutAU", &id);

    assert_eq!(h.send_uri(&req, &uri).await, h.expected(&req));
    assert!(h.history("sonarr_sab_history").await.is_empty());
    assert!(!h.complete.join("tv").read_dir().unwrap().any(|_| true));
    // Deleting again is still fine.
    assert_eq!(h.send_uri(&req, &uri).await, json!({ "status": true }));
}

#[tokio::test]
async fn queue_delete_removes_the_record() {
    let h = Harness::new().await;
    let id = h.add("sonarr_sab_addfile").await;
    let req = arr_request("sonarr_sab_queue_delete");
    let uri = req.uri().replace("SABnzbd_nzo_tubarr_0eD-GQCutAU", &id);

    assert_eq!(h.send_uri(&req, &uri).await, h.expected(&req));
    assert!(h.history("sonarr_sab_history").await.is_empty());
    assert!(h.state.db.grab(&id).await.unwrap().is_none());
}

#[tokio::test]
async fn history_pages_with_start_and_limit() {
    let h = Harness::new().await;
    let first = h.add("sonarr_sab_addfile").await;
    let second = h.add("sonarr_sab_addfile").await;
    let req = arr_request("sonarr_sab_history");
    let page = |start: u32, limit: u32| {
        req.uri()
            .replace("start=0", &format!("start={start}"))
            .replace("limit=60", &format!("limit={limit}"))
    };
    let ids = |answer: Value| -> Vec<String> {
        answer["history"]["slots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["nzo_id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        ids(h.send_uri(&req, &page(0, 0)).await),
        [second.clone(), first.clone()]
    );
    assert_eq!(
        ids(h.send_uri(&req, &page(0, 1)).await),
        std::slice::from_ref(&second)
    );
    assert_eq!(
        ids(h.send_uri(&req, &page(1, 1)).await),
        std::slice::from_ref(&first)
    );
    assert!(ids(h.send_uri(&req, &page(2, 1)).await).is_empty());
}

#[tokio::test]
async fn history_never_answers_more_than_its_cap() {
    let h = Harness::new().await;
    let total = sabnzbd::HISTORY_MAX + 5;
    for n in 0..total {
        h.state
            .db
            .insert_grab(&NewGrab {
                nzo_id: format!("SABnzbd_nzo_{n:016}"),
                release_name: format!("Show.S01E{n:02}.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR"),
                video_id: "0eD-GQCutAU".to_owned(),
                category: "tv".to_owned(),
                status: GrabStatus::Completed,
                storage_path: format!("/nowhere/{n}"),
                fail_message: None,
            })
            .await
            .unwrap();
    }
    let req = arr_request("sonarr_sab_history");
    let count = |limit: &str| {
        let uri = req.uri().replace("limit=60", &format!("limit={limit}"));
        let h = &h;
        let req = &req;
        async move {
            h.send_uri(req, &uri).await["history"]["slots"]
                .as_array()
                .unwrap()
                .len()
        }
    };
    // Sonarr's own page, everything SABnzbd style, and more than the cap.
    assert_eq!(count("60").await, 60);
    assert_eq!(count("0").await, sabnzbd::HISTORY_MAX as usize);
    assert_eq!(count("100000").await, sabnzbd::HISTORY_MAX as usize);
}

#[tokio::test]
async fn invalid_nzbs_are_refused_and_nothing_is_written() {
    let h = Harness::new().await;
    let req = arr_request("sonarr_sab_addfile");
    let good = fixture_string("arr/newznab/episode.nzb");
    let cases = [
        "garbage".to_owned(),
        good.replace("<meta type=\"tubarr-video\">0eD-GQCutAU</meta>", ""),
        good.replace("0eD-GQCutAU</meta>", "x/y</meta>"),
    ];
    for nzb in cases {
        let boundary = "b";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"name\"; filename=\"x.nzb\"\r\n\r\n{nzb}\r\n--{boundary}--\r\n"
        );
        let request = Request::post(req.uri())
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body))
            .unwrap();
        let response = h.router.clone().oneshot(request).await.unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let answer: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(answer["status"], false, "{answer}");
        assert!(
            answer["error"].as_str().unwrap().contains("NZB"),
            "{answer}"
        );
    }

    // No file at all.
    let request = Request::post(req.uri()).body(Body::empty()).unwrap();
    let response = h.router.clone().oneshot(request).await.unwrap();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let answer: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        answer,
        json!({ "status": false, "error": "no NZB file in the request" })
    );

    assert!(!h.complete.exists());
    assert!(h.history("sonarr_sab_history").await.is_empty());
}

struct FailingWriter;

impl StrmWriter for FailingWriter {
    async fn write(&self, job: StrmJob<'_>) -> Result<PathBuf, StrmError> {
        Err(StrmError {
            path: job.release_name.to_owned(),
            source: std::io::Error::other("disk full"),
        })
    }
}

#[tokio::test]
async fn a_failed_write_records_no_grab() {
    let h = Harness::new().await;
    let upload = Upload {
        nzb: fixture_string("arr/newznab/episode.nzb").into_bytes(),
        file_name: None,
        category: Some("tv".into()),
        nzbname: None,
    };
    let err = sabnzbd::add_file(&h.state, &FailingWriter, upload)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("disk full"), "{err}");
    assert!(h.history("sonarr_sab_history").await.is_empty());
}

#[tokio::test]
async fn pause_resume_and_unknown_modes() {
    let h = Harness::new().await;
    let req = arr_request("sonarr_sab_version");
    for (mode, answer) in [
        ("pause", json!({ "status": true })),
        ("resume", json!({ "status": true })),
        (
            "warnings",
            json!({ "status": false, "error": "not implemented" }),
        ),
    ] {
        let uri = req.uri().replace("mode=version", &format!("mode={mode}"));
        assert_eq!(h.send_uri(&req, &uri).await, answer, "{mode}");
    }
}

#[tokio::test]
async fn a_missing_or_wrong_key_is_refused_like_sabnzbd() {
    let h = Harness::new().await;
    let req = arr_request("sonarr_sab_version");
    let key = format!("apikey={}", support::arr_api_key());
    for (uri, error) in [
        (req.uri().replace(&key, "apikey=wrong"), "API Key Incorrect"),
        (
            req.uri().replace(&format!("&{key}"), ""),
            "API Key Required",
        ),
    ] {
        assert!(!uri.contains(&key), "{uri}");
        assert_eq!(
            h.send_uri(&req, &uri).await,
            json!({ "status": false, "error": error })
        );
    }
}

/// A release joined from two parts: its Newznab NZB carries both ids, and the grab writes a
/// `.strm` that plays them in order, without captions.
#[tokio::test]
async fn a_joined_release_goes_from_nzb_to_strm_with_both_ids() {
    const P1: &str = "0eD-GQCutAU";
    const P2: &str = "ldmPWRb-XcM";
    let h = Harness::new().await;
    let mapping = h
        .state
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
        .unwrap();
    h.state
        .db
        .insert_release(&NewRelease {
            mapping_id: mapping.id,
            video_id: P1.to_owned(),
            part_ids: vec![P1.to_owned(), P2.to_owned()],
            target: PinTarget::Episode {
                season: 1,
                episode: 26,
            },
            year: None,
            imdb_id: None,
            video_title: "Kabhi Main Kabhi Tum - Episode 26 - Part 1".to_owned(),
            language: "Urdu".to_owned(),
            height: 1080,
            size: Some(620_000_000),
            published_at: 1_792_000_000,
        })
        .await
        .unwrap();

    let ids = format!("{P1},{P2}");
    let request = Request::get(format!(
        "/api?t=get&id={ids}&apikey={}",
        support::arr_api_key()
    ))
    .body(Body::empty())
    .unwrap();
    let response = h.router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), 200);
    let nzb = response.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8(nzb.to_vec()).unwrap();
    assert!(
        text.contains(&format!("<meta type=\"tubarr-video\">{ids}</meta>")),
        "{text}"
    );

    let upload = Upload {
        nzb: nzb.to_vec(),
        file_name: None,
        category: Some("tv".into()),
        nzbname: None,
    };
    let writer = sabnzbd::GrabWriter::new(&h.state);
    sabnzbd::add_file(&h.state, &writer, upload).await.unwrap();
    let release = "Kabhi.Main.Kabhi.Tum.S01E26.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR";
    let folder = h.complete.join("tv").join(release);
    let strm = std::fs::read_to_string(folder.join(format!("{release}.strm"))).unwrap();
    assert_eq!(strm, format!("http://localhost:8480/play/{ids}\n"));
    // Only the .strm: captions are not joined.
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1);
    let history = h.history("sonarr_sab_history").await;
    assert_eq!(history.len(), 1);
}

struct UnreachableWriter;

impl StrmWriter for UnreachableWriter {
    async fn write(&self, _: StrmJob<'_>) -> Result<PathBuf, StrmError> {
        panic!("an unplayable grab must not write files");
    }
}

#[tokio::test]
async fn a_grab_of_an_unplayable_video_fails_in_history_and_marks_the_video() {
    let h = Harness::with_config(|dir| {
        let json = dir.join("opus.json");
        std::fs::write(
            &json,
            r#"{"id": "0eD-GQCutAU", "title": "T", "formats": [
              {"format_id": "251", "vcodec": "none", "acodec": "opus", "protocol": "https",
               "url": "https://rr3---sn-a.googlevideo.com/videoplayback?itag=251"},
              {"format_id": "137", "vcodec": "avc1.640028", "acodec": "none", "protocol": "https",
               "url": "https://rr3---sn-a.googlevideo.com/videoplayback?itag=137"}]}"#,
        )
        .unwrap();
        let fake = dir.join("yt-dlp");
        support::install_executable(
            &fake,
            format!("#!/bin/sh\ncat '{}'\n", json.display()).as_bytes(),
        );
        format!("[binaries]\nyt_dlp = {fake:?}\n")
    })
    .await;
    let upload = Upload {
        nzb: fixture_string("arr/newznab/episode.nzb").into_bytes(),
        file_name: None,
        category: Some("tv".into()),
        nzbname: None,
    };
    let answer = sabnzbd::add_file(&h.state, &UnreachableWriter, upload)
        .await
        .unwrap();
    assert_eq!(answer["status"], true, "{answer}");

    let history = h.history("sonarr_sab_history").await;
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["status"], "Failed");
    let message = history[0]["fail_message"].as_str().unwrap();
    assert!(
        message.contains("cannot play") && message.contains("AAC"),
        "{message}"
    );
    assert_eq!(history[0]["nzo_id"], answer["nzo_ids"][0]);
    assert!(!h.complete.exists(), "nothing written");

    let marked = h.state.db.unplayable_videos().await.unwrap();
    assert_eq!(marked.len(), 1);
    assert_eq!(marked[0].video_id, "0eD-GQCutAU");
}
