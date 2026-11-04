//! `/admin/api/unmatched` and `/admin/api/pins`: what the matcher left out, and Hani's picks.
//!
//! A pin or an unpin wakes the matcher, so the release follows within its settle time.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use serde::{Deserialize, Serialize};

use super::{
    AdminError, Authorized,
    paging::{Page, like_pattern},
};
use crate::{
    AppState,
    resolver::is_video_id,
    store::{MappingKind, Pin, PinTarget, StoreError, UnmatchedState},
};

/// One note with what the watcher knows about its video.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct UnmatchedView {
    pub id: i64,
    pub mapping_id: i64,
    pub mapping_title: String,
    pub mapping_kind: String,
    pub video_id: String,
    pub video_title: Option<String>,
    pub channel_id: Option<String>,
    pub duration_secs: Option<i64>,
    /// Unix seconds from the feed or listing, else null.
    pub published_at: Option<i64>,
    /// `YYYYMMDD` from a yt-dlp lookup, for videos the watcher never saw.
    pub upload_date: Option<String>,
    pub state: UnmatchedState,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub part: Option<i64>,
    pub reason: String,
    pub ignored: bool,
    pub updated_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct UnmatchedFilter {
    state: Option<String>,
    mapping_id: Option<i64>,
    /// Ignored notes are left out unless this is true.
    #[serde(default)]
    ignored: bool,
    /// Text the video title, show title, video id or channel id contains, any case.
    q: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// One page of notes and how many match the filter in all.
#[derive(Debug, Serialize)]
pub struct UnmatchedPage {
    pub items: Vec<UnmatchedView>,
    pub total: i64,
}

/// The joins and filter both the page and its count read. Binds: state, mapping id, show
/// ignored, `LIKE` pattern. A macro so `concat!` keeps the SQL a literal.
macro_rules! list_from {
    () => {
        "FROM unmatched u JOIN mappings m ON m.id = u.mapping_id \
         LEFT JOIN channel_videos c ON c.video_id = u.video_id \
         LEFT JOIN videos v ON v.video_id = u.video_id \
         WHERE (?1 IS NULL OR u.state = ?1) AND (?2 IS NULL OR u.mapping_id = ?2) \
           AND (?3 OR NOT u.ignored) \
           AND (?4 IS NULL OR coalesce(c.title, v.title) LIKE ?4 ESCAPE '\\' \
             OR m.title LIKE ?4 ESCAPE '\\' OR u.video_id LIKE ?4 ESCAPE '\\' \
             OR coalesce(c.channel_id, v.channel_id) LIKE ?4 ESCAPE '\\')"
    };
}

fn parse_state(name: &str) -> Result<UnmatchedState, AdminError> {
    match name {
        "pending" => Ok(UnmatchedState::Pending),
        "duplicate" => Ok(UnmatchedState::Duplicate),
        "unsure" => Ok(UnmatchedState::Unsure),
        "partial" => Ok(UnmatchedState::Partial),
        "capped" => Ok(UnmatchedState::Capped),
        other => Err(AdminError::Invalid(format!(
            "state must be pending, duplicate, unsure, partial or capped, not {other:?}"
        ))),
    }
}

pub async fn list(
    _: Authorized,
    State(state): State<AppState>,
    Query(filter): Query<UnmatchedFilter>,
) -> Result<Json<UnmatchedPage>, AdminError> {
    let wanted = filter.state.as_deref().map(parse_state).transpose()?;
    let page = Page::new(filter.limit, filter.offset)?;
    let pattern = like_pattern(filter.q.as_deref());
    let pool = state.db.pool();
    let items = sqlx::query_as(concat!(
        "SELECT u.id, u.mapping_id, m.title AS mapping_title, m.kind AS mapping_kind, u.video_id, \
           coalesce(c.title, v.title) AS video_title, \
           coalesce(c.channel_id, v.channel_id) AS channel_id, \
           coalesce(c.duration_secs, v.duration_secs) AS duration_secs, \
           c.published_at, v.upload_date, u.state, u.season, u.episode, u.part, u.reason, \
           u.ignored, u.updated_at ",
        list_from!(),
        " ORDER BY u.updated_at DESC, u.id DESC LIMIT ?5 OFFSET ?6"
    ))
    .bind(wanted)
    .bind(filter.mapping_id)
    .bind(filter.ignored)
    .bind(&pattern)
    .bind(page.limit)
    .bind(page.offset)
    .fetch_all(pool)
    .await
    .map_err(StoreError::from)?;
    let total = sqlx::query_scalar(concat!("SELECT count(*) ", list_from!()))
        .bind(wanted)
        .bind(filter.mapping_id)
        .bind(filter.ignored)
        .bind(&pattern)
        .fetch_one(pool)
        .await
        .map_err(StoreError::from)?;
    Ok(Json(UnmatchedPage { items, total }))
}

#[derive(Debug, Deserialize)]
pub struct IgnoreBody {
    ignored: bool,
}

pub async fn ignore(
    _: Authorized,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<IgnoreBody>,
) -> Result<StatusCode, AdminError> {
    if state.db.set_unmatched_ignored(id, body.ignored).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AdminError::NotFound)
    }
}

#[derive(Debug, Serialize)]
pub struct PinView {
    pub id: i64,
    pub mapping_id: i64,
    pub video_id: String,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub air_date: Option<String>,
    pub created_at: i64,
}

impl From<Pin> for PinView {
    fn from(pin: Pin) -> Self {
        let (season, episode, air_date) = match pin.target {
            PinTarget::Episode { season, episode } => (Some(season), Some(episode), None),
            PinTarget::AirDate(date) => (None, None, Some(date)),
            PinTarget::Film => (None, None, None),
        };
        Self {
            id: pin.id,
            mapping_id: pin.mapping_id,
            video_id: pin.video_id,
            season,
            episode,
            air_date,
            created_at: pin.created_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PinFilter {
    mapping_id: i64,
}

pub async fn pins(
    _: Authorized,
    State(state): State<AppState>,
    Query(filter): Query<PinFilter>,
) -> Result<Json<Vec<PinView>>, AdminError> {
    let pins = state.db.pins(filter.mapping_id).await?;
    Ok(Json(pins.into_iter().map(PinView::from).collect()))
}

#[derive(Debug, Deserialize)]
pub struct NewPin {
    pub(super) mapping_id: i64,
    pub(super) video_id: String,
    pub(super) season: Option<i64>,
    pub(super) episode: Option<i64>,
    pub(super) air_date: Option<String>,
}

fn is_air_date(date: &str) -> bool {
    parse_air_date(date).is_some()
}

/// `YYYY-MM-DD` with a real month and day, without pulling in a date crate for one check.
fn parse_air_date(date: &str) -> Option<(u32, u32, u32)> {
    let mut parts = date.split('-');
    let (y, m, d) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || y.len() != 4 || m.len() != 2 || d.len() != 2 {
        return None;
    }
    let (y, m, d): (u32, u32, u32) = (y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (1..=days).contains(&d).then_some((y, m, d))
}

pub(super) fn target_for(kind: MappingKind, new: &NewPin) -> Result<PinTarget, AdminError> {
    let invalid = |msg: &str| AdminError::Invalid(msg.to_owned());
    match kind {
        MappingKind::Show => match (new.season, new.episode, &new.air_date) {
            (Some(season), Some(episode), None) if season >= 0 && episode > 0 => {
                Ok(PinTarget::Episode { season, episode })
            }
            _ => Err(invalid(
                "a show pin needs season and episode, and no air_date",
            )),
        },
        MappingKind::Daily => match (new.season, new.episode, &new.air_date) {
            (None, None, Some(date)) if is_air_date(date) => Ok(PinTarget::AirDate(date.clone())),
            _ => Err(invalid(
                "a daily show pin needs air_date as YYYY-MM-DD, and nothing else",
            )),
        },
        MappingKind::Film => match (new.season, new.episode, &new.air_date) {
            (None, None, None) => Ok(PinTarget::Film),
            _ => Err(invalid("a film pin takes no season, episode or air_date")),
        },
    }
}

pub async fn create_pin(
    _: Authorized,
    State(state): State<AppState>,
    Json(new): Json<NewPin>,
) -> Result<(StatusCode, Json<PinView>), AdminError> {
    let video_id = new.video_id.trim();
    if !is_video_id(video_id) {
        return Err(AdminError::Invalid(format!(
            "{video_id:?} is not a YouTube video id"
        )));
    }
    let mapping = state
        .db
        .mapping(new.mapping_id)
        .await?
        .ok_or(AdminError::NotFound)?;
    let target = target_for(mapping.kind, &new)?;
    let pin = state.db.set_pin(mapping.id, &target, video_id).await?;
    state.matcher.waker().notify_one();
    Ok((StatusCode::CREATED, Json(pin.into())))
}

pub async fn delete_pin(
    _: Authorized,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AdminError> {
    if !state.db.delete_pin(id).await? {
        return Err(AdminError::NotFound);
    }
    state.matcher.waker().notify_one();
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn air_dates_must_be_real_days() {
        assert!(is_air_date("2026-10-14"));
        assert!(is_air_date("2024-02-29"));
        assert!(!is_air_date("2025-02-29"));
        assert!(!is_air_date("2026-13-01"));
        assert!(!is_air_date("2026-1-01"));
        assert!(!is_air_date("20261014"));
    }

    #[test]
    fn targets_follow_the_mapping_kind() {
        let pin = |season, episode, air_date: Option<&str>| NewPin {
            mapping_id: 1,
            video_id: "0eD-GQCutAU".to_owned(),
            season,
            episode,
            air_date: air_date.map(str::to_owned),
        };
        assert_eq!(
            target_for(MappingKind::Show, &pin(Some(1), Some(3), None)).unwrap(),
            PinTarget::Episode {
                season: 1,
                episode: 3
            }
        );
        assert!(target_for(MappingKind::Show, &pin(Some(1), None, None)).is_err());
        assert!(target_for(MappingKind::Daily, &pin(None, None, Some("2026-10-14"))).is_ok());
        assert!(target_for(MappingKind::Daily, &pin(Some(1), Some(1), None)).is_err());
        assert_eq!(
            target_for(MappingKind::Film, &pin(None, None, None)).unwrap(),
            PinTarget::Film
        );
        assert!(target_for(MappingKind::Film, &pin(None, None, Some("2026-10-14"))).is_err());
    }
}
