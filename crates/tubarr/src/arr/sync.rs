//! The sync job: tagged titles from Sonarr and Radarr become mappings, at start and on an interval.

use std::{sync::Arc, time::Duration};

use tokio::{sync::Notify, time::MissedTickBehavior};

use super::{
    ArrConnections, ArrError, Radarr, RootFolderDefaults, Routing, SeriesType, Sonarr, http_client,
    known_year, language_name, non_empty,
};
use crate::{
    channels::Channels,
    config::Config,
    metrics::METRICS,
    store::{ArrEntry, ArrSource, Db, MappingKind, StoreError, SyncCounts},
};

/// Why one sync failed. Nothing in the database changes when the arr could not be read.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Arr(#[from] ArrError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What one sync of one arr found and changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncReport {
    /// Tagged titles the arr reported.
    pub tagged: usize,
    pub counts: SyncCounts,
}

/// Reads the series tagged `tag` from Sonarr into the mappings table.
pub async fn sync_sonarr(db: &Db, sonarr: &Sonarr, tag: &str) -> Result<SyncReport, SyncError> {
    let series = sonarr.tagged_series(tag).await?;
    let mut entries = Vec::with_capacity(series.len());
    let mut folders = Vec::with_capacity(series.len());
    for s in series {
        if s.tvdb_id <= 0 {
            tracing::warn!(title = %s.title, "skipping Sonarr series without a TVDB id");
            continue;
        }
        folders.push((s.tvdb_id, s.root_folder_path));
        entries.push(ArrEntry {
            kind: match s.series_type {
                SeriesType::Daily => MappingKind::Daily,
                SeriesType::Standard | SeriesType::Anime => MappingKind::Show,
            },
            key: s.tvdb_id,
            arr_id: s.id,
            language: language_name(s.original_language.as_ref()),
            aliases: s
                .alternate_titles
                .into_iter()
                .filter_map(|a| a.title)
                .filter(|a| a.trim() != s.title.trim())
                .collect(),
            title: s.title,
            year: known_year(s.year),
            imdb_id: non_empty(s.imdb_id),
        });
    }
    let counts = db.sync_arr_mappings(ArrSource::Sonarr, &entries).await?;
    db.set_current_root_folders(ArrSource::Sonarr, &folders)
        .await?;
    Ok(SyncReport {
        tagged: entries.len(),
        counts,
    })
}

/// Reads the films tagged `tag` from Radarr into the mappings table.
pub async fn sync_radarr(db: &Db, radarr: &Radarr, tag: &str) -> Result<SyncReport, SyncError> {
    let movies = radarr.tagged_movies(tag).await?;
    let mut entries = Vec::with_capacity(movies.len());
    let mut folders = Vec::with_capacity(movies.len());
    for m in movies {
        if m.tmdb_id <= 0 {
            tracing::warn!(title = %m.title, "skipping Radarr film without a TMDB id");
            continue;
        }
        folders.push((m.tmdb_id, m.root_folder_path));
        entries.push(ArrEntry {
            kind: MappingKind::Film,
            key: m.tmdb_id,
            arr_id: m.id,
            language: language_name(m.original_language.as_ref()),
            title: m.title,
            year: known_year(m.year),
            imdb_id: non_empty(m.imdb_id),
            // Radarr's alternate titles are not seeded: the film matcher scores aliases by
            // similarity, and Radarr lists many loose translations.
            aliases: vec![],
        });
    }
    let counts = db.sync_arr_mappings(ArrSource::Radarr, &entries).await?;
    db.set_current_root_folders(ArrSource::Radarr, &folders)
        .await?;
    Ok(SyncReport {
        tagged: entries.len(),
        counts,
    })
}

/// The arrs in use and how often to read them.
#[derive(Debug, Clone)]
pub struct ArrSync {
    db: Db,
    arrs: ArrConnections,
    http: reqwest::Client,
    tag: String,
    interval: Duration,
    channels: Channels,
    root_folders: RootFolderDefaults,
    synced: Option<Arc<Notify>>,
}

impl ArrSync {
    /// Syncs whichever arrs `arrs` holds at each round. Fails only if the HTTP client cannot be
    /// built.
    pub fn new(config: &Config, arrs: ArrConnections, db: Db) -> Result<Self, reqwest::Error> {
        Ok(Self {
            db,
            arrs,
            http: http_client()?,
            tag: config.arr.tag.clone(),
            interval: config.arr.sync_interval,
            channels: config.channels.clone(),
            root_folders: config.root_folders.clone(),
            synced: None,
        })
    }

    /// The arrs of the config file only.
    pub fn from_config(config: &Config, db: Db) -> Result<Self, reqwest::Error> {
        Self::new(config, ArrConnections::from_config(config), db)
    }

    /// Notifies `synced` after every sync, so the matcher sees new titles and episodes.
    pub fn with_synced(mut self, synced: Arc<Notify>) -> Self {
        self.synced = Some(synced);
        self
    }

    /// Syncs every arr in use once. A failure is logged and the other arr still runs. Then shows
    /// still on the arr's English or Unknown take their channel's language, and titles of an arr
    /// that synced move to their root folder. Does nothing while no arr is connected.
    pub async fn sync_once(&self) {
        let in_use = self.arrs.current();
        if in_use.sonarr.is_none() && in_use.radarr.is_none() {
            return;
        }
        let sonarr = in_use
            .sonarr
            .as_ref()
            .map(|c| Sonarr::with_http(c, self.http.clone()));
        let radarr = in_use
            .radarr
            .as_ref()
            .map(|c| Radarr::with_http(c, self.http.clone()));
        let mut synced_sonarr = None;
        if let Some(s) = &sonarr {
            synced_sonarr = log("Sonarr", sync_sonarr(&self.db, s, &self.tag).await).then_some(s);
        }
        let mut synced_radarr = None;
        if let Some(r) = &radarr {
            synced_radarr = log("Radarr", sync_radarr(&self.db, r, &self.tag).await).then_some(r);
        }
        if let Err(e) = self.db.languages_from_channels(&self.channels, None).await {
            tracing::warn!(error = %e, "could not apply channel languages");
        }
        // Only after a good sync: the current root folders are fresh and the arr is up.
        if synced_sonarr.is_some() || synced_radarr.is_some() {
            let routing = Routing {
                sonarr: synced_sonarr,
                radarr: synced_radarr,
                channels: &self.channels,
                defaults: &self.root_folders,
            };
            match routing.run(&self.db, None).await {
                Ok(r) if r != Default::default() => tracing::info!(
                    moved = r.moved,
                    missing = r.missing,
                    failed = r.failed,
                    "root folder routing done"
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!(error = %e, "root folder routing failed"),
            }
        }
        if let Some(synced) = &self.synced {
            synced.notify_one();
        }
    }

    /// Syncs now, then every interval and whenever a connection is saved, changed or removed,
    /// forever.
    pub async fn run(self) {
        let mut changes = self.arrs.watch();
        let mut ticks = tokio::time::interval(self.interval);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticks.tick() => {}
                Ok(()) = changes.changed() => {
                    // A full interval from this sync, not from the last tick.
                    ticks.reset();
                }
            }
            self.sync_once().await;
        }
    }
}

/// Logs and counts one arr's sync. True when it worked.
fn log(app: &str, result: Result<SyncReport, SyncError>) -> bool {
    METRICS.arr_sync(app, result.is_ok());
    match result {
        Ok(report) => {
            tracing::info!(
                app,
                tagged = report.tagged,
                changed = report.counts.changed,
                deactivated = report.counts.deactivated,
                "arr sync done"
            );
            true
        }
        Err(e) => {
            tracing::warn!(app, error = %e, "arr sync failed, keeping the current mappings");
            false
        }
    }
}
