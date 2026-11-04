//! `/admin/api/mappings/export` and `/import`: every mapping with its rules, aliases and pins as
//! one JSON file, to move them to another instance or keep them apart from the database.
//!
//! The file holds what a person set up, nothing the instance learns by itself: no row ids, no
//! Sonarr or Radarr ids, no active flag, no notes, releases or caches, and no secrets. An import
//! finds an existing mapping by its TVDB id (shows and daily shows) or TMDB id (films). A match
//! gets the file's rules and user aliases, and the file's pins are added or replace the pin for
//! the same episode. Pins the file does not list stay.

use std::collections::{HashMap, HashSet};

use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderValue, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use sqlx::{SqliteConnection, types::Json as SqlJson};

use super::{
    AdminError, Authorized,
    mappings::{blank_to_none, check, clean_list, parse_kind},
    unmatched::{NewPin, target_for},
};
use crate::{
    AppState,
    arr::root_folders,
    matcher::dates::timezone_name,
    resolver::is_video_id,
    store::{
        Mapping, MappingKind, MatchOptions, PinTarget, PlaylistMode, StoreError, replace_pin,
        unix_now,
    },
};

/// The file format's version. A file from a newer Tubarr is refused rather than half read.
const FORMAT: u32 = 1;

/// The whole file.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingExport {
    pub format: u32,
    pub exported_at: i64,
    pub mappings: Vec<ExportedMapping>,
}

/// One mapping in the file. Fields left out of a hand-written file take the defaults a new
/// mapping gets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields, default)]
pub struct ExportedMapping {
    pub kind: String,
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    pub title: String,
    pub year: Option<i64>,
    pub language: String,
    /// Who set the language: `arr`, `channel` or `user`.
    pub language_source: String,
    #[sqlx(json)]
    pub channel_ids: Vec<String>,
    pub episode_regex: Option<String>,
    #[sqlx(json)]
    pub excludes: Vec<String>,
    pub min_duration_secs: Option<i64>,
    pub utc_offset_mins: Option<i64>,
    pub timezone: Option<String>,
    pub playlist_id: Option<String>,
    pub playlist_season: i64,
    pub playlist_offset: i64,
    pub playlist_mode: PlaylistMode,
    pub backfill_limit: Option<i64>,
    pub title_number: bool,
    pub audio_version: bool,
    pub episode_thumbnails: bool,
    /// The root folder override: an absolute path or `none`. Absent follows the config.
    pub root_folder: Option<String>,
    #[sqlx(skip)]
    pub aliases: Vec<ExportedAlias>,
    #[sqlx(skip)]
    pub pins: Vec<ExportedPin>,
}

impl Default for ExportedMapping {
    fn default() -> Self {
        let options = MatchOptions::default();
        Self {
            kind: String::new(),
            tvdb_id: None,
            tmdb_id: None,
            imdb_id: None,
            title: String::new(),
            year: None,
            language: String::new(),
            language_source: user_source(),
            channel_ids: vec![],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: None,
            utc_offset_mins: options.utc_offset_mins,
            timezone: options.timezone,
            playlist_id: options.playlist_id,
            playlist_season: options.playlist_season,
            playlist_offset: options.playlist_offset,
            playlist_mode: options.playlist_mode,
            backfill_limit: None,
            title_number: options.title_number,
            audio_version: false,
            episode_thumbnails: false,
            root_folder: None,
            aliases: vec![],
            pins: vec![],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct ExportedAlias {
    pub alias: String,
    /// `user`, or `arr` for one Sonarr listed. Left out means `user`.
    #[serde(default = "user_source")]
    pub source: String,
}

fn user_source() -> String {
    "user".to_owned()
}

/// A pin: season and episode for a show, the air date for a daily show, neither for a film.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::FromRow)]
#[serde(deny_unknown_fields)]
pub struct ExportedPin {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub season: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub episode: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub air_date: Option<String>,
    /// One video id, or the parts of a split upload joined by commas in play order.
    pub video_id: String,
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    #[sqlx(flatten)]
    mapping: ExportedMapping,
}

/// An alias or pin row with the mapping it belongs to.
#[derive(sqlx::FromRow)]
struct Child<T> {
    mapping_id: i64,
    #[sqlx(flatten)]
    item: T,
}

/// Every mapping with its aliases and pins, keyed by row id.
async fn exported(conn: &mut SqliteConnection) -> Result<Vec<(i64, ExportedMapping)>, StoreError> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, kind, tvdb_id, tmdb_id, imdb_id, title, year, language, language_source, \
         channel_ids, episode_regex, excludes, min_duration_secs, utc_offset_mins, timezone, \
         playlist_id, playlist_season, playlist_offset, playlist_mode, backfill_limit, \
         title_number, audio_version, episode_thumbnails, root_folder \
         FROM mappings ORDER BY kind, title, id",
    )
    .fetch_all(&mut *conn)
    .await?;
    let aliases: Vec<Child<ExportedAlias>> = sqlx::query_as(
        "SELECT mapping_id, alias, source FROM mapping_aliases ORDER BY mapping_id, alias",
    )
    .fetch_all(&mut *conn)
    .await?;
    let pins: Vec<Child<ExportedPin>> = sqlx::query_as(
        "SELECT mapping_id, season, episode, air_date, video_id FROM pins \
         ORDER BY mapping_id, season, episode, air_date",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut by_id: HashMap<i64, ExportedMapping> =
        rows.iter().map(|r| (r.id, r.mapping.clone())).collect();
    for a in aliases {
        if let Some(m) = by_id.get_mut(&a.mapping_id) {
            m.aliases.push(a.item);
        }
    }
    for p in pins {
        if let Some(m) = by_id.get_mut(&p.mapping_id) {
            m.pins.push(p.item);
        }
    }
    Ok(rows
        .into_iter()
        .filter_map(|r| by_id.remove(&r.id).map(|m| (r.id, m)))
        .collect())
}

/// `GET /admin/api/mappings/export`: the file, as a download.
pub async fn export(_: Authorized, State(state): State<AppState>) -> Result<Response, AdminError> {
    let mut conn = state.db.pool().acquire().await.map_err(StoreError::from)?;
    let mappings = exported(&mut conn)
        .await?
        .into_iter()
        .map(|(_, m)| m)
        .collect();
    let file = MappingExport {
        format: FORMAT,
        exported_at: unix_now(),
        mappings,
    };
    let mut response = Json(file).into_response();
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"tubarr-mappings.json\""),
    );
    Ok(response)
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ImportQuery {
    /// Report what would change and write nothing.
    pub dry_run: bool,
}

/// What an import did, or would do on a dry run. A mapping in `conflicts` is left as it is.
#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    pub dry_run: bool,
    pub creates: Vec<ImportItem>,
    pub updates: Vec<ImportItem>,
    pub unchanged: Vec<ImportItem>,
    pub conflicts: Vec<ImportConflict>,
}

#[derive(Debug, Serialize)]
pub struct ImportItem {
    pub title: String,
    pub kind: String,
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct ImportConflict {
    pub title: String,
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub reason: String,
}

enum Plan {
    Create(ExportedMapping),
    Update(i64, ExportedMapping),
}

/// `POST /admin/api/mappings/import[?dry_run=true]`: checks every mapping in the file, then
/// creates and updates the ones without a conflict in one transaction.
pub async fn import(
    _: Authorized,
    State(state): State<AppState>,
    Query(query): Query<ImportQuery>,
    Json(file): Json<MappingExport>,
) -> Result<Json<ImportReport>, AdminError> {
    if file.format != FORMAT {
        return Err(AdminError::Invalid(format!(
            "this file is format {}, this Tubarr reads format {FORMAT}",
            file.format
        )));
    }
    let mut tx = state.db.pool().begin().await.map_err(StoreError::from)?;
    let existing = exported(&mut tx).await?;
    let mut report = ImportReport {
        dry_run: query.dry_run,
        ..ImportReport::default()
    };
    let mut plans = Vec::new();
    // Each id may stand for one mapping only, in the file and against other rows.
    let mut claimed: HashSet<(&str, i64)> = HashSet::new();
    for entry in file.mappings {
        let conflict = |reason: String, e: &ExportedMapping| ImportConflict {
            title: e.title.clone(),
            tvdb_id: e.tvdb_id,
            tmdb_id: e.tmdb_id,
            reason,
        };
        let entry = match normalize(entry.clone()) {
            Ok(e) => e,
            Err(reason) => {
                report.conflicts.push(conflict(reason, &entry));
                continue;
            }
        };
        let ids = [("TVDB", entry.tvdb_id), ("TMDB", entry.tmdb_id)];
        if let Some((name, id)) = ids.iter().find_map(|(n, id)| {
            id.filter(|id| claimed.contains(&(*n, *id)))
                .map(|id| (n, id))
        }) {
            let reason = format!("{name} id {id} is in the file twice");
            report.conflicts.push(conflict(reason, &entry));
            continue;
        }
        let key_is_tmdb = entry.kind == "film";
        let found = existing.iter().find(|(_, m)| {
            if key_is_tmdb {
                m.tmdb_id.is_some() && m.tmdb_id == entry.tmdb_id
            } else {
                m.tvdb_id.is_some() && m.tvdb_id == entry.tvdb_id
            }
        });
        if let Some((_, m)) = found
            && m.kind != entry.kind
        {
            let reason = format!(
                "a {} with this id exists here, the file has a {}",
                m.kind, entry.kind
            );
            report.conflicts.push(conflict(reason, &entry));
            continue;
        }
        let found_id = found.map(|(id, _)| *id);
        if let Some((name, id)) = ids.iter().find_map(|(n, id)| {
            let id = (*id)?;
            existing
                .iter()
                .any(|(row, m)| {
                    Some(*row) != found_id
                        && match *n {
                            "TVDB" => m.tvdb_id == Some(id),
                            _ => m.tmdb_id == Some(id),
                        }
                })
                .then_some((n, id))
        }) {
            let reason = format!("{name} id {id} belongs to another mapping here");
            report.conflicts.push(conflict(reason, &entry));
            continue;
        }
        for (name, id) in ids {
            if let Some(id) = id {
                claimed.insert((name, id));
            }
        }
        let item = ImportItem {
            title: entry.title.clone(),
            kind: entry.kind.clone(),
            tvdb_id: entry.tvdb_id,
            tmdb_id: entry.tmdb_id,
        };
        match found {
            None => {
                report.creates.push(item);
                plans.push(Plan::Create(entry));
            }
            Some((_, current)) if !changes(current, &entry) => report.unchanged.push(item),
            Some((id, _)) => {
                report.updates.push(item);
                plans.push(Plan::Update(*id, entry));
            }
        }
    }
    if query.dry_run || plans.is_empty() {
        return Ok(Json(report));
    }
    for plan in &plans {
        match plan {
            Plan::Create(m) => {
                let id = write(&mut tx, None, m).await?;
                write_children(&mut tx, id, m).await?;
            }
            Plan::Update(id, m) => {
                write(&mut tx, Some(*id), m).await?;
                write_children(&mut tx, *id, m).await?;
            }
        }
    }
    tx.commit().await.map_err(StoreError::from)?;
    tracing::info!(
        created = report.creates.len(),
        updated = report.updates.len(),
        conflicts = report.conflicts.len(),
        "mappings imported in the admin UI"
    );
    state.matcher.waker().notify_one();
    Ok(Json(report))
}

/// Checks one mapping from the file the way the admin API checks a new one, and tidies it the
/// same way (trimmed lists, blank strings as absent). `Err` says what is wrong.
fn normalize(mut e: ExportedMapping) -> Result<ExportedMapping, String> {
    let kind = parse_kind(&e.kind).map_err(|e| e.to_string())?;
    e.title = e.title.trim().to_owned();
    if e.title.is_empty() {
        return Err("title must not be empty".to_owned());
    }
    match kind {
        MappingKind::Film if e.tmdb_id.is_none() => return Err("a film needs tmdb_id".to_owned()),
        MappingKind::Show | MappingKind::Daily if e.tvdb_id.is_none() => {
            return Err("a show needs tvdb_id".to_owned());
        }
        _ => {}
    }
    e.language = e.language.trim().to_owned();
    if e.language.is_empty() {
        return Err("language must not be empty".to_owned());
    }
    if !matches!(e.language_source.as_str(), "arr" | "channel" | "user") {
        return Err("language_source must be arr, channel or user".to_owned());
    }
    e.channel_ids = clean_list(std::mem::take(&mut e.channel_ids));
    e.excludes = clean_list(std::mem::take(&mut e.excludes));
    e.episode_regex = blank_to_none(e.episode_regex);
    e.imdb_id = blank_to_none(e.imdb_id);
    e.playlist_id = blank_to_none(e.playlist_id);
    e.timezone = blank_to_none(e.timezone)
        .map(|name| timezone_name(&name))
        .transpose()?;
    e.root_folder = blank_to_none(e.root_folder);
    if let Some(folder) = &e.root_folder {
        root_folders::check_override(folder)?;
    }
    let draft = Mapping {
        id: 0,
        kind,
        tvdb_id: e.tvdb_id,
        tmdb_id: e.tmdb_id,
        title: e.title.clone(),
        language: e.language.clone(),
        channel_ids: e.channel_ids.clone(),
        episode_regex: e.episode_regex.clone(),
        excludes: e.excludes.clone(),
        min_duration_secs: e.min_duration_secs,
        created_at: 0,
        updated_at: 0,
        active: true,
        arr_id: None,
        year: e.year,
        imdb_id: e.imdb_id.clone(),
        utc_offset_mins: e.utc_offset_mins,
        timezone: e.timezone.clone(),
        playlist_id: e.playlist_id.clone(),
        playlist_season: e.playlist_season,
        playlist_offset: e.playlist_offset,
        playlist_mode: e.playlist_mode,
        backfill_limit: e.backfill_limit,
        title_number: e.title_number,
        audio_version: e.audio_version,
        episode_thumbnails: e.episode_thumbnails,
    };
    check(&draft).map_err(|e| e.to_string())?;

    let mut aliases: Vec<ExportedAlias> = Vec::new();
    for mut a in std::mem::take(&mut e.aliases) {
        a.alias = a.alias.trim().to_owned();
        if !matches!(a.source.as_str(), "arr" | "user") {
            return Err(format!("alias {:?}: source must be arr or user", a.alias));
        }
        if !a.alias.is_empty() && !aliases.iter().any(|b| b.alias == a.alias) {
            aliases.push(a);
        }
    }
    e.aliases = aliases;
    for pin in &mut e.pins {
        pin.video_id = pin.video_id.trim().to_owned();
        if !pin.video_id.split(',').all(is_video_id) {
            return Err(format!("{:?} is not a YouTube video id", pin.video_id));
        }
        let new = NewPin {
            mapping_id: 0,
            video_id: pin.video_id.clone(),
            season: pin.season,
            episode: pin.episode,
            air_date: pin.air_date.clone(),
        };
        target_for(kind, &new).map_err(|e| e.to_string())?;
    }
    let targets: HashSet<_> = e
        .pins
        .iter()
        .map(|p| (p.season, p.episode, p.air_date.clone()))
        .collect();
    if targets.len() != e.pins.len() {
        return Err("two pins for the same episode".to_owned());
    }
    Ok(e)
}

/// Whether importing `new` over `current` would change anything: its fields, its user aliases,
/// an arr alias it adds, or a pin it adds or replaces.
fn changes(current: &ExportedMapping, new: &ExportedMapping) -> bool {
    let fields = |m: &ExportedMapping| ExportedMapping {
        aliases: vec![],
        pins: vec![],
        ..m.clone()
    };
    let user = |m: &ExportedMapping| -> HashSet<String> {
        m.aliases
            .iter()
            .filter(|a| a.source == "user")
            .map(|a| a.alias.clone())
            .collect()
    };
    fields(current) != fields(new)
        || user(current) != user(new)
        || new.aliases.iter().any(|a| !current.aliases.contains(a))
        || new.pins.iter().any(|p| !current.pins.contains(p))
}

/// Inserts (`id` is `None`) or overwrites one mapping's fields. Returns its id.
async fn write(
    conn: &mut SqliteConnection,
    id: Option<i64>,
    m: &ExportedMapping,
) -> Result<i64, StoreError> {
    let now = unix_now();
    let sql = match id {
        None => {
            "INSERT INTO mappings (kind, tvdb_id, tmdb_id, imdb_id, title, year, language, \
             language_source, channel_ids, episode_regex, excludes, min_duration_secs, \
             utc_offset_mins, timezone, playlist_id, playlist_season, playlist_offset, \
             playlist_mode, backfill_limit, title_number, audio_version, episode_thumbnails, \
             root_folder, updated_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             RETURNING id"
        }
        Some(_) => {
            "UPDATE mappings SET kind = ?, tvdb_id = ?, tmdb_id = ?, imdb_id = ?, title = ?, \
             year = ?, language = ?, language_source = ?, channel_ids = ?, episode_regex = ?, \
             excludes = ?, min_duration_secs = ?, utc_offset_mins = ?, timezone = ?, \
             playlist_id = ?, playlist_season = ?, playlist_offset = ?, playlist_mode = ?, \
             backfill_limit = ?, title_number = ?, audio_version = ?, episode_thumbnails = ?, \
             root_folder = ?, updated_at = ? WHERE id = ? RETURNING id"
        }
    };
    Ok(sqlx::query_scalar(sql)
        .bind(&m.kind)
        .bind(m.tvdb_id)
        .bind(m.tmdb_id)
        .bind(&m.imdb_id)
        .bind(&m.title)
        .bind(m.year)
        .bind(&m.language)
        .bind(&m.language_source)
        .bind(SqlJson(&m.channel_ids))
        .bind(&m.episode_regex)
        .bind(SqlJson(&m.excludes))
        .bind(m.min_duration_secs)
        .bind(m.utc_offset_mins)
        .bind(&m.timezone)
        .bind(&m.playlist_id)
        .bind(m.playlist_season)
        .bind(m.playlist_offset)
        .bind(m.playlist_mode)
        .bind(m.backfill_limit)
        .bind(m.title_number)
        .bind(m.audio_version)
        .bind(m.episode_thumbnails)
        .bind(&m.root_folder)
        .bind(now)
        // The last placeholder is `created_at` for an insert and the row id for an update.
        .bind(id.unwrap_or(now))
        .fetch_one(&mut *conn)
        .await?)
}

/// The file's user aliases replace the mapping's, its arr aliases are added, its pins are added or
/// replace the pin for the same target.
async fn write_children(
    conn: &mut SqliteConnection,
    id: i64,
    m: &ExportedMapping,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM mapping_aliases WHERE mapping_id = ? AND source = 'user'")
        .bind(id)
        .execute(&mut *conn)
        .await?;
    for a in &m.aliases {
        sqlx::query(
            "INSERT INTO mapping_aliases (mapping_id, alias, source) VALUES (?, ?, ?) \
             ON CONFLICT (mapping_id, alias) DO UPDATE SET source = \
             CASE WHEN excluded.source = 'user' THEN 'user' ELSE source END",
        )
        .bind(id)
        .bind(&a.alias)
        .bind(&a.source)
        .execute(&mut *conn)
        .await?;
    }
    for p in &m.pins {
        let target = match (p.season, p.episode, &p.air_date) {
            (Some(season), Some(episode), _) => PinTarget::Episode { season, episode },
            (_, _, Some(date)) => PinTarget::AirDate(date.clone()),
            _ => PinTarget::Film,
        };
        replace_pin(conn, id, &target, &p.video_id).await?;
    }
    Ok(())
}
