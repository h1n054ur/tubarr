//! A channel's RSS feed: the newest 15 uploads, free to read and not counted as a lookup.

use reqwest::{Client, StatusCode, header};

use super::WatchError;
use crate::resolver::is_video_id;

const ATOM: &str = "http://www.w3.org/2005/Atom";
const YT: &str = "http://www.youtube.com/xml/schemas/2015";
const MEDIA: &str = "http://search.yahoo.com/mrss/";

/// One `<entry>` of a channel feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedEntry {
    pub video_id: String,
    pub title: String,
    /// Unix seconds.
    pub published_at: Option<i64>,
    /// The entry links to `/shorts/`: a YouTube Short.
    pub short: bool,
}

/// Validators from the last good answer, sent back so an unchanged feed costs a 304.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedAnswer {
    NotModified,
    Entries(Vec<FeedEntry>),
}

/// Reads one feed with a conditional GET and updates `validators` on a fresh answer. Anything but
/// 200 or 304 is an error: YouTube answers 404 and 500 now and then for feeds that exist.
pub async fn fetch(
    client: &Client,
    url: &str,
    validators: &mut Validators,
) -> Result<FeedAnswer, WatchError> {
    let mut request = client.get(url);
    if let Some(etag) = &validators.etag {
        request = request.header(header::IF_NONE_MATCH, etag);
    }
    if let Some(modified) = &validators.last_modified {
        request = request.header(header::IF_MODIFIED_SINCE, modified);
    }
    let response = request.send().await?;
    let status = response.status();
    if status == StatusCode::NOT_MODIFIED {
        return Ok(FeedAnswer::NotModified);
    }
    if status != StatusCode::OK {
        return Err(WatchError::Status(status.as_u16()));
    }
    let header_text = |name| {
        response
            .headers()
            .get(name)
            .and_then(|v: &header::HeaderValue| v.to_str().ok())
            .map(str::to_owned)
    };
    let fresh = Validators {
        etag: header_text(header::ETAG),
        last_modified: header_text(header::LAST_MODIFIED),
    };
    let body = response.text().await?;
    let entries = parse(&body)?;
    // Validators are kept only once the body parsed, so a broken answer is fetched in full again.
    *validators = fresh;
    Ok(FeedAnswer::Entries(entries))
}

/// Parses a YouTube channel feed (Atom with the `yt:` and `media:` extensions). Entries without a
/// valid video id are skipped.
pub fn parse(xml: &str) -> Result<Vec<FeedEntry>, WatchError> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| WatchError::Feed(e.to_string()))?;
    let root = doc.root_element();
    if !root.has_tag_name((ATOM, "feed")) {
        return Err(WatchError::Feed(format!(
            "expected an Atom feed, got <{}>",
            root.tag_name().name()
        )));
    }
    let child_text = |node: roxmltree::Node, ns: &str, name: &str| {
        node.children()
            .find(|c| c.has_tag_name((ns, name)))
            .and_then(|c| c.text())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
    };
    let entries = root
        .children()
        .filter(|n| n.has_tag_name((ATOM, "entry")))
        .filter_map(|entry| {
            let video_id = child_text(entry, YT, "videoId").filter(|id| is_video_id(id))?;
            let title = child_text(entry, ATOM, "title").or_else(|| {
                let group = entry
                    .children()
                    .find(|c| c.has_tag_name((MEDIA, "group")))?;
                child_text(group, MEDIA, "title")
            })?;
            let published_at = child_text(entry, ATOM, "published").and_then(|t| parse_rfc3339(&t));
            let short = entry
                .children()
                .filter(|c| c.has_tag_name((ATOM, "link")))
                .filter_map(|c| c.attribute("href"))
                .any(|href| href.contains("/shorts/"));
            Some(FeedEntry {
                video_id,
                title,
                published_at,
                short,
            })
        })
        .collect();
    Ok(entries)
}

/// `2024-07-02T15:00:12+00:00` (or with `Z`, fractions, other offsets) to Unix seconds.
pub fn parse_rfc3339(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |from: usize, to: usize| text.get(from..to)?.parse::<i64>().ok();
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 {
        return None;
    }
    let mut rest = &text[19..];
    if let Some(frac) = rest.strip_prefix('.') {
        rest = frac.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ => {
            let sign = match rest.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let (h, m) = rest[1..].split_once(':')?;
            sign * (h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60)
        }
    };
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_dates() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339("2024-07-02T15:00:12+00:00"),
            Some(1_719_932_412)
        );
        assert_eq!(
            parse_rfc3339("2024-07-02T20:00:12.250+05:00"),
            Some(1_719_932_412)
        );
        assert_eq!(
            parse_rfc3339("2000-02-29T00:00:00-01:30"),
            Some(951_787_800)
        );
        assert_eq!(parse_rfc3339("2024-13-02T15:00:12Z"), None);
        assert_eq!(parse_rfc3339("yesterday"), None);
    }

    #[test]
    fn html_is_not_a_feed() {
        let page = "<html><head><title>Error 500</title></head></html>";
        assert!(matches!(parse(page), Err(WatchError::Feed(_))));
        assert!(matches!(parse("not xml"), Err(WatchError::Feed(_))));
    }

    #[test]
    fn entries_without_an_id_are_skipped_and_media_title_is_a_fallback() {
        let xml = format!(
            r#"<feed xmlns="{ATOM}" xmlns:yt="{YT}" xmlns:media="{MEDIA}">
              <entry><yt:videoId>bad</yt:videoId><title>x</title></entry>
              <entry><yt:videoId>abcdefghijk</yt:videoId><title> </title>
                <media:group><media:title>From media</media:title></media:group></entry>
            </feed>"#
        );
        let entries = parse(&xml).unwrap();
        assert_eq!(
            entries,
            [FeedEntry {
                video_id: "abcdefghijk".to_owned(),
                title: "From media".to_owned(),
                published_at: None,
                short: false,
            }]
        );
    }

    #[test]
    fn a_shorts_link_marks_a_short() {
        let xml = format!(
            r#"<feed xmlns="{ATOM}" xmlns:yt="{YT}">
              <entry><yt:videoId>episode0001</yt:videoId><title>#2569 - Bill Thompson</title>
                <link rel="alternate" href="https://www.youtube.com/watch?v=episode0001"/></entry>
              <entry><yt:videoId>shortlink01</yt:videoId><title>Bill on bears</title>
                <link rel="alternate" href="https://www.youtube.com/shorts/shortlink01"/></entry>
            </feed>"#
        );
        let shorts: Vec<bool> = parse(&xml).unwrap().iter().map(|e| e.short).collect();
        assert_eq!(shorts, [false, true]);
    }
}
