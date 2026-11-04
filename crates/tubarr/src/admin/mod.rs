//! `/admin/api`: the JSON API behind the web UI, and the embedded UI itself ([`ui`]).
//!
//! Every call needs a signed-in session or the key Sonarr and Radarr use for `/api`, sent as
//! `X-Api-Key` (see [`crate::auth`] for the rules, CSRF included).

mod arr_connections;
mod dry_run;
mod mappings;
pub(crate) mod paging;
mod playability;
mod system;
mod transfer;
pub mod ui;
mod unmatched;

use axum::{
    Json, Router,
    extract::FromRequestParts,
    http::{StatusCode, request::Parts},
    response::{IntoResponse, Response},
    routing::{any, delete, get, post},
};
use serde_json::json;

use crate::{AppState, auth, store::StoreError};

/// The header the UI sends the key in.
pub const KEY_HEADER: &str = "x-api-key";

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/admin/api/mappings",
            get(mappings::list).post(mappings::create),
        )
        .route("/admin/api/mappings/export", get(transfer::export))
        .route("/admin/api/mappings/import", post(transfer::import))
        .route(
            "/admin/api/mappings/{id}",
            get(mappings::one).patch(mappings::patch),
        )
        .route("/admin/api/mappings/{id}/test", post(mappings::test))
        .route(
            "/admin/api/mappings/{id}/audio-versions",
            post(mappings::audio_versions),
        )
        .route(
            "/admin/api/mappings/{id}/thumbnails",
            post(mappings::thumbnails),
        )
        .route(
            "/admin/api/mappings/{id}/aliases",
            get(system::aliases).put(system::set_aliases),
        )
        .route("/admin/api/unmatched", get(unmatched::list))
        .route("/admin/api/unmatched/{id}/ignore", post(unmatched::ignore))
        .route(
            "/admin/api/pins",
            get(unmatched::pins).post(unmatched::create_pin),
        )
        .route("/admin/api/pins/{id}", delete(unmatched::delete_pin))
        .route("/admin/api/unplayable", get(playability::list))
        .route(
            "/admin/api/unplayable/{video_id}",
            delete(playability::clear),
        )
        .route("/admin/api/releases/check", get(playability::releases))
        .route("/admin/api/activity", get(system::activity))
        .route("/admin/api/activity/events", get(system::events))
        .route("/admin/api/binaries/check", post(system::check_binaries))
        .route(
            "/admin/api/binaries/{tool}/rollback",
            post(system::rollback),
        )
        .route("/admin/api/settings", get(system::settings))
        .route("/admin/api/arr/connections", get(arr_connections::list))
        .route(
            "/admin/api/arr/{app}/connection",
            axum::routing::put(arr_connections::save).delete(arr_connections::remove),
        )
        .route("/admin/api/arr/{app}/test", post(arr_connections::test))
        .route("/admin/api/arr/{app}/wiring", post(arr_connections::wiring))
        .route("/admin/api/arr/push-released", post(system::push_released))
        .merge(crate::drafts::router())
        .route("/admin/api/{*rest}", any(not_found))
}

async fn not_found() -> AdminError {
    AdminError::NotFound
}

/// Proof that the request is allowed in. Handlers take it as an argument.
pub struct Authorized;

impl FromRequestParts<AppState> for Authorized {
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        auth::authorize(state, &parts.method, &parts.headers, &parts.extensions)
            .await
            .map(|_| Self)
            .map_err(IntoResponse::into_response)
    }
}

/// Why an admin call failed. Every answer is JSON with an `error` message.
#[derive(Debug, thiserror::Error)]
pub enum AdminError {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Invalid(String),
    /// Something outside Tubarr's store failed, e.g. a binary rollback.
    #[error("{0}")]
    Failed(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Failed(_) => StatusCode::BAD_GATEWAY,
            Self::Store(StoreError::Duplicate(_)) => StatusCode::CONFLICT,
            Self::Store(e) => {
                tracing::error!(error = %e, "admin API store error");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        (status, Json(json!({ "error": self.to_string() }))).into_response()
    }
}
