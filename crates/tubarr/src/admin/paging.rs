//! `limit` and `offset` for the admin lists that page on the server (Unmatched, Activity), and
//! the escaped `LIKE` pattern for their text search.

use super::AdminError;

/// Rows a page holds when the call does not say.
const DEFAULT_LIMIT: i64 = 25;
/// Most rows one page holds.
const MAX_LIMIT: i64 = 100;

/// A checked page: `limit` in 1 to 100, `offset` at least 0.
#[derive(Debug, Clone, Copy)]
pub struct Page {
    pub limit: i64,
    pub offset: i64,
}

impl Page {
    pub fn new(limit: Option<i64>, offset: Option<i64>) -> Result<Self, AdminError> {
        let limit = limit.unwrap_or(DEFAULT_LIMIT);
        if !(1..=MAX_LIMIT).contains(&limit) {
            return Err(AdminError::Invalid(format!(
                "limit must be 1 to {MAX_LIMIT}, not {limit}"
            )));
        }
        let offset = offset.unwrap_or(0);
        if offset < 0 {
            return Err(AdminError::Invalid(format!(
                "offset must be 0 or more, not {offset}"
            )));
        }
        Ok(Self { limit, offset })
    }
}

/// `%q%` for `LIKE ... ESCAPE '\'`, with `%`, `_` and `\` in the query taken literally. `None`
/// for a blank query. SQLite's `LIKE` ignores case for ASCII letters.
pub fn like_pattern(q: Option<&str>) -> Option<String> {
    let q = q?.trim();
    if q.is_empty() {
        return None;
    }
    let mut pattern = String::with_capacity(q.len() + 2);
    pattern.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('%');
    Some(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_default_and_stay_in_range() {
        let page = Page::new(None, None).unwrap();
        assert_eq!((page.limit, page.offset), (25, 0));
        assert!(Page::new(Some(100), Some(200)).is_ok());
        assert!(Page::new(Some(0), None).is_err());
        assert!(Page::new(Some(101), None).is_err());
        assert!(Page::new(None, Some(-1)).is_err());
    }

    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(like_pattern(Some(" 100% ")).as_deref(), Some("%100\\%%"));
        assert_eq!(
            like_pattern(Some("a_b\\c")).as_deref(),
            Some("%a\\_b\\\\c%")
        );
        assert_eq!(like_pattern(Some("  ")), None);
        assert_eq!(like_pattern(None), None);
    }
}
