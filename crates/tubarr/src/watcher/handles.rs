//! Turning what a person types for a channel into the `UC...` id Tubarr stores: the id itself, an
//! `@handle`, a channel link (`/channel/UC...`, `/@handle`, `/c/name`, `/user/name`) or a video
//! link, which stands for the video's uploader.
//!
//! An id or a `/channel/` link is read as typed. Anything else costs one plain GET of the YouTube
//! page it names, which carries the channel id in its markup: no yt-dlp run, but it still takes a
//! token from the shared lookup budget, in the play lane since a person is waiting on the save.
//! Answers are kept for a few minutes, so saving the same form twice asks YouTube once.

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

use regex::Regex;
use reqwest::{Client, StatusCode, header};
use tokio::time::Instant;
use url::Url;

use super::is_channel_id;
use crate::{
    network::{self, OutboundProxy},
    ratelimit::{Lane, RateLimiter},
    resolver::is_video_id,
};

/// How long a resolved page is kept.
const CACHE_FOR: Duration = Duration::from_secs(10 * 60);

/// What one line of input turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelInput {
    /// A channel id, as typed or read off a `/channel/` link.
    Id(String),
    /// A page to read the id from: a handle or custom URL, or a video's watch page.
    Page(Page),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Page {
    /// `/@handle`, `/c/name` or `/user/name`, as a path on the YouTube site.
    Channel(String),
    /// A video id: the channel is its uploader.
    Video(String),
}

impl Page {
    fn path_and_query(&self) -> (String, Option<String>) {
        match self {
            Self::Channel(path) => (path.clone(), None),
            Self::Video(id) => ("/watch".to_owned(), Some(format!("v={id}"))),
        }
    }

    /// The cache key. Handles and custom names are case insensitive on YouTube.
    fn key(&self) -> String {
        match self {
            Self::Channel(path) => path.to_lowercase(),
            Self::Video(id) => format!("v={id}"),
        }
    }
}

/// A resolved channel. `title` is the channel's name when the page gave one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub channel_id: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LookupError {
    #[error("{0:?} is not a channel id (UC...), an @handle or a YouTube channel or video link")]
    Invalid(String),
    #[error("YouTube has no channel at {0}")]
    NotFound(String),
    #[error("{0}: the page names no channel, it may be removed or private")]
    NoChannel(String),
    #[error("{0}: the page names more than one channel, enter the UC... id instead")]
    Ambiguous(String),
    #[error(
        "{0}: the video is private or removed, so its channel cannot be read, enter the channel instead"
    )]
    VideoUnavailable(String),
    #[error(
        "YouTube lookups are paused for {secs} more seconds after a throttle, so {input} was not resolved. Try again then, or enter the UC... id"
    )]
    Paused { input: String, secs: u64 },
    #[error(
        "YouTube is throttling lookups, so {0} was not resolved. Try again later, or enter the UC... id"
    )]
    Throttled(String),
    #[error("could not reach YouTube to resolve {input}: {reason}")]
    Unreachable { input: String, reason: String },
}

impl LookupError {
    /// True when the input is at fault, false when YouTube could not be asked.
    pub fn is_input_error(&self) -> bool {
        !matches!(
            self,
            Self::Paused { .. } | Self::Throttled(_) | Self::Unreachable { .. }
        )
    }
}

/// A handle as YouTube allows them: 3 to 30 letters, digits, `_`, `-` and `.`. Letters may be
/// non Latin.
fn is_handle(handle: &str) -> bool {
    (3..=30).contains(&handle.chars().count())
        && handle
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '\u{b7}'))
}

/// A `/c/` or `/user/` name: no slashes or spaces, which a path segment already rules out.
fn is_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 100 && !name.contains(char::is_whitespace)
}

/// Reads one line of input without asking YouTube.
pub fn parse(input: &str) -> Result<ChannelInput, LookupError> {
    let input = input.trim();
    let invalid = || LookupError::Invalid(input.chars().take(100).collect());
    if is_channel_id(input) {
        return Ok(ChannelInput::Id(input.to_owned()));
    }
    if let Some(handle) = input.strip_prefix('@') {
        return if is_handle(handle) {
            Ok(ChannelInput::Page(Page::Channel(format!("/@{handle}"))))
        } else {
            Err(invalid())
        };
    }
    let with_scheme = if input.contains("://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let url = Url::parse(&with_scheme).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid());
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let segments: Vec<String> = url
        .path_segments()
        .map(|s| s.filter(|s| !s.is_empty()).map(decode_segment).collect())
        .unwrap_or_default();
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    let video = |id: &str| {
        if is_video_id(id) {
            Ok(ChannelInput::Page(Page::Video(id.to_owned())))
        } else {
            Err(invalid())
        }
    };
    if host == "youtu.be" {
        return match segments.as_slice() {
            [id] => video(id),
            _ => Err(invalid()),
        };
    }
    if !matches!(
        host.as_str(),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com"
    ) {
        return Err(invalid());
    }
    match segments.as_slice() {
        ["channel", id, ..] if is_channel_id(id) => Ok(ChannelInput::Id((*id).to_owned())),
        ["watch", ..] => {
            let v = url
                .query_pairs()
                .find(|(k, _)| k == "v")
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default();
            video(&v)
        }
        ["shorts" | "live" | "embed", id, ..] => video(id),
        [kind @ ("c" | "user"), name, ..] if is_name(name) => {
            Ok(ChannelInput::Page(Page::Channel(format!("/{kind}/{name}"))))
        }
        [first, ..] => match first.strip_prefix('@') {
            Some(handle) if is_handle(handle) => {
                Ok(ChannelInput::Page(Page::Channel(format!("/@{handle}"))))
            }
            _ => Err(invalid()),
        },
        [] => Err(invalid()),
    }
}

/// Decodes `%XX` escapes in a path segment, for handles in non Latin scripts.
fn decode_segment(segment: &str) -> String {
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = segment
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

static CANONICAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<link rel="canonical" href="https?://(?:www\.)?youtube\.com/channel/(UC[A-Za-z0-9_-]{22})""#)
        .expect("valid regex")
});
static EXTERNAL_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""externalId":"(UC[A-Za-z0-9_-]{22})""#).expect("valid regex"));
static IDENTIFIER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<meta itemprop="identifier" content="(UC[A-Za-z0-9_-]{22})""#)
        .expect("valid regex")
});
static OG_TITLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<meta property="og:title" content="([^"]*)""#).expect("valid regex")
});
static VIDEO_CHANNEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""channelId":"(UC[A-Za-z0-9_-]{22})""#).expect("valid regex"));
static VIDEO_AUTHOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""author":("(?:[^"\\]|\\.)*")"#).expect("valid regex"));

/// What a page gave, before the input is named in an error.
#[derive(Debug, PartialEq, Eq)]
enum PageAnswer {
    Found(Found),
    NoChannel,
    Ambiguous,
    VideoUnavailable,
}

/// Reads a channel page: the canonical link, the `externalId` in the page data and the
/// `identifier` meta tag must agree on one id.
fn read_channel_page(html: &str) -> PageAnswer {
    let mut ids: Vec<&str> = Vec::new();
    for re in [&*CANONICAL, &*EXTERNAL_ID, &*IDENTIFIER] {
        for caps in re.captures_iter(html) {
            let id = caps.get(1).map_or("", |m| m.as_str());
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    match ids.as_slice() {
        [] => PageAnswer::NoChannel,
        [id] => PageAnswer::Found(Found {
            channel_id: (*id).to_owned(),
            title: OG_TITLE
                .captures(html)
                .and_then(|c| c.get(1))
                .map(|m| unescape_html(m.as_str()))
                .filter(|t| !t.trim().is_empty()),
        }),
        _ => PageAnswer::Ambiguous,
    }
}

/// Reads a watch page: the uploader is the first `channelId` in the player's `videoDetails`.
fn read_watch_page(html: &str) -> PageAnswer {
    let Some(start) = html.find(r#""videoDetails":{"#) else {
        return PageAnswer::VideoUnavailable;
    };
    let details = &html[start..];
    let Some(id) = VIDEO_CHANNEL.captures(details).and_then(|c| c.get(1)) else {
        return PageAnswer::NoChannel;
    };
    let title = VIDEO_AUTHOR
        .captures(details)
        .and_then(|c| c.get(1))
        .and_then(|m| serde_json::from_str::<String>(m.as_str()).ok())
        .filter(|t| !t.trim().is_empty());
    PageAnswer::Found(Found {
        channel_id: id.as_str().to_owned(),
        title,
    })
}

/// The few entities YouTube writes in attribute values.
fn unescape_html(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Resolves channel input against YouTube, within the shared lookup budget.
#[derive(Debug)]
pub struct ChannelLookup {
    client: Client,
    /// The YouTube site: the origin of `watcher.feed_url`, so tests point both at one fake.
    site: Url,
    limiter: Arc<RateLimiter>,
    throttle_pause: Duration,
    cache: Mutex<HashMap<String, (Instant, Found)>>,
}

impl ChannelLookup {
    pub fn new(
        feed_url: &Url,
        limiter: Arc<RateLimiter>,
        throttle_pause: Duration,
        proxy: Option<&OutboundProxy>,
    ) -> Result<Self, reqwest::Error> {
        let builder = Client::builder()
            .user_agent(concat!("tubarr/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30));
        let client = network::youtube_client(builder, proxy)?.build()?;
        let mut site = feed_url.clone();
        site.set_path("/");
        site.set_query(None);
        Ok(Self {
            client,
            site,
            limiter,
            throttle_pause,
            cache: Mutex::new(HashMap::new()),
        })
    }

    /// Resolves every line, in order, dropping repeats of the same channel. Stops at the first
    /// line that fails, so nothing half resolved is saved.
    pub async fn resolve_all(&self, inputs: &[String]) -> Result<Vec<Found>, LookupError> {
        let mut out: Vec<Found> = Vec::new();
        for input in inputs.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            let found = self.resolve(input).await?;
            match out.iter_mut().find(|f| f.channel_id == found.channel_id) {
                // A repeat keeps its place, but a name read for it still counts.
                Some(kept) => kept.title = kept.title.take().or(found.title),
                None => out.push(found),
            }
        }
        Ok(out)
    }

    pub async fn resolve(&self, input: &str) -> Result<Found, LookupError> {
        let page = match parse(input)? {
            ChannelInput::Id(channel_id) => {
                return Ok(Found {
                    channel_id,
                    title: None,
                });
            }
            ChannelInput::Page(page) => page,
        };
        let key = page.key();
        if let Some(found) = self.cached(&key) {
            return Ok(found);
        }
        let found = self.fetch(input, &page).await?;
        self.lock_cache()
            .insert(key, (Instant::now(), found.clone()));
        Ok(found)
    }

    fn cached(&self, key: &str) -> Option<Found> {
        let mut cache = self.lock_cache();
        cache.retain(|_, (at, _)| at.elapsed() < CACHE_FOR);
        cache.get(key).map(|(_, found)| found.clone())
    }

    fn lock_cache(&self) -> std::sync::MutexGuard<'_, HashMap<String, (Instant, Found)>> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn fetch(&self, input: &str, page: &Page) -> Result<Found, LookupError> {
        let input = input.to_owned();
        if let Some(pause) = self.limiter.status().paused_for {
            return Err(LookupError::Paused {
                input,
                secs: pause.as_secs().max(1),
            });
        }
        self.limiter.acquire_in(Lane::Play).await;
        let (path, query) = page.path_and_query();
        let mut url = self.site.clone();
        url.set_path(&path);
        url.set_query(query.as_deref());
        let unreachable = |reason: String| LookupError::Unreachable {
            input: input.clone(),
            reason,
        };
        let response = self
            .client
            .get(url)
            // Declines the cookie consent page YouTube shows in some regions.
            .header(header::COOKIE, "SOCS=CAI")
            .header(header::ACCEPT_LANGUAGE, "en")
            .send()
            .await
            .map_err(|e| unreachable(e.to_string()))?;
        let status = response.status();
        let landed_away = response.url().host_str() != self.site.host_str()
            || response.url().path().starts_with("/sorry");
        if status == StatusCode::TOO_MANY_REQUESTS || landed_away {
            tracing::warn!(input, pause = ?self.throttle_pause, "YouTube is throttling, pausing lookups");
            self.limiter.penalize(self.throttle_pause);
            return Err(LookupError::Throttled(input));
        }
        if status == StatusCode::NOT_FOUND {
            return Err(LookupError::NotFound(input));
        }
        if !status.is_success() {
            return Err(unreachable(format!("HTTP {}", status.as_u16())));
        }
        let html = response
            .text()
            .await
            .map_err(|e| unreachable(e.to_string()))?;
        let answer = match page {
            Page::Channel(_) => read_channel_page(&html),
            Page::Video(_) => read_watch_page(&html),
        };
        match answer {
            PageAnswer::Found(found) => {
                tracing::info!(input, channel = found.channel_id, "channel resolved");
                Ok(found)
            }
            PageAnswer::NoChannel => Err(LookupError::NoChannel(input)),
            PageAnswer::Ambiguous => Err(LookupError::Ambiguous(input)),
            PageAnswer::VideoUnavailable => Err(LookupError::VideoUnavailable(input)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";

    fn channel(path: &str) -> ChannelInput {
        ChannelInput::Page(Page::Channel(path.to_owned()))
    }

    #[test]
    fn every_input_form_parses() {
        let id = ChannelInput::Id(ARY.to_owned());
        let cases = [
            (ARY.to_owned(), id.clone()),
            (format!("  {ARY} "), id.clone()),
            (format!("https://www.youtube.com/channel/{ARY}"), id.clone()),
            (format!("youtube.com/channel/{ARY}/videos"), id.clone()),
            ("@arydigital".to_owned(), channel("/@arydigital")),
            (
                "https://www.youtube.com/@ARYDigital".to_owned(),
                channel("/@ARYDigital"),
            ),
            (
                "m.youtube.com/@arydigital/videos".to_owned(),
                channel("/@arydigital"),
            ),
            (
                "https://www.youtube.com/c/HumTV".to_owned(),
                channel("/c/HumTV"),
            ),
            (
                "https://www.youtube.com/user/arydigitallive".to_owned(),
                channel("/user/arydigitallive"),
            ),
            (
                "https://www.youtube.com/watch?v=0eD-GQCutAU&t=10".to_owned(),
                ChannelInput::Page(Page::Video("0eD-GQCutAU".to_owned())),
            ),
            (
                "https://youtu.be/0eD-GQCutAU".to_owned(),
                ChannelInput::Page(Page::Video("0eD-GQCutAU".to_owned())),
            ),
            (
                "https://www.youtube.com/shorts/0eD-GQCutAU".to_owned(),
                ChannelInput::Page(Page::Video("0eD-GQCutAU".to_owned())),
            ),
            (
                "https://www.youtube.com/@%D8%AC%DB%8C%D9%88".to_owned(),
                channel("/@\u{62c}\u{6cc}\u{648}"),
            ),
        ];
        for (input, want) in cases {
            assert_eq!(parse(&input), Ok(want), "{input}");
        }
    }

    #[test]
    fn bad_input_is_refused() {
        for bad in [
            "",
            "arydigital",
            "@a",
            "@has space",
            "UC4JCksJF76g_MdzPVBJoC3",
            "https://vimeo.com/@arydigital",
            "https://www.youtube.com/watch?v=short",
            "https://www.youtube.com/channel/notanid",
            "https://www.youtube.com/",
            "https://www.youtube.com/feed/subscriptions",
            "ftp://www.youtube.com/@arydigital",
        ] {
            assert!(
                matches!(parse(bad), Err(LookupError::Invalid(_))),
                "{bad:?}: {:?}",
                parse(bad)
            );
        }
    }

    #[test]
    fn channel_pages_give_one_id_and_the_title() {
        let html = format!(
            r#"<link rel="canonical" href="https://www.youtube.com/channel/{ARY}">
            <meta property="og:title" content="ARY Digital &amp; Friends">
            <meta itemprop="identifier" content="{ARY}">{{"externalId":"{ARY}"}}"#
        );
        assert_eq!(
            read_channel_page(&html),
            PageAnswer::Found(Found {
                channel_id: ARY.to_owned(),
                title: Some("ARY Digital & Friends".to_owned()),
            })
        );
        let other = format!(r#"{html}{{"externalId":"UCxxxxxxxxxxxxxxxxxxxxxx"}}"#);
        assert_eq!(read_channel_page(&other), PageAnswer::Ambiguous);
        assert_eq!(read_channel_page("<html></html>"), PageAnswer::NoChannel);
    }

    #[test]
    fn watch_pages_give_the_uploader() {
        let html = format!(
            r#"{{"channelId":"UCxxxxxxxxxxxxxxxxxxxxxx"}}{{"videoDetails":{{"videoId":"0eD-GQCutAU","channelId":"{ARY}","author":"ARY \"Digital\""}}}}"#
        );
        assert_eq!(
            read_watch_page(&html),
            PageAnswer::Found(Found {
                channel_id: ARY.to_owned(),
                title: Some("ARY \"Digital\"".to_owned()),
            })
        );
        assert_eq!(
            read_watch_page(r#"{"playabilityStatus":{"status":"ERROR"}}"#),
            PageAnswer::VideoUnavailable
        );
    }
}
