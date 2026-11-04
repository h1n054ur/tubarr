//! The Activity, Health and Settings pages' admin calls: grabs and events page by page, yt-dlp and deno
//! updates and rollbacks, the effective config without secret values, and the catch-up push.

use axum::{
    Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{AdminError, Authorized, paging::Page};
use crate::{
    AppState,
    activity::Event,
    arr::connections::InUse,
    binaries::Tool,
    config::{Config, NotifyTarget, Secret},
    store::{AliasEntry, Grab, GrabStatus},
};

#[derive(Debug, Serialize)]
pub struct GrabView {
    pub nzo_id: String,
    pub release_name: String,
    pub video_id: String,
    pub category: String,
    pub status: &'static str,
    pub storage_path: String,
    pub created_at: i64,
}

impl From<Grab> for GrabView {
    fn from(g: Grab) -> Self {
        Self {
            nzo_id: g.nzo_id,
            release_name: g.release_name,
            video_id: g.video_id,
            category: g.category,
            status: match g.status {
                GrabStatus::Completed => "completed",
                GrabStatus::Failed => "failed",
            },
            storage_path: g.storage_path,
            created_at: g.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ActivityView {
    /// One page of grabs, newest first.
    pub grabs: Vec<GrabView>,
    /// How many grabs the category has in all.
    pub total: i64,
}

#[derive(Debug, Deserialize)]
pub struct ActivityFilter {
    category: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub async fn activity(
    _: Authorized,
    State(state): State<AppState>,
    Query(filter): Query<ActivityFilter>,
) -> Result<Json<ActivityView>, AdminError> {
    let category = filter.category.as_deref().filter(|c| !c.is_empty());
    let page = Page::new(filter.limit, filter.offset)?;
    let grabs = state.db.grabs(category, page.limit, page.offset).await?;
    Ok(Json(ActivityView {
        grabs: grabs.into_iter().map(GrabView::from).collect(),
        total: state.db.count_grabs(category).await?,
    }))
}

#[derive(Debug, Serialize)]
pub struct EventsView {
    /// One page of searches, arr pushes and play starts, newest first.
    pub items: Vec<Event>,
    /// How many events of the kind are stored in all.
    pub total: i64,
    /// `[activity] retention`: how long an event is kept. 0 keeps every event.
    pub retention_secs: u64,
}

#[derive(Debug, Deserialize)]
pub struct EventsFilter {
    /// `search`, `arr_push` or `play`. Without it, every kind.
    kind: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

pub async fn events(
    _: Authorized,
    State(state): State<AppState>,
    Query(filter): Query<EventsFilter>,
) -> Result<Json<EventsView>, AdminError> {
    let kind = filter.kind.as_deref().filter(|k| !k.is_empty());
    let page = Page::new(filter.limit, filter.offset)?;
    let (items, total) = state.activity.page(kind, page.limit, page.offset).await?;
    Ok(Json(EventsView {
        items,
        total,
        retention_secs: state.config.activity.retention.as_secs(),
    }))
}

#[derive(Debug, Serialize)]
pub struct CheckResult {
    pub tool: Tool,
    pub ok: bool,
    pub result: String,
}

/// Asks GitHub for new yt-dlp and deno releases now and installs them, like the canary does.
pub async fn check_binaries(
    _: Authorized,
    State(state): State<AppState>,
) -> Json<Vec<CheckResult>> {
    let results = state.binaries.check(true).await;
    state.health.events().publish_binary_results(&results);
    Json(
        results
            .into_iter()
            .map(|(tool, r)| match r {
                Ok(outcome) => CheckResult {
                    tool,
                    ok: true,
                    result: outcome.to_string(),
                },
                Err(e) => CheckResult {
                    tool,
                    ok: false,
                    result: e.to_string(),
                },
            })
            .collect(),
    )
}

fn parse_tool(name: &str) -> Result<Tool, AdminError> {
    Tool::ALL
        .into_iter()
        .find(|t| t.name() == name)
        .ok_or(AdminError::NotFound)
}

pub async fn rollback(
    _: Authorized,
    State(state): State<AppState>,
    Path(tool): Path<String>,
) -> Result<Json<Value>, AdminError> {
    let tool = parse_tool(&tool)?;
    let rolled_back = state
        .binaries
        .rollback(tool)
        .await
        .map_err(|e| AdminError::Failed(e.to_string()))?;
    if rolled_back {
        tracing::info!(tool = tool.name(), "binary rolled back from the admin UI");
    }
    Ok(Json(json!({ "tool": tool, "rolled_back": rolled_back })))
}

fn set(secret: Option<&Secret>) -> &'static str {
    if secret.is_some() { "set" } else { "not set" }
}

/// The config Tubarr runs with, and the Sonarr and Radarr connections in use. Secret values never
/// leave the process: only whether each is set.
pub fn settings_view(c: &Config, arrs: &InUse) -> Value {
    let arr = |conn: Option<&crate::config::ArrConnection>| match conn {
        Some(conn) => json!({ "url": conn.url, "api_key": "set" }),
        None => Value::Null,
    };
    let notify = match &c.notify {
        None => Value::Null,
        Some(NotifyTarget::Discord { .. }) => json!({ "kind": "discord", "webhook": "set" }),
        Some(NotifyTarget::CardRelay { url, .. }) => {
            json!({ "kind": "card_relay", "url": url, "token": "set" })
        }
    };
    json!({
        "listen": c.listen.to_string(),
        "public_url": c.public_url,
        "data_dir": c.data_dir,
        "complete_dir": c.complete_dir,
        "cache_ttl_secs": c.cache_ttl.as_secs(),
        "lookup_timeout_secs": c.lookup_timeout.as_secs(),
        "api_key": set(c.api_key.as_ref()),
        "rate_limit": {
            "lookups": c.rate_limit.lookups,
            "interval_secs": c.rate_limit.interval.as_secs(),
            "burst": c.rate_limit.burst,
            "jitter_secs": c.rate_limit.jitter.as_secs(),
            "throttle_pause_secs": c.rate_limit.throttle_pause.as_secs(),
        },
        "binaries": {
            "bin_dir": c.binaries.bin_dir,
            "yt_dlp": c.binaries.yt_dlp_path(),
            "deno": c.binaries.deno_path(),
            "yt_dlp_channel": format!("{:?}", c.binaries.yt_dlp_channel).to_lowercase(),
            "check_interval_secs": c.binaries.check_interval.as_secs(),
        },
        "watcher": {
            "rss_interval_secs": c.watcher.rss_interval.as_secs(),
            "listing_interval_secs": c.watcher.listing_interval.as_secs(),
            "listing_size": c.watcher.listing_size,
            "fallback_listing_size": c.watcher.fallback_listing_size,
            "fallback_per_hour": c.watcher.fallback_per_hour,
            "backfill_days": c.watcher.backfill_days,
            "backfill_interval_secs": c.watcher.backfill_interval.as_secs(),
        },
        "subtitles": {
            "languages": c.subtitles.languages,
            "automatic": c.subtitles.automatic,
        },
        "sonarr": arr(arrs.sonarr.as_ref()),
        "radarr": arr(arrs.radarr.as_ref()),
        "arr": {
            "tag": c.arr.tag,
            "sync_interval_secs": c.arr.sync_interval.as_secs(),
        },
        "films": {
            "channels": c.films.channels,
            "min_duration_secs": c.films.min_duration.as_secs(),
            "similarity": c.films.similarity,
        },
        // Each channel's network, language and root folders, and the language defaults.
        "channels": c.channels.iter().collect::<std::collections::BTreeMap<_, _>>(),
        "root_folders": c.root_folders,
        "fallback": {
            "po_token_url": c.fallback.po_token_url,
            "plugin_dir": c.fallback.plugin_dir,
            "cookies_file": if c.fallback.cookies_file.is_some() { "set" } else { "not set" },
            "sticky_for_secs": c.fallback.sticky_for.as_secs(),
            "min_height": c.fallback.min_height,
        },
        "canary": {
            "video_id": c.canary.video_id,
            "interval_secs": c.canary.interval.as_secs(),
        },
        "matcher": {
            "interval_secs": c.matcher.interval.as_secs(),
            "lookups_per_run": c.matcher.lookups_per_run,
            "assumed_height": c.matcher.assumed_height,
        },
        "newznab": {
            "report_size": c.newznab.report_size,
        },
        "notify": notify,
        "auth": auth_view(&c.auth),
    })
}

/// The sign-in setup, flat so the UI can list it. Never the OIDC client secret.
fn auth_view(a: &crate::auth::AuthConfig) -> Value {
    let oidc = a.oidc.as_ref();
    json!({
        "password": a.forms,
        "oidc": oidc.is_some(),
        "local_bypass": a.local_bypass,
        "session_ttl_secs": a.session_ttl.as_secs(),
        "session_max_age_secs": a.session_max_age.as_secs(),
        "oidc_issuer": oidc.map(|o| o.issuer_url.as_str()),
        "allowed_groups": oidc.map(|o| o.allowed_groups.as_slice()),
        "auto_create_users": oidc.map(|o| o.auto_create_users),
        "play_allowed_networks": a.play_allowed_networks.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "play_networks_default": a.play_networks_default,
        "trusted_proxies": a.trusted_proxies.iter().map(ToString::to_string).collect::<Vec<_>>(),
    })
}

pub async fn settings(_: Authorized, State(state): State<AppState>) -> Json<Value> {
    Json(settings_view(&state.config, &state.arrs.current()))
}

/// Catch-up: queues a push for every episode and film with a release in Tubarr. The pusher skips
/// what the arr already has or does not monitor, and paces the rest by `arr.push_interval`.
pub async fn push_released(
    _: Authorized,
    State(state): State<AppState>,
) -> Result<Json<Value>, AdminError> {
    let push = state.push.as_ref().ok_or_else(|| {
        AdminError::Invalid("auto push is off: it needs arr.auto_push and api_key_file".to_owned())
    })?;
    let queued = push.queue_released().await?;
    Ok(Json(
        json!({ "queued": queued, "waiting": push.queued().len() }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct AliasesBody {
    aliases: Vec<String>,
}

async fn known_mapping(state: &AppState, id: i64) -> Result<(), AdminError> {
    state.db.mapping(id).await?.ok_or(AdminError::NotFound)?;
    Ok(())
}

pub async fn aliases(
    _: Authorized,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<Vec<AliasEntry>>, AdminError> {
    known_mapping(&state, id).await?;
    Ok(Json(state.db.alias_entries(id).await?))
}

/// Replaces the aliases and wakes the matcher, since a new name can match a video already seen.
pub async fn set_aliases(
    _: Authorized,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(body): Json<AliasesBody>,
) -> Result<Json<Vec<AliasEntry>>, AdminError> {
    known_mapping(&state, id).await?;
    state.db.set_aliases(id, &body.aliases).await?;
    state.matcher.waker().notify_one();
    Ok(Json(state.db.alias_entries(id).await?))
}
