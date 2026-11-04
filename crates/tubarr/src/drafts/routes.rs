//! `/admin/api/drafts`: the check and the drafts, behind the admin API key.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    DraftError, Kind, StoredDraft,
    film::{self, FilmRequest},
    show::{self, ShowRequest},
    store,
};
use crate::{
    AppState,
    admin::{
        AdminError, Authorized,
        paging::{Page, like_pattern},
    },
};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/admin/api/drafts/check", post(check))
        .route("/admin/api/drafts/show", post(make_show))
        .route("/admin/api/drafts/film", post(make_film))
        .route("/admin/api/drafts", get(list))
        .route("/admin/api/drafts/{id}", get(one).delete(remove))
}

impl From<DraftError> for AdminError {
    fn from(e: DraftError) -> Self {
        match e {
            DraftError::BadRequest(m) | DraftError::NotReady(m) => Self::Invalid(m),
            DraftError::NotFound(_) => Self::NotFound,
            DraftError::Store(e) => Self::Store(e),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckRequest {
    kind: Kind,
    title: String,
    #[serde(default)]
    original_title: Option<String>,
    #[serde(default)]
    year: Option<i64>,
}

async fn check(
    _: Authorized,
    State(state): State<AppState>,
    Json(request): Json<CheckRequest>,
) -> Result<Json<Value>, AdminError> {
    let title = request.title.trim();
    if title.is_empty() {
        return Err(AdminError::Invalid("title must not be empty".to_owned()));
    }
    let result = state
        .drafts
        .check(
            request.kind,
            title,
            request.original_title.as_deref(),
            request.year,
        )
        .await;
    let summary = result.summary();
    let mut body = serde_json::to_value(result).map_err(|e| AdminError::Invalid(e.to_string()))?;
    body["summary"] = json!(summary);
    Ok(Json(body))
}

async fn make_show(
    _: Authorized,
    State(state): State<AppState>,
    Json(request): Json<ShowRequest>,
) -> Result<Json<StoredDraft>, AdminError> {
    let draft = state.drafts.show_draft(&request).await?;
    let text = show::text(&draft);
    let stored = store::insert(
        &state.db,
        Kind::Show,
        &draft.series_name,
        to_json(&request)?,
        to_json(&draft)?,
        text,
    )
    .await?;
    Ok(Json(stored))
}

async fn make_film(
    _: Authorized,
    State(state): State<AppState>,
    Json(request): Json<FilmRequest>,
) -> Result<Json<StoredDraft>, AdminError> {
    let draft = state.drafts.film_draft(&request).await?;
    let text = film::text(&draft);
    let stored = store::insert(
        &state.db,
        Kind::Film,
        &draft.title,
        to_json(&request)?,
        to_json(&draft)?,
        text,
    )
    .await?;
    Ok(Json(stored))
}

#[derive(Debug, Deserialize)]
struct ListFilter {
    kind: Option<Kind>,
    /// Text the title contains, any case.
    q: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

/// Newest first, with which sites the check can ask, so the UI can say when a key is missing.
async fn list(
    _: Authorized,
    State(state): State<AppState>,
    Query(filter): Query<ListFilter>,
) -> Result<Json<Value>, AdminError> {
    let page = Page::new(filter.limit, filter.offset)?;
    let pattern = like_pattern(filter.q.as_deref());
    let (items, total) = store::list(
        &state.db,
        filter.kind.map(Kind::as_str),
        pattern.as_deref(),
        page.limit,
        page.offset,
    )
    .await?;
    Ok(Json(json!({
        "items": items,
        "total": total,
        "keys": state.drafts.keys(),
    })))
}

async fn remove(
    _: Authorized,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AdminError> {
    if store::delete(&state.db, id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AdminError::NotFound)
    }
}

async fn one(
    _: Authorized,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<StoredDraft>, AdminError> {
    state
        .drafts
        .get(id)
        .await?
        .map(Json)
        .ok_or(AdminError::NotFound)
}

fn to_json(value: &impl serde::Serialize) -> Result<Value, AdminError> {
    serde_json::to_value(value).map_err(|e| AdminError::Invalid(e.to_string()))
}
