//! Dates for daily shows: the air date a title names, and the broadcast date of an upload time.

use std::{ops::Range, sync::LazyLock};

use jiff::{Timestamp, tz::TimeZone};
use regex::{Captures, Regex};
use time::{Date, Month, OffsetDateTime, UtcOffset};

/// Pakistan time, the default for ARY style channels. No DST since 2009.
pub const DEFAULT_UTC_OFFSET_MINS: i64 = 5 * 60;

/// Where a channel's day is reckoned: an IANA time zone, which follows DST (a US podcast is at
/// -05:00 in summer and -06:00 in winter), or a fixed offset from UTC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Zone {
    Named(TimeZone),
    Fixed(UtcOffset),
}

impl Zone {
    /// The zone a mapping or channel sets: `timezone` wins over `offset_mins`, and neither means
    /// Pakistan. `Err` says what is wrong with the one given.
    pub fn new(timezone: Option<&str>, offset_mins: Option<i64>) -> Result<Self, String> {
        if let Some(name) = timezone {
            return TimeZone::get(name)
                .map(Zone::Named)
                .map_err(|_| unknown_timezone(name));
        }
        let mins = offset_mins.unwrap_or(DEFAULT_UTC_OFFSET_MINS);
        i32::try_from(mins * 60)
            .ok()
            .and_then(|secs| UtcOffset::from_whole_seconds(secs).ok())
            .map(Zone::Fixed)
            .ok_or_else(|| format!("utc_offset_mins {mins} is out of range"))
    }

    /// The offset from UTC at `published_at`.
    fn offset_at(&self, published_at: i64) -> Option<UtcOffset> {
        match self {
            Zone::Fixed(offset) => Some(*offset),
            Zone::Named(tz) => {
                let at = Timestamp::from_second(published_at).ok()?;
                UtcOffset::from_whole_seconds(tz.to_offset(at).seconds()).ok()
            }
        }
    }
}

/// The IANA name as the time zone database spells it ("america/chicago" is "America/Chicago").
/// `Err` for a name the database does not know, ready to show to the user.
pub fn timezone_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    let tz = TimeZone::get(name).map_err(|_| unknown_timezone(name))?;
    Ok(tz.iana_name().unwrap_or(name).to_owned())
}

fn unknown_timezone(name: &str) -> String {
    format!("{name:?} is not a time zone: use an IANA name like Europe/London or America/Chicago")
}

/// Uploads before this local hour belong to the previous day's broadcast: a late show that airs
/// at 11 pm is often on YouTube after midnight.
pub const BROADCAST_DAY_STARTS_AT_HOUR: u8 = 6;

const MONTH: &str = r"(?P<month>jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sep(?:t(?:ember)?)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)";
const DAY: &str = r"(?P<day>\d{1,2})(?:st|nd|rd|th)?";

/// "14th October 2026", "14 Oct 2026", "14-Oct-26", "14th of October, 2026".
static DAY_MONTH_YEAR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)\b{DAY}(?:\s+of)?[\s\-./,]+{MONTH}\.?[\s\-./,]+(?P<year>\d{{4}}|\d{{2}})\b"
    ))
    .expect("valid day month year regex")
});
/// "Oct 14, 2026", "October 14th 2026".
static MONTH_DAY_YEAR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?i)\b{MONTH}\.?[\s\-./]+{DAY},?[\s\-./]+(?P<year>\d{{4}})\b"
    ))
    .expect("valid month day year regex")
});
/// "14-10-2026", "14/10/26", "14.10.2026": day first, as Pakistani and Indian channels write it.
static NUMERIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?P<day>\d{1,2})[-/.](?P<month>\d{1,2})[-/.](?P<year>\d{4}|\d{2})\b")
        .expect("valid numeric date regex")
});
/// "2026-10-14".
static ISO: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?P<year>\d{4})-(?P<month>\d{1,2})-(?P<day>\d{1,2})\b").expect("valid iso regex")
});

/// What a title says about its air date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleDate {
    None,
    One(Date),
    /// Two different dates, like a recap of last week in today's show.
    Several(Vec<Date>),
}

/// Reads the air date from a title. A day and month without a year is not a date.
pub fn title_date(title: &str) -> TitleDate {
    let mut dates: Vec<Date> = date_spans(title).into_iter().map(|(_, d)| d).collect();
    dates.sort_unstable();
    dates.dedup();
    match dates.as_slice() {
        [] => TitleDate::None,
        [one] => TitleDate::One(*one),
        _ => TitleDate::Several(dates),
    }
}

/// The title with every date taken out, so two uploads of the same show on different days, or
/// with the date written differently, compare equal.
pub fn without_dates(title: &str) -> String {
    let mut spans: Vec<Range<usize>> = date_spans(title).into_iter().map(|(s, _)| s).collect();
    spans.sort_by_key(|s| s.start);
    let mut out = String::with_capacity(title.len());
    let mut at = 0;
    for span in spans {
        if span.start >= at {
            out.push_str(&title[at..span.start]);
            out.push(' ');
            at = span.end;
        }
    }
    out.push_str(&title[at..]);
    out
}

fn date_spans(title: &str) -> Vec<(Range<usize>, Date)> {
    let mut found = Vec::new();
    for re in [&*DAY_MONTH_YEAR, &*MONTH_DAY_YEAR, &*ISO, &*NUMERIC] {
        for caps in re.captures_iter(title) {
            let Some(whole) = caps.get(0) else { continue };
            // An ISO date also looks like a numeric one further in: keep the first reading.
            if found
                .iter()
                .any(|(s, _): &(Range<usize>, Date)| s.start < whole.end() && whole.start() < s.end)
            {
                continue;
            }
            if let Some(date) = from_caps(&caps) {
                found.push((whole.range(), date));
            }
        }
    }
    found
}

fn from_caps(caps: &Captures<'_>) -> Option<Date> {
    let day: u8 = caps.name("day")?.as_str().parse().ok()?;
    let month = caps.name("month")?.as_str();
    let month = match month.parse::<u8>() {
        Ok(n) => Month::try_from(n).ok()?,
        Err(_) => month_name(month)?,
    };
    let year = caps.name("year")?.as_str();
    let year: i32 = match year.len() {
        2 => 2000 + year.parse::<i32>().ok()?,
        _ => year.parse().ok()?,
    };
    Date::from_calendar_date(year, month, day).ok()
}

fn month_name(s: &str) -> Option<Month> {
    let s = s.to_ascii_lowercase();
    Some(match s.get(..3)? {
        "jan" => Month::January,
        "feb" => Month::February,
        "mar" => Month::March,
        "apr" => Month::April,
        "may" => Month::May,
        "jun" => Month::June,
        "jul" => Month::July,
        "aug" => Month::August,
        "sep" => Month::September,
        "oct" => Month::October,
        "nov" => Month::November,
        "dec" => Month::December,
        _ => return None,
    })
}

/// The broadcast date of an upload: its local date in `zone` at that moment, the previous day
/// before [`BROADCAST_DAY_STARTS_AT_HOUR`].
pub fn broadcast_date(published_at: i64, zone: &Zone) -> Option<Date> {
    let offset = zone.offset_at(published_at)?;
    let local = OffsetDateTime::from_unix_timestamp(published_at)
        .ok()?
        .to_offset(offset);
    if local.hour() < BROADCAST_DAY_STARTS_AT_HOUR {
        local.date().previous_day()
    } else {
        Some(local.date())
    }
}

/// `YYYY-MM-DD`, as TVDB, Sonarr and the releases table write air dates.
pub fn iso(date: Date) -> String {
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(y: i32, m: u8, d: u8) -> Date {
        Date::from_calendar_date(y, Month::try_from(m).unwrap(), d).unwrap()
    }

    #[test]
    fn titles_from_pakistani_and_indian_channels() {
        let oct14 = TitleDate::One(date(2026, 10, 14));
        let cases: &[(&str, TitleDate)] = &[
            (
                "Har Lamha Purjosh | 14th October 2026 | ARY News",
                oct14.clone(),
            ),
            ("Har Lamha Purjosh 14 Oct 2026", oct14.clone()),
            ("Har Lamha Purjosh - 14-Oct-2026", oct14.clone()),
            ("Har Lamha Purjosh 14th of October, 2026", oct14.clone()),
            ("Har Lamha Purjosh | 14-10-2026", oct14.clone()),
            ("Har Lamha Purjosh | 14/10/26", oct14.clone()),
            ("Har Lamha Purjosh | 14.10.2026", oct14.clone()),
            ("Har Lamha Purjosh | Oct 14, 2026", oct14.clone()),
            ("Har Lamha Purjosh | October 14th 2026", oct14.clone()),
            ("Har Lamha Purjosh | 2026-10-14", oct14.clone()),
            (
                "Good Morning Pakistan | 1st Nov 2026",
                TitleDate::One(date(2026, 11, 1)),
            ),
            (
                "Shan-e-Suhoor | 2nd March 2026",
                TitleDate::One(date(2026, 3, 2)),
            ),
            ("Bulletin 23rd Sept 2026", TitleDate::One(date(2026, 9, 23))),
            // The same date written twice is still one date.
            ("Show 14 Oct 2026 (14-10-2026)", oct14.clone()),
            // Day first: 03/04 is the 3rd of April.
            ("Show | 03/04/2026", TitleDate::One(date(2026, 4, 3))),
            // Traps.
            ("Kabhi Main Kabhi Tum - Episode 14", TitleDate::None),
            ("Kabhi Main Kabhi Tum Episode 14 - 10", TitleDate::None),
            ("Best of 2026", TitleDate::None),
            ("Show | 03/04", TitleDate::None),
            ("Show | 14 October", TitleDate::None),
            ("Show | 10:30 PM", TitleDate::None),
            ("Show 31/02/2026", TitleDate::None),
            ("Show 14/13/2026", TitleDate::None),
            ("Mayday | Season 3", TitleDate::None),
            ("Ep 25 - 2", TitleDate::None),
            (
                "Show 14 Oct 2026 recap of 7 Oct 2026",
                TitleDate::Several(vec![date(2026, 10, 7), date(2026, 10, 14)]),
            ),
        ];
        for (title, want) in cases {
            assert_eq!(&title_date(title), want, "{title}");
        }
    }

    #[test]
    fn dates_come_out_of_titles() {
        assert_eq!(
            without_dates("Show | 14th October 2026 | ARY"),
            without_dates("Show | 15 Oct 2026 | ARY")
        );
        assert_eq!(without_dates("Show 2026-10-14 end"), "Show   end");
    }

    fn fixed(mins: i64) -> Zone {
        Zone::new(None, Some(mins)).unwrap()
    }

    fn named(name: &str) -> Zone {
        Zone::new(Some(name), None).unwrap()
    }

    #[test]
    fn uploads_near_midnight_at_plus_five() {
        let plus_five = Zone::new(None, None).unwrap();
        // 2026-10-14 19:10 UTC is 00:10 on the 15th at +05:00: still the 14th's broadcast.
        let late = 1_792_005_000;
        assert_eq!(iso(broadcast_date(late, &plus_five).unwrap()), "2026-10-14");
        // 2026-10-15 00:59 UTC is 05:59 at +05:00: still the 14th.
        assert_eq!(
            broadcast_date(1_792_025_940, &plus_five),
            Some(date(2026, 10, 14))
        );
        // 01:00 UTC is 06:00 at +05:00: the 15th.
        assert_eq!(
            broadcast_date(1_792_026_000, &plus_five),
            Some(date(2026, 10, 15))
        );
        // 2026-10-14 18:50 UTC is 23:50 at +05:00: the 14th.
        assert_eq!(
            broadcast_date(1_792_003_800, &plus_five),
            Some(date(2026, 10, 14))
        );
        // India is half an hour ahead: 00:29 UTC is 05:59 IST, 00:31 is 06:01.
        assert_eq!(
            broadcast_date(1_792_024_140, &fixed(330)),
            Some(date(2026, 10, 14))
        );
        assert_eq!(
            broadcast_date(1_792_024_260, &fixed(330)),
            Some(date(2026, 10, 15))
        );
        // At UTC itself, 05:00 is still the day before.
        assert_eq!(
            broadcast_date(1_792_040_400, &fixed(0)),
            Some(date(2026, 10, 14))
        );
        assert!(Zone::new(None, Some(99_999)).is_err());
        // A +05:00 zone by name (Etc/GMT-5) dates exactly as the fixed +05:00 does.
        for at in [late, 1_792_025_940, 1_792_026_000, 1_792_003_800] {
            assert_eq!(
                broadcast_date(at, &named("Etc/GMT-5")),
                broadcast_date(at, &plus_five)
            );
        }
    }

    #[test]
    fn a_us_podcast_keeps_its_local_day_across_the_dst_change() {
        let chicago = named("America/Chicago");
        // Saturday 2026-10-31 23:30 CDT (-05:00) is 2026-11-01 04:30 UTC.
        assert_eq!(
            broadcast_date(1_793_507_400, &chicago),
            Some(date(2026, 10, 31))
        );
        // Saturday 2026-11-07 23:30 CST (-06:00) is 2026-11-08 05:30 UTC.
        assert_eq!(
            broadcast_date(1_794_115_800, &chicago),
            Some(date(2026, 11, 7))
        );
        // Near the 06:00 day start a fixed offset is an hour off for half the year. 2026-10-31
        // 06:30 CDT is 11:30 UTC: a fixed -06:00 reads 05:30 and dates it the day before.
        assert_eq!(
            broadcast_date(1_793_446_200, &chicago),
            Some(date(2026, 10, 31))
        );
        assert_eq!(
            broadcast_date(1_793_446_200, &fixed(-360)),
            Some(date(2026, 10, 30))
        );
        // 2026-11-07 05:30 CST is 11:30 UTC: a fixed -05:00 reads 06:30 and dates it a day late.
        assert_eq!(
            broadcast_date(1_794_051_000, &chicago),
            Some(date(2026, 11, 6))
        );
        assert_eq!(
            broadcast_date(1_794_051_000, &fixed(-300)),
            Some(date(2026, 11, 7))
        );
    }

    #[test]
    fn the_time_zone_wins_over_the_offset() {
        assert_eq!(
            Zone::new(Some("America/Chicago"), Some(300)).unwrap(),
            named("America/Chicago")
        );
        assert_eq!(Zone::new(None, Some(-360)).unwrap(), fixed(-360));
        let err = Zone::new(Some("Mars/Olympus"), Some(300)).unwrap_err();
        assert!(err.contains("Mars/Olympus"), "{err}");
        assert_eq!(
            timezone_name(" america/chicago ").unwrap(),
            "America/Chicago"
        );
        assert!(timezone_name("Central Time").is_err());
    }
}
