//! The XML documents of the Newznab API: caps, the RSS result feed, errors and the NZB.

use std::fmt::Write;

use time::{OffsetDateTime, format_description::well_known::Rfc2822};

use super::{Category, Release, ReleaseTarget};

pub const NEWZNAB_NS: &str = "http://www.newznab.com/DTD/2010/feeds/attributes/";
const NZB_NS: &str = "http://www.newzbin.com/DTD/2003/nzb";
const DECLARATION: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";

/// Most items one answer holds, advertised in caps.
pub const MAX_LIMIT: u32 = 100;

/// Escapes text for element content and double-quoted attributes.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // Control characters other than tab and newlines are not allowed in XML 1.0.
            c if c.is_control() && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

/// What Tubarr can search, as Sonarr and Radarr read it when the indexer is added.
pub fn caps() -> String {
    let mut out = String::from(DECLARATION);
    let _ = write!(
        out,
        "<caps>\n\
         <server version=\"{version}\" title=\"Tubarr\"/>\n\
         <limits max=\"{MAX_LIMIT}\" default=\"{MAX_LIMIT}\"/>\n\
         <searching>\n\
         <search available=\"yes\" supportedParams=\"q\"/>\n\
         <tv-search available=\"yes\" supportedParams=\"q,tvdbid,season,ep\"/>\n\
         <movie-search available=\"yes\" supportedParams=\"q,imdbid,tmdbid\"/>\n\
         <audio-search available=\"no\" supportedParams=\"\"/>\n\
         <book-search available=\"no\" supportedParams=\"\"/>\n\
         </searching>\n\
         <categories>\n",
        version = env!("CARGO_PKG_VERSION"),
    );
    for parent in [Category::MOVIES, Category::TV] {
        let _ = writeln!(
            out,
            "<category id=\"{}\" name=\"{}\">",
            parent.id, parent.name
        );
        for sub in Category::ALL.iter().filter(|c| c.parent == Some(parent.id)) {
            let _ = writeln!(out, "<subcat id=\"{}\" name=\"{}\"/>", sub.id, sub.name);
        }
        out.push_str("</category>\n");
    }
    out.push_str("</categories>\n</caps>\n");
    out
}

/// A Newznab error document. Sonarr and Radarr treat codes 100 to 199 as a bad API key.
pub fn error(code: u16, description: &str) -> String {
    format!(
        "{DECLARATION}<error code=\"{code}\" description=\"{}\"/>\n",
        escape(description)
    )
}

/// One item of a result feed: the release and the URL its NZB downloads from.
pub struct Item<'a> {
    pub release: &'a Release,
    pub download_url: &'a str,
}

/// The RSS 2.0 result feed with Newznab attributes.
pub fn feed(channel_link: &str, offset: u32, items: &[Item<'_>]) -> String {
    let mut out = String::from(DECLARATION);
    let _ = write!(
        out,
        "<rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\" \
         xmlns:newznab=\"{NEWZNAB_NS}\">\n<channel>\n\
         <title>Tubarr</title>\n\
         <description>YouTube releases for Sonarr and Radarr</description>\n\
         <link>{}</link>\n\
         <newznab:response offset=\"{offset}\" total=\"{}\"/>\n",
        escape(channel_link),
        items.len(),
    );
    for item in items {
        write_item(&mut out, item);
    }
    out.push_str("</channel>\n</rss>\n");
    out
}

fn write_item(out: &mut String, item: &Item<'_>) {
    let r = item.release;
    let title = escape(&r.title);
    let url = escape(item.download_url);
    let category = r.category();
    let _ = write!(
        out,
        "<item>\n\
         <title>{title}</title>\n\
         <guid isPermaLink=\"false\">{guid}</guid>\n\
         <link>{url}</link>\n\
         <pubDate>{date}</pubDate>\n\
         <category>{category_name}</category>\n\
         <description>{title}</description>\n\
         <enclosure url=\"{url}\" length=\"{length}\" type=\"application/x-nzb\"/>\n",
        guid = escape(&r.guid()),
        date = rfc2822(r.published_at),
        category_name = escape(&category.full_name()),
        length = r.size.unwrap_or(0),
    );
    let mut attr = |name: &str, value: &str| {
        let _ = writeln!(
            out,
            "<newznab:attr name=\"{name}\" value=\"{}\"/>",
            escape(value)
        );
    };
    if let Some(parent) = category.parent {
        attr("category", &parent.to_string());
    }
    attr("category", &category.id.to_string());
    if let Some(size) = r.size {
        attr("size", &size.to_string());
    }
    match &r.target {
        ReleaseTarget::Episode {
            tvdb_id,
            season,
            episode,
        } => {
            attr("tvdbid", &tvdb_id.to_string());
            attr("season", &season.to_string());
            attr("episode", &episode.to_string());
        }
        ReleaseTarget::Daily { tvdb_id, air_date } => {
            attr("tvdbid", &tvdb_id.to_string());
            attr("tvairdate", air_date);
        }
        ReleaseTarget::Film { tmdb_id, imdb_id } => {
            attr("tmdbid", &tmdb_id.to_string());
            if let Some(imdb) = imdb_id {
                attr("imdb", &format!("{imdb:07}"));
            }
        }
        ReleaseTarget::Placeholder(_) => {}
    }
    if !r.language.is_empty() {
        attr("language", &r.language);
    }
    out.push_str("</item>\n");
}

/// RFC 2822 date of Unix seconds in UTC, as RSS `pubDate` wants. Out-of-range times show the epoch.
pub fn rfc2822(unix: i64) -> String {
    let at = OffsetDateTime::from_unix_timestamp(unix).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    at.format(&Rfc2822)
        .unwrap_or_else(|_| "Thu, 01 Jan 1970 00:00:00 +0000".to_owned())
}

/// A minimal NZB that passes the arrs' check (an `nzb` root with at least one `file`). It carries
/// the video id, which is all the download client needs, and points at no real Usenet article.
pub fn nzb(video_id: &str, release_title: &str, published_at: i64) -> String {
    let id = escape(video_id);
    let title = escape(release_title);
    format!(
        "{DECLARATION}<nzb xmlns=\"{NZB_NS}\">\n\
         <head>\n\
         <meta type=\"title\">{title}</meta>\n\
         <meta type=\"tubarr-video\">{id}</meta>\n\
         </head>\n\
         <file poster=\"Tubarr\" date=\"{published_at}\" subject=\"&quot;{title}.strm&quot; yEnc (1/1)\">\n\
         <groups><group>alt.binaries.tubarr</group></groups>\n\
         <segments><segment bytes=\"1\" number=\"1\">{id}@tubarr.invalid</segment></segments>\n\
         </file>\n\
         </nzb>\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup_and_drops_control_characters() {
        assert_eq!(
            escape("a & b <c> \"d\" 'e'\u{1}"),
            "a &amp; b &lt;c&gt; &quot;d&quot; &apos;e&apos;"
        );
    }

    #[test]
    fn pub_date_is_rfc_2822_utc() {
        assert_eq!(rfc2822(1_792_000_000), "Wed, 14 Oct 2026 17:46:40 +0000");
        assert_eq!(rfc2822(0), "Thu, 01 Jan 1970 00:00:00 +0000");
    }
}
