//! `/admin/api/mappings`: list, read, edit the Tubarr-owned rules of, add, and dry-run mappings.
//!
//! Title, kind and the TVDB, TMDB and IMDb ids belong to Sonarr and Radarr (the arr sync writes
//! them), so a PATCH refuses them. Only a manual mapping (POST) sets them, once.

use std::{
    collections::{BTreeMap, HashMap},
    sync::LazyLock,
};

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use regex::Regex;
use serde::{Deserialize, Deserializer, Serialize};
use sqlx::types::Json as SqlJson;

use super::{
    AdminError, Authorized,
    dry_run::{self, DryRun},
};
use crate::{
    AppState,
    arr::root_folders,
    config::Config,
    hooks,
    matcher::{
        dates::timezone_name,
        preset::{self, Preset},
        title::TitleRules,
    },
    store::{
        Db, LanguageSource, Mapping, MappingKind, MatchOptions, NewMapping, PlaylistMode,
        RootFolderRow, unix_now,
    },
};

/// A mapping as the UI sees it, with how many releases and unmatched notes it has.
#[derive(Debug, Serialize)]
pub struct MappingView {
    pub id: i64,
    pub kind: &'static str,
    /// A show matched by its running number, see [`Mapping::is_podcast`]. Its kind stays `show`.
    pub is_podcast: bool,
    pub active: bool,
    pub title: String,
    pub year: Option<i64>,
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub imdb_id: Option<String>,
    pub arr_id: Option<i64>,
    pub language: String,
    /// Who set the language: `arr`, `channel` (from `[channels]`) or `user`.
    pub language_source: LanguageSource,
    pub channel_ids: Vec<String>,
    /// The names of the channels that have one saved, from resolving a handle or link.
    pub channel_titles: BTreeMap<String, String>,
    pub episode_regex: Option<String>,
    pub excludes: Vec<String>,
    pub min_duration_secs: Option<i64>,
    pub utc_offset_mins: Option<i64>,
    /// An IANA time zone, over `utc_offset_mins`.
    pub timezone: Option<String>,
    pub playlist_id: Option<String>,
    pub playlist_season: i64,
    pub playlist_offset: i64,
    /// `source` (titles give the number) or `order` (the playlist place does).
    pub playlist_mode: PlaylistMode,
    /// Back episodes released: the newest this many uploaded before the mapping was added. Null
    /// releases all of them.
    pub backfill_limit: Option<i64>,
    /// Shows: match the `#N` in a video title against the number in TVDB's episode titles.
    pub title_number: bool,
    /// Shows and daily shows: an `Audio only` version next to each episode in Jellyfin.
    pub audio_version: bool,
    /// Shows and daily shows: the YouTube thumbnail as each episode's image in Jellyfin.
    pub episode_thumbnails: bool,
    pub releases: i64,
    pub unmatched: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// The user's root folder override: a path, `none` (routing off), or null (follow the config).
    pub root_folder: Option<String>,
    /// The Sonarr or Radarr root folder the title should live in: the override, else its first
    /// channel's, else its language's. Null for no routing or a mapping made by hand.
    pub wanted_root_folder: Option<String>,
    /// The root folder the arr reported at the last sync.
    pub current_root_folder: Option<String>,
    /// The last routing result: `moved`, `missing` (the arr has no such root folder) or
    /// `failed: ...`, and when.
    pub root_folder_result: Option<String>,
    pub root_folder_result_at: Option<i64>,
}

pub(super) fn kind_name(kind: MappingKind) -> &'static str {
    match kind {
        MappingKind::Show => "show",
        MappingKind::Daily => "daily",
        MappingKind::Film => "film",
    }
}

pub(super) fn parse_kind(name: &str) -> Result<MappingKind, AdminError> {
    match name {
        "show" => Ok(MappingKind::Show),
        "daily" => Ok(MappingKind::Daily),
        "film" => Ok(MappingKind::Film),
        other => Err(AdminError::Invalid(format!(
            "kind must be show, daily or film, not {other:?}"
        ))),
    }
}

impl MappingView {
    fn new(m: Mapping, (releases, unmatched, language_source): Extra) -> Self {
        Self {
            id: m.id,
            kind: kind_name(m.kind),
            is_podcast: m.is_podcast(),
            active: m.active,
            title: m.title,
            year: m.year,
            tvdb_id: m.tvdb_id,
            tmdb_id: m.tmdb_id,
            imdb_id: m.imdb_id,
            arr_id: m.arr_id,
            language: m.language,
            language_source,
            channel_ids: m.channel_ids,
            channel_titles: BTreeMap::new(),
            episode_regex: m.episode_regex,
            excludes: m.excludes,
            min_duration_secs: m.min_duration_secs,
            utc_offset_mins: m.utc_offset_mins,
            timezone: m.timezone,
            playlist_id: m.playlist_id,
            playlist_season: m.playlist_season,
            playlist_offset: m.playlist_offset,
            playlist_mode: m.playlist_mode,
            backfill_limit: m.backfill_limit,
            title_number: m.title_number,
            audio_version: m.audio_version,
            episode_thumbnails: m.episode_thumbnails,
            releases,
            unmatched,
            created_at: m.created_at,
            updated_at: m.updated_at,
            root_folder: None,
            wanted_root_folder: None,
            current_root_folder: None,
            root_folder_result: None,
            root_folder_result_at: None,
        }
    }

    fn with_channel_titles(mut self, titles: &HashMap<String, String>) -> Self {
        self.channel_titles = self
            .channel_ids
            .iter()
            .filter_map(|id| titles.get(id).map(|t| (id.clone(), t.clone())))
            .collect();
        self
    }

    /// Fills the root folder fields from the mapping's routing state.
    fn with_root_folder(mut self, row: Option<&RootFolderRow>, config: &Config) -> Self {
        let Some(row) = row else { return self };
        if row.arr_id.is_some() {
            self.wanted_root_folder = root_folders::wanted(
                row.kind,
                &row.language,
                &row.channel_ids,
                row.override_folder.as_deref(),
                &config.channels,
                &config.root_folders,
            );
        }
        self.root_folder = row.override_folder.clone();
        self.current_root_folder = row.current.clone();
        self.root_folder_result = row.result.clone();
        self.root_folder_result_at = row.result_at;
        self
    }
}

/// Every mapping's routing state by id.
async fn root_folder_states(db: &Db) -> Result<HashMap<i64, RootFolderRow>, AdminError> {
    Ok(db
        .root_folder_states()
        .await?
        .into_iter()
        .map(|r| (r.id, r))
        .collect())
}

/// Releases, unmatched notes (capped back episodes left out) and the language source of one
/// mapping.
type Extra = (i64, i64, LanguageSource);

/// [`Extra`] per mapping id.
async fn counts(db: &Db) -> Result<HashMap<i64, Extra>, sqlx::Error> {
    let rows: Vec<(i64, i64, i64, LanguageSource)> = sqlx::query_as(
        "SELECT m.id, \
           (SELECT count(*) FROM releases r WHERE r.mapping_id = m.id), \
           (SELECT count(*) FROM unmatched u WHERE u.mapping_id = m.id AND u.state <> 'capped'), \
           m.language_source \
         FROM mappings m",
    )
    .fetch_all(db.pool())
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, r, u, source)| (id, (r, u, source)))
        .collect())
}

/// For a row deleted between the read and the counts. Never shown in practice.
const NO_EXTRA: Extra = (0, 0, LanguageSource::User);

async fn view(state: &AppState, mapping: Mapping) -> Result<MappingView, AdminError> {
    let counts = counts(&state.db)
        .await
        .map_err(crate::store::StoreError::from)?;
    let c = counts.get(&mapping.id).copied().unwrap_or(NO_EXTRA);
    let routing = state.db.root_folder_state(mapping.id).await?;
    let titles = state.db.channel_titles().await?;
    Ok(MappingView::new(mapping, c)
        .with_root_folder(routing.as_ref(), &state.config)
        .with_channel_titles(&titles))
}

#[derive(Debug, Deserialize)]
pub struct ListFilter {
    /// `show`, `daily`, `film` or `podcast`. Shows include podcasts; `podcast` narrows to them.
    kind: Option<String>,
    active: Option<bool>,
}

/// The kinds the list filters by: a mapping kind, or the podcasts among the shows.
#[derive(Clone, Copy)]
enum KindFilter {
    Kind(MappingKind),
    Podcast,
}

impl KindFilter {
    fn parse(name: &str) -> Result<Self, AdminError> {
        match name {
            "podcast" => Ok(Self::Podcast),
            "show" | "daily" | "film" => parse_kind(name).map(Self::Kind),
            other => Err(AdminError::Invalid(format!(
                "kind must be show, daily, film or podcast, not {other:?}"
            ))),
        }
    }

    fn keeps(self, m: &Mapping) -> bool {
        match self {
            Self::Kind(kind) => m.kind == kind,
            Self::Podcast => m.is_podcast(),
        }
    }
}

pub async fn list(
    _: Authorized,
    State(state): State<AppState>,
    Query(filter): Query<ListFilter>,
) -> Result<Json<Vec<MappingView>>, AdminError> {
    let kind = filter.kind.as_deref().map(KindFilter::parse).transpose()?;
    let counts = counts(&state.db)
        .await
        .map_err(crate::store::StoreError::from)?;
    let routing = root_folder_states(&state.db).await?;
    let titles = state.db.channel_titles().await?;
    let views = state
        .db
        .mappings()
        .await?
        .into_iter()
        .filter(|m| kind.is_none_or(|k| k.keeps(m)))
        .filter(|m| filter.active.is_none_or(|a| m.active == a))
        .map(|m| {
            let c = counts.get(&m.id).copied().unwrap_or(NO_EXTRA);
            let r = routing.get(&m.id);
            MappingView::new(m, c)
                .with_root_folder(r, &state.config)
                .with_channel_titles(&titles)
        })
        .collect();
    Ok(Json(views))
}

pub async fn one(
    _: Authorized,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<MappingView>, AdminError> {
    let mapping = state.db.mapping(id).await?.ok_or(AdminError::NotFound)?;
    Ok(Json(view(&state, mapping).await?))
}

/// `Some(None)` for an explicit `null`, so a PATCH can clear a field. A missing field stays `None`.
fn nullable<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

/// The fields Tubarr owns. Missing fields keep their value, `null` clears a nullable one.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MappingPatch {
    pub language: Option<String>,
    pub channel_ids: Option<Vec<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub episode_regex: Option<Option<String>>,
    pub excludes: Option<Vec<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub min_duration_secs: Option<Option<i64>>,
    #[serde(default, deserialize_with = "nullable")]
    pub utc_offset_mins: Option<Option<i64>>,
    /// An IANA name like "America/Chicago". `null` (or "") falls back to `utc_offset_mins`.
    #[serde(default, deserialize_with = "nullable")]
    pub timezone: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    pub playlist_id: Option<Option<String>>,
    pub playlist_season: Option<i64>,
    pub playlist_offset: Option<i64>,
    pub playlist_mode: Option<PlaylistMode>,
    /// `null` releases every back episode.
    #[serde(default, deserialize_with = "nullable")]
    pub backfill_limit: Option<Option<i64>>,
    pub title_number: Option<bool>,
    pub audio_version: Option<bool>,
    pub episode_thumbnails: Option<bool>,
    /// Root folder override: a path, `none`, or `null` (or "") to follow the config again.
    #[serde(default, deserialize_with = "nullable")]
    pub root_folder: Option<Option<String>>,
}

static CHANNEL_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^UC[A-Za-z0-9_-]{22}$").expect("valid channel id regex"));

fn invalid(msg: impl Into<String>) -> AdminError {
    AdminError::Invalid(msg.into())
}

pub(super) fn clean_list(items: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        let item = item.trim().to_owned();
        if !item.is_empty() && !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// Turns the channel lines of a patch (ids, handles, channel or video links) into channel ids,
/// saving the names YouTube gave on the way.
async fn resolve_channels(
    state: &AppState,
    lines: &mut Option<Vec<String>>,
) -> Result<(), AdminError> {
    let Some(inputs) = lines.take() else {
        return Ok(());
    };
    let found = state
        .channel_lookup
        .resolve_all(&inputs)
        .await
        .map_err(|e| {
            if e.is_input_error() {
                invalid(e.to_string())
            } else {
                AdminError::Failed(e.to_string())
            }
        })?;
    for f in &found {
        if let Some(title) = &f.title {
            state.db.set_channel_title(&f.channel_id, title).await?;
        }
    }
    *lines = Some(found.into_iter().map(|f| f.channel_id).collect());
    Ok(())
}

pub(super) fn blank_to_none(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

impl MappingPatch {
    /// Applies the patch to `m` in memory and checks the result: channel ids, the regex, ranges.
    fn apply(self, m: &mut Mapping) -> Result<(), AdminError> {
        if let Some(language) = self.language {
            let language = language.trim().to_owned();
            if language.is_empty() {
                return Err(invalid("language must not be empty"));
            }
            m.language = language;
        }
        if let Some(ids) = self.channel_ids {
            m.channel_ids = clean_list(ids);
        }
        if let Some(regex) = self.episode_regex {
            m.episode_regex = blank_to_none(regex);
        }
        if let Some(excludes) = self.excludes {
            m.excludes = clean_list(excludes);
        }
        if let Some(min) = self.min_duration_secs {
            m.min_duration_secs = min;
        }
        if let Some(offset) = self.utc_offset_mins {
            m.utc_offset_mins = offset;
        }
        if let Some(timezone) = self.timezone {
            m.timezone = blank_to_none(timezone)
                .map(|name| timezone_name(&name))
                .transpose()
                .map_err(invalid)?;
        }
        if let Some(playlist) = self.playlist_id {
            m.playlist_id = blank_to_none(playlist);
        }
        if let Some(season) = self.playlist_season {
            m.playlist_season = season;
        }
        if let Some(offset) = self.playlist_offset {
            m.playlist_offset = offset;
        }
        if let Some(mode) = self.playlist_mode {
            m.playlist_mode = mode;
        }
        if let Some(limit) = self.backfill_limit {
            m.backfill_limit = limit;
        }
        if let Some(on) = self.title_number {
            m.title_number = on;
        }
        if let Some(on) = self.audio_version {
            m.audio_version = on;
        }
        if let Some(on) = self.episode_thumbnails {
            m.episode_thumbnails = on;
        }
        check(m)
    }
}

pub(super) fn check(m: &Mapping) -> Result<(), AdminError> {
    if let Some(bad) = m.channel_ids.iter().find(|id| !CHANNEL_ID.is_match(id)) {
        return Err(invalid(format!(
            "{bad:?} is not a YouTube channel id (UC followed by 22 characters)"
        )));
    }
    if m.min_duration_secs.is_some_and(|d| d <= 0) {
        return Err(invalid("min_duration_secs must be above 0"));
    }
    if m.utc_offset_mins
        .is_some_and(|o| !(-720..=840).contains(&o))
    {
        return Err(invalid("utc_offset_mins must be between -720 and 840"));
    }
    if m.backfill_limit.is_some_and(|n| n < 0) {
        return Err(invalid(
            "backfill_limit must be 0 or more, or null for no limit",
        ));
    }
    if m.playlist_season < 0 || m.playlist_offset < 0 {
        return Err(invalid(
            "playlist_season and playlist_offset must be 0 or more",
        ));
    }
    if m.audio_version && m.kind == MappingKind::Film {
        return Err(invalid(
            "audio_version is for shows and daily shows, not films",
        ));
    }
    if m.episode_thumbnails && m.kind == MappingKind::Film {
        return Err(invalid(
            "episode_thumbnails is for shows and daily shows, not films",
        ));
    }
    TitleRules::new(m).map_err(|e| invalid(e.to_string()))?;
    Ok(())
}

pub async fn patch(
    _: Authorized,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(mut patch): Json<MappingPatch>,
) -> Result<Json<MappingView>, AdminError> {
    let mut mapping = state.db.mapping(id).await?.ok_or(AdminError::NotFound)?;
    let before = mapping.clone();
    let root_folder = patch.root_folder.take().map(blank_to_none);
    if let Some(Some(folder)) = &root_folder {
        root_folders::check_override(folder).map_err(invalid)?;
    }
    resolve_channels(&state, &mut patch.channel_ids).await?;
    patch.apply(&mut mapping)?;
    // The UI sends the language on every save, so only a changed one counts as set by the user.
    let user_language = mapping.language != before.language;
    // One statement, so the rules and the match options change together.
    let saved: Option<Mapping> = sqlx::query_as(
        "UPDATE mappings SET language = ?, channel_ids = ?, episode_regex = ?, excludes = ?, \
         min_duration_secs = ?, utc_offset_mins = ?, timezone = ?, playlist_id = ?, \
         playlist_season = ?, playlist_offset = ?, playlist_mode = ?, backfill_limit = ?, \
         title_number = ?, audio_version = ?, episode_thumbnails = ?, updated_at = ?, \
         language_source = CASE WHEN ? THEN 'user' ELSE language_source END \
         WHERE id = ? RETURNING *",
    )
    .bind(&mapping.language)
    .bind(SqlJson(&mapping.channel_ids))
    .bind(&mapping.episode_regex)
    .bind(SqlJson(&mapping.excludes))
    .bind(mapping.min_duration_secs)
    .bind(mapping.utc_offset_mins)
    .bind(&mapping.timezone)
    .bind(&mapping.playlist_id)
    .bind(mapping.playlist_season)
    .bind(mapping.playlist_offset)
    .bind(mapping.playlist_mode)
    .bind(mapping.backfill_limit)
    .bind(mapping.title_number)
    .bind(mapping.audio_version)
    .bind(mapping.episode_thumbnails)
    .bind(unix_now())
    .bind(user_language)
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(crate::store::StoreError::from)?;
    let mut saved = saved.ok_or(AdminError::NotFound)?;
    // A show still on the arr's default language takes the language of its new first channel.
    if !user_language && saved.channel_ids != before.channel_ids {
        let channels = &state.config.channels;
        if state.db.languages_from_channels(channels, Some(id)).await? > 0 {
            saved = state.db.mapping(id).await?.ok_or(AdminError::NotFound)?;
        }
    }
    let routing_before = state.db.root_folder_state(id).await?;
    let old_override = routing_before.and_then(|r| r.override_folder);
    let new_override = root_folder.clone().unwrap_or_else(|| old_override.clone());
    if new_override != old_override {
        state
            .db
            .set_root_folder_override(id, new_override.as_deref())
            .await?;
    }
    // A new override, channel or language can change the wanted root folder: move now, not at
    // the next sync. A failure here is logged and shown in the result fields, never a failed save.
    if (new_override != old_override
        || saved.channel_ids != before.channel_ids
        || saved.language != before.language)
        && let Err(e) = root_folders::route_one(&state.config, &state.arrs, &state.db, id).await
    {
        tracing::warn!(mapping = id, error = %e, "root folder routing failed");
    }
    // Write or remove the audio only files of episodes Sonarr already has. A failure is logged,
    // never a failed save: the webhook keeps new episodes in step either way.
    if saved.audio_version != before.audio_version {
        match hooks::backfill(
            &state.config,
            state.arrs.sonarr().as_ref(),
            state.jellyfin.as_deref(),
            &saved,
        )
        .await
        {
            Ok(n) => tracing::info!(
                mapping = id,
                files = n,
                on = saved.audio_version,
                "audio only versions updated"
            ),
            Err(e) => tracing::warn!(mapping = id, error = %e, "audio only versions not updated"),
        }
    }
    // Fetch the thumbnails of episodes Sonarr already has, the same way. Turning the option off
    // leaves the images in place.
    if saved.episode_thumbnails && !before.episode_thumbnails {
        match hooks::backfill_thumbnails(
            &state.config,
            state.arrs.sonarr().as_ref(),
            &state.thumbs,
            &saved,
        )
        .await
        {
            Ok(n) => tracing::info!(
                mapping = id,
                written = n.written,
                failed = n.failed,
                "episode thumbnails fetched"
            ),
            Err(e) => tracing::warn!(mapping = id, error = %e, "episode thumbnails not fetched"),
        }
    }
    tracing::info!(mapping = id, title = %saved.title, "mapping rules changed in the admin UI");
    // The new rules apply on the matcher's next run, so wake it now.
    state.matcher.waker().notify_one();
    Ok(Json(view(&state, saved).await?))
}

/// `POST /admin/api/mappings/{id}/audio-versions`: writes or removes the audio only versions of
/// every episode Sonarr has for the mapping, as turning the option on or off does, and queues
/// their Jellyfin merges or splits again. For merges Jellyfin was given up on.
pub async fn audio_versions(
    _: Authorized,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<serde_json::Value>, AdminError> {
    let mapping = state.db.mapping(id).await?.ok_or(AdminError::NotFound)?;
    if mapping.kind == MappingKind::Film {
        return Err(invalid("a film has no audio only versions"));
    }
    let changed = hooks::backfill(
        &state.config,
        state.arrs.sonarr().as_ref(),
        state.jellyfin.as_deref(),
        &mapping,
    )
    .await
    .map_err(|e| AdminError::Failed(e.to_string()))?;
    let queued = match &state.jellyfin {
        Some(jellyfin) => jellyfin.health().await.queued,
        None => 0,
    };
    tracing::info!(
        mapping = id,
        files = changed,
        queued,
        "audio only versions synced again"
    );
    Ok(Json(
        serde_json::json!({ "changed": changed, "queued": queued }),
    ))
}

/// `POST /admin/api/mappings/{id}/thumbnails`: fetches the episode thumbnails still missing
/// next to the episodes Sonarr has for the mapping, as turning the option on does. For episodes
/// imported before the option existed, or whose fetch failed.
pub async fn thumbnails(
    _: Authorized,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<hooks::ThumbCount>, AdminError> {
    let mapping = state.db.mapping(id).await?.ok_or(AdminError::NotFound)?;
    if !mapping.episode_thumbnails {
        return Err(invalid("episode_thumbnails is off for this mapping"));
    }
    let count = hooks::backfill_thumbnails(
        &state.config,
        state.arrs.sonarr().as_ref(),
        &state.thumbs,
        &mapping,
    )
    .await
    .map_err(|e| AdminError::Failed(e.to_string()))?;
    tracing::info!(
        mapping = id,
        written = count.written,
        failed = count.failed,
        "episode thumbnails fetched again"
    );
    Ok(Json(count))
}

/// A mapping made by hand, for a title Sonarr or Radarr do not tag.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewManual {
    pub kind: String,
    pub title: String,
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub language: String,
    #[serde(default)]
    pub channel_ids: Vec<String>,
    pub episode_regex: Option<String>,
    #[serde(default)]
    pub excludes: Vec<String>,
    pub min_duration_secs: Option<i64>,
    pub utc_offset_mins: Option<i64>,
    pub timezone: Option<String>,
    pub playlist_id: Option<String>,
    pub playlist_season: Option<i64>,
    pub playlist_offset: Option<i64>,
    pub playlist_mode: Option<PlaylistMode>,
    pub backfill_limit: Option<i64>,
    pub title_number: Option<bool>,
    pub audio_version: Option<bool>,
    /// Off by default, on with the podcast preset.
    pub episode_thumbnails: Option<bool>,
    /// Starting rules. A podcast fills in what the fields above leave out, see [`preset`].
    #[serde(default)]
    pub preset: Preset,
}

pub async fn create(
    _: Authorized,
    State(state): State<AppState>,
    Json(new): Json<NewManual>,
) -> Result<(StatusCode, Json<MappingView>), AdminError> {
    let kind = parse_kind(&new.kind)?;
    let title = new.title.trim().to_owned();
    if title.is_empty() {
        return Err(invalid("title must not be empty"));
    }
    match kind {
        MappingKind::Film if new.tmdb_id.is_none() => {
            return Err(invalid("a film needs tmdb_id"));
        }
        MappingKind::Show | MappingKind::Daily if new.tvdb_id.is_none() => {
            return Err(invalid("a show needs tvdb_id"));
        }
        _ => {}
    }
    // Build the full mapping in memory first, so every rule is checked before anything is written.
    let options = MatchOptions::default();
    let mut draft = Mapping {
        id: 0,
        kind,
        tvdb_id: new.tvdb_id,
        tmdb_id: new.tmdb_id,
        title,
        language: new.language,
        channel_ids: vec![],
        episode_regex: None,
        excludes: vec![],
        min_duration_secs: None,
        created_at: 0,
        updated_at: 0,
        active: true,
        arr_id: None,
        year: None,
        imdb_id: None,
        utc_offset_mins: options.utc_offset_mins,
        timezone: options.timezone.clone(),
        playlist_id: options.playlist_id.clone(),
        playlist_season: options.playlist_season,
        playlist_offset: options.playlist_offset,
        playlist_mode: options.playlist_mode,
        backfill_limit: None,
        title_number: options.title_number,
        audio_version: false,
        episode_thumbnails: false,
    };
    let mut channel_ids = Some(new.channel_ids);
    resolve_channels(&state, &mut channel_ids).await?;
    let rules = MappingPatch {
        language: Some(draft.language.clone()),
        channel_ids,
        episode_regex: Some(new.episode_regex),
        excludes: Some(new.excludes),
        min_duration_secs: Some(new.min_duration_secs),
        utc_offset_mins: Some(new.utc_offset_mins),
        timezone: Some(new.timezone.clone()),
        playlist_id: Some(new.playlist_id),
        playlist_season: new.playlist_season,
        playlist_offset: new.playlist_offset,
        playlist_mode: new.playlist_mode,
        backfill_limit: Some(new.backfill_limit),
        title_number: new.title_number,
        audio_version: new.audio_version,
        episode_thumbnails: new.episode_thumbnails,
        root_folder: None,
    };
    rules.apply(&mut draft)?;
    if new.preset == Preset::Podcast {
        if kind != MappingKind::Show {
            return Err(invalid("the podcast preset is for shows"));
        }
        let zone = preset::podcast_zone(
            new.timezone.as_deref(),
            new.utc_offset_mins,
            &state.config.channels,
            &draft.channel_ids,
        )
        .map_err(invalid)?;
        preset::apply_podcast(&mut draft, zone);
        draft.episode_thumbnails = new.episode_thumbnails.unwrap_or(true);
        check(&draft)?;
    }
    let stored = state
        .db
        .insert_mapping(&NewMapping {
            kind,
            tvdb_id: draft.tvdb_id,
            tmdb_id: draft.tmdb_id,
            title: draft.title.clone(),
            language: draft.language.clone(),
            channel_ids: draft.channel_ids.clone(),
            episode_regex: draft.episode_regex.clone(),
            excludes: draft.excludes.clone(),
            min_duration_secs: draft.min_duration_secs,
        })
        .await?;
    let wanted = MatchOptions {
        utc_offset_mins: draft.utc_offset_mins,
        timezone: draft.timezone,
        playlist_id: draft.playlist_id,
        playlist_season: draft.playlist_season,
        playlist_offset: draft.playlist_offset,
        playlist_mode: draft.playlist_mode,
        title_number: draft.title_number,
    };
    let stored = if wanted == options {
        stored
    } else {
        state
            .db
            .set_match_options(stored.id, &wanted)
            .await?
            .ok_or(AdminError::NotFound)?
    };
    let stored = match draft.backfill_limit {
        None => stored,
        limit => state
            .db
            .set_backfill_limit(stored.id, limit)
            .await?
            .ok_or(AdminError::NotFound)?,
    };
    // A manual mapping has no Sonarr id, so there are no episode files to write versions for yet.
    let stored = if draft.audio_version {
        state
            .db
            .set_audio_version(stored.id, true)
            .await?
            .ok_or(AdminError::NotFound)?
    } else {
        stored
    };
    let stored = if draft.episode_thumbnails {
        state
            .db
            .set_episode_thumbnails(stored.id, true)
            .await?
            .ok_or(AdminError::NotFound)?
    } else {
        stored
    };
    tracing::info!(mapping = stored.id, title = %stored.title, "manual mapping added in the admin UI");
    state.matcher.waker().notify_one();
    Ok((StatusCode::CREATED, Json(view(&state, stored).await?)))
}

/// Dry run. The body may carry unsaved edits (the same fields as a PATCH), so the UI can test
/// rules before saving them. An empty body tests the stored rules.
pub async fn test(
    _: Authorized,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    body: Option<Json<MappingPatch>>,
) -> Result<Json<DryRun>, AdminError> {
    let mut mapping = state.db.mapping(id).await?.ok_or(AdminError::NotFound)?;
    if let Some(Json(mut edits)) = body {
        resolve_channels(&state, &mut edits.channel_ids).await?;
        edits.apply(&mut mapping)?;
    }
    let videos = state.db.videos_on_channels(&mapping.channel_ids).await?;
    let aliases = state.db.aliases(mapping.id).await?;
    let run = dry_run::run(&mapping, &aliases, videos).map_err(|e| invalid(e.to_string()))?;
    Ok(Json(run))
}
