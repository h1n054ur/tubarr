//! Writing the chosen videos of one mapping to `releases`, for any target shape.

use std::{
    collections::{BTreeMap, HashSet},
    hash::Hash,
};

use crate::arr::PushTarget;
use crate::store::{Db, EpisodeFilter, Mapping, NewRelease, PinTarget, StoreError};

use super::{Candidate, Metadata, Report};

/// A release target as a map key: (season, episode), an air date, or `()` for a film.
pub(super) trait Target: Ord + Clone + Hash {
    fn to_pin(&self) -> PinTarget;
    fn from_pin(target: &PinTarget) -> Option<Self>;
}

impl Target for (i64, i64) {
    fn to_pin(&self) -> PinTarget {
        PinTarget::Episode {
            season: self.0,
            episode: self.1,
        }
    }
    fn from_pin(target: &PinTarget) -> Option<Self> {
        match target {
            PinTarget::Episode { season, episode } => Some((*season, *episode)),
            _ => None,
        }
    }
}

/// `YYYY-MM-DD`.
impl Target for String {
    fn to_pin(&self) -> PinTarget {
        PinTarget::AirDate(self.clone())
    }
    fn from_pin(target: &PinTarget) -> Option<Self> {
        match target {
            PinTarget::AirDate(date) => Some(date.clone()),
            _ => None,
        }
    }
}

/// The film itself: a film mapping has one target.
impl Target for () {
    fn to_pin(&self) -> PinTarget {
        PinTarget::Film
    }
    fn from_pin(target: &PinTarget) -> Option<Self> {
        matches!(target, PinTarget::Film).then_some(())
    }
}

/// The video chosen for one target, or the parts of one split upload in play order.
pub(super) struct Choice {
    pub ids: Vec<String>,
    pub title: String,
    pub published_at: i64,
    pub height: i64,
    /// Bytes, when known for every part.
    pub size: Option<i64>,
    /// The release language when the video says otherwise than the mapping, like a Hindi dub.
    pub language: Option<String>,
}

impl Choice {
    /// What the release, its NZB and its `/play` link carry: the video id, or the part ids joined
    /// by commas. A pin holds the same.
    pub fn key(&self) -> String {
        self.ids.join(",")
    }
}

/// The mapping's pins of this target shape: target to video id, or to part ids joined by commas.
pub(super) async fn pins<K: Target>(
    db: &Db,
    mapping_id: i64,
) -> Result<BTreeMap<K, String>, StoreError> {
    Ok(db
        .pins(mapping_id)
        .await?
        .into_iter()
        .filter_map(|p| K::from_pin(&p.target).map(|k| (k, p.video_id)))
        .collect())
}

/// Brings the mapping's releases of this shape in line with `desired`: missing ones are written,
/// kept ones take the desired height and size, other videos on a desired target removed, and a
/// pinned video's releases on targets it is not pinned to removed. Targets with no desired video
/// keep whatever they have, except with `listed`, every target Sonarr lists (monitored or not) of
/// a show: then a release also goes when Sonarr no longer lists its target, or when its video is
/// desired on another target, so one upload never holds two episodes. An empty list
/// says nothing (a series Sonarr has not filled yet) and retires nothing.
pub(super) async fn reconcile<K: Target>(
    db: &Db,
    mapping: &Mapping,
    pinned: &BTreeMap<K, String>,
    desired: &BTreeMap<K, Choice>,
    listed: Option<&HashSet<K>>,
    report: &mut Report,
) -> Result<(), StoreError> {
    let listed = listed.filter(|l| !l.is_empty());
    let existing = db
        .releases_for_mappings(&[mapping.id], &EpisodeFilter::Any, i64::MAX)
        .await?;
    let mut have: HashSet<(K, String)> = HashSet::new();
    let released_before: HashSet<K> = existing
        .iter()
        .filter_map(|r| K::from_pin(&r.target))
        .collect();
    for release in &existing {
        let Some(target) = K::from_pin(&release.target) else {
            continue;
        };
        let key = release.key();
        let stale_on_target = desired.get(&target).is_some_and(|c| c.key() != key);
        let pinned_elsewhere = pinned.iter().any(|(t, v)| *v == key && *t != target);
        let pinned_here = pinned.get(&target) == Some(&key);
        let retired = listed.is_some_and(|listed| {
            !desired.contains_key(&target)
                && (!listed.contains(&target) || desired_elsewhere(desired, &target, &key))
        });
        if stale_on_target || (pinned_elsewhere && !pinned_here) || retired {
            db.delete_release(release.id).await?;
            report.replaced += 1;
            continue;
        }
        // The same video with a different height, size or upload time: an assumed height the
        // resolver has since replaced with the real one, or a channel list's guessed date a feed
        // has since given exactly.
        if let Some(choice) = desired.get(&target).filter(|c| c.key() == key)
            && (choice.height != release.height
                || choice.size != release.size
                || choice.published_at != release.published_at)
        {
            tracing::info!(
                release = release.id,
                from = release.height,
                to = choice.height,
                published_at = choice.published_at,
                "release corrected"
            );
            db.correct_release(release.id, choice.height, choice.size, choice.published_at)
                .await?;
        }
        have.insert((target, key));
    }
    for (target, choice) in desired {
        if have.contains(&(target.clone(), choice.key())) {
            continue;
        }
        let (video_id, part_ids) = match choice.ids.as_slice() {
            [one] => (one.clone(), Vec::new()),
            [first, ..] => (first.clone(), choice.ids.clone()),
            [] => continue,
        };
        let pin = target.to_pin();
        // Film release names carry the year, and Radarr reads the IMDb id.
        let film = pin == PinTarget::Film;
        db.insert_release(&NewRelease {
            mapping_id: mapping.id,
            video_id,
            part_ids,
            target: pin,
            year: mapping.year.filter(|_| film),
            imdb_id: mapping
                .imdb_id
                .as_deref()
                .and_then(imdb_digits)
                .filter(|_| film),
            video_title: choice.title.clone(),
            language: choice
                .language
                .clone()
                .unwrap_or_else(|| mapping.language.clone()),
            height: choice.height,
            size: choice.size,
            published_at: choice.published_at,
        })
        .await?;
        report.released += 1;
        if !released_before.contains(target)
            && let Some(push) =
                PushTarget::for_release(mapping.kind, mapping.arr_id, &target.to_pin())
        {
            report.first_released.push(push);
        }
    }
    Ok(())
}

/// Whether a video of the release `key` (one id, or part ids joined by commas) is desired on
/// another target than `target`.
fn desired_elsewhere<K: Target>(desired: &BTreeMap<K, Choice>, target: &K, key: &str) -> bool {
    let ids: HashSet<&str> = key.split(',').collect();
    desired
        .iter()
        .any(|(t, c)| t != target && c.ids.iter().any(|id| ids.contains(id.as_str())))
}

/// The release for a pin: one video id, or part ids joined by commas. From the channel list or
/// kept metadata. `Err` names the videos the resolver still has to look up.
pub(super) async fn pinned_choice(
    pin: &str,
    on_channels: &[&Candidate],
    metadata: &impl Metadata,
) -> Result<Result<Choice, Vec<String>>, StoreError> {
    let ids: Vec<String> = pin.split(',').map(str::to_owned).collect();
    let mut first_title: Option<String> = None;
    let (mut height, mut size, mut latest) = (i64::MAX, Some(0i64), i64::MIN);
    let mut missing = Vec::new();
    for id in &ids {
        let known = metadata.known(id).await?;
        let cand = on_channels.iter().find(|c| &c.video_id == id);
        let title = cand.map(|c| c.title.clone()).or(known.title);
        let published = cand.map(|c| c.published_at).or(known.published_at);
        // Only a video on an official channel gets the assumed height.
        let part_height = known
            .height
            .or_else(|| cand.and_then(|c| metadata.assumed_height(&c.channel_id)));
        let (Some(title), Some(published_at), Some(h)) = (title, published, part_height) else {
            missing.push(id.clone());
            continue;
        };
        first_title.get_or_insert(title);
        height = height.min(h);
        latest = latest.max(published_at);
        size = size.zip(known.size).map(|(a, b)| a + b);
    }
    Ok(match first_title {
        Some(title) if missing.is_empty() => Ok(Choice {
            ids,
            title,
            // A joined release is complete when its last part is up.
            published_at: latest,
            height,
            size,
            language: None,
        }),
        _ => Err(missing),
    })
}

/// `tt4938374` as 4938374.
fn imdb_digits(id: &str) -> Option<i64> {
    id.trim()
        .trim_start_matches("tt")
        .parse()
        .ok()
        .filter(|n| *n > 0)
}
