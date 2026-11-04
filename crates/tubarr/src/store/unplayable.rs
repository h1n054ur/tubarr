//! Videos Tubarr cannot serve, and the releases still pointing at them or not yet looked up.

use super::{Db, StoreError, unix_now};
use crate::resolver::{MEMBERS_ONLY, ResolveError};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow, serde::Serialize)]
pub struct UnplayableVideo {
    pub video_id: String,
    pub reason: String,
    pub checked_at: i64,
    /// Looked up again later: YouTube had not offered the formats Tubarr plays yet, the
    /// livestream had not finished, or the video was for members or Premium only.
    pub retry: bool,
}

/// When marks with `retry` come due for another lookup, as Unix seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecheckDue {
    /// A mark for missing formats or an unfinished livestream checked at or before this.
    pub checked_before: i64,
    /// A members-only or Premium-only mark checked at or before this, when its video was first
    /// seen on a channel at or after `members_seen_since`.
    pub members_checked_before: i64,
    /// Members-only videos first seen before this wait for a channel list to show them open.
    pub members_seen_since: i64,
}

/// A release whose video (or one of its parts) is unplayable or was never looked up.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow, serde::Serialize)]
pub struct ReleaseCheck {
    pub release_id: i64,
    pub mapping_id: i64,
    pub video_id: String,
    pub video_title: String,
    pub season: Option<i64>,
    pub episode: Option<i64>,
    pub air_date: Option<String>,
    /// `unplayable` or `unresolved`.
    pub status: String,
    /// The unplayable reason, empty while unresolved.
    pub reason: String,
}

/// Every video id of a release, its parts included, as `rv (release_id, video_id)` rows.
macro_rules! with_release_videos {
    ($rest:literal) => {
        concat!(
            "WITH rv AS (SELECT r.id AS release_id, coalesce(p.value, r.video_id) AS video_id \
             FROM releases r LEFT JOIN json_each(r.part_ids) p) ",
            $rest
        )
    };
}

impl Db {
    /// Marks the video and removes every release that plays it (as the whole video or one part),
    /// so it leaves the Newznab feed. A mark with `retry` is looked up again once it is old enough
    /// ([`Self::unplayable_due`]). Returns how many releases went.
    pub async fn mark_unplayable(
        &self,
        video_id: &str,
        reason: &str,
        retry: bool,
    ) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO unplayable_videos (video_id, reason, checked_at, retry) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT (video_id) DO UPDATE SET reason = excluded.reason, \
             checked_at = excluded.checked_at, retry = excluded.retry",
        )
        .bind(video_id)
        .bind(reason)
        .bind(unix_now())
        .bind(retry)
        .execute(&mut *tx)
        .await?;
        let removed = sqlx::query(
            "DELETE FROM releases WHERE video_id = ?1 \
             OR ?1 IN (SELECT value FROM json_each(coalesce(part_ids, '[]')))",
        )
        .bind(video_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(removed)
    }

    /// Marks a livestream a channel list shows as not finished, with the reason a lookup of it
    /// gives, to be looked up again later. A video already marked keeps its mark and check time,
    /// so a stream seen in every list does not put its lookup off. `true` when newly marked.
    pub async fn hold_livestream(&self, video_id: &str) -> Result<bool, StoreError> {
        let marked: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM unplayable_videos WHERE video_id = ?)",
        )
        .bind(video_id)
        .fetch_one(&self.pool)
        .await?;
        if marked {
            return Ok(false);
        }
        self.mark_unplayable(video_id, &ResolveError::Live.to_string(), true)
            .await?;
        Ok(true)
    }

    /// `true` when the video was marked.
    pub async fn clear_unplayable(&self, video_id: &str) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM unplayable_videos WHERE video_id = ?")
            .bind(video_id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn unplayable_videos(&self) -> Result<Vec<UnplayableVideo>, StoreError> {
        Ok(
            sqlx::query_as("SELECT * FROM unplayable_videos ORDER BY checked_at DESC, video_id")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    /// Video ids marked with `retry` that are `due`, longest waiting first, at most `limit`.
    pub async fn unplayable_due(
        &self,
        due: RecheckDue,
        limit: i64,
    ) -> Result<Vec<String>, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT u.video_id FROM unplayable_videos u \
             LEFT JOIN channel_videos c ON c.video_id = u.video_id \
             WHERE u.retry AND CASE WHEN substr(u.reason, 1, length(?1)) = ?1 \
               THEN u.checked_at <= ?2 AND c.first_seen >= ?3 \
               ELSE u.checked_at <= ?4 END \
             ORDER BY u.checked_at, u.video_id LIMIT ?5",
        )
        .bind(MEMBERS_ONLY)
        .bind(due.members_checked_before)
        .bind(due.members_seen_since)
        .bind(due.checked_before)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Video ids of releases that no lookup has confirmed yet, oldest release first, at most
    /// `limit`. Releases made at an assumed height start out this way.
    pub async fn unverified_release_videos(&self, limit: i64) -> Result<Vec<String>, StoreError> {
        Ok(sqlx::query_scalar(with_release_videos!(
            "SELECT rv.video_id FROM rv JOIN releases r ON r.id = rv.release_id \
             WHERE rv.video_id NOT IN (SELECT video_id FROM videos) \
               AND rv.video_id NOT IN (SELECT video_id FROM unplayable_videos) \
             GROUP BY rv.video_id ORDER BY min(r.created_at), rv.video_id LIMIT ?"
        ))
        .bind(limit)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Releases with an unplayable or never looked up video, unplayable first.
    pub async fn release_checks(&self) -> Result<Vec<ReleaseCheck>, StoreError> {
        Ok(sqlx::query_as(with_release_videos!(
            "SELECT r.id AS release_id, r.mapping_id, rv.video_id, r.video_title, r.season, \
               r.episode, r.air_date, \
               CASE WHEN u.video_id IS NOT NULL THEN 'unplayable' ELSE 'unresolved' END AS status, \
               coalesce(u.reason, '') AS reason \
             FROM rv JOIN releases r ON r.id = rv.release_id \
             LEFT JOIN unplayable_videos u ON u.video_id = rv.video_id \
             WHERE u.video_id IS NOT NULL OR rv.video_id NOT IN (SELECT video_id FROM videos) \
             ORDER BY u.video_id IS NULL, r.created_at, r.id, rv.video_id"
        ))
        .fetch_all(&self.pool)
        .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::RecheckDue;
    use crate::resolver::ResolveError;
    use crate::store::{Availability, SeenVideo, VideoSource, test_db};

    /// Every kind of mark due once checked at or before `at`, whenever its video was first seen.
    fn due(at: i64) -> RecheckDue {
        RecheckDue {
            checked_before: at,
            members_checked_before: at,
            members_seen_since: 0,
        }
    }

    #[tokio::test]
    async fn mark_list_and_clear() {
        let (db, _dir) = test_db::fresh().await;
        db.mark_unplayable("aaaaaaaaaaa", "no AAC audio", true)
            .await
            .unwrap();
        db.mark_unplayable("aaaaaaaaaaa", "gone", false)
            .await
            .unwrap();
        let all = db.unplayable_videos().await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].reason, "gone");
        assert!(!all[0].retry);
        assert!(db.clear_unplayable("aaaaaaaaaaa").await.unwrap());
        assert!(!db.clear_unplayable("aaaaaaaaaaa").await.unwrap());
        assert!(db.unplayable_videos().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_retry_marks_come_due() {
        let (db, _dir) = test_db::fresh().await;
        db.mark_unplayable("aaaaaaaaaaa", "no H.264 video", true)
            .await
            .unwrap();
        db.mark_unplayable("bbbbbbbbbbb", "gone", false)
            .await
            .unwrap();
        let now = crate::store::unix_now();
        assert_eq!(
            db.unplayable_due(due(now), 10).await.unwrap(),
            ["aaaaaaaaaaa"]
        );
        assert!(
            db.unplayable_due(due(now - 60), 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_livestream_hold_keeps_its_first_check_time_and_any_other_mark() {
        let (db, _dir) = test_db::fresh().await;
        assert!(db.hold_livestream("aaaaaaaaaaa").await.unwrap());
        sqlx::query("UPDATE unplayable_videos SET checked_at = 100")
            .execute(db.pool())
            .await
            .unwrap();
        assert!(!db.hold_livestream("aaaaaaaaaaa").await.unwrap());
        db.mark_unplayable("bbbbbbbbbbb", "gone", false)
            .await
            .unwrap();
        assert!(!db.hold_livestream("bbbbbbbbbbb").await.unwrap());
        let all = db.unplayable_videos().await.unwrap();
        let live = all.iter().find(|u| u.video_id == "aaaaaaaaaaa").unwrap();
        assert_eq!(
            (live.reason.as_str(), live.checked_at, live.retry),
            ("livestream still processing", 100, true)
        );
        let gone = all.iter().find(|u| u.video_id == "bbbbbbbbbbb").unwrap();
        assert_eq!((gone.reason.as_str(), gone.retry), ("gone", false));
        assert_eq!(
            db.unplayable_due(due(200), 10).await.unwrap(),
            ["aaaaaaaaaaa"]
        );
    }

    #[tokio::test]
    async fn members_only_marks_come_due_daily_while_the_video_is_new() {
        let (db, _dir) = test_db::fresh().await;
        let members = ResolveError::MembersOnly("Join this channel".to_owned()).to_string();
        for (id, first_seen) in [("members0001", 1_000), ("members0002", 10)] {
            db.record_sighting(
                &SeenVideo {
                    video_id: id.to_owned(),
                    channel_id: "UC4JCksJF76g_MdzPVBJoC3Q".to_owned(),
                    title: "#2569 - Bill Thompson".to_owned(),
                    published_at: None,
                    published_approx: false,
                    duration_secs: Some(9000),
                    source: VideoSource::Rss,
                },
                Availability::Unknown,
            )
            .await
            .unwrap();
            sqlx::query("UPDATE channel_videos SET first_seen = ? WHERE video_id = ?")
                .bind(first_seen)
                .bind(id)
                .execute(db.pool())
                .await
                .unwrap();
        }
        // Never seen on a channel: nothing would match it anyway.
        for id in ["members0001", "members0002", "unknown0001"] {
            db.mark_unplayable(id, &members, true).await.unwrap();
        }
        db.mark_unplayable("formats0001", "no H.264 video", true)
            .await
            .unwrap();
        sqlx::query("UPDATE unplayable_videos SET checked_at = 500")
            .execute(db.pool())
            .await
            .unwrap();

        let at = |checked_before, members_checked_before| RecheckDue {
            checked_before,
            members_checked_before,
            members_seen_since: 100,
        };
        // Six hours on, only the missing formats are looked up again.
        assert_eq!(
            db.unplayable_due(at(500, 499), 10).await.unwrap(),
            ["formats0001"]
        );
        // A day on, the members-only video first seen in the window is too.
        assert_eq!(
            db.unplayable_due(at(500, 500), 10).await.unwrap(),
            ["formats0001", "members0001"]
        );
    }
}
