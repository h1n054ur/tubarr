//! The Newznab indexer API that Sonarr and Radarr search: `t=caps`, `tvsearch`, `movie`, `search`,
//! the RSS feed (a search without ids) and `t=get` for the NZB.
//!
//! Answers come from a [`ReleaseSource`]: the matcher's accepted videos in the `releases` table.

pub mod title;
pub mod xml;

use std::{collections::HashMap, future::Future};

use axum::{
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use url::Url;

use crate::{
    AppState,
    activity::{self, Activity},
    store::{Db, EpisodeFilter, MappingKind, PinTarget, ReleaseKind, StoreError, StoredRelease},
};
use title::{TitleTag, release_title};

/// A Newznab category Tubarr files releases under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Category {
    pub id: u32,
    pub name: &'static str,
    pub parent: Option<u32>,
}

impl Category {
    pub const MOVIES: Self = Self::new(2000, "Movies", None);
    pub const MOVIES_HD: Self = Self::new(2040, "HD", Some(2000));
    pub const TV: Self = Self::new(5000, "TV", None);
    pub const TV_HD: Self = Self::new(5040, "HD", Some(5000));
    pub const ALL: [Self; 4] = [Self::MOVIES, Self::MOVIES_HD, Self::TV, Self::TV_HD];

    const fn new(id: u32, name: &'static str, parent: Option<u32>) -> Self {
        Self { id, name, parent }
    }

    /// `TV > HD`, the text of an item's `<category>`.
    pub fn full_name(&self) -> String {
        match Self::ALL.iter().find(|c| Some(c.id) == self.parent) {
            Some(parent) => format!("{} > {}", parent.name, self.name),
            None => self.name.to_owned(),
        }
    }

    /// The category every release of `kind` is filed under, with its parent as a second one.
    fn of(kind: ReleaseKind) -> Self {
        match kind {
            ReleaseKind::Tv => Self::TV_HD,
            ReleaseKind::Film => Self::MOVIES_HD,
        }
    }
}

/// One answer to a search: a YouTube video offered as a release for one episode or film.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The video id, or a split upload's part ids joined by commas in play order.
    pub video_id: String,
    /// The scene-style release name, built from the arr's title.
    pub title: String,
    /// Upload time, Unix seconds.
    pub published_at: i64,
    /// Bytes, only when known.
    pub size: Option<i64>,
    /// English language name.
    pub language: String,
    pub target: ReleaseTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseTarget {
    Episode {
        tvdb_id: i64,
        season: i64,
        episode: i64,
    },
    Daily {
        tvdb_id: i64,
        /// `YYYY-MM-DD`.
        air_date: String,
    },
    Film {
        tmdb_id: i64,
        imdb_id: Option<i64>,
    },
    /// The one item an empty RSS feed holds (see [`respond`]). It carries no ids.
    Placeholder(ReleaseKind),
}

impl Release {
    pub fn kind(&self) -> ReleaseKind {
        match self.target {
            ReleaseTarget::Film { .. } => ReleaseKind::Film,
            ReleaseTarget::Placeholder(kind) => kind,
            ReleaseTarget::Episode { .. } | ReleaseTarget::Daily { .. } => ReleaseKind::Tv,
        }
    }

    pub fn category(&self) -> Category {
        Category::of(self.kind())
    }

    /// Stable per video and target, so an arr's RSS sync sees the same release only once.
    pub fn guid(&self) -> String {
        let target = match &self.target {
            ReleaseTarget::Episode {
                season, episode, ..
            } => format!("S{season:02}E{episode:02}"),
            ReleaseTarget::Daily { air_date, .. } => air_date.clone(),
            ReleaseTarget::Film { .. } => "film".to_owned(),
            ReleaseTarget::Placeholder(_) => return PLACEHOLDER_ID.to_owned(),
        };
        format!("tubarr-{}-{target}", self.video_id)
    }

    /// The release for a stored row. `None` for a row whose target does not fit its mapping (an
    /// episode on a film), which the matcher never writes.
    pub fn from_stored(row: StoredRelease) -> Option<Self> {
        let (tag, target) = match (&row.target, row.kind, row.tvdb_id, row.tmdb_id) {
            (PinTarget::Episode { season, episode }, MappingKind::Show, Some(tvdb_id), _) => (
                TitleTag::Episode {
                    season: *season,
                    episode: *episode,
                },
                ReleaseTarget::Episode {
                    tvdb_id,
                    season: *season,
                    episode: *episode,
                },
            ),
            (PinTarget::AirDate(date), MappingKind::Daily, Some(tvdb_id), _) => (
                TitleTag::AirDate(date),
                ReleaseTarget::Daily {
                    tvdb_id,
                    air_date: date.clone(),
                },
            ),
            (PinTarget::Film, MappingKind::Film, _, Some(tmdb_id)) => (
                TitleTag::Film { year: row.year },
                ReleaseTarget::Film {
                    tmdb_id,
                    imdb_id: row.imdb_id,
                },
            ),
            _ => {
                tracing::warn!(release = row.id, "release target does not fit its mapping");
                return None;
            }
        };
        Some(Self {
            title: release_title(&row.arr_title, tag, row.height, &row.language),
            video_id: row.key(),
            published_at: row.published_at,
            size: row.size,
            language: row.language,
            target,
        })
    }
}

/// Where releases come from. The Newznab handlers only talk to this, so the matcher can change
/// how candidates are stored without touching the API. Every list is newest first.
pub trait ReleaseSource: Sync {
    /// Releases of a series by TVDB id.
    fn series(
        &self,
        tvdb_id: i64,
        filter: &EpisodeFilter,
        limit: i64,
    ) -> impl Future<Output = Result<Vec<Release>, StoreError>> + Send;

    /// Releases of a film by TMDB id or IMDb id (digits).
    fn film(
        &self,
        tmdb_id: Option<i64>,
        imdb_id: Option<i64>,
        limit: i64,
    ) -> impl Future<Output = Result<Vec<Release>, StoreError>> + Send;

    /// Releases whose name holds every word of `query`.
    fn search(
        &self,
        query: &str,
        filter: &EpisodeFilter,
        kind: Option<ReleaseKind>,
        limit: i64,
    ) -> impl Future<Output = Result<Vec<Release>, StoreError>> + Send;

    /// The newest releases, for the arrs' RSS sync.
    fn recent(
        &self,
        kind: Option<ReleaseKind>,
        limit: i64,
    ) -> impl Future<Output = Result<Vec<Release>, StoreError>> + Send;

    /// The release of a video, for its NZB.
    fn release(
        &self,
        video_id: &str,
    ) -> impl Future<Output = Result<Option<Release>, StoreError>> + Send;
}

fn releases(rows: Vec<StoredRelease>) -> Vec<Release> {
    rows.into_iter().filter_map(Release::from_stored).collect()
}

/// Lower-cased words, for matching a query against a release name.
fn folded_words(text: &str) -> Vec<String> {
    title::words(text)
        .into_iter()
        .map(|w| w.to_lowercase())
        .collect()
}

impl ReleaseSource for Db {
    async fn series(
        &self,
        tvdb_id: i64,
        filter: &EpisodeFilter,
        limit: i64,
    ) -> Result<Vec<Release>, StoreError> {
        Ok(releases(
            self.releases_for_series(tvdb_id, filter, limit).await?,
        ))
    }

    async fn film(
        &self,
        tmdb_id: Option<i64>,
        imdb_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Release>, StoreError> {
        Ok(releases(
            self.releases_for_film(tmdb_id, imdb_id, limit).await?,
        ))
    }

    async fn search(
        &self,
        query: &str,
        filter: &EpisodeFilter,
        kind: Option<ReleaseKind>,
        limit: i64,
    ) -> Result<Vec<Release>, StoreError> {
        let wanted = folded_words(query);
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        // Mappings sharing a word with the query, then the full check on each release name, so
        // a query like "Theri 2016" or "Kabhi Main S01E01" also matches.
        let mapping_ids: Vec<i64> = self
            .mappings()
            .await?
            .into_iter()
            .filter(|m| match kind {
                Some(ReleaseKind::Film) => m.kind == MappingKind::Film,
                Some(ReleaseKind::Tv) => m.kind != MappingKind::Film,
                None => true,
            })
            .filter(|m| {
                let title = folded_words(&m.title);
                wanted.iter().any(|w| title.contains(w))
            })
            .map(|m| m.id)
            .collect();
        if mapping_ids.is_empty() {
            return Ok(Vec::new());
        }
        let found = releases(
            self.releases_for_mappings(&mapping_ids, filter, i64::from(xml::MAX_LIMIT))
                .await?,
        );
        Ok(found
            .into_iter()
            .filter(|r| {
                let name = folded_words(&r.title);
                wanted.iter().all(|w| name.contains(w))
            })
            .take(usize::try_from(limit).unwrap_or(0))
            .collect())
    }

    async fn recent(
        &self,
        kind: Option<ReleaseKind>,
        limit: i64,
    ) -> Result<Vec<Release>, StoreError> {
        Ok(releases(self.recent_releases(kind, limit).await?))
    }

    async fn release(&self, video_id: &str) -> Result<Option<Release>, StoreError> {
        Ok(self
            .release_by_video(video_id)
            .await?
            .and_then(Release::from_stored))
    }
}

/// A Newznab error, sent as an `<error>` document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiError {
    BadKey,
    Missing(&'static str),
    Incorrect(&'static str),
    NoSuchFunction,
    NoSuchItem,
    Internal,
}

impl ApiError {
    fn code(&self) -> u16 {
        match self {
            Self::BadKey => 100,
            Self::Missing(_) => 200,
            Self::Incorrect(_) => 201,
            Self::NoSuchFunction => 202,
            Self::NoSuchItem => 300,
            Self::Internal => 900,
        }
    }

    fn description(&self) -> String {
        match self {
            Self::BadKey => "Incorrect user credentials".to_owned(),
            Self::Missing(name) => format!("Missing parameter ({name})"),
            Self::Incorrect(name) => format!("Incorrect parameter ({name})"),
            Self::NoSuchFunction => "No such function".to_owned(),
            Self::NoSuchItem => "No such item".to_owned(),
            Self::Internal => "Unknown error".to_owned(),
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            Self::BadKey => StatusCode::UNAUTHORIZED,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            // Newznab sends the rest as 200, so the arrs read the description instead of
            // stopping at the HTTP status.
            _ => StatusCode::OK,
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        tracing::error!(error = %e, "newznab search failed");
        Self::Internal
    }
}

/// A finished answer, turned into an HTTP response at the edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Xml(StatusCode, String),
    Nzb { filename: String, body: String },
}

impl From<ApiError> for Reply {
    fn from(e: ApiError) -> Self {
        Self::Xml(e.status(), xml::error(e.code(), &e.description()))
    }
}

impl IntoResponse for Reply {
    fn into_response(self) -> Response {
        match self {
            Self::Xml(status, body) => (
                status,
                [(header::CONTENT_TYPE, "application/xml; charset=utf-8")],
                body,
            )
                .into_response(),
            Self::Nzb { filename, body } => {
                let disposition = format!("attachment; filename=\"{}\"", filename.replace('"', ""));
                let mut response = (
                    [(header::CONTENT_TYPE, "application/x-nzb; charset=utf-8")],
                    body,
                )
                    .into_response();
                if let Ok(value) = HeaderValue::from_str(&disposition) {
                    response
                        .headers_mut()
                        .insert(header::CONTENT_DISPOSITION, value);
                }
                response
            }
        }
    }
}

/// What the answers need besides the query: where the NZBs download from.
pub struct Context<'a> {
    pub public_url: &'a Url,
    /// Repeated in every download link, since the arrs fetch the enclosure as given.
    pub api_key: &'a str,
    /// `[newznab] report_size`. Off, every item goes out with no size, so the arrs skip their
    /// size check.
    pub report_size: bool,
}

impl Context<'_> {
    /// The `t=get` link of a release's NZB, as every feed item carries it.
    pub fn download_url(&self, video_id: &str) -> String {
        let mut url = self
            .public_url
            .join("api")
            .unwrap_or_else(|_| self.public_url.clone());
        url.query_pairs_mut()
            .append_pair("t", "get")
            .append_pair("id", video_id)
            .append_pair("apikey", self.api_key);
        url.into()
    }
}

/// Answers a Newznab request (`t=` present) on `/api`, after the API key check.
pub async fn handle(state: &AppState, params: &HashMap<String, String>) -> Response {
    let Some(key) = crate::api::checked_key(state, params) else {
        return Reply::from(ApiError::BadKey).into_response();
    };
    let ctx = Context {
        public_url: &state.config.public_url,
        api_key: key,
        report_size: state.config.newznab.report_size,
    };
    let reply = respond(&state.db, &ctx, params).await;
    let function = param(params, "t").unwrap_or_default();
    if let ("search" | "tvsearch" | "tv" | "movie", Reply::Xml(StatusCode::OK, body)) =
        (function, &reply)
    {
        state.activity.record(Activity::Search {
            function: function.to_owned(),
            query: activity::search_query(params),
            results: body.matches("<item>").count(),
        });
    }
    reply.into_response()
}

/// The Newznab API over any [`ReleaseSource`]. The key is already checked.
pub async fn respond<S: ReleaseSource>(
    source: &S,
    ctx: &Context<'_>,
    params: &HashMap<String, String>,
) -> Reply {
    let result = match param(params, "t").unwrap_or_default() {
        "caps" => Ok(Reply::Xml(StatusCode::OK, xml::caps())),
        "get" => nzb(source, params).await,
        "search" => search(source, ctx, params, Function::Search).await,
        "tvsearch" | "tv" => search(source, ctx, params, Function::Tv).await,
        "movie" => search(source, ctx, params, Function::Movie).await,
        _ => Err(ApiError::NoSuchFunction),
    };
    result.unwrap_or_else(Reply::from)
}

fn param<'a>(params: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    params.get(name).map(|v| v.trim()).filter(|v| !v.is_empty())
}

fn number<T: std::str::FromStr>(
    params: &HashMap<String, String>,
    name: &'static str,
) -> Result<Option<T>, ApiError> {
    param(params, name)
        .map(|v| v.parse().map_err(|_| ApiError::Incorrect(name)))
        .transpose()
}

async fn nzb<S: ReleaseSource>(
    source: &S,
    params: &HashMap<String, String>,
) -> Result<Reply, ApiError> {
    let id = param(params, "id").ok_or(ApiError::Missing("id"))?;
    let release = source.release(id).await?.ok_or(ApiError::NoSuchItem)?;
    Ok(Reply::Nzb {
        filename: format!("{}.nzb", release.title),
        body: xml::nzb(&release.video_id, &release.title, release.published_at),
    })
}

/// Every id a search can carry. Tubarr indexes only TVDB, TMDB and IMDb ids.
const ID_PARAMS: [&str; 5] = ["tvdbid", "tmdbid", "imdbid", "rid", "tvmazeid"];

/// The id of the placeholder release. No video has it, so its NZB is "no such item".
pub const PLACEHOLDER_ID: &str = "tubarr-placeholder";

fn placeholder(function: Function) -> Release {
    Release {
        video_id: PLACEHOLDER_ID.to_owned(),
        title: "Tubarr.Placeholder.No.Releases.Yet".to_owned(),
        published_at: crate::store::unix_now(),
        size: None,
        language: String::new(),
        target: ReleaseTarget::Placeholder(match function {
            Function::Movie => ReleaseKind::Film,
            Function::Tv | Function::Search => ReleaseKind::Tv,
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Function {
    Search,
    Tv,
    Movie,
}

/// Which kinds of release the `cat` parameter lets through. No `cat` lets everything through.
fn allowed_kinds(params: &HashMap<String, String>) -> Result<Vec<ReleaseKind>, ApiError> {
    let all = vec![ReleaseKind::Tv, ReleaseKind::Film];
    let Some(cats) = param(params, "cat") else {
        return Ok(all);
    };
    let cats: Vec<u32> = cats
        .split(',')
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(|c| c.parse().map_err(|_| ApiError::Incorrect("cat")))
        .collect::<Result<_, _>>()?;
    if cats.is_empty() {
        return Ok(all);
    }
    Ok(all
        .into_iter()
        .filter(|kind| {
            let category = Category::of(*kind);
            cats.iter()
                .any(|c| *c == category.id || Some(*c) == category.parent)
        })
        .collect())
}

/// The episode part of a TV search. Daily shows come as `season=2026&ep=10/14` (Sonarr's
/// NewznabRequestGenerator), numbered ones as `season=1&ep=2`. Sonarr sends season 0 as `00`.
fn episode_filter(params: &HashMap<String, String>) -> Result<EpisodeFilter, ApiError> {
    let season = param(params, "season");
    let ep = param(params, "ep");
    match (season, ep) {
        (None, None) => Ok(EpisodeFilter::Any),
        (None, Some(_)) => Err(ApiError::Missing("season")),
        (Some(season), None) => season
            .parse()
            .map(EpisodeFilter::Season)
            .map_err(|_| ApiError::Incorrect("season")),
        (Some(season), Some(ep)) if ep.contains('/') => {
            air_date(season, ep).map(EpisodeFilter::AirDate)
        }
        (Some(season), Some(ep)) => Ok(EpisodeFilter::Episode {
            season: season.parse().map_err(|_| ApiError::Incorrect("season"))?,
            episode: ep.parse().map_err(|_| ApiError::Incorrect("ep"))?,
        }),
    }
}

fn air_date(year: &str, month_day: &str) -> Result<String, ApiError> {
    let digits = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_digit());
    let (month, day) = month_day.split_once('/').ok_or(ApiError::Incorrect("ep"))?;
    if !digits(year, 4) {
        return Err(ApiError::Incorrect("season"));
    }
    if !digits(month, 2) || !digits(day, 2) {
        return Err(ApiError::Incorrect("ep"));
    }
    Ok(format!("{year}-{month}-{day}"))
}

/// IMDb ids come as digits (Radarr strips the `tt`), some clients keep it.
fn imdb_id(params: &HashMap<String, String>) -> Result<Option<i64>, ApiError> {
    param(params, "imdbid")
        .map(|v| {
            v.trim_start_matches("tt")
                .parse()
                .map_err(|_| ApiError::Incorrect("imdbid"))
        })
        .transpose()
}

async fn search<S: ReleaseSource>(
    source: &S,
    ctx: &Context<'_>,
    params: &HashMap<String, String>,
    function: Function,
) -> Result<Reply, ApiError> {
    let limit = number::<u32>(params, "limit")?
        .unwrap_or(xml::MAX_LIMIT)
        .clamp(1, xml::MAX_LIMIT);
    let offset = number::<u32>(params, "offset")?.unwrap_or(0);
    let mut kinds = allowed_kinds(params)?;
    match function {
        Function::Tv => kinds.retain(|k| *k == ReleaseKind::Tv),
        Function::Movie => kinds.retain(|k| *k == ReleaseKind::Film),
        Function::Search => {}
    }
    let query = param(params, "q");
    let limit_i = i64::from(limit);
    let has_id = ID_PARAMS.iter().any(|id| param(params, id).is_some());
    // No ids and no words: the arrs' RSS sync and their "Test" button.
    let rss = query.is_none() && !has_id;
    let kind = match kinds.as_slice() {
        [only] => Some(*only),
        _ => None,
    };

    // Everything fits on the first page, so later pages are empty and the arrs stop paging.
    let mut found = if offset > 0 || kinds.is_empty() {
        Vec::new()
    } else if rss {
        source.recent(kind, limit_i).await?
    } else {
        match function {
            Function::Tv => {
                let filter = episode_filter(params)?;
                if let Some(tvdb_id) = number::<i64>(params, "tvdbid")? {
                    source.series(tvdb_id, &filter, limit_i).await?
                } else if has_id {
                    // Only ids Tubarr does not index: no answer rather than a guess.
                    Vec::new()
                } else {
                    source
                        .search(query.unwrap_or_default(), &filter, kind, limit_i)
                        .await?
                }
            }
            Function::Movie => {
                let tmdb_id = number::<i64>(params, "tmdbid")?;
                let imdb_id = imdb_id(params)?;
                if tmdb_id.is_some() || imdb_id.is_some() {
                    source.film(tmdb_id, imdb_id, limit_i).await?
                } else if has_id {
                    Vec::new()
                } else {
                    let any = EpisodeFilter::Any;
                    source
                        .search(query.unwrap_or_default(), &any, kind, limit_i)
                        .await?
                }
            }
            Function::Search if has_id => Vec::new(),
            Function::Search => {
                let any = EpisodeFilter::Any;
                source
                    .search(query.unwrap_or_default(), &any, kind, limit_i)
                    .await?
            }
        }
    };

    // The arrs' "Test" button reads the RSS feed and fails on an empty one, so until the matcher
    // has found anything the feed holds one release no arr can parse, and its NZB does not exist.
    if found.is_empty() && rss && offset == 0 {
        found.push(placeholder(function));
    }
    if !ctx.report_size {
        for release in &mut found {
            release.size = None;
        }
    }
    let urls: Vec<String> = found
        .iter()
        .map(|r| ctx.download_url(&r.video_id))
        .collect();
    let items: Vec<xml::Item<'_>> = found
        .iter()
        .zip(&urls)
        .map(|(release, url)| xml::Item {
            release,
            download_url: url,
        })
        .collect();
    Ok(Reply::Xml(
        StatusCode::OK,
        xml::feed(ctx.public_url.as_str(), offset, &items),
    ))
}
