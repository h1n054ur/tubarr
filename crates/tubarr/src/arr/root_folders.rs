//! Root folder routing: each title lives under the Sonarr or Radarr root folder of its library.
//!
//! Jellyfin libraries are folders, so Pakistani dramas, Indian dramas and Indian films only stay
//! out of the English Shows and Movies when Sonarr and Radarr keep them under their own root
//! folders. The folder a mapping wants comes from its override (admin PATCH, `none` turns routing
//! off), else its first channel in `[channels]`, else `[root_folders]` by language. After each
//! sync Tubarr asks the arr to move a title whose root folder differs, through the editor
//! endpoints the arrs' own UI uses (`PUT /api/v3/series/editor`, `PUT /api/v3/movie/editor` with
//! `moveFiles`, Sonarr 4.0.20 and Radarr 6.4.4). Tubarr never creates root folders, and it asks
//! once per wanted folder: a move the user undid in the arr is left alone until the wanted folder
//! changes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{ArrConnections, ArrError, Radarr, RootFolder, Sonarr};
use crate::{
    channels::{Channels, is_root_folder},
    config::{Config, ConfigError},
    store::{Db, MappingKind, RootFolderRow, StoreError},
};

/// The override value that turns routing off for one mapping.
pub const NO_ROUTING: &str = "none";

/// `[root_folders]`: root folders by language, for titles whose channels set none.
///
/// ```toml
/// [root_folders.shows]
/// Urdu = "/data/media/pk-dramas"
/// [root_folders.films]
/// Hindi = "/data/media/in-films"
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RootFolderDefaults {
    /// Language name (any case) to root folder, for shows and daily shows.
    pub shows: BTreeMap<String, String>,
    /// Language name (any case) to root folder, for films.
    pub films: BTreeMap<String, String>,
}

impl RootFolderDefaults {
    pub(crate) fn check(self) -> Result<Self, ConfigError> {
        for (key, map) in [
            ("root_folders.shows", &self.shows),
            ("root_folders.films", &self.films),
        ] {
            for (language, folder) in map {
                if language.trim().is_empty() || !is_root_folder(folder) {
                    return Err(ConfigError::Invalid {
                        key,
                        reason: format!(
                            "{language:?} = {folder:?}: needs a language name and an absolute path"
                        ),
                    });
                }
            }
        }
        Ok(self)
    }

    fn get(&self, language: &str, film: bool) -> Option<&str> {
        let map = if film { &self.films } else { &self.shows };
        map.iter()
            .find(|(l, _)| l.trim().eq_ignore_ascii_case(language.trim()))
            .map(|(_, f)| f.as_str())
    }
}

/// The root folder a mapping should live in, or `None` when nothing routes it.
pub fn wanted(
    kind: MappingKind,
    language: &str,
    channel_ids: &[String],
    override_folder: Option<&str>,
    channels: &Channels,
    defaults: &RootFolderDefaults,
) -> Option<String> {
    let film = kind == MappingKind::Film;
    match override_folder {
        Some(NO_ROUTING) => None,
        Some(folder) => Some(folder.to_owned()),
        None => channel_ids
            .first()
            .and_then(|c| channels.root_folder(c, film))
            .or_else(|| defaults.get(language, film))
            .map(str::to_owned),
    }
}

/// Checks an override from the admin API: `none`, or an absolute path.
pub fn check_override(value: &str) -> Result<(), String> {
    if value == NO_ROUTING || is_root_folder(value) {
        Ok(())
    } else {
        Err(format!(
            "root_folder must be an absolute path or \"{NO_ROUTING}\", not {value:?}"
        ))
    }
}

/// The arrs list root folders with or without a trailing slash.
pub fn same_folder(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// What one routing pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RouteReport {
    pub moved: usize,
    /// Wanted folders the arr does not have.
    pub missing: usize,
    pub failed: usize,
}

/// Which arrs to route, and the config the wanted folders come from.
#[derive(Debug, Clone, Copy)]
pub struct Routing<'a> {
    pub sonarr: Option<&'a Sonarr>,
    pub radarr: Option<&'a Radarr>,
    pub channels: &'a Channels,
    pub defaults: &'a RootFolderDefaults,
}

impl Routing<'_> {
    /// Moves every active synced mapping (or only `id`) whose wanted root folder differs from the
    /// one the arr reported at the last sync and has not been asked for yet.
    pub async fn run(&self, db: &Db, id: Option<i64>) -> Result<RouteReport, StoreError> {
        let mut report = RouteReport::default();
        let mut sonarr_folders = None;
        let mut radarr_folders = None;
        for row in db.root_folder_rows(id).await? {
            let Some(want) = self.wanted(&row) else {
                continue;
            };
            let (Some(arr_id), Some(current)) = (row.arr_id, row.current.as_deref()) else {
                continue;
            };
            if same_folder(&want, current)
                || row
                    .attempted
                    .as_deref()
                    .is_some_and(|a| same_folder(a, &want))
            {
                continue;
            }
            let film = row.kind == MappingKind::Film;
            let folders = if film {
                let Some(radarr) = self.radarr else { continue };
                folders_of(&mut radarr_folders, radarr.root_folders()).await
            } else {
                let Some(sonarr) = self.sonarr else { continue };
                folders_of(&mut sonarr_folders, sonarr.root_folders()).await
            };
            let folders = match folders {
                Ok(f) => f,
                Err(e) => {
                    tracing::warn!(error = %e, "cannot read the root folders, title not moved");
                    continue;
                }
            };
            let Some(target) = folders.iter().find(|f| same_folder(&f.path, &want)) else {
                tracing::warn!(
                    mapping = row.id,
                    title = %row.title,
                    folder = %want,
                    "root folder missing in the arr: add it there (Settings, Media Management), \
                     Tubarr never creates one"
                );
                db.record_root_folder_result(row.id, None, "missing", None)
                    .await?;
                report.missing += 1;
                continue;
            };
            let result = match (film, self.radarr, self.sonarr) {
                (true, Some(radarr), _) => radarr.move_to_root_folder(arr_id, &target.path).await,
                (false, _, Some(sonarr)) => sonarr.move_to_root_folder(arr_id, &target.path).await,
                _ => continue,
            };
            match result {
                Ok(()) => {
                    tracing::info!(mapping = row.id, title = %row.title, from = %current, to = %target.path, "moved to its root folder");
                    db.record_root_folder_result(row.id, Some(&want), "moved", Some(&target.path))
                        .await?;
                    report.moved += 1;
                }
                // Nothing reached the arr, so the next round tries again.
                Err(e @ ArrError::Unreachable { .. }) => {
                    tracing::warn!(mapping = row.id, error = %e, "root folder move not sent");
                    report.failed += 1;
                }
                Err(e) => {
                    tracing::warn!(mapping = row.id, title = %row.title, error = %e, "root folder move refused");
                    let result = format!("failed: {e}");
                    db.record_root_folder_result(row.id, Some(&want), &result, None)
                        .await?;
                    report.failed += 1;
                }
            }
        }
        Ok(report)
    }

    /// The wanted folder of a stored row.
    pub fn wanted(&self, row: &RootFolderRow) -> Option<String> {
        wanted(
            row.kind,
            &row.language,
            &row.channel_ids,
            row.override_folder.as_deref(),
            self.channels,
            self.defaults,
        )
    }
}

/// Moves only `id`, with clients for the arrs connected now. For the admin PATCH.
pub async fn route_one(
    config: &Config,
    arrs: &ArrConnections,
    db: &Db,
    id: i64,
) -> Result<RouteReport, RouteError> {
    let in_use = arrs.current();
    let sonarr = in_use.sonarr.as_ref().map(Sonarr::new).transpose()?;
    let radarr = in_use.radarr.as_ref().map(Radarr::new).transpose()?;
    let routing = Routing {
        sonarr: sonarr.as_ref(),
        radarr: radarr.as_ref(),
        channels: &config.channels,
        defaults: &config.root_folders,
    };
    Ok(routing.run(db, Some(id)).await?)
}

#[derive(Debug, thiserror::Error)]
pub enum RouteError {
    #[error(transparent)]
    Arr(#[from] ArrError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Reads an arr's root folders once per pass, on first need.
async fn folders_of(
    cache: &mut Option<Vec<RootFolder>>,
    fetch: impl Future<Output = Result<Vec<RootFolder>, ArrError>>,
) -> Result<&[RootFolder], ArrError> {
    if cache.is_none() {
        *cache = Some(fetch.await?);
    }
    Ok(cache.as_deref().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::{ChannelInfo, from_file};

    const ARY: &str = "UC4JCksJF76g_MdzPVBJoC3Q";

    fn channels() -> Channels {
        let info = ChannelInfo {
            show_root_folder: Some("/data/media/pk-dramas".to_owned()),
            ..ChannelInfo::default()
        };
        from_file(BTreeMap::from([(ARY.to_owned(), info)]), BTreeMap::new()).unwrap()
    }

    fn defaults() -> RootFolderDefaults {
        RootFolderDefaults {
            shows: BTreeMap::from([("hindi".to_owned(), "/data/media/in-dramas".to_owned())]),
            films: BTreeMap::from([("Hindi".to_owned(), "/data/media/in-films".to_owned())]),
        }
    }

    #[test]
    fn override_then_channel_then_language() {
        let (c, d) = (channels(), defaults());
        let ary = [ARY.to_owned()];
        let w = |kind, lang, ch: &[String], o| wanted(kind, lang, ch, o, &c, &d);
        assert_eq!(
            w(MappingKind::Show, "Urdu", &ary, None).as_deref(),
            Some("/data/media/pk-dramas")
        );
        assert_eq!(
            w(MappingKind::Show, "Urdu", &ary, Some("/x")).as_deref(),
            Some("/x")
        );
        assert_eq!(w(MappingKind::Show, "Urdu", &ary, Some(NO_ROUTING)), None);
        // The channel sets only a show folder, so its films fall back to the language.
        assert_eq!(
            w(MappingKind::Film, "Hindi", &ary, None).as_deref(),
            Some("/data/media/in-films")
        );
        assert_eq!(
            w(MappingKind::Daily, "HINDI", &[], None).as_deref(),
            Some("/data/media/in-dramas")
        );
        assert_eq!(w(MappingKind::Film, "English", &[], None), None);
    }

    #[test]
    fn overrides_and_defaults_are_checked() {
        assert!(check_override("none").is_ok());
        assert!(check_override("/data/media/x/").is_ok());
        assert!(check_override("media/x").is_err());
        let bad = RootFolderDefaults {
            films: BTreeMap::from([("Hindi".to_owned(), "in-films".to_owned())]),
            ..RootFolderDefaults::default()
        };
        let err = bad.check().unwrap_err().to_string();
        assert!(err.contains("root_folders.films"), "{err}");
        assert!(same_folder("/data/media/tv/", "/data/media/tv"));
    }
}
