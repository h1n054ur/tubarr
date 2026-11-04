//! The TMDB draft of a film, from one stored upload.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{CheckResult, DraftError, DraftHelper, Kind, names::is_native_script};
use crate::matcher::films::{read_title, title_language};

/// `POST /admin/api/drafts/film`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilmRequest {
    pub video_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilmDraft {
    pub title: String,
    /// The name in Urdu or Hindi script, when the title carries it.
    pub original_title: Option<String>,
    pub year: Option<i64>,
    /// The year is the upload's, not one the title names.
    pub year_from_upload: bool,
    pub runtime_mins: Option<i64>,
    pub language: String,
    pub country: String,
    pub channel_id: Option<String>,
    pub network: Option<String>,
    pub youtube_url: String,
    /// A telefilm, entered on TMDB as a TV film.
    pub tv_film: bool,
    /// "Part 2" of a film split in two: the runtime covers only this upload.
    pub part: Option<i64>,
    pub check: CheckResult,
}

static TELEFILM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:tele\s*-?\s*film|tv\s*film|tv\s*movie)s?\b")
        .expect("valid telefilm regex")
});

fn upload_year(upload_date: Option<&str>, published_at: Option<i64>) -> Option<i64> {
    upload_date
        .and_then(|d| d.get(0..4))
        .and_then(|y| y.parse().ok())
        .or_else(|| {
            published_at
                .and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok())
                .map(|t| i64::from(t.year()))
        })
}

/// The title's own spelling of `name`, which [`read_title`] gives in lower case.
fn as_written(title: &str, name: &str) -> String {
    let words: Vec<String> = name.split(' ').map(regex::escape).collect();
    Regex::new(&format!(r"(?i)\b{}\b", words.join(r"\W+")))
        .ok()
        .and_then(|re| re.find(title).map(|m| m.as_str().to_owned()))
        .unwrap_or_else(|| name.to_owned())
}

impl DraftHelper {
    /// Drafts a film from its stored upload. Costs no YouTube lookups.
    pub async fn film_draft(&self, request: &FilmRequest) -> Result<FilmDraft, DraftError> {
        let video_id = request.video_id.trim();
        let stored = self.db.video(video_id).await?;
        let seen = self.db.channel_video(video_id).await?;
        let (title, duration, upload_date, channel_id) = match (&stored, &seen) {
            (Some(v), seen) => (
                v.meta.title.clone(),
                v.meta.duration.and_then(|d| i64::try_from(d).ok()),
                v.meta.upload_date.clone(),
                v.meta
                    .channel_id
                    .clone()
                    .or_else(|| seen.as_ref().map(|s| s.channel_id.clone())),
            ),
            (None, Some(s)) => (
                s.title.clone(),
                s.duration_secs,
                None,
                Some(s.channel_id.clone()),
            ),
            (None, None) => {
                return Err(DraftError::NotFound(format!(
                    "video {video_id} is not stored: Tubarr drafts only from videos it has seen"
                )));
            }
        };
        let read = read_title(&title, false);
        let name = read
            .names
            .first()
            .map(|n| as_written(&title, n))
            .unwrap_or_else(|| title.clone());
        let named_year = read.years.first().copied();
        let year = named_year.or_else(|| {
            upload_year(
                upload_date.as_deref(),
                seen.as_ref().and_then(|s| s.published_at),
            )
        });
        let original_title = title
            .split(['|', '(', ')', '-'])
            .map(str::trim)
            .find(|seg| is_native_script(seg))
            .map(str::to_owned);
        let info = channel_id
            .as_deref()
            .map(|c| self.channel(c))
            .unwrap_or_default();
        // A TV network's channel uploading a film is uploading one of its telefilms.
        let tv_film = TELEFILM.is_match(&title) || info.network.is_some();
        let check = self
            .check(Kind::Film, &name, original_title.as_deref(), year)
            .await;
        Ok(FilmDraft {
            title: name,
            original_title,
            year,
            year_from_upload: named_year.is_none() && year.is_some(),
            runtime_mins: duration.map(|s| (s + 30) / 60),
            language: title_language(&title).unwrap_or_else(|| info.language.clone()),
            country: info.country.clone(),
            channel_id,
            network: info.network.clone(),
            youtube_url: format!("https://www.youtube.com/watch?v={video_id}"),
            tv_film,
            part: read.part,
            check,
        })
    }
}

/// The copy-paste block, in the order of TMDB's "add new movie" form.
pub fn text(draft: &FilmDraft) -> String {
    let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "(unknown)".to_owned());
    let mut out = format!("{}\n\nTMDB: add movie\n", draft.check.summary());
    out.push_str(&format!("Original language: {}\n", draft.language));
    out.push_str(&format!(
        "Original title: {}\n",
        draft
            .original_title
            .clone()
            .unwrap_or_else(|| draft.title.clone())
    ));
    out.push_str(&format!("English title: {}\n", draft.title));
    out.push_str(&format!(
        "Year: {}{}\n",
        draft
            .year
            .map_or_else(|| "(unknown)".to_owned(), |y| y.to_string()),
        if draft.year_from_upload {
            " (upload year, check it)"
        } else {
            ""
        }
    ));
    out.push_str(&format!(
        "Runtime: {}\n",
        draft
            .runtime_mins
            .map_or_else(|| "(unknown)".to_owned(), |m| format!("{m} min"))
    ));
    out.push_str(&format!("Country: {}\n", draft.country));
    if draft.tv_film {
        out.push_str("Genre: TV Movie (a telefilm)\n");
    }
    out.push_str(&format!("Network or channel: {}\n", opt(&draft.network)));
    out.push_str(&format!("Watch: {}\n", draft.youtube_url));
    if let Some(part) = draft.part {
        out.push_str(&format!(
            "Note: this upload is part {part}; add the runtimes of all parts\n"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_keep_the_titles_spelling() {
        assert_eq!(
            as_written("New Movie 2023 | ISHQ Hua | Vijay", "ishq hua"),
            "ISHQ Hua"
        );
        assert_eq!(
            as_written("Zindagi-Gulzar Hai", "zindagi gulzar hai"),
            "Zindagi-Gulzar Hai"
        );
    }
}
