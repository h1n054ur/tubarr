//! The `[channels]` config section: what Tubarr knows about each official channel, by channel id.
//!
//! Drafts read the network, country and offset from it. The arr sync and the admin API read the
//! language, since Sonarr has no Urdu and reports such shows as English: a show whose first
//! channel is listed here gets the channel's language in its release names instead.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::{
    config::{ConfigError, MAX_ASSUMED_HEIGHT},
    matcher::dates::{DEFAULT_UTC_OFFSET_MINS, timezone_name},
    watcher::is_channel_id,
};

/// One official channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ChannelInfo {
    /// The TV network the channel belongs to, like "ARY Digital". None for a film label.
    pub network: Option<String>,
    pub country: String,
    /// Original language of what the channel uploads, as an English name.
    pub language: String,
    /// The channel's offset from UTC, to turn an upload time into an air date.
    pub utc_offset_mins: i64,
    /// The channel's IANA time zone, like "America/Chicago". Wins over `utc_offset_mins`, and
    /// follows DST where the offset cannot.
    pub timezone: Option<String>,
    /// The Sonarr or Radarr root folder (and so the Jellyfin library) the channel's titles live
    /// in. `show_root_folder` and `film_root_folder` win over it for their kind.
    pub root_folder: Option<String>,
    pub show_root_folder: Option<String>,
    pub film_root_folder: Option<String>,
    /// How many of the newest uploads the channel's daily list reads, over `watcher.listing_size`.
    /// Busy channels (dozens of uploads a day) need more to reach back a few weeks.
    pub listing_size: Option<u32>,
    /// The height the channel's releases are named with until a lookup gives the real one, over
    /// `matcher.assumed_height`. 0 waits for a lookup.
    pub assumed_height: Option<u32>,
}

impl Default for ChannelInfo {
    fn default() -> Self {
        Self {
            network: None,
            country: "Pakistan".to_owned(),
            language: "Urdu".to_owned(),
            utc_offset_mins: DEFAULT_UTC_OFFSET_MINS,
            timezone: None,
            root_folder: None,
            show_root_folder: None,
            film_root_folder: None,
            listing_size: None,
            assumed_height: None,
        }
    }
}

/// The configured channels, checked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Channels(BTreeMap<String, ChannelInfo>);

impl Channels {
    /// Checks the entries, naming `key` (`channels`, or `drafts.channels` in older files) in errors.
    pub(crate) fn check(
        key: &'static str,
        mut map: BTreeMap<String, ChannelInfo>,
    ) -> Result<Self, ConfigError> {
        let invalid = |reason: String| ConfigError::Invalid { key, reason };
        for (id, info) in &mut map {
            if !is_channel_id(id) {
                return Err(invalid(format!(
                    "{id:?} is not a channel id (UC...). A mapping's Official channels field takes a handle or link and shows its id"
                )));
            }
            if !(-720..=840).contains(&info.utc_offset_mins) {
                return Err(invalid(format!(
                    "{id}: utc_offset_mins must be between -720 and 840"
                )));
            }
            if let Some(name) = &info.timezone {
                info.timezone =
                    Some(timezone_name(name).map_err(|e| invalid(format!("{id}: {e}")))?);
            }
            if info.network.as_deref().is_some_and(|n| n.trim().is_empty())
                || info.country.trim().is_empty()
                || info.language.trim().is_empty()
            {
                return Err(invalid(format!(
                    "{id}: network, country and language must not be empty"
                )));
            }
            if info.listing_size.is_some_and(|n| !(1..=1000).contains(&n)) {
                return Err(invalid(format!(
                    "{id}: listing_size must be between 1 and 1000"
                )));
            }
            if info.assumed_height.is_some_and(|h| h > MAX_ASSUMED_HEIGHT) {
                return Err(invalid(format!(
                    "{id}: assumed_height must be at most {MAX_ASSUMED_HEIGHT}"
                )));
            }
            for folder in [
                &info.root_folder,
                &info.show_root_folder,
                &info.film_root_folder,
            ]
            .into_iter()
            .flatten()
            {
                if !is_root_folder(folder) {
                    return Err(invalid(format!(
                        "{id}: root folder {folder:?} must be an absolute path"
                    )));
                }
            }
        }
        Ok(Self(map))
    }

    /// The configured entry, if the channel is listed.
    pub fn get(&self, channel_id: &str) -> Option<&ChannelInfo> {
        self.0.get(channel_id)
    }

    /// The language of a listed channel. `None` for a channel not in the config: the defaults of
    /// [`ChannelInfo`] are only a starting point for drafts, never a reason to rename releases.
    pub fn language(&self, channel_id: &str) -> Option<&str> {
        self.get(channel_id).map(|c| c.language.trim())
    }

    /// The root folder a listed channel's shows (or films) go to, if one is set.
    pub fn root_folder(&self, channel_id: &str, film: bool) -> Option<&str> {
        let info = self.get(channel_id)?;
        let specific = if film {
            &info.film_root_folder
        } else {
            &info.show_root_folder
        };
        specific
            .as_ref()
            .or(info.root_folder.as_ref())
            .map(String::as_str)
    }

    /// The channels with their own `listing_size`.
    pub fn listing_sizes(&self) -> HashMap<String, u32> {
        self.0
            .iter()
            .filter_map(|(id, info)| info.listing_size.map(|n| (id.clone(), n)))
            .collect()
    }

    /// The channels with their own `assumed_height`.
    pub fn assumed_heights(&self) -> HashMap<String, u32> {
        self.0
            .iter()
            .filter_map(|(id, info)| info.assumed_height.map(|h| (id.clone(), h)))
            .collect()
    }

    /// Every listed channel, for the Settings page.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ChannelInfo)> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A root folder as the arrs take it: an absolute path. A trailing slash is allowed, since the
/// arrs list theirs either way.
pub(crate) fn is_root_folder(path: &str) -> bool {
    path.starts_with('/') && path.trim_end_matches('/').len() > 1 && path.trim() == path
}

/// Reads `[channels]`, or `[drafts.channels]` from files written before the section moved.
pub(crate) fn from_file(
    channels: BTreeMap<String, ChannelInfo>,
    old: BTreeMap<String, ChannelInfo>,
) -> Result<Channels, ConfigError> {
    match (channels.is_empty(), old.is_empty()) {
        (_, true) => Channels::check("channels", channels),
        (true, false) => Channels::check("drafts.channels", old),
        (false, false) => Err(ConfigError::Invalid {
            key: "drafts.channels",
            reason: "moved to [channels], set only one of them".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";

    fn one(id: &str, language: &str) -> BTreeMap<String, ChannelInfo> {
        let info = ChannelInfo {
            language: language.to_owned(),
            ..ChannelInfo::default()
        };
        BTreeMap::from([(id.to_owned(), info)])
    }

    #[test]
    fn only_listed_channels_have_a_language() {
        let channels = from_file(one(ARY, "Urdu"), BTreeMap::new()).unwrap();
        assert_eq!(channels.language(ARY), Some("Urdu"));
        assert_eq!(channels.language("UCxxxxxxxxxxxxxxxxxxxxxx"), None);
    }

    #[test]
    fn the_old_drafts_key_still_reads_but_not_together_with_the_new_one() {
        let old = from_file(BTreeMap::new(), one(ARY, "Urdu")).unwrap();
        assert_eq!(old.language(ARY), Some("Urdu"));
        let both = from_file(one(ARY, "Urdu"), one(ARY, "Urdu")).unwrap_err();
        assert!(both.to_string().contains("drafts.channels"), "{both}");
    }

    #[test]
    fn root_folders_by_kind_and_checked() {
        let mut map = one(ARY, "Urdu");
        let info = map.get_mut(ARY).unwrap();
        info.root_folder = Some("/data/media/pk-dramas".to_owned());
        info.film_root_folder = Some("/data/media/pk-films/".to_owned());
        let channels = from_file(map.clone(), BTreeMap::new()).unwrap();
        assert_eq!(
            channels.root_folder(ARY, false),
            Some("/data/media/pk-dramas")
        );
        assert_eq!(
            channels.root_folder(ARY, true),
            Some("/data/media/pk-films/")
        );
        assert_eq!(
            channels.root_folder("UCxxxxxxxxxxxxxxxxxxxxxx", false),
            None
        );

        for bad in ["data/media", "/", " /data", ""] {
            map.get_mut(ARY).unwrap().show_root_folder = Some(bad.to_owned());
            let err = from_file(map.clone(), BTreeMap::new()).unwrap_err();
            assert!(err.to_string().contains("absolute path"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn time_zones_are_checked_and_spelled_as_the_database_does() {
        let mut map = one(ARY, "Urdu");
        map.get_mut(ARY).unwrap().timezone = Some("america/new_york".to_owned());
        let channels = from_file(map.clone(), BTreeMap::new()).unwrap();
        assert_eq!(
            channels.get(ARY).unwrap().timezone.as_deref(),
            Some("America/New_York")
        );
        map.get_mut(ARY).unwrap().timezone = Some("Mars/Olympus".to_owned());
        let err = from_file(map, BTreeMap::new()).unwrap_err();
        assert!(err.to_string().contains("Mars/Olympus"), "{err}");
    }

    #[test]
    fn bad_entries_name_the_key_they_came_from() {
        let err = from_file(one("ARY", "Urdu"), BTreeMap::new()).unwrap_err();
        assert!(err.to_string().contains("`channels`"), "{err}");
        let err = from_file(BTreeMap::new(), one(ARY, " ")).unwrap_err();
        assert!(err.to_string().contains("drafts.channels"), "{err}");
    }
}
