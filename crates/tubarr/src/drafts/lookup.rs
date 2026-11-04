//! Read-only searches to see whether a show or film is on TVDB or TMDB before a draft is entered
//! by hand. The main check asks Sonarr (`/api/v3/series/lookup`, TVDB data) and Radarr
//! (`/api/v3/movie/lookup`, TMDB data), so no key of Tubarr's own is needed. TheTVDB (API v4) and
//! TMDB (API v3) are asked directly as extras when their key files are set. Tubarr only reads:
//! nothing is ever written to any of them.
//!
//! TVDB: `POST /login` with the project key (and a subscriber PIN for user-supported keys) gives a
//! bearer token valid for a month, then `GET /search?query=&type=series`. TMDB: the read access
//! token is the bearer token for `GET /search/movie` and `GET /search/tv`.

use std::time::Duration;

use reqwest::{Client, Response, StatusCode, header::RETRY_AFTER};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use url::Url;

use super::names::score;
use crate::{
    arr::{AlternateTitle, Language, MovieLookup, SeriesLookup},
    config::Secret,
};

/// TMDB's genre id for TV films, which is how telefilms are entered there.
pub const TMDB_TV_MOVIE_GENRE: i64 = 10770;

/// How many candidates a source reports, best first.
const KEEP: usize = 5;

/// Overview snippets are cut to about this many characters.
const SNIPPET: usize = 200;

/// Where a candidate comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Sonarr's series lookup, which answers from TVDB.
    Sonarr,
    /// Radarr's film lookup, which answers from TMDB.
    Radarr,
    TvdbSeries,
    TmdbMovie,
    TmdbTv,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sonarr => "Sonarr (TVDB)",
            Self::Radarr => "Radarr (TMDB)",
            Self::TvdbSeries => "TVDB",
            Self::TmdbMovie => "TMDB films",
            Self::TmdbTv => "TMDB TV",
        }
    }
}

/// An entry a search returned, scored against the title asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub source: Source,
    pub id: String,
    pub name: String,
    pub original_name: Option<String>,
    pub year: Option<i64>,
    pub overview: Option<String>,
    pub country: Option<String>,
    pub language: Option<String>,
    /// TMDB lists it as a TV film.
    pub tv_film: bool,
    /// Sonarr or Radarr already has it.
    pub in_arr: bool,
    pub url: String,
    /// 0 to 1: the best match of the title (and original title) against every name it has.
    pub score: f64,
    /// The year is more than one off the year asked for.
    pub year_differs: bool,
    /// Every name the entry goes by: aliases and translations. Not sent out, only scored.
    #[serde(skip)]
    pub names: Vec<String>,
}

/// Why a search failed. Never carries a key or token.
#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error("rate limited{}", retry_text(*.retry_after))]
    RateLimited { retry_after: Option<u64> },
    #[error("the API key or token was refused (HTTP {0})")]
    Refused(u16),
    #[error("HTTP {0}")]
    Status(u16),
    #[error("request failed: {0}")]
    Http(String),
    #[error("unexpected answer: {0}")]
    Parse(String),
}

fn retry_text(after: Option<u64>) -> String {
    after.map_or_else(String::new, |s| format!(", retry after {s}s"))
}

impl From<reqwest::Error> for LookupError {
    fn from(e: reqwest::Error) -> Self {
        // Without the URL: it holds only the query, but the message stays short.
        Self::Http(e.without_url().to_string())
    }
}

/// Builds the client both sources share.
pub fn client() -> Result<Client, reqwest::Error> {
    Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(concat!("tubarr/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// The status a search answered with, as an error unless it is a success.
async fn checked(response: Response) -> Result<Response, LookupError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    Err(match status {
        StatusCode::TOO_MANY_REQUESTS => LookupError::RateLimited {
            retry_after: response
                .headers()
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse().ok()),
        },
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => LookupError::Refused(status.as_u16()),
        s => LookupError::Status(s.as_u16()),
    })
}

fn snippet(text: Option<String>) -> Option<String> {
    let text = text?.trim().to_owned();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= SNIPPET {
        return Some(text);
    }
    let cut: String = text.chars().take(SNIPPET).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(head, _)| head);
    Some(format!("{cut}..."))
}

fn year_of(date: Option<&str>) -> Option<i64> {
    date.and_then(|d| d.get(0..4)).and_then(|y| y.parse().ok())
}

/// What is being looked for.
#[derive(Debug, Clone)]
pub struct Query<'a> {
    pub title: &'a str,
    pub original_title: Option<&'a str>,
    pub year: Option<i64>,
}

/// Scores `found` against the query, keeps the best few.
pub fn rank(mut found: Vec<Found>, query: &Query<'_>) -> Vec<Found> {
    for f in &mut found {
        let asked = std::iter::once(query.title).chain(query.original_title);
        let mut best = 0.0_f64;
        for a in asked {
            for name in std::iter::once(&f.name)
                .chain(f.original_name.iter())
                .chain(f.names.iter())
            {
                best = best.max(score(a, name));
            }
        }
        f.year_differs = matches!((query.year, f.year), (Some(a), Some(b)) if (a - b).abs() > 1);
        if f.year_differs {
            best *= 0.8;
        }
        f.score = (best * 1000.0).round() / 1000.0;
    }
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    found.truncate(KEEP);
    found
}

/// TheTVDB v4.
#[derive(Debug)]
pub struct Tvdb {
    client: Client,
    base: Url,
    key: Secret,
    pin: Option<Secret>,
    token: Mutex<Option<String>>,
}

#[derive(Serialize)]
struct Login<'a> {
    apikey: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pin: Option<&'a str>,
}

#[derive(Deserialize)]
struct Envelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct Token {
    token: String,
}

#[derive(Deserialize)]
struct TvdbResult {
    tvdb_id: Option<String>,
    id: Option<String>,
    name: Option<String>,
    year: Option<String>,
    first_air_time: Option<String>,
    overview: Option<String>,
    #[serde(default)]
    overviews: std::collections::HashMap<String, String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    translations: std::collections::HashMap<String, String>,
    country: Option<String>,
    primary_language: Option<String>,
}

impl Tvdb {
    pub fn new(client: Client, base: Url, key: Secret, pin: Option<Secret>) -> Self {
        Self {
            client,
            base,
            key,
            pin,
            token: Mutex::new(None),
        }
    }

    async fn login(&self) -> Result<String, LookupError> {
        let url = self
            .base
            .join("login")
            .map_err(|e| LookupError::Parse(e.to_string()))?;
        let body = Login {
            apikey: self.key.expose(),
            pin: self.pin.as_ref().map(Secret::expose),
        };
        let response = checked(self.client.post(url).json(&body).send().await?).await?;
        let token: Envelope<Token> = response
            .json()
            .await
            .map_err(|e| LookupError::Parse(e.without_url().to_string()))?;
        Ok(token.data.token)
    }

    /// Series named like the query. Logs in on first use and again once if the token expired.
    pub async fn search(&self, query: &Query<'_>) -> Result<Vec<Found>, LookupError> {
        let mut url = self
            .base
            .join("search")
            .map_err(|e| LookupError::Parse(e.to_string()))?;
        url.query_pairs_mut()
            .append_pair("query", query.title)
            .append_pair("type", "series");
        let mut token = self.token.lock().await;
        for attempt in 0..2 {
            let current = match token.as_ref() {
                Some(t) => t.clone(),
                None => {
                    let fresh = self.login().await?;
                    *token = Some(fresh.clone());
                    fresh
                }
            };
            let response = self
                .client
                .get(url.clone())
                .bearer_auth(&current)
                .send()
                .await?;
            if response.status() == StatusCode::UNAUTHORIZED && attempt == 0 {
                tracing::debug!("TVDB token refused, logging in again");
                *token = None;
                continue;
            }
            let results: Envelope<Option<Vec<TvdbResult>>> = checked(response)
                .await?
                .json()
                .await
                .map_err(|e| LookupError::Parse(e.without_url().to_string()))?;
            let found = results
                .data
                .unwrap_or_default()
                .into_iter()
                .filter_map(tvdb_found)
                .collect();
            return Ok(rank(found, query));
        }
        Err(LookupError::Refused(401))
    }
}

fn tvdb_found(r: TvdbResult) -> Option<Found> {
    let id = r
        .tvdb_id
        .or_else(|| r.id.map(|i| i.trim_start_matches("series-").to_owned()))?;
    let name = r.name?;
    let year = r
        .year
        .as_deref()
        .and_then(|y| y.parse().ok())
        .or_else(|| year_of(r.first_air_time.as_deref()));
    let overview = r.overview.or_else(|| r.overviews.get("eng").cloned());
    let original_name = r
        .primary_language
        .as_ref()
        .and_then(|lang| r.translations.get(lang))
        .filter(|n| **n != name)
        .cloned();
    let mut names = r.aliases;
    names.extend(r.translations.into_values());
    Some(Found {
        source: Source::TvdbSeries,
        url: format!("https://thetvdb.com/dereferrer/series/{id}"),
        id,
        name,
        original_name,
        year,
        overview: snippet(overview),
        country: r.country,
        language: r.primary_language,
        tv_film: false,
        in_arr: false,
        score: 0.0,
        year_differs: false,
        names,
    })
}

/// TMDB v3 with a read access token.
#[derive(Debug)]
pub struct Tmdb {
    client: Client,
    base: Url,
    token: Secret,
}

#[derive(Deserialize)]
struct TmdbPage {
    #[serde(default)]
    results: Vec<TmdbResult>,
}

#[derive(Deserialize)]
struct TmdbResult {
    id: i64,
    title: Option<String>,
    name: Option<String>,
    original_title: Option<String>,
    original_name: Option<String>,
    release_date: Option<String>,
    first_air_date: Option<String>,
    overview: Option<String>,
    original_language: Option<String>,
    #[serde(default)]
    origin_country: Vec<String>,
    #[serde(default)]
    genre_ids: Vec<i64>,
}

impl Tmdb {
    pub fn new(client: Client, base: Url, token: Secret) -> Self {
        Self {
            client,
            base,
            token,
        }
    }

    /// Films (`TmdbMovie`) or TV shows (`TmdbTv`) named like the query.
    pub async fn search(
        &self,
        source: Source,
        query: &Query<'_>,
    ) -> Result<Vec<Found>, LookupError> {
        let (path, year_key) = match source {
            Source::TmdbTv => ("search/tv", "first_air_date_year"),
            _ => ("search/movie", "year"),
        };
        let mut url = self
            .base
            .join(path)
            .map_err(|e| LookupError::Parse(e.to_string()))?;
        {
            let mut pairs = url.query_pairs_mut();
            pairs
                .append_pair("query", query.title)
                .append_pair("include_adult", "false");
            // Only films: a TV show's first air year is often a year before the YouTube upload.
            if let (Source::TmdbMovie, Some(year)) = (source, query.year) {
                pairs.append_pair(year_key, &year.to_string());
            }
        }
        let response = self
            .client
            .get(url)
            .bearer_auth(self.token.expose())
            .send()
            .await?;
        let page: TmdbPage = checked(response)
            .await?
            .json()
            .await
            .map_err(|e| LookupError::Parse(e.without_url().to_string()))?;
        let kind = if source == Source::TmdbTv {
            "tv"
        } else {
            "movie"
        };
        let found = page
            .results
            .into_iter()
            .filter_map(|r| {
                let name = r.title.or(r.name)?;
                let original_name = r.original_title.or(r.original_name);
                Some(Found {
                    source,
                    id: r.id.to_string(),
                    url: format!("https://www.themoviedb.org/{kind}/{}", r.id),
                    year: year_of(r.release_date.or(r.first_air_date).as_deref()),
                    overview: snippet(r.overview),
                    country: r.origin_country.first().cloned(),
                    language: r.original_language,
                    tv_film: r.genre_ids.contains(&TMDB_TV_MOVIE_GENRE),
                    in_arr: false,
                    original_name: original_name.filter(|o| *o != name),
                    name,
                    score: 0.0,
                    year_differs: false,
                    names: Vec::new(),
                })
            })
            .collect();
        Ok(rank(found, query))
    }
}

fn language(l: Option<Language>) -> Option<String> {
    l.and_then(|l| l.name)
        .filter(|n| !n.is_empty() && n != "Unknown")
}

fn alternates(titles: Vec<AlternateTitle>) -> Vec<String> {
    titles.into_iter().filter_map(|t| t.title).collect()
}

/// Sonarr's lookup answer as candidates. Entries without a TVDB id are not on TVDB.
pub fn from_sonarr(series: Vec<SeriesLookup>, query: &Query<'_>) -> Vec<Found> {
    let found = series
        .into_iter()
        .filter(|s| s.tvdb_id > 0)
        .map(|s| Found {
            source: Source::Sonarr,
            id: s.tvdb_id.to_string(),
            url: format!("https://thetvdb.com/dereferrer/series/{}", s.tvdb_id),
            name: s.title,
            original_name: None,
            year: (s.year > 0).then_some(s.year),
            overview: snippet(s.overview),
            country: None,
            language: language(s.original_language),
            tv_film: false,
            in_arr: s.id.is_some_and(|id| id > 0),
            score: 0.0,
            year_differs: false,
            names: alternates(s.alternate_titles),
        })
        .collect();
    rank(found, query)
}

/// Radarr's lookup answer as candidates. Entries without a TMDB id are not on TMDB.
pub fn from_radarr(movies: Vec<MovieLookup>, query: &Query<'_>) -> Vec<Found> {
    let found = movies
        .into_iter()
        .filter(|m| m.tmdb_id > 0)
        .map(|m| {
            let original_name = m.original_title.filter(|o| *o != m.title);
            Found {
                source: Source::Radarr,
                id: m.tmdb_id.to_string(),
                url: format!("https://www.themoviedb.org/movie/{}", m.tmdb_id),
                year: (m.year > 0).then_some(m.year),
                overview: snippet(m.overview),
                country: None,
                language: language(m.original_language),
                tv_film: m.genres.iter().any(|g| g.eq_ignore_ascii_case("TV Movie")),
                in_arr: m.id.is_some_and(|id| id > 0),
                score: 0.0,
                year_differs: false,
                names: alternates(m.alternate_titles),
                name: m.title,
                original_name,
            }
        })
        .collect();
    rank(found, query)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(name: &str, year: Option<i64>, names: &[&str]) -> Found {
        Found {
            source: Source::TvdbSeries,
            id: "1".to_owned(),
            name: name.to_owned(),
            original_name: None,
            year,
            overview: None,
            country: None,
            language: None,
            tv_film: false,
            in_arr: false,
            url: String::new(),
            score: 0.0,
            year_differs: false,
            names: names.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn aliases_and_original_titles_count() {
        let query = Query {
            title: "Kabhi Mein Kabhi Tum",
            original_title: Some("کبھی میں کبھی تم"),
            year: None,
        };
        let ranked = rank(
            vec![
                found("Something Else", None, &[]),
                found("KMKT", None, &["کبھی میں کبھی تم"]),
            ],
            &query,
        );
        assert_eq!(ranked[0].name, "KMKT");
        assert!((ranked[0].score - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_far_year_lowers_the_score() {
        let query = Query {
            title: "Theri",
            original_title: None,
            year: Some(2016),
        };
        let ranked = rank(
            vec![
                found("Theri", Some(1990), &[]),
                found("Theri", Some(2017), &[]),
            ],
            &query,
        );
        assert_eq!(ranked[0].year, Some(2017));
        assert!(!ranked[0].year_differs);
        assert!(ranked[1].year_differs);
        assert!(ranked[1].score < 0.85);
    }

    #[test]
    fn snippets_cut_at_a_word() {
        let long = "word ".repeat(100);
        let cut = snippet(Some(long)).unwrap();
        assert!(cut.ends_with("..."));
        assert!(cut.chars().count() <= SNIPPET + 3);
        assert_eq!(snippet(Some("  ".to_owned())), None);
    }
}
