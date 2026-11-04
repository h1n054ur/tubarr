//! Reading a YouTube title: does it belong to the show, is it an extra, which episode and part.

use std::{collections::HashMap, sync::LazyLock};

use regex::Regex;

use crate::store::Mapping;

/// Title words that mark extras rather than episodes. Matched case-insensitively as whole words,
/// with an optional plural `s`. The mapping's own excludes are added to these.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "Teaser",
    "Promo",
    "Highlights",
    "Trailer",
    "Best Scene",
    "BTS",
    "Review",
    "Reaction",
    "Shorts",
];

/// The floor when the mapping sets none: the live test saw 15 to 17 minute "Highlights" uploads.
pub const DEFAULT_MIN_DURATION_SECS: i64 = 25 * 60;

/// ARY style titles: "Kabhi Main Kabhi Tum - Episode 1 | ...", "Ep 25", "Season 2 Episode 3".
pub const DEFAULT_EPISODE_REGEX: &str =
    r"(?i)\b(?:season\s*(?P<season>\d{1,2})\W{0,3})?(?:episode|ep)\.?\s*#?\s*(?P<ep>\d{1,4})\b";

/// Web series style, tried before [`DEFAULT_EPISODE_REGEX`]: "S01 E01", "S01E02", "S3E02",
/// "S2 Ep 4", "S2 | EP6". The season and episode must sit together (at most three separator
/// characters apart), so "S01" alone or "S01 recap" is no hit.
pub const SEASON_EPISODE_REGEX: &str =
    r"(?i)\bS(?P<season>\d{1,2})\W{0,3}E(?:p(?:isode)?)?\.?\s*(?P<ep>\d{1,4})\b";

/// A bare "E01" between separators ("Show | E01 | Title", "Show - E3 - Title"), tried last.
/// Needs a separator or the title's edge on both sides, so "E01" inside "S01E01" or a code like
/// "E01X" is no hit.
pub const BARE_EPISODE_REGEX: &str =
    r"(?i)(?:^|[\s|:\-\x{2013}\x{2014}(\[,])E(?P<ep>\d{1,3})(?:$|[\s|:\-\x{2013}\x{2014})\],])";

/// Every "Episode N" in a title, to spot titles naming more than one episode.
static MENTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:episode|ep)\.?\s*#?\s*(\d{1,4})\b").expect("valid mention regex")
});
/// "Episode 2 of 3": one episode, never two.
static OF_TOTAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*of\s+\d{1,4}\b").expect("valid of-total regex"));
/// "Ep 1/3": episode 1 of 3 on a web series, but "Ep 24/25" is a joint upload on a drama
/// channel. Read as a total only when T is the season's episode count in Sonarr.
static SLASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*/\s*(\d{1,4})\b").expect("valid slash regex"));
/// A second number right after the episode number: "24 & 25", "24 and 25", "24/25".
static JOINED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:&|\+|/|,|and\b)\s*(?:(?:episode|ep)\.?\s*)?(\d{1,4})\b")
        .expect("valid joined regex")
});
/// "24 - 25" only counts as two episodes when the second follows the first, so "Episode 25 - 2"
/// or a date after a dash stays one episode.
static DASHED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*-\s*(\d{1,4})\b").expect("valid dashed regex"));
static PART: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bpart\s*[-#]?\s*(?P<part>\d{1,2})\b").expect("valid part regex")
});

/// The rules of one mapping, compiled.
#[derive(Debug)]
pub struct TitleRules {
    /// Tried in order, the first hit wins: the custom regex alone, or the built-in patterns.
    episode: Vec<Regex>,
    /// The normalized series title and aliases. Set only with the default regex: a channel like
    /// ARY Digital carries many dramas, and "Episode 12" alone does not say which. A custom regex
    /// is trusted to name the show itself.
    show: Option<Vec<String>>,
    excludes: Regex,
    /// Episodes per season in Sonarr, to tell "Ep 1/3" (1 of 3) from "Ep 24/25" (a joint upload).
    /// Empty: a slash always means two episodes.
    season_sizes: HashMap<i64, i64>,
}

/// What a title says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleMatch {
    /// Another show, an extra (Promo, Teaser...) or no pattern hit. Not worth a note.
    NotThisShow,
    /// The show is named but no episode number, like "Last Episode".
    NoNumber,
    /// More than one episode named, like "Episode 24 & 25".
    Several(Vec<i64>),
    Episode {
        season: i64,
        episode: i64,
        part: Option<i64>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    #[error("episode regex does not compile: {0}")]
    Regex(#[from] regex::Error),
    #[error("episode regex needs a named group `ep` (or `episode`)")]
    NoEpisodeGroup,
}

impl TitleRules {
    pub fn new(mapping: &Mapping) -> Result<Self, RulesError> {
        let (episode, show) = match &mapping.episode_regex {
            Some(custom) => {
                let re = Regex::new(custom)?;
                if !re
                    .capture_names()
                    .flatten()
                    .any(|n| n == "ep" || n == "episode")
                {
                    return Err(RulesError::NoEpisodeGroup);
                }
                (vec![re], None)
            }
            None => (
                vec![
                    Regex::new(SEASON_EPISODE_REGEX)?,
                    Regex::new(DEFAULT_EPISODE_REGEX)?,
                    Regex::new(BARE_EPISODE_REGEX)?,
                ],
                Some(show_names(&mapping.title, &[])),
            ),
        };
        Ok(Self {
            episode,
            show,
            excludes: excludes(mapping)?,
            season_sizes: HashMap::new(),
        })
    }

    /// Also accepts titles naming one of `aliases` instead of the series title, for shows YouTube
    /// calls something else than TVDB does. No effect with a custom regex.
    pub fn with_aliases(mut self, aliases: &[String]) -> Self {
        if let Some(show) = &mut self.show {
            *show = show_names(&show[0], aliases);
        }
        self
    }

    /// Gives the number of episodes Sonarr has in each season, so "Ep N/T" reads as episode N of
    /// T when T is that season's count. Without it "N/T" stays two episodes.
    pub fn with_season_sizes(mut self, sizes: HashMap<i64, i64>) -> Self {
        self.season_sizes = sizes;
        self
    }

    pub fn read(&self, title: &str) -> TitleMatch {
        if self.excludes.is_match(title) {
            return TitleMatch::NotThisShow;
        }
        if let Some(show) = &self.show
            && !names_any(title, show)
        {
            return TitleMatch::NotThisShow;
        }
        let Some(caps) = self.episode.iter().find_map(|re| re.captures(title)) else {
            // With a custom regex a miss may well be another show, so it gets no note.
            return if self.show.is_some() {
                TitleMatch::NoNumber
            } else {
                TitleMatch::NotThisShow
            };
        };
        let Some(ep) = caps.name("ep").or_else(|| caps.name("episode")) else {
            return TitleMatch::NotThisShow;
        };
        let Ok(episode) = ep.as_str().parse::<i64>() else {
            return TitleMatch::NotThisShow;
        };

        let mut named: Vec<i64> = MENTION
            .captures_iter(title)
            .filter_map(|c| c[1].parse().ok())
            .collect();
        named.push(episode);
        let season = caps
            .name("season")
            .and_then(|s| s.as_str().parse().ok())
            .unwrap_or(1);
        let rest = &title[ep.end()..];
        let of_total = OF_TOTAL.is_match(rest)
            || SLASH
                .captures(rest)
                .and_then(|c| c[1].parse::<i64>().ok())
                .is_some_and(|total| {
                    total >= episode && self.season_sizes.get(&season) == Some(&total)
                });
        if of_total {
            // "Ep 1/3" on a three episode season: episode 1 of 3.
        } else if let Some(next) = JOINED.captures(rest).and_then(|c| c[1].parse::<i64>().ok()) {
            named.push(next);
        } else if let Some(next) = DASHED
            .captures(rest)
            .and_then(|c| c[1].parse::<i64>().ok())
            .filter(|n| *n == episode + 1)
        {
            named.push(next);
        }
        named.sort_unstable();
        named.dedup();
        if named.len() > 1 {
            return TitleMatch::Several(named);
        }

        let part = caps
            .name("part")
            .or_else(|| PART.captures(title).and_then(|c| c.name("part")))
            .and_then(|p| p.as_str().parse().ok());
        TitleMatch::Episode {
            season,
            episode,
            part,
        }
    }
}

/// The default excludes plus the mapping's own, as one case-insensitive whole-word regex.
pub(crate) fn excludes(mapping: &Mapping) -> Result<Regex, regex::Error> {
    let words: Vec<String> = DEFAULT_EXCLUDES
        .iter()
        .copied()
        .chain(mapping.excludes.iter().map(String::as_str))
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(|w| regex::escape(w).replace(' ', r"\s+"))
        .collect();
    Regex::new(&format!(r"(?i)\b(?:{})s?\b", words.join("|")))
}

/// The normalized series title first, then the normalized aliases, without blanks or repeats.
pub(crate) fn show_names(title: &str, aliases: &[String]) -> Vec<String> {
    let mut names = vec![normalize(title)];
    for alias in aliases.iter().map(|a| normalize(a)) {
        if !alias.is_empty() && !names.contains(&alias) {
            names.push(alias);
        }
    }
    names
}

/// Whether `title` names one of `names` (from [`show_names`]) as whole words.
pub(crate) fn names_any(title: &str, names: &[String]) -> bool {
    let title = format!(" {} ", normalize(title));
    names.iter().any(|n| title.contains(&format!(" {n} ")))
}

/// Lower case, letters and digits only, single spaces.
pub(crate) fn normalize(s: &str) -> String {
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

/// "Eng Sub" uploads carry burned-in subtitles. The plan prefers the clean upload.
pub fn has_burned_subs(title: &str) -> bool {
    static ENG_SUB: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:eng(?:lish)?\s*sub(?:s|titles?)?)\b").expect("valid eng sub regex")
    });
    ENG_SUB.is_match(title)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MappingKind;

    fn mapping(regex: Option<&str>, excludes: &[&str]) -> Mapping {
        Mapping {
            id: 1,
            kind: MappingKind::Show,
            tvdb_id: Some(452_001),
            tmdb_id: None,
            title: "Kabhi Main Kabhi Tum".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec!["UCary".to_owned()],
            episode_regex: regex.map(str::to_owned),
            excludes: excludes.iter().map(|s| (*s).to_owned()).collect(),
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
            playlist_mode: crate::store::PlaylistMode::Source,
            backfill_limit: None,
            title_number: false,
            audio_version: false,
            episode_thumbnails: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn ep(season: i64, episode: i64, part: Option<i64>) -> TitleMatch {
        TitleMatch::Episode {
            season,
            episode,
            part,
        }
    }

    #[test]
    fn default_rules_on_ary_titles() {
        use TitleMatch::{NoNumber, NotThisShow, Several};
        let rules = TitleRules::new(&mapping(None, &[])).unwrap();
        let cases: &[(&str, TitleMatch)] = &[
            (
                "Kabhi Main Kabhi Tum - Episode 1 | Fahad Mustafa | Hania Aamir | ARY Digital",
                ep(1, 1, None),
            ),
            ("Kabhi Main Kabhi Tum Ep 25 | ARY Digital", ep(1, 25, None)),
            (
                "Kabhi Main Kabhi Tum - Episode 25 - [Eng Sub] - 24th July 2024",
                ep(1, 25, None),
            ),
            ("Kabhi Main Kabhi Tum Episode 07", ep(1, 7, None)),
            ("KABHI MAIN KABHI TUM - EP.12", ep(1, 12, None)),
            (
                "Kabhi Main Kabhi Tum - Episode 25 - Presented by Happilac Paints",
                ep(1, 25, None),
            ),
            (
                "Kabhi Main Kabhi Tum Season 2 Episode 3 | ARY",
                ep(2, 3, None),
            ),
            (
                "Kabhi Main Kabhi Tum - Episode 25 - Part 2",
                ep(1, 25, Some(2)),
            ),
            ("Kabhi Main Kabhi Tum Episode 25 Part-1", ep(1, 25, Some(1))),
            // Dash then a date or another number that is not the next episode: one episode.
            ("Kabhi Main Kabhi Tum - Episode 25 - 2", ep(1, 25, None)),
            // Traps.
            ("Kabhi Main Kabhi Tum - Episode 25 Promo | ARY", NotThisShow),
            ("Kabhi Main Kabhi Tum - Episode 25 Teaser", NotThisShow),
            ("Kabhi Main Kabhi Tum Last Episode Promo", NotThisShow),
            ("Kabhi Main Kabhi Tum Ep 25 - Highlights", NotThisShow),
            ("Kabhi Main Kabhi Tum Episode 25 Best Scenes", NotThisShow),
            ("Kabhi Main Kabhi Tum Episode 25 - Review", NotThisShow),
            ("Kabhi Main Kabhi Tum Episode 25 BTS", NotThisShow),
            ("Kabhi Main Kabhi Tum Ep 25 #shorts", NotThisShow),
            ("Kabhi Main Kabhi Tum - Official Trailer", NotThisShow),
            ("Kabhi Main Kabhi Tum Episode 25 Reaction", NotThisShow),
            ("Teri Chhaon Mein - Episode 25 | ARY Digital", NotThisShow),
            (
                "Kabhi Main Kabhi Tum - Last Episode | ARY Digital",
                NoNumber,
            ),
            ("Kabhi Main Kabhi Tum - 2nd Last Episode", NoNumber),
            (
                "Kabhi Main Kabhi Tum - Episode 24 & 25 | ARY Digital",
                Several(vec![24, 25]),
            ),
            (
                "Kabhi Main Kabhi Tum Episode 24 and Episode 25",
                Several(vec![24, 25]),
            ),
            ("Kabhi Main Kabhi Tum Ep 24 - 25", Several(vec![24, 25])),
        ];
        for (title, want) in cases {
            assert_eq!(&rules.read(title), want, "{title}");
        }
    }

    /// Default rules for `title`, with Sonarr's season sizes as in the test titles: three
    /// episodes in season 1.
    fn rules_for(title: &str) -> TitleRules {
        let mut m = mapping(None, &[]);
        m.title = title.to_owned();
        TitleRules::new(&m)
            .unwrap()
            .with_season_sizes(HashMap::from([(1, 3)]))
    }

    #[test]
    fn web_series_numbering() {
        let cases: &[(&str, &str, TitleMatch)] = &[
            (
                "Kota Factory",
                "Kota Factory - S01 E01 - Inventory",
                ep(1, 1, None),
            ),
            (
                "Kota Factory",
                "Kota Factory - S02 E05 - Rasoi | TVF",
                ep(2, 5, None),
            ),
            (
                "Aspirants",
                "TVF's Aspirants | S01 E01 | Watch all episodes",
                ep(1, 1, None),
            ),
            (
                "Little Things",
                "Dice Media | Little Things | S01E02 - Busy Bees",
                ep(1, 2, None),
            ),
            (
                "Adulting",
                "Adulting | S3E02 - Gossip Girls",
                ep(3, 2, None),
            ),
            ("Adulting", "Adulting | S2 Ep 4 | Girliyapa", ep(2, 4, None)),
            (
                "Please Find Attached",
                "Please Find Attached | Ep 1/3 | Girliyapa",
                ep(1, 1, None),
            ),
            (
                "Please Find Attached",
                "Please Find Attached | Ep 2/3 | Girliyapa",
                ep(1, 2, None),
            ),
            (
                "Please Find Attached",
                "Please Find Attached | Ep 3/3 | Girliyapa",
                ep(1, 3, None),
            ),
            (
                "Please Find Attached",
                "Please Find Attached | Episode 2 of 3",
                ep(1, 2, None),
            ),
            (
                "Very Parivarik",
                "Very Parivarik S2 | EP6 | Village (Part 1) | TVF",
                ep(2, 6, Some(1)),
            ),
            (
                "Very Parivarik",
                "Very Parivarik | EP7 | Village (Part 2)",
                ep(1, 7, Some(2)),
            ),
            ("Kota Factory", "Kota Factory | E03 | Lunch", ep(1, 3, None)),
            ("Kota Factory", "Kota Factory - E4", ep(1, 4, None)),
            // Negatives.
            (
                "Kota Factory",
                "Kota Factory Season 3 | Official Trailer",
                TitleMatch::NotThisShow,
            ),
            // No number: a note at most, never an episode. A numbered clip is left to the
            // duration floor (see the matcher tests).
            (
                "Kota Factory",
                "Kota Factory | Behind the Scenes | S02",
                TitleMatch::NoNumber,
            ),
            (
                "Kota Factory",
                "Kota Factory S01 Recap | TVF",
                TitleMatch::NoNumber,
            ),
            (
                "Kota Factory",
                "Kota Factory S01 | 1080p",
                TitleMatch::NoNumber,
            ),
            (
                "Kota Factory",
                "Kota Factory 2019 | S01",
                TitleMatch::NoNumber,
            ),
            ("Kota Factory", "Kota Factory Part 2", TitleMatch::NoNumber),
            ("Kota Factory", "Kota Factory S01E01X", TitleMatch::NoNumber),
            (
                "Aspirants",
                "Kota Factory - S01 E01",
                TitleMatch::NotThisShow,
            ),
        ];
        for (show, title, want) in cases {
            assert_eq!(&rules_for(show).read(title), want, "{title}");
        }
    }

    #[test]
    fn a_slash_is_a_total_only_when_it_is_the_season_size() {
        use TitleMatch::Several;
        let pfa = "Please Find Attached | Ep 1/3 | Girliyapa";
        assert_eq!(rules_for("Please Find Attached").read(pfa), ep(1, 1, None));
        // Without Sonarr's episode list the old reading stays: two episodes.
        let mut m = mapping(None, &[]);
        m.title = "Please Find Attached".to_owned();
        let no_sizes = TitleRules::new(&m).unwrap();
        assert_eq!(no_sizes.read(pfa), Several(vec![1, 3]));

        let forty = TitleRules::new(&mapping(None, &[]))
            .unwrap()
            .with_season_sizes(HashMap::from([(1, 40)]));
        assert_eq!(
            forty.read("Kabhi Main Kabhi Tum - Episode 24/25 | ARY Digital"),
            Several(vec![24, 25])
        );
        assert_eq!(
            forty.read("Kabhi Main Kabhi Tum Ep 5/3"),
            Several(vec![3, 5])
        );
        // The size of another season does not count.
        let s2 = TitleRules::new(&m)
            .unwrap()
            .with_season_sizes(HashMap::from([(2, 3)]));
        assert_eq!(s2.read(pfa), Several(vec![1, 3]));
        // "of T" was always one episode.
        assert_eq!(
            no_sizes.read("Please Find Attached | Episode 2 of 3"),
            ep(1, 2, None)
        );
    }

    #[test]
    fn show_title_is_matched_as_whole_words() {
        let rules = TitleRules::new(&mapping(None, &[])).unwrap();
        assert_eq!(rules.read("kabhi-main-kabhi-tum episode 3"), ep(1, 3, None));
        assert_eq!(
            rules.read("Kabhi Main Kabhi Tumhara Episode 3"),
            TitleMatch::NotThisShow
        );
    }

    #[test]
    fn aliases_name_the_show_too() {
        let mut m = mapping(None, &[]);
        m.title = "The Tale".to_owned();
        let aliases = ["Dastaan".to_owned(), "دھوپ کنارے".to_owned()];
        let plain = TitleRules::new(&m).unwrap();
        assert_eq!(
            plain.read("Dastaan Episode 3 | Hum TV"),
            TitleMatch::NotThisShow
        );
        let rules = TitleRules::new(&m).unwrap().with_aliases(&aliases);
        assert_eq!(rules.read("DASTAAN - Ep.3 | Hum TV"), ep(1, 3, None));
        assert_eq!(rules.read("The Tale Episode 4"), ep(1, 4, None));
        assert_eq!(rules.read("دھوپ کنارے | Episode 5 | PTV"), ep(1, 5, None));
        // Whole words, and excludes still win.
        assert_eq!(rules.read("Dastaangoi Episode 3"), TitleMatch::NotThisShow);
        assert_eq!(
            rules.read("Dastaan Episode 3 Promo"),
            TitleMatch::NotThisShow
        );
        // A custom regex is trusted as it is.
        let custom = TitleRules::new(&mapping(Some(r"Qist (?P<ep>\d+)"), &[]))
            .unwrap()
            .with_aliases(&aliases);
        assert_eq!(custom.read("Any Qist 2"), ep(1, 2, None));
    }

    #[test]
    fn mapping_excludes_add_to_the_defaults() {
        let rules = TitleRules::new(&mapping(None, &["Recap", "Making Of"])).unwrap();
        assert_eq!(
            rules.read("Kabhi Main Kabhi Tum Episode 3 Recap"),
            TitleMatch::NotThisShow
        );
        assert_eq!(
            rules.read("Kabhi Main Kabhi Tum Episode 3 making  of"),
            TitleMatch::NotThisShow
        );
        assert_eq!(
            rules.read("Kabhi Main Kabhi Tum Episode 3 Promo"),
            TitleMatch::NotThisShow
        );
        // Word-bounded: "Promotion" is not "Promo".
        assert_eq!(
            rules.read("Kabhi Main Kabhi Tum Episode 3 Promotion Special"),
            ep(1, 3, None)
        );
    }

    #[test]
    fn custom_regex_with_season_and_part_groups() {
        let rules = TitleRules::new(&mapping(
            Some(r"KMKT S(?P<season>\d+) Qist (?P<ep>\d+)(?: Hissa (?P<part>\d+))?"),
            &[],
        ))
        .unwrap();
        assert_eq!(rules.read("KMKT S2 Qist 4"), ep(2, 4, None));
        assert_eq!(rules.read("KMKT S2 Qist 4 Hissa 2"), ep(2, 4, Some(2)));
        // No show title check and no note when a custom regex misses.
        assert_eq!(rules.read("Something else"), TitleMatch::NotThisShow);
        // The old schema name `episode` works too.
        let old = TitleRules::new(&mapping(Some(r"Ep(?:isode)?\s*(?P<episode>\d+)"), &[])).unwrap();
        assert_eq!(old.read("Any show Episode 9"), ep(1, 9, None));
    }

    #[test]
    fn bad_custom_regex_is_refused() {
        assert!(matches!(
            TitleRules::new(&mapping(Some(r"Episode (\d+)"), &[])),
            Err(RulesError::NoEpisodeGroup)
        ));
        assert!(matches!(
            TitleRules::new(&mapping(Some(r"Episode (?P<ep>\d+"), &[])),
            Err(RulesError::Regex(_))
        ));
    }

    #[test]
    fn burned_subs() {
        assert!(has_burned_subs("Episode 25 - [Eng Sub]"));
        assert!(has_burned_subs("Episode 25 English Subtitles"));
        assert!(!has_burned_subs("Episode 25 - Presented by Happilac"));
    }
}
