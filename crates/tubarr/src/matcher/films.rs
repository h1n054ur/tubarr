//! The matcher for films: which upload on an official film channel is the film Radarr wants.
//!
//! Film channels like Goldmines upload hundreds of films, many of them Hindi dubs of South Indian
//! films under the dub's own marketing title. For each wanted film the matcher reads the uploads
//! on the film's channels plus the global film channels (`films.channels`), strips the marketing
//! words from each title ("Full Hindi Dubbed Movie", "(4K)", "| Vijay, Samantha |") and compares
//! what is left with the film's title as Radarr has it and its aliases. It stays conservative: a
//! near miss or a different year (a remake?) is noted for a pin, never released. A film split into
//! parts is released as one when every part is up and no full upload exists ([`super::joined`]).
//!
//! Wanted films are the tagged, monitored films, with or without a file, as for shows: the release
//! has to stay on offer for Radarr to grab it again after a file is lost or deleted, and an offer
//! for a film that already has a file costs nothing. Like the show matcher a run is a full
//! recompute per film, so running it twice changes nothing and a pin wins on the next run.

use std::{
    collections::{BTreeMap, HashSet},
    future::Future,
    sync::LazyLock,
    time::Duration,
};

use regex::Regex;
use serde::Deserialize;

use super::{
    Candidate, Candidates, Metadata, Report,
    joined::{self, Joined, PartUpload},
    release_set::{self, Choice},
};
use crate::store::{Db, Mapping, MappingKind, NewUnmatched, StoreError, UnmatchedState};

/// The `[films]` config section.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FilmSettings {
    /// Official film channel ids (`UC...`) searched for every film, after the film's own channels.
    pub channels: Vec<String>,
    /// Shorter uploads are clips, songs or trailers. A mapping's own floor wins.
    #[serde(with = "humantime_serde")]
    pub min_duration: Duration,
    /// How close (0 to 1, by edit distance) a title must be to the film's name or an alias to
    /// count as the same. Titles a little below this are noted as unsure.
    pub similarity: f64,
}

impl Default for FilmSettings {
    fn default() -> Self {
        Self {
            channels: Vec::new(),
            min_duration: Duration::from_secs(80 * 60),
            similarity: 0.9,
        }
    }
}

impl FilmSettings {
    /// The failing key and why, for the config loader.
    pub(crate) fn check(self) -> Result<Self, (&'static str, String)> {
        let mut seen = HashSet::new();
        for id in &self.channels {
            if id.is_empty() || id.chars().any(char::is_whitespace) {
                return Err(("films.channels", format!("{id:?} is not a channel id")));
            }
            if !seen.insert(id) {
                return Err(("films.channels", format!("{id:?} is listed twice")));
            }
        }
        if self.min_duration < Duration::from_secs(60) {
            return Err(("films.min_duration", "must be at least 1m".to_owned()));
        }
        if !(0.5..=1.0).contains(&self.similarity) {
            return Err(("films.similarity", "must be between 0.5 and 1".to_owned()));
        }
        Ok(self)
    }
}

/// The films to match. Only mappings of kind `film` are matched here.
pub trait WantedFilms: Sync {
    fn films(&self) -> impl Future<Output = Result<Vec<Mapping>, StoreError>> + Send;
}

/// Title words that mark clips and extras, matched as whole words with an optional plural `s`.
/// The mapping's own excludes are added. A word in the film's own name or aliases is left out.
pub const DEFAULT_FILM_EXCLUDES: &[&str] = &[
    "Trailer",
    "Teaser",
    "Promo",
    "Song",
    "Jukebox",
    "Scene",
    "Making",
    "Review",
    "Reaction",
    "Interview",
    "Climax",
    "Dialogue",
    "Shorts",
    "Status",
    "Spoof",
];

/// Below the threshold by at most this much, a title is noted as close rather than ignored.
const NEAR_BAND: f64 = 0.15;

/// A year this far from Radarr's still counts: release and TMDB dates can straddle a new year.
const YEAR_SLACK: i64 = 1;

const LANGS: &str =
    "hindi|urdu|tamil|telugu|malayalam|kannada|punjabi|bengali|marathi|bhojpuri|gujarati|english";

/// Marketing phrases around the film's name. Anything ending in "movie" or "film" with the usual
/// words before it, language and dub notes, quality tags.
static NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?ix)\b(?:
            (?:full\s+)?(?:(?:new|latest|superhit|super\s+hit|blockbuster|action|romantic|comedy|
                thriller|released|south|indian|bollywood|tollywood)\s+)*
                (?:(?:{LANGS})\s+)?(?:dub(?:bed)?\s+)?(?:full\s+)?(?:movie|film)s?
                (?:\s+(?:in\s+)?(?:{LANGS})(?:\s+dub(?:bed)?)?)?
            | (?:{LANGS})\s+dub(?:bed)?(?:\s+version)?
            | dubbed\s+in\s+(?:{LANGS})
            | (?:{LANGS})\s+version
            | world\s+(?:television|tv|digital)\s+premiere | wtp
            | full\s+hd | uhd | hd | 4k | 8k | 1080p | 720p | hdrip
            | official | exclusive
        )\b"
    ))
    .expect("valid noise regex")
});

/// "Hindi Dubbed", "Dubbed in Hindi", "Full Hindi Movie", "(Hindi)". A bare language word could
/// be part of the name ("English Vinglish"), so it only counts next to these words.
static LANGUAGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?ix)
            \b(?P<a>{LANGS})\s+(?:dub(?:bed)?|(?:full\s+)?(?:movie|film)|version)\b
            | \b(?:dubbed\s+in|full)\s+(?P<b>{LANGS})\b
            | [(\[]\s*(?P<c>{LANGS})\s*[)\]]"
    ))
    .expect("valid language regex")
});

static PART: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bpart\s*[-#]?\s*(?P<n>\d{1,2})\b").expect("valid part regex")
});

/// A bracket holding only a language, like "[Hindi]".
static LANG_ONLY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("^(?:{LANGS})$")).expect("valid language-only regex"));

static BRACKETS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[(\[]([^)\]]*)[)\]]").expect("valid bracket regex"));

/// Lower case, letters and digits only, single spaces.
fn normalize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_year(token: &str) -> bool {
    token.len() == 4
        && (token.starts_with("19") || token.starts_with("20"))
        && token.bytes().all(|b| b.is_ascii_digit())
}

/// The language a title names, as an English name ("Hindi").
pub fn title_language(title: &str) -> Option<String> {
    let caps = LANGUAGE.captures(title)?;
    let word = caps
        .name("a")
        .or(caps.name("b"))
        .or(caps.name("c"))?
        .as_str();
    let mut chars = word.chars();
    let first = chars.next()?;
    Some(
        first
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect(),
    )
}

/// What a YouTube title says once the marketing is stripped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitleRead {
    /// The name, and any other name in brackets: "Policeodu (Theri)" gives both.
    pub names: Vec<String>,
    /// Years in the name part, bracketed or bare.
    pub years: Vec<i64>,
    /// "Part 2" of a film split in two.
    pub part: Option<i64>,
}

/// Reads a title. The name is the first `|` segment that has anything left after stripping, so
/// "New South Movie 2023 | Theri | Vijay" names Theri and the actor list is never compared.
/// A segment before it that is only a year is kept as a name too: the film may be "1920".
/// With `keep_part` a "Part 2" stays in the name, for films whose own name carries one.
pub fn read_title(title: &str, keep_part: bool) -> TitleRead {
    let part = if keep_part {
        None
    } else {
        PART.captures(title).and_then(|c| c["n"].parse().ok())
    };
    let mut lone_years = Vec::new();
    for segment in title.split('|') {
        let segment = if keep_part {
            segment.to_owned()
        } else {
            PART.replace_all(segment, " ").into_owned()
        };
        let mut years = Vec::new();
        let mut names = Vec::new();
        let main = BRACKETS.replace_all(&segment, " ");
        let pieces = std::iter::once(main.as_ref()).chain(
            BRACKETS
                .captures_iter(&segment)
                .filter_map(|c| c.get(1).map(|m| m.as_str())),
        );
        let mut segment_years = Vec::new();
        for piece in pieces {
            match clean(piece) {
                Piece::Name(name, found) => {
                    names.push(name);
                    years.extend(found);
                }
                Piece::Year(year) => segment_years.push(year),
                Piece::Empty => {}
            }
        }
        if names.is_empty() {
            lone_years.extend(segment_years.iter().map(i64::to_string));
            continue;
        }
        years.extend(segment_years);
        names.extend(lone_years);
        return TitleRead { names, years, part };
    }
    TitleRead {
        names: lone_years,
        years: Vec::new(),
        part,
    }
}

/// One piece of a title once noise is stripped.
enum Piece {
    /// A name and the years next to it.
    Name(String, Vec<i64>),
    /// Only a year: a film year, an upload year in "New South Movie 2023", or the name "1920".
    Year(i64),
    Empty,
}

fn clean(piece: &str) -> Piece {
    let stripped = normalize(&NOISE.replace_all(piece, " "));
    if LANG_ONLY.is_match(&stripped) {
        return Piece::Empty;
    }
    let (found, rest): (Vec<&str>, Vec<&str>) = stripped
        .split(' ')
        .filter(|t| !t.is_empty())
        .partition(|t| is_year(t));
    let found: Vec<i64> = found.iter().filter_map(|y| y.parse().ok()).collect();
    match (rest.is_empty(), found.as_slice()) {
        (false, _) => Piece::Name(rest.join(" "), found),
        (true, [year]) => Piece::Year(*year),
        (true, _) => Piece::Empty,
    }
}

/// 1 for equal strings, falling with the edit distance. Spaces are ignored, so "Baahu Bali" and
/// "Baahubali" are equal.
pub(crate) fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().filter(|c| *c != ' ').collect();
    let b: Vec<char> = b.chars().filter(|c| *c != ' ').collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 0.0;
    }
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (diag + usize::from(ca != cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    let distance = row[b.len()];
    1.0 - distance as f64 / longest as f64
}

/// The digits in a name: "Singham 2" and "Singham" are never the same film.
fn numbers(s: &str) -> Vec<&str> {
    s.split(' ')
        .filter(|t| t.bytes().all(|b| b.is_ascii_digit()))
        .collect()
}

/// What a title means for one film.
#[derive(Debug, Clone, PartialEq)]
pub enum FilmMatch {
    /// Another film, an extra, or nothing to compare. Not worth a note.
    NotThisFilm,
    /// Close to the film's name but not close enough.
    Near {
        name: String,
        score: f64,
    },
    Film {
        years: Vec<i64>,
        part: Option<i64>,
    },
}

/// The rules of one film, compiled.
#[derive(Debug)]
pub struct FilmRules {
    /// The film's title and aliases, normalized.
    names: Vec<String>,
    excludes: Option<Regex>,
    /// The film's own name has "Part N" in it, so a part in the title is not a split upload.
    keep_part: bool,
    similarity: f64,
}

impl FilmRules {
    pub fn new(
        mapping: &Mapping,
        aliases: &[String],
        similarity: f64,
    ) -> Result<Self, regex::Error> {
        let mut names: Vec<String> = Vec::new();
        for name in std::iter::once(&mapping.title).chain(aliases) {
            let n = normalize(name);
            if !n.is_empty() && !names.contains(&n) {
                names.push(n);
            }
        }
        let keep_part = names.iter().any(|n| PART.is_match(n));
        let own = |w: &str| {
            let w = normalize(w);
            names
                .iter()
                .any(|n| format!(" {n} ").contains(&format!(" {w} ")))
        };
        let words: Vec<String> = DEFAULT_FILM_EXCLUDES
            .iter()
            .copied()
            .chain(mapping.excludes.iter().map(String::as_str))
            .map(str::trim)
            .filter(|w| !w.is_empty() && !own(w))
            .map(|w| regex::escape(w).replace(' ', r"\s+"))
            .collect();
        let excludes = if words.is_empty() {
            None
        } else {
            Some(Regex::new(&format!(r"(?i)\b(?:{})s?\b", words.join("|")))?)
        };
        Ok(Self {
            names,
            excludes,
            keep_part,
            similarity,
        })
    }

    pub fn read(&self, title: &str) -> FilmMatch {
        if self.excludes.as_ref().is_some_and(|re| re.is_match(title)) {
            return FilmMatch::NotThisFilm;
        }
        let read = read_title(title, self.keep_part);
        let mut best: Option<(f64, &str)> = None;
        for seen in &read.names {
            for name in &self.names {
                if numbers(seen) != numbers(name) {
                    continue;
                }
                let score = similarity(seen, name);
                if best.is_none_or(|(b, _)| score > b) {
                    best = Some((score, name));
                }
            }
        }
        match best {
            Some((score, _)) if score >= self.similarity => FilmMatch::Film {
                years: read.years,
                part: read.part,
            },
            Some((score, name)) if score >= self.similarity - NEAR_BAND => FilmMatch::Near {
                name: name.to_owned(),
                score,
            },
            _ => FilmMatch::NotThisFilm,
        }
    }
}

/// Runs the matcher over every wanted film.
pub async fn match_films(
    db: &Db,
    candidates: &impl Candidates,
    wanted: &impl WantedFilms,
    metadata: &impl Metadata,
    settings: &FilmSettings,
) -> Result<Report, StoreError> {
    let mut report = Report::default();
    for film in wanted.films().await? {
        if film.kind != MappingKind::Film {
            continue;
        }
        report.matched += 1;
        match_one(db, &film, candidates, metadata, settings, &mut report).await?;
    }
    Ok(report)
}

/// An upload whose title names the film, with its facts.
struct Upload<'a> {
    cand: &'a Candidate,
    duration: Option<i64>,
    /// From the title, when it names one. Otherwise the mapping's.
    language: Option<String>,
}

async fn match_one(
    db: &Db,
    film: &Mapping,
    candidates: &impl Candidates,
    metadata: &impl Metadata,
    settings: &FilmSettings,
    report: &mut Report,
) -> Result<(), StoreError> {
    let mut channels: Vec<String> = film.channel_ids.clone();
    for id in &settings.channels {
        if !channels.contains(id) {
            channels.push(id.clone());
        }
    }
    if channels.is_empty() {
        report
            .skipped
            .push((film.id, "no film channels set".to_owned()));
        return Ok(());
    }
    let aliases = db.aliases(film.id).await?;
    let rules = match FilmRules::new(film, &aliases, settings.similarity) {
        Ok(rules) => rules,
        Err(e) => {
            tracing::warn!(mapping = film.id, error = %e, "film skipped");
            report.skipped.push((film.id, e.to_string()));
            return Ok(());
        }
    };
    let floor = film
        .min_duration_secs
        .unwrap_or_else(|| i64::try_from(settings.min_duration.as_secs()).unwrap_or(i64::MAX));
    let pins: BTreeMap<(), String> = release_set::pins(db, film.id).await?;
    let pinned = pins.get(&()).cloned();

    let all = candidates.on_channels(&channels).await?;
    let on_channels: Vec<&Candidate> = all
        .iter()
        .filter(|c| channels.contains(&c.channel_id))
        .collect();

    let mut notes: Vec<NewUnmatched> = Vec::new();
    let note = |cand: &Candidate, state, reason: String| NewUnmatched {
        video_id: cand.video_id.clone(),
        state,
        season: None,
        episode: None,
        part: None,
        reason,
    };
    let mut uploads: Vec<Upload<'_>> = Vec::new();
    let mut parts: Vec<PartUpload<'_>> = Vec::new();
    for cand in &on_channels {
        if pinned
            .as_deref()
            .is_some_and(|p| p.split(',').any(|id| id == cand.video_id))
        {
            continue;
        }
        match rules.read(&cand.title) {
            FilmMatch::NotThisFilm => {}
            FilmMatch::Near { name, score } => notes.push(note(
                cand,
                UnmatchedState::Unsure,
                format!(
                    "the title is close to \"{name}\" ({:.0}% alike) but not the same, \
                     add an alias or pin it",
                    score * 100.0
                ),
            )),
            FilmMatch::Film { years, part } => {
                if let Some(want) = film.year
                    && let Some(off) = years.iter().find(|y| (**y - want).abs() > YEAR_SLACK)
                {
                    notes.push(note(
                        cand,
                        UnmatchedState::Unsure,
                        format!(
                            "the title says {off}, Radarr has {want}: a remake or another film?"
                        ),
                    ));
                    continue;
                }
                if let Some(part) = part {
                    parts.push(PartUpload { cand, part });
                    continue;
                }
                let duration = match cand.duration_secs {
                    Some(d) => Some(d),
                    None => metadata.known(&cand.video_id).await?.duration_secs,
                };
                uploads.push(Upload {
                    cand,
                    duration,
                    language: title_language(&cand.title),
                });
            }
        }
    }

    let mut desired: BTreeMap<(), Choice> = BTreeMap::new();
    if pinned.is_some() {
        for cand in uploads
            .iter()
            .map(|u| u.cand)
            .chain(parts.iter().map(|p| p.cand))
        {
            notes.push(note(
                cand,
                UnmatchedState::Duplicate,
                "the film is pinned to another video".to_owned(),
            ));
        }
    } else {
        let (long, short): (Vec<_>, Vec<_>) = uploads
            .into_iter()
            .partition(|u| u.duration.is_none_or(|d| d >= floor));
        if long.is_empty() {
            for u in short {
                let mins = u.duration.unwrap_or_default() / 60;
                notes.push(note(
                    u.cand,
                    UnmatchedState::Unsure,
                    format!("{mins} min is under the {} min floor", floor / 60),
                ));
            }
            // No full upload: the parts may make one. With a full upload they get no note.
            if !parts.is_empty() {
                let part_notes =
                    match joined::join(parts, "the film", floor, &channels, metadata).await? {
                        Joined::Release(mut choice, part_notes) => {
                            choice.language = title_language(&choice.title);
                            desired.insert((), choice);
                            part_notes
                        }
                        Joined::Wait(part_notes) => part_notes,
                    };
                notes.extend(part_notes.into_iter().map(|n| NewUnmatched {
                    video_id: n.video_id,
                    state: n.state,
                    season: None,
                    episode: None,
                    part: Some(n.part),
                    reason: n.reason,
                }));
            }
        } else {
            // Short uploads next to a full one are clips with no tell-tale word. Not worth a note.
            let mut ranked = Vec::with_capacity(long.len());
            for u in long {
                let known = metadata.known(&u.cand.video_id).await?;
                let height = super::height_or_assumed(metadata, &known, &u.cand.channel_id);
                ranked.push((u, height, known.size));
            }
            ranked.sort_by_key(|(u, height, _)| {
                (
                    channels.iter().position(|c| *c == u.cand.channel_id),
                    std::cmp::Reverse(u.duration.unwrap_or(0)),
                    std::cmp::Reverse(height.unwrap_or(0)),
                    u.cand.published_at,
                    u.cand.video_id.clone(),
                )
            });
            let mut ranked = ranked.into_iter();
            if let Some((best, height, size)) = ranked.next() {
                for (other, ..) in ranked {
                    notes.push(note(
                        other.cand,
                        UnmatchedState::Duplicate,
                        format!("another upload of the film, kept {}", best.cand.video_id),
                    ));
                }
                match (best.duration, height) {
                    (None, _) => notes.push(note(
                        best.cand,
                        UnmatchedState::Pending,
                        "duration unknown until the resolver looks the video up".to_owned(),
                    )),
                    (Some(_), None) => notes.push(note(
                        best.cand,
                        UnmatchedState::Pending,
                        "video height unknown until the resolver looks the video up".to_owned(),
                    )),
                    (Some(_), Some(height)) => {
                        desired.insert(
                            (),
                            Choice {
                                ids: vec![best.cand.video_id.clone()],
                                title: best.cand.title.clone(),
                                published_at: best.cand.published_at,
                                height,
                                size,
                                language: best.language,
                            },
                        );
                    }
                }
            }
        }
    }

    if let Some(pin) = &pinned {
        match release_set::pinned_choice(pin, &on_channels, metadata).await? {
            Ok(mut choice) => {
                choice.language = title_language(&choice.title);
                desired.insert((), choice);
            }
            Err(missing) => notes.extend(missing.into_iter().map(|video_id| NewUnmatched {
                video_id,
                state: UnmatchedState::Pending,
                season: None,
                episode: None,
                part: None,
                reason: "pinned, waiting for the resolver to look the video up".to_owned(),
            })),
        }
    }

    release_set::reconcile(db, film, &pins, &desired, None, report).await?;
    let mut seen = HashSet::new();
    notes.retain(|n| seen.insert(n.video_id.clone()));
    report.notes_changed += db.replace_unmatched(film.id, &notes).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn film(title: &str, year: Option<i64>, excludes: &[&str]) -> Mapping {
        Mapping {
            id: 1,
            kind: MappingKind::Film,
            tvdb_id: None,
            tmdb_id: Some(352_173),
            title: title.to_owned(),
            language: "Tamil".to_owned(),
            channel_ids: vec![],
            episode_regex: None,
            excludes: excludes.iter().map(|s| (*s).to_owned()).collect(),
            min_duration_secs: None,
            active: true,
            arr_id: None,
            year,
            imdb_id: Some("tt4938374".to_owned()),
            utc_offset_mins: None,
            timezone: None,
            playlist_id: None,
            playlist_season: 1,
            playlist_offset: 1,
            playlist_mode: crate::store::PlaylistMode::Source,
            backfill_limit: None,
            title_number: false,
            audio_version: false,
            episode_thumbnails: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn names(title: &str) -> (Vec<String>, Vec<i64>) {
        let r = read_title(title, false);
        (r.names, r.years)
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn goldmines_titles_strip_to_the_name() {
        let cases: &[(&str, &[&str], &[i64])] = &[
            (
                "Theri (2016) Full Hindi Dubbed Movie | Vijay, Samantha, Amy Jackson | Atlee",
                &["theri"],
                &[2016],
            ),
            (
                "Theri Hindi Dubbed Full Movie 4K | Thalapathy Vijay",
                &["theri"],
                &[],
            ),
            ("Theri (4K) Full Movie | Hindi Dubbed", &["theri"], &[]),
            (
                "New South Movie 2023 | Theri | Hindi Dubbed | Vijay",
                &["theri", "2023"],
                &[],
            ),
            (
                "Policeodu (Theri) Full Movie Hindi Dubbed | Vijay",
                &["policeodu", "theri"],
                &[],
            ),
            (
                "Theri - New Released South Indian Hindi Dubbed Movie 2016 | Vijay",
                &["theri"],
                &[2016],
            ),
            (
                "Kaththi Full Movie Dubbed In Hindi | Vijay, Samantha",
                &["kaththi"],
                &[],
            ),
            (
                "Sarrainodu (2016) New Released Hindi Dubbed Full Movie | Allu Arjun",
                &["sarrainodu"],
                &[2016],
            ),
            (
                "Baahubali 2 The Conclusion Full Movie [Hindi] HD",
                &["baahubali 2 the conclusion"],
                &[],
            ),
            (
                "Mersal (2017) | World Television Premiere",
                &["mersal"],
                &[2017],
            ),
            ("1920 Full Movie | Horror", &["horror", "1920"], &[]),
            ("English Vinglish Full Movie", &["english vinglish"], &[]),
            (
                "Hindi Medium (2017) Full Movie | Irrfan Khan",
                &["hindi medium"],
                &[2017],
            ),
            ("| Full Movie |", &[], &[]),
        ];
        for (title, want_names, want_years) in cases {
            assert_eq!(
                names(title),
                (s(want_names), want_years.to_vec()),
                "{title}"
            );
        }
    }

    #[test]
    fn language_from_title_words() {
        let cases: &[(&str, Option<&str>)] = &[
            (
                "Theri (2016) Full Hindi Dubbed Movie | Vijay",
                Some("Hindi"),
            ),
            ("Kaththi Full Movie Dubbed In Hindi", Some("Hindi")),
            ("Baahubali 2 Full Movie [Hindi] HD", Some("Hindi")),
            ("Theri Tamil Full Movie", Some("Tamil")),
            ("Jawan Full Movie (Urdu)", Some("Urdu")),
            ("English Vinglish Full Movie", None),
            ("Theri Full Movie 4K", None),
        ];
        for (title, want) in cases {
            assert_eq!(title_language(title).as_deref(), *want, "{title}");
        }
    }

    #[test]
    fn rules_on_goldmines_titles() {
        let theri = film("Theri", Some(2016), &[]);
        let rules = FilmRules::new(&theri, &s(&["Policeodu"]), 0.9).unwrap();
        let yes = |years: &[i64], part| FilmMatch::Film {
            years: years.to_vec(),
            part,
        };
        let cases: &[(&str, FilmMatch)] = &[
            (
                "Theri (2016) Full Hindi Dubbed Movie | Vijay, Samantha | Atlee",
                yes(&[2016], None),
            ),
            ("Theri Hindi Dubbed Full Movie 4K", yes(&[], None)),
            ("Policeodu Full Movie Hindi Dubbed | Vijay", yes(&[], None)),
            ("Theri Full Movie Part 2 | Hindi Dubbed", yes(&[], Some(2))),
            // Remake years are checked by the matcher, the rules only report them.
            ("Theri (2010) Full Movie", yes(&[2010], None)),
            // Extras.
            ("Theri Official Trailer | Vijay", FilmMatch::NotThisFilm),
            ("Theri Hindi Dubbed Teaser", FilmMatch::NotThisFilm),
            (
                "Jithu Jilladi Video Song | Theri | Vijay",
                FilmMatch::NotThisFilm,
            ),
            (
                "Theri Best Fight Scene | Hindi Dubbed",
                FilmMatch::NotThisFilm,
            ),
            ("Theri Movie Review", FilmMatch::NotThisFilm),
            ("Making of Theri", FilmMatch::NotThisFilm),
            // Other films, and an actor list that happens to share a word.
            ("Theri 2 Full Movie Hindi Dubbed", FilmMatch::NotThisFilm),
            (
                "Mersal Full Movie | Vijay, Theri team",
                FilmMatch::NotThisFilm,
            ),
            ("Kaththi Full Movie Hindi Dubbed", FilmMatch::NotThisFilm),
        ];
        for (title, want) in cases {
            assert_eq!(&rules.read(title), want, "{title}");
        }
    }

    #[test]
    fn near_misses_are_unsure_and_aliases_settle_them() {
        let m = film("Baahubali: The Beginning", Some(2015), &[]);
        let rules = FilmRules::new(&m, &[], 0.9).unwrap();
        assert_eq!(
            rules.read("Bahubali The Beginning Full Movie Hindi Dubbed"),
            FilmMatch::Film {
                years: vec![],
                part: None
            },
            "one letter over 21 is within 0.9"
        );
        let pushpa = film("Pushpa", Some(2021), &[]);
        let rules = FilmRules::new(&pushpa, &[], 0.9).unwrap();
        let FilmMatch::Near { name, score } = rules.read("Puspa Full Movie Hindi Dubbed") else {
            panic!("a close transliteration is noted");
        };
        assert_eq!(name, "pushpa");
        assert!(score > 0.8 && score < 0.9, "{score}");
        // Too far apart for a note, until an alias names it.
        let kaththi = film("Kaththi", Some(2014), &[]);
        let rules = FilmRules::new(&kaththi, &[], 0.9).unwrap();
        assert_eq!(
            rules.read("Kathi Full Movie Hindi Dubbed"),
            FilmMatch::NotThisFilm
        );
        let rules = FilmRules::new(&kaththi, &s(&["Kathi"]), 0.9).unwrap();
        assert!(matches!(
            rules.read("Kathi Full Movie Hindi Dubbed"),
            FilmMatch::Film { .. }
        ));
    }

    #[test]
    fn excludes_skip_words_of_the_films_own_name() {
        // A film called "Climax" is not ruled out by the Climax exclude.
        let m = film("Climax", Some(2021), &["Recap"]);
        let rules = FilmRules::new(&m, &[], 0.9).unwrap();
        assert!(matches!(
            rules.read("Climax Full Movie"),
            FilmMatch::Film { .. }
        ));
        assert_eq!(rules.read("Climax Recap"), FilmMatch::NotThisFilm);
        assert_eq!(rules.read("Climax Trailer"), FilmMatch::NotThisFilm);
    }

    #[test]
    fn a_part_in_the_films_own_name_is_not_a_split() {
        let m = film("Ponniyin Selvan: Part Two", Some(2023), &[]);
        let rules = FilmRules::new(&m, &s(&["Ponniyin Selvan Part 2"]), 0.9).unwrap();
        assert_eq!(
            rules.read("Ponniyin Selvan Part 2 Full Movie Hindi Dubbed"),
            FilmMatch::Film {
                years: vec![],
                part: None
            }
        );
    }

    #[test]
    fn similarity_ignores_spaces() {
        assert!((similarity("theri", "theri") - 1.0).abs() < f64::EPSILON);
        assert!((similarity("baahu bali", "baahubali") - 1.0).abs() < f64::EPSILON);
        assert!(similarity("kathi", "kaththi") < 0.9);
        assert!(similarity("", "") < f64::EPSILON);
    }

    #[test]
    fn settings_are_checked() {
        assert!(FilmSettings::default().check().is_ok());
        let bad = |edit: fn(&mut FilmSettings)| {
            let mut s = FilmSettings::default();
            edit(&mut s);
            s.check().unwrap_err().0
        };
        assert_eq!(bad(|s| s.similarity = 0.2), "films.similarity");
        assert_eq!(
            bad(|s| s.min_duration = Duration::from_secs(5)),
            "films.min_duration"
        );
        assert_eq!(
            bad(|s| s.channels = vec!["UCx".to_owned(), "UCx".to_owned()]),
            "films.channels"
        );
        assert_eq!(
            bad(|s| s.channels = vec!["UC x".to_owned()]),
            "films.channels"
        );
    }
}
