//! Tubarr: YouTube as a source for Sonarr and Radarr.
//!
//! The binary in `main.rs` loads the config, opens the store and serves [`app`]. Everything a test
//! needs is reachable from here.

pub mod activity;
pub mod admin;
pub mod api;
pub mod arr;
pub mod auth;
pub mod binaries;
pub mod channels;
pub mod config;
pub mod drafts;
pub mod health;
pub mod healthcheck;
pub mod hooks;
pub mod housekeeping;
pub mod jellyfin;
pub mod matcher;
pub mod metrics;
pub mod newznab;
pub mod notify;
pub mod play;
pub mod ratelimit;
pub mod resolver;
pub mod sabnzbd;
pub mod security_headers;
pub mod store;
pub mod subtitles;
pub mod watcher;

use std::sync::Arc;

/// The running version, from the crate version. See "Versions and releases" in the README.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The commit the binary was built from, when the build passed one in (the image build does).
pub const GIT_SHA: Option<&str> = option_env!("TUBARR_BUILD_SHA");

use axum::{Router, routing::get};

use crate::{
    activity::ActivityLog,
    arr::{ArrConnections, AutoPush, FeedLinks},
    auth::Auth,
    binaries::{BinaryManager, Tool},
    config::Config,
    drafts::DraftHelper,
    health::{Events, Health},
    hooks::Thumbs,
    jellyfin::Jellyfin,
    matcher::ShowMatcher,
    play::PlayState,
    ratelimit::{ProcessSlots, RateLimiter},
    resolver::{Resolver, YtDlp},
    store::Db,
    watcher::{Watcher, handles::ChannelLookup},
};

/// What every request handler can reach.
#[derive(Debug, Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub db: Db,
    /// The Sonarr and Radarr connections in use. Starts with the config file's; `main` adds the
    /// saved ones and the Settings page changes them while running.
    pub arrs: ArrConnections,
    /// yt-dlp and deno: where to run them from, their versions and updates.
    pub binaries: Arc<BinaryManager>,
    /// The one YouTube lookup budget, shared by the resolver and anything else that asks YouTube.
    pub limiter: Arc<RateLimiter>,
    /// Video id to stream addresses, cached in the database.
    pub resolver: Arc<Resolver<Db>>,
    /// Finds new uploads on mapped channels. `main` starts its loops.
    pub watcher: Arc<Watcher>,
    /// Turns channel handles and links typed in the UI into channel ids.
    pub channel_lookup: Arc<ChannelLookup>,
    /// The `/play` relay: virtual files, the fragment cache and the client for YouTube.
    pub play: Arc<PlayState<Db>>,
    /// The canary and the health events. `main` starts the canary loop.
    pub health: Arc<Health>,
    /// Matches numbered shows. `main` starts its loop, the watcher and the arr sync wake it.
    pub matcher: Arc<ShowMatcher>,
    /// Searches, pushes and play starts for the Activity page.
    pub activity: Arc<ActivityLog>,
    /// Pushes what the matcher first releases to the arrs connected at the time. `main` starts its
    /// loop. `None` with `arr.auto_push` off or no `api_key_file` (the feed's links need the key).
    pub push: Option<Arc<AutoPush>>,
    /// Checks TVDB and TMDB (through Sonarr and Radarr) and drafts missing titles.
    pub drafts: Arc<DraftHelper>,
    /// Users, sessions and sign-in for the web UI.
    pub auth: Arc<Auth>,
    /// Merges audio only versions into their episodes. `main` starts its worker. `None` without
    /// `[jellyfin]`.
    pub jellyfin: Option<Arc<Jellyfin>>,
    /// Fetches the YouTube thumbnails saved as episode images, see [`hooks::thumbs`].
    pub thumbs: Arc<Thumbs>,
}

impl AppState {
    /// Builds the limiter and the resolver from `config`, running the binaries `binaries` points
    /// at and caching lookups in `db`. Fails only if an HTTP client (YouTube, the arrs, the
    /// OpenID Connect provider) cannot be built.
    pub fn new(
        config: Config,
        db: Db,
        binaries: Arc<BinaryManager>,
    ) -> Result<Self, reqwest::Error> {
        let limiter = Arc::new(RateLimiter::new(config.rate_limit_config()));
        let events = Events::new();
        let ytdlp = YtDlp {
            binary: binaries.path(Tool::YtDlp),
            deno: Some(binaries.path(Tool::Deno)),
            timeout: config.lookup_timeout,
            processes: Arc::new(ProcessSlots::new(config.rate_limit.processes)),
        };
        let arrs = ArrConnections::from_config(&config);
        let wake = Arc::new(tokio::sync::Notify::new());
        let watcher = Watcher::new(
            config.watcher.clone(),
            db.clone(),
            limiter.clone(),
            ytdlp.clone(),
            config.rate_limit.throttle_pause,
        )
        .with_found(wake.clone())
        .with_channels(config.films.channels.clone())
        .with_listing_sizes(config.channels.listing_sizes());
        let channel_lookup = Arc::new(ChannelLookup::new(
            &config.watcher.feed_url,
            limiter.clone(),
            config.rate_limit.throttle_pause,
        )?);
        let resolver = Resolver::new(ytdlp, limiter.clone(), db.clone())
            .with_policy(config.cache_policy())
            .with_throttle_pause(config.rate_limit.throttle_pause)
            .with_fallback(config.fallback_steps())
            .with_events(events.clone());
        let resolver = Arc::new(resolver);
        let health = Arc::new(Health::new(config.canary.clone(), events));
        let play = Arc::new(
            PlayState::new(resolver.clone(), config.play_config())?
                .with_index_dir(config.play_index_dir())
                .with_prebuild_store(db.clone())
                .with_release_check(db.clone()),
        );
        let mut matcher = ShowMatcher::new(
            db.clone(),
            resolver.clone(),
            arrs.clone(),
            config.matcher.clone(),
            wake,
        )
        .with_channel_heights(config.channels.assumed_heights());
        let activity = Arc::new(ActivityLog::new(db.clone()));
        let push = match &config.api_key {
            _ if !config.arr.auto_push => None,
            None => {
                if config.sonarr.is_some() || config.radarr.is_some() {
                    tracing::warn!(
                        "arr.auto_push is on but api_key_file is not set: nothing is pushed"
                    );
                }
                None
            }
            Some(key) => Some(Arc::new(AutoPush::new(
                arrs.clone(),
                &config.arr,
                FeedLinks {
                    public_url: config.public_url.clone(),
                    api_key: key.clone(),
                    report_size: config.newznab.report_size,
                },
                db.clone(),
                activity.clone(),
            )?)),
        };
        if let Some(push) = &push {
            matcher = matcher.with_auto_push(push.clone());
        }
        let matcher = matcher.with_films(config.arr.tag.clone(), config.films.clone());
        let matcher = Arc::new(matcher);
        let drafts = Arc::new(DraftHelper::new(
            db.clone(),
            &config.drafts,
            config.channels.clone(),
            arrs.clone(),
        )?);
        let auth = Arc::new(Auth::new(config.auth.clone(), db.clone())?);
        let jellyfin = config
            .jellyfin
            .clone()
            .map(|j| Jellyfin::new(j, db.clone()).map(Arc::new))
            .transpose()?;
        let thumbs = Arc::new(Thumbs::new(config.hooks.thumbnail_url.clone())?);
        Ok(Self {
            config: Arc::new(config),
            db,
            arrs,
            binaries,
            limiter,
            resolver,
            play,
            watcher: Arc::new(watcher),
            channel_lookup,
            health,
            matcher,
            activity,
            push,
            drafts,
            auth,
            jellyfin,
            thumbs,
        })
    }
}

/// Builds the HTTP router.
pub fn app(state: AppState) -> Router {
    let log = state.activity.clone();
    let access = Arc::new(auth::PlayAccess::from_config(&state.config.auth));
    let play = play::router(state.play.clone())
        .layer(axum::middleware::from_fn(move |request, next| {
            activity::record_play(log.clone(), request, next)
        }))
        .layer(axum::middleware::from_fn(move |request, next| {
            auth::play_guard(access.clone(), request, next)
        }))
        .layer(axum::middleware::from_fn(security_headers::nosniff));
    Router::new()
        .route("/healthz", get(healthz))
        .merge(api::router())
        .merge(health::router())
        .merge(metrics::router())
        .merge(admin::router())
        .merge(auth::router())
        .merge(hooks::router())
        .fallback(admin::ui::serve)
        .layer(axum::middleware::from_fn(security_headers::all))
        .with_state(state)
        .merge(play)
}

async fn healthz() -> &'static str {
    "ok"
}
