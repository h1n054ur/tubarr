//! The audio only files in the media library: where Sonarr's paths are in Tubarr's container, and
//! writing or removing the `<episode> - Audio only.strm` next to an episode's `.strm`.

use std::path::{Component, Path, PathBuf};

use url::Url;

use super::{HookError, thumbs};
use crate::{play::parts, sabnzbd::strm::write_atomic};

/// What Jellyfin reads as the version label: `<episode> - Audio only.strm`.
pub const LABEL: &str = " - Audio only";

/// A `.strm` holds one URL. Anything bigger is not one of Tubarr's.
const MAX_STRM_BYTES: u64 = 4096;

/// What [`Library::settle`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Written,
    Removed,
    /// The audio only file was already Tubarr's and up to date.
    Kept,
    Unchanged,
}

/// The library as Tubarr sees it.
#[derive(Debug, Clone, Copy)]
pub struct Library<'a> {
    /// `[Sonarr prefix, Tubarr prefix]` pairs.
    pub paths: &'a [(PathBuf, PathBuf)],
    /// The base every Tubarr play URL starts with.
    pub public_url: &'a Url,
}

impl Library<'_> {
    /// Where the file Sonarr calls `path` is in Tubarr's container. Refuses a relative path, one
    /// with `..` or `.` in it, and one under none of the mapped prefixes.
    pub fn local(&self, path: &str) -> Result<PathBuf, HookError> {
        let path = Path::new(path);
        let plain = path.is_absolute()
            && path
                .components()
                .all(|c| matches!(c, Component::RootDir | Component::Normal(_)));
        if plain {
            for (sonarr, tubarr) in self.paths {
                if let Ok(rest) = path.strip_prefix(sonarr) {
                    return Ok(tubarr.join(rest));
                }
            }
        }
        Err(HookError::OutsideLibrary(path.display().to_string()))
    }

    /// Brings the audio only file of episode file `main` in step: written when `on` and `main` is
    /// a Tubarr `.strm` that exists, removed otherwise. Only a file that is Tubarr's own audio
    /// link is ever removed or overwritten.
    pub async fn settle(&self, main: &Path, on: bool) -> Result<Change, HookError> {
        let Some(audio) = audio_path(main) else {
            return Ok(Change::Unchanged);
        };
        let ids = match read_link(main).await? {
            Some(link) if on => parts::ids_in(self.public_url, &link),
            _ => None,
        };
        let current = read_link(&audio).await?;
        let ours = current.as_deref().is_some_and(|l| self.is_audio_link(l));
        match ids {
            Some(ids) => {
                let url = parts::audio_url(self.public_url, &ids)
                    .map_err(|e| HookError::Io(main.display().to_string(), e.to_string()))?;
                if current.as_deref() == Some(url.as_str()) {
                    return Ok(Change::Kept);
                }
                if current.is_some() && !ours {
                    tracing::warn!(file = %audio.display(), "not Tubarr's, left alone");
                    return Ok(Change::Unchanged);
                }
                write_atomic(&audio, format!("{url}\n").as_bytes())
                    .await
                    .map_err(|e| HookError::Io(e.path, e.source.to_string()))?;
                tracing::info!(file = %audio.display(), "audio only version written");
                Ok(Change::Written)
            }
            None if ours => {
                remove(&audio).await?;
                tracing::info!(file = %audio.display(), "audio only version removed");
                Ok(Change::Removed)
            }
            None => Ok(Change::Unchanged),
        }
    }

    /// Removes every audio only file of Tubarr's under `folder` (a series folder and its season
    /// folders), and every episode thumbnail whose `.strm` is gone. A missing folder has none.
    /// Returns how many audio only files went.
    pub async fn clear(&self, folder: &Path) -> Result<usize, HookError> {
        let mut removed = 0;
        let mut stack = vec![(folder.to_path_buf(), 0)];
        while let Some((dir, depth)) = stack.pop() {
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(e) => e,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io(&dir, &e)),
            };
            while let Some(entry) = entries.next_entry().await.map_err(|e| io(&dir, &e))? {
                let path = entry.path();
                let kind = entry.file_type().await.map_err(|e| io(&path, &e))?;
                if kind.is_dir() && depth < 2 {
                    stack.push((path, depth + 1));
                } else if kind.is_file() && is_orphan_thumb(&path).await {
                    remove(&path).await?;
                    tracing::info!(file = %path.display(), "episode thumbnail removed");
                } else if kind.is_file()
                    && is_audio_path(&path)
                    && read_link(&path)
                        .await?
                        .is_some_and(|l| self.is_audio_link(&l))
                {
                    remove(&path).await?;
                    tracing::info!(file = %path.display(), "audio only version removed");
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    fn is_audio_link(&self, link: &str) -> bool {
        link.strip_suffix("/audio")
            .is_some_and(|play| parts::ids_in(self.public_url, play).is_some())
    }
}

/// `<stem> - Audio only.strm` next to `<stem>.strm`. `None` for any other file, an audio only file
/// itself included.
pub fn audio_path(main: &Path) -> Option<PathBuf> {
    let name = main.file_name()?.to_str()?;
    let stem = name.strip_suffix(".strm")?;
    if stem.is_empty() || stem.ends_with(LABEL) {
        return None;
    }
    Some(main.with_file_name(format!("{stem}{LABEL}.strm")))
}

/// `<stem>-thumb.jpg` with no `<stem>.strm` next to it.
async fn is_orphan_thumb(path: &Path) -> bool {
    let Some(stem) = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(thumbs::SUFFIX))
    else {
        return false;
    };
    !tokio::fs::try_exists(path.with_file_name(format!("{stem}.strm")))
        .await
        .unwrap_or(true)
}

fn is_audio_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(&format!("{LABEL}.strm")))
}

/// The one line of a small `.strm`. `None` when the file is missing, larger than a link or not
/// text.
pub(super) async fn read_link(path: &Path) -> Result<Option<String>, HookError> {
    match tokio::fs::metadata(path).await {
        Ok(m) if m.is_file() && m.len() <= MAX_STRM_BYTES => {}
        Ok(_) => return Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(path, &e)),
    }
    match tokio::fs::read(path).await {
        Ok(bytes) => Ok(String::from_utf8(bytes).ok().map(|s| s.trim().to_owned())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(path, &e)),
    }
}

async fn remove(path: &Path) -> Result<(), HookError> {
    match tokio::fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io(path, &e)),
    }
}

fn io(path: &Path, e: &std::io::Error) -> HookError {
    HookError::Io(path.display().to_string(), e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_path_sits_next_to_the_episode() {
        assert_eq!(
            audio_path(Path::new("/m/Show/Season 1/Show - S01E02 - Two.strm")).unwrap(),
            Path::new("/m/Show/Season 1/Show - S01E02 - Two - Audio only.strm")
        );
        assert_eq!(audio_path(Path::new("/m/Show/a - Audio only.strm")), None);
        assert_eq!(audio_path(Path::new("/m/Show/a.mkv")), None);
        assert_eq!(audio_path(Path::new("/m/Show/.strm")), None);
    }

    #[test]
    fn sonarr_paths_map_onto_the_library_or_are_refused() {
        let paths = vec![(PathBuf::from("/mnt/fk/data/media"), PathBuf::from("/media"))];
        let url = Url::parse("http://tubarr:8480/").unwrap();
        let lib = Library {
            paths: &paths,
            public_url: &url,
        };
        assert_eq!(
            lib.local("/mnt/fk/data/media/pk-dramas/A/Season 1/A.strm")
                .unwrap(),
            Path::new("/media/pk-dramas/A/Season 1/A.strm")
        );
        for bad in [
            "/mnt/fk/data/media2/A.strm",
            "/mnt/fk/data/media/../etc/passwd",
            "relative/A.strm",
            "/etc/passwd",
        ] {
            assert!(lib.local(bad).is_err(), "{bad}");
        }
    }
}
