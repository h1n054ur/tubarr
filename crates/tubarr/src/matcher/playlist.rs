//! The matcher for shows numbered by an official playlist: titles carry no episode number, so the
//! episode is the video's place in the playlist plus the mapping's offset.
//!
//! Extras the excludes catch are skipped and take no number. A short video they miss, or one not
//! looked up yet, may or may not be an episode, so every number after it is unsure (or waits)
//! until an exclude, a lookup or a pin settles it.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::store::{Db, NewUnmatched, StoreError, UnmatchedState};

use super::{
    Candidate, Candidates, Metadata, Report, WantedSeries, dedup_notes, limit,
    release_set::{Choice, pinned_choice, pins, reconcile},
    title::{DEFAULT_MIN_DURATION_SECS, excludes},
};

pub(super) async fn match_one(
    db: &Db,
    series: &WantedSeries,
    playlist_id: &str,
    candidates: &impl Candidates,
    metadata: &impl Metadata,
    report: &mut Report,
) -> Result<(), StoreError> {
    let mapping = &series.mapping;
    let excludes = match excludes(mapping) {
        Ok(re) => re,
        Err(e) => {
            tracing::warn!(mapping = mapping.id, error = %e, "mapping skipped");
            report
                .skipped
                .push((mapping.id, format!("excludes do not compile: {e}")));
            return Ok(());
        }
    };
    let Some(order) = candidates.playlist(playlist_id).await? else {
        report
            .skipped
            .push((mapping.id, format!("playlist {playlist_id} not listed yet")));
        return Ok(());
    };
    let floor = mapping
        .min_duration_secs
        .unwrap_or(DEFAULT_MIN_DURATION_SECS);
    let season = mapping.playlist_season;
    let wanted: HashSet<(i64, i64)> = series.monitored().map(|e| (e.season, e.episode)).collect();
    let pinned: BTreeMap<(i64, i64), String> = pins(db, mapping.id).await?;
    let pinned_videos: HashSet<&str> = pinned.values().flat_map(|v| v.split(',')).collect();

    // The playlist is the official source, so its videos count whichever channel they are on.
    let listed = candidates.on_channels(&mapping.channel_ids).await?;
    let seen: HashMap<&str, &Candidate> = listed.iter().map(|c| (c.video_id.as_str(), c)).collect();

    let mut notes: Vec<NewUnmatched> = Vec::new();
    let mut desired: BTreeMap<(i64, i64), Choice> = BTreeMap::new();
    let mut next = mapping.playlist_offset;
    // Set at the first video that may or may not be an episode: every later number hangs on it.
    let mut blocked: Option<(UnmatchedState, String)> = None;
    let mut done: HashSet<&str> = HashSet::new();
    for (index, video_id) in order.iter().enumerate() {
        if !done.insert(video_id.as_str()) {
            continue;
        }
        let place = index + 1;
        let cand = seen.get(video_id.as_str()).copied();
        let known = metadata.known(video_id).await?;
        let note = |state, episode: Option<i64>, reason: String| NewUnmatched {
            video_id: video_id.clone(),
            state,
            season: episode.map(|_| season),
            episode,
            part: None,
            reason,
        };
        let Some(title) = cand.map(|c| c.title.clone()).or(known.title.clone()) else {
            notes.push(note(
                UnmatchedState::Pending,
                None,
                format!("video {place} of the playlist, waiting for the resolver to look it up"),
            ));
            blocked.get_or_insert((
                UnmatchedState::Pending,
                format!("waits for video {place} of the playlist ({video_id}) to be looked up"),
            ));
            continue;
        };
        if excludes.is_match(&title) {
            continue;
        }
        match cand.and_then(|c| c.duration_secs).or(known.duration_secs) {
            None => {
                notes.push(note(
                    UnmatchedState::Pending,
                    None,
                    format!(
                        "video {place} of the playlist, duration unknown until it is looked up"
                    ),
                ));
                blocked.get_or_insert((
                    UnmatchedState::Pending,
                    format!("waits for the duration of video {place} of the playlist ({video_id})"),
                ));
                continue;
            }
            Some(d) if d < floor => {
                let why = format!(
                    "video {place} of the playlist ({video_id}, {} min) is under the {} min floor \
                     and no exclude catches it, so the numbering after it is unsure: add an \
                     exclude or pin",
                    d / 60,
                    floor / 60
                );
                notes.push(note(UnmatchedState::Unsure, None, why.clone()));
                blocked.get_or_insert((UnmatchedState::Unsure, why));
                continue;
            }
            Some(_) => {}
        }
        let target = (season, next);
        next += 1;
        if pinned_videos.contains(video_id.as_str()) {
            continue;
        }
        if let Some((state, why)) = &blocked {
            notes.push(note(
                *state,
                Some(target.1),
                format!("would be E{:02}, but {why}", target.1),
            ));
            continue;
        }
        if !wanted.contains(&target) {
            report.not_wanted += 1;
            continue;
        }
        if pinned.contains_key(&target) {
            notes.push(note(
                UnmatchedState::Duplicate,
                Some(target.1),
                format!("S{season:02}E{:02} is pinned to another video", target.1),
            ));
            continue;
        }
        let published = cand.map(|c| c.published_at).or(known.published_at);
        // Only a video on an official channel gets the assumed height.
        let height = known
            .height
            .or_else(|| cand.and_then(|c| metadata.assumed_height(&c.channel_id)));
        let (Some(height), Some(published_at)) = (height, published) else {
            notes.push(note(
                UnmatchedState::Pending,
                Some(target.1),
                "video height unknown until the resolver looks the video up".to_owned(),
            ));
            continue;
        };
        desired.insert(
            target,
            Choice {
                ids: vec![video_id.clone()],
                title,
                published_at,
                height,
                size: known.size,
                language: None,
            },
        );
    }

    let listed_refs: Vec<&Candidate> = listed.iter().collect();
    for (&(season, episode), pin) in &pinned {
        match pinned_choice(pin, &listed_refs, metadata).await? {
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
        &listed,
        &pinned_videos,
        |t| aired.get(t).copied(),
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
        Some(&series.listed()),
        report,
    )
    .await?;
    dedup_notes(&mut notes);
    report.notes_changed += db.replace_unmatched(mapping.id, &notes).await?;
    Ok(())
}
