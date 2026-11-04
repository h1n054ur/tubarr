//! The Settings page's Sonarr and Radarr connections: list, save after a test, remove, test, and
//! the wiring check. API keys never leave the process: answers say only whether one is set.

use axum::{
    Json,
    extract::{Path, State},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{AdminError, Authorized};
use crate::{
    AppState,
    arr::{
        App, Radarr, Sonarr,
        connections::{self, Expected},
    },
    config::{ArrConnection, ConnectionSource, Secret},
    store::SavedArr,
};

fn app_from(path: &str) -> Result<App, AdminError> {
    App::from_key(path).ok_or(AdminError::NotFound)
}

fn view(state: &AppState, app: App) -> Value {
    match state.arrs.get(app) {
        Some(conn) => json!({
            "source": conn.source,
            "url": conn.url,
            "api_key": "set",
        }),
        None => json!({
            "source": null,
            "url": null,
            "api_key": "not set",
        }),
    }
}

/// `GET /admin/api/arr/connections`: where each arr's connection comes from, never the key.
pub async fn list(_: Authorized, State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "sonarr": view(&state, App::Sonarr),
        "radarr": view(&state, App::Radarr),
    }))
}

#[derive(Debug, Deserialize)]
pub struct SaveBody {
    url: String,
    /// Left out or empty keeps the key already saved.
    #[serde(default)]
    api_key: Option<String>,
}

fn in_config(app: App) -> AdminError {
    AdminError::Invalid(format!(
        "{app} is set in the config file ([{}]): change it there",
        app.config_key()
    ))
}

/// `PUT /admin/api/arr/{app}/connection`: tests the URL and key against the arr's system status
/// and saves them only when the arr answers. The connection is in use at once: the arr sync runs
/// right away. Refused while the config file sets this arr.
pub async fn save(
    _: Authorized,
    State(state): State<AppState>,
    Path(app): Path<String>,
    Json(body): Json<SaveBody>,
) -> Result<Json<Value>, AdminError> {
    let app = app_from(&app)?;
    if state.arrs.from_file(app) {
        return Err(in_config(app));
    }
    let url = connections::parse_url(&body.url).map_err(AdminError::Invalid)?;
    let typed = body
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|k| !k.is_empty());
    let api_key = match typed {
        Some(key) => key.to_owned(),
        None => match state.db.saved_arr(app.config_key()).await? {
            Some(saved) => saved.api_key,
            None => {
                return Err(AdminError::Invalid(format!(
                    "Enter the API key from {app}, Settings > General"
                )));
            }
        },
    };
    let conn = ArrConnection {
        url,
        api_key: Secret::new(api_key),
        source: ConnectionSource::Saved,
    };
    // Test error messages never hold the API key.
    let version = connections::test(app, &conn)
        .await
        .map_err(|e| AdminError::Failed(format!("Not saved: {e}")))?;
    state
        .db
        .save_arr(
            app.config_key(),
            &SavedArr {
                url: conn.url.to_string(),
                api_key: conn.api_key.expose().to_owned(),
            },
        )
        .await?;
    state.arrs.set(app, Some(conn));
    let mut answer = view(&state, app);
    answer["version"] = json!(version);
    Ok(Json(answer))
}

/// `DELETE /admin/api/arr/{app}/connection`: forgets the saved connection and stops using it.
pub async fn remove(
    _: Authorized,
    State(state): State<AppState>,
    Path(app): Path<String>,
) -> Result<Json<Value>, AdminError> {
    let app = app_from(&app)?;
    if state.arrs.from_file(app) {
        return Err(in_config(app));
    }
    if !state.db.forget_arr(app.config_key()).await? {
        return Err(AdminError::NotFound);
    }
    state.arrs.set(app, None);
    Ok(Json(view(&state, app)))
}

fn not_set(app: App) -> AdminError {
    AdminError::Invalid(format!("{app} is not connected"))
}

/// `POST /admin/api/arr/{app}/test`: looks up the Tubarr tag, which proves the URL, the key and
/// the tag in one call. A failed test is a normal answer (`ok: false`), not an HTTP error.
pub async fn test(
    _: Authorized,
    State(state): State<AppState>,
    Path(app): Path<String>,
) -> Result<Json<Value>, AdminError> {
    let app = app_from(&app)?;
    let conn = state.arrs.get(app).ok_or_else(|| not_set(app))?;
    let tag = &state.config.arr.tag;
    let result = match app {
        App::Sonarr => match Sonarr::new(&conn) {
            Ok(s) => s.tag_id(tag).await,
            Err(e) => Err(e),
        },
        App::Radarr => match Radarr::new(&conn) {
            Ok(r) => r.tag_id(tag).await,
            Err(e) => Err(e),
        },
    };
    // ArrError messages never hold the API key.
    Ok(Json(match result {
        Ok(id) => json!({ "ok": true, "tag": tag, "tag_id": id }),
        Err(e) => json!({ "ok": false, "tag": tag, "error": e.to_string() }),
    }))
}

/// `POST /admin/api/arr/{app}/wiring`: what Tubarr needs in the arr and whether each is there.
/// Read only. An arr that cannot be read is a normal answer (`ok: false`).
pub async fn wiring(
    _: Authorized,
    State(state): State<AppState>,
    Path(app): Path<String>,
) -> Result<Json<Value>, AdminError> {
    let app = app_from(&app)?;
    let conn = state.arrs.get(app).ok_or_else(|| not_set(app))?;
    let expected = Expected::from_config(&state.config);
    Ok(Json(
        match connections::wiring(app, &conn, &expected).await {
            Ok(items) => json!({ "ok": true, "items": items }),
            Err(e) => json!({ "ok": false, "error": e.to_string() }),
        },
    ))
}
