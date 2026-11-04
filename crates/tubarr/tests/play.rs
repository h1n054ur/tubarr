//! The play endpoint against a fake yt-dlp and a fake googlevideo with Range support, serving two
//! tiny fragmented MP4s made by ffmpeg the way YouTube lays them out (ftyp, moov, sidx, fragments),
//! joined into one flat MP4.

mod support;

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use support::mp4::*;
use tubarr::{
    play::{
        PlayConfig, PlayState,
        prebuild::{MAX_ATTEMPTS, PrebuildStats},
        router,
    },
    ratelimit::{RateLimitConfig, RateLimiter},
    resolver::{MemoryCache, Resolver, YtDlp},
    store::{Db, GrabStatus, MappingKind, NewGrab, NewMapping, NewRelease, PinTarget},
};

const ID: &str = "0eD-GQCutAU";
const VIDEO: &[u8] = include_bytes!("fixtures/play/video-137.mp4");
const AUDIO: &[u8] = include_bytes!("fixtures/play/audio-140.mp4");

/// What the fake googlevideo saw and whether it refuses a signature.
#[derive(Default)]
struct Upstream {
    requests: Mutex<Vec<String>>,
    /// A signature that is refused, and the status it is refused with.
    refused: Mutex<Option<(String, StatusCode)>>,
    /// A signature whose video file has this many bytes more at its end, as after a re-encode.
    grown: Mutex<Option<(String, usize)>>,
}

impl Upstream {
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn refuse(&self, sig: &str, status: StatusCode) {
        *self.refused.lock().unwrap() = Some((sig.to_owned(), status));
    }
}

async fn upstream_file(
    State(up): State<Arc<Upstream>>,
    Path(name): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let sig = query.get("sig").cloned().unwrap_or_default();
    let mut data = match name.as_str() {
        "v.mp4" => VIDEO.to_vec(),
        "a.mp4" => AUDIO.to_vec(),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if let Some((grown, extra)) = &*up.grown.lock().unwrap()
        && *grown == sig
        && name == "v.mp4"
    {
        data.resize(data.len() + extra, 0);
    }
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    up.requests
        .lock()
        .unwrap()
        .push(format!("{name} sig={sig} {range}"));
    if let Some((refused, status)) = &*up.refused.lock().unwrap()
        && *refused == sig
    {
        return (*status).into_response();
    }
    let Some((a, b)) = range.strip_prefix("bytes=").and_then(|r| r.split_once('-')) else {
        return (StatusCode::OK, data.to_vec()).into_response();
    };
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

fn info(upstream: SocketAddr, sig: u32) -> String {
    let expire = (SystemTime::now() + Duration::from_secs(6 * 3600))
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let url = |file: &str| format!("http://{upstream}/{file}?expire={expire}&sig={sig}");
    serde_json::json!({
        "id": ID,
        "title": "Kabhi Main Kabhi Tum Episode 1",
        "duration": 6,
        "formats": [
            {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2",
             "protocol": "https", "filesize": AUDIO.len(), "url": url("a.mp4"),
             "http_headers": {"User-Agent": "Mozilla/5.0"}},
            {"format_id": "137", "ext": "mp4", "vcodec": "avc1.640028", "acodec": "none",
             "protocol": "https", "filesize": VIDEO.len(), "url": url("v.mp4"),
             "http_headers": {"User-Agent": "Mozilla/5.0"}}
        ]
    })
    .to_string()
}

struct Harness {
    dir: tempfile::TempDir,
    state: Arc<PlayState<MemoryCache>>,
    upstream: Arc<Upstream>,
    limiter: Arc<RateLimiter>,
    /// Seconds the resolver's clock runs ahead of the real one.
    skew: Arc<AtomicU64>,
    base: String,
    client: reqwest::Client,
}

impl Harness {
    /// A fake yt-dlp that answers its n-th call with `info-<n>.json` (or the last one written).
    async fn new() -> Self {
        Self::with_index(None).await
    }

    /// The same, keeping sample tables in `index` when given.
    async fn with_index(index: Option<PathBuf>) -> Self {
        Self::serving(index, None, 0).await
    }

    /// The same, with the table queue kept in `store` and the first `fail_first` lookups failing.
    async fn serving(index: Option<PathBuf>, store: Option<Db>, fail_first: u32) -> Self {
        Self::checked(index, store, None, fail_first).await
    }

    /// The same, serving only the videos `released` holds when given.
    async fn checked(
        index: Option<PathBuf>,
        store: Option<Db>,
        released: Option<Db>,
        fail_first: u32,
    ) -> Self {
        let upstream = Arc::new(Upstream::default());
        let up_addr = spawn(
            Router::new()
                .route("/{name}", get(upstream_file))
                .with_state(upstream.clone()),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        for sig in 1..=2 {
            std::fs::write(
                dir.path().join(format!("info-{sig}.json")),
                info(up_addr, sig),
            )
            .unwrap();
        }
        let d = dir.path().display().to_string();
        Self::with_script(
            dir,
            upstream,
            &format!(
                "n=$(($(cat '{d}/count' 2>/dev/null || echo 0) + 1))\necho $n > '{d}/count'\n\
                 [ $n -le {fail_first} ] && {{ echo 'ERROR: boom' >&2; exit 1; }}\n\
                 f='{d}/info-'$n.json\n[ -f \"$f\" ] || f='{d}/info-2.json'\ncat \"$f\""
            ),
            index,
            store,
            released,
        )
        .await
    }

    /// A fake yt-dlp that fails with `stderr`.
    async fn failing(stderr: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        Self::with_script(
            dir,
            Arc::new(Upstream::default()),
            &format!("echo '{stderr}' >&2\nexit 1"),
            None,
            None,
            None,
        )
        .await
    }

    async fn with_script(
        dir: tempfile::TempDir,
        upstream: Arc<Upstream>,
        body: &str,
        index: Option<PathBuf>,
        store: Option<Db>,
        released: Option<Db>,
    ) -> Self {
        let path = dir.path().join("yt-dlp");
        support::install_executable(&path, format!("#!/bin/sh\n{body}\n").as_bytes());
        let limiter = Arc::new(RateLimiter::new(RateLimitConfig {
            interval: Duration::from_millis(1),
            burst: 100,
            max_jitter: Duration::ZERO,
        }));
        let skew = Arc::new(AtomicU64::new(0));
        let clock_skew = skew.clone();
        let resolver = Arc::new(
            Resolver::new(YtDlp::new(path), limiter.clone(), MemoryCache::new()).with_clock(
                Arc::new(move || {
                    SystemTime::now() + Duration::from_secs(clock_skew.load(Ordering::Relaxed))
                }),
            ),
        );
        let config = PlayConfig {
            prebuild_retry: Duration::from_millis(20),
            ..PlayConfig::default()
        };
        let mut state = PlayState::new(resolver, config).unwrap();
        if let Some(index) = index {
            state = state.with_index_dir(index);
        }
        if let Some(store) = store {
            state = state.with_prebuild_store(store);
        }
        if let Some(released) = released {
            state = state.with_release_check(released);
        }
        let state = Arc::new(state);
        let addr = spawn(router(state.clone())).await;
        Self {
            dir,
            state,
            upstream,
            limiter,
            skew,
            base: format!("http://{addr}/play/"),
            client: reqwest::Client::new(),
        }
    }

    /// Moves the clock `secs` ahead, as if the file had been built that long ago.
    fn advance(&self, secs: u64) {
        self.skew.fetch_add(secs, Ordering::Relaxed);
    }

    /// Rewrites every fake lookup answer with `edit`.
    fn edit_info(&self, edit: impl Fn(&mut serde_json::Value)) {
        for sig in 1..=2 {
            let path = self.dir.path().join(format!("info-{sig}.json"));
            let mut info: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            edit(&mut info);
            std::fs::write(&path, info.to_string()).unwrap();
        }
    }

    /// The one-byte reads that check URLs before a header is served.
    fn checks(&self) -> usize {
        self.upstream
            .requests()
            .iter()
            .filter(|r| r.ends_with("bytes=0-0"))
            .count()
    }

    fn ytdlp_calls(&self) -> u32 {
        std::fs::read_to_string(self.dir.path().join("count"))
            .map(|s| s.trim().parse().unwrap())
            .unwrap_or(0)
    }

    async fn get(&self, range: Option<&str>) -> reqwest::Response {
        let mut req = self.client.get(format!("{}{ID}", self.base));
        if let Some(r) = range {
            req = req.header(header::RANGE, r);
        }
        req.send().await.unwrap()
    }

    /// A `HEAD` of the whole file, which builds it.
    async fn head(&self) {
        let res = self
            .client
            .head(format!("{}{ID}", self.base))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    /// Waits until every queued background read has finished or failed.
    async fn prebuilds_done(&self) -> PrebuildStats {
        for _ in 0..1000 {
            let stats = self.state.prebuild_stats();
            if stats.finished + stats.failed == stats.queued {
                return stats;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "background reads still running: {:?}",
            self.state.prebuild_stats()
        );
    }

    /// Upstream reads other than the file heads. A table read twice shows up as a repeat.
    fn body_reads(&self) -> Vec<String> {
        self.upstream
            .requests()
            .into_iter()
            .filter(|r| !r.contains("bytes=0-"))
            .collect()
    }

    async fn full(&self) -> Vec<u8> {
        let res = self.get(None).await;
        assert_eq!(res.status(), StatusCode::OK);
        res.bytes().await.unwrap().to_vec()
    }
}

#[tokio::test]
async fn virtual_file_is_flat_with_both_tracks_up_front() {
    let h = Harness::new().await;
    let res = h.get(None).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "video/mp4");
    assert_eq!(res.headers()[header::ACCEPT_RANGES], "bytes");
    let length: usize = res.headers()[header::CONTENT_LENGTH]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    let file = res.bytes().await.unwrap().to_vec();
    assert_eq!(file.len(), length);

    // ftyp, moov, one mdat; the samples of both tracks tile the mdat.
    let [video, audio] = flat_tracks(&file);
    let ftyp = top(&file, "ftyp");
    assert_eq!(&file[ftyp.0 + 8..ftyp.0 + 12], b"isom");

    // moov: video as track 1, audio as track 2, next track id 3.
    let moov = top(&file, "moov");
    let traks = kids(&file, moov, "trak");
    assert_eq!((video.id, audio.id), (1, 2));
    let hdlr = |t| {
        let h = kid(&file, kid(&file, t, "mdia"), "hdlr");
        String::from_utf8_lossy(&file[h.0 + 16..h.0 + 20]).into_owned()
    };
    assert_eq!(
        (hdlr(traks[0]), hdlr(traks[1])),
        ("vide".into(), "soun".into())
    );
    let mvhd = kid(&file, moov, "mvhd");
    assert_eq!(be32(&file, mvhd.1 - 4), 3);

    // Every sample of the source is there, in order, with the media bytes untouched, and the
    // sample durations add up to the source's length.
    for (track, src) in [(&video, VIDEO), (&audio, AUDIO)] {
        assert_eq!(track.data.len(), source_samples(src), "track {}", track.id);
        assert_eq!(track.bytes(&file), source_media(src), "track {}", track.id);
        let (ts, ticks) = source_length(src);
        assert_eq!(track.timescale, ts);
        assert_eq!(track.ticks(), ticks, "track {}", track.id);
        assert!(!track.has_ctts, "the fixtures have no B-frames");
    }
    // Video needs an stss starting at its first key frame; AAC samples all sync.
    let sync = video.sync.as_ref().expect("video stss");
    assert_eq!(sync[0], 1);
    assert!(sync.len() < video.data.len());
    assert!(audio.sync.is_none());
    // Both tracks are interleaved: chunks of the two alternate rather than one after the other.
    assert!(audio.chunks[0] < *video.chunks.last().unwrap());
    assert!(video.chunks[0] < *audio.chunks.last().unwrap());

    // The moov states the real lengths: 6 s of video, 6.128 s of audio.
    assert_eq!(
        moov_durations(&file),
        (6128, vec![(6000, 61_440), (6128, 49_024)])
    );

    // HEAD: same headers, no body.
    let head = h
        .client
        .head(format!("{}{ID}", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert_eq!(
        head.headers()[header::CONTENT_LENGTH],
        length.to_string().as_str()
    );
    assert!(head.bytes().await.unwrap().is_empty());
    assert_eq!(h.ytdlp_calls(), 1);
}

#[tokio::test]
async fn sample_tables_are_kept_on_disk_and_rebuilt_when_deleted() {
    let index = tempfile::tempdir().unwrap();
    let dir = index.path().join("tables");
    let first = Harness::with_index(Some(dir.clone())).await;
    let reference = first.full().await;
    let fresh_reads = first.upstream.requests().len();
    let mut kept: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    kept.sort();
    // Named by video, format and where the fragments end (the fixtures close with an mfra).
    assert_eq!(
        kept,
        [
            format!("{ID}.137.{}.tbt", top(VIDEO, "mfra").0),
            format!("{ID}.140.{}.tbt", top(AUDIO, "mfra").0)
        ]
    );

    // A new process reads only the two file heads before answering, then the media.
    let second = Harness::with_index(Some(dir.clone())).await;
    let head = second.get(Some("bytes=0-99")).await;
    assert_eq!(head.status(), StatusCode::PARTIAL_CONTENT);
    head.bytes().await.unwrap();
    assert_eq!(
        second.upstream.requests().len(),
        2,
        "{:?}",
        second.upstream.requests()
    );
    assert_eq!(second.full().await, reference);
    assert!(second.upstream.requests().len() < fresh_reads + 2);

    // A damaged or deleted cache is read again from the moofs.
    std::fs::write(dir.join(&kept[0]), b"TBT1 not deflate").unwrap();
    std::fs::remove_file(dir.join(&kept[1])).unwrap();
    let third = Harness::with_index(Some(dir.clone())).await;
    assert_eq!(third.full().await, reference);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(Harness::with_index(Some(dir)).await.full().await, reference);
}

/// YouTube can offer a plain and a DRC mix of one itag at the same size (ebioxADOBOw). Their tables
/// get separate files, and a table read from another stream is never used.
#[tokio::test]
async fn sample_tables_of_other_streams_are_not_used() {
    let index = tempfile::tempdir().unwrap();
    let dir = index.path().join("tables");
    let audio_name = |format: &str| format!("{ID}.{format}.{}.tbt", top(AUDIO, "mfra").0);
    let video_name = format!("{ID}.137.{}.tbt", top(VIDEO, "mfra").0);
    let reference = Harness::with_index(Some(dir.clone())).await.full().await;
    let plain = std::fs::read(dir.join(audio_name("140"))).unwrap();

    // The same bytes under the DRC id: a file of its own, the plain one untouched.
    let drc = Harness::with_index(Some(dir.clone())).await;
    for sig in 1..=2 {
        let path = drc.dir.path().join(format!("info-{sig}.json"));
        let json = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, json.replace(r#""140""#, r#""140-drc""#)).unwrap();
    }
    assert_eq!(drc.full().await, reference);
    let mut kept: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    kept.sort();
    assert_eq!(
        kept,
        [video_name.clone(), audio_name("140-drc"), audio_name("140")]
    );
    assert_eq!(std::fs::read(dir.join(audio_name("140"))).unwrap(), plain);

    // A table read from another stream (here the video's) under the audio's name is read again
    // from the audio's moofs and replaced.
    std::fs::copy(dir.join(&video_name), dir.join(audio_name("140"))).unwrap();
    let swapped = Harness::with_index(Some(dir.clone())).await;
    assert_eq!(swapped.full().await, reference);
    assert_eq!(std::fs::read(dir.join(audio_name("140"))).unwrap(), plain);
}

#[tokio::test]
async fn ranges_match_slices_of_the_full_file() {
    let h = Harness::new().await;
    let full = h.full().await;
    let n = full.len();
    let head_len = top(&full, "mdat").0 + 8;
    // Start and end of every chunk, for ranges that begin or end on a chunk boundary.
    let [video, audio] = flat_tracks(&full);
    let chunk = audio.chunks[1] as usize;
    let cases = [
        (chunk - 1, chunk),
        (chunk - 700, chunk + 20),
        (chunk, n - 1),
        (video.chunks[1] as usize + 3, n - 2),
        (n - 16, n - 1),
        (0, 0),
        (0, 99),
        (head_len - 10, head_len + 10),
        (head_len, n - 1),
        (n / 3, n / 2),
        (n / 2 + 7, n / 2 + 8),
        (n - 1, n - 1),
    ];
    for (a, b) in cases {
        let res = h.get(Some(&format!("bytes={a}-{b}"))).await;
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            res.headers()[header::CONTENT_RANGE],
            format!("bytes {a}-{b}/{n}").as_str()
        );
        assert_eq!(
            res.headers()[header::CONTENT_LENGTH],
            (b - a + 1).to_string().as_str()
        );
        assert_eq!(res.bytes().await.unwrap(), full[a..=b], "bytes={a}-{b}");
    }

    let open = h.get(Some(&format!("bytes={}-", n - 500))).await;
    assert_eq!(open.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(open.bytes().await.unwrap(), full[n - 500..]);

    let suffix = h.get(Some("bytes=-300")).await;
    assert_eq!(
        suffix.headers()[header::CONTENT_RANGE],
        format!("bytes {}-{}/{n}", n - 300, n - 1).as_str()
    );
    assert_eq!(suffix.bytes().await.unwrap(), full[n - 300..]);

    let past = h.get(Some(&format!("bytes={n}-"))).await;
    assert_eq!(past.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        past.headers()[header::CONTENT_RANGE],
        format!("bytes */{n}").as_str()
    );

    let head = h
        .client
        .head(format!("{}{ID}", h.base))
        .header(header::RANGE, "bytes=10-19")
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(head.headers()[header::CONTENT_LENGTH], "10");
    assert!(head.bytes().await.unwrap().is_empty());
}

/// The audio-only file: an M4A with the audio track alone, its samples the source's, served from the
/// lookup and fragments the video's file already holds.
#[tokio::test]
async fn audio_only_file_has_one_track_and_shares_the_lookup() {
    let index = tempfile::tempdir().unwrap();
    let h = Harness::with_index(Some(index.path().join("tables"))).await;
    let muxed = h.full().await;
    let reads = h.upstream.requests().len();
    let url = format!("{}{ID}/audio", h.base);
    let res = h.client.get(&url).send().await.unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(res.headers()[header::CONTENT_TYPE], "audio/mp4");
    assert_eq!(res.headers()[header::ACCEPT_RANGES], "bytes");
    let file = res.bytes().await.unwrap().to_vec();

    let ftyp = top(&file, "ftyp");
    assert_eq!(&file[ftyp.0 + 8..ftyp.0 + 12], b"M4A ");
    let tracks = flat_samples(&file);
    let [audio] = tracks.as_slice() else {
        panic!("one track, got {}", tracks.len());
    };
    let moov = top(&file, "moov");
    let trak = kids(&file, moov, "trak")[0];
    let hdlr = kid(&file, kid(&file, trak, "mdia"), "hdlr");
    assert_eq!(&file[hdlr.0 + 16..hdlr.0 + 20], b"soun");
    assert_eq!(audio.id, 1);
    assert_eq!(be32(&file, kid(&file, moov, "mvhd").1 - 4), 2);
    assert_eq!(audio.data.len(), source_samples(AUDIO));
    assert_eq!(audio.bytes(&file), source_media(AUDIO));
    assert_eq!((audio.timescale, audio.ticks()), source_length(AUDIO));
    // 6.128 s, in the audio source's movie timescale (its sample rate) as well as in the track's.
    assert_eq!(moov_durations(&file), (49_024, vec![(49_024, 49_024)]));
    assert!(file.len() < muxed.len());
    // No second lookup and nothing read again but the two file heads: the tables come from disk
    // and every audio fragment from the cache.
    assert_eq!(h.ytdlp_calls(), 1);
    let fresh: Vec<String> = h.upstream.requests()[reads..]
        .iter()
        .filter(|r| !r.contains("bytes=0-"))
        .cloned()
        .collect();
    assert!(fresh.is_empty(), "{fresh:?}");

    // Ranges are slices of the whole file, the muxed file is unchanged.
    let n = file.len();
    let mdat = top(&file, "mdat").0;
    for (a, b) in [
        (0, 0),
        (0, 99),
        (mdat - 5, mdat + 20),
        (n / 2, n - 1),
        (n - 1, n - 1),
    ] {
        let res = h
            .client
            .get(&url)
            .header(header::RANGE, format!("bytes={a}-{b}"))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            res.headers()[header::CONTENT_RANGE],
            format!("bytes {a}-{b}/{n}").as_str()
        );
        assert_eq!(res.bytes().await.unwrap(), file[a..=b], "bytes={a}-{b}");
    }
    let past = h
        .client
        .get(&url)
        .header(header::RANGE, format!("bytes={n}-"))
        .send()
        .await
        .unwrap();
    assert_eq!(past.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(h.full().await, muxed);
}

#[tokio::test]
async fn repeated_ranges_come_from_the_fragment_cache() {
    let h = Harness::new().await;
    let n = h.full().await.len();
    let before = h.upstream.requests().len();
    for _ in 0..3 {
        let res = h.get(Some(&format!("bytes={}-{}", n / 4, n - 1))).await;
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        res.bytes().await.unwrap();
    }
    assert_eq!(
        h.upstream.requests().len(),
        before,
        "{:?}",
        h.upstream.requests()
    );
    assert_eq!(h.ytdlp_calls(), 1);
}

#[tokio::test]
async fn expired_urls_are_looked_up_again_mid_stream() {
    let reference = Harness::new().await.full().await;

    // 403 for an expired or moved URL, 401 and 410 for a file YouTube has replaced.
    for status in [
        StatusCode::FORBIDDEN,
        StatusCode::UNAUTHORIZED,
        StatusCode::GONE,
    ] {
        let h = Harness::new().await;
        h.head().await;
        h.upstream.refuse("1", status);

        assert_eq!(h.full().await, reference, "{status}");
        assert_eq!(h.ytdlp_calls(), 2, "{status}");
        let requests = h.upstream.requests();
        assert!(
            requests
                .iter()
                .any(|r| r.contains("sig=1") && r.contains("bytes=") && !r.contains("bytes=0-"))
        );
        assert!(requests.iter().any(|r| r.contains("sig=2")), "{requests:?}");
    }
}

#[tokio::test]
async fn urls_refused_before_the_build_are_looked_up_again() {
    let reference = Harness::new().await.full().await;

    // The cached lookup is already dead when the header is read, as after a re-encode.
    let h = Harness::new().await;
    h.upstream.refuse("1", StatusCode::UNAUTHORIZED);
    assert_eq!(h.full().await, reference);
    assert_eq!(h.ytdlp_calls(), 2);
}

#[tokio::test]
async fn parallel_ranges_share_one_fresh_lookup() {
    let reference = Harness::new().await.full().await;
    let n = reference.len();
    let ranges: Vec<(usize, usize)> = (0..6).map(|k| (k * n / 6, (k + 1) * n / 6 - 1)).collect();
    async fn fetch_all(h: &Harness, ranges: &[(usize, usize)], reference: &[u8]) {
        let gets = ranges.iter().map(|&(a, b)| async move {
            let res = h.get(Some(&format!("bytes={a}-{b}"))).await;
            assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
            assert_eq!(res.bytes().await.unwrap()[..], reference[a..=b]);
        });
        futures_util::future::join_all(gets).await;
    }

    // Refused mid-play, after the file was built.
    let h = Harness::new().await;
    h.head().await;
    h.upstream.refuse("1", StatusCode::UNAUTHORIZED);
    fetch_all(&h, &ranges, &reference).await;
    assert_eq!(h.ytdlp_calls(), 2);

    // Refused while the first requests build the file.
    let h = Harness::new().await;
    h.upstream.refuse("1", StatusCode::UNAUTHORIZED);
    fetch_all(&h, &ranges, &reference).await;
    assert_eq!(h.ytdlp_calls(), 2);
}

#[tokio::test]
async fn expired_urls_on_both_lookups_fail_the_stream() {
    let h = Harness::new().await;
    h.head().await;
    h.upstream.refuse("1", StatusCode::UNAUTHORIZED);
    // The fresh lookup hands out the same refused signature, so the body is cut short.
    std::fs::copy(
        h.dir.path().join("info-1.json"),
        h.dir.path().join("info-2.json"),
    )
    .unwrap();
    let res = h.get(None).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.bytes().await.is_err());
    assert_eq!(h.ytdlp_calls(), 2);

    // Before the build, the request fails as a whole, still after a single fresh lookup.
    let h = Harness::new().await;
    h.upstream.refuse("1", StatusCode::UNAUTHORIZED);
    std::fs::copy(
        h.dir.path().join("info-1.json"),
        h.dir.path().join("info-2.json"),
    )
    .unwrap();
    assert_eq!(h.get(None).await.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(h.ytdlp_calls(), 2);
}

#[tokio::test]
async fn a_re_encode_mid_play_cuts_the_stream_and_the_next_request_gets_the_new_file() {
    let reference = Harness::new().await.full().await;

    let h = Harness::new().await;
    h.head().await;
    h.upstream.refuse("1", StatusCode::UNAUTHORIZED);
    // The fresh lookup reports another size for the video file, so the layout the player holds is
    // no longer the one behind the URLs.
    let path = h.dir.path().join("info-2.json");
    let mut info: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    info["formats"][1]["filesize"] = (VIDEO.len() + 1).into();
    std::fs::write(&path, info.to_string()).unwrap();

    let res = h.get(None).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.bytes().await.is_err());
    assert_eq!(h.full().await, reference);
    assert_eq!(h.ytdlp_calls(), 2);
}

/// Two hours after the build the URLs in memory are dead. A new play that starts at the header has
/// them looked up again before it is answered, so its media reads are not cut.
#[tokio::test]
async fn dead_urls_in_memory_are_refreshed_before_the_header_is_served() {
    let reference = Harness::new().await.full().await;
    let h = Harness::new().await;
    h.head().await;
    h.advance(2 * 3600);
    h.upstream.refuse("1", StatusCode::FORBIDDEN);
    let before = h.upstream.requests().len();

    let res = h.get(Some("bytes=0-")).await;
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    // Looked up again before the answer started.
    assert_eq!(h.ytdlp_calls(), 2);
    assert_eq!(res.bytes().await.unwrap(), reference);
    assert_eq!(h.ytdlp_calls(), 2);
    // The refused signature answered only the check, never a media read.
    let reads = &h.upstream.requests()[before..];
    assert!(reads[0].ends_with("bytes=0-0"), "{reads:?}");
    assert!(
        reads
            .iter()
            .filter(|r| r.contains("sig=1"))
            .all(|r| r.ends_with("bytes=0-0")),
        "{reads:?}"
    );
    // The fresh URLs were just seen working: the next header read checks nothing.
    let checks = h.checks();
    assert_eq!(h.full().await, reference);
    assert_eq!(h.checks(), checks);
    assert_eq!(h.ytdlp_calls(), 2);
}

/// A seek into the mdat of a running play never pays for the check, however old the lookup.
#[tokio::test]
async fn a_seek_into_the_mdat_is_not_checked() {
    let h = Harness::new().await;
    let full = h.full().await;
    let head_len = top(&full, "mdat").0 + 8;
    h.advance(3 * 3600);
    let n = full.len();
    let res = h
        .get(Some(&format!("bytes={}-{}", head_len + 10, n - 1)))
        .await;
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(res.bytes().await.unwrap(), full[head_len + 10..]);
    assert_eq!(h.checks(), 0);

    // The header of the same old lookup is checked once, with live URLs no lookup is needed.
    for _ in 0..2 {
        let res = h.get(Some("bytes=0-99")).await;
        assert_eq!(res.bytes().await.unwrap(), full[..100]);
    }
    assert_eq!(h.checks(), 2);
    assert_eq!(h.ytdlp_calls(), 1);
}

/// yt-dlp gives no filesize, so a re-encode is told apart by the size googlevideo reports.
#[tokio::test]
async fn a_re_encode_without_filesize_is_found_by_the_upstream_size() {
    let reference = Harness::new().await.full().await;
    let no_sizes = |info: &mut serde_json::Value| {
        for f in info["formats"].as_array_mut().unwrap() {
            f.as_object_mut().unwrap().remove("filesize");
        }
    };

    // Mid-play: the stream is cut, and the next request builds the file from the new source.
    let h = Harness::new().await;
    h.edit_info(no_sizes);
    h.head().await;
    h.upstream.refuse("1", StatusCode::UNAUTHORIZED);
    *h.upstream.grown.lock().unwrap() = Some(("2".to_owned(), 100));
    let res = h.get(None).await;
    assert_eq!(res.status(), StatusCode::OK);
    assert!(res.bytes().await.is_err());
    let built = h.upstream.requests().len();
    assert_eq!(h.full().await, reference);
    // Built again: the new source's head was read.
    assert!(
        h.upstream.requests()[built..]
            .iter()
            .any(|r| r.contains("v.mp4 sig=2 bytes=0-6")),
        "{:?}",
        h.upstream.requests()
    );
    assert_eq!(h.ytdlp_calls(), 2);

    // Before the header of an old lookup: found by the check, the file is built again at once.
    let h = Harness::new().await;
    h.edit_info(no_sizes);
    h.head().await;
    h.advance(2 * 3600);
    h.upstream.refuse("1", StatusCode::UNAUTHORIZED);
    *h.upstream.grown.lock().unwrap() = Some(("2".to_owned(), 100));
    let built = h.upstream.requests().len();
    assert_eq!(h.full().await, reference);
    assert!(
        h.upstream.requests()[built..]
            .iter()
            .any(|r| r.contains("v.mp4 sig=2 bytes=0-6")),
        "{:?}",
        h.upstream.requests()
    );
    assert_eq!(h.ytdlp_calls(), 2);
}

#[tokio::test]
async fn lookup_errors_map_to_status_codes() {
    let cases = [
        (
            "ERROR: [youtube] x: Video unavailable",
            StatusCode::NOT_FOUND,
        ),
        (
            "ERROR: [youtube] x: The uploader has not made this video available in your country",
            StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
        ),
        (
            "ERROR: unable to download: HTTP Error 429: Too Many Requests",
            StatusCode::SERVICE_UNAVAILABLE,
        ),
        ("ERROR: something odd", StatusCode::BAD_GATEWAY),
    ];
    for (stderr, status) in cases {
        let h = Harness::failing(stderr).await;
        let res = h.get(None).await;
        assert_eq!(res.status(), status, "{stderr}");
        if status == StatusCode::SERVICE_UNAVAILABLE {
            let wait: u64 = res.headers()[header::RETRY_AFTER]
                .to_str()
                .unwrap()
                .parse()
                .unwrap();
            assert!(wait >= 30);
        }
    }
    let h = Harness::failing("unused").await;
    let res = h
        .client
        .get(format!("{}not-an-id", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_play_during_a_throttle_pause_answers_503_with_retry_after() {
    let h = Harness::new().await;
    h.limiter.penalize(Duration::from_secs(900));
    let res = h.get(None).await;
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let wait: u64 = res.headers()[header::RETRY_AFTER]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(wait > 890, "{wait}");
    assert_eq!(h.ytdlp_calls(), 0);
}

/// Needs ffprobe and ffmpeg on the PATH: `cargo test --test play -- --ignored`.
#[tokio::test]
#[ignore]
async fn ffprobe_reads_the_duration_and_seeks_decode() {
    let h = Harness::new().await;
    let url = format!("{}{ID}", h.base);
    let duration = probe_duration(&url, &["h264", "aac"]).await;
    assert!((duration - 6.128).abs() < 0.05, "{duration}");
    // From the start, and after seeks to 60 % and near the end.
    for seek in ["0", "3.7", "5.5"] {
        decode_from(&url, seek).await;
    }
}

/// Needs ffprobe and ffmpeg on the PATH: `cargo test --test play -- --ignored`.
#[tokio::test]
#[ignore]
async fn ffprobe_reads_the_audio_only_file() {
    let h = Harness::new().await;
    let url = format!("{}{ID}/audio", h.base);
    let duration = probe_duration(&url, &["aac"]).await;
    assert!((duration - 6.128).abs() < 0.05, "{duration}");
    for seek in ["0", "3.7", "5.5"] {
        decode_from(&url, seek).await;
    }
}

fn table_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

fn assert_no_repeats(reads: &[String]) {
    let unique: std::collections::HashSet<_> = reads.iter().collect();
    assert_eq!(unique.len(), reads.len(), "{reads:?}");
}

/// A grab queues its video: the tables are read once in the background, a second grab of the same
/// video adds nothing, and the play that follows reads only the two file heads before its media.
#[tokio::test]
async fn a_grab_reads_the_tables_in_the_background_once() {
    let index = tempfile::tempdir().unwrap();
    let dir = index.path().join("tables");
    let h = Harness::with_index(Some(dir.clone())).await;
    h.state.prebuild(ID).await;
    h.state.prebuild(ID).await;
    let stats = h.prebuilds_done().await;
    assert_eq!(
        stats,
        PrebuildStats {
            queued: 1,
            finished: 1,
            tables_read: 2,
            ..PrebuildStats::default()
        }
    );
    assert_eq!(table_names(&dir).len(), 2);
    let table_reads = h.body_reads();
    assert!(!table_reads.is_empty());

    let before = h.upstream.requests().len();
    let head = h.get(Some("bytes=0-99")).await;
    assert_eq!(head.status(), StatusCode::PARTIAL_CONTENT);
    head.bytes().await.unwrap();
    assert_eq!(h.upstream.requests().len(), before + 2);
    let file = h.full().await;
    let [video, audio] = flat_tracks(&file);
    assert_eq!(video.bytes(&file), source_media(VIDEO));
    assert_eq!(audio.bytes(&file), source_media(AUDIO));
    assert_no_repeats(&h.body_reads());
    assert_eq!(h.ytdlp_calls(), 1);

    // Done and played: queuing it again reads nothing.
    let before = h.upstream.requests().len();
    h.state.prebuild(ID).await;
    let stats = h.prebuilds_done().await;
    assert_eq!((stats.queued, stats.tables_read), (2, 2));
    assert_eq!(h.upstream.requests().len(), before);
}

/// Tables already on disk from an earlier run cost only the two file heads that prove them valid.
#[tokio::test]
async fn a_cached_video_is_not_read_again() {
    let index = tempfile::tempdir().unwrap();
    let dir = index.path().join("tables");
    Harness::with_index(Some(dir.clone())).await.full().await;
    let h = Harness::with_index(Some(dir.clone())).await;
    h.state.prebuild(ID).await;
    let stats = h.prebuilds_done().await;
    assert_eq!((stats.finished, stats.tables_read), (1, 0));
    assert_eq!(
        h.upstream.requests().len(),
        2,
        "{:?}",
        h.upstream.requests()
    );
}

/// A play that comes in while the background read runs is served from that same read: every table
/// is read from YouTube once.
#[tokio::test]
async fn a_play_during_a_background_read_shares_it() {
    let index = tempfile::tempdir().unwrap();
    let reference = Harness::with_index(Some(index.path().join("reference")))
        .await
        .full()
        .await;
    let dir = index.path().join("tables");
    let h = Harness::with_index(Some(dir.clone())).await;
    h.state.prebuild(ID).await;
    assert_eq!(h.full().await, reference);
    let stats = h.prebuilds_done().await;
    assert_eq!(stats.finished, 1);
    assert_no_repeats(&h.body_reads());
    assert_eq!(table_names(&dir).len(), 2);
}

/// Without a folder to keep tables in there is nothing to read ahead.
#[tokio::test]
async fn no_background_reads_without_an_index_dir() {
    let h = Harness::new().await;
    h.state.prebuild(ID).await;
    assert_eq!(h.state.prebuild_stats(), PrebuildStats::default());
    assert!(h.upstream.requests().is_empty());
}

async fn stored_queue(db: &Db) -> Vec<(String, i64)> {
    db.queued_prebuilds()
        .await
        .unwrap()
        .into_iter()
        .map(|q| (q.video_id, q.attempts))
        .collect()
}

/// A grab stores its video before the read. When Tubarr stops before the read is done, the next
/// start reads it and takes it off the stored queue.
#[tokio::test]
async fn the_table_queue_survives_a_restart() {
    let index = tempfile::tempdir().unwrap();
    let dir = index.path().join("tables");
    let db = Db::open(&index.path().join("tubarr.db")).await.unwrap();
    // A lookup that never finishes in time, like one cut short by an out of memory kill.
    let stuck = Harness::with_script(
        tempfile::tempdir().unwrap(),
        Arc::new(Upstream::default()),
        "sleep 30",
        Some(dir.clone()),
        Some(db.clone()),
        None,
    )
    .await;
    stuck.state.prebuild(ID).await;
    assert_eq!(stored_queue(&db).await, [(ID.to_owned(), 0)]);
    drop(stuck);

    let h = Harness::serving(Some(dir.clone()), Some(db.clone()), 0).await;
    h.state.resume_prebuilds().await;
    let stats = h.prebuilds_done().await;
    assert_eq!(
        stats,
        PrebuildStats {
            queued: 1,
            finished: 1,
            tables_read: 2,
            ..PrebuildStats::default()
        }
    );
    assert_eq!(table_names(&dir).len(), 2);
    assert!(stored_queue(&db).await.is_empty());
}

/// A failed read (here two failed lookups) is tried again later and the third try reads the
/// tables.
#[tokio::test]
async fn a_failed_background_read_is_tried_again() {
    let index = tempfile::tempdir().unwrap();
    let dir = index.path().join("tables");
    let db = Db::open(&index.path().join("tubarr.db")).await.unwrap();
    let h = Harness::serving(Some(dir.clone()), Some(db.clone()), 2).await;
    h.state.prebuild(ID).await;
    let stats = h.prebuilds_done().await;
    assert_eq!(
        stats,
        PrebuildStats {
            queued: 1,
            retried: 2,
            finished: 1,
            tables_read: 2,
            ..PrebuildStats::default()
        }
    );
    assert_eq!(h.ytdlp_calls(), 3);
    assert_eq!(table_names(&dir).len(), 2);
    assert!(stored_queue(&db).await.is_empty());
}

/// A video whose reads keep failing is given up on after the last try and leaves the stored
/// queue. Tries made before a restart count.
#[tokio::test]
async fn a_background_read_is_given_up_after_the_last_try() {
    let index = tempfile::tempdir().unwrap();
    let dir = index.path().join("tables");
    let db = Db::open(&index.path().join("tubarr.db")).await.unwrap();
    let h = Harness::serving(Some(dir.clone()), Some(db.clone()), u32::MAX).await;
    h.state.prebuild(ID).await;
    let stats = h.prebuilds_done().await;
    assert_eq!(
        (stats.retried, stats.failed, stats.finished),
        (u64::from(MAX_ATTEMPTS) - 1, 1, 0)
    );
    assert_eq!(h.ytdlp_calls(), MAX_ATTEMPTS);
    assert!(stored_queue(&db).await.is_empty());

    db.queue_prebuild(ID).await.unwrap();
    for _ in 1..MAX_ATTEMPTS {
        db.prebuild_failed(ID, "YouTube refused the stream")
            .await
            .unwrap();
    }
    let h = Harness::serving(Some(dir.clone()), Some(db.clone()), u32::MAX).await;
    h.state.resume_prebuilds().await;
    let stats = h.prebuilds_done().await;
    assert_eq!((stats.retried, stats.failed), (0, 1));
    assert_eq!(h.ytdlp_calls(), 1);
    assert!(stored_queue(&db).await.is_empty());
}

const OTHER: &str = "ldmPWRb-XcM";

async fn fresh_db() -> (Db, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("tubarr.db")).await.unwrap();
    (db, dir)
}

async fn mapping(db: &Db, kind: MappingKind) -> i64 {
    let (tvdb_id, tmdb_id) = match kind {
        MappingKind::Film => (None, Some(352_173)),
        _ => (Some(452_001), None),
    };
    db.insert_mapping(&NewMapping {
        kind,
        tvdb_id,
        tmdb_id,
        title: "Kabhi Main Kabhi Tum".to_owned(),
        language: "Urdu".to_owned(),
        channel_ids: vec![],
        episode_regex: None,
        excludes: vec![],
        min_duration_secs: None,
    })
    .await
    .unwrap()
    .id
}

async fn release(db: &Db, mapping_id: i64, parts: &[&str], target: PinTarget) -> i64 {
    db.insert_release(&NewRelease {
        mapping_id,
        video_id: parts[0].to_owned(),
        part_ids: if parts.len() > 1 {
            parts.iter().map(|p| (*p).to_owned()).collect()
        } else {
            vec![]
        },
        target,
        year: None,
        imdb_id: None,
        video_title: "t".to_owned(),
        language: "Urdu".to_owned(),
        height: 1080,
        size: None,
        published_at: 0,
    })
    .await
    .unwrap()
    .id
}

async fn play_status(h: &Harness, path: &str) -> StatusCode {
    let res = h
        .client
        .get(format!("{}{path}", h.base))
        .header(header::RANGE, "bytes=0-99")
        .send()
        .await
        .unwrap();
    let status = res.status();
    res.bytes().await.unwrap();
    status
}

/// A video Tubarr never released, pinned or grabbed gets a 404 before any lookup or table file,
/// alone, as one part of a joined path or as audio.
#[tokio::test]
async fn an_unreleased_video_is_refused_without_a_lookup() {
    let (db, _db_dir) = fresh_db().await;
    let show = mapping(&db, MappingKind::Show).await;
    release(
        &db,
        show,
        &[ID],
        PinTarget::Episode {
            season: 1,
            episode: 1,
        },
    )
    .await;
    let index = tempfile::tempdir().unwrap();
    let tables = index.path().join("tables");
    let h = Harness::checked(Some(tables.clone()), None, Some(db), 0).await;
    for path in [
        OTHER.to_owned(),
        format!("{OTHER}/audio"),
        format!("{ID},{OTHER}"),
    ] {
        assert_eq!(
            play_status(&h, &path).await,
            StatusCode::NOT_FOUND,
            "{path}"
        );
    }
    assert_eq!(h.ytdlp_calls(), 0);
    assert!(h.upstream.requests().is_empty());
    assert!(!tables.exists());
}

/// Released episodes and films, every part of a joined release, pinned videos and grabbed ones
/// play, the audio version too. A release whose grab Sonarr cleared from history, and one the
/// matcher has since replaced, still play: their `.strm` files are still in the library.
#[tokio::test]
async fn released_pinned_and_grabbed_videos_play() {
    for case in ["episode", "film", "part", "pin", "grab", "replaced"] {
        let (db, _db_dir) = fresh_db().await;
        let show = mapping(&db, MappingKind::Show).await;
        let episode = PinTarget::Episode {
            season: 1,
            episode: 1,
        };
        match case {
            "episode" => {
                release(&db, show, &[ID], episode).await;
            }
            "film" => {
                let film = mapping(&db, MappingKind::Film).await;
                release(&db, film, &[ID], PinTarget::Film).await;
            }
            "part" => {
                release(&db, show, &[OTHER, ID], episode).await;
            }
            "pin" => {
                db.set_pin(show, &episode, &format!("{OTHER},{ID}"))
                    .await
                    .unwrap();
            }
            "grab" | "replaced" => {
                let id = release(&db, show, &[ID], episode).await;
                db.insert_grab(&NewGrab {
                    nzo_id: "SABnzbd_nzo_x".to_owned(),
                    release_name: "r".to_owned(),
                    video_id: ID.to_owned(),
                    category: "tv".to_owned(),
                    status: GrabStatus::Completed,
                    storage_path: "/downloads/tv/r".to_owned(),
                    fail_message: None,
                })
                .await
                .unwrap();
                assert!(db.delete_grab("SABnzbd_nzo_x").await.unwrap());
                if case == "replaced" {
                    assert!(db.delete_release(id).await.unwrap());
                    assert!(db.delete_mapping(show).await.unwrap());
                }
            }
            _ => unreachable!(),
        }
        let h = Harness::checked(None, None, Some(db), 0).await;
        assert_eq!(
            play_status(&h, ID).await,
            StatusCode::PARTIAL_CONTENT,
            "{case}"
        );
        assert_eq!(
            play_status(&h, &format!("{ID}/audio")).await,
            StatusCode::PARTIAL_CONTENT,
            "{case} audio"
        );
    }
}

/// Upgrading: tables already on disk from earlier plays count as released, so a `.strm` whose
/// release and grab rows went before this version still plays.
#[tokio::test]
async fn videos_with_tables_on_disk_count_as_released() {
    let index = tempfile::tempdir().unwrap();
    let tables = index.path().join("tables");
    Harness::with_index(Some(tables.clone())).await.full().await;
    let (db, _db_dir) = fresh_db().await;
    let h = Harness::checked(Some(tables), None, Some(db.clone()), 0).await;
    assert_eq!(play_status(&h, ID).await, StatusCode::NOT_FOUND);
    h.state.remember_indexed_videos().await;
    assert_eq!(play_status(&h, ID).await, StatusCode::PARTIAL_CONTENT);
    assert!(!db.all_released(&[OTHER.to_owned()]).await.unwrap());
}
