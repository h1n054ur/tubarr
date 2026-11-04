//! Episode images: the YouTube thumbnail of the video an episode `.strm` plays, saved next to it
//! as `<episode>-thumb.jpg`, the name Jellyfin reads as a local episode image. For podcasts, whose
//! episodes TheTVDB rarely has stills for.
//!
//! A thumbnail is an extra: one that cannot be fetched or written is logged and tried again the
//! next time the file is settled (a later import, a rename, the backfill), never a failed import.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use url::Url;

use super::library::{LABEL, Library, read_link};
use crate::{
    network::{self, OutboundProxy},
    play::parts,
    sabnzbd::strm::write_atomic,
};

/// The image Jellyfin takes for `<episode>.strm`.
pub const SUFFIX: &str = "-thumb.jpg";

/// YouTube's own thumbnails, largest first. Not every video has `maxresdefault`.
const SIZES: [&str; 2] = ["maxresdefault.jpg", "hqdefault.jpg"];

/// Anything bigger is not a thumbnail.
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// What [`Thumbs::settle`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbChange {
    Written,
    /// The episode file is gone, so its thumbnail went too.
    Removed,
    /// Already there.
    Kept,
    /// Fetching or writing it failed, logged. A later settle tries again.
    Failed,
    Unchanged,
}

/// Fetches thumbnails from YouTube's image host.
#[derive(Debug, Clone)]
pub struct Thumbs {
    http: reqwest::Client,
    base: Url,
}

impl Thumbs {
    /// `base` is `[hooks] thumbnail_url`, `https://i.ytimg.com/` outside tests. `proxy` is
    /// `[network] proxy`.
    pub fn new(base: Url, proxy: Option<&OutboundProxy>) -> Result<Self, reqwest::Error> {
        let builder = reqwest::Client::builder().timeout(Duration::from_secs(20));
        let http = network::youtube_client(builder, proxy)?.build()?;
        Ok(Self { http, base })
    }

    /// Brings the thumbnail of episode file `main` in step: removed when `main` is gone, fetched
    /// when `on`, `main` is a Tubarr `.strm` and there is no thumbnail yet. A thumbnail in place is
    /// never fetched again, and turning the option off leaves it.
    pub async fn settle(&self, library: &Library<'_>, main: &Path, on: bool) -> ThumbChange {
        let Some(thumb) = thumb_path(main) else {
            return ThumbChange::Unchanged;
        };
        if !exists(main).await {
            if !exists(&thumb).await {
                return ThumbChange::Unchanged;
            }
            return match tokio::fs::remove_file(&thumb).await {
                Ok(()) => {
                    tracing::info!(file = %thumb.display(), "episode thumbnail removed");
                    ThumbChange::Removed
                }
                Err(e) => {
                    tracing::warn!(file = %thumb.display(), error = %e, "episode thumbnail not removed");
                    ThumbChange::Failed
                }
            };
        }
        if !on || exists(&thumb).await {
            return if on {
                ThumbChange::Kept
            } else {
                ThumbChange::Unchanged
            };
        }
        let ids = match read_link(main).await {
            Ok(Some(link)) => parts::ids_in(library.public_url, &link),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(error = %e, "episode file not read for its thumbnail");
                return ThumbChange::Failed;
            }
        };
        // Joined parts show the first part's thumbnail.
        let Some(id) = ids.and_then(|ids| ids.into_iter().next()) else {
            return ThumbChange::Unchanged;
        };
        let Some(image) = self.fetch(&id).await else {
            return ThumbChange::Failed;
        };
        match write_atomic(&thumb, &image).await {
            Ok(()) => {
                tracing::info!(file = %thumb.display(), video = id, "episode thumbnail written");
                ThumbChange::Written
            }
            Err(e) => {
                tracing::warn!(error = %e, "episode thumbnail not written");
                ThumbChange::Failed
            }
        }
    }

    /// The largest thumbnail YouTube has for `id`, `None` (logged) when none could be fetched.
    async fn fetch(&self, id: &str) -> Option<Vec<u8>> {
        for size in SIZES {
            let url = match self.base.join(&format!("vi/{id}/{size}")) {
                Ok(url) => url,
                Err(e) => {
                    tracing::warn!(video = id, error = %e, "no thumbnail URL");
                    return None;
                }
            };
            match self.get(url).await {
                Ok(image) => return Some(image),
                Err(e) => {
                    tracing::debug!(video = id, size, error = %e, "thumbnail size not fetched")
                }
            }
        }
        tracing::warn!(
            video = id,
            "episode thumbnail not fetched, tried again on a later pass"
        );
        None
    }

    async fn get(&self, url: Url) -> Result<Vec<u8>, String> {
        let res = self.http.get(url).send().await.map_err(|e| e.to_string())?;
        let status = res.status();
        if !status.is_success() {
            return Err(format!("HTTP {status}"));
        }
        if res.content_length().is_some_and(|n| n > MAX_BYTES as u64) {
            return Err("too large".to_owned());
        }
        let body = res.bytes().await.map_err(|e| e.to_string())?;
        if body.is_empty() || body.len() > MAX_BYTES {
            return Err(format!("{} bytes", body.len()));
        }
        Ok(body.to_vec())
    }
}

/// `<stem>-thumb.jpg` next to `<stem>.strm`. `None` for any other file, an audio only one
/// included, so the merged versions of an episode share the main file's image.
pub fn thumb_path(main: &Path) -> Option<PathBuf> {
    let name = main.file_name()?.to_str()?;
    let stem = name.strip_suffix(".strm")?;
    if stem.is_empty() || stem.ends_with(LABEL) {
        return None;
    }
    Some(main.with_file_name(format!("{stem}{SUFFIX}")))
}

/// Moves the thumbnail of a renamed episode file along with it, so it is not fetched again.
/// Leaves it when the new name already has one or the new file is not there.
pub async fn follow_rename(old: &Path, new: &Path) {
    let (Some(from), Some(to)) = (thumb_path(old), thumb_path(new)) else {
        return;
    };
    if from == to || !exists(&from).await || exists(&to).await || !exists(new).await {
        return;
    }
    match tokio::fs::rename(&from, &to).await {
        Ok(()) => tracing::info!(file = %to.display(), "episode thumbnail moved"),
        Err(e) => tracing::warn!(file = %from.display(), error = %e, "episode thumbnail not moved"),
    }
}

/// When it cannot be told, a file counts as there: nothing is removed or fetched on a guess.
async fn exists(path: &Path) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thumbnail_is_named_after_the_main_file() {
        assert_eq!(
            thumb_path(Path::new("/m/Show/Season 1/Show - S01E02 - Two.strm")).unwrap(),
            Path::new("/m/Show/Season 1/Show - S01E02 - Two-thumb.jpg")
        );
        assert_eq!(thumb_path(Path::new("/m/Show/a - Audio only.strm")), None);
        assert_eq!(thumb_path(Path::new("/m/Show/a.mkv")), None);
        assert_eq!(thumb_path(Path::new("/m/Show/.strm")), None);
    }

    #[tokio::test]
    async fn thumbnails_go_through_the_outbound_proxy() {
        use std::sync::{Arc, Mutex};

        // A plain HTTP proxy that records each absolute URL and answers it itself, so the
        // `.test` host is reachable only through it.
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = seen.clone();
        let router = axum::Router::new().fallback(move |uri: axum::http::Uri| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push(uri.to_string());
                "jpeg bytes"
            }
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let proxy = OutboundProxy::parse(&format!("http://u:p@{addr}")).unwrap();
        let thumbs = Thumbs::new(Url::parse("http://ytimg.test/").unwrap(), Some(&proxy)).unwrap();
        assert_eq!(thumbs.fetch("0eD-GQCutAU").await.unwrap(), b"jpeg bytes");
        assert_eq!(
            *seen.lock().unwrap(),
            vec!["http://ytimg.test/vi/0eD-GQCutAU/maxresdefault.jpg".to_owned()]
        );
    }
}
