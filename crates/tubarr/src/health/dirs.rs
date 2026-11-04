//! The folders Tubarr writes to. A folder Docker created as root leaves the container user unable
//! to write there, and every grab or play build then fails. Checked once at start-up with a warning
//! in the log and again on each health report, so the reason clears once the folder is fixed.

use std::path::{Path, PathBuf};

use crate::config::Config;

/// A folder Tubarr writes to and what stops working without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritableDir {
    /// The config key that sets it.
    pub key: &'static str,
    pub path: PathBuf,
    /// Whether losing it stops lookups or grabs (failing) or only part of the work (degraded).
    pub critical: bool,
}

/// The folders a running Tubarr writes to. `data_dir` is left out: the database cannot open
/// without it, so start-up already stops with an error.
pub fn writable_dirs(config: &Config) -> Vec<WritableDir> {
    let mut dirs = vec![
        WritableDir {
            key: "complete_dir",
            path: config.complete_dir.clone(),
            critical: true,
        },
        WritableDir {
            key: "play.index_dir",
            path: config.play_index_dir(),
            critical: false,
        },
    ];
    if config.binaries.yt_dlp.is_none() || config.binaries.deno.is_none() {
        dirs.push(WritableDir {
            key: "binaries.bin_dir",
            path: config.binaries.bin_dir.clone(),
            critical: false,
        });
    }
    dirs
}

/// Creates the folder when it is missing (writes would create it too), then creates and removes a
/// file in it.
pub fn probe(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let file = dir.join(format!(
        ".tubarr-write-check.{:016x}",
        rand::random::<u64>()
    ));
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)?;
    std::fs::remove_file(&file)
}

/// The user and group the process runs as, as `chown` takes them.
fn owner() -> String {
    format!("{}:{}", nix::unistd::geteuid(), nix::unistd::getegid())
}

/// The health reason for a folder that failed [`probe`].
pub fn reason(dir: &WritableDir, error: &std::io::Error) -> String {
    format!(
        "cannot write to {} ({}) as {}: {error}",
        dir.path.display(),
        dir.key,
        owner()
    )
}

/// Each folder in [`writable_dirs`] that fails [`probe`], with the error.
pub fn check(config: &Config) -> Vec<(WritableDir, std::io::Error)> {
    writable_dirs(config)
        .into_iter()
        .filter_map(|dir| probe(&dir.path).err().map(|error| (dir, error)))
        .collect()
}

/// The start-up check: one warning per folder Tubarr cannot write to, with the fix.
pub fn warn_unwritable(config: &Config) {
    for (dir, error) in check(config) {
        let owner = owner();
        let fix = if error.kind() == std::io::ErrorKind::PermissionDenied {
            format!("give the host folder mounted there to {owner} (chown -R {owner})")
        } else {
            "check that the path is a folder Tubarr can create".to_owned()
        };
        tracing::warn!(
            path = %dir.path.display(),
            key = dir.key,
            user = %owner,
            error = %error,
            "Tubarr cannot write to this folder, so the work that needs it fails. Fix: {fix}"
        );
    }
}
