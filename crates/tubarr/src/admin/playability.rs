//! `/admin/api/unplayable` and `/admin/api/releases/check`: videos Tubarr cannot serve, and the
//! releases (imported ones included) whose video is unplayable or was never looked up, so dead
//! `.strm` files can be found and cleaned up.

use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
};

use super::{AdminError, Authorized};
use crate::{
    AppState,
    store::{ReleaseCheck, UnplayableVideo},
};

pub async fn list(
    _: Authorized,
    State(state): State<AppState>,
) -> Result<Json<Vec<UnplayableVideo>>, AdminError> {
    Ok(Json(state.db.unplayable_videos().await?))
}

/// Drops the mark so the matcher considers the video again and the next run looks it up.
pub async fn clear(
    _: Authorized,
    State(state): State<AppState>,
    Path(video_id): Path<String>,
) -> Result<StatusCode, AdminError> {
    if state.db.clear_unplayable(&video_id).await? {
        state.matcher.waker().notify_one();
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AdminError::NotFound)
    }
}

pub async fn releases(
    _: Authorized,
    State(state): State<AppState>,
) -> Result<Json<Vec<ReleaseCheck>>, AdminError> {
    Ok(Json(state.db.release_checks().await?))
}
