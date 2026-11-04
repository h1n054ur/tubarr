//! The id in a `/play` path: one YouTube video id, or the ids of an episode's parts joined by
//! commas (`/play/<part1>,<part2>`), played as one file.
//!
//! The ids live in the path itself rather than behind a key in the database, so a `.strm` file
//! keeps working on its own and the path says what it plays. A comma is allowed unescaped in a URL
//! path (RFC 3986 sub-delims), never appears in a video id, and ffmpeg and Jellyfin pass it through
//! as is. A client that escapes it as `%2C` still works, since the path is decoded first.

use url::Url;

use crate::resolver::is_video_id;

/// Most parts one path may join. Real uploads come in two or three.
pub const MAX_PARTS: usize = 6;

const SEPARATOR: char = ',';

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PartsError {
    #[error("no video id")]
    Empty,
    #[error("{0} parts, at most {MAX_PARTS} can be joined")]
    TooMany(usize),
    #[error("not a YouTube video id: {0:?}")]
    InvalidId(String),
    #[error("cannot join the play path onto the public URL: {0}")]
    Url(#[from] url::ParseError),
}

/// The video ids in a `/play` path segment, in play order.
pub fn parse(segment: &str) -> Result<Vec<String>, PartsError> {
    let ids: Vec<&str> = segment.split(SEPARATOR).collect();
    check(&ids)?;
    Ok(ids.into_iter().map(str::to_owned).collect())
}

fn check<S: AsRef<str>>(ids: &[S]) -> Result<(), PartsError> {
    match ids.len() {
        0 => return Err(PartsError::Empty),
        n if n > MAX_PARTS => return Err(PartsError::TooMany(n)),
        _ => {}
    }
    match ids.iter().find(|id| !is_video_id(id.as_ref())) {
        Some(bad) => Err(PartsError::InvalidId(
            bad.as_ref().chars().take(64).collect(),
        )),
        None => Ok(()),
    }
}

/// The URL a `.strm` file holds for a video, or for an episode split over several videos in play
/// order. `public_url` is the configured base (ending in `/`), whose path prefix is kept.
pub fn play_url<S: AsRef<str>>(public_url: &Url, ids: &[S]) -> Result<Url, PartsError> {
    check(ids)?;
    let joined: Vec<&str> = ids.iter().map(AsRef::as_ref).collect();
    Ok(public_url.join(&format!("play/{}", joined.join(",")))?)
}

/// The URL of the same episode's audio alone: the play URL with `/audio` after it.
pub fn audio_url<S: AsRef<str>>(public_url: &Url, ids: &[S]) -> Result<Url, PartsError> {
    let url = play_url(public_url, ids)?;
    Ok(Url::parse(&format!("{url}/audio"))?)
}

/// The ids a play URL under `public_url` names, as [`play_url`] writes it. `None` for anything
/// else, an audio URL included.
pub fn ids_in(public_url: &Url, url: &str) -> Option<Vec<String>> {
    let segment = url
        .strip_prefix(public_url.as_str())?
        .strip_prefix("play/")?;
    parse(segment).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "0eD-GQCutAU";
    const B: &str = "ldmPWRb-XcM";

    #[test]
    fn parses_one_or_several_ids() {
        assert_eq!(parse(A).unwrap(), [A]);
        assert_eq!(parse(&format!("{A},{B}")).unwrap(), [A, B]);
        assert_eq!(parse(&format!("{B},{A},{B}")).unwrap(), [B, A, B]);
    }

    #[test]
    fn reads_the_ids_back_from_play_urls() {
        let base = Url::parse("http://tubarr:8480/base/").unwrap();
        let one = play_url(&base, &[A]).unwrap();
        assert_eq!(ids_in(&base, one.as_str()).unwrap(), [A]);
        let both = play_url(&base, &[A, B]).unwrap();
        assert_eq!(ids_in(&base, both.as_str()).unwrap(), [A, B]);
        let audio = audio_url(&base, &[A, B]).unwrap();
        assert_eq!(
            audio.as_str(),
            format!("http://tubarr:8480/base/play/{A},{B}/audio")
        );
        assert_eq!(ids_in(&base, audio.as_str()), None);
        assert_eq!(
            ids_in(&base, &format!("http://other:8480/base/play/{A}")),
            None
        );
        assert_eq!(ids_in(&base, &format!("{base}play/{A}?x=1")), None);
    }

    #[test]
    fn refuses_bad_paths() {
        assert_eq!(parse(""), Err(PartsError::InvalidId(String::new())));
        assert_eq!(
            parse(&format!("{A},")),
            Err(PartsError::InvalidId(String::new()))
        );
        assert_eq!(
            parse(&format!("{A}+{B}")),
            Err(PartsError::InvalidId(format!("{A}+{B}")))
        );
        assert_eq!(parse(&[A; 7].join(",")), Err(PartsError::TooMany(7)));
        assert!(parse(&[A; 6].join(",")).is_ok());
        assert_eq!(play_url(&base(), &[] as &[&str]), Err(PartsError::Empty));
    }

    fn base() -> Url {
        Url::parse("http://tubarr.example:8409/tubarr/").unwrap()
    }

    #[test]
    fn builds_strm_urls_that_parse_back() {
        let one = play_url(&base(), &[A]).unwrap();
        assert_eq!(
            one.as_str(),
            format!("http://tubarr.example:8409/tubarr/play/{A}")
        );
        let two = play_url(&base(), &[A.to_owned(), B.to_owned()]).unwrap();
        assert_eq!(
            two.as_str(),
            format!("http://tubarr.example:8409/tubarr/play/{A},{B}")
        );
        let segment = two.path_segments().unwrap().next_back().unwrap();
        assert_eq!(parse(segment).unwrap(), [A, B]);
        assert!(matches!(
            play_url(&base(), &["nope"]),
            Err(PartsError::InvalidId(_))
        ));
    }
}
