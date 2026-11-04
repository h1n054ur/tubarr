//! Starting rules for a new show: a drama (the plain defaults) or a video podcast.
//!
//! A podcast is matched by its running number (`title_number`), releases only its newest back
//! episodes, keeps clips out by length and by title, and needs its channel's time zone for the
//! air date fallback. The zone is never guessed: Pakistan's +05:00 fits dramas, not a US show.

use serde::{Deserialize, Serialize};

use super::dates::{Zone, timezone_name};
use crate::{channels::Channels, store::Mapping};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    #[default]
    Drama,
    Podcast,
}

/// Clip words on podcast channels. Highlights, Trailer and Shorts (so `#shorts` too) are in the
/// default excludes already.
pub const PODCAST_EXCLUDES: &[&str] = &["Clip", "Best of"];
/// Full episodes run an hour or more, clips a few minutes.
pub const PODCAST_MIN_DURATION_SECS: i64 = 20 * 60;
/// A podcast with thousands of back episodes releases only the newest of them.
pub const PODCAST_BACKFILL_LIMIT: i64 = 10;

/// A channel's zone as a mapping stores it: a time zone or a fixed offset, never both. Neither is
/// Pakistan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ZoneSetting {
    pub timezone: Option<String>,
    pub utc_offset_mins: Option<i64>,
}

impl ZoneSetting {
    pub fn zone(&self) -> Result<Zone, String> {
        Zone::new(self.timezone.as_deref(), self.utc_offset_mins)
    }
}

/// The zone asked for (a time zone over an offset), else the `[channels]` entry of the first
/// listed channel. `None` when neither says.
pub fn channel_zone(
    asked_timezone: Option<&str>,
    asked_offset: Option<i64>,
    channels: &Channels,
    channel_ids: &[String],
) -> Result<Option<ZoneSetting>, String> {
    if let Some(name) = asked_timezone {
        return Ok(Some(ZoneSetting {
            timezone: Some(timezone_name(name)?),
            utc_offset_mins: None,
        }));
    }
    if asked_offset.is_some() {
        return Ok(Some(ZoneSetting {
            timezone: None,
            utc_offset_mins: asked_offset,
        }));
    }
    Ok(channel_ids
        .iter()
        .find_map(|id| channels.get(id))
        .map(|c| match &c.timezone {
            Some(name) => ZoneSetting {
                timezone: Some(name.clone()),
                utc_offset_mins: None,
            },
            None => ZoneSetting {
                timezone: None,
                utc_offset_mins: Some(c.utc_offset_mins),
            },
        }))
}

/// The zone for a podcast, as [`channel_zone`] finds it. `Err` names the channel, for a message
/// asking the user to pick one.
pub fn podcast_zone(
    asked_timezone: Option<&str>,
    asked_offset: Option<i64>,
    channels: &Channels,
    channel_ids: &[String],
) -> Result<ZoneSetting, String> {
    channel_zone(asked_timezone, asked_offset, channels, channel_ids)?.ok_or_else(|| {
        let which = channel_ids
            .first()
            .map_or_else(|| "no channel".to_owned(), |id| format!("channel {id}"));
        format!(
            "a podcast needs its channel's time zone: {which} is not in [channels], so set \
             timezone (an IANA name like America/Chicago or Europe/London) or utc_offset_mins"
        )
    })
}

/// Sets the podcast rules on `m`, keeping any value the user already gave. `zone` comes from
/// [`podcast_zone`].
pub fn apply_podcast(m: &mut Mapping, zone: ZoneSetting) {
    m.title_number = true;
    m.backfill_limit.get_or_insert(PODCAST_BACKFILL_LIMIT);
    m.min_duration_secs.get_or_insert(PODCAST_MIN_DURATION_SECS);
    m.timezone = zone.timezone;
    m.utc_offset_mins = zone.utc_offset_mins;
    for word in PODCAST_EXCLUDES {
        if !m.excludes.iter().any(|e| e.eq_ignore_ascii_case(word)) {
            m.excludes.push((*word).to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::channels::ChannelInfo;

    const LISTED: &str = "UCzQUP1qoWDoEbmsQxvdjxgQ";
    const ZONED: &str = "UCGq-a57w-aPwyi3pW7XLiHw";
    const OTHER: &str = "UC4JCksJF76g_MdzPVBJoC3Q";

    fn channels() -> Channels {
        let us = |timezone: Option<&str>| ChannelInfo {
            utc_offset_mins: -360,
            timezone: timezone.map(str::to_owned),
            country: "United States".to_owned(),
            language: "English".to_owned(),
            ..ChannelInfo::default()
        };
        Channels::check(
            "channels",
            BTreeMap::from([
                (LISTED.to_owned(), us(None)),
                (ZONED.to_owned(), us(Some("america/chicago"))),
            ]),
        )
        .unwrap()
    }

    fn offset(mins: i64) -> ZoneSetting {
        ZoneSetting {
            timezone: None,
            utc_offset_mins: Some(mins),
        }
    }

    fn named(name: &str) -> ZoneSetting {
        ZoneSetting {
            timezone: Some(name.to_owned()),
            utc_offset_mins: None,
        }
    }

    #[test]
    fn the_zone_comes_from_the_request_then_the_config_and_is_never_guessed() {
        let ids = |id: &str| vec![id.to_owned()];
        let c = channels();
        assert_eq!(
            podcast_zone(None, Some(60), &c, &ids(LISTED)),
            Ok(offset(60))
        );
        assert_eq!(podcast_zone(None, None, &c, &ids(LISTED)), Ok(offset(-360)));
        // The config's time zone wins over its offset, spelled as the database spells it.
        assert_eq!(
            podcast_zone(None, None, &c, &ids(ZONED)),
            Ok(named("America/Chicago"))
        );
        // An asked time zone wins over an asked offset and over the config.
        assert_eq!(
            podcast_zone(Some("Europe/London"), Some(60), &c, &ids(ZONED)),
            Ok(named("Europe/London"))
        );
        assert!(podcast_zone(Some("Mars/Olympus"), None, &c, &ids(ZONED)).is_err());
        let err = podcast_zone(None, None, &c, &ids(OTHER)).unwrap_err();
        assert!(
            err.contains(OTHER) && err.contains("timezone") && err.contains("utc_offset_mins"),
            "{err}"
        );
        assert_eq!(channel_zone(None, None, &c, &ids(OTHER)), Ok(None));
    }
}
