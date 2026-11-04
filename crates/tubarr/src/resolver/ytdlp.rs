//! Running yt-dlp for one video and turning its JSON into [`Resolved`].

use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use serde::{Deserialize, Serialize};

use super::ResolveError;
use crate::{
    newznab::title::quality_height,
    ratelimit::{Lane, ProcessSlots},
};

/// Video-only H.264 formats in order of preference: 1080p, 720p, 480p.
pub const VIDEO_FORMATS: [&str; 3] = ["137", "136", "135"];
/// AAC audio in fragmented MP4, in order of preference: 128k, 256k, 48k HE-AAC. WebM Opus (251,
/// 250) is left out on purpose: the virtual file takes its sample tables from ISO BMFF fragments
/// and cannot carry a track from a WebM file without demuxing it.
pub const AUDIO_FORMATS: [&str; 3] = ["140", "141", "139"];

/// How to run yt-dlp.
#[derive(Debug, Clone)]
pub struct YtDlp {
    pub binary: PathBuf,
    /// Passed as `--js-runtimes deno:<path>` so yt-dlp can solve YouTube's JavaScript challenges.
    pub deno: Option<PathBuf>,
    /// The process group is killed when a lookup takes longer than this.
    pub timeout: Duration,
    /// Shared by every clone, so lookups, listings and checks together never run more processes
    /// at once than it allows.
    pub processes: Arc<ProcessSlots>,
}

impl YtDlp {
    /// One process at a time, the default of `rate_limit.processes`.
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            deno: None,
            timeout: Duration::from_secs(60),
            processes: Arc::new(ProcessSlots::new(1)),
        }
    }

    /// The arguments for one lookup, with a fallback step's `extra` options before the watch URL.
    /// `--ignore-config` keeps a stray yt-dlp config (or cookies file named in one) from changing
    /// what Tubarr asks for.
    pub fn args(&self, video_id: &str, extra: &[String]) -> Vec<String> {
        let mut args = vec![
            "-J".to_owned(),
            "--no-warnings".to_owned(),
            "--ignore-config".to_owned(),
            "--no-playlist".to_owned(),
        ];
        if let Some(deno) = &self.deno {
            args.push("--js-runtimes".to_owned());
            args.push(format!("deno:{}", deno.display()));
        }
        args.extend_from_slice(extra);
        args.push(format!("https://www.youtube.com/watch?v={video_id}"));
        args
    }

    /// Runs yt-dlp with `args` under the timeout and returns its stdout, or the failure sorted by
    /// [`classify`]. `what` names the target in logs. Waits for a process slot in `lane` first;
    /// the timeout starts once it runs. Rate limiting is the caller's job.
    pub async fn run(
        &self,
        args: Vec<String>,
        what: &str,
        lane: Lane,
    ) -> Result<Vec<u8>, ResolveError> {
        // Declared before the child, so it is given back only after the group is gone.
        let _slot = self.processes.acquire(lane).await;
        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The standalone yt-dlp is a PyInstaller bootloader with a Python child, and deno runs
            // below that, so a timeout has to kill the whole group, not just the first process.
            .process_group(0)
            .kill_on_drop(true);
        let child = spawn(&mut command).await.map_err(|e| ResolveError::Spawn {
            binary: self.binary.display().to_string(),
            message: e.to_string(),
        })?;
        let pid = child.id();
        let group = KillGroupOnDrop(pid);

        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(result) => result.map_err(|e| ResolveError::Spawn {
                binary: self.binary.display().to_string(),
                message: e.to_string(),
            })?,
            Err(_) => {
                tracing::warn!(what, timeout = ?self.timeout, "yt-dlp timed out, killing it");
                return Err(ResolveError::Timeout(self.timeout));
            }
        };
        // The leader exited on its own, so its group id may be reused: do not signal it.
        std::mem::forget(group);

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(classify(&stderr, output.status.code()));
        }
        Ok(output.stdout)
    }
}

/// Spawns `command`, retrying a few times on ETXTBSY. That error means some process still holds a
/// write descriptor to the binary: a fork elsewhere in this process that has not reached exec yet
/// can inherit one from a file being written at that moment (an update, a test fake). It clears
/// within milliseconds.
async fn spawn(command: &mut tokio::process::Command) -> std::io::Result<tokio::process::Child> {
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Err(e)
                if e.raw_os_error() == Some(nix::errno::Errno::ETXTBSY as i32) && attempt < 3 =>
            {
                attempt += 1;
                tokio::time::sleep(Duration::from_millis(20 * attempt)).await;
            }
            result => return result,
        }
    }
}

/// Kills the child's process group when a lookup is abandoned (timeout or a dropped request).
struct KillGroupOnDrop(Option<u32>);

impl Drop for KillGroupOnDrop {
    fn drop(&mut self) {
        if let Some(pid) = self.0.and_then(|p| i32::try_from(p).ok()) {
            // Fails harmlessly with ESRCH when the group is already gone.
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
        }
    }
}

/// Sorts a failed run into the cases Tubarr reacts to differently.
pub fn classify(stderr: &str, exit_code: Option<i32>) -> ResolveError {
    let message = last_error_line(stderr);
    // yt-dlp writes both straight and curly apostrophes depending on the message source.
    let lower = stderr.to_lowercase().replace('\u{2019}', "'");
    let has = |needles: &[&str]| needles.iter().any(|n| lower.contains(n));

    if has(&["confirm you're not a bot", "confirm that you're not a bot"]) {
        ResolveError::BotCheck(message)
    } else if has(&[
        "http error 429",
        "too many requests",
        "rate-limited",
        "rate limited",
    ]) {
        ResolveError::RateLimited(message)
    } else if has(&[
        "confirm your age",
        "age-restricted",
        "age restricted",
        "inappropriate for some users",
    ]) {
        ResolveError::AgeRestricted(message)
    } else if has(&[
        "not made this video available in your country",
        "not available in your country",
        "geo restrict",
        "geo-restrict",
        "blocked it in your country",
    ]) {
        ResolveError::GeoBlocked(message)
    } else if has(&[
        "this live event will begin",
        "premieres in",
        "premiere will begin",
    ]) {
        ResolveError::Live
    } else if has(&[
        "members-only",
        "join this channel to get access",
        "only available to music premium members",
        "only available to youtube premium members",
    ]) {
        ResolveError::MembersOnly(message)
    } else if has(&[
        "video unavailable",
        "video is unavailable",
        "private video",
        "this video is private",
        "has been removed",
        "no longer available",
        "account associated with this video has been terminated",
        "this video does not exist",
    ]) {
        ResolveError::Unavailable(message)
    } else {
        ResolveError::Failed { exit_code, message }
    }
}

/// The last `ERROR:` line, or the last non-empty line, trimmed to something a log can hold.
fn last_error_line(stderr: &str) -> String {
    let mut lines = stderr.lines().map(str::trim).filter(|l| !l.is_empty());
    let line = lines
        .clone()
        .rfind(|l| l.starts_with("ERROR:"))
        .or_else(|| lines.next_back())
        .unwrap_or("yt-dlp failed without a message");
    line.chars().take(500).collect()
}

/// Everything Tubarr needs from one lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub meta: VideoMeta,
    pub video: StreamFormat,
    pub audio: StreamFormat,
    pub subtitles: Vec<Subtitle>,
    /// The earliest `expire=` found in the two stream URLs, after which YouTube refuses them.
    pub expires_at: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoMeta {
    pub id: String,
    pub title: String,
    /// Whole seconds.
    pub duration: Option<u64>,
    /// `YYYYMMDD` as yt-dlp gives it.
    pub upload_date: Option<String>,
    /// The exact upload time in Unix seconds, when yt-dlp gives it.
    pub timestamp: Option<i64>,
    pub channel_id: Option<String>,
}

/// Serializable so a database cache can keep it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamFormat {
    pub format_id: String,
    pub url: String,
    pub ext: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub filesize: Option<u64>,
    pub filesize_approx: Option<u64>,
    /// Headers yt-dlp says the request must carry (User-Agent and friends).
    pub http_headers: BTreeMap<String, String>,
}

impl StreamFormat {
    /// The exact size when known, else YouTube's estimate.
    pub fn size(&self) -> Option<u64> {
        self.filesize.or(self.filesize_approx)
    }

    /// The resolution label ([`quality_height`]) a release of this video carries, from the itag
    /// when yt-dlp left the height out, so the name matches the stream served.
    pub fn quality_height(&self) -> Option<u32> {
        match self.height.filter(|h| *h > 0) {
            Some(h) => Some(quality_height(self.width, h)),
            None => match itag(&self.format_id) {
                "137" => Some(1080),
                "136" => Some(720),
                "135" => Some(480),
                _ => None,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subtitle {
    pub lang: String,
    /// True for YouTube's speech-to-text captions, false for uploaded subtitles.
    pub automatic: bool,
    pub ext: String,
    pub url: String,
}

#[derive(Deserialize)]
struct RawInfo {
    id: String,
    title: String,
    duration: Option<f64>,
    upload_date: Option<String>,
    timestamp: Option<i64>,
    channel_id: Option<String>,
    live_status: Option<String>,
    #[serde(default)]
    formats: Vec<RawFormat>,
    #[serde(default)]
    subtitles: HashMap<String, Vec<RawSubtitle>>,
    #[serde(default)]
    automatic_captions: HashMap<String, Vec<RawSubtitle>>,
}

#[derive(Deserialize)]
struct RawFormat {
    format_id: String,
    url: Option<String>,
    ext: Option<String>,
    vcodec: Option<String>,
    acodec: Option<String>,
    protocol: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
    filesize: Option<u64>,
    filesize_approx: Option<u64>,
    /// yt-dlp gives the original audio track 10 and YouTube's dubbed tracks -1.
    language_preference: Option<i64>,
    #[serde(default)]
    http_headers: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct RawSubtitle {
    ext: Option<String>,
    url: Option<String>,
}

/// Parses `yt-dlp -J` output and picks the formats Tubarr serves.
pub fn parse(json: &[u8]) -> Result<Resolved, ResolveError> {
    let raw: RawInfo =
        serde_json::from_slice(json).map_err(|e| ResolveError::Parse(e.to_string()))?;
    if raw.live_status.as_deref().is_some_and(is_unfinished_live) {
        return Err(ResolveError::Live);
    }

    let video = VIDEO_FORMATS
        .iter()
        .find_map(|id| pick(&raw.formats, id, is_h264_video))
        .ok_or_else(|| ResolveError::NoFormats {
            video_id: raw.id.clone(),
            missing: "H.264 video (137, 136 or 135)",
        })?;
    let audio = pick_audio(&raw.formats).ok_or_else(|| ResolveError::NoFormats {
        video_id: raw.id.clone(),
        missing: "AAC audio (140, 141 or 139)",
    })?;

    let expires_at = [&video.url, &audio.url]
        .into_iter()
        .filter_map(|url| url_expiry(url))
        .min();

    let mut subtitles = subtitle_list(raw.subtitles, false);
    subtitles.extend(subtitle_list(raw.automatic_captions, true));

    Ok(Resolved {
        meta: VideoMeta {
            id: raw.id,
            title: raw.title,
            duration: raw
                .duration
                .filter(|d| d.is_finite() && *d >= 0.0)
                .map(|d| d.round() as u64),
            upload_date: raw.upload_date,
            timestamp: raw.timestamp,
            channel_id: raw.channel_id,
        },
        video,
        audio,
        subtitles,
        expires_at,
    })
}

/// yt-dlp's `live_status` for a stream that is live, still to come, or over but not processed
/// into the usual DASH formats yet. A `was_live` stream plays like any upload.
pub fn is_unfinished_live(status: &str) -> bool {
    matches!(status, "is_live" | "is_upcoming" | "post_live")
}

/// The tallest video format in `yt-dlp -J` output, of any codec or protocol, for logs about a
/// lookup whose formats are not good enough.
pub(super) fn max_height(json: &[u8]) -> Option<u32> {
    let raw: RawInfo = serde_json::from_slice(json).ok()?;
    raw.formats
        .iter()
        .filter(|f| f.vcodec.as_deref().is_some_and(|v| v != "none"))
        .filter_map(|f| f.height)
        .max()
}

fn pick(formats: &[RawFormat], id: &str, fits: fn(&RawFormat) -> bool) -> Option<StreamFormat> {
    let f = formats
        .iter()
        .find(|f| itag(&f.format_id) == id && !is_drc(f) && fits(f) && is_plain_https(f))?;
    stream(f)
}

/// The original audio track before any dub, then the itag order of [`AUDIO_FORMATS`], then the
/// plain mix before its dynamic range compressed variant, then the format id, so the same answer
/// always picks the same stream whatever order yt-dlp lists it in.
fn pick_audio(formats: &[RawFormat]) -> Option<StreamFormat> {
    let f = formats
        .iter()
        .filter(|f| is_aac_audio(f) && is_plain_https(f))
        .filter_map(|f| {
            let rank = AUDIO_FORMATS
                .iter()
                .position(|id| *id == itag(&f.format_id))?;
            Some((f, rank))
        })
        .min_by_key(|(f, rank)| {
            (
                std::cmp::Reverse(f.language_preference.unwrap_or(-1)),
                *rank,
                is_drc(f),
                f.format_id.as_str(),
            )
        })?
        .0;
    stream(f)
}

/// The itag a format id stands for. Videos with several audio tracks (YouTube's dubs) name them
/// `140-0`, `140-1` and so on, and dynamic range compressed audio is `140-drc`.
fn itag(format_id: &str) -> &str {
    format_id.split('-').next().unwrap_or(format_id)
}

fn is_drc(f: &RawFormat) -> bool {
    f.format_id.split('-').any(|part| part == "drc")
}

fn stream(f: &RawFormat) -> Option<StreamFormat> {
    Some(StreamFormat {
        format_id: f.format_id.clone(),
        url: f.url.clone()?,
        ext: f.ext.clone(),
        width: f.width,
        height: f.height,
        filesize: f.filesize,
        filesize_approx: f.filesize_approx,
        http_headers: f.http_headers.clone(),
    })
}

fn is_h264_video(f: &RawFormat) -> bool {
    f.vcodec.as_deref().is_some_and(|v| v.starts_with("avc1"))
        && f.acodec.as_deref().is_none_or(|a| a == "none")
}

fn is_aac_audio(f: &RawFormat) -> bool {
    f.vcodec.as_deref().is_none_or(|v| v == "none")
        && f.acodec.as_deref().is_some_and(|a| a.starts_with("mp4a"))
}

/// Only direct HTTPS files can be relayed with Range requests; HLS and DASH manifests cannot.
fn is_plain_https(f: &RawFormat) -> bool {
    f.url.is_some() && f.protocol.as_deref().is_none_or(|p| p == "https")
}

/// One entry per language, preferring WebVTT because it converts to SRT without loss.
fn subtitle_list(raw: HashMap<String, Vec<RawSubtitle>>, automatic: bool) -> Vec<Subtitle> {
    let mut list: Vec<Subtitle> = raw
        .into_iter()
        .filter_map(|(lang, tracks)| {
            let with_url = || tracks.iter().filter(|t| t.url.is_some());
            let track = with_url()
                .find(|t| t.ext.as_deref() == Some("vtt"))
                .or_else(|| with_url().next())?;
            Some(Subtitle {
                lang,
                automatic,
                ext: track.ext.clone().unwrap_or_else(|| "vtt".to_owned()),
                url: track.url.clone()?,
            })
        })
        .collect();
    list.sort_by(|a, b| a.lang.cmp(&b.lang));
    list
}

/// Reads the `expire=<unix seconds>` query parameter from a googlevideo URL.
pub fn url_expiry(url: &str) -> Option<SystemTime> {
    let query = url.split_once('?')?.1;
    let value = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("expire="))?;
    let secs: u64 = value.parse().ok()?;
    UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_carry_deno_and_the_watch_url() {
        let mut ytdlp = YtDlp::new("/config/bin/yt-dlp");
        assert!(
            !ytdlp
                .args("0eD-GQCutAU", &[])
                .contains(&"--js-runtimes".to_owned())
        );
        ytdlp.deno = Some("/config/bin/deno".into());
        let args = ytdlp.args("0eD-GQCutAU", &[]);
        let at = args.iter().position(|a| a == "--js-runtimes").unwrap();
        assert_eq!(args[at + 1], "deno:/config/bin/deno");
        assert_eq!(
            args.last().unwrap(),
            "https://www.youtube.com/watch?v=0eD-GQCutAU"
        );
        assert!(args.contains(&"-J".to_owned()));
        assert!(!args.iter().any(|a| a.contains("cookies")));
    }

    #[test]
    fn expiry_is_read_from_the_query() {
        let url =
            "https://rr1---sn-x.googlevideo.com/videoplayback?expire=1760000000&ei=abc&itag=137";
        assert_eq!(
            url_expiry(url),
            Some(UNIX_EPOCH + Duration::from_secs(1_760_000_000))
        );
        assert_eq!(
            url_expiry("https://example.com/videoplayback?itag=137"),
            None
        );
        assert_eq!(url_expiry("https://example.com/a?expire=soon"), None);
    }

    fn kind(stderr: &str) -> String {
        format!("{:?}", classify(stderr, Some(1)))
            .split(['(', ' '])
            .next()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn stderr_is_classified() {
        let cases = [
            (
                "ERROR: [youtube] abc: Sign in to confirm you\u{2019}re not a bot. Use --cookies-from-browser",
                "BotCheck",
            ),
            (
                "ERROR: [youtube] abc: Unable to download API page: HTTP Error 429: Too Many Requests",
                "RateLimited",
            ),
            (
                "ERROR: [youtube] abc: This content isn't available, try again later. Your account has been rate-limited by YouTube for up to an hour.",
                "RateLimited",
            ),
            (
                "ERROR: [youtube] abc: Sign in to confirm your age. This video may be inappropriate for some users.",
                "AgeRestricted",
            ),
            (
                "ERROR: [youtube] abc: Video unavailable. The uploader has not made this video available in your country",
                "GeoBlocked",
            ),
            (
                "ERROR: [youtube] abc: Private video. Sign in if you've been granted access",
                "Unavailable",
            ),
            (
                "ERROR: [youtube] abc: Video unavailable. This video has been removed by the uploader",
                "Unavailable",
            ),
            ("ERROR: [youtube] abc: Video unavailable", "Unavailable"),
            (
                "ERROR: [youtube] abc: This video is unavailable",
                "Unavailable",
            ),
            (
                "ERROR: [youtube] abc: Join this channel to get access to members-only content like this video, and other exclusive perks.",
                "MembersOnly",
            ),
            (
                "ERROR: [youtube] abc: This video is only available to Music Premium members",
                "MembersOnly",
            ),
            (
                "ERROR: [youtube] abc: This live event will begin in 3 hours.",
                "Live",
            ),
            ("ERROR: [youtube] abc: Premieres in 2 hours", "Live"),
            ("ERROR: [youtube] abc: Some new failure", "Failed"),
        ];
        for (stderr, expected) in cases {
            assert_eq!(kind(stderr), expected, "{stderr}");
            let e = classify(stderr, Some(1));
            // A gone video stays out for good, a members-only one or a livestream is looked at
            // again.
            match expected {
                "Unavailable" => assert!(e.is_settled() && !e.retry_later(), "{stderr}"),
                "MembersOnly" | "Live" => assert!(e.is_settled() && e.retry_later(), "{stderr}"),
                _ => {}
            }
        }
    }

    #[test]
    fn message_is_the_last_error_line() {
        let stderr =
            "[youtube] abc: Downloading webpage\nERROR: [youtube] abc: Video unavailable\n\n";
        match classify(stderr, Some(1)) {
            ResolveError::Unavailable(m) => {
                assert_eq!(m, "ERROR: [youtube] abc: Video unavailable")
            }
            other => panic!("{other:?}"),
        }
        match classify("", Some(2)) {
            ResolveError::Failed { exit_code, message } => {
                assert_eq!(exit_code, Some(2));
                assert_eq!(message, "yt-dlp failed without a message");
            }
            other => panic!("{other:?}"),
        }
    }

    fn format(id: &str, vcodec: &str, acodec: &str) -> serde_json::Value {
        serde_json::json!({
            "format_id": id, "vcodec": vcodec, "acodec": acodec, "protocol": "https",
            "url": format!("https://rr1.googlevideo.com/videoplayback?itag={id}&expire=1760000000"),
            "ext": "mp4", "http_headers": {"User-Agent": "Mozilla/5.0"}
        })
    }

    fn info(formats: Vec<serde_json::Value>) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "id": "abcdefghijk", "title": "T", "duration": 2101.4,
            "upload_date": "20241001", "timestamp": 1727795400, "channel_id": "UC123",
            "formats": formats
        }))
        .unwrap()
    }

    #[test]
    fn falls_back_to_lower_h264_formats() {
        let json = info(vec![
            format("136", "avc1.4d401f", "none"),
            format("135", "avc1.4d401e", "none"),
            format("140", "none", "mp4a.40.2"),
        ]);
        let resolved = parse(&json).unwrap();
        assert_eq!(resolved.video.format_id, "136");
        assert_eq!(resolved.meta.duration, Some(2101));
        assert_eq!(resolved.meta.timestamp, Some(1_727_795_400));
    }

    #[test]
    fn skips_non_h264_and_manifest_formats() {
        let mut hls = format("137", "avc1.640028", "none");
        hls["protocol"] = "m3u8_native".into();
        let json = info(vec![
            hls,
            format("136", "vp09.00.40.08", "none"),
            format("135", "avc1.4d401e", "none"),
            format("140", "none", "mp4a.40.2"),
        ]);
        assert_eq!(parse(&json).unwrap().video.format_id, "135");
    }

    fn video(id: &str, vcodec: &str, width: u32, height: u32) -> serde_json::Value {
        let mut f = format(id, vcodec, "none");
        f["width"] = width.into();
        f["height"] = height.into();
        f
    }

    /// What YouTube offers a typical upload: VP9 (248, 247) and AV1 (399, 398) next to H.264.
    fn modern(h264: &[(&str, u32, u32)]) -> Vec<serde_json::Value> {
        let mut formats = vec![
            video("399", "av01.0.08M.08", 1920, 1080),
            video("248", "vp09.00.40.08", 1920, 1080),
            video("398", "av01.0.05M.08", 1280, 720),
            video("247", "vp09.00.31.08", 1280, 720),
        ];
        formats.extend(
            h264.iter()
                .map(|(id, w, h)| video(id, "avc1.640028", *w, *h)),
        );
        formats.push(format("251", "none", "opus"));
        formats.push(format("140", "none", "mp4a.40.2"));
        formats
    }

    #[test]
    fn h264_1080p_is_picked_when_offered() {
        let json = info(modern(&[
            ("137", 1920, 1080),
            ("136", 1280, 720),
            ("135", 854, 480),
        ]));
        let resolved = parse(&json).unwrap();
        assert_eq!(resolved.video.format_id, "137");
        assert_eq!(resolved.video.quality_height(), Some(1080));
        assert_eq!(resolved.audio.format_id, "140");
    }

    #[test]
    fn h264_720p_is_picked_over_vp9_and_av1_at_1080p() {
        let json = info(modern(&[("136", 1280, 720), ("135", 854, 480)]));
        let resolved = parse(&json).unwrap();
        assert_eq!(resolved.video.format_id, "136");
        assert_eq!(resolved.video.quality_height(), Some(720));
    }

    #[test]
    fn an_unfinished_livestream_waits_and_a_finished_one_plays() {
        let with_status = |status: &str| {
            let mut json: serde_json::Value = serde_json::from_slice(&info(vec![
                format("137", "avc1.640028", "none"),
                format("140", "none", "mp4a.40.2"),
            ]))
            .unwrap();
            json["live_status"] = status.into();
            parse(&serde_json::to_vec(&json).unwrap())
        };
        for status in ["is_live", "is_upcoming", "post_live"] {
            let err = with_status(status).unwrap_err();
            assert_eq!(err, ResolveError::Live, "{status}");
            assert!(err.is_settled() && err.retry_later());
            assert_eq!(err.to_string(), "livestream still processing");
        }
        for status in ["was_live", "not_live"] {
            assert!(with_status(status).is_ok(), "{status}");
        }
    }

    #[test]
    fn vp9_and_av1_only_is_not_playable_yet() {
        let err = parse(&info(modern(&[]))).unwrap_err();
        assert!(
            matches!(
                err,
                ResolveError::NoFormats {
                    missing: "H.264 video (137, 136 or 135)",
                    ..
                }
            ),
            "{err:?}"
        );
        assert!(err.is_settled() && err.retry_later());
    }

    #[test]
    fn quality_height_falls_back_to_the_itag() {
        let label = |id: &str, width: Option<u32>, height: Option<u32>| {
            let mut f = format(id, "avc1.640028", "none");
            f["width"] = width.into();
            f["height"] = height.into();
            parse(&info(vec![f, format("140", "none", "mp4a.40.2")]))
                .unwrap()
                .video
                .quality_height()
        };
        assert_eq!(label("137", None, None), Some(1080));
        assert_eq!(label("136", None, None), Some(720));
        assert_eq!(label("135", None, Some(0)), Some(480));
        // A cropped 1080p upload is still labelled 1080p.
        assert_eq!(label("137", Some(1920), Some(804)), Some(1080));
    }

    #[test]
    fn missing_audio_is_an_error() {
        let json = info(vec![format("137", "avc1.640028", "none")]);
        assert!(matches!(
            parse(&json),
            Err(ResolveError::NoFormats {
                missing: "AAC audio (140, 141 or 139)",
                ..
            })
        ));
    }

    fn audio(id: &str, acodec: &str, language_preference: i64) -> serde_json::Value {
        let mut f = format(id, "none", acodec);
        f["language_preference"] = language_preference.into();
        f
    }

    fn audio_pick(formats: Vec<serde_json::Value>) -> Result<String, ResolveError> {
        let mut all = vec![format("137", "avc1.640028", "none")];
        all.extend(formats);
        parse(&info(all)).map(|r| r.audio.format_id)
    }

    #[test]
    fn audio_prefers_the_original_track_over_dubs() {
        let picked = audio_pick(vec![
            audio("140-0", "mp4a.40.2", -1),
            audio("140-1", "mp4a.40.2", 10),
            audio("251-1", "opus", 10),
        ]);
        assert_eq!(picked.unwrap(), "140-1");
        // The original track at a lower bitrate still beats a dub at 128k.
        let picked = audio_pick(vec![
            audio("140-0", "mp4a.40.2", -1),
            audio("139-1", "mp4a.40.5", 10),
        ]);
        assert_eq!(picked.unwrap(), "139-1");
    }

    #[test]
    fn audio_falls_back_to_other_aac_itags() {
        let only_139 = vec![audio("139", "mp4a.40.5", -1), audio("251", "opus", -1)];
        assert_eq!(audio_pick(only_139).unwrap(), "139");
        let with_141 = vec![audio("139", "mp4a.40.5", -1), audio("141", "mp4a.40.2", -1)];
        assert_eq!(audio_pick(with_141).unwrap(), "141");
        let drc = vec![
            audio("140-drc", "mp4a.40.2", -1),
            audio("140", "mp4a.40.2", -1),
        ];
        assert_eq!(audio_pick(drc).unwrap(), "140");
        let only_drc = vec![audio("140-drc", "mp4a.40.2", -1)];
        assert_eq!(audio_pick(only_drc).unwrap(), "140-drc");
    }

    /// ebioxADOBOw lists a plain and a DRC 140 of the same size, and Opus beside them. The pick
    /// must not depend on the order yt-dlp lists them in.
    #[test]
    fn audio_pick_ignores_listing_order() {
        let plain_and_drc = || {
            vec![
                audio("140", "mp4a.40.2", 10),
                audio("140-drc", "mp4a.40.2", 10),
                audio("251", "opus", 10),
                audio("251-drc", "opus", 10),
            ]
        };
        let mut reversed = plain_and_drc();
        reversed.reverse();
        assert_eq!(audio_pick(plain_and_drc()).unwrap(), "140");
        assert_eq!(audio_pick(reversed).unwrap(), "140");
        let tracks = vec![
            audio("140-1", "mp4a.40.2", -1),
            audio("140-0", "mp4a.40.2", -1),
        ];
        assert_eq!(audio_pick(tracks).unwrap(), "140-0");
    }

    #[test]
    fn opus_only_audio_is_not_playable() {
        let err = audio_pick(vec![audio("251", "opus", -1), audio("250", "opus", -1)]);
        assert!(
            matches!(err, Err(ResolveError::NoFormats { missing, .. }) if missing.contains("AAC")),
            "{err:?}"
        );
    }

    #[test]
    fn max_height_spans_every_video_format() {
        let mut muxed = format("18", "avc1.42001E", "mp4a.40.2");
        muxed["height"] = 324.into();
        let mut audio = format("140", "none", "mp4a.40.2");
        audio["height"] = 999.into();
        assert_eq!(max_height(&info(vec![muxed, audio])), Some(324));
        assert_eq!(max_height(&info(vec![])), None);
        assert_eq!(max_height(b"not json"), None);
    }

    #[test]
    fn bad_json_is_a_parse_error() {
        assert!(matches!(parse(b"not json"), Err(ResolveError::Parse(_))));
    }
}
