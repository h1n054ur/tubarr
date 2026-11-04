//! Comparing a title with the names TVDB and TMDB hold. Urdu and Hindi titles have no fixed Latin
//! spelling ("Main" or "Mein", "Mohabbat" or "Muhabbat"), so two names are compared as written and
//! once more folded to a rough sound skeleton.

use unicode_normalization::{UnicodeNormalization, char::is_combining_mark};

use crate::matcher::films::similarity;

/// Lower case, accents dropped, letters and digits only, single spaces. Other scripts are kept.
pub fn normalize(s: &str) -> String {
    s.nfkd()
        .filter(|c| !is_combining_mark(*c))
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

/// The sound skeleton of a romanized title: every vowel run becomes `a`, doubled letters count
/// once, and the common spelling swaps (q/k, w/v, ph/f, z/j) fall together.
pub fn fold(s: &str) -> String {
    normalize(s)
        .split(' ')
        .map(fold_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn fold_word(word: &str) -> String {
    let word = word.replace("ph", "f");
    let mut out = String::with_capacity(word.len());
    for c in word.chars() {
        let c = match c {
            'a' | 'e' | 'i' | 'o' | 'u' => 'a',
            'q' => 'k',
            'w' => 'v',
            'z' => 'j',
            c => c,
        };
        if out.ends_with(c) {
            continue;
        }
        out.push(c);
    }
    out
}

/// How alike two names are, 0 to 1. A match only after folding scores a little below an exact
/// one, so the closest spelling wins.
pub fn score(a: &str, b: &str) -> f64 {
    let plain = similarity(&normalize(a), &normalize(b));
    let folded = similarity(&fold(a), &fold(b)) * 0.97;
    plain.max(folded)
}

/// Is any letter in Arabic script (Urdu) or Devanagari (Hindi)?
pub fn is_native_script(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c as u32, 0x0600..=0x06FF | 0x0750..=0x077F | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF | 0x0900..=0x097F)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spelling_variants_fold_together() {
        assert_eq!(fold("Kabhi Main Kabhi Tum"), fold("Kabhi Mein Kabhi Tum"));
        assert_eq!(fold("Mohabbat"), fold("Muhabbat"));
        assert_eq!(fold("Ishq"), fold("Ishk"));
        assert_eq!(fold("Pehli Si Muhabbat"), fold("Pehli See Mohabat"));
        assert!(score("Kabhi Main Kabhi Tum", "Kabhi Mein Kabhi Tum") > 0.95);
        assert!((score("Tere Bin", "Tere Bin") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn other_titles_stay_apart() {
        assert!(score("Kabhi Main Kabhi Tum", "Teri Chhaon Mein") < 0.5);
        assert!(score("Jaan Nisar", "Jaan e Jahan") < 0.75);
    }

    #[test]
    fn accents_and_punctuation_are_ignored() {
        assert_eq!(normalize("Ẓindagī  Gulzar-Hai!"), "zindagi gulzar hai");
    }

    #[test]
    fn native_script_is_spotted() {
        assert!(is_native_script("کبھی میں کبھی تم"));
        assert!(is_native_script("कभी मैं कभी तुम"));
        assert!(!is_native_script("Kabhi Main Kabhi Tum"));
    }
}
