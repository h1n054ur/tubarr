//! Radarr: tagged films.

use serde::Deserialize;

use super::{AlternateTitle, App, ArrClient, ArrError, Language, RootFolder};
use crate::config::ArrConnection;

/// The fields of a `MovieResource` Tubarr uses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Movie {
    pub id: i64,
    pub title: String,
    #[serde(default)]
    pub year: i64,
    pub tmdb_id: i64,
    #[serde(default)]
    pub imdb_id: Option<String>,
    #[serde(default)]
    pub original_language: Option<Language>,
    pub monitored: bool,
    pub has_file: bool,
    #[serde(default)]
    pub tags: Vec<i64>,
    /// The root folder the film lives under, as the arr works it out from its path.
    #[serde(default)]
    pub root_folder_path: Option<String>,
}

/// A film as `GET /api/v3/movie/lookup` returns it: TMDB data through Radarr's metadata
/// service. `id` is set (non-zero) only for a film already in Radarr.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MovieLookup {
    #[serde(default)]
    pub id: Option<i64>,
    pub title: String,
    #[serde(default)]
    pub year: i64,
    #[serde(default)]
    pub tmdb_id: i64,
    #[serde(default)]
    pub original_title: Option<String>,
    /// TMDB genre names; telefilms carry "TV Movie".
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub runtime: Option<i64>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub original_language: Option<Language>,
    #[serde(default)]
    pub alternate_titles: Vec<AlternateTitle>,
}

/// A typed Radarr v3 API client.
#[derive(Debug, Clone)]
pub struct Radarr {
    client: ArrClient,
}

impl Radarr {
    pub fn new(connection: &ArrConnection) -> Result<Self, ArrError> {
        Ok(Self {
            client: ArrClient::new(App::Radarr, connection)?,
        })
    }

    /// A client on an existing HTTP client, for callers that share one.
    pub fn with_http(connection: &ArrConnection, http: reqwest::Client) -> Self {
        Self {
            client: ArrClient::with_http(App::Radarr, connection, http),
        }
    }

    /// Radarr's search for new films by name. Read-only: nothing is added.
    pub async fn lookup(&self, term: &str) -> Result<Vec<MovieLookup>, ArrError> {
        self.client
            .get("api/v3/movie/lookup", &[("term", term.to_owned())])
            .await
    }

    /// The id of the tag labelled `label`. [`ArrError::TagMissing`] when Radarr has none.
    pub async fn tag_id(&self, label: &str) -> Result<i64, ArrError> {
        self.client.tag_id(label).await
    }

    /// Every film. Radarr has no tag filter on this call.
    pub async fn movies(&self) -> Result<Vec<Movie>, ArrError> {
        self.client.get("api/v3/movie", &[]).await
    }

    /// The films carrying the tag labelled `label`.
    pub async fn tagged_movies(&self, label: &str) -> Result<Vec<Movie>, ArrError> {
        let tag = self.tag_id(label).await?;
        let mut all = self.movies().await?;
        all.retain(|m| m.tags.contains(&tag));
        Ok(all)
    }

    /// Tagged, monitored films that have no file yet: what Tubarr should look for.
    pub async fn wanted_movies(&self, label: &str) -> Result<Vec<Movie>, ArrError> {
        let mut movies = self.tagged_movies(label).await?;
        movies.retain(|m| m.monitored && !m.has_file);
        Ok(movies)
    }

    /// Every root folder Radarr has. Tubarr never adds one.
    pub async fn root_folders(&self) -> Result<Vec<RootFolder>, ArrError> {
        self.client.root_folders().await
    }

    /// Moves film `id` and its files to the root folder `path` (one Radarr lists), with the
    /// editor call Radarr's own UI uses: Radarr builds the new folder name and moves in the
    /// background.
    pub async fn move_to_root_folder(&self, id: i64, path: &str) -> Result<(), ArrError> {
        let body =
            serde_json::json!({ "movieIds": [id], "rootFolderPath": path, "moveFiles": true });
        self.client.put("api/v3/movie/editor", &body).await
    }

    /// Film `id` (Radarr's id, not the TMDB id).
    pub async fn movie(&self, id: i64) -> Result<Movie, ArrError> {
        self.client.get(&format!("api/v3/movie/{id}"), &[]).await
    }
}
