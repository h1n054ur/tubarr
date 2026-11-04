//! Multi-part episodes on the play endpoint: `/play/<part1>,<part2>` against a fake yt-dlp that
//! answers per video id and a fake googlevideo. Part 2 is a shorter remux of the part 1 fixtures,
//! so it decodes the same way while its bitrate boxes and compressor name differ, as real uploads do.

mod support;

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
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
    play::{PlayConfig, PlayState, router},
    ratelimit::{RateLimitConfig, RateLimiter},
    resolver::{MemoryCache, Resolver, YtDlp},
};

const ID1: &str = "0eD-GQCutAU";
const ID2: &str = "ldmPWRb-XcM";
/// Its video claims another picture size than part 1.
const ODD: &str = "oddWidth_01";
const VIDEO1: &[u8] = include_bytes!("fixtures/play/video-137.mp4");
const AUDIO1: &[u8] = include_bytes!("fixtures/play/audio-140.mp4");
const VIDEO2: &[u8] = include_bytes!("fixtures/play/video-137-part2.mp4");
const AUDIO2: &[u8] = include_bytes!("fixtures/play/audio-140-part2.mp4");

/// Part 1 again, but its moov states its own length, as some muxers write it.
const LONG: &str = "withLength1";
/// Video with B-frames, so its truns carry composition offsets, with the part 1 audio.
const BFRAMES: &str = "bFrames_001";
const VIDEO_B: &[u8] = include_bytes!("fixtures/play/video-137-bframes.mp4");

/// Offset of the duration field in a version 0 mvhd, tkhd or mdhd box.
fn duration_at(kind: &str) -> usize {
    if kind == "tkhd" { 28 } else { 24 }
}

/// A copy of a fixture whose mvhd, tkhd and mdhd durations hold its sidx length (all version 0).
fn with_moov_length(src: &[u8]) -> Vec<u8> {
    let mut f = src.to_vec();
    let (ts, ticks) = source_length(src);
    let top = walk(src, 0, src.len());
    let moov = top
        .iter()
        .find(|k| k.0 == "moov")
        .map(|k| (k.1, k.2))
        .unwrap();
    let trak = kid(src, moov, "trak");
    let mvhd = kid(src, moov, "mvhd");
    let movie_ts = be32(src, mvhd.0 + 20);
    let movie = ticks * u64::from(movie_ts) / u64::from(ts);
    for (b, value) in [
        (mvhd, movie),
        (kid(src, trak, "tkhd"), movie),
        (kid(src, kid(src, trak, "mdia"), "mdhd"), ticks),
    ] {
        assert_eq!(src[b.0 + 8], 0);
        let kind = String::from_utf8_lossy(&src[b.0 + 4..b.0 + 8]).into_owned();
        let at = b.0 + duration_at(&kind);
        f[at..at + 4].copy_from_slice(&(value as u32).to_be_bytes());
    }
    f
}

fn odd_video() -> Vec<u8> {
    let mut v = VIDEO2.to_vec();
    let at = v.windows(4).position(|w| w == b"avc1").unwrap();
    // Visual sample entry: width sits 24 bytes into the entry body.
    v[at + 28..at + 30].copy_from_slice(&80u16.to_be_bytes());
    v
}

#[derive(Default)]
struct Upstream {
    files: HashMap<String, Vec<u8>>,
    requests: Mutex<Vec<String>>,
    refused_sig: Mutex<Option<String>>,
}

async fn upstream_file(
    State(up): State<Arc<Upstream>>,
    Path(name): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let Some(data) = up.files.get(&name) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let sig = query.get("sig").cloned().unwrap_or_default();
    let range = headers
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    up.requests
        .lock()
        .unwrap()
        .push(format!("{name} sig={sig} {range}"));
    if up.refused_sig.lock().unwrap().as_deref() == Some(sig.as_str()) {
        return StatusCode::FORBIDDEN.into_response();
    }
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

/// yt-dlp's answer for `id` on its `n`-th lookup: URLs signed `<id>-<n>`.
fn info(upstream: SocketAddr, id: &str, n: u32, video: &[u8], audio: &[u8]) -> String {
    let expire = (SystemTime::now() + Duration::from_secs(6 * 3600))
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let url =
        |kind: &str| format!("http://{upstream}/{id}-{kind}.mp4?expire={expire}&sig={id}-{n}");
    serde_json::json!({
        "id": id,
        "title": "Kabhi Main Kabhi Tum Episode 1",
        "duration": 6,
        "formats": [
            {"format_id": "140", "ext": "m4a", "vcodec": "none", "acodec": "mp4a.40.2",
             "protocol": "https", "filesize": audio.len(), "url": url("a")},
            {"format_id": "137", "ext": "mp4", "vcodec": "avc1.640028", "acodec": "none",
             "protocol": "https", "filesize": video.len(), "url": url("v")}
        ]
    })
    .to_string()
}

struct Harness {
    dir: tempfile::TempDir,
    upstream: Arc<Upstream>,
    base: String,
    client: reqwest::Client,
}

impl Harness {
    async fn new() -> Self {
        let sources: [(&str, Vec<u8>, Vec<u8>); 5] = [
            (ID1, VIDEO1.to_vec(), AUDIO1.to_vec()),
            (ID2, VIDEO2.to_vec(), AUDIO2.to_vec()),
            (ODD, odd_video(), AUDIO2.to_vec()),
            (LONG, with_moov_length(VIDEO1), with_moov_length(AUDIO1)),
            (BFRAMES, VIDEO_B.to_vec(), AUDIO1.to_vec()),
        ];
        let mut files = HashMap::new();
        for (id, video, audio) in &sources {
            files.insert(format!("{id}-v.mp4"), video.clone());
            files.insert(format!("{id}-a.mp4"), audio.clone());
        }
        let upstream = Arc::new(Upstream {
            files,
            ..Default::default()
        });
        let up_addr = spawn(
            Router::new()
                .route("/{name}", get(upstream_file))
                .with_state(upstream.clone()),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        for (id, video, audio) in &sources {
            for n in 1..=2 {
                std::fs::write(
                    dir.path().join(format!("info-{id}-{n}.json")),
                    info(up_addr, id, n, video, audio),
                )
                .unwrap();
            }
        }
        // The video id is the end of the watch URL, the last argument.
        let d = dir.path().display().to_string();
        let script = format!(
            "#!/bin/sh\nfor last; do :; done\nid=${{last##*=}}\n\
             n=$(($(cat '{d}/count-'$id 2>/dev/null || echo 0) + 1))\necho $n > '{d}/count-'$id\n\
             f='{d}/info-'$id-$n.json\n[ -f \"$f\" ] || f='{d}/info-'$id-2.json\ncat \"$f\"\n"
        );
        let path = dir.path().join("yt-dlp");
        support::install_executable(&path, script.as_bytes());
        let limiter = Arc::new(RateLimiter::new(RateLimitConfig {
            interval: Duration::from_millis(1),
            burst: 100,
            max_jitter: Duration::ZERO,
        }));
        let resolver = Arc::new(Resolver::new(YtDlp::new(path), limiter, MemoryCache::new()));
        let state = Arc::new(PlayState::new(resolver, PlayConfig::default()).unwrap());
        let addr = spawn(router(state)).await;
        Self {
            dir,
            upstream,
            base: format!("http://{addr}/play/"),
            client: reqwest::Client::new(),
        }
    }

    fn ytdlp_calls(&self, id: &str) -> u32 {
        std::fs::read_to_string(self.dir.path().join(format!("count-{id}")))
            .map(|s| s.trim().parse().unwrap())
            .unwrap_or(0)
    }

    async fn get(&self, path: &str, range: Option<&str>) -> reqwest::Response {
        let mut req = self.client.get(format!("{}{path}", self.base));
        if let Some(r) = range {
            req = req.header(header::RANGE, r);
        }
        req.send().await.unwrap()
    }

    async fn full(&self, path: &str) -> Vec<u8> {
        let res = self.get(path, None).await;
        assert_eq!(res.status(), StatusCode::OK);
        res.bytes().await.unwrap().to_vec()
    }
}

fn joined() -> String {
    format!("{ID1},{ID2}")
}

#[tokio::test]
async fn two_parts_play_as_one_continuous_file() {
    let h = Harness::new().await;
    let file = h.full(&joined()).await;
    let [video, audio] = flat_tracks(&file);

    // Every sample of both parts, part 1 first, media bytes untouched.
    for (track, (p1, p2)) in [(&video, (VIDEO1, VIDEO2)), (&audio, (AUDIO1, AUDIO2))] {
        assert_eq!(
            track.data.len(),
            source_samples(p1) + source_samples(p2),
            "track {}",
            track.id
        );
        assert_eq!(
            track.bytes(&file),
            [source_media(p1), source_media(p2)].concat(),
            "track {}",
            track.id
        );
    }

    // Part 2 starts where the longer track of part 1 ends, on both tracks: the decode time of its
    // first sample is that length, and the track lasts that plus part 2.
    let (vts, v1_len) = source_length(VIDEO1);
    let (ats, a1_len) = source_length(AUDIO1);
    let part1_secs = (v1_len as f64 / f64::from(vts)).max(a1_len as f64 / f64::from(ats));
    let join = [
        (part1_secs * f64::from(vts)).ceil() as u64,
        (part1_secs * f64::from(ats)).ceil() as u64,
    ];
    for (t, (track, (p1, p2))) in [(&video, (VIDEO1, VIDEO2)), (&audio, (AUDIO1, AUDIO2))]
        .into_iter()
        .enumerate()
    {
        let first_of_part2 = source_samples(p1);
        let start: u64 = track.durations[..first_of_part2]
            .iter()
            .map(|d| u64::from(*d))
            .sum();
        assert_eq!(start, join[t], "track {} at the join", t + 1);
        assert_eq!(track.ticks(), join[t] + source_length(p2).1);
    }
    // 6 s of video and 6.128 s of audio in part 1: the last video frame of part 1 is held until
    // the audio ends.
    let last = source_samples(VIDEO1) - 1;
    assert!(video.durations[last] > video.durations[last - 1]);

    // Part 2's video opens on a key frame the stss lists.
    let sync = video.sync.as_ref().unwrap();
    assert!(sync.contains(&(source_samples(VIDEO1) as u32 + 1)));
    assert_eq!((h.ytdlp_calls(ID1), h.ytdlp_calls(ID2)), (1, 1));
}

#[tokio::test]
async fn ranges_across_the_join_match_slices_of_the_full_file() {
    let h = Harness::new().await;
    let full = h.full(&joined()).await;
    let n = full.len();
    // Where the first sample of part 2's video lies.
    let [video, _] = flat_tracks(&full);
    let join = video.data[source_samples(VIDEO1)].0 as usize;
    let mdat = top(&full, "mdat").0;
    let cases = [
        (0, 99),
        (join, n - 2),
        (mdat - 1, mdat + 30),
        (join - 1, join),
        (join - 300, join + 300),
        (join, n - 1),
        (n / 4, 3 * n / 4),
        (n - 1, n - 1),
    ];
    for (a, b) in cases {
        let res = h.get(&joined(), Some(&format!("bytes={a}-{b}"))).await;
        assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            res.headers()[header::CONTENT_RANGE],
            format!("bytes {a}-{b}/{n}").as_str()
        );
        assert_eq!(res.bytes().await.unwrap(), full[a..=b], "bytes={a}-{b}");
    }
}

#[tokio::test]
async fn a_part_whose_urls_expire_is_looked_up_again_alone() {
    let reference = Harness::new().await.full(&joined()).await;

    let h = Harness::new().await;
    let head = h
        .client
        .head(format!("{}{}", h.base, joined()))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    *h.upstream.refused_sig.lock().unwrap() = Some(format!("{ID2}-1"));

    assert_eq!(h.full(&joined()).await, reference);
    assert_eq!((h.ytdlp_calls(ID1), h.ytdlp_calls(ID2)), (1, 2));
    let requests = h.upstream.requests.lock().unwrap().clone();
    assert!(
        requests.iter().any(|r| r.contains(&format!("sig={ID2}-2"))),
        "{requests:?}"
    );
}

#[tokio::test]
async fn parts_that_decode_differently_are_refused() {
    let h = Harness::new().await;
    let res = h.get(&format!("{ID1},{ODD}"), None).await;
    assert_eq!(res.status(), StatusCode::CONFLICT);
    let text = res.text().await.unwrap();
    assert!(
        text.contains("part 2") && text.contains("video format"),
        "{text}"
    );
}

#[tokio::test]
async fn bad_part_lists_are_not_found() {
    let h = Harness::new().await;
    for path in [
        format!("{ID1},"),
        format!("{ID1},,{ID2}"),
        format!("{ID1}+{ID2}"),
        [ID1; 7].join(","),
    ] {
        let res = h.get(&path, None).await;
        assert_eq!(res.status(), StatusCode::NOT_FOUND, "{path}");
    }
    assert_eq!(h.ytdlp_calls(ID1), 0);
    // An escaped comma is the same path.
    let res = h.get(&format!("{ID1}%2C{ID2}"), Some("bytes=0-9")).await;
    assert_eq!(res.status(), StatusCode::PARTIAL_CONTENT);
}

/// Needs ffprobe and ffmpeg on the PATH: `cargo test --test play_parts -- --ignored`.
#[tokio::test]
#[ignore]
async fn ffprobe_reads_the_summed_duration() {
    let h = Harness::new().await;
    for first in [ID1, LONG] {
        let url = format!("{}{first},{ID2}", h.base);
        // Part 1 lasts 6.128 s (its audio), part 2 4 s (its video).
        let duration = probe_duration(&url, &["h264", "aac"]).await;
        assert!((duration - 10.128).abs() < 0.05, "{duration}");
        // Decodes cleanly from the start, at 60 % and after a seek into part 2.
        for seek in ["0", "6.1", "8"] {
            decode_from(&url, seek).await;
        }
    }
}

#[tokio::test]
async fn moov_durations_hold_the_summed_length() {
    let h = Harness::new().await;
    // Part 1 lasts 6.128 s, part 2 4 s of video and 3.2 s of audio. Movie timescale 1000, video
    // 10240, audio 8000.
    let expected = (10128, vec![(10128, 103_711), (9328, 74_624)]);
    for first in [ID1, LONG] {
        let file = h.full(&format!("{first},{ID2}")).await;
        assert_eq!(moov_durations(&file), expected, "first part {first}");
    }
    // A single video gets its own length, whatever its moov said.
    let single = h.full(LONG).await;
    assert_eq!(
        moov_durations(&single),
        (6128, vec![(6000, 61_440), (6128, 49_024)])
    );
}

#[tokio::test]
async fn composition_offsets_get_a_ctts_and_an_edit_list() {
    let h = Harness::new().await;
    let file = h.full(BFRAMES).await;
    let [video, audio] = flat_tracks(&file);
    assert!(video.has_ctts && !audio.has_ctts);
    assert_eq!(video.bytes(&file), source_media(VIDEO_B));
    // The first frame is shown two frames (2048 ticks) after it is decoded, so the edit list
    // starts the track there and shows its full 6 s.
    let moov = top(&file, "moov");
    let trak = kids(&file, moov, "trak")[0];
    let elst = kid(&file, kid(&file, trak, "edts"), "elst");
    assert_eq!(file[elst.0 + 8], 1, "elst version 1");
    assert_eq!(be32(&file, elst.0 + 12), 1);
    assert_eq!(
        be64(&file, elst.0 + 16),
        6000,
        "segment duration in movie ticks"
    );
    assert_eq!(be64(&file, elst.0 + 24), 2048, "media time");
    assert!(find_kid(&file, kids(&file, moov, "trak")[1], "edts").is_none());
    assert_eq!(
        moov_durations(&file),
        (6128, vec![(6000, 61_440), (6128, 49_024)])
    );
}

/// Needs ffprobe and ffmpeg on the PATH: `cargo test --test play_parts -- --ignored`.
#[tokio::test]
#[ignore]
async fn b_frames_play_from_zero_and_seek() {
    let h = Harness::new().await;
    let url = format!("{}{BFRAMES}", h.base);
    let duration = probe_duration(&url, &["h264", "aac"]).await;
    assert!((duration - 6.128).abs() < 0.05, "{duration}");
    let out = tokio::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v",
            "-show_entries",
            "stream=start_time",
            "-of",
            "csv=p=0",
            &url,
        ])
        .output()
        .await
        .unwrap();
    let start: f64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    assert!(start.abs() < 0.01, "video starts at {start}");
    for seek in ["0", "3.7", "5.5"] {
        decode_from(&url, seek).await;
    }
    // Every decoded frame matches the source, in the same order and at the same time.
    let src = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(src.path(), VIDEO_B).unwrap();
    let frames = |input: String| async move {
        let out = tokio::process::Command::new("ffmpeg")
            .args([
                "-v", "error", "-i", &input, "-map", "0:v", "-f", "framemd5", "-",
            ])
            .output()
            .await
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| {
                let f: Vec<&str> = l.split(',').map(str::trim).collect();
                format!("{} {}", f[2], f[5])
            })
            .collect::<Vec<_>>()
    };
    let ours = frames(url.clone()).await;
    assert_eq!(ours.len(), 60);
    assert_eq!(ours, frames(src.path().display().to_string()).await);
}
