//! The yt-dlp and deno binaries Tubarr runs: fetched from their GitHub releases into the bin dir,
//! checked against the published SHA-256 sums, and kept current by a daily check.
//!
//! yt-dlp is the standalone `yt-dlp_linux` build (`yt-dlp_linux_aarch64` on arm64, PyPI is far too slow from
//! where Tubarr runs), deno is what yt-dlp uses to solve YouTube's JS challenges. Other architectures get no
//! managed copy: the paths under `[binaries]` must point at binaries installed some other way.
//!
//! Every swap keeps the old file as `<name>.prev`, and a new binary that fails its `--version` smoke
//! run is rolled back at once.

mod github;
mod install;

use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::config::{Binaries as Settings, Channel};
use github::{Latest, Release};

/// How long one `--version` smoke run may take. The PyInstaller build unpacks itself on start.
const SMOKE_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the background loop wakes up. The check interval decides whether it asks GitHub.
const LOOP_TICK: Duration = Duration::from_secs(3600);
const STATE_FILE: &str = "binaries.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tool {
    YtDlp,
    Deno,
}

impl Tool {
    pub const ALL: [Tool; 2] = [Tool::YtDlp, Tool::Deno];

    pub fn name(self) -> &'static str {
        match self {
            Tool::YtDlp => "yt-dlp",
            Tool::Deno => "deno",
        }
    }

    /// The release asset for this machine. `arch` is `std::env::consts::ARCH`, passed in so tests can pick one.
    fn spec(self, channel: Channel, arch: &str) -> Result<Spec, BinaryError> {
        let unsupported = || BinaryError::UnsupportedArch {
            arch: arch.to_owned(),
        };
        Ok(match self {
            Tool::YtDlp => Spec {
                repo: match channel {
                    Channel::Stable => "yt-dlp/yt-dlp",
                    Channel::Nightly => "yt-dlp/yt-dlp-nightly-builds",
                },
                asset: match arch {
                    "x86_64" => "yt-dlp_linux",
                    "aarch64" => "yt-dlp_linux_aarch64",
                    _ => return Err(unsupported()),
                },
                sums: "SHA2-256SUMS",
                zip_entry: None,
            },
            Tool::Deno => {
                let (asset, sums) = match arch {
                    "x86_64" => (
                        "deno-x86_64-unknown-linux-gnu.zip",
                        "deno-x86_64-unknown-linux-gnu.zip.sha256sum",
                    ),
                    "aarch64" => (
                        "deno-aarch64-unknown-linux-gnu.zip",
                        "deno-aarch64-unknown-linux-gnu.zip.sha256sum",
                    ),
                    _ => return Err(unsupported()),
                };
                Spec {
                    repo: "denoland/deno",
                    asset,
                    sums,
                    zip_entry: Some("deno"),
                }
            }
        })
    }

    /// The version from `--version` output: yt-dlp prints just the version, deno prints `deno 2.9.7 (...)`.
    fn parse_version(self, stdout: &str) -> Option<String> {
        let line = stdout.lines().next()?.trim();
        let version = match self {
            Tool::YtDlp => line,
            Tool::Deno => line.split_whitespace().nth(1)?,
        };
        (!version.is_empty()).then(|| version.to_owned())
    }
}

/// Where a binary comes from.
struct Spec {
    repo: &'static str,
    asset: &'static str,
    sums: &'static str,
    /// The file inside the asset when the asset is a zip.
    zip_entry: Option<&'static str>,
}

#[derive(Debug, thiserror::Error)]
pub enum BinaryError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("{url} answered {status}")]
    Status {
        url: String,
        status: reqwest::StatusCode,
    },
    #[error("GitHub rate limit reached, next try after unix time {until}")]
    RateLimited { until: u64 },
    #[error("release {tag} has no asset {asset}")]
    MissingAsset { tag: String, asset: String },
    #[error("no checksum listed for {asset}")]
    NoChecksum { asset: String },
    #[error("checksum mismatch for {asset}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        asset: String,
        expected: String,
        actual: String,
    },
    #[error("file error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error(
        "no managed build for the {arch} architecture: install yt-dlp and deno yourself and set their paths under [binaries]"
    )]
    UnsupportedArch { arch: String },
    #[error("smoke run of {path} failed: {reason}")]
    Smoke { path: String, reason: String },
}

/// What one update check did for one binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Checked less than the interval ago, GitHub not asked.
    Skipped,
    Current(String),
    Updated {
        from: Option<String>,
        to: String,
    },
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Outcome::Skipped => write!(f, "checked recently"),
            Outcome::Current(v) => write!(f, "up to date at {v}"),
            Outcome::Updated { from: None, to } => write!(f, "installed {to}"),
            Outcome::Updated {
                from: Some(from),
                to,
            } => write!(f, "updated from {from} to {to}"),
        }
    }
}

/// One binary's line on the Health page.
#[derive(Debug, Clone, Serialize)]
pub struct ToolStatus {
    pub tool: Tool,
    /// From a live `--version` run. None when the binary is missing or broken.
    pub installed: Option<String>,
    /// The latest release tag seen on GitHub.
    pub latest: Option<String>,
    /// Unix time of the last time GitHub was asked.
    pub last_check: Option<u64>,
    pub last_result: Option<String>,
    /// Whether a previous version is kept to roll back to.
    pub can_roll_back: bool,
}

/// Persisted between restarts so the ETag and the once-a-day limit survive.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    yt_dlp: ToolState,
    deno: ToolState,
    retry_after: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ToolState {
    /// The repo the rest was read from. A channel switch starts over.
    repo: String,
    etag: Option<String>,
    latest: Option<Release>,
    last_check: Option<u64>,
    last_result: Option<String>,
}

impl State {
    fn tool(&self, tool: Tool) -> &ToolState {
        match tool {
            Tool::YtDlp => &self.yt_dlp,
            Tool::Deno => &self.deno,
        }
    }

    fn tool_mut(&mut self, tool: Tool) -> &mut ToolState {
        match tool {
            Tool::YtDlp => &mut self.yt_dlp,
            Tool::Deno => &mut self.deno,
        }
    }
}

/// Owns the bin dir. One instance per process, shared behind an `Arc` in `AppState`.
#[derive(Debug)]
pub struct BinaryManager {
    config: Settings,
    client: Client,
    /// Also serialises installs, so a forced check cannot race the daily one.
    state: Mutex<State>,
}

impl BinaryManager {
    pub fn new(config: Settings) -> Result<Self, BinaryError> {
        // The one client that still follows `HTTPS_PROXY` and the like from the environment:
        // GitHub may sit behind a site's own proxy. `[network] proxy` is not used here.
        let client = Client::builder()
            .user_agent(concat!("tubarr/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .build()?;
        let state = std::fs::read(config.bin_dir.join(STATE_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Ok(Self {
            config,
            client,
            state: Mutex::new(state),
        })
    }

    /// The path to run: an explicit one from the config, else the managed copy in the bin dir.
    pub fn path(&self, tool: Tool) -> PathBuf {
        match tool {
            Tool::YtDlp => self.config.yt_dlp_path(),
            Tool::Deno => self.config.deno_path(),
        }
    }

    /// Binaries with an explicit path were installed some other way and are left alone.
    fn managed(&self) -> impl Iterator<Item = Tool> + '_ {
        Tool::ALL.into_iter().filter(|tool| match tool {
            Tool::YtDlp => self.config.yt_dlp.is_none(),
            Tool::Deno => self.config.deno.is_none(),
        })
    }

    /// The architecture name when a managed binary has no build for this machine, for the Health page.
    pub fn unsupported_arch(&self) -> Option<&'static str> {
        let arch = std::env::consts::ARCH;
        self.managed()
            .any(|tool| tool.spec(self.config.yt_dlp_channel, arch).is_err())
            .then_some(arch)
    }

    /// Installs whichever managed binary is missing or does not run. Does nothing for binaries that work.
    pub async fn ensure(&self) -> Result<(), BinaryError> {
        let mut state = self.state.lock().await;
        for tool in self.managed() {
            if self.version(tool).await.is_err() {
                self.update_tool(&mut state, tool, true).await?;
            }
        }
        Ok(())
    }

    /// Checks the managed binaries for a newer release and installs it. Without `force` a binary checked less
    /// than the interval ago is skipped. Use `force` when yt-dlp starts failing on YouTube.
    pub async fn check(&self, force: bool) -> Vec<(Tool, Result<Outcome, BinaryError>)> {
        let mut state = self.state.lock().await;
        let mut results = Vec::new();
        for tool in self.managed() {
            results.push((tool, self.update_tool(&mut state, tool, force).await));
        }
        results
    }

    /// Runs `--version` on both binaries.
    pub async fn smoke(&self) -> Vec<(Tool, Result<String, BinaryError>)> {
        let mut results = Vec::new();
        for tool in Tool::ALL {
            results.push((tool, self.version(tool).await));
        }
        results
    }

    pub async fn status(&self) -> Vec<ToolStatus> {
        let mut out = Vec::new();
        for tool in Tool::ALL {
            let installed = self.version(tool).await.ok();
            let state = self.state.lock().await;
            let ts = state.tool(tool);
            out.push(ToolStatus {
                tool,
                installed,
                latest: ts.latest.as_ref().map(|r| r.tag_name.clone()),
                last_check: ts.last_check,
                last_result: ts.last_result.clone(),
                can_roll_back: install::previous_path(&self.path(tool)).exists(),
            });
        }
        out
    }

    /// Puts the previous yt-dlp back, for when a new release breaks lookups.
    pub async fn rollback(&self, tool: Tool) -> Result<bool, BinaryError> {
        let _guard = self.state.lock().await;
        Ok(install::rollback(&self.path(tool))?)
    }

    /// Background loop: install what is missing, then check on every tick (the interval limits real checks).
    /// Updates and failed updates go out on `events`.
    pub async fn run(self: std::sync::Arc<Self>, events: crate::health::Events) {
        loop {
            if let Err(e) = self.ensure().await {
                tracing::warn!(error = %e, "cannot install yt-dlp or deno");
            }
            let results = self.check(false).await;
            for (tool, result) in &results {
                match result {
                    Ok(Outcome::Skipped) => {}
                    Ok(outcome) => tracing::info!(tool = tool.name(), %outcome, "binary check"),
                    Err(e) => tracing::warn!(tool = tool.name(), error = %e, "binary check failed"),
                }
            }
            events.publish_binary_results(&results);
            tokio::time::sleep(LOOP_TICK).await;
        }
    }

    /// The installed version, from a `--version` run under a timeout.
    pub async fn version(&self, tool: Tool) -> Result<String, BinaryError> {
        let path = self.path(tool);
        let smoke_err = |reason: String| BinaryError::Smoke {
            path: path.display().to_string(),
            reason,
        };
        let output = run_version(&path)
            .await
            .map_err(|e| smoke_err(e.to_string()))?;
        if !output.status.success() {
            return Err(smoke_err(format!("exited with {}", output.status)));
        }
        tool.parse_version(&String::from_utf8_lossy(&output.stdout))
            .ok_or_else(|| smoke_err("printed no version".to_owned()))
    }

    async fn update_tool(
        &self,
        state: &mut State,
        tool: Tool,
        force: bool,
    ) -> Result<Outcome, BinaryError> {
        let result = self.update_tool_inner(state, tool, force).await;
        if let Err(BinaryError::RateLimited { until }) = &result {
            state.retry_after = Some(*until);
        }
        if !matches!(result, Ok(Outcome::Skipped)) {
            state.tool_mut(tool).last_result = Some(match &result {
                Ok(outcome) => outcome.to_string(),
                Err(e) => e.to_string(),
            });
            self.save(state);
        }
        result
    }

    async fn update_tool_inner(
        &self,
        state: &mut State,
        tool: Tool,
        force: bool,
    ) -> Result<Outcome, BinaryError> {
        let spec = tool.spec(self.config.yt_dlp_channel, std::env::consts::ARCH)?;
        if state.tool(tool).repo != spec.repo {
            *state.tool_mut(tool) = ToolState {
                repo: spec.repo.to_owned(),
                ..ToolState::default()
            };
        }
        let installed = self.version(tool).await.ok();
        let now = now_unix();
        let interval = self.config.check_interval.as_secs();
        let ts = state.tool(tool);
        if !force && installed.is_some() && ts.last_check.is_some_and(|t| now < t + interval) {
            return Ok(Outcome::Skipped);
        }
        if let Some(until) = state.retry_after.filter(|&until| now < until) {
            return Err(BinaryError::RateLimited { until });
        }

        let release = self.latest(state.tool_mut(tool), &spec).await?;
        state.tool_mut(tool).last_check = Some(now);
        let to = release.tag_name.trim_start_matches('v').to_owned();
        if let Some(current) = &installed
            && !is_newer(&to, current)
        {
            return Ok(Outcome::Current(current.clone()));
        }

        self.install(&spec, &release, &self.path(tool)).await?;
        if let Err(e) = self.version(tool).await {
            // A release that does not even print its version is worse than the old one.
            let restored = install::rollback(&self.path(tool))?;
            tracing::warn!(tool = tool.name(), error = %e, restored, "new binary failed its smoke run");
            return Err(e);
        }
        tracing::info!(tool = tool.name(), from = ?installed, %to, "binary installed");
        Ok(Outcome::Updated {
            from: installed,
            to,
        })
    }

    /// The latest release, asked with the stored ETag and falling back to the stored release on 304.
    async fn latest(&self, ts: &mut ToolState, spec: &Spec) -> Result<Release, BinaryError> {
        let repo = spec.repo;
        let api = self.config.github_api.as_str();
        // A stored release without our asset (the bin dir moved to another architecture) is no use on a 304.
        let etag = ts
            .latest
            .as_ref()
            .filter(|r| r.asset_url(spec.asset).is_some())
            .and(ts.etag.as_deref());
        match github::latest_release(&self.client, api, repo, etag).await? {
            Latest::Fresh { mut release, etag } => {
                // Only the two assets we use go into the state file, not the dozens of other builds.
                release
                    .assets
                    .retain(|a| a.name == spec.asset || a.name == spec.sums);
                ts.etag = etag;
                ts.latest = Some(release.clone());
                Ok(release)
            }
            // We only send an ETag together with a stored release, so this is a server oddity.
            Latest::NotModified => ts.latest.clone().ok_or(BinaryError::Status {
                url: format!("{}/repos/{repo}/releases/latest", api.trim_end_matches('/')),
                status: reqwest::StatusCode::NOT_MODIFIED,
            }),
        }
    }

    /// Downloads, verifies and swaps in one binary. Leaves no temp files behind, whatever happens.
    async fn install(
        &self,
        spec: &Spec,
        release: &Release,
        target: &Path,
    ) -> Result<(), BinaryError> {
        let missing = |asset: &str| BinaryError::MissingAsset {
            tag: release.tag_name.clone(),
            asset: asset.to_owned(),
        };
        let asset_url = release
            .asset_url(spec.asset)
            .ok_or_else(|| missing(spec.asset))?;
        let sums_url = release
            .asset_url(spec.sums)
            .ok_or_else(|| missing(spec.sums))?;
        let sums = install::fetch_text(&self.client, sums_url).await?;
        let expected =
            install::expected_sha(&sums, spec.asset).ok_or_else(|| BinaryError::NoChecksum {
                asset: spec.asset.to_owned(),
            })?;

        std::fs::create_dir_all(&self.config.bin_dir)?;
        let download = self
            .config
            .bin_dir
            .join(format!(".{}.download", spec.asset));
        let staged = self.config.bin_dir.join(format!(".{}.staged", spec.asset));
        let result = async {
            let actual = install::download(&self.client, asset_url, &download).await?;
            if actual != expected {
                return Err(BinaryError::ChecksumMismatch {
                    asset: spec.asset.to_owned(),
                    expected,
                    actual,
                });
            }
            let file = match spec.zip_entry {
                None => download.clone(),
                Some(entry) => {
                    let (zip, out) = (download.clone(), staged.clone());
                    tokio::task::spawn_blocking(move || {
                        install::extract_zip_entry(&zip, entry, &out)
                    })
                    .await
                    .map_err(std::io::Error::other)??;
                    staged.clone()
                }
            };
            Ok(install::swap_in(&file, target)?)
        }
        .await;
        install::remove_if_exists(&download)?;
        install::remove_if_exists(&staged)?;
        result
    }

    fn save(&self, state: &State) {
        let path = self.config.bin_dir.join(STATE_FILE);
        let tmp = self.config.bin_dir.join(format!(".{STATE_FILE}.tmp"));
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(&self.config.bin_dir)?;
            std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)?;
            std::fs::rename(&tmp, &path)
        };
        if let Err(e) = write() {
            tracing::warn!(error = %e, path = %path.display(), "cannot save binary state");
        }
    }
}

async fn run_version(path: &Path) -> std::io::Result<std::process::Output> {
    let attempt = || async {
        let child = tokio::process::Command::new(path)
            .arg("--version")
            .kill_on_drop(true)
            .output();
        tokio::time::timeout(SMOKE_TIMEOUT, child)
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "timed out"))?
    };
    match attempt().await {
        // A file renamed in a moment ago can still be open for writing in a process forked meanwhile.
        Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
            tokio::time::sleep(Duration::from_millis(100)).await;
            attempt().await
        }
        other => other,
    }
}

/// Compares dotted numeric versions: yt-dlp dates (`2026.08.19`, nightly `2026.10.10.233141`) and deno semver.
fn is_newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-', '+'])
            .map_while(|part| part.parse().ok())
            .collect()
    };
    parse(candidate) > parse(current)
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert!(is_newer("2026.10.10", "2026.08.19"));
        assert!(is_newer("2026.08.19.233141", "2026.08.19"));
        assert!(!is_newer("2026.08.19", "2026.08.19"));
        assert!(!is_newer("2026.08.01", "2026.08.19"));
        assert!(is_newer("v2.10.0", "2.9.7"));
        assert!(!is_newer("2.9.7", "2.9.7"));
    }

    #[test]
    fn parses_version_output() {
        assert_eq!(
            Tool::YtDlp.parse_version("2026.08.19\n").as_deref(),
            Some("2026.08.19")
        );
        assert_eq!(
            Tool::Deno
                .parse_version("deno 2.9.7 (stable, release, x86_64-unknown-linux-gnu)\nv8 14.0\n")
                .as_deref(),
            Some("2.9.7")
        );
        assert_eq!(Tool::Deno.parse_version(""), None);
    }

    #[test]
    fn channel_picks_repo() {
        let repo = |channel| Tool::YtDlp.spec(channel, "x86_64").unwrap().repo;
        assert_eq!(repo(Channel::Stable), "yt-dlp/yt-dlp");
        assert_eq!(repo(Channel::Nightly), "yt-dlp/yt-dlp-nightly-builds");
    }

    #[test]
    fn arch_picks_asset() {
        let pick = |tool: Tool, arch| {
            let spec = tool.spec(Channel::Stable, arch).unwrap();
            (spec.asset, spec.sums)
        };
        assert_eq!(
            pick(Tool::YtDlp, "x86_64"),
            ("yt-dlp_linux", "SHA2-256SUMS")
        );
        assert_eq!(
            pick(Tool::YtDlp, "aarch64"),
            ("yt-dlp_linux_aarch64", "SHA2-256SUMS")
        );
        assert_eq!(
            pick(Tool::Deno, "x86_64"),
            (
                "deno-x86_64-unknown-linux-gnu.zip",
                "deno-x86_64-unknown-linux-gnu.zip.sha256sum"
            )
        );
        assert_eq!(
            pick(Tool::Deno, "aarch64"),
            (
                "deno-aarch64-unknown-linux-gnu.zip",
                "deno-aarch64-unknown-linux-gnu.zip.sha256sum"
            )
        );
        // The nightly channel ships the same asset names.
        assert_eq!(
            Tool::YtDlp.spec(Channel::Nightly, "aarch64").unwrap().asset,
            "yt-dlp_linux_aarch64"
        );
    }

    #[test]
    fn unsupported_arch_is_an_error() {
        for tool in Tool::ALL {
            let err = tool.spec(Channel::Stable, "riscv64").err().unwrap();
            assert!(matches!(err, BinaryError::UnsupportedArch { ref arch } if arch == "riscv64"));
            assert!(err.to_string().contains("riscv64"), "{err}");
        }
    }

    #[test]
    fn both_arches_verify_against_published_sums() {
        // The two checksum file formats as published: yt-dlp's SHA2-256SUMS lists every build, deno's
        // per-asset file names just its own zip.
        let yt = format!(
            "{}  yt-dlp_linux\n{}  yt-dlp_linux_aarch64\n",
            "a".repeat(64),
            "b".repeat(64)
        );
        assert_eq!(
            install::expected_sha(&yt, "yt-dlp_linux_aarch64"),
            Some("b".repeat(64))
        );
        assert_eq!(
            install::expected_sha(&yt, "yt-dlp_linux"),
            Some("a".repeat(64))
        );
        let deno = format!("{}  deno-aarch64-unknown-linux-gnu.zip\n", "c".repeat(64));
        let spec = Tool::Deno.spec(Channel::Stable, "aarch64").unwrap();
        assert_eq!(
            install::expected_sha(&deno, spec.asset),
            Some("c".repeat(64))
        );
    }
}
