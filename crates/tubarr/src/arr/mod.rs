//! Reading the titles tagged for Tubarr from Sonarr and Radarr (their v3 APIs, checked against
//! Sonarr 4.0.20 and Radarr 6.4.4), and keeping the mappings table in step with them.

pub mod connections;
pub mod push;
mod radarr;
pub mod root_folders;
mod sonarr;
mod sync;

use std::{fmt, time::Duration};

use reqwest::{Method, StatusCode, header::HeaderValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use url::Url;

use crate::config::{ArrConnection, Secret};

pub use connections::ArrConnections;
pub use push::{AutoPush, FeedLinks, Outcome, PushTarget, Pushed};
pub use radarr::{Movie, MovieLookup, Radarr};
pub use root_folders::{RootFolderDefaults, RouteReport, Routing};
pub use sonarr::{Episode, EpisodeFile, Series, SeriesLookup, SeriesType, Sonarr};
pub use sync::{ArrSync, SyncError, SyncReport, sync_radarr, sync_sonarr};

/// A whole series list can be large, but an arr that takes longer than this is stuck.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Which arr a call went to, for messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum App {
    Sonarr,
    Radarr,
}

impl App {
    /// The arr named by its config section (`sonarr`, `radarr`), as the admin API paths name it.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "sonarr" => Some(Self::Sonarr),
            "radarr" => Some(Self::Radarr),
            _ => None,
        }
    }

    /// The config section that holds this arr's connection.
    pub fn config_key(self) -> &'static str {
        match self {
            Self::Sonarr => "sonarr",
            Self::Radarr => "radarr",
        }
    }

    fn titles(self) -> &'static str {
        match self {
            Self::Sonarr => "series",
            Self::Radarr => "films",
        }
    }
}

impl fmt::Display for App {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Sonarr => "Sonarr",
            Self::Radarr => "Radarr",
        })
    }
}

/// Why a call to Sonarr or Radarr failed. No message ever holds the API key.
#[derive(Debug, thiserror::Error)]
pub enum ArrError {
    #[error("cannot set up the HTTP client for {app}: {source}")]
    Client { app: App, source: reqwest::Error },
    #[error("the {app} API key is not a valid header value")]
    BadKey { app: App },
    #[error("{app} at {url} cannot be reached: {source}")]
    Unreachable {
        app: App,
        url: String,
        source: reqwest::Error,
    },
    #[error(
        "{app} refused the API key (HTTP {status}): copy it again from {app}, Settings > General"
    )]
    Unauthorized { app: App, status: StatusCode },
    /// HTTP 400, with the reasons the arr gave (its validation messages).
    #[error("{app} refused {path} (HTTP 400): {message}")]
    BadRequest {
        app: App,
        path: String,
        message: String,
    },
    #[error(
        "{app} has no enabled Usenet download client named \"{name}\": add Tubarr as a SABnzbd client under that name, or set arr.download_client"
    )]
    ClientMissing { app: App, name: String },
    #[error("{app} answered HTTP {status} to {path}")]
    Status {
        app: App,
        path: String,
        status: StatusCode,
    },
    #[error("{app} sent an answer to {path} that Tubarr cannot read: {source}")]
    Decode {
        app: App,
        path: String,
        source: serde_json::Error,
    },
    #[error(
        "{app} has no tag \"{tag}\": create it in {app} (Settings, Tags) and put it on the {} Tubarr should serve",
        .app.titles()
    )]
    TagMissing { app: App, tag: String },
}

/// A tag as `GET /api/v3/tag` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Tag {
    pub id: i64,
    pub label: Option<String>,
}

/// A language as the arrs report it, e.g. `{"id": 26, "name": "Hindi"}` in Radarr.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Language {
    pub id: i64,
    pub name: Option<String>,
}

/// A root folder as `GET /api/v3/rootfolder` lists it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RootFolder {
    pub id: i64,
    pub path: String,
    #[serde(default)]
    pub accessible: bool,
}

/// Another name a title goes by, as the lookups list them (`alternateTitles`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AlternateTitle {
    #[serde(default)]
    pub title: Option<String>,
}

/// An HTTP client with the arr timeouts, for components that build their arr clients per use.
pub(crate) fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    // Direct even when the process environment names a proxy: see [`crate::network`].
    reqwest::Client::builder()
        .no_proxy()
        .timeout(REQUEST_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
}

/// The HTTP part both arrs share: base URL, API key header, timeouts, error mapping.
#[derive(Debug, Clone)]
struct ArrClient {
    app: App,
    base: Url,
    api_key: Secret,
    http: reqwest::Client,
}

impl ArrClient {
    fn new(app: App, connection: &ArrConnection) -> Result<Self, ArrError> {
        let http = http_client().map_err(|source| ArrError::Client { app, source })?;
        Ok(Self::with_http(app, connection, http))
    }

    fn with_http(app: App, connection: &ArrConnection, http: reqwest::Client) -> Self {
        Self {
            app,
            base: connection.url.clone(),
            api_key: connection.api_key.clone(),
            http,
        }
    }

    /// GETs `path` (relative to the base URL, so a URL base path is kept) and decodes the JSON.
    async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ArrError> {
        let body = self.send(Method::GET, path, query, None::<&()>).await?;
        serde_json::from_slice(&body).map_err(|source| ArrError::Decode {
            app: self.app,
            path: path.to_owned(),
            source,
        })
    }

    /// PUTs `body` as JSON to `path`. The answer's body is not read.
    async fn put<B: Serialize>(&self, path: &str, body: &B) -> Result<(), ArrError> {
        self.send(Method::PUT, path, &[], Some(body))
            .await
            .map(drop)
    }

    /// POSTs `body` as JSON to `path` and decodes the JSON answer.
    async fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ArrError> {
        let answer = self.send(Method::POST, path, &[], Some(body)).await?;
        serde_json::from_slice(&answer).map_err(|source| ArrError::Decode {
            app: self.app,
            path: path.to_owned(),
            source,
        })
    }

    async fn send<B: Serialize>(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&B>,
    ) -> Result<bytes::Bytes, ArrError> {
        let app = self.app;
        let mut url = self.base.join(path).map_err(|_| ArrError::Status {
            app,
            path: path.to_owned(),
            status: StatusCode::BAD_REQUEST,
        })?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let mut key =
            HeaderValue::from_str(self.api_key.expose()).map_err(|_| ArrError::BadKey { app })?;
        // Keeps the value out of reqwest's own Debug output.
        key.set_sensitive(true);
        let unreachable = |source: reqwest::Error| ArrError::Unreachable {
            app,
            url: self.base.to_string(),
            source: source.without_url(),
        };
        let mut request = self.http.request(method, url).header("X-Api-Key", key);
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(unreachable)?;
        let status = response.status();
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(ArrError::Unauthorized { app, status });
        }
        if status == StatusCode::BAD_REQUEST {
            let body = response.bytes().await.unwrap_or_default();
            return Err(ArrError::BadRequest {
                app,
                path: path.to_owned(),
                message: bad_request_message(&body),
            });
        }
        if !status.is_success() {
            return Err(ArrError::Status {
                app,
                path: path.to_owned(),
                status,
            });
        }
        response.bytes().await.map_err(unreachable)
    }

    /// Every root folder the arr has.
    async fn root_folders(&self) -> Result<Vec<RootFolder>, ArrError> {
        self.get("api/v3/rootfolder", &[]).await
    }

    /// The id of the tag labelled `label`, compared without case (the arrs store labels in lower
    /// case).
    async fn tag_id(&self, label: &str) -> Result<i64, ArrError> {
        let tags: Vec<Tag> = self.get("api/v3/tag", &[]).await?;
        tags.into_iter()
            .find(|t| {
                t.label
                    .as_deref()
                    .is_some_and(|l| l.eq_ignore_ascii_case(label))
            })
            .map(|t| t.id)
            .ok_or_else(|| ArrError::TagMissing {
                app: self.app,
                tag: label.to_owned(),
            })
    }
}

/// The messages of a 400 answer. The arrs send their validation failures as a list of
/// `{"propertyName", "errorMessage"}`. Anything else is cut to a short line.
fn bad_request_message(body: &[u8]) -> String {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Failure {
        error_message: Option<String>,
    }
    if let Ok(list) = serde_json::from_slice::<Vec<Failure>>(body) {
        let messages: Vec<String> = list.into_iter().filter_map(|f| f.error_message).collect();
        if !messages.is_empty() {
            return messages.join("; ");
        }
    }
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.is_empty() {
        return "no reason given".to_owned();
    }
    text.chars().take(200).collect()
}

/// The arrs send `""` or leave out an IMDb id they do not know.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

/// Sonarr sends year 0 for a series without a first air date.
fn known_year(year: i64) -> Option<i64> {
    (year > 0).then_some(year)
}

fn language_name(language: Option<&Language>) -> String {
    language
        .and_then(|l| l.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "Unknown".to_owned())
}
