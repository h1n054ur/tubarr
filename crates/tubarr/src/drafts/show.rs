//! The TVDB draft of a show, from what the watcher stored about its official channel.
//!
//! Episodes are numbered the way the show matcher numbers them: by the episode regex in the title
//! (the default one, or the request's own), or by place in an official playlist once it is
//! listed. Extras, short uploads and titles naming several episodes are left out and listed with
//! the reason, so Hani can see what was skipped. Parts of one episode count once.

use std::{
    collections::{BTreeMap, HashMap},
    sync::LazyLock,
};

use regex::Regex;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use super::{ChannelInfo, CheckResult, DraftError, DraftHelper, Kind, names::is_native_script};
use crate::{
    matcher::{
        Candidates, DEFAULT_NUMBER_REGEX, Metadata,
        dates::{Zone, broadcast_date, iso},
        preset::{self, PODCAST_EXCLUDES, PODCAST_MIN_DURATION_SECS, Preset},
        title::{DEFAULT_MIN_DURATION_SECS, TitleMatch, TitleRules, excludes, has_burned_subs},
    },
    store::{Mapping, MappingKind, VideoOnChannel, unix_now},
    watcher::{is_channel_id, playlists::is_playlist_id},
};

/// `POST /admin/api/drafts/show`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShowRequest {
    pub channel_id: String,
    /// The series name as the channel writes it. A channel like ARY carries many dramas, so the
    /// name picks this one's uploads (unless `regex` names the show itself).
    pub title: String,
    /// Number by place in this official playlist instead of by title.
    #[serde(default)]
    pub playlist_id: Option<String>,
    /// A custom episode regex with an `ep` group, like a mapping's.
    #[serde(default)]
    pub regex: Option<String>,
    /// The season a playlist holds. Default 1.
    #[serde(default)]
    pub season: Option<i64>,
    /// A podcast is numbered by the running `#N` in its titles, with a 20 minute floor and the
    /// clip words left out, see [`preset`].
    #[serde(default)]
    pub preset: Preset,
    /// The channel's offset from UTC, over its `[channels]` entry. A podcast on a channel not
    /// listed there needs it.
    #[serde(default)]
    pub utc_offset_mins: Option<i64>,
    /// The channel's IANA time zone, like "America/Chicago", over `utc_offset_mins` and its
    /// `[channels]` entry.
    #[serde(default)]
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EpisodeDraft {
    pub season: i64,
    pub number: i64,
    /// The YouTube title's own episode name, or TVDB's usual "Episode N".
    pub name: String,
    /// Channel-local upload day, `YYYY-MM-DD`.
    pub aired: Option<String>,
    /// The upload time is a guess (a channel list's "3 days ago" or a bare upload date).
    pub aired_approx: bool,
    pub runtime_mins: Option<i64>,
    /// The uploads it was drafted from: one, or one per part.
    pub video_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Skipped {
    pub video_id: String,
    pub title: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShowDraft {
    pub series_name: String,
    /// The name in Urdu or Hindi script, when the channel's titles carry it.
    pub original_title: Option<String>,
    pub network: Option<String>,
    pub country: String,
    pub original_language: String,
    pub first_aired: Option<String>,
    /// A guess: "Ended" after a "Last Episode" upload or three quiet weeks, else "Continuing".
    pub status: String,
    pub channel_id: String,
    pub youtube_channel: String,
    /// `title` (episode regex) or `playlist`.
    pub numbering: String,
    pub episodes: Vec<EpisodeDraft>,
    pub skipped: Vec<Skipped>,
    pub check: CheckResult,
}

/// Three weeks without an episode and a weekly drama is probably over.
const QUIET_DAYS: i64 = 21;

static ENDED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:last|final|concluding)\s+episode\b").expect("valid ended regex")
});
/// Title segments that are marketing rather than an episode name.
static NOISE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:eng(?:lish)?|sub(?:title)?s?|subbed|drama|dramas|digital|tv|presented|sponsored|powered|official|full|hd|4k|new|latest|pakistani|indian|serial|channel|ost|episode|ep|part|season)\b",
    )
    .expect("valid noise regex")
});

/// A throwaway mapping, so the draft reads titles exactly as the show matcher would. A podcast
/// reads the `#N` in its titles as the episode number.
fn mapping(request: &ShowRequest) -> Mapping {
    let podcast = request.preset == Preset::Podcast;
    let regex = match &request.regex {
        Some(regex) => Some(regex.clone()),
        None if podcast => Some(DEFAULT_NUMBER_REGEX.to_owned()),
        None => None,
    };
    let excludes = if podcast {
        PODCAST_EXCLUDES.iter().map(|w| (*w).to_owned()).collect()
    } else {
        Vec::new()
    };
    Mapping {
        id: 0,
        kind: MappingKind::Show,
        tvdb_id: None,
        tmdb_id: None,
        title: request.title.clone(),
        language: String::new(),
        channel_ids: vec![request.channel_id.clone()],
        episode_regex: regex,
        excludes,
        min_duration_secs: podcast.then_some(PODCAST_MIN_DURATION_SECS),
        created_at: 0,
        updated_at: 0,
        active: true,
        arr_id: None,
        year: None,
        imdb_id: None,
        utc_offset_mins: None,
        timezone: None,
        playlist_id: request.playlist_id.clone(),
        playlist_season: request.season.unwrap_or(1),
        playlist_offset: 1,
        playlist_mode: crate::store::PlaylistMode::Order,
        backfill_limit: None,
        title_number: false,
        audio_version: false,
        episode_thumbnails: false,
    }
}

/// One upload that counts toward an episode.
struct Upload {
    video_id: String,
    title: String,
    part: Option<i64>,
    published_at: i64,
    approx: bool,
    duration_secs: Option<i64>,
}

/// The day an upload aired. A guessed time is only good for its UTC date.
fn air_date(published_at: i64, approx: bool, zone: &Zone) -> Option<String> {
    let date = if approx {
        OffsetDateTime::from_unix_timestamp(published_at)
            .ok()?
            .date()
    } else {
        broadcast_date(published_at, zone)?
    };
    Some(iso(date))
}

/// The episode's own name, when exactly one title segment is left once the show name, episode
/// tag, channel name, dates and marketing are taken out. Otherwise `None`.
fn episode_name(title: &str, show: &str, network: Option<&str>) -> Option<String> {
    let show = super::names::normalize(show);
    let network = network.map(super::names::normalize);
    let left: Vec<&str> = title
        .split('|')
        .flat_map(|s| s.split(" - "))
        .map(str::trim)
        .filter(|seg| {
            let norm = super::names::normalize(seg);
            !norm.is_empty()
                && !norm.contains(&show)
                && network.as_ref().is_none_or(|n| !norm.contains(n.as_str()))
                && !seg.contains('#')
                && !seg.chars().any(|c| c.is_ascii_digit())
                && !is_native_script(seg)
                && !NOISE.is_match(seg)
                && norm.chars().filter(|c| c.is_alphabetic()).count() >= 3
        })
        .collect();
    match left.as_slice() {
        [one] => Some((*one).to_owned()),
        _ => None,
    }
}

/// The words in Urdu or Hindi script, joined, if a title carries any.
fn native_part(title: &str) -> Option<String> {
    let words: Vec<&str> = title
        .split(|c: char| c.is_whitespace() || c == '|' || c == '-')
        .filter(|w| is_native_script(w))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

impl DraftHelper {
    /// Drafts a show from stored channel videos. Costs no YouTube lookups.
    pub async fn show_draft(&self, request: &ShowRequest) -> Result<ShowDraft, DraftError> {
        if !is_channel_id(&request.channel_id) {
            return Err(DraftError::BadRequest(format!(
                "{:?} is not a channel id (UC...)",
                request.channel_id
            )));
        }
        if let Some(playlist) = &request.playlist_id
            && !is_playlist_id(playlist)
        {
            return Err(DraftError::BadRequest(format!(
                "{playlist:?} is not a playlist id (PL...)"
            )));
        }
        if request.title.trim().is_empty() {
            return Err(DraftError::BadRequest("title must not be empty".to_owned()));
        }
        let mapping = mapping(request);
        let rules = TitleRules::new(&mapping).map_err(|e| DraftError::BadRequest(e.to_string()))?;
        let info = self.channel(&request.channel_id);
        let ids = std::slice::from_ref(&request.channel_id);
        let (timezone, offset) = (request.timezone.as_deref(), request.utc_offset_mins);
        let zone = match request.preset {
            Preset::Podcast => preset::podcast_zone(timezone, offset, &self.channels, ids),
            Preset::Drama => preset::channel_zone(timezone, offset, &self.channels, ids)
                .map(Option::unwrap_or_default),
        }
        .and_then(|z| z.zone())
        .map_err(DraftError::BadRequest)?;
        let videos = self
            .db
            .videos_on_channels(std::slice::from_ref(&request.channel_id))
            .await?;
        if videos.is_empty() {
            return Err(DraftError::NotReady(format!(
                "no videos stored for channel {} yet: map a show on it so the watcher reads it",
                request.channel_id
            )));
        }
        let mut skipped = Vec::new();
        let (groups, numbering) = match &request.playlist_id {
            Some(playlist) => (
                self.by_playlist(&mapping, playlist, &videos, &mut skipped)
                    .await?,
                "playlist",
            ),
            None => (
                self.by_title(&rules, &mapping, &videos, &mut skipped)
                    .await?,
                "title",
            ),
        };
        if groups.is_empty() {
            return Err(DraftError::NotReady(format!(
                "no episodes of {:?} found among {} stored videos of the channel",
                request.title,
                videos.len()
            )));
        }
        let episodes: Vec<EpisodeDraft> = groups
            .into_iter()
            .map(|((season, number), uploads)| {
                episode(season, number, uploads, request, &info, &zone)
            })
            .collect();

        let mut originals: HashMap<String, usize> = HashMap::new();
        for e in &episodes {
            for v in &e.video_ids {
                if let Some(title) = videos.iter().find(|c| c.video_id == *v).map(|c| &c.title)
                    && let Some(native) = native_part(title)
                {
                    *originals.entry(native).or_default() += 1;
                }
            }
        }
        let original_title = originals
            .into_iter()
            .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
            .map(|(t, _)| t);
        let first_aired = episodes.iter().filter_map(|e| e.aired.clone()).min();
        let last_upload = videos
            .iter()
            .filter(|v| episodes.iter().any(|e| e.video_ids.contains(&v.video_id)))
            .map(|v| v.published_at)
            .max()
            .unwrap_or(0);
        let ended = videos.iter().any(|v| {
            ENDED.is_match(&v.title)
                && super::names::normalize(&v.title)
                    .contains(&super::names::normalize(&request.title))
        });
        let status = if ended || unix_now() - last_upload > QUIET_DAYS * 86_400 {
            "Ended"
        } else {
            "Continuing"
        };
        let check = self
            .check(Kind::Show, &request.title, original_title.as_deref(), None)
            .await;
        Ok(ShowDraft {
            series_name: request.title.trim().to_owned(),
            original_title,
            network: info.network.clone(),
            country: info.country.clone(),
            original_language: info.language.clone(),
            first_aired,
            status: status.to_owned(),
            channel_id: request.channel_id.clone(),
            youtube_channel: format!("https://www.youtube.com/channel/{}", request.channel_id),
            numbering: numbering.to_owned(),
            episodes,
            skipped,
            check,
        })
    }

    async fn by_title(
        &self,
        rules: &TitleRules,
        mapping: &Mapping,
        videos: &[VideoOnChannel],
        skipped: &mut Vec<Skipped>,
    ) -> Result<BTreeMap<(i64, i64), Vec<Upload>>, DraftError> {
        let show = super::names::normalize(&mapping.title);
        let mut groups: BTreeMap<(i64, i64), Vec<Upload>> = BTreeMap::new();
        for v in videos {
            let skip = |reason: &str| Skipped {
                video_id: v.video_id.clone(),
                title: v.title.clone(),
                reason: reason.to_owned(),
            };
            let names_show =
                format!(" {} ", super::names::normalize(&v.title)).contains(&format!(" {show} "));
            match rules.read(&v.title) {
                // Other dramas on the channel are not worth a line; this show's extras are.
                TitleMatch::NotThisShow => {
                    if names_show {
                        skipped.push(skip("an extra (promo, teaser, highlights...)"));
                    }
                }
                TitleMatch::NoNumber => skipped.push(skip("no episode number in the title")),
                TitleMatch::Several(n) => {
                    skipped.push(skip(&format!("names several episodes {n:?}")));
                }
                TitleMatch::Episode {
                    season,
                    episode,
                    part,
                } => {
                    let duration = self.duration(v).await?;
                    if part.is_none() && duration.is_some_and(|d| d < floor(mapping)) {
                        skipped.push(skip(&format!(
                            "shorter than {} minutes, likely a highlight",
                            floor(mapping) / 60
                        )));
                        continue;
                    }
                    groups.entry((season, episode)).or_default().push(Upload {
                        video_id: v.video_id.clone(),
                        title: v.title.clone(),
                        part,
                        published_at: v.published_at,
                        approx: v.published_approx,
                        duration_secs: duration,
                    });
                }
            }
        }
        Ok(groups)
    }

    async fn by_playlist(
        &self,
        mapping: &Mapping,
        playlist: &str,
        videos: &[VideoOnChannel],
        skipped: &mut Vec<Skipped>,
    ) -> Result<BTreeMap<(i64, i64), Vec<Upload>>, DraftError> {
        let Some(order) = self.db.playlist(playlist).await? else {
            self.db.request_playlist(playlist).await?;
            return Err(DraftError::NotReady(format!(
                "playlist {playlist} has not been listed yet: the watcher lists it within a few \
                 minutes, try again then"
            )));
        };
        let excludes = excludes(mapping).map_err(|e| DraftError::BadRequest(e.to_string()))?;
        let by_id: HashMap<&str, &VideoOnChannel> =
            videos.iter().map(|v| (v.video_id.as_str(), v)).collect();
        let mut groups = BTreeMap::new();
        let mut next = mapping.playlist_offset;
        for video_id in order {
            let Some(v) = by_id.get(video_id.as_str()) else {
                skipped.push(Skipped {
                    video_id: video_id.clone(),
                    title: String::new(),
                    reason: "in the playlist but not stored yet; later numbers may be off"
                        .to_owned(),
                });
                continue;
            };
            let skip = |reason: &str| Skipped {
                video_id: v.video_id.clone(),
                title: v.title.clone(),
                reason: reason.to_owned(),
            };
            if excludes.is_match(&v.title) {
                skipped.push(skip("an extra (promo, teaser, highlights...)"));
                continue;
            }
            let duration = self.duration(v).await?;
            if duration.is_some_and(|d| d < floor(mapping)) {
                skipped.push(skip(&format!(
                    "shorter than {} minutes, not counted; check the numbering",
                    floor(mapping) / 60
                )));
                continue;
            }
            groups.insert(
                (mapping.playlist_season, next),
                vec![Upload {
                    video_id: v.video_id.clone(),
                    title: v.title.clone(),
                    part: None,
                    published_at: v.published_at,
                    approx: v.published_approx,
                    duration_secs: duration,
                }],
            );
            next += 1;
        }
        Ok(groups)
    }

    async fn duration(&self, v: &VideoOnChannel) -> Result<Option<i64>, DraftError> {
        Ok(match v.duration_secs {
            Some(d) => Some(d),
            None => self.db.known(&v.video_id).await?.duration_secs,
        })
    }
}

/// The length under which an upload is a clip or a highlight.
fn floor(mapping: &Mapping) -> i64 {
    mapping
        .min_duration_secs
        .unwrap_or(DEFAULT_MIN_DURATION_SECS)
}

/// A podcast episode's name: the title without the show's name, kept with its `#N` so the
/// matcher's `title_number` finds it on TVDB. "Joe Rogan Experience #2569 - Bill Thompson" on
/// "Joe Rogan Experience" is "#2569 - Bill Thompson".
fn podcast_name(title: &str, show: &str, number: i64) -> String {
    let tag = format!("#{number}");
    let name = Regex::new(&format!(r"(?i){}", regex::escape(show.trim())))
        .map(|re| re.replace(title, "").into_owned())
        .unwrap_or_else(|_| title.to_owned());
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    let name = name.trim_matches(|c: char| c.is_whitespace() || "|-:".contains(c));
    if name.is_empty() {
        tag
    } else if name.contains(&tag) {
        name.to_owned()
    } else {
        format!("{tag} - {name}")
    }
}

/// One episode from its uploads: a whole upload if there is one (clean before burned-in
/// subtitles, earliest first), else one upload per part with the runtimes added up.
fn episode(
    season: i64,
    number: i64,
    mut uploads: Vec<Upload>,
    request: &ShowRequest,
    info: &ChannelInfo,
    zone: &Zone,
) -> EpisodeDraft {
    uploads.sort_by_key(|u| (has_burned_subs(&u.title), u.published_at));
    let chosen: Vec<&Upload> = match uploads.iter().find(|u| u.part.is_none()) {
        Some(whole) => vec![whole],
        None => {
            let mut parts: BTreeMap<i64, &Upload> = BTreeMap::new();
            for u in &uploads {
                if let Some(p) = u.part {
                    parts.entry(p).or_insert(u);
                }
            }
            parts.into_values().collect()
        }
    };
    let first = chosen
        .iter()
        .min_by_key(|u| u.published_at)
        .copied()
        .unwrap_or(&uploads[0]);
    let runtime_secs: Option<i64> = chosen.iter().map(|u| u.duration_secs).sum();
    let name = match request.preset {
        Preset::Podcast => podcast_name(&first.title, &request.title, number),
        Preset::Drama => chosen
            .iter()
            .find_map(|u| episode_name(&u.title, &request.title, info.network.as_deref()))
            .unwrap_or_else(|| format!("Episode {number}")),
    };
    EpisodeDraft {
        season,
        number,
        name,
        aired: air_date(first.published_at, first.approx, zone),
        aired_approx: first.approx,
        runtime_mins: runtime_secs.map(|s| (s + 30) / 60),
        video_ids: chosen.iter().map(|u| u.video_id.clone()).collect(),
    }
}

/// The copy-paste block, in the order of TVDB's "add series" and "add episodes" forms.
pub fn text(draft: &ShowDraft) -> String {
    let opt = |v: &Option<String>| v.clone().unwrap_or_else(|| "(unknown)".to_owned());
    let mut out = String::new();
    out.push_str(&format!("{}\n\n", draft.check.summary()));
    out.push_str("TVDB: add series\n");
    out.push_str(&format!("Series name: {}\n", draft.series_name));
    out.push_str(&format!(
        "Original name ({}): {}\n",
        draft.original_language,
        opt(&draft.original_title)
    ));
    out.push_str(&format!("Country of origin: {}\n", draft.country));
    out.push_str(&format!("Original language: {}\n", draft.original_language));
    out.push_str(&format!("Network: {}\n", opt(&draft.network)));
    out.push_str(&format!("First aired: {}\n", opt(&draft.first_aired)));
    out.push_str(&format!("Status: {}\n", draft.status));
    out.push_str("Season type: Aired order\n");
    out.push_str(&format!("Official channel: {}\n", draft.youtube_channel));
    out.push_str("\nTVDB: add episodes (season, number, name, aired, runtime)\n");
    for e in &draft.episodes {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}{}\t{}\n",
            e.season,
            e.number,
            e.name,
            opt(&e.aired),
            if e.aired_approx { " (approx)" } else { "" },
            e.runtime_mins.map_or_else(String::new, |m| m.to_string()),
        ));
    }
    if !draft.skipped.is_empty() {
        out.push_str("\nLeft out:\n");
        for s in &draft.skipped {
            out.push_str(&format!("{} {}: {}\n", s.video_id, s.title, s.reason));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_episode_names_are_kept_and_marketing_is_not() {
        assert_eq!(
            episode_name(
                "Kabhi Main Kabhi Tum Episode 3 | The Wedding | ARY Digital",
                "Kabhi Main Kabhi Tum",
                Some("ARY Digital"),
            ),
            Some("The Wedding".to_owned())
        );
        assert_eq!(
            episode_name(
                "Kabhi Main Kabhi Tum Episode 3 | Eng Sub | 2 Sept 2024 | ARY Digital",
                "Kabhi Main Kabhi Tum",
                Some("ARY Digital"),
            ),
            None
        );
        assert_eq!(
            episode_name(
                "Kabhi Main Kabhi Tum - Ep 1 - #fahadmustafa",
                "Kabhi Main Kabhi Tum",
                None
            ),
            None
        );
    }

    #[test]
    fn podcast_names_keep_the_number_and_drop_the_show() {
        assert_eq!(
            podcast_name(
                "Joe Rogan Experience #2569 - Bill Thompson",
                "Joe Rogan Experience",
                2569
            ),
            "#2569 - Bill Thompson"
        );
        assert_eq!(
            podcast_name(
                "Nate Bargatze | This Past Weekend w/ Theo Von #660",
                "This Past Weekend w/ Theo Von",
                660
            ),
            "Nate Bargatze | #660"
        );
        assert_eq!(podcast_name("Some Show", "Some Show", 12), "#12");
    }

    #[test]
    fn native_script_words_are_pulled_out() {
        assert_eq!(
            native_part("Kabhi Main Kabhi Tum Ep 1 | کبھی میں کبھی تم"),
            Some("کبھی میں کبھی تم".to_owned())
        );
        assert_eq!(native_part("Kabhi Main Kabhi Tum Ep 1"), None);
    }

    #[test]
    fn air_dates_use_the_broadcast_day() {
        // 2026-10-05 19:00 UTC is 2026-10-06 00:00 in Pakistan: still the 5th's broadcast.
        assert_eq!(
            air_date(1_791_226_800, false, &Zone::new(None, Some(300)).unwrap()),
            Some("2026-10-05".to_owned())
        );
        // A bare upload date is midnight UTC and stays that day.
        assert_eq!(
            air_date(1_791_158_400, true, &Zone::new(None, Some(300)).unwrap()),
            Some("2026-10-05".to_owned())
        );
    }
}
