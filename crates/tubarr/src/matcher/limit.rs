//! A mapping's backfill limit: of the episodes uploaded before the mapping was added, only the
//! newest `backfill_limit` are released. Everything uploaded after it always is.
//!
//! The limit applies to what a matcher would release or look up on this run. Back episodes are
//! ranked newest first by where they sit in the show: the target's TheTVDB air date, then its
//! season and episode, then the upload time. Only a note on a video with no target yet goes by its
//! upload time alone, slotted in among the others by date. A channel list often dates a dozen
//! uploads the same rough "1 month ago", so an upload time never reorders episodes whose place in
//! the show is known. The ones kept can change when a newer back episode turns up later (it takes
//! the place of the oldest kept one). An episode
//! counts once, whether one upload, a split upload's parts, or a video still waiting for its
//! lookup. Pinned episodes are always released and do not count. Left out uploads get a `capped`
//! note instead of a release or a pending note, so the matcher never looks them up, and a release
//! one of them already had is removed.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::store::{
    Db, EpisodeFilter, Mapping, NewUnmatched, PinTarget, StoreError, UnmatchedState,
};

use super::{
    Candidate, Report,
    release_set::{Choice, Target},
};

/// What the limit did to one mapping on one run.
pub(super) struct Limited<K> {
    /// Targets the limit left out. Their releases go.
    pub capped: BTreeSet<K>,
    /// Every kept back episode has its release: a search for more back episodes has nothing left
    /// to find a place for.
    pub full: bool,
}

/// One episode as the limit counts it.
enum Unit<K> {
    /// A release the matcher chose.
    Release(K),
    /// Notes waiting for a lookup, by season and episode, else by video.
    Waiting(Vec<usize>),
}

/// Where an episode sits in the show, compared field by field: its TheTVDB air date (midnight
/// UTC, Unix seconds), its season and episode, then its upload time. A field nobody knows sorts
/// as the oldest.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Place {
    aired: Option<i64>,
    number: Option<(i64, i64)>,
    uploaded: i64,
}

impl Place {
    fn of<K: Target>(target: &K, air_date: &impl Fn(&K) -> Option<i64>, uploaded: i64) -> Self {
        Place {
            aired: air_date(target),
            number: match target.to_pin() {
                PinTarget::Episode { season, episode } => Some((season, episode)),
                _ => None,
            },
            uploaded,
        }
    }

    /// The date it is slotted in by among uploads with no target.
    fn when(&self) -> i64 {
        self.aired.unwrap_or(self.uploaded)
    }
}

/// Applies `mapping.backfill_limit` to what a matcher decided: `desired` releases and `notes`.
/// `air_date` gives a target's TheTVDB air date as midnight UTC, when known. `None` when the
/// mapping has no limit, and then nothing changes.
pub(super) fn apply<K: Target>(
    mapping: &Mapping,
    candidates: &[Candidate],
    pinned_videos: &HashSet<&str>,
    air_date: impl Fn(&K) -> Option<i64>,
    desired: &mut BTreeMap<K, Choice>,
    notes: &mut Vec<NewUnmatched>,
) -> Option<Limited<K>> {
    let limit = usize::try_from(mapping.backfill_limit?).unwrap_or(0);
    let published: HashMap<&str, i64> = candidates
        .iter()
        .map(|c| (c.video_id.as_str(), c.published_at))
        .collect();
    let before = |at: i64| at < mapping.created_at;

    // Back episodes with a target, and notes on videos with none yet.
    let mut placed: Vec<(Place, Unit<K>)> = Vec::new();
    let mut loose: Vec<(i64, Unit<K>)> = Vec::new();
    for (target, choice) in desired.iter() {
        let pinned = choice
            .ids
            .iter()
            .any(|id| pinned_videos.contains(id.as_str()));
        if !pinned && before(choice.published_at) {
            placed.push((
                Place::of(target, &air_date, choice.published_at),
                Unit::Release(target.clone()),
            ));
        }
    }
    // Waiting notes by episode (or by video when they name none): the newest upload, the notes.
    let mut waiting = BTreeMap::new();
    for (i, note) in notes.iter().enumerate() {
        if note.state != UnmatchedState::Pending || pinned_videos.contains(note.video_id.as_str()) {
            continue;
        }
        let Some(&at) = published.get(note.video_id.as_str()) else {
            continue;
        };
        let key = match (note.season, note.episode) {
            (Some(s), Some(e)) => Ok((s, e)),
            _ => Err(note.video_id.as_str()),
        };
        let (latest, members) = waiting.entry(key).or_insert((at, Vec::new()));
        *latest = (*latest).max(at);
        members.push(i);
    }
    for (key, (at, members)) in waiting {
        if !before(at) {
            continue;
        }
        let target = key
            .ok()
            .and_then(|(season, episode)| K::from_pin(&PinTarget::Episode { season, episode }));
        match target {
            Some(target) => {
                placed.push((Place::of(&target, &air_date, at), Unit::Waiting(members)))
            }
            None => loose.push((at, Unit::Waiting(members))),
        }
    }
    let back = newest_first(placed, loose);

    let reason = reason(limit);
    let mut capped = BTreeSet::new();
    let mut extra = Vec::new();
    let mut kept_released = 0;
    for (rank, unit) in back.into_iter().enumerate() {
        let keep = rank < limit;
        match unit {
            Unit::Release(_) if keep => kept_released += 1,
            Unit::Waiting(_) if keep => {}
            Unit::Release(target) => {
                if let Some(choice) = desired.remove(&target) {
                    let (season, episode) = match target.to_pin() {
                        PinTarget::Episode { season, episode } => (Some(season), Some(episode)),
                        _ => (None, None),
                    };
                    extra.extend(choice.ids.into_iter().map(|video_id| NewUnmatched {
                        video_id,
                        state: UnmatchedState::Capped,
                        season,
                        episode,
                        part: None,
                        reason: reason.clone(),
                    }));
                }
                capped.insert(target);
            }
            Unit::Waiting(members) => {
                for i in members {
                    notes[i].state = UnmatchedState::Capped;
                    notes[i].reason.clone_from(&reason);
                }
            }
        }
    }
    notes.extend(extra);
    Some(Limited {
        capped,
        full: kept_released >= limit,
    })
}

/// Episodes newest first by their place in the show, with the ones that have none slotted in by
/// date. The sorts are stable, so ties keep the matcher's own order run after run.
fn newest_first<K>(
    mut placed: Vec<(Place, Unit<K>)>,
    mut loose: Vec<(i64, Unit<K>)>,
) -> Vec<Unit<K>> {
    placed.sort_by_key(|(place, _)| std::cmp::Reverse(*place));
    loose.sort_by_key(|(at, _)| std::cmp::Reverse(*at));
    let mut placed = placed.into_iter().peekable();
    let mut loose = loose.into_iter().peekable();
    let mut out = Vec::new();
    loop {
        let take_placed = match (placed.peek(), loose.peek()) {
            (Some((place, _)), Some((at, _))) => place.when() >= *at,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return out,
        };
        let next = if take_placed {
            placed.next().map(|(_, unit)| unit)
        } else {
            loose.next().map(|(_, unit)| unit)
        };
        out.extend(next);
    }
}

/// Removes the mapping's releases on the targets the limit left out.
pub(super) async fn drop_capped<K: Target>(
    db: &Db,
    mapping: &Mapping,
    capped: &BTreeSet<K>,
    report: &mut Report,
) -> Result<(), StoreError> {
    if capped.is_empty() {
        return Ok(());
    }
    let existing = db
        .releases_for_mappings(&[mapping.id], &EpisodeFilter::Any, i64::MAX)
        .await?;
    for release in existing {
        if K::from_pin(&release.target).is_some_and(|t| capped.contains(&t)) {
            db.delete_release(release.id).await?;
            report.replaced += 1;
        }
    }
    Ok(())
}

/// The note on a left out upload.
fn reason(limit: usize) -> String {
    let kept = match limit {
        0 => "the backfill limit releases no back episodes".to_owned(),
        1 => "the backfill limit keeps only the newest back episode".to_owned(),
        n => format!("the backfill limit keeps only the newest {n} back episodes"),
    };
    format!(
        "uploaded before the show was added and {kept}, pin it or raise the limit to release it"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400;

    fn place(aired: Option<i64>, episode: i64, uploaded: i64) -> Place {
        Place {
            aired,
            number: Some((2026, episode)),
            uploaded,
        }
    }

    fn order(placed: Vec<(Place, i64)>, loose: Vec<(i64, i64)>) -> Vec<i64> {
        let placed = placed
            .into_iter()
            .map(|(p, id)| (p, Unit::Release(id)))
            .collect();
        let loose = loose
            .into_iter()
            .map(|(at, id)| (at, Unit::Release(id)))
            .collect();
        newest_first(placed, loose)
            .into_iter()
            .map(|u| match u {
                Unit::Release(id) => id,
                Unit::Waiting(_) => unreachable!(),
            })
            .collect()
    }

    #[test]
    fn a_shared_rough_date_goes_by_episode_and_a_newer_guess_does_not_outrank_it() {
        let rough = 30 * DAY;
        let placed = vec![
            (place(None, 1, rough + DAY), 1),
            (place(None, 3, rough), 3),
            (place(None, 2, rough), 2),
        ];
        assert_eq!(order(placed, vec![]), [3, 2, 1]);
    }

    #[test]
    fn the_air_date_comes_before_the_number_and_the_upload_time() {
        let placed = vec![
            (place(Some(2 * DAY), 1, 0), 1),
            (place(Some(DAY), 2, 9 * DAY), 2),
        ];
        assert_eq!(order(placed, vec![]), [1, 2]);
    }

    #[test]
    fn an_upload_with_no_target_slots_in_by_its_date_and_moves_with_it() {
        let placed = vec![(place(Some(DAY), 1, 0), 1), (place(Some(3 * DAY), 3, 0), 3)];
        assert_eq!(order(placed.clone(), vec![(5 * DAY, 9)]), [9, 3, 1]);
        // A feed gives its exact time, two days older than the guess.
        assert_eq!(order(placed, vec![(2 * DAY, 9)]), [3, 9, 1]);
    }
}
