//! Sonarr: tagged series and their episodes.

use serde::Deserialize;

use super::{AlternateTitle, App, ArrClient, ArrError, Language, RootFolder};
use crate::config::ArrConnection;

/// How Sonarr numbers a series' episodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SeriesType {
    Standard,
    /// Episodes are identified by air date.
    Daily,
    Anime,
}

/// The fields of a `SeriesResource` Tubarr uses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Series {
    pub id: i64,
    pub title: String,
    /// 0 when the series has no first air date.
    #[serde(default)]
    pub year: i64,
    pub tvdb_id: i64,
    #[serde(default)]
    pub imdb_id: Option<String>,
    pub series_type: SeriesType,
    #[serde(default)]
    pub original_language: Option<Language>,
    pub monitored: bool,
    #[serde(default)]
    pub tags: Vec<i64>,
    /// The root folder the series lives under, as the arr works it out from its path.
    #[serde(default)]
    pub root_folder_path: Option<String>,
    /// Other names of the series (scene names and the like), seeded as arr aliases on sync.
    #[serde(default)]
    pub alternate_titles: Vec<AlternateTitle>,
}

/// The fields of an `EpisodeResource` Tubarr uses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    pub id: i64,
    pub series_id: i64,
    pub season_number: i64,
    pub episode_number: i64,
    #[serde(default)]
    pub title: Option<String>,
    /// Local air date as `YYYY-MM-DD`, what daily shows are matched on.
    #[serde(default)]
    pub air_date: Option<String>,
    #[serde(default)]
    pub air_date_utc: Option<String>,
    pub monitored: bool,
    pub has_file: bool,
}

/// The fields of an `EpisodeFileResource` Tubarr uses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeFile {
    pub id: i64,
    pub series_id: i64,
    /// The full path as Sonarr sees it.
    #[serde(default)]
    pub path: Option<String>,
}

/// A series as `GET /api/v3/series/lookup` returns it: TVDB data through Sonarr's metadata
/// service. `id` is set (non-zero) only for a series already in Sonarr.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesLookup {
    #[serde(default)]
    pub id: Option<i64>,
    pub title: String,
    #[serde(default)]
    pub year: i64,
    #[serde(default)]
    pub tvdb_id: i64,
    #[serde(default)]
    pub network: Option<String>,
    /// `continuing`, `ended`, `upcoming`.
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub original_language: Option<Language>,
    #[serde(default)]
    pub alternate_titles: Vec<AlternateTitle>,
}

/// A typed Sonarr v3 API client.
#[derive(Debug, Clone)]
pub struct Sonarr {
    client: ArrClient,
}

impl Sonarr {
    pub fn new(connection: &ArrConnection) -> Result<Self, ArrError> {
        Ok(Self {
            client: ArrClient::new(App::Sonarr, connection)?,
        })
    }

    /// A client on an existing HTTP client, for callers that share one.
    pub fn with_http(connection: &ArrConnection, http: reqwest::Client) -> Self {
        Self {
            client: ArrClient::with_http(App::Sonarr, connection, http),
        }
    }

    /// Sonarr's search for new series by name. Read-only: nothing is added.
    pub async fn lookup(&self, term: &str) -> Result<Vec<SeriesLookup>, ArrError> {
        self.client
            .get("api/v3/series/lookup", &[("term", term.to_owned())])
            .await
    }

    /// The id of the tag labelled `label`. [`ArrError::TagMissing`] when Sonarr has none.
    pub async fn tag_id(&self, label: &str) -> Result<i64, ArrError> {
        self.client.tag_id(label).await
    }

    /// Every series. Sonarr has no tag filter on this call.
    pub async fn series(&self) -> Result<Vec<Series>, ArrError> {
        self.client.get("api/v3/series", &[]).await
    }

    /// The series carrying the tag labelled `label`.
    pub async fn tagged_series(&self, label: &str) -> Result<Vec<Series>, ArrError> {
        let tag = self.tag_id(label).await?;
        let mut all = self.series().await?;
        all.retain(|s| s.tags.contains(&tag));
        Ok(all)
    }

    /// Every episode of series `series_id` (Sonarr's id, not the TVDB id).
    pub async fn episodes(&self, series_id: i64) -> Result<Vec<Episode>, ArrError> {
        self.client
            .get("api/v3/episode", &[("seriesId", series_id.to_string())])
            .await
    }

    /// Every episode file of series `series_id` that Sonarr has imported.
    pub async fn episode_files(&self, series_id: i64) -> Result<Vec<EpisodeFile>, ArrError> {
        self.client
            .get("api/v3/episodefile", &[("seriesId", series_id.to_string())])
            .await
    }

    /// Monitored episodes of series `series_id` that have no file yet: what Tubarr should look
    /// for. Whether the series itself is monitored is on [`Series::monitored`].
    pub async fn wanted_episodes(&self, series_id: i64) -> Result<Vec<Episode>, ArrError> {
        let mut episodes = self.episodes(series_id).await?;
        episodes.retain(|e| e.monitored && !e.has_file);
        Ok(episodes)
    }

    /// Every root folder Sonarr has. Tubarr never adds one.
    pub async fn root_folders(&self) -> Result<Vec<RootFolder>, ArrError> {
        self.client.root_folders().await
    }

    /// Moves series `id` and its files to the root folder `path` (one Sonarr lists), with the
    /// editor call Sonarr's own UI uses: Sonarr builds the new folder name and moves in the
    /// background.
    pub async fn move_to_root_folder(&self, id: i64, path: &str) -> Result<(), ArrError> {
        let body =
            serde_json::json!({ "seriesIds": [id], "rootFolderPath": path, "moveFiles": true });
        self.client.put("api/v3/series/editor", &body).await
    }
}
