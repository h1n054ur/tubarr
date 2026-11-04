//! A fake Jellyfin 10.11 with just what the version merges use: users, the series and episode
//! listings, items by id with their media sources, a series refresh, PlaybackInfo, MergeVersions
//! and AlternateSources. MergeVersions picks the primary like the real one: the widest probed
//! video, else the lowest item id. Like the real one, a listing with a `userId` leaves out
//! alternate versions and the libraries that user cannot see, while one with the API key alone
//! lists both, and `GET /Items/{id}` without a `userId` answers 400.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, Mutex},
};

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use serde_json::{Value, json};

pub const API_KEY: &str = "jellyfin-test-key";
pub const ADMIN: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

#[derive(Debug, Default)]
pub struct Library {
    /// Item id to path, for the episodes Jellyfin has scanned.
    pub items: BTreeMap<String, String>,
    /// Item id to path, for files on disk that a refresh of their series scans.
    pub unscanned: BTreeMap<String, String>,
    /// Series id to its folder.
    pub series: BTreeMap<String, String>,
    /// A library folder no user's view shows, like a library not shared with the administrator.
    pub hidden: Option<String>,
    /// Ids with a probed video stream (1920 wide).
    pub probed: HashSet<String>,
    /// Primary id to its alternate versions' ids.
    pub versions: BTreeMap<String, Vec<String>>,
    /// `METHOD /path` of every request, in order.
    pub calls: Vec<String>,
    /// The query of every request, in the same order.
    pub queries: Vec<String>,
}

impl Library {
    fn hidden_from(&self, user: Option<&String>, path: &str) -> bool {
        user.is_some() && self.hidden.as_ref().is_some_and(|h| path.starts_with(h))
    }
}

#[derive(Clone)]
pub struct FakeJellyfin {
    pub url: String,
    pub library: Arc<Mutex<Library>>,
}

impl FakeJellyfin {
    pub async fn start() -> Self {
        let library = Arc::new(Mutex::new(Library::default()));
        let app = Router::new()
            .route("/Users", get(users))
            .route("/Items", get(items))
            .route("/Items/{id}", get(item))
            .route("/Items/{id}/Refresh", post(refresh))
            .route("/Items/{id}/PlaybackInfo", post(playback_info))
            .route("/Videos/MergeVersions", post(merge))
            .route("/Videos/{id}/AlternateSources", delete(split))
            .layer(axum::middleware::from_fn_with_state(library.clone(), guard))
            .with_state(library.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            url: format!("http://{addr}"),
            library,
        }
    }

    pub fn add(&self, id: &str, path: &str) {
        self.lock().items.insert(id.to_owned(), path.to_owned());
    }

    pub fn add_series(&self, id: &str, folder: &str) {
        self.lock().series.insert(id.to_owned(), folder.to_owned());
    }

    pub fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }

    /// `METHOD /path?query` of every request, in order.
    pub fn requests(&self) -> Vec<String> {
        let lib = self.lock();
        lib.calls
            .iter()
            .zip(&lib.queries)
            .map(|(call, query)| format!("{call}?{query}"))
            .collect()
    }

    pub fn clear_calls(&self) {
        let mut lib = self.lock();
        lib.calls.clear();
        lib.queries.clear();
    }

    /// The ids of `id`'s media sources, primary first, as `GET /Items/{id}` shows them.
    pub fn sources(&self, id: &str) -> Vec<String> {
        let lib = self.lock();
        let mut out = vec![id.to_owned()];
        out.extend(lib.versions.get(id).cloned().unwrap_or_default());
        out
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, Library> {
        self.library.lock().unwrap()
    }
}

type Shared = State<Arc<Mutex<Library>>>;

async fn guard(
    State(lib): Shared,
    headers: HeaderMap,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let line = format!("{} {}", req.method(), req.uri().path());
    let query = req.uri().query().unwrap_or_default().to_owned();
    {
        let mut lib = lib.lock().unwrap();
        lib.calls.push(line);
        lib.queries.push(query);
    }
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !auth.contains(&format!("Token=\"{API_KEY}\"")) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    next.run(req).await
}

async fn users() -> Json<Value> {
    Json(json!([
        { "Id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "Policy": { "IsAdministrator": false } },
        { "Id": ADMIN, "Policy": { "IsAdministrator": true } },
    ]))
}

/// Series, episodes (all, or those under `parentId`, a series), or the episodes `ids` names
/// with their media sources.
async fn items(State(lib): Shared, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let lib = lib.lock().unwrap();
    let user = q.get("userId");
    if let Some(ids) = q.get("ids") {
        let sources = q
            .get("fields")
            .is_some_and(|f| f.split(',').any(|f| f == "MediaSources"));
        let items: Vec<Value> = ids
            .split(',')
            .filter_map(|id| {
                let path = lib.items.get(id)?;
                (!lib.hidden_from(user, path)).then(|| episode(&lib, id, path, sources))
            })
            .collect();
        return Json(json!({ "Items": items }));
    }
    let (kind, listed) = match q.get("includeItemTypes").map(String::as_str) {
        Some("Series") => ("Series", &lib.series),
        Some("Episode") => ("Episode", &lib.items),
        other => panic!("the fake lists series or episodes, not {other:?}"),
    };
    // An unknown parent lists nothing.
    let folder = q
        .get("parentId")
        .map(|p| lib.series.get(p).map(|f| format!("{f}/")));
    let alternates: HashSet<&String> = lib.versions.values().flatten().collect();
    let items: Vec<Value> = listed
        .iter()
        .filter(|(id, path)| {
            !lib.hidden_from(user, path)
                && !(user.is_some() && alternates.contains(id))
                && match &folder {
                    None => true,
                    Some(f) => f.as_ref().is_some_and(|f| path.starts_with(f)),
                }
        })
        .map(|(id, path)| json!({ "Id": id, "Path": path, "Type": kind }))
        .collect();
    Json(json!({ "Items": items }))
}

fn episode(lib: &Library, id: &str, path: &str, sources: bool) -> Value {
    let mut item = json!({ "Id": id, "Path": path, "Type": "Episode" });
    if sources {
        let mut list = vec![json!({ "Id": id, "Path": path, "Name": "Episode" })];
        for alt in lib.versions.get(id).into_iter().flatten() {
            list.push(json!({ "Id": alt, "Path": lib.items[alt], "Name": "Audio only" }));
        }
        item["MediaSources"] = Value::Array(list);
    }
    item
}

/// 10.11 routes this to the user library controller, which needs a `userId`.
async fn item(
    State(lib): Shared,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Response {
    let Some(user) = q.get("userId") else {
        return (StatusCode::BAD_REQUEST, "Guid can't be empty").into_response();
    };
    let lib = lib.lock().unwrap();
    let Some(path) = lib
        .items
        .get(&id)
        .filter(|p| !lib.hidden_from(Some(user), p))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(episode(&lib, &id, path, true)).into_response()
}

/// Scans the unscanned files under series `id`'s folder at once (the real one queues it).
async fn refresh(State(lib): Shared, Path(id): Path<String>) -> StatusCode {
    let mut lib = lib.lock().unwrap();
    let Some(folder) = lib.series.get(&id).map(|f| format!("{f}/")) else {
        return StatusCode::NOT_FOUND;
    };
    let found: Vec<(String, String)> = lib
        .unscanned
        .iter()
        .filter(|(_, path)| path.starts_with(&folder))
        .map(|(id, path)| (id.clone(), path.clone()))
        .collect();
    for (id, path) in found {
        lib.unscanned.remove(&id);
        lib.items.insert(id, path);
    }
    StatusCode::NO_CONTENT
}

async fn playback_info(State(lib): Shared, Path(id): Path<String>) -> Response {
    let mut lib = lib.lock().unwrap();
    if !lib.items.contains_key(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    lib.probed.insert(id);
    Json(json!({ "MediaSources": [] })).into_response()
}

async fn merge(State(lib): Shared, Query(q): Query<HashMap<String, String>>) -> StatusCode {
    let mut lib = lib.lock().unwrap();
    let mut ids: Vec<String> = q["ids"].split(',').map(str::to_owned).collect();
    ids.sort();
    let primary = ids
        .iter()
        .find(|i| lib.probed.contains(*i))
        .unwrap_or(&ids[0])
        .clone();
    let rest = ids.into_iter().filter(|i| *i != primary).collect();
    lib.versions.insert(primary, rest);
    StatusCode::NO_CONTENT
}

/// Like 10.11, fails on an item with no versions (it parses a missing primary version id).
async fn split(State(lib): Shared, Path(id): Path<String>) -> StatusCode {
    let mut lib = lib.lock().unwrap();
    if !lib.items.contains_key(&id) {
        return StatusCode::NOT_FOUND;
    }
    match lib.versions.remove(&id) {
        Some(_) => StatusCode::NO_CONTENT,
        None => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
