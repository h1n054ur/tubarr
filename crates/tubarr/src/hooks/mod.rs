//! `POST /hooks/sonarr`: Sonarr's Connect webhook, which keeps the audio only versions of episodes
//! in step with the files Sonarr imports, renames and deletes.
//!
//! For a show or daily show with `audio_version` on, every imported episode `X.strm` that holds a
//! Tubarr play link gets `X - Audio only.strm` next to it, holding the same episode's
//! `/play/<ids>/audio` link. Jellyfin 10.11 does not group episode files by name, so with
//! `[jellyfin]` set each change also queues a merge or split there ([`crate::jellyfin`]), and the
//! label shows as a version to pick. Payloads follow Sonarr 4's webhook schema.
//!
//! With `episode_thumbnails` on, each such file also gets the video's YouTube thumbnail as
//! `X-thumb.jpg`, see [`thumbs`]. A thumbnail goes when its episode file does, and moves with a
//! rename.

pub mod library;
pub mod thumbs;

use std::path::PathBuf;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    AppState,
    api::constant_time_eq,
    arr::{ArrError, Sonarr},
    config::{ArrConnection, Config},
    jellyfin::Jellyfin,
    store::{Mapping, StoreError, VersionAction},
};
pub use library::{Change, LABEL, Library, audio_path};
pub use thumbs::{ThumbChange, Thumbs, thumb_path};

#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("{0} is outside hooks.library_paths, left alone")]
    OutsideLibrary(String),
    #[error("cannot change {0}: {1}")]
    Io(String, String),
    #[error("hooks.library_paths is empty, so Tubarr touches no library file")]
    NoLibrary,
    #[error("no [sonarr] connection is set")]
    NoSonarr,
    #[error(transparent)]
    Arr(#[from] ArrError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

pub fn router() -> Router<AppState> {
    Router::new().route("/hooks/sonarr", post(sonarr))
}

/// The fields of Sonarr's webhook payloads Tubarr reads. Every event shares `eventType`, the rest
/// depends on it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    event_type: String,
    #[serde(default)]
    series: Option<HookSeries>,
    /// Download and EpisodeFileDelete.
    #[serde(default)]
    episode_file: Option<HookFile>,
    /// Download of several files at once.
    #[serde(default)]
    episode_files: Vec<HookFile>,
    /// Download: the files an upgrade replaced. SeriesDelete: whether the folder went too (a
    /// bool), not read.
    #[serde(default)]
    deleted_files: Value,
    #[serde(default)]
    renamed_episode_files: Vec<RenamedFile>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HookSeries {
    id: i64,
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HookFile {
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenamedFile {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    previous_path: Option<String>,
}

fn reply(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

fn authorized(config: &Config, headers: &HeaderMap) -> Option<bool> {
    let expected = config.hooks.sonarr_token.as_ref()?;
    let given = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    Some(constant_time_eq(
        expected.expose().as_bytes(),
        given.trim().as_bytes(),
    ))
}

async fn sonarr(State(state): State<AppState>, headers: HeaderMap, body: String) -> Response {
    match authorized(&state.config, &headers) {
        None => {
            return reply(
                StatusCode::NOT_FOUND,
                json!({ "error": "hooks.sonarr_token_file is not set" }),
            );
        }
        Some(false) => {
            return reply(StatusCode::UNAUTHORIZED, json!({ "error": "bad token" }));
        }
        Some(true) => {}
    }
    let payload: Payload = match serde_json::from_str(&body) {
        Ok(p) => p,
        Err(e) => {
            return reply(
                StatusCode::BAD_REQUEST,
                json!({ "error": format!("not a Sonarr webhook payload: {e}") }),
            );
        }
    };
    match handle(&state, payload).await {
        Ok(changes) => reply(StatusCode::OK, changes),
        Err(e @ HookError::OutsideLibrary(_)) => {
            tracing::warn!(error = %e, "Sonarr webhook refused");
            reply(StatusCode::BAD_REQUEST, json!({ "error": e.to_string() }))
        }
        Err(e) => {
            tracing::warn!(error = %e, "Sonarr webhook failed");
            reply(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "error": e.to_string() }),
            )
        }
    }
}

/// Files in the order they are settled, each with whether its audio version should exist.
type Plan = Vec<(PathBuf, bool)>;

async fn handle(state: &AppState, payload: Payload) -> Result<Value, HookError> {
    let event = payload.event_type.as_str();
    let library = Library {
        paths: &state.config.hooks.library_paths,
        public_url: &state.config.public_url,
    };
    let Some(series) = payload.series.as_ref().filter(|_| {
        matches!(
            event,
            "Download" | "Rename" | "EpisodeFileDelete" | "SeriesDelete"
        )
    }) else {
        // Test, Grab, Health and the rest.
        return Ok(json!({ "event": event, "ignored": true }));
    };
    if library.paths.is_empty() {
        return Err(HookError::NoLibrary);
    }
    let mapping = state
        .db
        .mapping_by_sonarr_id(series.id)
        .await?
        .filter(|m| m.active);
    let on = mapping.as_ref().is_some_and(|m| m.audio_version);
    let thumbs_on = mapping.as_ref().is_some_and(|m| m.episode_thumbnails);

    if event == "SeriesDelete" {
        let Some(folder) = &series.path else {
            return Ok(json!({ "event": event, "removed": 0 }));
        };
        let folder = library.local(folder)?;
        let removed = library.clear(&folder).await?;
        return Ok(json!({ "event": event, "removed": removed }));
    }

    // Every path is mapped before any file changes, so a refused one leaves the library as it was.
    let mut plan: Plan = Vec::new();
    let mut add = |path: &Option<String>, on: bool| -> Result<(), HookError> {
        if let Some(path) = path {
            plan.push((library.local(path)?, on));
        }
        Ok(())
    };
    let mut renames = Vec::new();
    match event {
        "Download" => {
            // Replaced files first, so an upgrade under the same name ends written, not removed.
            if let Value::Array(deleted) = &payload.deleted_files {
                for file in deleted {
                    let path = file.get("path").and_then(Value::as_str).map(str::to_owned);
                    add(&path, on)?;
                }
            }
            if let Some(file) = &payload.episode_file {
                add(&file.path, on)?;
            }
            for file in &payload.episode_files {
                add(&file.path, on)?;
            }
        }
        "Rename" => {
            for file in &payload.renamed_episode_files {
                add(&file.previous_path, on)?;
                add(&file.path, on)?;
                if let (Some(old), Some(new)) = (&file.previous_path, &file.path) {
                    renames.push((library.local(old)?, library.local(new)?));
                }
            }
        }
        // The main file is gone, so settling removes its audio version. One imported again under
        // the same name keeps it.
        _ => {
            if let Some(file) = &payload.episode_file {
                add(&file.path, on)?;
            }
        }
    }
    for (old, new) in &renames {
        thumbs::follow_rename(old, new).await;
    }
    let (mut written, mut removed, mut thumbnails) = (0, 0, 0);
    for (path, on) in plan {
        let change = library.settle(&path, on).await?;
        match change {
            Change::Written => written += 1,
            Change::Removed => removed += 1,
            Change::Kept | Change::Unchanged => {}
        }
        tell_jellyfin(state.jellyfin.as_deref(), &path, change).await;
        if state.thumbs.settle(&library, &path, thumbs_on).await == ThumbChange::Written {
            thumbnails += 1;
        }
    }
    Ok(json!({
        "event": event,
        "written": written,
        "removed": removed,
        "thumbnails": thumbnails,
    }))
}

/// Queues the Jellyfin merge of an audio only file that is in place, or the split of one just
/// removed. A renamed episode gets both: the split of its old path, the merge of its new one.
async fn tell_jellyfin(jellyfin: Option<&Jellyfin>, main: &std::path::Path, change: Change) {
    let Some(jellyfin) = jellyfin else {
        return;
    };
    match change {
        Change::Written | Change::Kept => jellyfin.queue(main, VersionAction::Merge).await,
        Change::Removed => jellyfin.queue(main, VersionAction::Split).await,
        Change::Unchanged => {}
    }
}

/// Writes (on) or removes (off) the audio only versions of every episode file Sonarr has for
/// `mapping`, after its `audio_version` changed, and queues their Jellyfin merges or splits.
/// Returns how many files changed. A mapping made by hand has no Sonarr series and so no files.
pub async fn backfill(
    config: &Config,
    sonarr: Option<&ArrConnection>,
    jellyfin: Option<&Jellyfin>,
    mapping: &Mapping,
) -> Result<usize, HookError> {
    let library = library(config);
    let on = mapping.active && mapping.audio_version;
    let mut changed = 0;
    for local in episode_files(config, sonarr, mapping).await? {
        let change = library.settle(&local, on).await?;
        if matches!(change, Change::Written | Change::Removed) {
            changed += 1;
        }
        tell_jellyfin(jellyfin, &local, change).await;
    }
    Ok(changed)
}

/// How many episode thumbnails a [`backfill_thumbnails`] wrote, and how many it could not.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct ThumbCount {
    pub written: usize,
    pub failed: usize,
}

/// Fetches the thumbnails missing next to every episode file Sonarr has for `mapping`, after
/// `episode_thumbnails` was turned on or to catch up on failed ones. Does nothing with the option
/// off: thumbnails in place stay.
pub async fn backfill_thumbnails(
    config: &Config,
    sonarr: Option<&ArrConnection>,
    thumbs: &Thumbs,
    mapping: &Mapping,
) -> Result<ThumbCount, HookError> {
    let mut count = ThumbCount::default();
    if !(mapping.active && mapping.episode_thumbnails) {
        return Ok(count);
    }
    let library = library(config);
    for local in episode_files(config, sonarr, mapping).await? {
        match thumbs.settle(&library, &local, true).await {
            ThumbChange::Written => count.written += 1,
            ThumbChange::Failed => count.failed += 1,
            ThumbChange::Removed | ThumbChange::Kept | ThumbChange::Unchanged => {}
        }
    }
    Ok(count)
}

fn library(config: &Config) -> Library<'_> {
    Library {
        paths: &config.hooks.library_paths,
        public_url: &config.public_url,
    }
}

/// Where the episode files the connected `sonarr` has for `mapping` are in Tubarr's container.
/// Files outside the library are logged and skipped. A mapping made by hand has no Sonarr series
/// and so no files.
async fn episode_files(
    config: &Config,
    sonarr: Option<&ArrConnection>,
    mapping: &Mapping,
) -> Result<Vec<PathBuf>, HookError> {
    let Some(series_id) = mapping.arr_id else {
        return Ok(Vec::new());
    };
    let connection = sonarr.ok_or(HookError::NoSonarr)?;
    if config.hooks.library_paths.is_empty() {
        return Err(HookError::NoLibrary);
    }
    let library = library(config);
    let files = Sonarr::new(connection)?.episode_files(series_id).await?;
    Ok(files
        .iter()
        .filter_map(|f| f.path.as_deref())
        .filter_map(|path| match library.local(path) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!(error = %e, "episode file skipped");
                None
            }
        })
        .collect())
}
