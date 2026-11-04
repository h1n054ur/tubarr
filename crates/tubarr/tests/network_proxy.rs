//! `[network] proxy`: everything toward YouTube goes through it, the arrs never do. The "proxy" is
//! a local recorder that answers plain HTTP requests in absolute form, and the YouTube hosts are
//! `.test` names that only it can reach. Nothing here reaches YouTube.

mod support;

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::json;
use tubarr::{
    AppState,
    arr::{App, connections},
    binaries::BinaryManager,
    config::Config,
    sabnzbd::{GrabWriter, StrmJob, StrmWriter},
};

const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";
const PASSWORD: &str = "hunter2-proxy";
const CAPTION: &str = "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nWhere were you?\n";

/// The absolute URLs the proxy was asked for.
type Seen = Arc<Mutex<Vec<String>>>;

async fn serve(router: Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    addr
}

/// An HTTP proxy for plain `http://` targets: it records each request and answers it itself.
async fn recorder() -> (std::net::SocketAddr, Seen) {
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let router = Router::new().fallback(move |uri: Uri| {
        let log = log.clone();
        async move {
            log.lock().unwrap().push(uri.to_string());
            answer(&uri)
        }
    });
    (serve(router).await, seen)
}

fn answer(uri: &Uri) -> Response {
    match (uri.host(), uri.path()) {
        (Some("youtube.test"), "/@arydigital") => format!(
            r#"<html><head><link rel="canonical" href="https://www.youtube.com/channel/{ARY}">
            <meta property="og:title" content="ARY Digital"></head><body></body></html>"#
        )
        .into_response(),
        (Some("rr1.googlevideo.test"), "/videoplayback") => "media bytes".into_response(),
        (Some("captions.test"), "/en.vtt") => CAPTION.into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A fake yt-dlp that records its arguments and its proxy environment. With `lookup` it prints
/// that `-J` output. Without, it fails quoting the proxy it was given, as yt-dlp does when it
/// cannot reach one.
fn fake_ytdlp(dir: &Path, lookup: Option<&serde_json::Value>) -> std::path::PathBuf {
    let path = dir.join("yt-dlp");
    let log = dir.join("yt-dlp.args");
    let env = dir.join("yt-dlp.env");
    let outcome = match lookup {
        Some(json) => {
            let file = dir.join("lookup.json");
            std::fs::write(&file, json.to_string()).unwrap();
            format!("cat '{}'\nexit 0\n", file.display())
        }
        None => "echo \"ERROR: Unable to connect to proxy $ALL_PROXY: Connection refused\" >&2\nexit 1\n"
            .to_owned(),
    };
    let script = format!(
        "#!/bin/sh\n: > '{log}'\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{log}'; done\n\
         env | grep -i '_proxy=' | sort > '{env}'\n{outcome}",
        log = log.display(),
        env = env.display(),
    );
    support::install_executable(&path, script.as_bytes());
    path
}

struct Kit {
    state: AppState,
    seen: Seen,
    proxy_url: String,
    sonarr_calls: Arc<Mutex<u32>>,
    dir: tempfile::TempDir,
}

async fn kit() -> Kit {
    kit_with(None).await
}

async fn kit_with(lookup: Option<&serde_json::Value>) -> Kit {
    let (proxy, seen) = recorder().await;

    let sonarr_calls = Arc::new(Mutex::new(0u32));
    let calls = sonarr_calls.clone();
    let sonarr = serve(Router::new().route(
        "/api/v3/system/status",
        get(move || {
            let calls = calls.clone();
            async move {
                *calls.lock().unwrap() += 1;
                Json(json!({"appName": "Sonarr", "version": "4.0.0"}))
            }
        }),
    ))
    .await;

    let dir = support::temp_dir();
    let ytdlp = fake_ytdlp(dir.path(), lookup);
    let proxy_url = format!("http://alice:{PASSWORD}@{proxy}");
    let proxy_file = dir.path().join("proxy");
    std::fs::write(&proxy_file, &proxy_url).unwrap();
    let sonarr_key = dir.path().join("sonarr_key");
    std::fs::write(&sonarr_key, "sonarr-key").unwrap();
    let toml = format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {data:?}\ncomplete_dir = {complete:?}\n\
         [binaries]\nyt_dlp = {ytdlp:?}\n\
         [watcher]\nfeed_url = \"http://youtube.test/feeds/videos.xml\"\n\
         [hooks]\nthumbnail_url = \"http://ytimg.test/\"\n\
         [sonarr]\nurl = \"http://{sonarr}\"\napi_key_file = {sonarr_key:?}\n\
         [fallback]\npo_token_url = \"http://bgutil:4416\"\n\
         [network]\nproxy_file = {proxy_file:?}\n",
        data = dir.path(),
        complete = dir.path().join("complete"),
    );
    let config = Config::from_toml(&toml).unwrap();
    let db = tubarr::store::Db::open(&config.database_path())
        .await
        .unwrap();
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let state = AppState::new(config, db, binaries).unwrap();
    Kit {
        state,
        seen,
        proxy_url,
        sonarr_calls,
        dir,
    }
}

impl Kit {
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    /// `NAME=value` lines of the proxy variables yt-dlp ran with.
    fn ytdlp_env(&self) -> std::collections::BTreeMap<String, String> {
        std::fs::read_to_string(self.dir.path().join("yt-dlp.env"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect()
    }

    fn ytdlp_args(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("yt-dlp.args"))
            .map(|s| s.lines().map(str::to_owned).collect())
            .unwrap_or_default()
    }
}

#[tokio::test]
async fn yt_dlp_gets_the_proxy_in_its_environment_and_its_errors_hide_the_password() {
    let kit = kit().await;
    let err = kit.state.resolver.resolve("0eD-GQCutAU").await.unwrap_err();
    let args = kit.ytdlp_args();
    assert!(args.last().unwrap().ends_with("/watch?v=0eD-GQCutAU"));
    assert!(!args.iter().any(|a| a == "--proxy"), "{args:?}");
    assert!(
        !args
            .iter()
            .any(|a| a.contains(PASSWORD) || a.contains("alice")),
        "the password is on the command line"
    );

    let env = kit.ytdlp_env();
    for name in [
        "HTTP_PROXY",
        "http_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        assert_eq!(env[name].trim_end_matches('/'), kit.proxy_url, "{name}");
    }
    assert_eq!(env["NO_PROXY"], "localhost,127.0.0.1,::1,bgutil");
    assert_eq!(env["no_proxy"], env["NO_PROXY"]);

    let message = format!("{err} {err:?}");
    assert!(!message.contains(PASSWORD), "{message}");
    assert!(!message.contains("alice"), "{message}");
}

#[tokio::test]
async fn channel_pages_and_feeds_go_through_the_proxy() {
    let kit = kit().await;
    let found = kit
        .state
        .channel_lookup
        .resolve("@arydigital")
        .await
        .unwrap();
    assert_eq!(found.channel_id, ARY);

    // The fake answers the feed with a 404: only where the request went matters here.
    let _ = kit.state.watcher.poll_feed(ARY).await;

    let seen = kit.seen();
    assert!(
        seen.iter()
            .any(|u| u.starts_with("http://youtube.test/@arydigital")),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|u| u.starts_with(&format!(
            "http://youtube.test/feeds/videos.xml?channel_id={ARY}"
        ))),
        "{seen:?}"
    );
}

#[tokio::test]
async fn the_play_relay_fetches_media_through_the_proxy() {
    let kit = kit().await;
    let body = kit
        .state
        .play
        .client()
        .get("http://rr1.googlevideo.test/videoplayback?itag=140")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "media bytes");
    assert_eq!(
        kit.seen(),
        vec!["http://rr1.googlevideo.test/videoplayback?itag=140".to_owned()]
    );
}

#[tokio::test]
async fn grab_captions_go_through_the_proxy() {
    let media = "http://rr1.googlevideo.test/videoplayback?expire=4102444800";
    let lookup = json!({
        "id": "0eD-GQCutAU",
        "title": "Kabhi Main Kabhi Tum Episode 1",
        "duration": 2100.0,
        "formats": [
            {
                "format_id": "137", "url": media, "ext": "mp4", "vcodec": "avc1.640028",
                "acodec": "none", "protocol": "https"
            },
            {
                "format_id": "140", "url": media, "ext": "m4a", "vcodec": "none",
                "acodec": "mp4a.40.2", "protocol": "https"
            }
        ],
        "subtitles": {
            "en": [{ "ext": "vtt", "url": "http://captions.test/en.vtt", "name": "English" }]
        }
    });
    let kit = kit_with(Some(&lookup)).await;
    let folder = GrabWriter::new(&kit.state)
        .write(StrmJob {
            release_name: "Kabhi.Main.Kabhi.Tum.S01E01",
            video_id: "0eD-GQCutAU",
            category_dir: "",
        })
        .await
        .unwrap();
    let srt = std::fs::read_to_string(folder.join("Kabhi.Main.Kabhi.Tum.S01E01.en.srt")).unwrap();
    assert!(srt.contains("Where were you?"), "{srt}");
    assert!(
        kit.seen()
            .iter()
            .any(|u| u.starts_with("http://captions.test/en.vtt")),
        "{:?}",
        kit.seen()
    );
}

#[tokio::test]
async fn sonarr_never_goes_through_the_proxy() {
    let kit = kit().await;
    let sonarr = kit.state.arrs.sonarr().unwrap();
    let version = connections::test(App::Sonarr, &sonarr).await.unwrap();
    assert_eq!(version, "4.0.0");
    assert_eq!(*kit.sonarr_calls.lock().unwrap(), 1);
    assert!(kit.seen().is_empty(), "{:?}", kit.seen());
}

#[tokio::test]
async fn health_shows_the_proxy_without_credentials() {
    let kit = kit().await;
    let report = tubarr::health::HealthReport::collect(&kit.state).await;
    let shown = report.outbound_proxy.clone().unwrap();
    assert!(shown.starts_with("http://127.0.0.1:"), "{shown}");
    assert!(!shown.contains(PASSWORD) && !shown.contains("alice"));
    let json = serde_json::to_string(&report).unwrap();
    assert!(!json.contains(PASSWORD), "{json}");
}

/// A SOCKS5 server that takes user name and password auth, records the target each CONNECT names
/// and pipes it to `upstream`, whatever the target. With `socks5h` the target is a host name.
async fn socks5(upstream: std::net::SocketAddr) -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let targets: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = targets.clone();
    tokio::spawn(async move {
        loop {
            let (mut client, _) = listener.accept().await.unwrap();
            let log = log.clone();
            tokio::spawn(async move {
                let mut head = [0u8; 2];
                client.read_exact(&mut head).await.unwrap();
                let mut methods = vec![0u8; head[1] as usize];
                client.read_exact(&mut methods).await.unwrap();
                assert!(methods.contains(&2), "no user name and password offered");
                client.write_all(&[5, 2]).await.unwrap();

                let mut version = [0u8; 2];
                client.read_exact(&mut version).await.unwrap();
                let mut user = vec![0u8; version[1] as usize];
                client.read_exact(&mut user).await.unwrap();
                let mut len = [0u8; 1];
                client.read_exact(&mut len).await.unwrap();
                let mut password = vec![0u8; len[0] as usize];
                client.read_exact(&mut password).await.unwrap();
                let ok = user == b"alice" && password == PASSWORD.as_bytes();
                client
                    .write_all(&[1, if ok { 0 } else { 1 }])
                    .await
                    .unwrap();
                if !ok {
                    return;
                }

                let mut request = [0u8; 4];
                client.read_exact(&mut request).await.unwrap();
                let host = match request[3] {
                    3 => {
                        client.read_exact(&mut len).await.unwrap();
                        let mut name = vec![0u8; len[0] as usize];
                        client.read_exact(&mut name).await.unwrap();
                        String::from_utf8(name).unwrap()
                    }
                    1 => {
                        let mut ip = [0u8; 4];
                        client.read_exact(&mut ip).await.unwrap();
                        std::net::Ipv4Addr::from(ip).to_string()
                    }
                    other => panic!("address type {other}"),
                };
                let mut port = [0u8; 2];
                client.read_exact(&mut port).await.unwrap();
                log.lock()
                    .unwrap()
                    .push(format!("{host}:{}", u16::from_be_bytes(port)));
                let mut target = tokio::net::TcpStream::connect(upstream).await.unwrap();
                client
                    .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut client, &mut target).await;
            });
        }
    });
    (addr, targets)
}

#[tokio::test]
async fn a_socks5h_proxy_carries_the_host_name_and_the_credentials() {
    let (web, seen) = recorder().await;
    let (socks, targets) = socks5(web).await;
    let proxy =
        tubarr::network::OutboundProxy::parse(&format!("socks5h://alice:{PASSWORD}@{socks}"))
            .unwrap();
    let client = tubarr::network::youtube_client(reqwest::Client::builder(), Some(&proxy))
        .unwrap()
        .build()
        .unwrap();
    // Only the proxy knows where `youtube.test` is. The recorder sees an ordinary request.
    let status = client
        .get("http://youtube.test/@arydigital")
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(*targets.lock().unwrap(), vec!["youtube.test:80".to_owned()]);
    assert_eq!(
        *seen.lock().unwrap(),
        vec!["/@arydigital".to_owned()],
        "{status}"
    );
}
