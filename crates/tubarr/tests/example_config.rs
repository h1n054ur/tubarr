//! The shipped example config must load as it is, so a renamed or removed key shows up here.

use tubarr::config::Config;

const EXAMPLE: &str = include_str!("../../../config/tubarr.example.toml");

#[test]
fn example_config_loads() {
    let config = Config::from_toml(EXAMPLE).expect("config/tubarr.example.toml loads");
    assert_eq!(config.listen.port(), 8480);
    assert!(config.sonarr.is_none() && config.api_key.is_none());
    assert!(config.channels.is_empty());
    assert_eq!(config.watcher.backfill_days, 14);
}

/// The commented-out channel table loads once its lines are uncommented.
#[test]
fn example_channel_table_loads() {
    let start = EXAMPLE
        .find("# [channels.UC4JCksJF76g_MdzPVBJoC3Q]")
        .expect("the example channel table");
    let block: String = EXAMPLE[start..]
        .lines()
        .take(5)
        .map(|line| format!("{}\n", line.trim_start_matches("# ")))
        .collect();
    let text = format!("public_url = \"http://tubarr.example.lan:8480\"\n{block}");
    let config = Config::from_toml(&text).expect("the example channel table loads");
    assert_eq!(
        config.channels.language("UC4JCksJF76g_MdzPVBJoC3Q"),
        Some("Urdu")
    );
    assert!(config.channels.listing_sizes().is_empty());
}
