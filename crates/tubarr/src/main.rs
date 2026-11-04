use std::{io::IsTerminal as _, path::PathBuf, sync::Arc, time::Duration};

use anyhow::Context;
use clap::Parser;
use tracing_subscriber::EnvFilter;
use tubarr::{AppState, binaries::BinaryManager, config::Config, store::Db};

/// YouTube as a source for Sonarr and Radarr.
#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    /// Path to the TOML config file. Only `public_url` is required, every other key has a default.
    #[arg(long, env = "TUBARR_CONFIG", default_value = "/config/tubarr.toml")]
    config: PathBuf,

    /// Ask the running server's /healthz (on loopback, at the configured port) and exit 0 when it
    /// answers 200, else 1. For the container's HEALTHCHECK, which has no curl.
    #[arg(long)]
    healthcheck: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if cli.healthcheck {
        let config = Config::load(&cli.config)?;
        let addr = tubarr::healthcheck::probe_addr(config.listen);
        tubarr::healthcheck::probe(addr, Duration::from_secs(5)).await?;
        return Ok(());
    }

    tracing_subscriber::fmt()
        // Colours only on a terminal, so `docker logs` gets plain lines. A non-empty NO_COLOR
        // turns them off on a terminal too, as tracing-subscriber does by default.
        .with_ansi(
            std::io::stdout().is_terminal()
                && std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()),
        )
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::load(&cli.config)?;
    if let Some(warning) = config.auth.play_networks_warning() {
        tracing::warn!("{warning}");
    }
    tubarr::health::dirs::warn_unwritable(&config);
    let db_path = config.database_path();
    let db = Db::open_backed_up(&db_path, config.backup).await?;
    tracing::info!(database = %db_path.display(), "database ready");
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone())?);

    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .with_context(|| format!("cannot listen on {}", config.listen))?;
    tracing::info!(listen = %config.listen, config = %cli.config.display(), "tubarr started");

    let state =
        AppState::new(config, db, binaries).context("cannot build the HTTP client for YouTube")?;
    state
        .auth
        .announce_setup()
        .await
        .context("cannot count users")?;
    // Sonarr and Radarr saved from the Settings page, for each the config file does not set.
    state.arrs.load_saved(&state.db).await?;
    // Idle while no arr is connected. A down arr only logs a warning on each round, Tubarr keeps
    // serving.
    let arr_sync = tubarr::arr::ArrSync::new(&state.config, state.arrs.clone(), state.db.clone())
        .context("cannot build the HTTP client for the arrs")?;
    if let Some(target) = &state.config.notify {
        tubarr::notify::spawn(target, state.health.events(), state.db.clone())
            .context("cannot build the HTTP client for Discord")?;
    }
    tokio::spawn(state.binaries.clone().run(state.health.events().clone()));
    tokio::spawn(state.watcher.clone().run());
    state.play.remember_indexed_videos().await;
    let play = state.play.clone();
    tokio::spawn(async move { play.resume_prebuilds().await });
    tokio::spawn(
        state
            .health
            .clone()
            .run_canary(state.resolver.clone(), state.binaries.clone()),
    );
    tokio::spawn(state.matcher.clone().run());
    tokio::spawn(tubarr::housekeeping::run(
        state.db.clone(),
        state.config.clone(),
    ));
    if let Some(push) = &state.push {
        tokio::spawn(push.clone().run());
    }
    if let Some(jellyfin) = &state.jellyfin {
        tokio::spawn(jellyfin.clone().run());
    }
    tokio::spawn(arr_sync.with_synced(state.matcher.waker()).run());

    // The peer address, through auth.trusted_proxies, feeds auth.local_bypass, the login and setup
    // code throttles and auth.play_allowed_networks.
    let app = tubarr::app(state).into_make_service_with_connect_info::<std::net::SocketAddr>();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
        .context("server error")
}

/// Resolves on Ctrl+C or SIGTERM (what Docker sends on stop).
async fn shutdown() {
    let ctrl_c = async {
        tokio::signal::ctrl_c().await.ok();
    };
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    tokio::select! {
        () = ctrl_c => {},
        () = term => {},
    }
    tracing::info!("shutting down");
}
