//! The matcher for numbered shows: which discovered video is which episode.
//!
//! For each show mapping it reads the discovered videos on the mapping's channels, keeps the ones
//! whose title names exactly one episode of the show and that pass the duration floor, picks one
//! upload per episode and writes it to `releases`. Everything it looked at but did not release
//! goes to `unmatched` with a reason. When unsure it answers nothing: no release, a note instead.
//!
//! A run is a full recompute per mapping, so running it twice changes nothing, and a pin added
//! later replaces the automatic choice on the next run. Daily shows ([`daily`]), shows numbered
//! by playlist order ([`playlist`]) and podcasts numbered by a running count in TVDB's episode
//! titles ([`title_number`]) have their own modules, and so do films ([`films`]). A show
//! whose playlist is only a source of videos is matched by title here like any other.

mod daily;
pub mod dates;
pub mod films;
mod joined;
mod limit;
mod playlist;
pub mod preset;
mod release_set;
mod run;
pub mod title;
mod title_number;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    future::Future,
};

use time::{Date, Month};

use crate::arr::PushTarget;
use crate::store::{
    Db, Mapping, MappingKind, NewUnmatched, PlaylistMode, StoreError, UnmatchedState,
};
pub use films::{FilmSettings, WantedFilms, match_films};
use release_set::Choice;
pub use run::{FilmHealth, MatcherHealth, RunError, RunSummary, ShowMatcher};
use title::{DEFAULT_MIN_DURATION_SECS, TitleMatch, TitleRules, has_burned_subs};
pub use title_number::DEFAULT_NUMBER_REGEX;

/// A video the channel watcher found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub video_id: String,
    pub channel_id: String,
    pub title: String,
    /// Upload time, Unix seconds.
    pub published_at: i64,
    /// `published_at` is a guess (a channel list's "3 days ago", a lookup's bare date, or when the
    /// watcher first saw the video), not the feed's or a lookup's exact time.
    pub published_approx: bool,
    /// Whole seconds, when the watcher knows it.
    pub duration_secs: Option<i64>,
}

/// The discovered videos. Must return every known video on these channels, not just new ones:
/// notes for videos missing from the answer are dropped.
pub trait Candidates: Sync {
    fn on_channels(
        &self,
        channel_ids: &[String],
    ) -> impl Future<Output = Result<Vec<Candidate>, StoreError>> + Send;

    /// The video ids of a playlist in playlist order, or `None` while it has not been listed.
    /// Shows numbered by playlist wait until it is.
    fn playlist(
        &self,
        playlist_id: &str,
    ) -> impl Future<Output = Result<Option<Vec<String>>, StoreError>> + Send {
        let _ = playlist_id;
        async { Ok(None) }
    }
}

/// An episode Sonarr lists.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WantedEpisode {
    pub season: i64,
    pub episode: i64,
    /// `YYYY-MM-DD` from TVDB, when known.
    pub air_date: Option<String>,
    /// TVDB's episode title, when known: where a podcast keeps its running number.
    pub title: Option<String>,
    /// Sonarr wants it. An unmonitored episode is never released, but the daily and title number
    /// matchers still read its date and number, so its upload never lands on a neighbour.
    pub monitored: bool,
}

/// A series mapping with every episode Sonarr lists of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WantedSeries {
    pub mapping: Mapping,
    pub episodes: Vec<WantedEpisode>,
}

impl WantedSeries {
    /// The episodes Sonarr wants.
    pub fn monitored(&self) -> impl Iterator<Item = &WantedEpisode> + Clone {
        self.episodes.iter().filter(|e| e.monitored)
    }

    /// Every episode Sonarr lists, monitored or not: a release on any other is retired.
    pub fn listed(&self) -> HashSet<(i64, i64)> {
        self.episodes
            .iter()
            .map(|e| (e.season, e.episode))
            .collect()
    }

    /// Each listed episode's TheTVDB air date as midnight UTC, where known: how the backfill
    /// limit tells newer back episodes from older ones.
    fn air_dates(&self) -> HashMap<(i64, i64), i64> {
        self.episodes
            .iter()
            .filter_map(|e| {
                Some((
                    (e.season, e.episode),
                    air_date_secs(e.air_date.as_deref()?)?,
                ))
            })
            .collect()
    }
}

/// The series to match: mappings of kind `show` and `daily`.
pub trait Wanted: Sync {
    fn series(&self) -> impl Future<Output = Result<Vec<WantedSeries>, StoreError>> + Send;
}

/// What the resolver has learned about a video so far.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Known {
    pub title: Option<String>,
    pub duration_secs: Option<i64>,
    /// Upload time, Unix seconds.
    pub published_at: Option<i64>,
    /// Height of the format Tubarr would serve.
    pub height: Option<i64>,
    /// Bytes of the formats Tubarr would serve, when yt-dlp gave them exactly.
    pub size: Option<i64>,
}

/// Metadata from earlier lookups, so the matcher never asks YouTube itself.
pub trait Metadata: Sync {
    fn known(&self, video_id: &str) -> impl Future<Output = Result<Known, StoreError>> + Send;

    /// The height to name a release from this official channel with while the video's real
    /// height is unknown. `None` waits for a lookup. A later lookup's real height replaces it on
    /// the stored release (see [`release_set::reconcile`]).
    fn assumed_height(&self, channel_id: &str) -> Option<i64> {
        let _ = channel_id;
        None
    }
}

/// The real height when a lookup gave it, else the channel's assumed one.
fn height_or_assumed(metadata: &impl Metadata, known: &Known, channel_id: &str) -> Option<i64> {
    known.height.or_else(|| metadata.assumed_height(channel_id))
}

/// Assumed heights from the config: per channel, else the `[matcher]` default. 0 means none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssumedHeights {
    pub default: u32,
    pub channels: HashMap<String, u32>,
}

impl AssumedHeights {
    pub fn get(&self, channel_id: &str) -> Option<i64> {
        let h = self
            .channels
            .get(channel_id)
            .copied()
            .unwrap_or(self.default);
        (h > 0).then_some(i64::from(h))
    }
}

/// Kept metadata with the configured assumed heights on top: what the background matcher uses.
pub struct WithAssumed<'a, M> {
    pub metadata: &'a M,
    pub heights: &'a AssumedHeights,
}

impl<M: Metadata> Metadata for WithAssumed<'_, M> {
    async fn known(&self, video_id: &str) -> Result<Known, StoreError> {
        self.metadata.known(video_id).await
    }

    fn assumed_height(&self, channel_id: &str) -> Option<i64> {
        self.heights.get(channel_id)
    }
}

/// The kept `videos` row, which outlives the cached streams.
impl Metadata for Db {
    async fn known(&self, video_id: &str) -> Result<Known, StoreError> {
        let Some(video) = self.video(video_id).await? else {
            return Ok(Known::default());
        };
        let meta = video.meta;
        Ok(Known {
            duration_secs: meta.duration.and_then(|d| i64::try_from(d).ok()),
            published_at: meta
                .timestamp
                .or_else(|| meta.upload_date.as_deref().and_then(upload_date_secs)),
            title: Some(meta.title),
            height: video.height,
            size: video.size,
        })
    }
}

/// What a run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Releases written.
    pub released: u64,
    /// Releases removed because a pin or a better upload took their episode, the backfill limit
    /// left it out, Sonarr no longer lists it, or its upload moved to another episode.
    pub replaced: u64,
    /// Unmatched notes added or changed.
    pub notes_changed: u64,
    /// Mappings matched, by the background matcher only those whose inputs changed since their
    /// last run.
    pub matched: u64,
    /// Mappings the background matcher left alone: nothing they read changed since their last run.
    pub unchanged: u64,
    /// Matches for episodes Sonarr does not want, left out.
    pub not_wanted: u64,
    /// Mappings that could not be matched, with the reason.
    pub skipped: Vec<(i64, String)>,
    /// Episodes and films that got their first release, to push to the arr (a target whose
    /// release only moved to another video is not here).
    pub first_released: Vec<PushTarget>,
}

/// An upload must not come this long before the episode's TVDB air date.
const AIR_DATE_SLACK_SECS: i64 = 3 * 86_400;

/// Runs the matcher over every wanted show and daily show.
pub async fn match_shows(
    db: &Db,
    candidates: &impl Candidates,
    wanted: &impl Wanted,
    metadata: &impl Metadata,
) -> Result<Report, StoreError> {
    let mut report = Report::default();
    for series in wanted.series().await? {
        let mapping = &series.mapping;
        if mapping.kind != MappingKind::Film {
            report.matched += 1;
        }
        let playlist = mapping.playlist_id.as_deref();
        match (mapping.kind, playlist, mapping.playlist_mode) {
            (MappingKind::Show, Some(playlist), PlaylistMode::Order) => {
                playlist::match_one(db, &series, playlist, candidates, metadata, &mut report)
                    .await?;
            }
            // In source mode the watcher stores the playlist's videos as channel uploads, so the
            // title matcher sees them with everything else and numbers them by title.
            (MappingKind::Show, _, _) => {
                if mapping.title_number {
                    title_number::match_one(db, &series, candidates, metadata, &mut report).await?;
                } else {
                    match_one(db, &series, candidates, metadata, &mut report).await?;
                }
                let wanted: Vec<(i64, i64)> =
                    series.monitored().map(|e| (e.season, e.episode)).collect();
                db.finish_backfill_if_complete(mapping.id, &wanted).await?;
            }
            (MappingKind::Daily, _, _) => {
                daily::match_one(db, &series, candidates, metadata, &mut report).await?;
            }
            (MappingKind::Film, _, _) => {}
        }
    }
    Ok(report)
}

/// A full episode upload that passed the title rules.
struct Full<'a> {
    cand: &'a Candidate,
    season: i64,
    episode: i64,
    duration: Option<i64>,
}

async fn match_one(
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
    let rules = match TitleRules::new(mapping) {
        Ok(rules) => rules
            .with_aliases(&aliases)
            .with_season_sizes(season_sizes(series.monitored())),
        Err(e) => {
            tracing::warn!(mapping = mapping.id, error = %e, "mapping skipped");
            report.skipped.push((mapping.id, e.to_string()));
            return Ok(());
        }
    };
    let floor = mapping
        .min_duration_secs
        .unwrap_or(DEFAULT_MIN_DURATION_SECS);
    let wanted: HashMap<(i64, i64), &WantedEpisode> = series
        .monitored()
        .map(|e| ((e.season, e.episode), e))
        .collect();
    let year_seasons = by_year_seasons(series.monitored());

    let pinned: BTreeMap<(i64, i64), String> = release_set::pins(db, mapping.id).await?;
    // A pin names one video or a split upload's parts joined by commas.
    let pinned_videos: HashSet<&str> = pinned.values().flat_map(|v| v.split(',')).collect();

    let all = candidates.on_channels(&mapping.channel_ids).await?;
    let on_channels: Vec<&Candidate> = all
        .iter()
        .filter(|c| mapping.channel_ids.contains(&c.channel_id))
        .collect();

    let reads: Vec<(&Candidate, TitleMatch)> = on_channels
        .iter()
        .map(|c| (*c, rules.read(&c.title)))
        .collect();
    let serial = serial_parts(&reads);

    let mut notes: Vec<NewUnmatched> = Vec::new();
    let mut full: BTreeMap<(i64, i64), Vec<Full<'_>>> = BTreeMap::new();
    let mut parts: BTreeMap<(i64, i64), Vec<joined::PartUpload<'_>>> = BTreeMap::new();
    for (cand, read) in reads {
        // A pinned video goes where its pin says and nowhere else.
        if pinned_videos.contains(cand.video_id.as_str()) {
            continue;
        }
        let note = |state, season, episode, reason: String| NewUnmatched {
            video_id: cand.video_id.clone(),
            state,
            season,
            episode,
            part: None,
            reason,
        };
        match read {
            TitleMatch::NotThisShow => {}
            TitleMatch::NoNumber => notes.push(note(
                UnmatchedState::Unsure,
                None,
                None,
                "the title names the show but no episode number, pin it to its episode".to_owned(),
            )),
            TitleMatch::Several(eps) => notes.push(note(
                UnmatchedState::Unsure,
                None,
                None,
                format!(
                    "the title names episodes {}, pin each one if it is a joint upload",
                    join(&eps)
                ),
            )),
            TitleMatch::Episode {
                season,
                episode,
                part,
            } => {
                let Some(want) = wanted.get(&(season, episode)) else {
                    if let Some((first, last)) = year_seasons
                        && !(first..=last).contains(&season)
                    {
                        notes.push(note(
                            UnmatchedState::Unsure,
                            Some(season),
                            Some(episode),
                            format!(
                                "the title says episode {episode} but Sonarr numbers this show by \
                                 year ({first} to {last}), the upload may count reruns, pin it to \
                                 its episode"
                            ),
                        ));
                    } else {
                        report.not_wanted += 1;
                    }
                    continue;
                };
                if let Some(air) = want.air_date.as_deref().and_then(air_date_secs)
                    && cand.published_at < air - AIR_DATE_SLACK_SECS
                {
                    notes.push(note(
                        UnmatchedState::Unsure,
                        Some(season),
                        Some(episode),
                        format!(
                            "uploaded before S{season:02}E{episode:02} aired ({}), \
                             the number or season may be off",
                            want.air_date.as_deref().unwrap_or_default()
                        ),
                    ));
                    continue;
                }
                if let Some(part) = part
                    && !serial.contains(&(season, episode))
                {
                    parts
                        .entry((season, episode))
                        .or_default()
                        .push(joined::PartUpload { cand, part });
                    continue;
                }
                let duration = match cand.duration_secs {
                    Some(d) => Some(d),
                    None => metadata.known(&cand.video_id).await?.duration_secs,
                };
                full.entry((season, episode)).or_default().push(Full {
                    cand,
                    season,
                    episode,
                    duration,
                });
            }
        }
    }

    let mut desired: BTreeMap<(i64, i64), Choice> = BTreeMap::new();
    let mut decided: HashSet<(i64, i64)> = pinned.keys().copied().collect();
    for (target, mut uploads) in full {
        if pinned.contains_key(&target) {
            // The pin wins: the uploads the matcher would have picked from are noted as such.
            for f in &uploads {
                notes.push(episode_note(
                    f,
                    UnmatchedState::Duplicate,
                    format!(
                        "S{:02}E{:02} is pinned to another video",
                        f.season, f.episode
                    ),
                ));
            }
            continue;
        }
        let (long, short): (Vec<_>, Vec<_>) = uploads
            .drain(..)
            .partition(|f| f.duration.is_none_or(|d| d >= floor));
        if long.is_empty() {
            for f in short {
                let mins = f.duration.unwrap_or_default() / 60;
                notes.push(episode_note(
                    &f,
                    UnmatchedState::Unsure,
                    format!("{mins} min is under the {} min floor", floor / 60),
                ));
            }
            continue;
        }
        decided.insert(target);
        // Short uploads next to a full one are extras with no tell-tale word. Not worth a note.
        let mut long = long;
        long.sort_by_key(|f| rank(mapping, f.cand));
        let mut uploads = long.into_iter();
        let Some(best) = uploads.next() else {
            continue;
        };
        for other in uploads {
            notes.push(episode_note(
                &other,
                UnmatchedState::Duplicate,
                format!(
                    "another upload of S{:02}E{:02}, kept {}",
                    other.season, other.episode, best.cand.video_id
                ),
            ));
        }
        if best.duration.is_none() {
            notes.push(episode_note(
                &best,
                UnmatchedState::Pending,
                "duration unknown until the resolver looks the video up".to_owned(),
            ));
            continue;
        }
        let known = metadata.known(&best.cand.video_id).await?;
        let Some(height) = height_or_assumed(metadata, &known, &best.cand.channel_id) else {
            notes.push(episode_note(
                &best,
                UnmatchedState::Pending,
                "video height unknown until the resolver looks the video up".to_owned(),
            ));
            continue;
        };
        desired.insert(
            target,
            Choice {
                ids: vec![best.cand.video_id.clone()],
                title: best.cand.title.clone(),
                published_at: best.cand.published_at,
                height,
                size: known.size,
                language: None,
            },
        );
    }

    // Split uploads, unless a full upload or a pin already settled the episode.
    for ((season, episode), uploads) in parts {
        if decided.contains(&(season, episode)) {
            continue;
        }
        let what = format!("S{season:02}E{episode:02}");
        let part_notes =
            match joined::join(uploads, &what, floor, &mapping.channel_ids, metadata).await? {
                joined::Joined::Release(choice, part_notes) => {
                    desired.insert((season, episode), choice);
                    part_notes
                }
                joined::Joined::Wait(part_notes) => part_notes,
            };
        notes.extend(part_notes.into_iter().map(|n| NewUnmatched {
            video_id: n.video_id,
            state: n.state,
            season: Some(season),
            episode: Some(episode),
            part: Some(n.part),
            reason: n.reason,
        }));
    }

    for (&(season, episode), pin) in &pinned {
        match release_set::pinned_choice(pin, &on_channels, metadata).await? {
            Ok(choice) => {
                desired.insert((season, episode), choice);
            }
            Err(missing) => notes.extend(missing.into_iter().map(|video_id| NewUnmatched {
                video_id,
                state: UnmatchedState::Pending,
                season: Some(season),
                episode: Some(episode),
                part: None,
                reason: "pinned, waiting for the resolver to look the video up".to_owned(),
            })),
        }
    }

    let aired = series.air_dates();
    if let Some(limited) = limit::apply(
        mapping,
        &all,
        &pinned_videos,
        |t| aired.get(t).copied(),
        &mut desired,
        &mut notes,
    ) {
        limit::drop_capped(db, mapping, &limited.capped, report).await?;
        // The kept back episodes are all out, and a search only finds older ones.
        if limited.full {
            db.finish_backfill(mapping.id).await?;
        }
    }
    release_set::reconcile(
        db,
        mapping,
        &pinned,
        &desired,
        Some(&series.listed()),
        report,
    )
    .await?;
    dedup_notes(&mut notes);
    report.notes_changed += db.replace_unmatched(mapping.id, &notes).await?;
    Ok(())
}

/// Episodes whose "Part N" only counts the episodes of a story arc, not pieces of one episode:
/// "EP6 Village (Part 1)", "EP7 Village (Part 2)", "EP8 (Part 3)". An episode counts when all its
/// part uploads say the same part and the episode before says the part before, or the episode
/// after says the part after. Parts that share one episode number stay a split upload.
fn serial_parts(reads: &[(&Candidate, TitleMatch)]) -> HashSet<(i64, i64)> {
    let mut seen: BTreeMap<(i64, i64), HashSet<i64>> = BTreeMap::new();
    for (_, read) in reads {
        if let TitleMatch::Episode {
            season,
            episode,
            part: Some(part),
        } = read
        {
            seen.entry((*season, *episode)).or_default().insert(*part);
        }
    }
    let single = |season: i64, episode: i64| -> Option<i64> {
        let parts = seen.get(&(season, episode))?;
        (parts.len() == 1).then(|| parts.iter().copied().next())?
    };
    seen.keys()
        .copied()
        .filter(|&(season, episode)| {
            let Some(part) = single(season, episode) else {
                return false;
            };
            single(season, episode - 1) == Some(part - 1)
                || single(season, episode + 1) == Some(part + 1)
        })
        .collect()
}

/// How many episodes Sonarr has in each season.
fn season_sizes<'a>(episodes: impl Iterator<Item = &'a WantedEpisode>) -> HashMap<i64, i64> {
    let mut seen: BTreeMap<i64, HashSet<i64>> = BTreeMap::new();
    for e in episodes {
        seen.entry(e.season).or_default().insert(e.episode);
    }
    seen.into_iter()
        .map(|(season, eps)| (season, i64::try_from(eps.len()).unwrap_or(i64::MAX)))
        .collect()
}

/// The first and last wanted season when every wanted season is a year, as TVDB numbers some long
/// running shows (CID: S1998E01). A title's own count never names such a season, so its uploads
/// are noted as unsure, never released by that count.
fn by_year_seasons<'a>(
    episodes: impl Iterator<Item = &'a WantedEpisode> + Clone,
) -> Option<(i64, i64)> {
    let first = episodes.clone().map(|e| e.season).min()?;
    let last = episodes.map(|e| e.season).max()?;
    (first >= 1900).then_some((first, last))
}

/// Lower sorts first: the mapping's first channel, a clean upload over burned-in subtitles, the
/// earliest upload, then the video id so the pick never flips between runs.
pub(super) fn rank<'a>(mapping: &Mapping, cand: &'a Candidate) -> (bool, bool, i64, &'a str) {
    (
        mapping.channel_ids.first() != Some(&cand.channel_id),
        has_burned_subs(&cand.title),
        cand.published_at,
        cand.video_id.as_str(),
    )
}

fn episode_note(f: &Full<'_>, state: UnmatchedState, reason: String) -> NewUnmatched {
    NewUnmatched {
        video_id: f.cand.video_id.clone(),
        state,
        season: Some(f.season),
        episode: Some(f.episode),
        part: None,
        reason,
    }
}

/// One note per video. The table keys on (mapping, video), so a repeat would overwrite anyway.
pub(super) fn dedup_notes(notes: &mut Vec<NewUnmatched>) {
    let mut seen = HashSet::new();
    notes.retain(|n| seen.insert(n.video_id.clone()));
}

fn join(eps: &[i64]) -> String {
    match eps {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!(
            "{} and {last}",
            rest.iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `YYYYMMDD` (yt-dlp) as midnight UTC in Unix seconds.
fn upload_date_secs(s: &str) -> Option<i64> {
    let (y, m, d) = (s.get(0..4)?, s.get(4..6)?, s.get(6..8)?);
    date_secs(y, m, d)
}

/// `YYYY-MM-DD` (TVDB) as midnight UTC in Unix seconds.
fn air_date_secs(s: &str) -> Option<i64> {
    let mut it = s.splitn(3, '-');
    date_secs(it.next()?, it.next()?, it.next()?)
}

fn date_secs(y: &str, m: &str, d: &str) -> Option<i64> {
    let month = Month::try_from(m.parse::<u8>().ok()?).ok()?;
    let date = Date::from_calendar_date(y.parse().ok()?, month, d.parse().ok()?).ok()?;
    Some(date.midnight().assume_utc().unix_timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates() {
        assert_eq!(upload_date_secs("19700102"), Some(86_400));
        assert_eq!(air_date_secs("1970-01-02"), Some(86_400));
        assert_eq!(upload_date_secs("2024"), None);
        assert_eq!(air_date_secs("2024-13-01"), None);
    }

    #[test]
    fn a_channel_height_wins_and_zero_means_wait() {
        let heights = AssumedHeights {
            default: 1080,
            channels: HashMap::from([("UCa".to_owned(), 720), ("UCb".to_owned(), 0)]),
        };
        assert_eq!(heights.get("UCa"), Some(720));
        assert_eq!(heights.get("UCb"), None);
        assert_eq!(heights.get("UCc"), Some(1080));
        let off = AssumedHeights::default();
        assert_eq!(off.get("UCc"), None);
    }

    #[test]
    fn episode_lists_read_naturally() {
        assert_eq!(join(&[24, 25]), "24 and 25");
        assert_eq!(join(&[1, 2, 3]), "1, 2 and 3");
    }
}
