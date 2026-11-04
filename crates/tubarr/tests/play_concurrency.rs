//! First plays of different videos build side by side, plays of one video share its build, and
//! `[play] max_builds` and `max_streams` hold. The fake googlevideo can hold back every request for
//! a video until the test lets it go, so the tests wait on what the server saw, never on time.

mod support;

use std::{
    collections::HashSet,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use tokio::sync::watch;
use tubarr::{
    play::{PlayConfig, PlayLoad, PlayState, router},
    ratelimit::{RateLimitConfig, RateLimiter},
    resolver::{MemoryCache, Resolver, YtDlp},
};

const ID1: &str = "0eD-GQCutAU";
const ID2: &str = "ldmPWRb-XcM";
const VIDEO: &[u8] = include_bytes!("fixtures/play/video-137.mp4");
const AUDIO: &[u8] = include_bytes!("fixtures/play/audio-140.mp4");
/// Longest any step may take before the test counts it as stuck behind another build.
const STUCK: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Upstream {
    /// Every request seen, as `<file> <range>`, including the held ones.
    seen: Mutex<Vec<String>>,
    /// Bumped on every request, so a test can wait for one.
    arrived: watch::Sender<usize>,
    /// Videos whose requests wait until they leave the set.
    held: watch::Sender<HashSet<String>>,
}

impl Upstream {
    fn hold(&self, id: &str) {
        self.held.send_modify(|h| {
            h.insert(id.to_owned());
        });
    }

    fn release(&self, id: &str) {
        self.held.send_modify(|h| {
            h.remove(id);
        });
    }

    fn seen(&self, prefix: &str) -> Vec<String> {
        let seen = self.seen.lock().unwrap();
        seen.iter()
            .filter(|r| r.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// Waits until a request for a file starting with `prefix` has come in.
    async fn wait_for(&self, prefix: &str) {
        let mut rx = self.arrived.subscribe();
        tokio::time::timeout(STUCK, rx.wait_for(|_| !self.seen(prefix).is_empty()))
            .await
            .expect("no request came in")
            .unwrap();
    }
}

async fn upstream_file(
    State(up): State<Arc<Upstream>>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    // Ids may hold a dash of their own: the kind is what follows the last one.
    let (id, kind) = name.rsplit_once('-').unwrap_or((name.as_str(), ""));
    let data = match kind {
        "v.mp4" => VIDEO,
        "a.mp4" => AUDIO,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    up.seen.lock().unwrap().push(format!("{name} {range}"));
    up.arrived.send_modify(|n| *n += 1);
    let mut held = up.held.subscribe();
    let _ = held.wait_for(|h| !h.contains(id)).await;
    let (a, b) = range
        .strip_prefix("bytes=")
        .and_then(|r| r.split_once('-'))
        .unwrap();
    let start: usize = a.parse().unwrap();
    let end = b
        .parse::<usize>()
        .map_or(data.len() - 1, |e| e.min(data.len() - 1));
    (
        StatusCode::PARTIAL_CONTENT,
        [(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", data.len()),
        )],
        data[start..=end].to_vec(),
    )
        .into_response()
}

async fn spawn(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn info(upstream: SocketAddr, id: &str) -> String {
    let expire = (SystemTime::now() + Duration::from_secs(6 * 3600))
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let url = |kind: &str| format!("http://{upstream}/{id}-{kind}.mp4?expire={expire}&sig=1");
    serde_json::json!({
        "id": id,
        "title": "Kabhi Main Kabhi Tum Episode 1",
        "duration": 6,
        "formats": [
            {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2",
             "protocol": "https", "filesize": AUDIO.len(), "url": url("a")},
            {"format_id": "137", "ext": "mp4", "vcodec": "avc1.640028", "acodec": "none",
             "protocol": "https", "filesize": VIDEO.len(), "url": url("v")}
        ]
    })
    .to_string()
}

struct Harness {
    _dir: tempfile::TempDir,
    state: Arc<PlayState<MemoryCache>>,
    upstream: Arc<Upstream>,
    base: String,
    client: reqwest::Client,
}

impl Harness {
    async fn new(max_builds: usize, max_streams: usize) -> Self {
        let upstream = Arc::new(Upstream::default());
        let up_addr = spawn(
            Router::new()
                .route("/{name}", get(upstream_file))
                .with_state(upstream.clone()),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        for id in [ID1, ID2] {
            std::fs::write(
                dir.path().join(format!("info-{id}.json")),
                info(up_addr, id),
            )
            .unwrap();
        }
        // The video id is the end of the watch URL, the last argument.
        let d = dir.path().display().to_string();
        let script =
            format!("#!/bin/sh\nfor last; do :; done\nid=${{last##*=}}\ncat '{d}/info-'$id.json\n");
        let path = dir.path().join("yt-dlp");
        support::install_executable(&path, script.as_bytes());
        let limiter = Arc::new(RateLimiter::new(RateLimitConfig {
            interval: Duration::from_millis(1),
            burst: 100,
            max_jitter: Duration::ZERO,
        }));
        let resolver = Arc::new(Resolver::new(YtDlp::new(path), limiter, MemoryCache::new()));
        let config = PlayConfig {
            max_builds,
            max_streams,
            ..PlayConfig::default()
        };
        let state = Arc::new(PlayState::new(resolver, config, None).unwrap());
        let addr = spawn(router(state.clone())).await;
        Self {
            _dir: dir,
            state,
            upstream,
            base: format!("http://{addr}/play/"),
            client: reqwest::Client::new(),
        }
    }

    /// A `HEAD` of `id` in the background: it builds the file and streams nothing.
    fn head(&self, id: &str) -> tokio::task::JoinHandle<StatusCode> {
        let req = self.client.head(format!("{}{id}", self.base));
        tokio::spawn(async move { req.send().await.unwrap().status() })
    }

    async fn get(&self, id: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{id}", self.base))
            .send()
            .await
            .unwrap()
    }

    /// Header reads of `id`'s video file: one per build.
    fn builds_of(&self, id: &str) -> usize {
        self.upstream
            .seen(&format!("{id}-v.mp4 bytes=0-"))
            .iter()
            .filter(|r| !r.ends_with("bytes=0-0"))
            .count()
    }

    /// Waits until the load the play endpoint reports passes `done`.
    async fn load_until(&self, done: impl Fn(&PlayLoad) -> bool) -> PlayLoad {
        tokio::time::timeout(STUCK, async {
            loop {
                let load = self.state.load();
                if done(&load) {
                    return load;
                }
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the play load never got there")
    }
}

async fn finished(task: tokio::task::JoinHandle<StatusCode>) -> StatusCode {
    tokio::time::timeout(STUCK, task)
        .await
        .expect("the play is stuck")
        .unwrap()
}

#[tokio::test]
async fn first_plays_of_different_videos_build_side_by_side() {
    let h = Harness::new(2, 8).await;
    h.upstream.hold(ID1);
    let first = h.head(ID1);
    h.upstream.wait_for(ID1).await;
    assert_eq!(h.state.load().builds, 1);

    // ID1's build cannot go on until it is released, so ID2 only finishes if it does not wait.
    assert_eq!(finished(h.head(ID2)).await, StatusCode::OK);
    assert!(
        !first.is_finished(),
        "ID1 finished while its reads were held"
    );
    assert_eq!(h.state.load().builds, 1);

    h.upstream.release(ID1);
    assert_eq!(finished(first).await, StatusCode::OK);
    assert_eq!(h.state.load().builds, 0);
    assert_eq!((h.builds_of(ID1), h.builds_of(ID2)), (1, 1));
}

#[tokio::test]
async fn plays_of_one_video_share_its_build() {
    let h = Harness::new(4, 8).await;
    h.upstream.hold(ID1);
    let plays: Vec<_> = (0..3).map(|_| h.head(ID1)).collect();
    h.upstream.wait_for(ID1).await;
    h.upstream.release(ID1);
    for play in plays {
        assert_eq!(finished(play).await, StatusCode::OK);
    }
    assert_eq!(h.builds_of(ID1), 1);
}

#[tokio::test]
async fn builds_past_max_builds_wait_for_a_turn() {
    let h = Harness::new(1, 8).await;
    h.upstream.hold(ID1);
    let first = h.head(ID1);
    h.upstream.wait_for(ID1).await;
    let second = h.head(ID2);
    let load = h.load_until(|l| l.builds_waiting == 1).await;
    assert_eq!((load.builds, load.max_builds), (1, 1));
    assert!(h.upstream.seen(ID2).is_empty(), "ID2 read before its turn");

    h.upstream.release(ID1);
    assert_eq!(finished(first).await, StatusCode::OK);
    assert_eq!(finished(second).await, StatusCode::OK);
    assert_eq!(
        h.state.load(),
        PlayLoad {
            max_builds: 1,
            max_streams: 8,
            ..PlayLoad::default()
        }
    );
}

#[tokio::test]
async fn streams_past_max_streams_answer_503_with_retry_after() {
    let h = Harness::new(2, 1).await;
    assert_eq!(finished(h.head(ID1)).await, StatusCode::OK);

    // The header comes from memory; the sample data waits on the held reads, so the body stays open.
    h.upstream.hold(ID1);
    let open = h.get(ID1).await;
    assert_eq!(open.status(), StatusCode::OK);
    assert_eq!(h.state.load().streams, 1);

    let refused = h.get(ID2).await;
    assert_eq!(refused.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(refused.headers()[header::RETRY_AFTER], "5");
    assert!(
        h.upstream.seen(ID2).is_empty(),
        "a refused play looked its video up"
    );
    // A HEAD streams nothing, so it still answers.
    assert_eq!(finished(h.head(ID1)).await, StatusCode::OK);

    h.upstream.release(ID1);
    let body = tokio::time::timeout(STUCK, open.bytes())
        .await
        .unwrap()
        .unwrap();
    assert!(!body.is_empty());
    h.load_until(|l| l.streams == 0).await;
    assert_eq!(h.get(ID2).await.status(), StatusCode::OK);
}
