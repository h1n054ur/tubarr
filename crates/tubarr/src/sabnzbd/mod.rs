//! The SABnzbd download client API Sonarr and Radarr talk to: every `/api` call with a `mode=`.
//!
//! A grab never downloads the video. `addfile` reads the video id from the NZB, writes the `.strm`
//! and its captions at once and records the job as completed, so the queue stays empty and history
//! has the result. The video's sample tables are then read in the background for its first play.
//! The `/api` dispatch checks the API key before a request gets here.

mod nzb;
mod playable;
pub(crate) mod strm;

use std::{collections::HashMap, path::Path};

use axum::{
    Json,
    extract::{FromRequest, Multipart, Query, Request},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

pub use nzb::{NzbError, NzbInfo};
pub use strm::{GrabWriter, StrmError, StrmJob, StrmWriter};

use crate::{
    AppState,
    config::Config,
    health::HealthEvent,
    store::{Grab, GrabStatus, NewGrab, StoreError},
};

/// The SABnzbd version Tubarr claims. Sonarr 4 and Radarr 6 accept it (tested live).
const VERSION: &str = "4.5.1";

/// The categories Tubarr offers and their folders under `complete_dir`. `*` is SABnzbd's default.
const CATEGORIES: [(&str, &str); 3] = [("*", ""), ("tv", "tv"), ("movies", "movies")];

/// Why a call failed. The text is sent back as SABnzbd's `error`.
#[derive(Debug, thiserror::Error)]
pub enum SabError {
    #[error("not implemented")]
    NotImplemented,
    #[error("invalid query: {0}")]
    Query(String),
    #[error("no NZB file in the request")]
    NoFile,
    #[error("cannot read the upload: {0}")]
    Upload(String),
    #[error(transparent)]
    Nzb(#[from] NzbError),
    #[error(transparent)]
    Write(#[from] StrmError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("cannot delete {path}: {source}")]
    Delete {
        path: String,
        source: std::io::Error,
    },
}

/// SABnzbd's answer to a missing or wrong key. Sonarr and Radarr look for these exact texts to
/// tell the user the key is wrong rather than report a broken client.
pub fn key_refused(key_given: bool) -> Response {
    let error = if key_given {
        "API Key Incorrect"
    } else {
        "API Key Required"
    };
    Json(json!({ "status": false, "error": error })).into_response()
}

/// Answers one SABnzbd call. The whole request comes in because `addfile` carries the NZB as a
/// multipart body.
pub async fn handle(state: &AppState, request: Request) -> Response {
    let result = match Query::<HashMap<String, String>>::try_from_uri(request.uri()) {
        Ok(Query(params)) => dispatch(state, &params, request).await,
        Err(e) => Err(SabError::Query(e.body_text())),
    };
    match result {
        Ok(body) => Json(body).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "SABnzbd call failed");
            Json(json!({ "status": false, "error": e.to_string() })).into_response()
        }
    }
}

async fn dispatch(
    state: &AppState,
    params: &HashMap<String, String>,
    request: Request,
) -> Result<Value, SabError> {
    let param = |key: &str| params.get(key).map(String::as_str);
    let config = &state.config;
    match param("mode").unwrap_or_default() {
        "version" => Ok(json!({ "version": VERSION })),
        "get_config" => Ok(get_config(config)),
        "fullstatus" => Ok(json!({ "status": { "completedir": config.complete_dir } })),
        "addfile" => {
            let multipart = Multipart::from_request(request, &())
                .await
                .map_err(|_| SabError::NoFile)?;
            let upload = Upload::read(multipart, params).await?;
            add_file(state, &GrabWriter::new(state), upload).await
        }
        "queue" if param("name") == Some("delete") => delete(state, params).await,
        "queue" => Ok(json!({
            "queue": { "paused": false, "my_home": config.complete_dir, "slots": [] }
        })),
        "history" if param("name") == Some("delete") => delete(state, params).await,
        "history" => history(state, params).await,
        // Nothing ever runs, so there is nothing to pause.
        "pause" | "resume" => Ok(json!({ "status": true })),
        _ => Err(SabError::NotImplemented),
    }
}

fn get_config(config: &Config) -> Value {
    let categories: Vec<Value> = CATEGORIES
        .iter()
        .enumerate()
        .map(|(order, (name, dir))| {
            // Mirrors a stock SABnzbd: the default category post-processes, the others inherit.
            let (pp, script, priority) = if *name == "*" {
                ("3", "None", 0)
            } else {
                ("", "Default", -100)
            };
            json!({
                "name": name, "order": order, "pp": pp, "script": script,
                "dir": dir, "priority": priority,
            })
        })
        .collect();
    json!({
        "config": {
            "misc": {
                "complete_dir": config.complete_dir,
                "tv_categories": [],
                "enable_tv_sorting": false,
                "movie_categories": [],
                "enable_movie_sorting": false,
                "date_categories": [],
                "enable_date_sorting": false,
                "pre_check": false,
                "history_retention": "",
                "history_retention_option": "all",
                "history_retention_number": 0,
            },
            "categories": categories,
            "servers": [],
            "sorters": [],
        }
    })
}

/// An `addfile` call: the NZB plus `cat` and `nzbname`, from the query or from form fields.
#[derive(Debug)]
pub struct Upload {
    pub nzb: Vec<u8>,
    pub file_name: Option<String>,
    pub category: Option<String>,
    pub nzbname: Option<String>,
}

impl Upload {
    async fn read(
        mut multipart: Multipart,
        params: &HashMap<String, String>,
    ) -> Result<Self, SabError> {
        let mut upload = Upload {
            nzb: Vec::new(),
            file_name: None,
            category: params.get("cat").cloned(),
            nzbname: params.get("nzbname").cloned(),
        };
        let mut found = false;
        let bad = |e: axum::extract::multipart::MultipartError| SabError::Upload(e.body_text());
        while let Some(field) = multipart.next_field().await.map_err(bad)? {
            match field.name() {
                // SABnzbd takes the file as `name`, older clients send `nzbfile`.
                Some("name" | "nzbfile") => {
                    upload.file_name = field.file_name().map(str::to_owned);
                    upload.nzb = field.bytes().await.map_err(bad)?.to_vec();
                    found = true;
                }
                Some("cat") => upload.category = Some(field.text().await.map_err(bad)?),
                Some("nzbname") => upload.nzbname = Some(field.text().await.map_err(bad)?),
                _ => {}
            }
        }
        if found {
            Ok(upload)
        } else {
            Err(SabError::NoFile)
        }
    }
}

/// Writes the grab's files with `writer` and records it as completed.
pub async fn add_file<W: StrmWriter>(
    state: &AppState,
    writer: &W,
    upload: Upload,
) -> Result<Value, SabError> {
    let info = nzb::parse(&upload.nzb)?;
    let release_name = nzb::release_name(
        upload.nzbname.as_deref().filter(|n| !n.is_empty()),
        &info,
        upload.file_name.as_deref(),
    );
    // An unknown category falls back to the default, like SABnzbd.
    let (category, category_dir) = CATEGORIES
        .iter()
        .find(|(name, _)| upload.category.as_deref() == Some(*name))
        .copied()
        .unwrap_or(CATEGORIES[0]);
    if let Some(reason) = playable::check(state, &info.video_id).await {
        return failed_grab(state, release_name, info.video_id, category, reason).await;
    }
    let storage = writer
        .write(StrmJob {
            release_name: &release_name,
            video_id: &info.video_id,
            category_dir,
        })
        .await?;
    let grab = state
        .db
        .insert_grab(&NewGrab {
            nzo_id: new_nzo_id(),
            release_name,
            video_id: info.video_id,
            category: category.to_owned(),
            status: GrabStatus::Completed,
            storage_path: storage.display().to_string(),
            fail_message: None,
        })
        .await?;
    tracing::info!(
        nzo_id = %grab.nzo_id, release = %grab.release_name, video = %grab.video_id,
        storage = %grab.storage_path, "grab written"
    );
    state.play.prebuild(&grab.video_id).await;
    state.health.events().publish(HealthEvent::GrabCompleted {
        video_id: grab.video_id,
        release_name: grab.release_name,
        category: grab.category,
    });
    Ok(json!({ "status": true, "nzo_ids": [grab.nzo_id] }))
}

/// Records a grab that wrote nothing because its video cannot play. Sonarr and Radarr read the
/// Failed history slot, blocklist the release and search for another.
async fn failed_grab(
    state: &AppState,
    release_name: String,
    video_id: String,
    category: &str,
    reason: String,
) -> Result<Value, SabError> {
    let grab = state
        .db
        .insert_grab(&NewGrab {
            nzo_id: new_nzo_id(),
            release_name,
            video_id,
            category: category.to_owned(),
            status: GrabStatus::Failed,
            storage_path: String::new(),
            fail_message: Some(reason),
        })
        .await?;
    tracing::warn!(
        nzo_id = %grab.nzo_id, release = %grab.release_name, video = %grab.video_id,
        reason = grab.fail_message.as_deref().unwrap_or_default(), "grab failed, video cannot play"
    );
    Ok(json!({ "status": true, "nzo_ids": [grab.nzo_id] }))
}

/// `SABnzbd_nzo_` plus 16 random characters, the shape SABnzbd itself uses.
fn new_nzo_id() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let suffix: String = (0..16)
        .map(|_| char::from(ALPHABET[rand::random_range(0..ALPHABET.len())]))
        .collect();
    format!("SABnzbd_nzo_{suffix}")
}

/// Most slots one history answer holds. Sonarr and Radarr ask for their
/// `DownloadClientHistoryLimit`, 60 unless changed, and only look for the grabs they made, which
/// are minutes old.
pub const HISTORY_MAX: i64 = 500;

async fn history(state: &AppState, params: &HashMap<String, String>) -> Result<Value, SabError> {
    let number = |key: &str| {
        params
            .get(key)
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|n| *n >= 0)
            .unwrap_or(0)
    };
    let category = params
        .get("category")
        .map(String::as_str)
        .filter(|c| !c.is_empty() && *c != "*");
    let (start, limit) = (number("start"), number("limit"));
    // limit 0 means everything in SABnzbd. Here it means the newest HISTORY_MAX, the most any
    // page gets, since each slot reads its folder.
    let fetch = if limit == 0 {
        HISTORY_MAX
    } else {
        limit.min(HISTORY_MAX)
    };
    let grabs = state.db.grabs(category, fetch, start).await?;
    let mut slots = Vec::new();
    for grab in &grabs {
        slots.push(history_slot(grab).await);
    }
    Ok(json!({ "history": { "paused": false, "slots": slots } }))
}

async fn history_slot(grab: &Grab) -> Value {
    let (status, fail_message) = match grab.status {
        GrabStatus::Completed => ("Completed", ""),
        GrabStatus::Failed => (
            "Failed",
            grab.fail_message
                .as_deref()
                .unwrap_or("Tubarr could not write the .strm file"),
        ),
    };
    json!({
        "nzo_id": grab.nzo_id,
        "name": grab.release_name,
        "nzb_name": format!("{}.nzb", grab.release_name),
        "category": grab.category,
        "status": status,
        "bytes": folder_bytes(Path::new(&grab.storage_path)).await,
        "download_time": 0,
        "storage": grab.storage_path,
        "fail_message": fail_message,
    })
}

/// Total size of the files in a grab's folder, 0 once the arr has moved them away.
async fn folder_bytes(folder: &Path) -> u64 {
    let Ok(mut entries) = tokio::fs::read_dir(folder).await else {
        return 0;
    };
    let mut total = 0;
    while let Ok(Some(entry)) = entries.next_entry().await {
        if let Ok(meta) = entry.metadata().await
            && meta.is_file()
        {
            total += meta.len();
        }
    }
    total
}

/// `name=delete&value=<nzo id>[,<nzo id>...]`: drops the records and, with `del_files=1`, their
/// folders. Unknown ids are skipped, like SABnzbd.
async fn delete(state: &AppState, params: &HashMap<String, String>) -> Result<Value, SabError> {
    let del_files = params.get("del_files").is_some_and(|v| v == "1");
    let ids = params.get("value").map(String::as_str).unwrap_or_default();
    for nzo_id in ids.split(',').map(str::trim).filter(|id| !id.is_empty()) {
        let Some(grab) = state.db.grab(nzo_id).await? else {
            continue;
        };
        if del_files {
            remove_folder(&state.config.complete_dir, Path::new(&grab.storage_path)).await?;
        }
        state.db.delete_grab(nzo_id).await?;
        tracing::info!(nzo_id, del_files, "grab deleted");
    }
    Ok(json!({ "status": true }))
}

/// Removes a grab's folder, but only one strictly inside `complete_dir`.
async fn remove_folder(complete_dir: &Path, folder: &Path) -> Result<(), SabError> {
    let inside = folder.starts_with(complete_dir)
        && folder != complete_dir
        && folder.components().all(|c| {
            matches!(
                c,
                std::path::Component::Normal(_) | std::path::Component::RootDir
            )
        });
    if !inside {
        tracing::warn!(folder = %folder.display(), "not deleting a folder outside complete_dir");
        return Ok(());
    }
    match tokio::fs::remove_dir_all(folder).await {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(SabError::Delete {
            path: folder.display().to_string(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nzo_ids_look_like_sabnzbd_and_differ() {
        let (a, b) = (new_nzo_id(), new_nzo_id());
        assert!(a.starts_with("SABnzbd_nzo_") && a.len() == 28, "{a}");
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn remove_folder_stays_inside_complete_dir() {
        let dir = tempfile::tempdir().unwrap();
        let complete = dir.path().join("complete");
        let outside = dir.path().join("outside");
        let inside = complete.join("tv/Release");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        remove_folder(&complete, &outside).await.unwrap();
        remove_folder(&complete, &complete).await.unwrap();
        remove_folder(&complete, &complete.join("tv/../../outside"))
            .await
            .unwrap();
        assert!(outside.exists() && complete.exists());

        remove_folder(&complete, &inside).await.unwrap();
        assert!(!inside.exists());
        // Already gone is fine.
        remove_folder(&complete, &inside).await.unwrap();
    }
}
