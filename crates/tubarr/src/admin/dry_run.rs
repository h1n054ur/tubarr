//! "Test against channel": runs a mapping's title rules over the videos already known on its
//! channels and says, per video, what the rules make of it. Nothing is written.
//!
//! This is a preview of the rules, not the whole matcher: it does not know which episodes Sonarr
//! wants, pins, or which of two uploads of an episode would win.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::{
    matcher::title::{
        DEFAULT_MIN_DURATION_SECS, RulesError, TitleMatch, TitleRules, excludes, names_any,
        show_names,
    },
    store::{Mapping, MappingKind, PlaylistMode, VideoOnChannel},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// The rules accept it: an episode of this show (or this film) of full length.
    Matched,
    /// The rules point at the show but cannot say for certain. The matcher would note it.
    Unsure,
    /// Another show, an extra, or no pattern hit.
    Excluded,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VideoVerdict {
    pub video_id: String,
    pub channel_id: String,
    pub title: String,
    pub published_at: i64,
    pub duration_secs: Option<i64>,
    pub verdict: Verdict,
    pub reason: String,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub part: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub matched: usize,
    pub unsure: usize,
    pub excluded: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DryRun {
    pub mapping_id: i64,
    pub counts: Counts,
    pub videos: Vec<VideoVerdict>,
    /// Set when the rules need the show's name and no title on the channel carries it.
    pub name_hint: Option<NameHint>,
}

/// Why nothing can match: TVDB names the show one way, the channel another ("The Joe Rogan
/// Experience" against "Joe Rogan Experience #2569").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NameHint {
    pub message: String,
    /// A shorter form of the title that the channel's titles do use: add it as an alias.
    pub alias: Option<String>,
}

/// A leading article and a trailing "(2009)" or "(US)": the parts TVDB adds and YouTube drops.
static ARTICLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(?:the|a|an)\s+").expect("valid article regex"));
static SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\([^)]*\)\s*$").expect("valid suffix regex"));

#[derive(Debug, thiserror::Error)]
pub enum DryRunError {
    #[error(transparent)]
    Rules(#[from] RulesError),
    #[error("excludes do not compile: {0}")]
    Excludes(#[from] regex::Error),
}

/// Judges every video in `videos` by `mapping`'s rules and `aliases`, newest first.
pub fn run(
    mapping: &Mapping,
    aliases: &[String],
    videos: Vec<VideoOnChannel>,
) -> Result<DryRun, DryRunError> {
    let judge = Judge::new(mapping, aliases)?;
    let name_hint = if judge.needs_name() {
        name_hint(&mapping.title, &judge.names, &videos)
    } else {
        None
    };
    let mut out: Vec<VideoVerdict> = videos
        .into_iter()
        .filter(|v| mapping.channel_ids.contains(&v.channel_id))
        .map(|v| judge.verdict(v))
        .collect();
    out.sort_by(|a, b| {
        b.published_at
            .cmp(&a.published_at)
            .then_with(|| a.video_id.cmp(&b.video_id))
    });
    let mut counts = Counts::default();
    for v in &out {
        match v.verdict {
            Verdict::Matched => counts.matched += 1,
            Verdict::Unsure => counts.unsure += 1,
            Verdict::Excluded => counts.excluded += 1,
        }
    }
    Ok(DryRun {
        mapping_id: mapping.id,
        counts,
        videos: out,
        name_hint,
    })
}

/// A hint when none of `videos` names the show by `names` (the title and aliases).
fn name_hint(title: &str, names: &[String], videos: &[VideoOnChannel]) -> Option<NameHint> {
    if videos.is_empty() || videos.iter().any(|v| names_any(&v.title, names)) {
        return None;
    }
    let bare = ARTICLE.replace(title, "");
    let shorter = [bare.to_string(), SUFFIX.replace(&bare, "").to_string()];
    let alias = shorter
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty() && *s != title.trim())
        .map(|s| {
            let names = show_names(s, &[]);
            let named = videos
                .iter()
                .filter(|v| names_any(&v.title, &names))
                .count();
            (s, named)
        })
        .filter(|(_, named)| *named > 0)
        .max_by_key(|(_, named)| *named)
        .map(|(s, _)| s.to_owned());
    let message = match &alias {
        Some(a) => format!(
            "None of the {} videos names the show \"{title}\", so nothing can match. The titles \
             say \"{a}\": add it as an alias.",
            videos.len()
        ),
        None => format!(
            "None of the {} videos names the show \"{title}\", so nothing can match. Add the \
             name the titles below use as an alias.",
            videos.len()
        ),
    };
    Some(NameHint { message, alias })
}

/// Season, episode and part, as far as the title names them.
type Numbers = (Option<i64>, Option<i64>, Option<i64>);
type Judged = (Verdict, String, Option<Numbers>);

struct Judge<'a> {
    mapping: &'a Mapping,
    rules: TitleRules,
    excludes: Regex,
    /// The normalized title, then the aliases.
    names: Vec<String>,
    floor: i64,
}

impl<'a> Judge<'a> {
    fn new(mapping: &'a Mapping, aliases: &[String]) -> Result<Self, DryRunError> {
        Ok(Self {
            mapping,
            rules: TitleRules::new(mapping)?.with_aliases(aliases),
            excludes: excludes(mapping)?,
            names: show_names(&mapping.title, aliases),
            floor: mapping
                .min_duration_secs
                .unwrap_or(DEFAULT_MIN_DURATION_SECS),
        })
    }

    fn by_order(&self) -> bool {
        self.mapping.playlist_id.is_some() && self.mapping.playlist_mode == PlaylistMode::Order
    }

    /// Read by the episode regex rather than by name alone.
    fn numbered(&self) -> bool {
        self.mapping.kind == MappingKind::Show && !self.by_order() && !self.mapping.title_number
    }

    /// Whether a title must name the show: always, except for a numbered show read by its own
    /// regex.
    fn needs_name(&self) -> bool {
        !self.numbered() || self.mapping.episode_regex.is_none()
    }

    fn verdict(&self, video: VideoOnChannel) -> VideoVerdict {
        let (verdict, reason, numbers) = self.judge(&video.title, video.duration_secs);
        let (season, episode, part) = numbers.unwrap_or_default();
        VideoVerdict {
            video_id: video.video_id,
            channel_id: video.channel_id,
            title: video.title,
            published_at: video.published_at,
            duration_secs: video.duration_secs,
            verdict,
            reason,
            season,
            episode,
            part,
        }
    }

    fn judge(&self, title: &str, duration: Option<i64>) -> Judged {
        if let Some(word) = self.excludes.find(title) {
            return (
                Verdict::Excluded,
                format!("the title has the excluded word \"{}\"", word.as_str()),
                None,
            );
        }
        let by_order = self.by_order();
        if !self.numbered() {
            if !names_any(title, &self.names) {
                return (
                    Verdict::Excluded,
                    "the title does not name the show".to_owned(),
                    None,
                );
            }
            let how = match (self.mapping.kind, by_order) {
                (MappingKind::Film, _) => "names the film",
                (MappingKind::Daily, _) => "names the show, the upload date picks the episode",
                (_, false) => {
                    "names the show, its #number in TVDB's episode titles picks the episode"
                }
                _ => "names the show, the playlist position picks the episode",
            };
            return self.long_enough(duration, how.to_owned(), None);
        }
        match self.rules.read(title) {
            TitleMatch::NotThisShow => {
                let reason = if self.mapping.episode_regex.is_some() {
                    "the episode regex does not match"
                } else {
                    "the title does not name the show with an episode number"
                };
                (Verdict::Excluded, reason.to_owned(), None)
            }
            TitleMatch::NoNumber => (
                Verdict::Unsure,
                "names the show but no episode number".to_owned(),
                None,
            ),
            TitleMatch::Several(eps) => {
                let list: Vec<String> = eps.iter().map(i64::to_string).collect();
                (
                    Verdict::Unsure,
                    format!("names episodes {}", list.join(", ")),
                    None,
                )
            }
            TitleMatch::Episode {
                season,
                episode,
                part,
            } => {
                let numbers = Some((Some(season), Some(episode), part));
                let target = format!("S{season:02}E{episode:02}");
                if let Some(part) = part {
                    return (
                        Verdict::Unsure,
                        format!("part {part} of {target}, joined parts are not released yet"),
                        numbers,
                    );
                }
                self.long_enough(duration, target, numbers)
            }
        }
    }

    fn long_enough(&self, duration: Option<i64>, what: String, numbers: Option<Numbers>) -> Judged {
        match duration {
            Some(d) if d < self.floor => (
                Verdict::Unsure,
                format!(
                    "{what}, but {} min is under the {} min floor",
                    d / 60,
                    self.floor / 60
                ),
                numbers,
            ),
            Some(_) => (Verdict::Matched, what, numbers),
            None => (
                Verdict::Matched,
                format!("{what}, duration unknown until the resolver looks it up"),
                numbers,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping(kind: MappingKind) -> Mapping {
        Mapping {
            id: 7,
            kind,
            tvdb_id: Some(452_001),
            tmdb_id: None,
            title: "Kabhi Main Kabhi Tum".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec!["UCary".to_owned()],
            episode_regex: None,
            excludes: vec!["Recap".to_owned()],
            min_duration_secs: None,
            active: true,
            arr_id: None,
            year: None,
            imdb_id: None,
            utc_offset_mins: None,
            timezone: None,
            playlist_id: None,
            playlist_season: 1,
            playlist_offset: 1,
            playlist_mode: PlaylistMode::Source,
            backfill_limit: None,
            title_number: false,
            audio_version: false,
            episode_thumbnails: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn video(id: &str, title: &str, mins: Option<i64>, at: i64) -> VideoOnChannel {
        VideoOnChannel {
            video_id: id.to_owned(),
            channel_id: "UCary".to_owned(),
            title: title.to_owned(),
            published_at: at,
            published_approx: false,
            duration_secs: mins.map(|m| m * 60),
        }
    }

    #[test]
    fn show_verdicts_and_reasons() {
        let videos = vec![
            video("a", "Kabhi Main Kabhi Tum - Episode 1 | ARY", Some(36), 1),
            video("b", "Kabhi Main Kabhi Tum Episode 2 Teaser", Some(1), 2),
            video("c", "Kabhi Main Kabhi Tum Episode 3", Some(15), 3),
            video("d", "Kabhi Main Kabhi Tum Last Episode", Some(40), 4),
            video("e", "Another Drama Episode 9", Some(40), 5),
            video("f", "Kabhi Main Kabhi Tum Episode 4 Part 2", Some(20), 6),
            video("g", "Kabhi Main Kabhi Tum Recap", Some(40), 7),
            video("x", "Kabhi Main Kabhi Tum Episode 5", Some(40), 8),
        ];
        let mut videos = videos;
        videos[7].channel_id = "UCother".to_owned();
        let run = run(&mapping(MappingKind::Show), &[], videos).unwrap();
        let by_id = |id: &str| run.videos.iter().find(|v| v.video_id == id).unwrap();

        assert_eq!(run.videos.len(), 7, "videos on other channels are left out");
        assert_eq!(run.videos[0].video_id, "g", "newest first");
        assert_eq!(by_id("a").verdict, Verdict::Matched);
        assert_eq!(by_id("a").reason, "S01E01");
        assert_eq!((by_id("a").season, by_id("a").episode), (Some(1), Some(1)));
        assert_eq!(by_id("b").verdict, Verdict::Excluded);
        assert!(by_id("b").reason.contains("Teaser"));
        assert_eq!(by_id("c").verdict, Verdict::Unsure);
        assert!(
            by_id("c")
                .reason
                .contains("15 min is under the 25 min floor")
        );
        assert_eq!(by_id("d").verdict, Verdict::Unsure);
        assert_eq!(by_id("e").verdict, Verdict::Excluded);
        assert_eq!(by_id("f").verdict, Verdict::Unsure);
        assert_eq!(by_id("f").part, Some(2));
        assert!(by_id("g").reason.contains("Recap"));
        assert_eq!(
            run.counts,
            Counts {
                matched: 1,
                unsure: 3,
                excluded: 3
            }
        );
    }

    #[test]
    fn a_source_playlist_still_numbers_by_title_and_order_by_place() {
        let videos = vec![video(
            "a",
            "Kabhi Main Kabhi Tum - Episode 7 | ARY",
            Some(36),
            1,
        )];
        let mut m = mapping(MappingKind::Show);
        m.playlist_id = Some("PLkmktOfficial".to_owned());
        let run_with = |m: &Mapping| run(m, &[], videos.clone()).unwrap().videos.remove(0);
        assert_eq!(run_with(&m).reason, "S01E07");
        m.title_number = true;
        assert!(
            run_with(&m)
                .reason
                .contains("#number in TVDB's episode titles")
        );
        m.title_number = false;
        m.playlist_mode = PlaylistMode::Order;
        let ordered = run_with(&m);
        assert_eq!(ordered.episode, None);
        assert!(
            ordered.reason.contains("playlist position"),
            "{}",
            ordered.reason
        );
    }

    #[test]
    fn films_and_daily_shows_need_the_name_and_the_floor() {
        let mut film = mapping(MappingKind::Film);
        film.title = "Theri".to_owned();
        let run = run(
            &film,
            &[],
            vec![
                video("t", "Theri (2016) Hindi Dubbed Full Movie", None, 1),
                video("u", "Mersal Full Movie", Some(150), 2),
            ],
        )
        .unwrap();
        assert_eq!(run.videos[1].verdict, Verdict::Matched);
        assert!(run.videos[1].reason.contains("duration unknown"));
        assert_eq!(run.videos[0].verdict, Verdict::Excluded);

        let daily = mapping(MappingKind::Daily);
        let run = super::run(
            &daily,
            &[],
            vec![video(
                "d",
                "Kabhi Main Kabhi Tum 14th October 2026",
                Some(40),
                1,
            )],
        )
        .unwrap();
        assert_eq!(run.videos[0].verdict, Verdict::Matched);
        assert!(run.videos[0].reason.contains("upload date"));
    }

    #[test]
    fn aliases_count_for_every_kind() {
        let aliases = ["KMKT".to_owned()];
        for kind in [MappingKind::Show, MappingKind::Daily, MappingKind::Film] {
            let mut m = mapping(kind);
            m.title = "The Tale".to_owned();
            let title = match kind {
                MappingKind::Daily => "KMKT 14th October 2026",
                _ => "KMKT Episode 3",
            };
            let without = run(&m, &[], vec![video("a", title, Some(40), 1)]).unwrap();
            assert_eq!(without.videos[0].verdict, Verdict::Excluded, "{kind:?}");
            let with = run(&m, &aliases, vec![video("a", title, Some(40), 1)]).unwrap();
            assert_eq!(with.videos[0].verdict, Verdict::Matched, "{kind:?}");
        }
    }

    #[test]
    fn a_podcast_named_differently_on_tvdb_gets_an_alias_hint() {
        let mut m = mapping(MappingKind::Show);
        m.title = "The Joe Rogan Experience".to_owned();
        m.title_number = true;
        let videos = vec![
            video(
                "a",
                "Joe Rogan Experience #2569 - Bill Thompson",
                Some(170),
                1,
            ),
            video("b", "Joe Rogan Experience #2568 - Mike Baker", Some(160), 2),
        ];
        let run = run(&m, &[], videos.clone()).unwrap();
        assert_eq!(run.counts.matched, 0);
        let hint = run.name_hint.expect("a hint");
        assert_eq!(hint.alias.as_deref(), Some("Joe Rogan Experience"));
        assert!(
            hint.message.contains("add it as an alias"),
            "{}",
            hint.message
        );

        let aliased = super::run(&m, &["Joe Rogan Experience".to_owned()], videos.clone()).unwrap();
        assert_eq!(aliased.counts.matched, 2);
        assert_eq!(aliased.name_hint, None);

        m.title = "Something Else".to_owned();
        let hint = super::run(&m, &[], videos)
            .unwrap()
            .name_hint
            .expect("a hint");
        assert_eq!(hint.alias, None);
        assert!(hint.message.contains("titles below"), "{}", hint.message);
    }

    #[test]
    fn a_bad_regex_is_an_error() {
        let mut m = mapping(MappingKind::Show);
        m.episode_regex = Some("Episode (\\d+)".to_owned());
        assert!(matches!(
            run(&m, &[], vec![]),
            Err(DryRunError::Rules(RulesError::NoEpisodeGroup))
        ));
    }
}
