//! The steps a lookup tries when YouTube pushes back: plain, then a PO token from a bgutil server,
//! then the cookies of a spare account. Each step after the first is optional and runs only when
//! the one before it ended in a 429, a bot check or a sign-in request. A fallback step that works
//! becomes sticky for a while: lookups start there, so they do not provoke YouTube with plain
//! lookups that are expected to fail.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use serde::Serialize;
use tokio::time::Instant;
use url::Url;

use super::{ProbeFailure, ResolveError, Resolved, YtDlp, parse, ytdlp::max_height};
use crate::{
    binaries::now_unix,
    health::{Events, HealthEvent},
    ratelimit::Lane,
};

/// One rung of the ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Plain,
    PoToken,
    Cookies,
}

impl Step {
    pub const ALL: [Step; 3] = [Step::Plain, Step::PoToken, Step::Cookies];

    fn index(self) -> usize {
        self as usize
    }
}

/// The configured steps. The default runs plain lookups only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fallback {
    pub po_token: Option<PoToken>,
    pub cookies_file: Option<PathBuf>,
    /// How long lookups start at the PO token step after it worked before plain is tried again.
    pub sticky_for: Duration,
    /// The lowest video height a fallback step may answer with. Plain answers are taken as they
    /// are, since a plain lookup sees every format the video has.
    pub min_height: u32,
}

impl Default for Fallback {
    fn default() -> Self {
        Self {
            po_token: None,
            cookies_file: None,
            sticky_for: Duration::from_secs(60 * 60),
            min_height: 720,
        }
    }
}

/// Player clients for the cookies step. With cookies, yt-dlp drops every client without
/// `SUPPORTS_COOKIES` (`visionos`, `android_vr`, `ios`, `android`) and by default asks
/// `web_embedded,tv_samsung,web`, of which `web_embedded` offers only muxed low formats. The TV
/// clients support cookies and need no GVS PO token for their HTTPS formats (yt-dlp
/// `extractor/youtube/_base.py`, `INNERTUBE_CLIENTS`), so they carry the DASH formats 137 and 140.
const COOKIE_CLIENTS: &str = "tv,tv_downgraded";
/// `web` supports cookies too, but its HTTPS formats need a GVS PO token, so it is only asked
/// when the PO token provider is configured.
const COOKIE_CLIENTS_WITH_PO_TOKEN: &str = "tv,tv_downgraded,web";

/// A bgutil PO token HTTP server and where yt-dlp finds the plugin that talks to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoToken {
    pub base_url: Url,
    pub plugin_dir: Option<PathBuf>,
}

impl Fallback {
    /// The steps that run, in order.
    pub fn steps(&self) -> Vec<Step> {
        let mut steps = vec![Step::Plain];
        if self.po_token.is_some() {
            steps.push(Step::PoToken);
        }
        if self.cookies_file.is_some() {
            steps.push(Step::Cookies);
        }
        steps
    }

    /// The bgutil options from its README: the provider's `base_url` as an extractor argument, and
    /// the plugin folder.
    fn po_token_args(&self) -> Vec<String> {
        let Some(po) = &self.po_token else {
            return Vec::new();
        };
        let mut args = vec![
            "--extractor-args".to_owned(),
            format!(
                "youtubepot-bgutilhttp:base_url={}",
                po.base_url.as_str().trim_end_matches('/')
            ),
        ];
        if let Some(dir) = &po.plugin_dir {
            args.push("--plugin-dirs".to_owned());
            args.push(dir.display().to_string());
        }
        args
    }

    /// The cookies step's options except the cookies file: the PO token options where configured
    /// (a token alone does not pass a bot check tied to the IP, cookies with a token do) and the
    /// player clients that keep the best formats with cookies.
    fn cookies_args(&self) -> Vec<String> {
        let mut args = self.po_token_args();
        let clients = if self.po_token.is_some() {
            COOKIE_CLIENTS_WITH_PO_TOKEN
        } else {
            COOKIE_CLIENTS
        };
        args.push("--extractor-args".to_owned());
        args.push(format!("youtube:player_client={clients}"));
        args
    }

    /// Runs the steps in order until one works or a failure is not YouTube pushing back. The
    /// caller holds one limiter token for the whole ladder: a fallback step is the same lookup
    /// asked differently, right after the refusal.
    /// A sticky step is where the ladder starts, and a failure there goes on to the later steps.
    pub(super) async fn run(
        &self,
        ytdlp: &YtDlp,
        video_id: &str,
        stats: &FallbackStats,
        sticky: &Sticky,
        events: &Events,
        lane: Lane,
    ) -> Result<(Resolved, Step), ProbeFailure> {
        let mut steps = self.steps();
        if let Some(start) = sticky
            .current()
            .and_then(|s| steps.iter().position(|&step| step == s.step))
        {
            steps.drain(..start);
        }
        let mut last: Option<(ResolveError, Step)> = None;
        // Whether the step before asks for the next one: YouTube refused it, or its answer was
        // below the quality floor.
        let mut go_on = true;
        for step in steps {
            if !go_on {
                break;
            }
            if let Some((e, _)) = &last {
                tracing::info!(video_id, error = %e, ?step, "trying the next lookup step");
            }
            let mut extra = Vec::new();
            // Held until yt-dlp is done, then deleted with whatever it wrote back.
            let mut cookie_copy = None;
            match step {
                Step::Plain => {}
                Step::PoToken => extra = self.po_token_args(),
                Step::Cookies => {
                    let Some(source) = &self.cookies_file else {
                        break;
                    };
                    let copy = match copy_cookies(source).await {
                        Ok(copy) => copy,
                        Err(e) => {
                            tracing::error!(video_id, file = %source.display(), error = %e, "cannot copy the cookies file, skipping the cookies step");
                            break;
                        }
                    };
                    tracing::warn!(
                        video_id,
                        "lookup is using the spare account's cookies, YouTube may flag the account"
                    );
                    events.publish(HealthEvent::CookiesUsed {
                        video_id: video_id.to_owned(),
                    });
                    extra = self.cookies_args();
                    extra.push("--cookies".to_owned());
                    extra.push(copy.path().display().to_string());
                    cookie_copy = Some(copy);
                }
            }
            let output = ytdlp
                .run(ytdlp.args(video_id, &extra), video_id, lane)
                .await;
            drop(cookie_copy);
            let result = output
                .map_err(Rejection::Error)
                .and_then(|json| self.judge(step, video_id, &json));
            match result {
                Ok(resolved) => {
                    stats.record(step, true);
                    if step != Step::Plain {
                        tracing::info!(video_id, ?step, "lookup worked on a fallback step");
                    }
                    sticky.worked(step, self.sticky_for);
                    return Ok((resolved, step));
                }
                Err(Rejection::LowQuality { max_height, error }) => {
                    stats.record(step, false);
                    tracing::warn!(
                        video_id,
                        ?step,
                        max_height,
                        min_height = self.min_height,
                        "lookup step answered below the height floor, not using it"
                    );
                    // The refusal that led here stays the reason if no later step works.
                    let reason = last.take().map_or(error, |(e, _)| e);
                    last = Some((reason, step));
                }
                Err(Rejection::Error(e)) => {
                    stats.record(step, false);
                    go_on = e.needs_fallback();
                    last = Some((e, step));
                }
            }
        }
        sticky.clear();
        Err(match last {
            Some((error, step)) => ProbeFailure {
                error,
                step: Some(step),
            },
            None => ResolveError::Failed {
                exit_code: None,
                message: "no lookup step ran".to_owned(),
            }
            .into(),
        })
    }

    /// Parses a step's answer. A fallback answer below the height floor, or without usable H.264
    /// video, is rejected. A plain answer is taken as it is: plain sees every format the video
    /// has, so a low one is the video's own limit.
    fn judge(&self, step: Step, video_id: &str, json: &[u8]) -> Result<Resolved, Rejection> {
        let parsed = parse(json);
        if step == Step::Plain {
            return parsed.map_err(Rejection::Error);
        }
        match parsed {
            Ok(resolved) if resolved.video.quality_height().unwrap_or(0) >= self.min_height => {
                Ok(resolved)
            }
            Ok(_) | Err(ResolveError::NoFormats { .. }) => Err(Rejection::LowQuality {
                max_height: max_height(json),
                error: ResolveError::NoFormats {
                    video_id: video_id.to_owned(),
                    missing: "H.264 video at or above the fallback height floor",
                },
            }),
            Err(e) => Err(Rejection::Error(e)),
        }
    }
}

/// Why a step's answer is not used.
enum Rejection {
    Error(ResolveError),
    /// The answer parsed, but its video is below the floor or missing.
    LowQuality {
        max_height: Option<u32>,
        error: ResolveError,
    },
}

/// A private temporary copy of the cookies file. Written from the bytes, so the copy is writable
/// by yt-dlp even when the mounted original is read-only.
async fn copy_cookies(source: &Path) -> std::io::Result<tempfile::NamedTempFile> {
    let bytes = tokio::fs::read(source).await?;
    tokio::task::spawn_blocking(move || {
        let mut copy = tempfile::Builder::new()
            .prefix("tubarr-cookies-")
            .suffix(".txt")
            .tempfile()?;
        copy.write_all(&bytes)?;
        copy.flush()?;
        Ok(copy)
    })
    .await
    .map_err(std::io::Error::other)?
}

/// The fallback step lookups start at for now, if any.
#[derive(Debug, Default)]
pub struct Sticky(Mutex<Option<StickyEntry>>);

#[derive(Debug, Clone, Copy)]
struct StickyEntry {
    step: Step,
    until: Instant,
    until_unix: u64,
}

/// What the Health page shows about the sticky step.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct StickyStatus {
    pub step: Step,
    /// Unix seconds when lookups go back to trying plain first.
    pub until: u64,
    pub remaining_secs: f64,
}

impl Sticky {
    pub fn status(&self) -> Option<StickyStatus> {
        let entry = self.current()?;
        Some(StickyStatus {
            step: entry.step,
            until: entry.until_unix,
            remaining_secs: entry
                .until
                .saturating_duration_since(Instant::now())
                .as_secs_f64(),
        })
    }

    fn current(&self) -> Option<StickyEntry> {
        let mut entry = self.lock();
        if entry.is_some_and(|e| e.until <= Instant::now()) {
            *entry = None;
        }
        *entry
    }

    /// Makes a working PO token step sticky. A step that is already sticky keeps its expiry, so
    /// plain gets tried again on time however busy lookups are. Plain working clears it.
    /// The cookies step is never sticky: each use risks the spare account, so it runs only for a
    /// lookup the earlier steps could not answer, and a sticky PO token step stays as it is.
    fn worked(&self, step: Step, sticky_for: Duration) {
        if step == Step::Cookies {
            return;
        }
        let current = self.current();
        let mut entry = self.lock();
        if step == Step::Plain {
            *entry = None;
        } else if current.is_none_or(|e| e.step != step) {
            tracing::info!(?step, for_ = ?sticky_for, "lookups start at this fallback step for now");
            *entry = Some(StickyEntry {
                step,
                until: Instant::now() + sticky_for,
                until_unix: now_unix() + sticky_for.as_secs(),
            });
        }
    }

    fn clear(&self) {
        *self.lock() = None;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<StickyEntry>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// How often each step ran and how it went, since start.
#[derive(Debug, Default)]
pub struct FallbackStats {
    counts: Mutex<[StepCounts; 3]>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct StepCounts {
    pub attempts: u64,
    /// Lookups this step answered.
    pub succeeded: u64,
    pub failed: u64,
}

impl FallbackStats {
    fn record(&self, step: Step, ok: bool) {
        let mut counts = self.counts.lock().unwrap_or_else(|e| e.into_inner());
        let c = &mut counts[step.index()];
        c.attempts += 1;
        if ok {
            c.succeeded += 1;
        } else {
            c.failed += 1;
        }
    }

    pub fn get(&self, step: Step) -> StepCounts {
        self.counts.lock().unwrap_or_else(|e| e.into_inner())[step.index()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_follow_the_config() {
        assert_eq!(Fallback::default().steps(), [Step::Plain]);
        let full = Fallback {
            po_token: Some(PoToken {
                base_url: Url::parse("http://bgutil:4416/").unwrap(),
                plugin_dir: Some("/config/yt-dlp-plugins".into()),
            }),
            cookies_file: Some("/secrets/cookies.txt".into()),
            ..Fallback::default()
        };
        assert_eq!(full.steps(), Step::ALL);
        assert_eq!(
            full.po_token_args(),
            [
                "--extractor-args",
                "youtubepot-bgutilhttp:base_url=http://bgutil:4416",
                "--plugin-dirs",
                "/config/yt-dlp-plugins"
            ]
        );
        assert_eq!(
            full.cookies_args()[4..],
            [
                "--extractor-args",
                "youtube:player_client=tv,tv_downgraded,web"
            ]
        );
        let cookies_only = Fallback {
            po_token: None,
            cookies_file: Some("/c.txt".into()),
            ..Fallback::default()
        };
        assert_eq!(cookies_only.steps(), [Step::Plain, Step::Cookies]);
        assert!(cookies_only.po_token_args().is_empty());
        assert_eq!(
            cookies_only.cookies_args(),
            ["--extractor-args", "youtube:player_client=tv,tv_downgraded"]
        );
    }

    #[test]
    fn stats_count_per_step() {
        let stats = FallbackStats::default();
        stats.record(Step::Plain, false);
        stats.record(Step::PoToken, true);
        assert_eq!(
            stats.get(Step::Plain),
            StepCounts {
                attempts: 1,
                succeeded: 0,
                failed: 1
            }
        );
        assert_eq!(stats.get(Step::PoToken).succeeded, 1);
        assert_eq!(stats.get(Step::Cookies), StepCounts::default());
    }

    #[tokio::test(start_paused = true)]
    async fn sticky_step_keeps_its_expiry_and_ends() {
        let sticky = Sticky::default();
        let hour = Duration::from_secs(3600);
        sticky.worked(Step::Plain, hour);
        assert!(sticky.status().is_none());
        sticky.worked(Step::PoToken, hour);
        tokio::time::advance(Duration::from_secs(1800)).await;
        // Working again does not push the expiry out.
        sticky.worked(Step::PoToken, hour);
        let status = sticky.status().unwrap();
        assert_eq!(status.step, Step::PoToken);
        assert_eq!(status.remaining_secs, 1800.0);
        // The cookies step working leaves the sticky step alone.
        sticky.worked(Step::Cookies, hour);
        let status = sticky.status().unwrap();
        assert_eq!(status.step, Step::PoToken);
        assert_eq!(status.remaining_secs, 1800.0);
        tokio::time::advance(hour).await;
        assert!(sticky.status().is_none());
        sticky.worked(Step::Cookies, hour);
        assert!(sticky.status().is_none());
        sticky.worked(Step::PoToken, hour);
        sticky.clear();
        assert!(sticky.status().is_none());
    }

    fn answer(video_itags: &[&str]) -> Vec<u8> {
        let mut formats: Vec<serde_json::Value> = video_itags
            .iter()
            .map(|id| {
                let vcodec = if id.starts_with('1') {
                    "avc1.640028"
                } else {
                    "vp09.00.40.08"
                };
                serde_json::json!({"format_id": id, "vcodec": vcodec, "acodec": "none",
                    "url": format!("https://r.googlevideo.com/v?itag={id}")})
            })
            .collect();
        formats.push(serde_json::json!({"format_id": "140", "vcodec": "none",
            "acodec": "mp4a.40.2", "url": "https://r.googlevideo.com/v?itag=140"}));
        serde_json::to_vec(&serde_json::json!({"id": "abcdefghijk", "title": "T",
            "formats": formats}))
        .unwrap()
    }

    fn judged(step: Step, video_itags: &[&str]) -> Result<String, String> {
        match Fallback::default().judge(step, "abcdefghijk", &answer(video_itags)) {
            Ok(resolved) => Ok(resolved.video.format_id),
            Err(Rejection::Error(e)) => Err(format!("{:?}", e.kind())),
            Err(Rejection::LowQuality { .. }) => Err("LowQuality".to_owned()),
        }
    }

    #[test]
    fn judge_takes_h264_only_and_floors_fallback_steps() {
        assert_eq!(judged(Step::Plain, &["248", "137", "136"]).unwrap(), "137");
        assert_eq!(judged(Step::Plain, &["248", "136"]).unwrap(), "136");
        // Plain sees every format, so 480p is the video's own limit.
        assert_eq!(judged(Step::Plain, &["135"]).unwrap(), "135");
        assert_eq!(
            judged(Step::Plain, &["248", "247"]).unwrap_err(),
            "NoFormats"
        );
        assert_eq!(judged(Step::PoToken, &["248", "136"]).unwrap(), "136");
        assert_eq!(
            judged(Step::PoToken, &["248", "135"]).unwrap_err(),
            "LowQuality"
        );
        assert_eq!(
            judged(Step::Cookies, &["248", "247"]).unwrap_err(),
            "LowQuality"
        );
    }
}
