//! A grab through the SABnzbd `addfile` call writes the `.srt` captions next to the `.strm`,
//! looked up with a fake yt-dlp and fetched from a fake caption server.

mod support;

use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, header},
    routing::get,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use tubarr::{AppState, binaries::BinaryManager, config::Config, store::Db};

const RELEASE: &str = "Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR";
const USER_AGENT: &str = "tubarr-test-agent";

const MANUAL_EN: &str = "WEBVTT\nKind: captions\nLanguage: en\n\n00:00:01.000 --> 00:00:03.500 align:start position:0%\n<i>Previously</i> on Kabhi Main Kabhi Tum\n\n00:00:03.500 --> 00:00:05.000\nWhere were you?\n";

const AUTO_UR: &str = "WEBVTT\nKind: captions\nLanguage: ur\n\n00:00:00.000 --> 00:00:02.000 align:start position:0%\n \nآپ<00:00:00.500><c> کہاں</c><00:00:01.000><c> تھے</c>\n\n00:00:02.000 --> 00:00:02.010 align:start position:0%\nآپ کہاں تھے\n \n\n00:00:02.010 --> 00:00:04.000 align:start position:0%\nآپ کہاں تھے\nمیں<00:00:02.500><c> گھر</c>\n";

/// Paths the caption server was asked for, with the User-Agent each request carried.
type Seen = Arc<Mutex<Vec<(String, String)>>>;

async fn caption(State(seen): State<Seen>, headers: HeaderMap, request: Request) -> String {
    let path = request.uri().path().to_owned();
    let agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    seen.lock().unwrap().push((path.clone(), agent));
    match path.as_str() {
        "/manual-en.vtt" => MANUAL_EN.to_owned(),
        "/auto-ur.vtt" => AUTO_UR.to_owned(),
        "/slow.vtt" => {
            tokio::time::sleep(Duration::from_secs(30)).await;
            MANUAL_EN.to_owned()
        }
        _ => "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\ntranslated\n".to_owned(),
    }
}

/// Serves captions on a free local port and returns its base URL.
async fn caption_server(seen: Seen) -> String {
    let app = Router::new()
        .route("/{file}", get(caption))
        .with_state(seen);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    base
}

/// `yt-dlp -J` output for the test episode with uploaded English and French (the French one
/// hangs), Urdu speech-to-text and YouTube's machine translations of it.
fn lookup_json(base: &str) -> Value {
    let track = |ext: &str, url: String| json!({ "ext": ext, "url": url, "name": "x" });
    json!({
        "id": "0eD-GQCutAU",
        "title": "Kabhi Main Kabhi Tum Episode 1",
        "duration": 2100.0,
        "formats": [
            {
                "format_id": "137", "url": "https://rr1.googlevideo.com/videoplayback?expire=4102444800",
                "ext": "mp4", "vcodec": "avc1.640028", "acodec": "none", "protocol": "https",
                "http_headers": { "User-Agent": USER_AGENT }
            },
            {
                "format_id": "140", "url": "https://rr1.googlevideo.com/videoplayback?expire=4102444800",
                "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2", "protocol": "https",
                "http_headers": { "User-Agent": USER_AGENT }
            }
        ],
        "subtitles": {
            "en": [track("json3", format!("{base}/manual-en.json3")), track("vtt", format!("{base}/manual-en.vtt"))],
            "fr": [track("vtt", format!("{base}/slow.vtt"))]
        },
        "automatic_captions": {
            "ur": [track("vtt", format!("{base}/auto-ur.vtt?lang=ur&kind=asr"))],
            "ur-orig": [track("vtt", format!("{base}/auto-ur.vtt?lang=ur&kind=asr"))],
            "hi": [track("vtt", format!("{base}/translated-hi.vtt?lang=ur&kind=asr&tlang=hi"))],
            "en": [track("vtt", format!("{base}/translated-en.vtt?lang=ur&kind=asr&tlang=en"))]
        }
    })
}

/// A yt-dlp that prints `stdout` (when given) and exits with `code`.
fn fake_ytdlp(dir: &Path, stdout: Option<&Value>, code: i32) -> PathBuf {
    let path = dir.join("yt-dlp");
    let out = match stdout {
        Some(json) => {
            let file = dir.join("lookup.json");
            std::fs::write(&file, json.to_string()).unwrap();
            format!("cat '{}'\n", file.display())
        }
        // A failure that says nothing about the video: the grab goes ahead without captions. (A
        // settled one like "Video unavailable" fails the grab, see tests/sabnzbd.rs.)
        None => "echo 'ERROR: [youtube] 0eD-GQCutAU: Some new failure' >&2\n".to_owned(),
    };
    support::install_executable(&path, format!("#!/bin/sh\n{out}exit {code}\n").as_bytes());
    path
}

struct Grab {
    folder: PathBuf,
    answer: Value,
    took: Duration,
    _dir: tempfile::TempDir,
}

/// Sends Sonarr's recorded addfile through `/api` with the given yt-dlp output.
async fn grab(stdout: Option<&Value>, code: i32, subtitles: &str) -> Grab {
    let dir = support::temp_dir();
    let ytdlp = fake_ytdlp(dir.path(), stdout, code);
    let key_file = dir.path().join("api_key");
    std::fs::write(&key_file, support::arr_api_key()).unwrap();
    let complete = dir.path().join("complete");
    let config = Config::from_toml(&format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\ncomplete_dir = {:?}\n\
         api_key_file = {:?}\n[binaries]\nyt_dlp = {:?}\n[subtitles]\n{subtitles}\n",
        dir.path(),
        complete,
        key_file,
        ytdlp
    ))
    .unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let app = tubarr::app(AppState::new(config, db, binaries).unwrap());

    let req = support::arr_request("sonarr_sab_addfile");
    let (content_type, body) = req.multipart_body().unwrap();
    let request = Request::post(req.uri())
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .unwrap();
    let started = Instant::now();
    let response = app.oneshot(request).await.unwrap();
    let took = started.elapsed();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    Grab {
        folder: complete.join("tv").join(RELEASE),
        answer: serde_json::from_slice(&body).unwrap(),
        took,
        _dir: dir,
    }
}

fn files(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn addfile_writes_the_wanted_captions_before_it_answers() {
    let seen: Seen = Arc::default();
    let base = caption_server(seen.clone()).await;
    let g = grab(
        Some(&lookup_json(&base)),
        0,
        "languages = [\"en\", \"ur\", \"hi\", \"fr\"]\ntimeout = \"2s\"",
    )
    .await;

    assert_eq!(g.answer["status"], true, "{}", g.answer);
    // The hanging French track is cut off at the budget, not waited for.
    assert!(g.took < Duration::from_secs(10), "took {:?}", g.took);
    assert_eq!(
        files(&g.folder),
        [
            format!("{RELEASE}.en.srt"),
            format!("{RELEASE}.strm"),
            format!("{RELEASE}.ur.srt"),
        ]
    );
    assert_eq!(
        std::fs::read_to_string(g.folder.join(format!("{RELEASE}.strm"))).unwrap(),
        "http://localhost:8480/play/0eD-GQCutAU\n"
    );
    assert_eq!(
        std::fs::read_to_string(g.folder.join(format!("{RELEASE}.en.srt"))).unwrap(),
        "1\n00:00:01,000 --> 00:00:03,500\n<i>Previously</i> on Kabhi Main Kabhi Tum\n\n\
         2\n00:00:03,500 --> 00:00:05,000\nWhere were you?\n\n"
    );
    assert_eq!(
        std::fs::read_to_string(g.folder.join(format!("{RELEASE}.ur.srt"))).unwrap(),
        "1\n00:00:00,000 --> 00:00:02,000\nآپ کہاں تھے\n\n\
         2\n00:00:02,010 --> 00:00:04,000\nمیں گھر\n\n"
    );

    let seen = seen.lock().unwrap().clone();
    let mut paths: Vec<&str> = seen.iter().map(|(p, _)| p.as_str()).collect();
    paths.sort();
    // Machine translations are never fetched, and the uploaded English beats them.
    assert_eq!(paths, ["/auto-ur.vtt", "/manual-en.vtt", "/slow.vtt"]);
    assert!(
        seen.iter().all(|(_, agent)| agent == USER_AGENT),
        "{seen:?}"
    );
}

#[tokio::test]
async fn speech_to_text_can_be_turned_off() {
    let seen: Seen = Arc::default();
    let base = caption_server(seen.clone()).await;
    let g = grab(
        Some(&lookup_json(&base)),
        0,
        "languages = [\"ur\", \"en\"]\nautomatic = false",
    )
    .await;
    assert_eq!(g.answer["status"], true, "{}", g.answer);
    assert_eq!(
        files(&g.folder),
        [format!("{RELEASE}.en.srt"), format!("{RELEASE}.strm")]
    );
}

#[tokio::test]
async fn a_failed_lookup_still_writes_the_strm() {
    let g = grab(None, 1, "").await;
    assert_eq!(g.answer["status"], true, "{}", g.answer);
    assert_eq!(files(&g.folder), [format!("{RELEASE}.strm")]);
}
