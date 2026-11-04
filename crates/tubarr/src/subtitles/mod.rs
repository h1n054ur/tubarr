//! Captions for a grab: which tracks of a lookup to take, fetching them within a time budget and
//! turning them into SRT.
//!
//! Captions are best effort. A failed lookup or download is logged and skipped, never a failed
//! grab, and nothing waits past `subtitles.timeout`: Sonarr and Radarr import the folder as soon
//! as the grab reports completed, so whatever is not in by then would never be picked up anyway.

mod vtt;

use std::sync::Arc;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use tokio::time::{Instant, timeout_at};

pub use vtt::to_srt;

use crate::{
    config::Subtitles,
    resolver::{Resolver, StreamFormat, Subtitle},
    store::Db,
};

/// A caption file is a few hundred KB at most. Anything far bigger is not a caption track.
const MAX_CAPTION_BYTES: usize = 8 * 1024 * 1024;

/// One converted caption track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caption {
    /// ISO 639-1, as it goes into `<release>.<lang>.srt`.
    pub lang: String,
    pub srt: String,
}

#[derive(Debug, thiserror::Error)]
enum FetchError {
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("larger than {MAX_CAPTION_BYTES} bytes")]
    TooLarge,
}

/// The ISO 639-1 part of a YouTube caption key: `en-GB` and `ur-orig` give `en` and `ur`.
fn base_language(key: &str) -> String {
    key.split('-').next().unwrap_or(key).to_ascii_lowercase()
}

/// YouTube's machine translations of a track carry `tlang=` in their URL. The speech-to-text track
/// in the spoken language is the one without it (yt-dlp also lists it as `<lang>-orig`).
fn is_translated(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| u.query_pairs().any(|(k, _)| k == "tlang"))
}

/// At most one track per configured language, in the configured order. Uploaded subtitles win.
/// Speech-to-text only fills in when `automatic` is on, and only untranslated. Only WebVTT is
/// taken: YouTube offers it for every track and the resolver prefers it.
pub fn select<'a>(tracks: &'a [Subtitle], config: &Subtitles) -> Vec<(String, &'a Subtitle)> {
    let find = |lang: &str, automatic: bool| tracks.iter().find(|t| fits(t, lang, automatic));
    config
        .languages
        .iter()
        .filter_map(|lang| {
            let track = find(lang, false).or_else(|| {
                if config.automatic {
                    find(lang, true)
                } else {
                    None
                }
            })?;
            Some((lang.clone(), track))
        })
        .collect()
}

fn fits(track: &Subtitle, lang: &str, automatic: bool) -> bool {
    track.automatic == automatic
        && track.ext == "vtt"
        && base_language(&track.lang) == lang
        && !(automatic && is_translated(&track.url))
}

/// Looks `video_id` up (cached and rate limited like every lookup) and fetches the captions
/// `config` asks for, all within `config.timeout`. Tracks that fail or come too late are left out.
pub async fn fetch(
    resolver: &Arc<Resolver<Db>>,
    client: &reqwest::Client,
    config: &Subtitles,
    video_id: &str,
) -> Vec<Caption> {
    if config.languages.is_empty() {
        return Vec::new();
    }
    let deadline = Instant::now() + config.timeout;
    // Its own task, so a lookup still running at the deadline finishes and fills the cache for
    // the first play instead of being killed with the grab's budget.
    let lookup = tokio::spawn({
        let resolver = resolver.clone();
        let video_id = video_id.to_owned();
        async move { resolver.resolve(&video_id).await }
    });
    let resolved = match timeout_at(deadline, lookup).await {
        Ok(Ok(Ok(resolved))) => resolved,
        Ok(Ok(Err(e))) => {
            tracing::warn!(video_id, error = %e, "lookup failed, writing no captions");
            return Vec::new();
        }
        Ok(Err(e)) => {
            tracing::warn!(video_id, error = %e, "lookup task failed, writing no captions");
            return Vec::new();
        }
        Err(_) => {
            tracing::warn!(video_id, timeout = ?config.timeout, "lookup too slow, writing no captions");
            return Vec::new();
        }
    };
    let chosen = select(&resolved.subtitles, config);
    if chosen.is_empty() {
        tracing::info!(video_id, languages = ?config.languages, "no captions in the wanted languages");
        return Vec::new();
    }
    let headers = request_headers(&resolved.video);
    let downloads = chosen.into_iter().map(|(lang, track)| {
        let headers = headers.clone();
        async move {
            match timeout_at(deadline, download(client, &track.url, headers)).await {
                Ok(Ok(text)) => {
                    let srt = to_srt(&text, track.automatic);
                    if srt.is_empty() {
                        tracing::warn!(video_id, lang, "caption track has no cues, skipped");
                        return None;
                    }
                    tracing::debug!(video_id, lang, automatic = track.automatic, "caption fetched");
                    Some(Caption { lang, srt })
                }
                Ok(Err(e)) => {
                    tracing::warn!(video_id, lang, error = %e, "caption download failed, skipped");
                    None
                }
                Err(_) => {
                    tracing::warn!(video_id, lang, timeout = ?config.timeout, "caption too slow, skipped");
                    None
                }
            }
        }
    });
    futures_util::future::join_all(downloads)
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// The headers yt-dlp says YouTube expects (User-Agent and friends), from the video format.
fn request_headers(format: &StreamFormat) -> HeaderMap {
    format
        .http_headers
        .iter()
        .filter_map(|(name, value)| {
            Some((
                HeaderName::from_bytes(name.as_bytes()).ok()?,
                HeaderValue::from_str(value).ok()?,
            ))
        })
        .collect()
}

async fn download(
    client: &reqwest::Client,
    url: &str,
    headers: HeaderMap,
) -> Result<String, FetchError> {
    let mut response = client
        .get(url)
        .headers(headers)
        .send()
        .await?
        .error_for_status()?;
    if response
        .content_length()
        .is_some_and(|n| n > MAX_CAPTION_BYTES as u64)
    {
        return Err(FetchError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_CAPTION_BYTES {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(lang: &str, automatic: bool, ext: &str, url: &str) -> Subtitle {
        Subtitle {
            lang: lang.to_owned(),
            automatic,
            ext: ext.to_owned(),
            url: url.to_owned(),
        }
    }

    fn config(languages: &[&str], automatic: bool) -> Subtitles {
        Subtitles {
            languages: languages.iter().map(|l| (*l).to_owned()).collect(),
            automatic,
            ..Subtitles::default()
        }
    }

    const TT: &str = "https://www.youtube.com/api/timedtext?v=x&fmt=vtt";

    /// The lists the resolver makes from a video with uploaded English, Urdu speech-to-text and
    /// YouTube's translations of both.
    fn tracks() -> Vec<Subtitle> {
        vec![
            track("en-GB", false, "vtt", &format!("{TT}&lang=en-GB")),
            track("fr", false, "srv3", &format!("{TT}&lang=fr")),
            track("en", true, "vtt", &format!("{TT}&lang=ur&tlang=en")),
            track("hi", true, "vtt", &format!("{TT}&lang=ur&tlang=hi")),
            track("ur", true, "vtt", &format!("{TT}&lang=ur&kind=asr")),
            track("ur-orig", true, "vtt", &format!("{TT}&lang=ur&kind=asr")),
        ]
    }

    fn picked(tracks: &[Subtitle], config: &Subtitles) -> Vec<(String, String, bool)> {
        select(tracks, config)
            .into_iter()
            .map(|(lang, t)| (lang, t.lang.clone(), t.automatic))
            .collect()
    }

    #[test]
    fn uploaded_first_then_untranslated_speech_to_text() {
        let tracks = tracks();
        assert_eq!(
            picked(&tracks, &config(&["en", "ur", "hi"], true)),
            [
                ("en".to_owned(), "en-GB".to_owned(), false),
                ("ur".to_owned(), "ur".to_owned(), true),
            ]
        );
    }

    #[test]
    fn speech_to_text_only_when_allowed() {
        let tracks = tracks();
        assert_eq!(
            picked(&tracks, &config(&["ur", "en"], false)),
            [("en".to_owned(), "en-GB".to_owned(), false)]
        );
    }

    #[test]
    fn order_follows_the_config_and_other_formats_are_skipped() {
        let tracks = tracks();
        assert_eq!(
            picked(&tracks, &config(&["ur", "fr", "en"], true)),
            [
                ("ur".to_owned(), "ur".to_owned(), true),
                ("en".to_owned(), "en-GB".to_owned(), false),
            ]
        );
        assert!(picked(&tracks, &config(&[], true)).is_empty());
    }

    #[test]
    fn the_orig_track_counts_as_the_spoken_language() {
        let tracks = vec![track(
            "ur-orig",
            true,
            "vtt",
            &format!("{TT}&lang=ur&kind=asr"),
        )];
        assert_eq!(
            picked(&tracks, &config(&["ur"], true)),
            [("ur".to_owned(), "ur-orig".to_owned(), true)]
        );
    }

    #[test]
    fn keys_reduce_to_their_language() {
        assert_eq!(base_language("en-GB"), "en");
        assert_eq!(base_language("ur-orig"), "ur");
        assert_eq!(base_language("HI"), "hi");
        assert!(is_translated(&format!("{TT}&tlang=en")));
        assert!(!is_translated(TT));
        assert!(!is_translated("not a url"));
    }
}
