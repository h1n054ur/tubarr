//! Scene-style release names. Sonarr and Radarr match a release by its leading title, so the name
//! starts with the title as the arr has it, never the YouTube title.

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

/// What comes after the title in a release name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleTag<'a> {
    Episode {
        season: i64,
        episode: i64,
    },
    /// `YYYY-MM-DD`.
    AirDate(&'a str),
    Film {
        year: Option<i64>,
    },
}

/// Every Tubarr release is YouTube's H.264 video with AAC audio, joined without re-encoding.
const SOURCE_AND_CODECS: &str = "WEB-DL.AAC2.0.H.264";
const GROUP: &str = "TUBARR";

/// The resolution Sonarr and Radarr read for a frame, by standard tier as scene names give it:
/// a cinematic 1920x804 or a 4:3 1440x1080 is 1080p, 1280x536 is 720p. The arrs know only the
/// standard labels and take anything else (`804p`) for 480p. Without a width the height alone
/// decides, on the same thresholds. A portrait frame is tiered by its long and short sides.
pub fn quality_height(width: Option<u32>, height: u32) -> u32 {
    let (long, short) = match width {
        Some(w) if w > 0 => (w.max(height), w.min(height)),
        _ => (0, height),
    };
    match (long, short) {
        (l, s) if l >= 3200 || s >= 1800 => 2160,
        (l, s) if l >= 1800 || s >= 1000 => 1080,
        (l, s) if l >= 1200 || s >= 700 => 720,
        (l, s) if l >= 1000 || s >= 560 => 576,
        (l, s) if l >= 800 || s >= 440 => 480,
        _ => 360,
    }
}

/// Builds a name such as `Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR`.
/// `height` is already a [`quality_height`] for anything resolved; a raw height left over is
/// tiered by itself so the name always carries a label the arrs know.
pub fn release_title(arr_title: &str, tag: TitleTag<'_>, height: i64, language: &str) -> String {
    let mut parts = vec![dotted(arr_title)];
    match tag {
        TitleTag::Episode { season, episode } => parts.push(format!("S{season:02}E{episode:02}")),
        TitleTag::AirDate(date) => parts.push(date.replace('-', ".")),
        TitleTag::Film { year: Some(year) } => parts.push(year.to_string()),
        TitleTag::Film { year: None } => {}
    }
    let label = u32::try_from(height).map_or(height, |h| i64::from(quality_height(None, h)));
    parts.push(format!("{label}p"));
    parts.push(SOURCE_AND_CODECS.to_owned());
    let language = dotted(language).to_uppercase();
    if !language.is_empty() {
        parts.push(language);
    }
    parts.retain(|p| !p.is_empty());
    format!("{}-{GROUP}", parts.join("."))
}

/// The words of `text` joined by dots: accents dropped, apostrophes removed inside words, `&`
/// spelled `and`, every other run of punctuation or space a single dot. Letters of any script
/// stay, since the arrs compare titles by letters and digits.
pub fn dotted(text: &str) -> String {
    words(text).join(".")
}

/// The words of `text` as [`dotted`] sees them, for comparing a search query with a title.
pub fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    // Decomposing splits `é` into `e` and an accent from the Latin diacritics block, which is
    // dropped. Marks of other scripts (Devanagari vowel signs) are part of the letter and stay.
    let plain: String = text
        .nfkd()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .nfc()
        .collect();
    for c in plain.chars() {
        match c {
            // Marks left now are vowel signs and the like, part of the letter.
            c if c.is_alphanumeric() || is_combining_mark(c) => word.push(c),
            '\'' | '\u{2019}' | '\u{2018}' | '`' => {}
            '&' => {
                flush(&mut word, &mut words);
                words.push("and".to_owned());
            }
            _ => flush(&mut word, &mut words),
        }
    }
    flush(&mut word, &mut words);
    words
}

fn flush(word: &mut String, words: &mut Vec<String>) {
    if !word.is_empty() {
        words.push(std::mem::take(word));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_daily_and_film_names() {
        assert_eq!(
            release_title(
                "Kabhi Main Kabhi Tum",
                TitleTag::Episode {
                    season: 1,
                    episode: 1
                },
                1080,
                "Urdu"
            ),
            "Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR"
        );
        assert_eq!(
            release_title("Show Name", TitleTag::AirDate("2026-10-14"), 720, "Urdu"),
            "Show.Name.2026.10.14.720p.WEB-DL.AAC2.0.H.264.URDU-TUBARR"
        );
        assert_eq!(
            release_title("Theri", TitleTag::Film { year: Some(2016) }, 1080, "Hindi"),
            "Theri.2016.1080p.WEB-DL.AAC2.0.H.264.HINDI-TUBARR"
        );
        assert_eq!(
            release_title("Theri", TitleTag::Film { year: None }, 480, ""),
            "Theri.480p.WEB-DL.AAC2.0.H.264-TUBARR"
        );
    }

    #[test]
    fn resolutions_take_the_standard_tier() {
        let cases = [
            ((1920, 804), 1080),
            ((1920, 800), 1080),
            ((1920, 1036), 1080),
            ((1920, 1080), 1080),
            ((1440, 1080), 1080),
            ((1080, 1920), 1080),
            ((3840, 1608), 2160),
            ((3840, 2160), 2160),
            ((1280, 536), 720),
            ((1280, 720), 720),
            ((960, 720), 720),
            ((1024, 576), 576),
            ((854, 480), 480),
            ((640, 480), 480),
            ((640, 360), 360),
            ((426, 240), 360),
        ];
        for ((w, h), want) in cases {
            assert_eq!(quality_height(Some(w), h), want, "{w}x{h}");
        }
        // Height alone, when the width is unknown.
        for (h, want) in [
            (2160, 2160),
            (1080, 1080),
            (804, 720),
            (720, 720),
            (480, 480),
            (360, 360),
        ] {
            assert_eq!(quality_height(None, h), want, "{h}");
            assert_eq!(quality_height(Some(0), h), want, "0x{h}");
        }
    }

    #[test]
    fn names_carry_only_labels_the_arrs_know() {
        let tag = TitleTag::Episode {
            season: 1,
            episode: 26,
        };
        let height = i64::from(quality_height(Some(1920), 804));
        assert_eq!(
            release_title("Main Manto Nahi Hoon", tag, height, "Urdu"),
            "Main.Manto.Nahi.Hoon.S01E26.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR"
        );
        // A raw height that slipped through still becomes a standard label.
        assert_eq!(
            release_title("Show", tag, 1036, ""),
            "Show.S01E26.1080p.WEB-DL.AAC2.0.H.264-TUBARR"
        );
    }

    #[test]
    fn long_runs_keep_three_digit_episodes() {
        let tag = TitleTag::Episode {
            season: 1,
            episode: 123,
        };
        assert_eq!(
            release_title("Bulbulay", tag, 1080, "Urdu"),
            "Bulbulay.S01E123.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR"
        );
    }

    #[test]
    fn punctuation_is_cleaned_like_scene_names() {
        let cases = [
            ("Grey's Anatomy", "Greys.Anatomy"),
            ("Mr. Robot", "Mr.Robot"),
            ("Star Wars: Andor", "Star.Wars.Andor"),
            ("Law & Order", "Law.and.Order"),
            ("Spider-Man: Far From Home", "Spider.Man.Far.From.Home"),
            ("Doctor Who (2005)", "Doctor.Who.2005"),
            ("  Tere  Bin… ", "Tere.Bin"),
            ("Who\u{2019}s Next?", "Whos.Next"),
            ("Mere Paas Tum Ho!", "Mere.Paas.Tum.Ho"),
        ];
        for (title, want) in cases {
            assert_eq!(dotted(title), want, "{title:?}");
        }
    }

    #[test]
    fn non_ascii_titles() {
        // Accents drop, other scripts stay as letters.
        assert_eq!(dotted("Pokémon: Ash’s Journey"), "Pokemon.Ashs.Journey");
        assert_eq!(dotted("Amélie"), "Amelie");
        assert_eq!(dotted("ﬁve ½ men"), "five.1.2.men");
        assert_eq!(dotted("میرے پاس تم ہو"), "میرے.پاس.تم.ہو");
        assert_eq!(dotted("दंगल"), "दंगल");
    }

    #[test]
    fn language_with_spaces_is_one_token() {
        assert_eq!(
            release_title(
                "Show",
                TitleTag::Film { year: Some(2020) },
                1080,
                "Brazilian Portuguese"
            ),
            "Show.2020.1080p.WEB-DL.AAC2.0.H.264.BRAZILIAN.PORTUGUESE-TUBARR"
        );
    }

    #[test]
    fn words_are_compared_case_sensitively_by_the_caller() {
        assert_eq!(
            words("Kabhi Main, Kabhi-Tum"),
            ["Kabhi", "Main", "Kabhi", "Tum"]
        );
    }
}
