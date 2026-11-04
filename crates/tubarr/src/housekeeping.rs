//! Keeps the stores that grow with a library's history bounded: the download history Sonarr and
//! Radarr poll (`[arr] grab_retention`), the Activity page's events (`[activity] retention`), the
//! watched videos no mapping can use (`[matcher] note_retention`) and the sample tables in
//! `[play] index_dir` (`index_max_bytes`, `index_max_idle`). Unmatched notes are bounded by the matcher itself. A pass runs a few minutes
//! after start and then every [`EVERY`].
//!
//! Dropping watched videos changes what the matcher's per-mapping digest reads, so the next run
//! matches each affected mapping once and then leaves it alone again.

use std::time::{Duration, SystemTime};

use crate::{
    config::Config,
    play::table::{self, IndexUsage},
    store::{Db, StoreError},
};

/// Time between two passes. Rows cross a retention line one upload at a time, so a pass every few
/// hours batches them without letting anything grow far.
pub const EVERY: Duration = Duration::from_secs(6 * 60 * 60);

/// The first pass waits this long, so it never adds to a busy start.
const START_DELAY: Duration = Duration::from_secs(10 * 60);

/// What one pass removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Swept {
    pub grabs: u64,
    pub activity_events: u64,
    pub channel_videos: u64,
    pub tables: IndexUsage,
}

/// Runs [`pass`] forever.
pub async fn run(db: Db, config: std::sync::Arc<Config>) {
    tokio::time::sleep(START_DELAY).await;
    loop {
        match pass(&db, &config).await {
            Ok(s) => tracing::info!(
                grabs = s.grabs,
                activity_events = s.activity_events,
                channel_videos = s.channel_videos,
                tables = s.tables.files,
                table_bytes = s.tables.bytes,
                "housekeeping done"
            ),
            Err(e) => tracing::warn!(error = %e, "housekeeping failed, trying again later"),
        }
        tokio::time::sleep(EVERY).await;
    }
}

/// One pass over every store. A zero retention or limit leaves that store alone.
pub async fn pass(db: &Db, config: &Config) -> Result<Swept, StoreError> {
    let grabs = match config.arr.grab_retention {
        d if d.is_zero() => 0,
        d => db.prune_grabs(d).await?,
    };
    let activity_events = match config.activity.retention {
        d if d.is_zero() => 0,
        d => db.prune_activity(d).await?,
    };
    let channel_videos = match config.matcher.note_retention {
        d if d.is_zero() => 0,
        d => db.prune_channel_videos(d).await?,
    };
    let dir = config.play_index_dir();
    let (max_bytes, max_idle) = (config.play.index_max_bytes, config.play.index_max_idle);
    let tables = tokio::task::spawn_blocking(move || {
        table::sweep(&dir, max_bytes, max_idle, SystemTime::now())
    })
    .await
    .unwrap_or_default();
    Ok(Swept {
        grabs,
        activity_events,
        channel_videos,
        tables,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{StoredEvent, test_db, unix_now};

    async fn config(dir: &std::path::Path, activity: &str) -> Config {
        Config::from_toml(&format!(
            "public_url = \"http://localhost:8480\"\ndata_dir = {dir:?}\n[activity]\n{activity}\n"
        ))
        .unwrap()
    }

    #[tokio::test]
    async fn a_pass_drops_activity_past_its_retention() {
        let (db, dir) = test_db::fresh().await;
        let now = unix_now();
        let event = |at| StoredEvent {
            at,
            kind: "play".to_owned(),
            data: "{}".to_owned(),
        };
        db.insert_activity(&[
            event(now - 31 * 86_400),
            event(now - 29 * 86_400),
            event(now),
        ])
        .await
        .unwrap();

        let kept = pass(&db, &config(dir.path(), "retention = \"0s\"").await)
            .await
            .unwrap();
        assert_eq!(kept.activity_events, 0);
        assert_eq!(db.count_activity(None).await.unwrap(), 3);

        let swept = pass(&db, &config(dir.path(), "").await).await.unwrap();
        assert_eq!(swept.activity_events, 1, "default 30 days");
        assert_eq!(db.count_activity(None).await.unwrap(), 2);
    }
}
