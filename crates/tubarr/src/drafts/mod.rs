//! The draft helper for shows missing from TVDB and films missing from TMDB.
//!
//! Sonarr only adds shows TVDB has and Radarr only films TMDB has. When a title is missing, Hani
//! adds it himself on tvdb.com or themoviedb.org. This module checks whether a title is there
//! (read-only searches, [`lookup`]) and drafts the entry from what the watcher already knows about
//! the official channel ([`show`], [`film`]): JSON for the UI plus a text block laid out in the
//! order of the site's add form. Tubarr never logs in to either site and never posts anything.
//!
//! Drafts use only stored data (`channel_videos` and `videos`), so drafting costs no YouTube
//! lookups. Without API keys the drafts still come out, marked as not checked.

pub mod film;
pub mod lookup;
pub mod names;
mod routes;
pub mod show;
mod store;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use url::Url;

pub use crate::channels::ChannelInfo;
use crate::{
    arr::{ArrConnections, Radarr, Sonarr},
    channels::Channels,
    config::{ConfigError, Secret, check_http_url, read_secret},
    store::{Db, StoreError},
};
use lookup::{Found, Query, Source, Tmdb, Tvdb};
pub use routes::router;
pub use store::StoredDraft;

/// The `[drafts]` config section as written.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DraftSettings {
    /// File holding the TVDB v4 project API key.
    pub tvdb_api_key_file: Option<PathBuf>,
    /// File holding the subscriber PIN, needed only with a user-supported TVDB key.
    pub tvdb_pin_file: Option<PathBuf>,
    /// File holding the TMDB API read access token (the long one, used as a bearer token).
    pub tmdb_read_token_file: Option<PathBuf>,
    /// API base URLs. Only tests point these elsewhere.
    pub tvdb_url: Url,
    pub tmdb_url: Url,
    /// Read only from files written before the map moved to `[channels]`.
    pub channels: BTreeMap<String, ChannelInfo>,
}

impl Default for DraftSettings {
    fn default() -> Self {
        Self {
            tvdb_api_key_file: None,
            tvdb_pin_file: None,
            tmdb_read_token_file: None,
            tvdb_url: Url::parse("https://api4.thetvdb.com/v4/").expect("valid TVDB URL"),
            tmdb_url: Url::parse("https://api.themoviedb.org/3/").expect("valid TMDB URL"),
            channels: BTreeMap::new(),
        }
    }
}

/// A key for one site: not configured, configured but unreadable, or ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    Unset,
    /// The file is named but cannot be read yet. Drafts go on unchecked.
    Unreadable(String),
    Ready(Secret),
}

impl Key {
    /// Reads `path` if set. A missing or empty file is not fatal: the keys are created later than
    /// the config, and a draft without a check is still useful.
    fn read(key: &'static str, path: Option<&Path>) -> Self {
        match path.map(|p| read_secret(key, p)) {
            None => Self::Unset,
            Some(Ok(secret)) => Self::Ready(secret),
            Some(Err(e)) => Self::Unreadable(e.to_string()),
        }
    }
}

/// The checked `[drafts]` settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drafts {
    pub tvdb_key: Key,
    pub tvdb_pin: Key,
    pub tmdb_token: Key,
    pub tvdb_url: Url,
    pub tmdb_url: Url,
}

impl Default for Drafts {
    fn default() -> Self {
        DraftSettings::default()
            .check()
            .expect("default draft settings are valid")
    }
}

impl DraftSettings {
    /// Checks everything but `channels`, which the config reads into [`Channels`].
    pub(crate) fn check(self) -> Result<Drafts, ConfigError> {
        let invalid = |key, reason: &str| ConfigError::Invalid {
            key,
            reason: reason.to_owned(),
        };
        if self.tvdb_pin_file.is_some() && self.tvdb_api_key_file.is_none() {
            return Err(invalid(
                "drafts.tvdb_pin_file",
                "only makes sense together with drafts.tvdb_api_key_file",
            ));
        }
        Ok(Drafts {
            tvdb_key: Key::read(
                "drafts.tvdb_api_key_file",
                self.tvdb_api_key_file.as_deref(),
            ),
            tvdb_pin: Key::read("drafts.tvdb_pin_file", self.tvdb_pin_file.as_deref()),
            tmdb_token: Key::read(
                "drafts.tmdb_read_token_file",
                self.tmdb_read_token_file.as_deref(),
            ),
            tvdb_url: check_http_url("drafts.tvdb_url", self.tvdb_url)?,
            tmdb_url: check_http_url("drafts.tmdb_url", self.tmdb_url)?,
        })
    }
}

/// Show (TVDB) or film (TMDB).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Show,
    Film,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Show => "show",
            Self::Film => "film",
        }
    }
}

/// How one source answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SourceState {
    Checked,
    NotChecked { reason: String },
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceReport {
    pub source: Source,
    #[serde(flatten)]
    pub state: SourceState,
    pub candidates: Vec<Found>,
}

/// The answer to "is it there?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// A candidate scores at or above [`FOUND`]: add it in Sonarr or Radarr instead.
    Found,
    /// Something close: look before adding, it may be the same title spelled differently.
    Possible,
    /// Nothing close: a draft is worth entering.
    ProbablyMissing,
    /// Neither Sonarr/Radarr nor a key for the site that decides.
    NotChecked,
    /// Nothing that decides answered.
    Unknown,
}

/// At or above this a candidate is taken as the same title.
pub const FOUND: f64 = 0.9;
/// At or above this a candidate is worth a look.
pub const POSSIBLE: f64 = 0.75;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    pub kind: Kind,
    pub title: String,
    pub original_title: Option<String>,
    pub year: Option<i64>,
    pub verdict: Verdict,
    /// The best candidate from the sources that decide: Sonarr or TVDB for shows, Radarr or TMDB
    /// films for films.
    pub best: Option<Found>,
    pub sources: Vec<SourceReport>,
}

impl CheckResult {
    /// One line for the text block.
    pub fn summary(&self) -> String {
        let site = match self.kind {
            Kind::Show => "TVDB",
            Kind::Film => "TMDB",
        };
        let best = |f: &Found| {
            format!(
                "best match {:.2}: {}{} {}",
                f.score,
                f.name,
                f.year.map(|y| format!(" ({y})")).unwrap_or_default(),
                f.url
            )
        };
        match self.verdict {
            Verdict::Found => format!(
                "Already on {site}, {}",
                self.best.as_ref().map(best).unwrap_or_default()
            ),
            Verdict::Possible => format!(
                "Maybe on {site}, check first: {}",
                self.best.as_ref().map(best).unwrap_or_default()
            ),
            Verdict::ProbablyMissing => format!(
                "Probably missing on {site}{}",
                self.best
                    .as_ref()
                    .map(|f| format!(" ({})", best(f)))
                    .unwrap_or_default()
            ),
            Verdict::NotChecked => {
                let arr = match self.kind {
                    Kind::Show => "Sonarr",
                    Kind::Film => "Radarr",
                };
                format!(
                    "Not checked against {site}: {arr} is not connected and no {site} key is set"
                )
            }
            Verdict::Unknown => format!("Not checked against {site}: the search failed"),
        }
    }
}

/// Why a draft could not be made.
#[derive(Debug, thiserror::Error)]
pub enum DraftError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    NotFound(String),
    /// The data a draft needs is not stored yet.
    #[error("{0}")]
    NotReady(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The helper behind the `/admin/api/drafts` endpoints.
#[derive(Debug)]
pub struct DraftHelper {
    db: Db,
    channels: Channels,
    arrs: ArrConnections,
    http: reqwest::Client,
    tvdb: Result<Tvdb, String>,
    tmdb: Result<Tmdb, String>,
}

fn unusable(site: &str, key: &Key) -> String {
    match key {
        Key::Unset => format!("no {site} key configured"),
        Key::Unreadable(why) => why.clone(),
        Key::Ready(_) => String::new(),
    }
}

impl DraftHelper {
    /// The Sonarr and Radarr `arrs` holds at the time of a check, when connected, are the main
    /// check. Fails only when the HTTP client cannot be built.
    pub fn new(
        db: Db,
        settings: &Drafts,
        channels: Channels,
        arrs: ArrConnections,
    ) -> Result<Self, reqwest::Error> {
        let client = lookup::client()?;
        let tvdb = match (&settings.tvdb_key, &settings.tvdb_pin) {
            (Key::Ready(key), Key::Ready(pin)) => Ok(Tvdb::new(
                client.clone(),
                settings.tvdb_url.clone(),
                key.clone(),
                Some(pin.clone()),
            )),
            (Key::Ready(key), Key::Unset) => Ok(Tvdb::new(
                client.clone(),
                settings.tvdb_url.clone(),
                key.clone(),
                None,
            )),
            (Key::Ready(_), pin) => Err(unusable("TVDB PIN", pin)),
            (key, _) => Err(unusable("TVDB", key)),
        };
        let tmdb = match &settings.tmdb_token {
            Key::Ready(token) => Ok(Tmdb::new(
                client.clone(),
                settings.tmdb_url.clone(),
                token.clone(),
            )),
            key => Err(unusable("TMDB", key)),
        };
        Ok(Self {
            db,
            channels,
            arrs,
            http: client,
            tvdb,
            tmdb,
        })
    }

    /// What a draft says about `channel_id`: the configured entry or the defaults.
    pub fn channel(&self, channel_id: &str) -> ChannelInfo {
        self.channels.get(channel_id).cloned().unwrap_or_default()
    }

    /// Asks one source. The error is the state to report: not set up, or failed.
    async fn ask(&self, source: Source, query: &Query<'_>) -> Result<Vec<Found>, SourceState> {
        let not_set = |reason: &str| SourceState::NotChecked {
            reason: reason.to_owned(),
        };
        let failed = |reason: String| {
            tracing::warn!(source = source.label(), error = %reason, "draft check failed");
            SourceState::Failed { reason }
        };
        match source {
            Source::Sonarr => {
                let sonarr = self
                    .arrs
                    .sonarr()
                    .ok_or_else(|| not_set("Sonarr is not connected"))?;
                Sonarr::with_http(&sonarr, self.http.clone())
                    .lookup(query.title)
                    .await
                    .map(|found| lookup::from_sonarr(found, query))
                    .map_err(|e| failed(e.to_string()))
            }
            Source::Radarr => {
                let radarr = self
                    .arrs
                    .radarr()
                    .ok_or_else(|| not_set("Radarr is not connected"))?;
                Radarr::with_http(&radarr, self.http.clone())
                    .lookup(query.title)
                    .await
                    .map(|found| lookup::from_radarr(found, query))
                    .map_err(|e| failed(e.to_string()))
            }
            Source::TvdbSeries => {
                let tvdb = self.tvdb.as_ref().map_err(|reason| not_set(reason))?;
                tvdb.search(query).await.map_err(|e| failed(e.to_string()))
            }
            Source::TmdbMovie | Source::TmdbTv => {
                let tmdb = self.tmdb.as_ref().map_err(|reason| not_set(reason))?;
                tmdb.search(source, query)
                    .await
                    .map_err(|e| failed(e.to_string()))
            }
        }
    }

    /// Searches for a title. Never fails: a source that cannot be asked is reported as such.
    /// Shows are decided by Sonarr and TVDB, films by Radarr and TMDB films; TMDB TV is shown for
    /// shows as a hint only.
    pub async fn check(
        &self,
        kind: Kind,
        title: &str,
        original_title: Option<&str>,
        year: Option<i64>,
    ) -> CheckResult {
        let query = Query {
            title,
            original_title,
            year,
        };
        let (deciding, extra): (&[Source], &[Source]) = match kind {
            Kind::Show => (&[Source::Sonarr, Source::TvdbSeries], &[Source::TmdbTv]),
            Kind::Film => (&[Source::Radarr, Source::TmdbMovie], &[]),
        };
        let mut reports = Vec::new();
        for &source in deciding.iter().chain(extra) {
            let (state, candidates) = match self.ask(source, &query).await {
                Ok(found) => (SourceState::Checked, found),
                Err(state) => (state, Vec::new()),
            };
            reports.push(SourceReport {
                source,
                state,
                candidates,
            });
        }
        let deciders: Vec<&SourceReport> = reports
            .iter()
            .filter(|r| deciding.contains(&r.source))
            .collect();
        let checked = deciders.iter().any(|r| r.state == SourceState::Checked);
        let best = deciders
            .iter()
            .flat_map(|r| r.candidates.first())
            .max_by(|a, b| a.score.total_cmp(&b.score))
            .cloned();
        let verdict = if checked {
            match best.as_ref().map(|b| b.score) {
                Some(s) if s >= FOUND => Verdict::Found,
                Some(s) if s >= POSSIBLE => Verdict::Possible,
                _ => Verdict::ProbablyMissing,
            }
        } else if deciders
            .iter()
            .any(|r| matches!(r.state, SourceState::Failed { .. }))
        {
            Verdict::Unknown
        } else {
            Verdict::NotChecked
        };
        CheckResult {
            kind,
            title: title.to_owned(),
            original_title: original_title.map(str::to_owned),
            year,
            verdict,
            best,
            sources: reports,
        }
    }

    /// Whether TVDB and TMDB can be asked, or why not.
    pub fn keys(&self) -> serde_json::Value {
        fn state<T>(site: &Result<T, String>) -> serde_json::Value {
            match site {
                Ok(_) => serde_json::json!({ "ready": true }),
                Err(reason) => serde_json::json!({ "ready": false, "reason": reason }),
            }
        }
        serde_json::json!({ "tvdb": state(&self.tvdb), "tmdb": state(&self.tmdb) })
    }

    pub async fn get(&self, id: i64) -> Result<Option<StoredDraft>, StoreError> {
        store::get(&self.db, id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_db;

    #[tokio::test]
    async fn without_keys_nothing_is_checked() {
        let (db, _dir) = test_db::fresh().await;
        let helper = DraftHelper::new(
            db,
            &Drafts::default(),
            Channels::default(),
            ArrConnections::new(None, None),
        )
        .unwrap();
        let result = helper.check(Kind::Show, "Tere Bin", None, None).await;
        assert_eq!(result.verdict, Verdict::NotChecked);
        assert_eq!(result.sources.len(), 3);
        assert!(matches!(
            &result.sources[1].state,
            SourceState::NotChecked { reason } if reason == "no TVDB key configured"
        ));
        assert_eq!(
            result.summary(),
            "Not checked against TVDB: Sonarr is not connected and no TVDB key is set"
        );
    }

    #[test]
    fn missing_key_files_are_not_fatal() {
        let drafts = DraftSettings {
            tvdb_api_key_file: Some("/nonexistent/tvdb".into()),
            tmdb_read_token_file: Some("/nonexistent/tmdb".into()),
            ..DraftSettings::default()
        }
        .check()
        .unwrap();
        assert!(
            matches!(&drafts.tvdb_key, Key::Unreadable(why) if why.contains("drafts.tvdb_api_key_file"))
        );
        assert!(matches!(&drafts.tmdb_token, Key::Unreadable(_)));
    }

    #[test]
    fn bad_settings_name_their_key() {
        let pin_only = DraftSettings {
            tvdb_pin_file: Some("/x".into()),
            ..DraftSettings::default()
        };
        assert!(
            pin_only
                .check()
                .unwrap_err()
                .to_string()
                .contains("drafts.tvdb_pin_file")
        );
    }
}
