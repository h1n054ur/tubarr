//! A channel's full video list, or an official playlist, from `yt-dlp --flat-playlist`: one
//! lookup that reads up to a few hundred titles without touching each video.

use serde::Deserialize;
use url::Url;

use super::{feed::days_from_civil, is_channel_id};
use crate::resolver::{ResolveError, YtDlp, is_unfinished_live, is_video_id};
use crate::store::{Availability, SHORT};

/// One entry of a flat channel list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingEntry {
    pub video_id: String,
    /// The uploader's channel, from the entry or else the list itself. `None` when neither names
    /// a `UC...` id.
    pub channel_id: Option<String>,
    pub title: String,
    /// Whole seconds.
    pub duration: Option<i64>,
    /// Unix seconds. A guess from "3 days ago" (the `approximate_date` extractor option), or the
    /// start of the upload day when yt-dlp gives only a date.
    pub published_at: Option<i64>,
    /// A Short, members only, a livestream not finished yet, or an open upload.
    pub availability: Availability,
}

/// A video this short is a Short even where the list does not link it as one (a playlist, a
/// search). Shorts may run three minutes now, but a mapping's duration floor already keeps the
/// longer ones out, and a minute is too short for any episode.
const SHORT_MAX_SECS: i64 = 60;

/// The arguments for listing the newest `limit` videos of `channel_id`.
pub fn args(ytdlp: &YtDlp, channel_id: &str, limit: u32) -> Vec<String> {
    with_url(
        ytdlp,
        limit,
        format!("https://www.youtube.com/channel/{channel_id}/videos"),
    )
}

/// The arguments for listing the first `limit` videos of an official playlist, in its order.
pub fn playlist_args(ytdlp: &YtDlp, playlist_id: &str, limit: u32) -> Vec<String> {
    with_url(
        ytdlp,
        limit,
        format!("https://www.youtube.com/playlist?list={playlist_id}"),
    )
}

/// The arguments for the first `limit` results of a search for `query` on one channel's search
/// tab. yt-dlp's YouTube tab extractor reads `/channel/<id>/search?query=...` as a list like any
/// other tab, so the output parses with [`parse`].
pub fn search_args(ytdlp: &YtDlp, channel_id: &str, query: &str, limit: u32) -> Vec<String> {
    let mut url = Url::parse("https://www.youtube.com/").expect("static URL is valid");
    url.set_path(&format!("channel/{channel_id}/search"));
    url.query_pairs_mut().append_pair("query", query);
    with_url(ytdlp, limit, url.into())
}

fn with_url(ytdlp: &YtDlp, limit: u32, url: String) -> Vec<String> {
    let mut args = vec![
        "--flat-playlist".to_owned(),
        "-J".to_owned(),
        "--no-warnings".to_owned(),
        "--ignore-config".to_owned(),
        "--playlist-end".to_owned(),
        limit.to_string(),
        // Reads "3 days ago" off the list page into `timestamp`, at no extra request.
        "--extractor-args".to_owned(),
        "youtubetab:approximate_date".to_owned(),
    ];
    if let Some(deno) = &ytdlp.deno {
        args.push("--js-runtimes".to_owned());
        args.push(format!("deno:{}", deno.display()));
    }
    args.push(url);
    args
}

#[derive(Deserialize)]
struct RawListing {
    channel_id: Option<String>,
    #[serde(default)]
    entries: Vec<Option<RawEntry>>,
}

#[derive(Deserialize)]
struct RawEntry {
    id: Option<String>,
    channel_id: Option<String>,
    title: Option<String>,
    duration: Option<f64>,
    timestamp: Option<i64>,
    upload_date: Option<String>,
    /// `https://www.youtube.com/shorts/<id>` for a Short.
    url: Option<String>,
    live_status: Option<String>,
    /// From the entry's badges: `subscriber_only`, `premium_only`, `needs_auth`, `private`, or
    /// `unlisted` and `public`, which play.
    availability: Option<String>,
}

impl RawEntry {
    fn availability(&self, duration: Option<i64>) -> Availability {
        let short_link = self.url.as_deref().is_some_and(|u| u.contains("/shorts/"));
        // A scheduled stream may list a zero duration.
        if short_link || duration.is_some_and(|d| (1..=SHORT_MAX_SECS).contains(&d)) {
            return Availability::Skip(SHORT);
        }
        let restricted = match self.availability.as_deref() {
            Some("subscriber_only") => Some("members only"),
            Some("premium_only") => Some("YouTube Premium only"),
            Some("needs_auth") => Some("needs a signed-in account"),
            Some("private") => Some("a private video"),
            _ => None,
        };
        if let Some(reason) = restricted {
            Availability::Skip(reason)
        } else if self.live_status.as_deref().is_some_and(is_unfinished_live) {
            Availability::Live
        } else {
            Availability::Open
        }
    }
}

/// Titles YouTube gives videos a playlist still holds but nobody can watch.
const UNAVAILABLE: [&str; 2] = ["[Private video]", "[Deleted video]"];

/// Parses `yt-dlp --flat-playlist -J` output. Entries without an id or a title, and private or
/// deleted videos, are skipped.
pub fn parse(json: &[u8]) -> Result<Vec<ListingEntry>, ResolveError> {
    let raw: RawListing =
        serde_json::from_slice(json).map_err(|e| ResolveError::Parse(e.to_string()))?;
    let list_channel = raw.channel_id.filter(|c| is_channel_id(c));
    Ok(raw
        .entries
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let duration = e
                .duration
                .filter(|d| d.is_finite() && *d >= 0.0)
                .map(|d| d.round() as i64);
            let availability = e.availability(duration);
            let video_id = e.id.filter(|id| is_video_id(id))?;
            let title = e
                .title
                .filter(|t| !t.trim().is_empty() && !UNAVAILABLE.contains(&t.trim()))?;
            Some(ListingEntry {
                video_id,
                channel_id: e
                    .channel_id
                    .filter(|c| is_channel_id(c))
                    .or_else(|| list_channel.clone()),
                title,
                duration,
                published_at: e
                    .timestamp
                    .or_else(|| e.upload_date.as_deref().and_then(day_start)),
                availability,
            })
        })
        .collect())
}

/// `YYYYMMDD` to Unix seconds at 00:00 UTC.
fn day_start(date: &str) -> Option<i64> {
    if date.len() != 8 || !date.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let num = |a: usize, b: usize| date[a..b].parse::<i64>().ok();
    let (y, m, d) = (num(0, 4)?, num(4, 6)?, num(6, 8)?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_ask_for_a_short_flat_list() {
        let mut ytdlp = YtDlp::new("/config/bin/yt-dlp");
        ytdlp.deno = Some("/config/bin/deno".into());
        let args = args(&ytdlp, "UC4JCksJF76g_MdzPVBJoC3Q", 100);
        let at = args.iter().position(|a| a == "--playlist-end").unwrap();
        assert_eq!(args[at + 1], "100");
        assert!(args.contains(&"--flat-playlist".to_owned()));
        assert!(args.contains(&"deno:/config/bin/deno".to_owned()));
        assert_eq!(
            args.last().unwrap(),
            "https://www.youtube.com/channel/UC4JCksJF76g_MdzPVBJoC3Q/videos"
        );
    }

    #[test]
    fn playlist_args_name_the_playlist_page() {
        let args = playlist_args(&YtDlp::new("yt-dlp"), "PLabcdefghijkl", 500);
        assert_eq!(
            args.last().unwrap(),
            "https://www.youtube.com/playlist?list=PLabcdefghijkl"
        );
        let at = args.iter().position(|a| a == "--playlist-end").unwrap();
        assert_eq!(args[at + 1], "500");
    }

    #[test]
    fn search_args_encode_the_query_on_the_channel_search_tab() {
        let args = search_args(
            &YtDlp::new("yt-dlp"),
            "UC4JCksJF76g_MdzPVBJoC3Q",
            "Dar E Nijaat & more",
            200,
        );
        assert_eq!(
            args.last().unwrap(),
            "https://www.youtube.com/channel/UC4JCksJF76g_MdzPVBJoC3Q/search?query=Dar+E+Nijaat+%26+more"
        );
        let at = args.iter().position(|a| a == "--playlist-end").unwrap();
        assert_eq!(args[at + 1], "200");
    }

    #[test]
    fn the_list_channel_fills_in_for_entries_without_one() {
        let json = br#"{"_type": "playlist", "channel_id": "UC4JCksJF76g_MdzPVBJoC3Q", "entries": [
            {"id": "abcdefghijk", "title": "A", "channel_id": null},
            {"id": "bcdefghijkl", "title": "B", "channel_id": "UCaaaaaaaaaaaaaaaaaaaaaa"},
            {"id": "cdefghijklm", "title": "C", "channel_id": "not a channel"}
        ]}"#;
        let channels: Vec<_> = parse(json)
            .unwrap()
            .into_iter()
            .map(|e| e.channel_id.unwrap())
            .collect();
        assert_eq!(
            channels,
            [
                "UC4JCksJF76g_MdzPVBJoC3Q",
                "UCaaaaaaaaaaaaaaaaaaaaaa",
                "UC4JCksJF76g_MdzPVBJoC3Q"
            ]
        );
    }

    #[test]
    fn upload_date_is_a_fallback_and_bad_entries_are_skipped() {
        let json = br#"{"_type": "playlist", "entries": [
            null,
            {"id": "abcdefghijk", "title": "A", "duration": 61.6, "upload_date": "20240702"},
            {"id": "short", "title": "B"},
            {"id": "bcdefghijkl", "title": null},
            {"id": "cdefghijklm", "title": "[Private video]", "duration": null}
        ]}"#;
        let entries = parse(json).unwrap();
        assert_eq!(
            entries,
            [ListingEntry {
                video_id: "abcdefghijk".to_owned(),
                channel_id: None,
                title: "A".to_owned(),
                duration: Some(62),
                published_at: Some(1_719_878_400),
                availability: Availability::Open,
            }]
        );
        assert!(matches!(parse(b"null"), Err(ResolveError::Parse(_))));
    }

    #[test]
    fn shorts_restricted_videos_and_unfinished_streams_are_told_apart() {
        let json = br##"{"_type": "playlist", "entries": [
            {"id": "episode0001", "title": "#2569 - Bill Thompson", "duration": 9000,
             "url": "https://www.youtube.com/watch?v=episode0001", "live_status": null,
             "availability": null},
            {"id": "shortlink01", "title": "Bill on bears", "duration": 95,
             "url": "https://www.youtube.com/shorts/shortlink01"},
            {"id": "shortlen001", "title": "Bill on bears", "duration": 45,
             "url": "https://www.youtube.com/watch?v=shortlen001"},
            {"id": "members0001", "title": "#2569 - Bill Thompson (early access)",
             "duration": 9000, "availability": "subscriber_only"},
            {"id": "premium0001", "title": "Premium extra", "duration": 9000,
             "availability": "premium_only"},
            {"id": "signedin001", "title": "Needs a login", "duration": 9000,
             "availability": "needs_auth"},
            {"id": "unlisted001", "title": "Unlisted episode", "duration": 9000,
             "availability": "unlisted"},
            {"id": "livenow0001", "title": "LIVE: the after show", "live_status": "is_live"},
            {"id": "premiere001", "title": "#2570 - Premiere", "duration": 0,
             "live_status": "is_upcoming"},
            {"id": "postlive001", "title": "Stream replay", "duration": 7200,
             "live_status": "post_live"},
            {"id": "waslive0001", "title": "Old stream replay", "duration": 7200,
             "live_status": "was_live"}
        ]}"##;
        let got: Vec<(String, Availability)> = parse(json)
            .unwrap()
            .into_iter()
            .map(|e| (e.video_id, e.availability))
            .collect();
        let want = [
            ("episode0001", Availability::Open),
            ("shortlink01", Availability::Skip(SHORT)),
            ("shortlen001", Availability::Skip(SHORT)),
            ("members0001", Availability::Skip("members only")),
            ("premium0001", Availability::Skip("YouTube Premium only")),
            (
                "signedin001",
                Availability::Skip("needs a signed-in account"),
            ),
            ("unlisted001", Availability::Open),
            ("livenow0001", Availability::Live),
            ("premiere001", Availability::Live),
            ("postlive001", Availability::Live),
            ("waslive0001", Availability::Open),
        ];
        assert_eq!(
            got,
            want.map(|(id, a)| (id.to_owned(), a)),
            "a Short is told by its link or a length of a minute at most"
        );
    }
}
