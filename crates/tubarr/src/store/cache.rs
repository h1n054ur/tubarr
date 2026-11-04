//! The resolve cache on SQLite, so lookups survive a restart.
//!
//! A lookup is split in two: video metadata goes to `videos` and is kept for good, the stream half
//! (format addresses, sizes, headers, subtitle addresses) goes to `resolve_cache` and is served only
//! until the time the resolver's [`CachePolicy`](crate::resolver::CachePolicy) gave it.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sqlx::types::Json;

use super::channel_videos::take_uploaded_at;
use super::{Db, StoreError};
use crate::resolver::{ResolveCache, Resolved, StreamFormat, Subtitle, VideoMeta};

/// The stream half of a lookup, kept as JSON in `resolve_cache.formats`.
#[derive(Serialize, Deserialize)]
struct Streams {
    video: StreamFormat,
    audio: StreamFormat,
    subtitles: Vec<Subtitle>,
    /// The URLs' own `expire=`, Unix seconds.
    url_expires_at: Option<i64>,
}

/// A subtitle track as kept with the metadata. No address, those expire with the streams.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubtitleTrack {
    pub lang: String,
    pub automatic: bool,
    pub ext: String,
}

/// What is known about a video for good, whether or not its streams are still cached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredVideo {
    pub meta: VideoMeta,
    pub subtitles: Vec<SubtitleTrack>,
    /// Resolution label of the video format the lookup picked: its standard tier
    /// ([`StreamFormat::quality_height`]), not the raw pixel height.
    pub height: Option<i64>,
    /// Bytes of the video and audio formats, when yt-dlp gave both exactly.
    pub size: Option<i64>,
}

type MetaRow = (
    String,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
);
type StreamRow = (
    String,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Json<Streams>,
);
type VideoRow = (
    String,
    Option<i64>,
    Option<String>,
    Option<i64>,
    Option<String>,
    Json<Vec<SubtitleTrack>>,
    Option<i64>,
    Option<i64>,
);

fn meta_from_row(
    video_id: &str,
    (title, duration, upload_date, timestamp, channel_id): MetaRow,
) -> VideoMeta {
    VideoMeta {
        id: video_id.to_owned(),
        title,
        duration: duration.and_then(|d| u64::try_from(d).ok()),
        upload_date,
        timestamp,
        channel_id,
    }
}

impl Db {
    /// Stores a lookup: its metadata for good, its streams until `valid_until` (Unix seconds).
    /// Stream entries already expired at `now` are dropped in the same transaction.
    pub async fn store_resolved(
        &self,
        resolved: &Resolved,
        now: i64,
        valid_until: i64,
    ) -> Result<(), StoreError> {
        let streams = Streams {
            video: resolved.video.clone(),
            audio: resolved.audio.clone(),
            subtitles: resolved.subtitles.clone(),
            url_expires_at: resolved.expires_at.map(unix_secs),
        };

        let mut tx = self.pool.begin().await?;
        upsert_video(&mut tx, resolved, now).await?;
        sqlx::query(
            "INSERT INTO resolve_cache (video_id, formats, expires_at) VALUES (?, ?, ?) \
             ON CONFLICT (video_id) DO UPDATE SET formats = excluded.formats, \
             expires_at = excluded.expires_at",
        )
        .bind(&resolved.meta.id)
        .bind(Json(&streams))
        .bind(valid_until)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM resolve_cache WHERE expires_at <= ?")
            .bind(now)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Keeps only the metadata half of a lookup, for one whose streams expire too soon to cache.
    pub async fn store_video(&self, resolved: &Resolved, now: i64) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        upsert_video(&mut tx, resolved, now).await?;
        tx.commit().await?;
        Ok(())
    }

    /// The full lookup for `video_id` if its streams are still valid at `now` (Unix seconds).
    pub async fn cached_resolved(
        &self,
        video_id: &str,
        now: i64,
    ) -> Result<Option<Resolved>, StoreError> {
        let row: Option<StreamRow> = sqlx::query_as(
            "SELECT v.title, v.duration_secs, v.upload_date, v.uploaded_at, v.channel_id, \
                 r.formats \
                 FROM resolve_cache r JOIN videos v ON v.video_id = r.video_id \
                 WHERE r.video_id = ? AND r.expires_at > ?",
        )
        .bind(video_id)
        .bind(now)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(title, duration, upload_date, uploaded_at, channel_id, Json(streams))| Resolved {
                meta: meta_from_row(
                    video_id,
                    (title, duration, upload_date, uploaded_at, channel_id),
                ),
                video: streams.video,
                audio: streams.audio,
                subtitles: streams.subtitles,
                expires_at: streams
                    .url_expires_at
                    .and_then(|s| u64::try_from(s).ok())
                    .and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s))),
            },
        ))
    }

    /// The kept metadata for `video_id`, even after its streams expired.
    pub async fn video(&self, video_id: &str) -> Result<Option<StoredVideo>, StoreError> {
        let row: Option<VideoRow> = sqlx::query_as(
            "SELECT title, duration_secs, upload_date, uploaded_at, channel_id, subtitles, height, \
                 size \
                 FROM videos WHERE video_id = ?",
        )
        .bind(video_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(
            |(
                title,
                duration,
                upload_date,
                uploaded_at,
                channel_id,
                Json(subtitles),
                height,
                size,
            )| {
                StoredVideo {
                    meta: meta_from_row(
                        video_id,
                        (title, duration, upload_date, uploaded_at, channel_id),
                    ),
                    subtitles,
                    height,
                    size,
                }
            },
        ))
    }

    /// Drops every stream entry expired at `now`. Metadata stays. Returns how many went.
    pub async fn purge_resolved(&self, now: i64) -> Result<u64, StoreError> {
        let done = sqlx::query("DELETE FROM resolve_cache WHERE expires_at <= ?")
            .bind(now)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }
}

/// Video plus audio bytes, only when yt-dlp reported both exactly: a release size is never a guess.
fn exact_size(resolved: &Resolved) -> Option<i64> {
    let total = resolved
        .video
        .filesize?
        .checked_add(resolved.audio.filesize?)?;
    i64::try_from(total).ok().filter(|n| *n > 0)
}

/// Writes the `videos` row of a lookup, replacing an older one. Its exact upload time replaces a
/// guessed date on the channel's video (see [`take_uploaded_at`]).
async fn upsert_video(
    tx: &mut sqlx::SqliteConnection,
    resolved: &Resolved,
    now: i64,
) -> Result<(), StoreError> {
    let meta = &resolved.meta;
    let tracks: Vec<SubtitleTrack> = resolved
        .subtitles
        .iter()
        .map(|s| SubtitleTrack {
            lang: s.lang.clone(),
            automatic: s.automatic,
            ext: s.ext.clone(),
        })
        .collect();
    sqlx::query(
        "INSERT INTO videos (video_id, title, duration_secs, upload_date, uploaded_at, \
         channel_id, subtitles, height, size, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (video_id) DO UPDATE SET title = excluded.title, \
         duration_secs = excluded.duration_secs, upload_date = excluded.upload_date, \
         uploaded_at = coalesce(excluded.uploaded_at, uploaded_at), \
         channel_id = excluded.channel_id, subtitles = excluded.subtitles, \
         height = excluded.height, size = excluded.size, updated_at = excluded.updated_at",
    )
    .bind(&meta.id)
    .bind(&meta.title)
    .bind(meta.duration.and_then(|d| i64::try_from(d).ok()))
    .bind(&meta.upload_date)
    .bind(meta.timestamp)
    .bind(&meta.channel_id)
    .bind(Json(&tracks))
    .bind(resolved.video.quality_height().map(i64::from))
    .bind(exact_size(resolved))
    .bind(now)
    .execute(&mut *tx)
    .await?;
    take_uploaded_at(tx, &meta.id).await
}

/// A database problem must not fail a play, so it counts as a cache miss and gets logged.
impl ResolveCache for Db {
    async fn get(&self, video_id: &str, now: SystemTime) -> Option<Resolved> {
        self.cached_resolved(video_id, unix_secs(now))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(video_id, error = %e, "cannot read the resolve cache");
                None
            })
    }

    async fn put(&self, resolved: Resolved, now: SystemTime, valid_until: SystemTime) {
        if let Err(e) = self
            .store_resolved(&resolved, unix_secs(now), unix_secs(valid_until))
            .await
        {
            tracing::warn!(video_id = %resolved.meta.id, error = %e, "cannot write the resolve cache");
        }
    }
}

fn unix_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::store::test_db;

    const HOUR: Duration = Duration::from_secs(3600);

    fn resolved(id: &str, url_expiry: i64) -> Resolved {
        let stream = |format_id: &str, size: u64| StreamFormat {
            format_id: format_id.to_owned(),
            url: format!(
                "https://rr1.googlevideo.com/videoplayback?expire={url_expiry}&itag={format_id}"
            ),
            ext: Some("mp4".to_owned()),
            width: Some(1920),
            height: Some(1080),
            filesize: Some(size),
            filesize_approx: None,
            http_headers: BTreeMap::from([("User-Agent".to_owned(), "Mozilla/5.0".to_owned())]),
        };
        Resolved {
            meta: VideoMeta {
                id: id.to_owned(),
                title: "Kabhi Main Kabhi Tum Episode 1".to_owned(),
                duration: Some(2101),
                upload_date: Some("20240701".to_owned()),
                timestamp: Some(1_719_838_800),
                channel_id: Some("UC4JCksJF76g_MdzPVBJoC3Q".to_owned()),
            },
            video: stream("137", 412_345_678),
            audio: stream("140", 34_012_345),
            subtitles: vec![Subtitle {
                lang: "ur".to_owned(),
                automatic: false,
                ext: "vtt".to_owned(),
                url: "https://www.youtube.com/api/timedtext?lang=ur&fmt=vtt".to_owned(),
            }],
            expires_at: UNIX_EPOCH.checked_add(Duration::from_secs(url_expiry as u64)),
        }
    }

    #[tokio::test]
    async fn a_lookup_round_trips_until_it_expires() {
        let (db, _dir) = test_db::fresh().await;
        let now = 1_000_000;
        let r = resolved("0eD-GQCutAU", now + 6 * 3600);
        db.store_resolved(&r, now, now + 3600).await.unwrap();
        assert_eq!(
            db.cached_resolved("0eD-GQCutAU", now + 3599).await.unwrap(),
            Some(r)
        );
        assert_eq!(
            db.cached_resolved("0eD-GQCutAU", now + 3600).await.unwrap(),
            None
        );
        assert_eq!(db.cached_resolved("other", now).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_cropped_frame_is_kept_at_its_standard_tier() {
        let (db, _dir) = test_db::fresh().await;
        let mut r = resolved("crop", 50_000);
        r.video.height = Some(804);
        db.store_resolved(&r, 0, 3600).await.unwrap();
        assert_eq!(db.video("crop").await.unwrap().unwrap().height, Some(1080));
    }

    #[tokio::test]
    async fn metadata_outlives_the_streams() {
        let (db, _dir) = test_db::fresh().await;
        let r = resolved("a", 50_000);
        db.store_resolved(&r, 0, 3600).await.unwrap();
        assert_eq!(db.purge_resolved(3600).await.unwrap(), 1);
        assert_eq!(db.cached_resolved("a", 0).await.unwrap(), None);

        let kept = db.video("a").await.unwrap().unwrap();
        assert_eq!(kept.meta, r.meta);
        assert_eq!(kept.height, Some(1080));
        assert_eq!(kept.size, Some(412_345_678 + 34_012_345));
        assert_eq!(
            kept.subtitles,
            vec![SubtitleTrack {
                lang: "ur".to_owned(),
                automatic: false,
                ext: "vtt".to_owned()
            }]
        );
        assert_eq!(db.video("b").await.unwrap(), None);

        // An estimate is not a size.
        let mut approx = resolved("c", 50_000);
        approx.audio.filesize = None;
        approx.audio.filesize_approx = Some(34_000_000);
        db.store_video(&approx, 0).await.unwrap();
        assert_eq!(db.video("c").await.unwrap().unwrap().size, None);
    }

    #[tokio::test]
    async fn storing_replaces_and_prunes_expired_streams() {
        let (db, _dir) = test_db::fresh().await;
        db.store_resolved(&resolved("old", 9_000), 0, 3600)
            .await
            .unwrap();
        db.store_resolved(&resolved("a", 9_000), 0, 3600)
            .await
            .unwrap();
        let mut newer = resolved("a", 20_000);
        newer.meta.title = "Renamed".to_owned();
        db.store_resolved(&newer, 4000, 8000).await.unwrap();

        assert_eq!(db.cached_resolved("a", 7999).await.unwrap(), Some(newer));
        assert_eq!(db.video("a").await.unwrap().unwrap().meta.title, "Renamed");
        // "old" expired at 3600 and went when "a" was stored at 4000, its metadata did not.
        assert_eq!(db.purge_resolved(4000).await.unwrap(), 0);
        assert!(db.video("old").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn the_trait_uses_wall_clock_times() {
        let (db, _dir) = test_db::fresh().await;
        let now = UNIX_EPOCH + 400_000 * HOUR;
        let r = resolved("a", unix_secs(now + 6 * HOUR));
        ResolveCache::put(&db, r.clone(), now, now + HOUR).await;
        assert_eq!(ResolveCache::get(&db, "a", now + HOUR / 2).await, Some(r));
        assert_eq!(ResolveCache::get(&db, "a", now + HOUR).await, None);
    }
}
