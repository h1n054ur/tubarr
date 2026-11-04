//! Episodes and films YouTube split into parts ("Episode 26 - Part 1", "Part 2"), released as one.
//!
//! The parts of one target are released together only when the set is whole: Part 1 to Part N
//! with no gap, N of at least 2, all on one channel, every part's duration and height known and
//! the total over the floor. The caller only asks when no full upload settled the target, so a
//! full upload always wins. A set with a gap waits as `partial`, and the next run releases it once
//! the missing part shows up. YouTube never says how many parts there are, so a set whose last
//! part is not up yet can look whole: the duration floor is what catches that.

use std::collections::BTreeMap;

use super::{Candidate, Metadata, release_set::Choice};
use crate::{
    play::parts::MAX_PARTS,
    store::{StoreError, UnmatchedState},
};

/// One part upload of a target.
pub(super) struct PartUpload<'a> {
    pub cand: &'a Candidate,
    pub part: i64,
}

/// A note for one part when nothing is released.
pub(super) struct PartNote {
    pub video_id: String,
    pub part: i64,
    pub state: UnmatchedState,
    pub reason: String,
}

pub(super) enum Joined {
    Release(Choice, Vec<PartNote>),
    Wait(Vec<PartNote>),
}

/// Joins the parts of one target. `what` names it in notes ("S01E26", "the film"), `channels` is
/// the mapping's channel order, which decides between sets on different channels.
pub(super) async fn join(
    parts: Vec<PartUpload<'_>>,
    what: &str,
    floor: i64,
    channels: &[String],
    metadata: &impl Metadata,
) -> Result<Joined, StoreError> {
    let mut notes = Vec::new();
    let note = |p: &PartUpload<'_>, state, reason: String| PartNote {
        video_id: p.cand.video_id.clone(),
        part: p.part,
        state,
        reason,
    };

    // One set per channel, one upload per part number: the earliest, then the lowest id.
    let mut sets: BTreeMap<(usize, &str), BTreeMap<i64, PartUpload<'_>>> = BTreeMap::new();
    let mut parts = parts;
    parts.sort_by_key(|p| (p.cand.published_at, p.cand.video_id.clone()));
    for p in parts {
        let rank = channels
            .iter()
            .position(|c| *c == p.cand.channel_id)
            .unwrap_or(usize::MAX);
        let set = sets.entry((rank, p.cand.channel_id.as_str())).or_default();
        match set.get(&p.part) {
            Some(kept) => notes.push(note(
                &p,
                UnmatchedState::Duplicate,
                format!(
                    "another upload of part {} of {what}, kept {}",
                    p.part, kept.cand.video_id
                ),
            )),
            None => {
                set.insert(p.part, p);
            }
        }
    }

    let mut whole = None;
    for (_, set) in sets {
        if whole.is_some() {
            for p in set.values() {
                notes.push(note(
                    p,
                    UnmatchedState::Duplicate,
                    format!("part {} of {what} on another channel", p.part),
                ));
            }
            continue;
        }
        match not_whole(&set) {
            None => whole = Some(set),
            Some(why) => {
                for p in set.values() {
                    notes.push(note(
                        p,
                        UnmatchedState::Partial,
                        format!("part {} of {what}, {why}", p.part),
                    ));
                }
            }
        }
    }
    let Some(set) = whole else {
        return Ok(Joined::Wait(notes));
    };

    let mut total = 0;
    let (mut height, mut size, mut latest) = (i64::MAX, Some(0i64), i64::MIN);
    let mut waiting = Vec::new();
    for p in set.values() {
        let known = metadata.known(&p.cand.video_id).await?;
        let duration = p.cand.duration_secs.or(known.duration_secs);
        match (
            duration,
            super::height_or_assumed(metadata, &known, &p.cand.channel_id),
        ) {
            (Some(d), Some(h)) => {
                total += d;
                height = height.min(h);
                size = size.zip(known.size).map(|(a, b)| a + b);
            }
            (None, _) => waiting.push((p, "duration")),
            (Some(_), None) => waiting.push((p, "video height")),
        }
        latest = latest.max(p.cand.published_at);
    }
    if !waiting.is_empty() {
        for p in set.values() {
            let pending = waiting
                .iter()
                .find(|(w, _)| w.cand.video_id == p.cand.video_id);
            notes.push(match pending {
                Some((_, fact)) => note(
                    p,
                    UnmatchedState::Pending,
                    format!(
                        "part {} of {what}, {fact} unknown until the resolver looks it up",
                        p.part
                    ),
                ),
                None => note(
                    p,
                    UnmatchedState::Partial,
                    format!(
                        "part {} of {what}, waiting for the other parts' lookups",
                        p.part
                    ),
                ),
            });
        }
        return Ok(Joined::Wait(notes));
    }
    if total < floor {
        for p in set.values() {
            notes.push(note(
                p,
                UnmatchedState::Partial,
                format!(
                    "part {} of {what}, the {} parts add up to {} min, under the {} min floor",
                    p.part,
                    set.len(),
                    total / 60,
                    floor / 60
                ),
            ));
        }
        return Ok(Joined::Wait(notes));
    }
    let first = set.values().next().map(|p| p.cand.title.clone());
    Ok(Joined::Release(
        Choice {
            ids: set.values().map(|p| p.cand.video_id.clone()).collect(),
            title: first.unwrap_or_default(),
            // The release is complete when its last part is up.
            published_at: latest,
            height,
            size,
            language: None,
        },
        notes,
    ))
}

/// Why a set is not whole, or `None` when it is: parts 1 to N with no gap, N from 2 up to what
/// one play path takes. A lone Part 1 waits for Part 2.
fn not_whole<T>(set: &BTreeMap<i64, T>) -> Option<String> {
    if let Some((want, _)) = (1..).zip(set.keys()).find(|(want, have)| want != *have) {
        return Some(format!("waiting for part {want}"));
    }
    match set.len() {
        n if n < 2 => Some(format!("waiting for part {}", n + 1)),
        n if n > MAX_PARTS => Some(format!("{n} parts, at most {MAX_PARTS} can be joined")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(parts: &[i64]) -> BTreeMap<i64, ()> {
        parts.iter().map(|p| (*p, ())).collect()
    }

    #[test]
    fn whole_sets_start_at_one_without_gaps() {
        assert_eq!(not_whole(&set(&[1, 2])), None);
        assert_eq!(not_whole(&set(&[1, 2, 3])), None);
        let why = |parts: &[i64]| not_whole(&set(parts)).unwrap();
        assert_eq!(why(&[1]), "waiting for part 2");
        assert_eq!(why(&[2, 3]), "waiting for part 1");
        assert_eq!(why(&[1, 3]), "waiting for part 2");
        assert_eq!(
            why(&[1, 2, 3, 4, 5, 6, 7]),
            "7 parts, at most 6 can be joined"
        );
    }
}
