//! The matcher for shows numbered by a running count in the title, as video podcasts are. TVDB
//! lists The Joe Rogan Experience with a season per year (S2026E136), and the podcast's own number
//! only in the episode title: "#2569 - Bill Thompson". The YouTube title carries the same number
//! ("Joe Rogan Experience #2569 - Bill Thompson"), so an upload is released as the wanted episode
//! whose TVDB title holds its number.
//!
//! TVDB names new episodes a few days late. An upload whose number is above every number TVDB's
//! titles hold goes by air date instead, with the daily rules ([`daily::place`]) over the dates of
//! the episodes TVDB has not numbered yet. A number at or below the highest one listed is an
//! episode Sonarr does not want. A number TVDB lists on an unmonitored episode is that episode:
//! noted, never released and never moved onto a date.
//!
//! Each number gets one release, picked as for daily shows ([`daily::pick`]): uploads under the
//! floor and clips under half the longest one never take it. Set on the mapping as
//! `title_number`; the mapping's regex, when set, reads the number from its `ep` group.

use std::{
    collections::{BTreeMap, HashSet},
    sync::LazyLock,
};

use regex::Regex;

use crate::store::{Db, Mapping, NewUnmatched, StoreError, UnmatchedState};

use super::{
    Candidate, Candidates, Metadata, Report, WantedEpisode, WantedSeries,
    daily::{self, Dating, Slot, Upload},
    dedup_notes, limit,
    release_set::{self, Choice},
    title::{DEFAULT_MIN_DURATION_SECS, excludes, names_any, show_names},
};

/// "Joe Rogan Experience #2569 - Bill Thompson", "Nate Bargatze | This Past Weekend w/ Theo Von
/// #660".
pub const DEFAULT_NUMBER_REGEX: &str = r"#(?P<ep>\d{2,5})\b";

/// A standalone `#2569` in a TVDB episode title: not inside a longer number or a word.
static LISTED_NUMBER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|[^\w#])#0*(\d{1,6})\b").expect("valid listed number regex"));

/// What a YouTube title says.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Read {
    /// Another show, or an extra. Not worth a note.
    NotThisShow,
    /// The show is named but no number.
    NoNumber,
    Several(Vec<u64>),
    Number(u64),
}

struct NumberRules {
    number: Regex,
    /// The normalized title and aliases, needed with the default regex only: a custom regex is
    /// trusted to name the show itself, as for numbered shows.
    show: Option<Vec<String>>,
    excludes: Regex,
}

impl NumberRules {
    fn new(mapping: &Mapping, aliases: &[String]) -> Result<Self, regex::Error> {
        let (number, show) = match &mapping.episode_regex {
            Some(custom) => (Regex::new(custom)?, None),
            None => (
                Regex::new(DEFAULT_NUMBER_REGEX)?,
                Some(show_names(&mapping.title, aliases)),
            ),
        };
        Ok(Self {
            number,
            show,
            excludes: excludes(mapping)?,
        })
    }

    fn read(&self, title: &str) -> Read {
        if self.excludes.is_match(title) {
            return Read::NotThisShow;
        }
        if let Some(show) = &self.show
            && !names_any(title, show)
        {
            return Read::NotThisShow;
        }
        let mut numbers: Vec<u64> = self
            .number
            .captures_iter(title)
            .filter_map(|c| c.name("ep").or_else(|| c.name("episode")))
            .filter_map(|m| m.as_str().parse().ok())
            .collect();
        numbers.sort_unstable();
        numbers.dedup();
        match numbers.as_slice() {
            [] if self.show.is_some() => Read::NoNumber,
            [] => Read::NotThisShow,
            [one] => Read::Number(*one),
            _ => Read::Several(numbers),
        }
    }
}

/// The episodes whose TVDB titles hold one running number.
#[derive(Debug, Default, PartialEq, Eq)]
struct Numbered {
    /// Monitored: the number's release goes to one of these.
    wanted: Vec<(i64, i64)>,
    /// Unmonitored in Sonarr: the number is known, so its upload never goes by date instead.
    unmonitored: Vec<(i64, i64)>,
}

/// The episodes Sonarr lists by the running numbers in their TVDB titles. A title may hold more
/// than one, and a number may show up on more than one episode.
fn by_number(episodes: &[WantedEpisode]) -> BTreeMap<u64, Numbered> {
    let mut out: BTreeMap<u64, Numbered> = BTreeMap::new();
    for e in episodes {
        let Some(title) = e.title.as_deref() else {
            continue;
        };
        let mut numbers: Vec<u64> = LISTED_NUMBER
            .captures_iter(title)
            .filter_map(|c| c[1].parse().ok())
            .collect();
        numbers.sort_unstable();
        numbers.dedup();
        for n in numbers {
            let numbered = out.entry(n).or_default();
            let into = if e.monitored {
                &mut numbered.wanted
            } else {
                &mut numbered.unmonitored
            };
            into.push((e.season, e.episode));
        }
    }
    out
}

/// The air date of an episode TVDB has not numbered yet.
fn unnumbered_date(e: &WantedEpisode) -> Option<&str> {
    let numbered = e
        .title
        .as_deref()
        .is_some_and(|t| LISTED_NUMBER.is_match(t));
    if numbered {
        None
    } else {
        e.air_date.as_deref()
    }
}

/// The wanted episodes TVDB has not numbered yet, by air date.
fn unnumbered_by_date<'a>(
    episodes: impl Iterator<Item = &'a WantedEpisode>,
) -> BTreeMap<String, Vec<(i64, i64)>> {
    let mut out: BTreeMap<String, Vec<(i64, i64)>> = BTreeMap::new();
    for e in episodes {
        if let Some(date) = unnumbered_date(e) {
            out.entry(date.to_owned())
                .or_default()
                .push((e.season, e.episode));
        }
    }
    out
}

fn sxxeyy((season, episode): (i64, i64)) -> String {
    format!("S{season:02}E{episode:02}")
}

fn on_episode(mut note: NewUnmatched, (season, episode): (i64, i64)) -> NewUnmatched {
    note.season = Some(season);
    note.episode = Some(episode);
    note
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
    let rules = match NumberRules::new(mapping, &aliases) {
        Ok(rules) => rules,
        Err(e) => {
            tracing::warn!(mapping = mapping.id, error = %e, "mapping skipped");
            report
                .skipped
                .push((mapping.id, format!("regex does not compile: {e}")));
            return Ok(());
        }
    };
    let names = show_names(&mapping.title, &aliases);
    let floor = mapping
        .min_duration_secs
        .unwrap_or(DEFAULT_MIN_DURATION_SECS);
    let Some(zone) = daily::zone(mapping, report) else {
        return Ok(());
    };

    let numbered = by_number(&series.episodes);
    let highest = numbered.keys().next_back().copied();
    let unnumbered = unnumbered_by_date(series.monitored());
    let listed = daily::Listed::new(
        series
            .episodes
            .iter()
            .filter_map(|e| Some((unnumbered_date(e)?, e.monitored))),
    );

    let pinned: BTreeMap<(i64, i64), String> = release_set::pins(db, mapping.id).await?;
    let pinned_videos: HashSet<&str> = pinned.values().flat_map(|v| v.split(',')).collect();
    // Pinned episodes by air date, so no upload drifts onto one.
    let pinned_dates: BTreeMap<String, String> = series
        .episodes
        .iter()
        .filter_map(|e| {
            let pin = pinned.get(&(e.season, e.episode))?;
            Some((e.air_date.clone()?, pin.clone()))
        })
        .collect();

    let all = candidates.on_channels(&mapping.channel_ids).await?;
    let on_channels: Vec<&Candidate> = all
        .iter()
        .filter(|c| mapping.channel_ids.contains(&c.channel_id))
        .collect();

    let mut notes: Vec<NewUnmatched> = Vec::new();
    // Each number's uploads, with the one wanted episode it names.
    let mut by_slot: BTreeMap<u64, ((i64, i64), Vec<Upload<'_>>)> = BTreeMap::new();
    let mut dated = Vec::new();
    for cand in &on_channels {
        if pinned_videos.contains(cand.video_id.as_str()) {
            continue;
        }
        let n = match rules.read(&cand.title) {
            Read::NotThisShow => continue,
            Read::NoNumber => {
                notes.push(daily::unsure(
                    cand,
                    "the title names the show but no #number, pin it to its episode".to_owned(),
                ));
                continue;
            }
            Read::Several(numbers) => {
                let numbers: Vec<String> = numbers.iter().map(|n| format!("#{n}")).collect();
                notes.push(daily::unsure(
                    cand,
                    format!(
                        "the title names {}, pin it to its episode",
                        numbers.join(" and ")
                    ),
                ));
                continue;
            }
            Read::Number(n) => n,
        };
        match numbered
            .get(&n)
            .map(|e| (e.wanted.as_slice(), e.unmonitored.as_slice()))
        {
            Some((&[target], _)) => {
                let duration = match cand.duration_secs {
                    Some(d) => Some(d),
                    None => metadata.known(&cand.video_id).await?.duration_secs,
                };
                by_slot
                    .entry(n)
                    .or_insert_with(|| (target, Vec::new()))
                    .1
                    .push(Upload { cand, duration });
            }
            // Known, but Sonarr does not want it: it never goes by date instead.
            Some(([], unmonitored)) => {
                let targets: Vec<String> = unmonitored.iter().copied().map(sxxeyy).collect();
                let note = daily::unsure(
                    cand,
                    format!(
                        "#{n} is {}, unmonitored in Sonarr, monitor it there to release this \
                         upload",
                        targets.join(" and ")
                    ),
                );
                notes.push(match unmonitored {
                    [one] => on_episode(note, *one),
                    _ => note,
                });
            }
            Some((targets, _)) => {
                let targets: Vec<String> = targets.iter().copied().map(sxxeyy).collect();
                notes.push(daily::unsure(
                    cand,
                    format!(
                        "TVDB titles name #{n} on {}, pin it to the right one",
                        targets.join(" and ")
                    ),
                ));
            }
            // An older episode Sonarr does not want, or one TVDB names without its number.
            None if highest.is_some_and(|h| n <= h) => report.not_wanted += 1,
            None => match daily::date_upload(cand, &zone, &listed, metadata).await? {
                Dating::Dated(d) => dated.push(d),
                Dating::Unsure(reason) => notes.push(daily::unsure(
                    cand,
                    format!("no TVDB title names #{n} yet and {reason}"),
                )),
                Dating::Unlisted => report.not_wanted += 1,
            },
        }
    }

    let mut desired: BTreeMap<(i64, i64), Choice> = BTreeMap::new();
    for (n, (target, uploads)) in by_slot {
        let picked = pick(
            Slot::Number(n),
            target,
            uploads,
            &pinned,
            (floor, mapping, &names),
            metadata,
        )
        .await?;
        notes.extend(picked.1);
        if let Some(choice) = picked.0 {
            desired.insert(target, choice);
        }
    }

    // Uploads TVDB has not numbered yet, by air date.
    let daily::Placed {
        by_date,
        unmonitored,
        unplaced,
    } = daily::place(dated, &listed, &pinned_dates, floor);
    report.not_wanted += unplaced;
    notes.extend(
        unmonitored
            .iter()
            .map(|(date, u)| daily::unsure(u.cand, daily::unmonitored_note(date))),
    );
    for (date, uploads) in by_date {
        let [target] = unnumbered[&date][..] else {
            notes.extend(uploads.iter().map(|u| {
                daily::unsure(
                    u.cand,
                    format!(
                        "TVDB lists several episodes on {date} without a number, pin it to its \
                         episode"
                    ),
                )
            }));
            continue;
        };
        let picked = pick(
            Slot::Date(&date),
            target,
            uploads,
            &pinned,
            (floor, mapping, &names),
            metadata,
        )
        .await?;
        notes.extend(picked.1);
        if let Some(choice) = picked.0 {
            desired.insert(target, choice);
        }
    }

    for (&target, pin) in &pinned {
        match release_set::pinned_choice(pin, &on_channels, metadata).await? {
            Ok(choice) => {
                desired.insert(target, choice);
            }
            Err(missing) => notes.extend(missing.into_iter().map(|video_id| NewUnmatched {
                video_id,
                state: UnmatchedState::Pending,
                season: Some(target.0),
                episode: Some(target.1),
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

/// [`daily::pick`] for one episode, unless a pin holds it. Its notes name the episode.
async fn pick(
    slot: Slot<'_>,
    target: (i64, i64),
    uploads: Vec<Upload<'_>>,
    pinned: &BTreeMap<(i64, i64), String>,
    (floor, mapping, names): (i64, &Mapping, &[String]),
    metadata: &impl Metadata,
) -> Result<(Option<Choice>, Vec<NewUnmatched>), StoreError> {
    if pinned.contains_key(&target) {
        let notes = uploads
            .iter()
            .map(|u| NewUnmatched {
                video_id: u.cand.video_id.clone(),
                state: UnmatchedState::Duplicate,
                season: Some(target.0),
                episode: Some(target.1),
                part: None,
                reason: format!("{} is pinned to another video", sxxeyy(target)),
            })
            .collect();
        return Ok((None, notes));
    }
    let (choice, notes) = daily::pick(slot, uploads, floor, mapping, names, metadata).await?;
    let notes = notes.into_iter().map(|n| on_episode(n, target)).collect();
    Ok((choice, notes))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::store::{MappingKind, PlaylistMode};

    fn mapping(title: &str, regex: Option<&str>) -> Mapping {
        Mapping {
            id: 1,
            kind: MappingKind::Show,
            tvdb_id: Some(326_959),
            tmdb_id: None,
            title: title.to_owned(),
            language: "English".to_owned(),
            channel_ids: vec!["UCpod".to_owned()],
            episode_regex: regex.map(str::to_owned),
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
            playlist_mode: PlaylistMode::Source,
            backfill_limit: None,
            title_number: true,
            audio_version: false,
            episode_thumbnails: false,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn rules(title: &str, aliases: &[&str]) -> NumberRules {
        let aliases: Vec<String> = aliases.iter().map(|a| (*a).to_owned()).collect();
        NumberRules::new(&mapping(title, None), &aliases).unwrap()
    }

    fn episode(episode: i64, title: Option<&str>, air_date: &str) -> WantedEpisode {
        WantedEpisode {
            season: 2026,
            episode,
            air_date: Some(air_date.to_owned()),
            title: title.map(str::to_owned),
            monitored: true,
        }
    }

    fn wanted(eps: &[(i64, i64)]) -> Numbered {
        Numbered {
            wanted: eps.to_vec(),
            unmonitored: vec![],
        }
    }

    #[test]
    fn reads_jre_and_theo_titles() {
        let jre = rules("The Joe Rogan Experience", &["Joe Rogan Experience"]);
        assert_eq!(
            jre.read("Joe Rogan Experience #2569 - Bill Thompson"),
            Read::Number(2569)
        );
        // Another show on the channel.
        assert_eq!(
            jre.read("JRE MMA Show #160 with Din Thomas"),
            Read::NotThisShow
        );
        assert_eq!(
            jre.read("Joe Rogan Experience - Best of the Month"),
            Read::NoNumber
        );
        assert_eq!(
            jre.read("Joe Rogan Experience #2569 #shorts"),
            Read::NotThisShow
        );
        assert_eq!(
            jre.read("Joe Rogan Experience #2569 and #2570 back to back"),
            Read::Several(vec![2569, 2570])
        );
        let theo = rules("This Past Weekend w/ Theo Von", &[]);
        assert_eq!(
            theo.read("Nate Bargatze | This Past Weekend w/ Theo Von #660"),
            Read::Number(660)
        );
        // A custom regex reads its `ep` group and needs no show name.
        let custom = NumberRules::new(&mapping("The Pod", Some(r"Ep\. (?P<ep>\d+)")), &[]).unwrap();
        assert_eq!(custom.read("Guest | Ep. 77"), Read::Number(77));
        assert_eq!(custom.read("Guest | live"), Read::NotThisShow);
    }

    #[test]
    fn indexes_tvdb_titles_by_their_number() {
        let episodes = [
            episode(136, Some("#2569 - Bill Thompson"), "2026-10-21"),
            episode(10, Some("Nate Bargatze | #660"), "2026-10-20"),
            // Not standalone: part of a word or a longer tag.
            episode(11, Some("Guest#12 | ##13"), "2026-10-22"),
            episode(137, None, "2026-10-23"),
            episode(138, Some("TBA"), "2026-10-24"),
        ];
        let numbered = by_number(&episodes);
        assert_eq!(numbered.get(&2569), Some(&wanted(&[(2026, 136)])));
        assert_eq!(numbered.get(&660), Some(&wanted(&[(2026, 10)])));
        assert_eq!(numbered.len(), 2);
        let unnumbered = unnumbered_by_date(episodes.iter());
        assert_eq!(
            unnumbered.keys().map(String::as_str).collect::<Vec<_>>(),
            ["2026-10-22", "2026-10-23", "2026-10-24"]
        );
    }

    #[test]
    fn a_number_on_two_episodes_is_kept_for_both() {
        let numbered = by_number(&[
            episode(1, Some("#2569 - Bill Thompson"), "2026-10-21"),
            episode(2, Some("#2569 - Bill Thompson (Part 2)"), "2026-10-22"),
        ]);
        assert_eq!(numbered[&2569], wanted(&[(2026, 1), (2026, 2)]));
    }

    #[test]
    fn an_unmonitored_number_is_known_apart() {
        let mut off = episode(2, Some("#2570 - Guest B"), "2026-10-22");
        off.monitored = false;
        let numbered = by_number(&[episode(1, Some("#2569 - Guest A"), "2026-10-21"), off]);
        assert_eq!(numbered[&2569], wanted(&[(2026, 1)]));
        assert_eq!(
            numbered[&2570],
            Numbered {
                wanted: vec![],
                unmonitored: vec![(2026, 2)],
            }
        );
    }

    #[test]
    fn notes_name_the_episode() {
        let note = on_episode(
            NewUnmatched {
                video_id: "v".to_owned(),
                state: UnmatchedState::Unsure,
                season: None,
                episode: None,
                part: None,
                reason: String::new(),
            },
            (2026, 136),
        );
        assert_eq!((note.season, note.episode), (Some(2026), Some(136)));
        assert_eq!(sxxeyy((2026, 136)), "S2026E136");
    }
}
