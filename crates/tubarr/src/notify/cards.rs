//! What each notice says. Plain English for whoever reads the channel: no internal names.

use super::card::{Card, Tone};
use crate::{
    binaries::Tool,
    resolver::{ErrorKind, Step},
    store::UnmatchedState,
};

/// Releases listed on one grabs card.
pub const GRABS_LISTED: usize = 10;

fn step_label(step: Step) -> &'static str {
    match step {
        Step::Plain => "Plain lookup",
        Step::PoToken => "Lookup with a proof of origin token",
        Step::Cookies => "Lookup with the spare account's cookies",
    }
}

fn kind_label(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::InvalidId => "The video id is not valid",
        ErrorKind::Unavailable => "YouTube says the video is unavailable",
        ErrorKind::GeoBlocked => "The video is blocked in this country",
        ErrorKind::RateLimited => "YouTube said too many requests",
        ErrorKind::BotCheck => "YouTube asked for a sign-in check",
        ErrorKind::AgeRestricted => "The video is age restricted",
        ErrorKind::MembersOnly => "The video is for channel members or Premium only",
        ErrorKind::NoFormats => "YouTube offered none of the streams Tubarr plays",
        ErrorKind::Live => "The livestream has not finished processing yet",
        ErrorKind::Timeout => "yt-dlp took too long to answer",
        ErrorKind::Busy => "Lookups were queued too long to wait",
        ErrorKind::Spawn => "yt-dlp could not be started",
        ErrorKind::Parse => "yt-dlp gave an answer Tubarr could not read",
        ErrorKind::Failed => "yt-dlp failed for another reason",
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub struct CanaryFailing<'a> {
    pub video_id: &'a str,
    pub failures: u32,
    pub error: &'a str,
    pub kind: ErrorKind,
    pub step: Option<Step>,
    pub forced_update: bool,
}

pub fn canary_failing(f: &CanaryFailing<'_>, at: u64) -> Card {
    let doing = if f.forced_update {
        "Updating yt-dlp now, since an old yt-dlp is the usual cause"
    } else {
        "yt-dlp was already updated in the last day, so Tubarr keeps checking every hour"
    };
    let mut card = Card::new(
        "canary_failing",
        Tone::Failure,
        "YouTube lookups are failing",
        at,
    )
    .status(format!(
        "{} in a row",
        plural(f.failures.into(), "failed check", "failed checks")
    ))
    .text("The hourly test lookup of a known good video keeps failing, so plays will likely fail too.");
    if let Some(step) = f.step {
        card = card.field("Last step tried", step_label(step));
    }
    card.field("Why", kind_label(f.kind))
        .field("What Tubarr is doing", doing)
        .field("Test video", f.video_id)
        .field("Details", f.error)
        .footer("canary")
}

pub fn canary_recovered(video_id: &str, step: Step, after_failures: u32, at: u64) -> Card {
    let mut card = Card::new(
        "canary_recovered",
        Tone::Success,
        "YouTube lookups work again",
        at,
    )
    .status(format!(
        "after {}",
        plural(after_failures.into(), "failed check", "failed checks")
    ))
    .field("Working step", step_label(step))
    .field("Test video", video_id);
    if step != Step::Plain {
        card = card.text("Lookups start at this step for a while, then try a plain lookup again.");
    }
    card.footer("canary")
}

/// `count` uses since the last cookies card. `first` is the card sent at once for a new burst.
pub fn cookies_used(count: u32, latest_video: &str, first: bool, at: u64) -> Card {
    let status = if first {
        "1 lookup just now".to_owned()
    } else {
        format!(
            "{} in the last hour",
            plural(count.into(), "more lookup", "more lookups")
        )
    };
    Card::new(
        "cookies_used",
        Tone::Warning,
        "Lookups are using the spare account",
        at,
    )
    .status(status)
    .text("YouTube refused the other lookup steps, so Tubarr used the spare account's cookies. YouTube may flag that account if this keeps happening.")
    .field("Latest video", latest_video)
    .footer("lookups")
}

pub fn binary_updated(tool: Tool, from: Option<&str>, to: &str, at: u64) -> Card {
    let status = match from {
        Some(from) => format!("{from} → {to}"),
        None => format!("installed {to}"),
    };
    Card::new(
        "binary_updated",
        Tone::Info,
        format!("{} updated", tool.name()),
        at,
    )
    .status(status)
    .footer("updates")
}

pub fn binary_update_failed(tool: Tool, error: &str, at: u64) -> Card {
    Card::new(
        "binary_update_failed",
        Tone::Failure,
        format!("{} update failed", tool.name()),
        at,
    )
    .text("Tubarr keeps using the version it has and tries again on the next check.")
    .field("Error", error)
    .footer("updates")
}

/// One grab on a grabs card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrabLine {
    pub release_name: String,
    /// The series or film as Sonarr or Radarr has it, when Tubarr knows the release.
    pub title: Option<String>,
    pub category: String,
}

fn category_label(category: &str) -> &'static str {
    match category {
        "tv" => "TV",
        "movies" => "Film",
        _ => "Other",
    }
}

/// `lines` holds at most [`GRABS_LISTED`]; `total` counts every grab in the window.
pub fn grabs(lines: &[GrabLine], total: usize, at: u64) -> Card {
    let title = if total == 1 {
        "Release sent to Sonarr or Radarr".to_owned()
    } else {
        format!("{total} releases sent to Sonarr or Radarr")
    };
    let mut items: Vec<String> = lines
        .iter()
        .map(|l| match &l.title {
            Some(t) => format!("{t} · {} · {}", category_label(&l.category), l.release_name),
            None => format!("{} · {}", category_label(&l.category), l.release_name),
        })
        .collect();
    if total > lines.len() {
        items.push(format!("and {} more", total - lines.len()));
    }
    Card::new("grabs", Tone::Info, title, at)
        .text("Sonarr or Radarr grabbed these and Tubarr wrote their stream files.")
        .list("Releases", items)
        .footer("grabs")
}

fn state_label(state: UnmatchedState) -> &'static str {
    match state {
        UnmatchedState::Pending => "Waiting for video details from YouTube",
        UnmatchedState::Unsure => "Not sure which episode or film it is",
        UnmatchedState::Partial => "One part of a split episode",
        UnmatchedState::Duplicate => "Another upload of the same episode won",
        UnmatchedState::Capped => "Older than the back episodes the backfill limit keeps",
    }
}

/// `counts` is worst first: the most common reason leads.
pub fn unmatched(counts: &[(UnmatchedState, i64)], at: u64) -> Card {
    let total: i64 = counts.iter().map(|(_, n)| n).sum();
    let total = u64::try_from(total).unwrap_or(0);
    Card::new(
        "unmatched",
        Tone::Warning,
        format!(
            "{} waiting for a match",
            plural(total, "video", "videos")
        ),
        at,
    )
    .text("The matcher saw these uploads on mapped channels but has not released them. Pinning a video to its episode or film settles the unsure ones.")
    .list(
        "Why",
        counts
            .iter()
            .take(4)
            .map(|(state, n)| format!("{}: {n}", state_label(*state)))
            .collect(),
    )
    .footer("daily digest")
}
