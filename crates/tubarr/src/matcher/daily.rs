//! The matcher for daily shows: Sonarr asks by air date, so each upload is given the date its
//! title names, or else its broadcast date in the channel's timezone. An upload dated by its
//! upload time may land a day off it, on a date Sonarr monitors that no upload has as its own
//! (see [`place`]). An upload whose own date is an unmonitored episode stays on it, unreleased.
//!
//! One release per air date, picked as [`choose`] describes: full episodes over clips, the
//! earliest full upload, its copies ranked as for numbered shows. A later full upload with
//! another title (a second episode, an evening edition) is unsure unless the mapping's regex
//! keeps only one of them.

use std::{
    collections::{BTreeMap, HashSet},
    sync::LazyLock,
};

use regex::Regex;
use time::Date;

use crate::store::{Db, Mapping, NewUnmatched, StoreError, UnmatchedState};

use super::{
    Candidate, Candidates, Metadata, Report, WantedSeries, air_date_secs,
    dates::{TitleDate, Zone, broadcast_date, iso, title_date, without_dates},
    dedup_notes, limit, rank,
    release_set::{Choice, pinned_choice, pins, reconcile},
    title::{DEFAULT_MIN_DURATION_SECS, excludes, names_any, normalize, show_names},
};

/// An upload may come this many days after the date in its title: ARY often uploads late.
const MAX_DAYS_LATE: i64 = 3;
/// And this many days before it, for a title dated by the next morning's clock.
const MAX_DAYS_EARLY: i64 = 1;

/// Which titles belong to the show.
struct DailyRules {
    /// The mapping's regex: a title must match it. With none, the title must name the show (its
    /// title or an alias).
    pattern: Option<Regex>,
    /// The normalized title, then the aliases.
    names: Vec<String>,
    excludes: Regex,
}

impl DailyRules {
    fn new(mapping: &Mapping, aliases: &[String]) -> Result<Self, regex::Error> {
        Ok(Self {
            pattern: mapping
                .episode_regex
                .as_deref()
                .map(Regex::new)
                .transpose()?,
            names: show_names(&mapping.title, aliases),
            excludes: excludes(mapping)?,
        })
    }

    fn is_this_show(&self, title: &str) -> bool {
        if self.excludes.is_match(title) {
            return false;
        }
        match &self.pattern {
            Some(re) => re.is_match(title),
            None => names_any(title, &self.names),
        }
    }
}

/// What is left of a title once dates, subtitle and sponsor tags are gone: equal for uploads of
/// the same show, different for two editions. An alias counts as the show's title, so one upload
/// named by its alias is not a second edition.
fn core_title(title: &str, names: &[String]) -> String {
    static TAGS: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)\b(?:eng(?:lish)?\s*sub(?:s|titles?)?)\b|\b(?:presented|powered|sponsored)\s+by\b.*$",
        )
        .expect("valid tags regex")
    });
    let core = format!(
        " {} ",
        normalize(&TAGS.replace_all(&without_dates(title), " "))
    );
    let core = match names.split_first() {
        Some((show, aliases)) => aliases.iter().fold(core, |core, alias| {
            core.replacen(&format!(" {alias} "), &format!(" {show} "), 1)
        }),
        None => core,
    };
    core.trim().to_owned()
}

pub(super) struct Upload<'a> {
    pub cand: &'a Candidate,
    pub duration: Option<i64>,
}

pub(super) async fn match_one(
    db: &Db,
    series: &WantedSeries,
    candidates: &impl Candidates,
    metadata: &impl Metadata,
    report: &mut Report,
) -> Result<(), StoreError> {
    let mapping = &series.mapping;
    if mapping.channel_ids.is_empty() {
        report
            .skipped
            .push((mapping.id, "no official channels set".to_owned()));
        return Ok(());
    }
    let aliases = db.aliases(mapping.id).await?;
    let rules = match DailyRules::new(mapping, &aliases) {
        Ok(rules) => rules,
        Err(e) => {
            tracing::warn!(mapping = mapping.id, error = %e, "mapping skipped");
            report
                .skipped
                .push((mapping.id, format!("regex does not compile: {e}")));
            return Ok(());
        }
    };
    let Some(zone) = zone(mapping, report) else {
        return Ok(());
    };
    let floor = mapping
        .min_duration_secs
        .unwrap_or(DEFAULT_MIN_DURATION_SECS);
    let listed = Listed::new(
        series
            .episodes
            .iter()
            .filter_map(|e| Some((e.air_date.as_deref()?, e.monitored))),
    );
    let pinned: BTreeMap<String, String> = pins(db, mapping.id).await?;
    let pinned_videos: HashSet<&str> = pinned.values().flat_map(|v| v.split(',')).collect();

    let all = candidates.on_channels(&mapping.channel_ids).await?;
    let on_channels: Vec<&Candidate> = all
        .iter()
        .filter(|c| mapping.channel_ids.contains(&c.channel_id))
        .collect();

    let mut notes: Vec<NewUnmatched> = Vec::new();
    let mut uploads: Vec<Dated<'_>> = Vec::new();
    for cand in &on_channels {
        if pinned_videos.contains(cand.video_id.as_str()) || !rules.is_this_show(&cand.title) {
            continue;
        }
        match date_upload(cand, &zone, &listed, metadata).await? {
            Dating::Dated(d) => uploads.push(d),
            Dating::Unsure(reason) => notes.push(unsure(cand, reason)),
            Dating::Unlisted => report.not_wanted += 1,
        }
    }
    let Placed {
        by_date,
        unmonitored,
        unplaced,
    } = place(uploads, &listed, &pinned, floor);
    report.not_wanted += unplaced;
    notes.extend(
        unmonitored
            .iter()
            .map(|(date, u)| unsure(u.cand, unmonitored_note(date))),
    );

    let mut desired: BTreeMap<String, Choice> = BTreeMap::new();
    for (date, uploads) in by_date {
        if pinned.contains_key(&date) {
            for u in &uploads {
                notes.push(dated(
                    u.cand,
                    UnmatchedState::Duplicate,
                    format!("{date} is pinned to another video"),
                ));
            }
            continue;
        }
        let (choice, picked) = pick(
            Slot::Date(&date),
            uploads,
            floor,
            mapping,
            &rules.names,
            metadata,
        )
        .await?;
        notes.extend(picked);
        if let Some(choice) = choice {
            desired.insert(date, choice);
        }
    }

    for (date, pin) in &pinned {
        match pinned_choice(pin, &on_channels, metadata).await? {
            Ok(choice) => {
                desired.insert(date.clone(), choice);
            }
            Err(missing) => notes.extend(missing.into_iter().map(|video_id| NewUnmatched {
                video_id,
                state: UnmatchedState::Pending,
                season: None,
                episode: None,
                part: None,
                reason: format!("pinned to {date}, waiting for the resolver to look the video up"),
            })),
        }
    }

    // A daily show's target is its air date.
    if let Some(limited) = limit::apply(
        mapping,
        &all,
        &pinned_videos,
        |date: &String| air_date_secs(date),
        &mut desired,
        &mut notes,
    ) {
        limit::drop_capped(db, mapping, &limited.capped, report).await?;
    }
    reconcile(
        db,
        mapping,
        &pinned,
        &desired,
        Some(&listed.dates()),
        report,
    )
    .await?;
    dedup_notes(&mut notes);
    report.notes_changed += db.replace_unmatched(mapping.id, &notes).await?;
    Ok(())
}

/// The air dates Sonarr lists, with a file or not.
pub(super) struct Listed<'a> {
    /// Dates with a monitored episode: an upload only ever lands on one of these.
    wanted: HashSet<&'a str>,
    /// Dates whose episodes are all unmonitored: never released, but an upload with one as its
    /// own date stays on it rather than moving to a neighbour, which is another episode.
    unmonitored: HashSet<&'a str>,
}

impl<'a> Listed<'a> {
    /// From each listed episode's air date and whether Sonarr monitors it.
    pub(super) fn new(dates: impl Iterator<Item = (&'a str, bool)>) -> Self {
        let (wanted, unmonitored): (Vec<_>, Vec<_>) = dates.partition(|(_, monitored)| *monitored);
        let wanted: HashSet<&str> = wanted.into_iter().map(|(d, _)| d).collect();
        let unmonitored = unmonitored
            .into_iter()
            .map(|(d, _)| d)
            .filter(|d| !wanted.contains(d))
            .collect();
        Self {
            wanted,
            unmonitored,
        }
    }

    /// Every listed date, monitored or not: a release on any other is retired.
    fn dates(&self) -> HashSet<String> {
        self.wanted
            .iter()
            .chain(&self.unmonitored)
            .map(|d| (*d).to_owned())
            .collect()
    }
}

/// The note for an upload whose own date is an unmonitored episode.
pub(super) fn unmonitored_note(date: &str) -> String {
    format!("{date} is unmonitored in Sonarr, monitor it there to release this upload")
}

/// An upload of the show with its own air date: the one its title names, or else its broadcast
/// date.
pub(super) struct Dated<'a> {
    upload: Upload<'a>,
    date: Date,
    /// The date is the upload's broadcast date, not one its title names, so it may be a day off
    /// the date TVDB lists.
    from_upload: bool,
}

/// What [`date_upload`] made of an upload.
pub(super) enum Dating<'a> {
    Dated(Dated<'a>),
    /// No date to trust, with the reason for its note.
    Unsure(String),
    /// Not within a day of a monitored date, nor on an unmonitored one.
    Unlisted,
}

/// The zone the mapping's uploads are dated in. `None`, with the mapping skipped in `report`, for
/// a time zone the database does not know (the admin API checks it, so only a hand edit does).
pub(super) fn zone(mapping: &Mapping, report: &mut Report) -> Option<Zone> {
    match Zone::new(mapping.timezone.as_deref(), mapping.utc_offset_mins) {
        Ok(zone) => Some(zone),
        Err(e) => {
            tracing::warn!(mapping = mapping.id, error = %e, "mapping skipped");
            report.skipped.push((mapping.id, e));
            None
        }
    }
}

/// Dates an upload of the show: the date its title names, or else its broadcast date in the
/// channel's time zone. One within a day of a monitored date, or on an unmonitored one, gets its
/// duration looked up for [`place`] and [`pick`].
pub(super) async fn date_upload<'a>(
    cand: &'a Candidate,
    zone: &Zone,
    listed: &Listed<'_>,
    metadata: &impl Metadata,
) -> Result<Dating<'a>, StoreError> {
    // A guessed upload time says too little about the day: only the title counts then.
    let uploaded = if cand.published_approx {
        None
    } else {
        broadcast_date(cand.published_at, zone)
    };
    let (date, from_upload) = match (title_date(&cand.title), uploaded) {
        (TitleDate::Several(dates), _) => {
            let dates: Vec<String> = dates.into_iter().map(iso).collect();
            return Ok(Dating::Unsure(format!(
                "the title names several dates ({}), pin it to its air date",
                dates.join(", ")
            )));
        }
        (TitleDate::One(named), Some(up))
            if !(-MAX_DAYS_EARLY..=MAX_DAYS_LATE).contains(&(up - named).whole_days()) =>
        {
            return Ok(Dating::Unsure(format!(
                "the title says {} but it went up on {}, pin it to its air date",
                iso(named),
                iso(up)
            )));
        }
        (TitleDate::One(named), _) => (named, false),
        (TitleDate::None, Some(up)) => (up, true),
        (TitleDate::None, None) => {
            return Ok(Dating::Unsure(
                "no date in the title and no exact upload time, pin it to its air date".to_owned(),
            ));
        }
    };
    let near = [Some(date), date.previous_day(), date.next_day()];
    if !listed.unmonitored.contains(iso(date).as_str())
        && !near
            .into_iter()
            .flatten()
            .any(|d| listed.wanted.contains(iso(d).as_str()))
    {
        return Ok(Dating::Unlisted);
    }
    let duration = match cand.duration_secs {
        Some(d) => Some(d),
        None => metadata.known(&cand.video_id).await?.duration_secs,
    };
    Ok(Dating::Dated(Dated {
        upload: Upload { cand, duration },
        date,
        from_upload,
    }))
}

/// At or above the floor, or not known yet.
fn is_long(u: &Upload<'_>, floor: i64) -> bool {
    u.duration.is_none_or(|d| d >= floor)
}

/// What [`place`] made of the dated uploads.
pub(super) struct Placed<'a> {
    /// The uploads on each monitored air date.
    pub by_date: BTreeMap<String, Vec<Upload<'a>>>,
    /// Uploads kept on their own date, an unmonitored episode: never released.
    pub unmonitored: Vec<(String, Upload<'a>)>,
    /// Uploads that fit no listed date.
    pub unplaced: u64,
}

/// Puts each upload on an air date Sonarr monitors, and counts the ones that fit none.
///
/// An upload goes on its own date when Sonarr monitors it. One whose own date is an unmonitored
/// episode stays there, unreleased: it is that episode, not a neighbour. One dated only by its
/// upload time, at or above the floor, may instead go a day either side (the day before first):
/// podcasts go up the day after recording, or a few hours off TVDB's timezone. It only lands on a
/// monitored date that no upload at or above the floor has as its own and no pin holds, so an
/// exact date always wins. A date a title names is never moved.
pub(super) fn place<'a>(
    dated: Vec<Dated<'a>>,
    listed: &Listed<'_>,
    pinned: &BTreeMap<String, String>,
    floor: i64,
) -> Placed<'a> {
    let mut by_date: BTreeMap<String, Vec<Upload<'a>>> = BTreeMap::new();
    let mut unmonitored: Vec<(String, Upload<'a>)> = Vec::new();
    let mut drifting: Vec<Dated<'a>> = Vec::new();
    let mut unplaced = 0;
    for d in dated {
        let own = iso(d.date);
        if listed.wanted.contains(own.as_str()) {
            by_date.entry(own).or_default().push(d.upload);
        } else if listed.unmonitored.contains(own.as_str()) {
            unmonitored.push((own, d.upload));
        } else if d.from_upload && is_long(&d.upload, floor) {
            drifting.push(d);
        } else {
            unplaced += 1;
        }
    }
    let claimed: HashSet<String> = by_date
        .iter()
        .filter(|(_, uploads)| uploads.iter().any(|u| is_long(u, floor)))
        .map(|(date, _)| date.clone())
        .chain(pinned.keys().cloned())
        .collect();
    for d in drifting {
        let near = [d.date.previous_day(), d.date.next_day()]
            .into_iter()
            .flatten()
            .map(iso)
            .find(|n| listed.wanted.contains(n.as_str()) && !claimed.contains(n));
        match near {
            Some(date) => by_date.entry(date).or_default().push(d.upload),
            None => unplaced += 1,
        }
    }
    Placed {
        by_date,
        unmonitored,
        unplaced,
    }
}

/// What one release is picked for, as the notes name it.
#[derive(Debug, Clone, Copy)]
pub(super) enum Slot<'s> {
    /// An air date, `YYYY-MM-DD`.
    Date(&'s str),
    /// A podcast's running number, `#2569`.
    Number(u64),
}

impl std::fmt::Display for Slot<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Date(date) => f.write_str(date),
            Self::Number(n) => write!(f, "#{n}"),
        }
    }
}

impl Slot<'_> {
    /// The note for a full upload with another title than the one picked.
    fn second(self, first_id: &str) -> String {
        match self {
            Self::Date(date) => format!(
                "a second full episode on {date} after {first_id}, Sonarr lists one per date: \
                 set a regex that keeps one edition or pin the right one"
            ),
            Self::Number(n) => format!(
                "a second full upload numbered #{n} after {first_id} with another title, \
                 pin the right one"
            ),
        }
    }
}

/// The release for one slot from its uploads, as [`choose`] picks it, and notes for the rest.
/// `None` while every upload is under the floor, or the pick's duration or height is unknown.
pub(super) async fn pick(
    slot: Slot<'_>,
    uploads: Vec<Upload<'_>>,
    floor: i64,
    mapping: &Mapping,
    names: &[String],
    metadata: &impl Metadata,
) -> Result<(Option<Choice>, Vec<NewUnmatched>), StoreError> {
    let (long, short): (Vec<_>, Vec<_>) = uploads.into_iter().partition(|u| is_long(u, floor));
    if long.is_empty() {
        let notes = short
            .into_iter()
            .map(|u| {
                let mins = u.duration.unwrap_or_default() / 60;
                unsure(
                    u.cand,
                    format!("{slot}: {mins} min is under the {} min floor", floor / 60),
                )
            })
            .collect();
        return Ok((None, notes));
    }
    let Some((best, mut notes)) = choose(slot, long, mapping, names) else {
        return Ok((None, Vec::new()));
    };
    if best.duration.is_none() {
        notes.push(dated(
            best.cand,
            UnmatchedState::Pending,
            format!("{slot}: duration unknown until the resolver looks the video up"),
        ));
        return Ok((None, notes));
    }
    let known = metadata.known(&best.cand.video_id).await?;
    let Some(height) = super::height_or_assumed(metadata, &known, &best.cand.channel_id) else {
        notes.push(dated(
            best.cand,
            UnmatchedState::Pending,
            format!("{slot}: video height unknown until the resolver looks the video up"),
        ));
        return Ok((None, notes));
    };
    let choice = Choice {
        ids: vec![best.cand.video_id.clone()],
        title: best.cand.title.clone(),
        published_at: best.cand.published_at,
        height,
        size: known.size,
        language: None,
    };
    Ok((Some(choice), notes))
}

/// Picks the upload for one slot from its uploads at or above the floor, and notes the rest.
/// Sonarr's daily format has one episode per date, and a running number names one episode. The
/// rule, in order:
///
/// 1. An upload's own date beats a neighbouring one: [`place`] only moves an upload onto a date
///    no upload at or above the floor has as its own.
/// 2. Full episodes beat clips: an upload under half the longest known duration of the slot is a
///    clip of it.
/// 3. The earliest full upload is the slot's episode, so a later one never takes the slot from
///    it. Copies of it (the same title once dates and tags are gone) are ranked by [`rank`]: the
///    mapping's first channel, a clean upload over burned-in subtitles, then the earliest.
///
/// Another full upload with a different title (a second episode, or an evening edition) is noted
/// as unsure, for a regex or a pin to settle.
fn choose<'a>(
    slot: Slot<'_>,
    long: Vec<Upload<'a>>,
    mapping: &Mapping,
    names: &[String],
) -> Option<(Upload<'a>, Vec<NewUnmatched>)> {
    let longest = long.iter().filter_map(|u| u.duration).max().unwrap_or(0);
    let (full, clips): (Vec<_>, Vec<_>) = long
        .into_iter()
        .partition(|u| u.duration.is_none_or(|d| d * 2 >= longest));
    let first = full
        .iter()
        .min_by_key(|u| (u.cand.published_at, u.cand.video_id.as_str()))?;
    let first_id = first.cand.video_id.clone();
    let edition = core_title(&first.cand.title, names);
    let (mut copies, others): (Vec<_>, Vec<_>) = full
        .into_iter()
        .partition(|u| core_title(&u.cand.title, names) == edition);
    copies.sort_by_key(|u| rank(mapping, u.cand));
    let mut copies = copies.into_iter();
    let best = copies.next()?;
    let mut notes: Vec<NewUnmatched> = copies
        .map(|u| {
            dated(
                u.cand,
                UnmatchedState::Duplicate,
                format!("another upload of {slot}, kept {}", best.cand.video_id),
            )
        })
        .collect();
    notes.extend(
        others
            .into_iter()
            .map(|u| unsure(u.cand, slot.second(&first_id))),
    );
    notes.extend(clips.into_iter().map(|u| {
        let mins = u.duration.unwrap_or_default() / 60;
        dated(
            u.cand,
            UnmatchedState::Duplicate,
            format!(
                "{slot}: a {mins} min clip next to the {} min episode {}",
                longest / 60,
                best.cand.video_id
            ),
        )
    }));
    Some((best, notes))
}

fn dated(cand: &Candidate, state: UnmatchedState, reason: String) -> NewUnmatched {
    NewUnmatched {
        video_id: cand.video_id.clone(),
        state,
        season: None,
        episode: None,
        part: None,
        reason,
    }
}

pub(super) fn unsure(cand: &Candidate, reason: String) -> NewUnmatched {
    dated(cand, UnmatchedState::Unsure, reason)
}

#[cfg(test)]
mod tests {
    use super::*;

    use time::Month;

    const FLOOR: i64 = DEFAULT_MIN_DURATION_SECS;

    fn day(d: u8) -> Date {
        Date::from_calendar_date(2026, Month::October, d).unwrap()
    }

    /// An upload at `hour` UTC on October `d`, `mins` long.
    fn cand(id: &str, title: &str, d: u8, hour: i64, mins: i64) -> Candidate {
        let midnight = day(d).midnight().assume_utc().unix_timestamp();
        Candidate {
            video_id: id.to_owned(),
            channel_id: "UCpod".to_owned(),
            title: title.to_owned(),
            published_at: midnight + hour * 3600,
            published_approx: false,
            duration_secs: Some(mins * 60),
        }
    }

    fn mapping() -> Mapping {
        Mapping {
            id: 1,
            kind: crate::store::MappingKind::Daily,
            tvdb_id: Some(460_001),
            tmdb_id: None,
            title: "The Pod".to_owned(),
            language: "English".to_owned(),
            channel_ids: vec!["UCpod".to_owned()],
            episode_regex: None,
            excludes: vec![],
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

    /// Air date to video ids.
    type ByDate = BTreeMap<String, Vec<String>>;

    /// Air date to video ids, from uploads dated `(date, from_upload)`, with every listed date
    /// monitored.
    fn placed(
        uploads: &[(Candidate, u8, bool)],
        listed: &[&str],
        pinned: &[&str],
    ) -> (ByDate, u64) {
        let (ids, unmonitored, unplaced) = placed_with(uploads, listed, &[], pinned);
        assert!(unmonitored.is_empty(), "{unmonitored:?}");
        (ids, unplaced)
    }

    /// As [`placed`], with `unmonitored` dates listed too, and the uploads kept on them.
    fn placed_with(
        uploads: &[(Candidate, u8, bool)],
        listed: &[&str],
        unmonitored: &[&str],
        pinned: &[&str],
    ) -> (ByDate, Vec<(String, String)>, u64) {
        let listed = Listed::new(
            listed
                .iter()
                .map(|d| (*d, true))
                .chain(unmonitored.iter().map(|d| (*d, false))),
        );
        let pins: BTreeMap<String, String> = pinned
            .iter()
            .map(|date| ((*date).to_owned(), "pinned".to_owned()))
            .collect();
        let dated = uploads
            .iter()
            .map(|(cand, d, from_upload)| Dated {
                upload: Upload {
                    cand,
                    duration: cand.duration_secs,
                },
                date: day(*d),
                from_upload: *from_upload,
            })
            .collect();
        let placed = place(dated, &listed, &pins, FLOOR);
        let ids = placed
            .by_date
            .into_iter()
            .map(|(date, ups)| (date, ups.iter().map(|u| u.cand.video_id.clone()).collect()))
            .collect();
        let kept = placed
            .unmonitored
            .into_iter()
            .map(|(date, u)| (date, u.cand.video_id.clone()))
            .collect();
        (ids, kept, placed.unplaced)
    }

    fn on(pairs: &[(&str, &[&str])]) -> ByDate {
        pairs
            .iter()
            .map(|(date, ids)| {
                (
                    (*date).to_owned(),
                    ids.iter().map(|s| (*s).to_owned()).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn an_upload_on_a_listed_date_stays_there() {
        let up = cand("a", "The Pod | Guest A", 14, 12, 60);
        assert_eq!(
            placed(&[(up, 14, true)], &["2026-10-13", "2026-10-14"], &[]),
            (on(&[("2026-10-14", &["a"])]), 0)
        );
    }

    #[test]
    fn an_upload_a_day_off_lands_on_the_listed_date() {
        // Up a day after recording: TVDB lists the day before.
        let late = cand("late", "The Pod | Guest A", 15, 12, 60);
        assert_eq!(
            placed(&[(late, 15, true)], &["2026-10-14"], &[]),
            (on(&[("2026-10-14", &["late"])]), 0)
        );
        // Up the evening before TVDB's date.
        let early = cand("early", "The Pod | Guest B", 13, 22, 60);
        assert_eq!(
            placed(&[(early, 13, true)], &["2026-10-14"], &[]),
            (on(&[("2026-10-14", &["early"])]), 0)
        );
        // Both neighbours listed and free: the day before.
        let between = cand("between", "The Pod | Guest C", 14, 12, 60);
        assert_eq!(
            placed(&[(between, 14, true)], &["2026-10-13", "2026-10-15"], &[]),
            (on(&[("2026-10-13", &["between"])]), 0)
        );
    }

    #[test]
    fn an_exact_date_beats_a_neighbour() {
        // The 14th has its own upload: the drifting one from the 15th takes the 16th instead.
        let own = cand("own", "The Pod | Guest A", 14, 12, 60);
        let drift = cand("drift", "The Pod | Guest B", 15, 12, 60);
        assert_eq!(
            placed(
                &[(own, 14, true), (drift, 15, true)],
                &["2026-10-14", "2026-10-16"],
                &[]
            ),
            (
                on(&[("2026-10-14", &["own"]), ("2026-10-16", &["drift"])]),
                0
            )
        );
        // With no free neighbour it fits nowhere.
        let own = cand("own", "The Pod | Guest A", 14, 12, 60);
        let drift = cand("drift", "The Pod | Guest B", 15, 12, 60);
        assert_eq!(
            placed(&[(own, 14, true), (drift, 15, true)], &["2026-10-14"], &[]),
            (on(&[("2026-10-14", &["own"])]), 1)
        );
        // A clip on its own date does not claim it from a full episode a day off.
        let clip = cand("clip", "The Pod | Best bits", 14, 12, 5);
        let drift = cand("drift", "The Pod | Guest B", 15, 12, 60);
        assert_eq!(
            placed(&[(clip, 14, true), (drift, 15, true)], &["2026-10-14"], &[]),
            (on(&[("2026-10-14", &["clip", "drift"])]), 0)
        );
        // A pinned date is taken too.
        let drift = cand("drift", "The Pod | Guest B", 15, 12, 60);
        assert_eq!(
            placed(&[(drift, 15, true)], &["2026-10-14"], &["2026-10-14"]),
            (BTreeMap::new(), 1)
        );
    }

    #[test]
    fn an_unlisted_date_is_not_matched() {
        // Two days off a listed date.
        let far = cand("far", "The Pod | Guest A", 16, 12, 60);
        // A date the title names is never moved.
        let named = cand("named", "The Pod | 15 Oct 2026", 15, 12, 60);
        // A clip does not drift.
        let clip = cand("clip", "The Pod | Best bits", 15, 12, 5);
        assert_eq!(
            placed(
                &[(far, 16, true), (named, 15, false), (clip, 15, true)],
                &["2026-10-14"],
                &[]
            ),
            (BTreeMap::new(), 3)
        );
    }

    #[test]
    fn an_unmonitored_date_keeps_its_upload() {
        // The 14th is unmonitored and the 15th monitored with no upload of its own yet: the 14th's
        // upload is that episode, so it stays there unreleased instead of taking the 15th.
        let own = cand("own", "The Pod | Guest A", 14, 12, 60);
        assert_eq!(
            placed_with(&[(own, 14, true)], &["2026-10-15"], &["2026-10-14"], &[]),
            (
                BTreeMap::new(),
                vec![("2026-10-14".to_owned(), "own".to_owned())],
                0
            )
        );
        // An upload a day off still drifts onto a monitored date, never onto an unmonitored one.
        let late = cand("late", "The Pod | Guest B", 16, 12, 60);
        assert_eq!(
            placed_with(&[(late, 16, true)], &["2026-10-17"], &["2026-10-15"], &[]),
            (on(&[("2026-10-17", &["late"])]), vec![], 0)
        );
        let late = cand("late", "The Pod | Guest B", 16, 12, 60);
        assert_eq!(
            placed_with(&[(late, 16, true)], &[], &["2026-10-15"], &[]),
            (BTreeMap::new(), vec![], 1)
        );
        // A date with a monitored and an unmonitored episode is monitored.
        let both = cand("both", "The Pod | Guest C", 14, 12, 60);
        assert_eq!(
            placed_with(&[(both, 14, true)], &["2026-10-14"], &["2026-10-14"], &[]),
            (on(&[("2026-10-14", &["both"])]), vec![], 0)
        );
    }

    fn chose(uploads: &[Candidate]) -> (String, Vec<NewUnmatched>) {
        let long = uploads
            .iter()
            .map(|cand| Upload {
                cand,
                duration: cand.duration_secs,
            })
            .collect();
        let (best, notes) = choose(
            Slot::Date("2026-10-14"),
            long,
            &mapping(),
            &show_names("The Pod", &[]),
        )
        .expect("a pick");
        (best.cand.video_id.clone(), notes)
    }

    #[test]
    fn a_full_episode_beats_a_clip() {
        // The clip went up first and is over the floor, but under half the episode.
        let (best, notes) = chose(&[
            cand("clip", "The Pod | Best moments with Guest A", 14, 9, 20),
            cand("full", "The Pod | Guest A full episode", 14, 12, 95),
        ]);
        assert_eq!(best, "full");
        assert_eq!(notes.len(), 1);
        assert_eq!(
            (notes[0].video_id.as_str(), notes[0].state),
            ("clip", UnmatchedState::Duplicate)
        );
        assert!(
            notes[0].reason.contains("20 min clip"),
            "{}",
            notes[0].reason
        );
    }

    #[test]
    fn a_second_full_episode_does_not_take_the_date() {
        // The second is longer, but the first came first and keeps the date.
        let (best, notes) = chose(&[
            cand("second", "The Pod | Guest B", 14, 18, 70),
            cand("first", "The Pod | Guest A", 14, 10, 60),
        ]);
        assert_eq!(best, "first");
        assert_eq!(notes.len(), 1);
        assert_eq!(
            (notes[0].video_id.as_str(), notes[0].state),
            ("second", UnmatchedState::Unsure)
        );
        assert!(
            notes[0]
                .reason
                .contains("second full episode on 2026-10-14 after first"),
            "{}",
            notes[0].reason
        );
    }

    #[test]
    fn copies_of_the_episode_are_ranked() {
        // A re-upload with burned-in subtitles went up first: the clean copy still wins.
        let (best, notes) = chose(&[
            cand("sub", "The Pod | Guest A | Eng Sub", 14, 10, 60),
            cand("clean", "The Pod | Guest A", 14, 11, 60),
        ]);
        assert_eq!(best, "clean");
        assert_eq!(notes[0].state, UnmatchedState::Duplicate);
        assert!(notes[0].reason.contains("kept clean"));
    }

    fn core_title_plain(title: &str) -> String {
        core_title(title, &[])
    }

    #[test]
    fn an_alias_is_the_same_edition() {
        let names = show_names("Khabarnaak", &["Khabarnak".to_owned()]);
        assert_eq!(
            core_title("Khabarnak | 14 Oct 2026 | GEO News", &names),
            core_title("Khabarnaak | 14th October 2026 | GEO News", &names)
        );
    }

    #[test]
    fn editions_differ_but_copies_do_not() {
        let a = core_title_plain("Har Lamha Purjosh | 14th October 2026 | ARY News");
        assert_eq!(
            a,
            core_title_plain("Har Lamha Purjosh | 14 Oct 2026 | ARY News [Eng Sub]")
        );
        assert_eq!(
            a,
            core_title_plain("Har Lamha Purjosh | 14-10-2026 | ARY News | Presented by Happilac")
        );
        assert_ne!(
            core_title_plain("Good Morning Pakistan | Morning Edition | 14 Oct 2026"),
            core_title_plain("Good Morning Pakistan | Evening Edition | 14 Oct 2026")
        );
    }
}
