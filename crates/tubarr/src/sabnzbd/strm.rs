//! Writing a grab's files into the download folder.

use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::Arc,
};

use url::Url;

use crate::{
    AppState,
    config::Subtitles,
    play::parts::{self, PartsError},
    resolver::Resolver,
    store::Db,
    subtitles,
};

/// One grab to write.
#[derive(Debug, Clone, Copy)]
pub struct StrmJob<'a> {
    pub release_name: &'a str,
    /// The video id, or a split upload's part ids joined by commas in play order.
    pub video_id: &'a str,
    /// The category's folder under `complete_dir`, empty for the default category.
    pub category_dir: &'a str,
}

#[derive(Debug, thiserror::Error)]
#[error("cannot write {path}: {source}")]
pub struct StrmError {
    pub path: String,
    pub source: std::io::Error,
}

/// Writes a grab's files and returns the folder holding them, which Sonarr and Radarr import from.
pub trait StrmWriter {
    fn write(&self, job: StrmJob<'_>) -> impl Future<Output = Result<PathBuf, StrmError>> + Send;
}

/// Writes `<complete_dir>/<category>/<release>/` with `<release>.strm` holding the play link and
/// `<release>.<lang>.srt` for each wanted caption that arrives within `subtitles.timeout`.
///
/// A release joined from parts gets no captions: shifting each part's cues by the parts before it
/// would need every part's exact played length within the same short budget, and a cue that drifts
/// is worse than none.
#[derive(Debug, Clone)]
pub struct GrabWriter {
    complete_dir: PathBuf,
    public_url: Url,
    subtitles: Subtitles,
    resolver: Arc<Resolver<Db>>,
    client: reqwest::Client,
}

impl GrabWriter {
    pub fn new(state: &AppState) -> Self {
        Self {
            complete_dir: state.config.complete_dir.clone(),
            public_url: state.config.public_url.clone(),
            subtitles: state.config.subtitles.clone(),
            resolver: state.resolver.clone(),
            client: state.play.client().clone(),
        }
    }

    /// The URL Jellyfin plays, `<public_url>play/<video id>` or `<public_url>play/<part>,<part>`.
    fn play_url(&self, ids: &[String]) -> Result<Url, PartsError> {
        parts::play_url(&self.public_url, ids)
    }
}

impl StrmWriter for GrabWriter {
    async fn write(&self, job: StrmJob<'_>) -> Result<PathBuf, StrmError> {
        let folder = self
            .complete_dir
            .join(job.category_dir)
            .join(job.release_name);
        tokio::fs::create_dir_all(&folder)
            .await
            .map_err(|source| error(&folder, source))?;
        let strm = folder.join(format!("{}.strm", job.release_name));
        // The NZB parser already checked the ids, so this only fails on a broken caller.
        let url =
            parts::parse(job.video_id).and_then(|ids| self.play_url(&ids).map(|url| (ids, url)));
        let (ids, url) = url.map_err(|e| {
            error(
                &strm,
                std::io::Error::new(std::io::ErrorKind::InvalidInput, e),
            )
        })?;
        write_atomic(&strm, format!("{url}\n").as_bytes()).await?;

        let captions = match ids.as_slice() {
            [one] => subtitles::fetch(&self.resolver, &self.client, &self.subtitles, one).await,
            _ => {
                tracing::info!(parts = job.video_id, "joined release, writing no captions");
                Vec::new()
            }
        };
        for caption in captions {
            let file = folder.join(format!("{}.{}.srt", job.release_name, caption.lang));
            // The .strm is the grab. A caption that cannot be written is only a missing extra.
            match write_atomic(&file, caption.srt.as_bytes()).await {
                Ok(()) => tracing::info!(file = %file.display(), "caption written"),
                Err(e) => tracing::warn!(error = %e, "caption not written"),
            }
        }
        Ok(folder)
    }
}

/// Writes to a hidden temporary file next to `path`, then renames it into place, so nobody ever
/// sees half a file.
pub(crate) async fn write_atomic(path: &Path, contents: &[u8]) -> Result<(), StrmError> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.{:016x}.tmp", rand::random::<u64>()));
    if let Err(source) = tokio::fs::write(&temp, contents).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error(path, source));
    }
    if let Err(source) = tokio::fs::rename(&temp, path).await {
        let _ = tokio::fs::remove_file(&temp).await;
        return Err(error(path, source));
    }
    Ok(())
}

fn error(path: &Path, source: std::io::Error) -> StrmError {
    StrmError {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[tokio::test]
    async fn atomic_write_replaces_and_leaves_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Show.S01E01.strm");
        write_atomic(&file, b"old\n").await.unwrap();
        write_atomic(&file, b"new\n").await.unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\n");
        assert_eq!(names(dir.path()), ["Show.S01E01.strm"]);
    }

    #[tokio::test]
    async fn failed_atomic_write_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        // A directory in the way makes the rename fail after the temp file was written.
        let blocked = dir.path().join("Show.S01E01.ur.srt");
        std::fs::create_dir(blocked.join("")).unwrap();
        std::fs::write(blocked.join("inside"), "x").unwrap();
        let err = write_atomic(&blocked, b"1\n").await.unwrap_err();
        assert!(err.path.ends_with("Show.S01E01.ur.srt"), "{err}");
        assert_eq!(names(dir.path()), ["Show.S01E01.ur.srt"]);

        let missing = dir.path().join("gone/Show.strm");
        write_atomic(&missing, b"x").await.unwrap_err();
        assert!(!dir.path().join("gone").exists());
    }
}
