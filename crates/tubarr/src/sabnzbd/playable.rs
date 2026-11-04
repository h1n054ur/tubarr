//! The check a grab makes before it writes anything: can Tubarr serve this video at all?

use crate::{AppState, play::parts, resolver::ResolveError};

/// Looks each video of a grab up (cached and rate limited, within the captions budget) and
/// returns why it cannot play when a lookup says so for certain. A slow or throttled lookup is
/// not proof of anything, so the grab goes ahead. A video found unplayable is marked, which takes
/// it out of the matcher and so out of the Newznab feed.
pub(super) async fn check(state: &AppState, video_ids: &str) -> Option<String> {
    let ids = parts::parse(video_ids).ok()?;
    let budget = state.config.subtitles.timeout;
    for id in ids {
        // Its own task, so a lookup still running at the deadline fills the cache for the
        // captions and the first play.
        let lookup = tokio::spawn({
            let resolver = state.resolver.clone();
            let id = id.clone();
            async move { resolver.resolve(&id).await }
        });
        match tokio::time::timeout(budget, lookup).await {
            Ok(Ok(Ok(_))) => {
                if let Err(e) = state.db.clear_unplayable(&id).await {
                    tracing::warn!(video_id = id, error = %e, "cannot clear the unplayable mark");
                }
            }
            Ok(Ok(Err(e))) if e.is_settled() => {
                let reason = reason(&e);
                match state
                    .db
                    .mark_unplayable(&id, &reason, e.retry_later())
                    .await
                {
                    Ok(removed) => {
                        tracing::warn!(
                            video_id = id,
                            removed,
                            "video cannot play, releases removed"
                        );
                    }
                    Err(e) => {
                        tracing::warn!(video_id = id, error = %e, "cannot mark the video unplayable");
                    }
                }
                return Some(reason);
            }
            Ok(Ok(Err(e))) => {
                tracing::info!(video_id = id, error = %e, "lookup failed, grabbing anyway");
            }
            Ok(Err(e)) => tracing::warn!(video_id = id, error = %e, "lookup task failed"),
            Err(_) => tracing::info!(video_id = id, "lookup too slow, grabbing anyway"),
        }
    }
    None
}

/// The fail message Sonarr and Radarr show.
fn reason(e: &ResolveError) -> String {
    format!("Tubarr cannot play this video: {e}")
}
