//! The one-time code that claims a fresh instance. While no user exists, `/auth/setup` needs it,
//! so whoever reaches an exposed port first cannot make themselves admin. It lives in memory only:
//! printed in the log at start, a new one on every restart, gone once the first admin exists.
//! `TUBARR_SETUP_CODE` sets it instead, for automated installs, and is then never logged.

use std::sync::Mutex;

use crate::api::constant_time_eq;

/// The environment variable that sets the code instead of a random one.
pub const SETUP_CODE_ENV: &str = "TUBARR_SETUP_CODE";

/// No 0, O, 1, I or L, so the code reads back from a log without guessing.
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
/// 24 characters of 31: about 119 random bits.
const GROUPS: usize = 6;
const GROUP_LEN: usize = 4;
/// The shortest code `TUBARR_SETUP_CODE` may set, separators left out.
pub const MIN_PRESET_LEN: usize = 12;

/// Case, spaces and dashes don't count, so `abcd efgh` matches `ABCD-EFGH`.
pub fn normalize(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// A fresh random code, shown as six groups of four: `K7QM-...`.
fn random_code() -> String {
    (0..GROUPS)
        .map(|_| {
            (0..GROUP_LEN)
                .map(|_| char::from(ALPHABET[rand::random_range(..ALPHABET.len())]))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("-")
}

pub struct SetupCode {
    /// Normalized for the comparison. `None` once setup is done.
    code: Mutex<Option<String>>,
    /// The display form of a random code, for the log. `None` when the code came from the env.
    shown: Mutex<Option<String>>,
}

impl std::fmt::Debug for SetupCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetupCode(***)")
    }
}

impl SetupCode {
    /// Takes `preset` when given, else makes a random code.
    pub fn new(preset: Option<&str>) -> Self {
        let (code, shown) = match preset {
            Some(preset) => (normalize(preset), None),
            None => {
                let shown = random_code();
                (normalize(&shown), Some(shown))
            }
        };
        Self {
            code: Mutex::new(Some(code)),
            shown: Mutex::new(shown),
        }
    }

    /// Whether `given` is the code, compared in constant time. Always false once cleared.
    pub fn matches(&self, given: &str) -> bool {
        let given = normalize(given);
        self.lock_code()
            .as_deref()
            .is_some_and(|code| constant_time_eq(code.as_bytes(), given.as_bytes()))
    }

    /// The random code as the log shows it. `None` for a code from the env or once cleared.
    pub fn shown(&self) -> Option<String> {
        if self.lock_code().is_none() {
            return None;
        }
        self.shown.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Ends the code for good: setup is done.
    pub fn clear(&self) {
        *self.lock_code() = None;
        *self.shown.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn lock_code(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        // A plain value: a panic elsewhere cannot leave it inconsistent.
        self.code.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_codes_are_grouped_unambiguous_and_differ() {
        let code = SetupCode::new(None);
        let shown = code.shown().unwrap();
        assert_eq!(shown.len(), GROUPS * GROUP_LEN + GROUPS - 1, "{shown}");
        assert!(shown.split('-').all(|g| g.len() == GROUP_LEN), "{shown}");
        assert!(
            shown
                .chars()
                .all(|c| c == '-' || ALPHABET.contains(&(c as u8))),
            "{shown}"
        );
        assert_ne!(shown, SetupCode::new(None).shown().unwrap());
        assert!(code.matches(&shown));
        assert!(code.matches(&shown.to_lowercase().replace('-', " ")));
        assert!(!code.matches(""));
        assert!(!code.matches(&shown[..shown.len() - 1]));
        code.clear();
        assert!(!code.matches(&shown));
        assert_eq!(code.shown(), None);
    }

    #[test]
    fn a_preset_code_matches_and_is_never_shown() {
        let code = SetupCode::new(Some("abcd-efgh-2345"));
        assert_eq!(code.shown(), None);
        assert!(code.matches("ABCDEFGH2345"));
        assert!(!code.matches("ABCDEFGH2346"));
        assert_eq!(format!("{code:?}"), "SetupCode(***)");
    }
}
