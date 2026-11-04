//! Videos the watcher found on mapped channels, and when each channel was last listed.

use std::{collections::HashMap, time::Duration};

use super::{Db, StoreError, unix_now};
use crate::resolver::MEMBERS_ONLY;

/// A watched video's upload time in SQL over `channel_videos c LEFT JOIN videos v`: the feed's or
/// list's time, else a lookup's upload date, else when the watcher first saw it.
macro_rules! published_at_sql {
    () => {
        "coalesce(c.published_at, \
           unixepoch(substr(v.upload_date, 1, 4) || '-' || substr(v.upload_date, 5, 2) \
             || '-' || substr(v.upload_date, 7, 2)), \
           c.first_seen)"
    };
}
pub(super) use published_at_sql;

/// Where a video was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum VideoSource {
    Rss,
    Listing,
}

/// Where the matcher is with a video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum MatchState {
    New,
    Matched,
    Ignored,
}

/// The skip reason of a YouTube Short.
pub const SHORT: &str = "a YouTube Short";

/// What a sighting says about whether the video can ever be a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Availability {
    /// The source cannot tell (a feed entry that is no Short): an earlier sighting's word holds.
    Unknown,
    /// An upload anyone can watch.
    Open,
    /// Never an episode, for this reason: a Short, members only, Premium only, private.
    Skip(&'static str),
    /// Live, an upcoming premiere, or a finished stream still processing: marked unplayable and
    /// looked up again later, like an upload still missing its formats.
    Live,
}

/// One sighting of a video in a feed or a channel list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeenVideo {
    pub video_id: String,
    pub channel_id: String,
    pub title: String,
    /// Unix seconds.
    pub published_at: Option<i64>,
    /// The date is a guess from text like "3 days ago".
    pub published_approx: bool,
    pub duration_secs: Option<i64>,
    pub source: VideoSource,
}

/// A stored video.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ChannelVideo {
    pub video_id: String,
    pub channel_id: String,
    pub title: String,
    pub published_at: Option<i64>,
    pub published_approx: bool,
    pub duration_secs: Option<i64>,
    pub source: VideoSource,
    pub first_seen: i64,
    pub last_seen: i64,
    pub match_state: MatchState,
    pub match_updated_at: Option<i64>,
    /// Why no matcher looks at it, when it can never be an episode.
    pub skip_reason: Option<String>,
}

/// A stored video with the gaps filled from a full lookup, as the show matcher reads it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct VideoOnChannel {
    pub video_id: String,
    pub channel_id: String,
    pub title: String,
    /// The feed's or a lookup's exact time, else the channel list's guess, else the lookup's
    /// upload day, else when the watcher first saw it.
    pub published_at: i64,
    /// Anything but an exact time.
    pub published_approx: bool,
    /// From the channel list, else from a lookup.
    pub duration_secs: Option<i64>,
}

/// Gives the channel's video the exact upload time a lookup stored for it, when its own date is
/// missing or a guess. An exact date it already has stays, and the time is never a guess again,
/// so a channel list seen later cannot move it back.
pub(super) async fn take_uploaded_at(
    conn: &mut sqlx::SqliteConnection,
    video_id: &str,
) -> Result<(), StoreError> {
    sqlx::query(
        "UPDATE channel_videos SET published_at = v.uploaded_at, published_approx = 0 \
         FROM videos v \
         WHERE v.video_id = channel_videos.video_id AND channel_videos.video_id = ? \
           AND v.uploaded_at IS NOT NULL \
           AND (channel_videos.published_at IS NULL OR channel_videos.published_approx)",
    )
    .bind(video_id)
    .execute(conn)
    .await?;
    Ok(())
}

impl Db {
    /// Records a sighting that says nothing about availability, as
    /// [`record_sighting`](Self::record_sighting) with [`Availability::Unknown`].
    pub async fn upsert_seen_video(&self, seen: &SeenVideo) -> Result<bool, StoreError> {
        self.record_sighting(seen, Availability::Unknown).await
    }

    /// Records a sighting. Returns `true` when the video was not known before. Seeing a video again
    /// keeps its match state, takes the newest title, fills in a missing duration, and lets an
    /// exact date (the feed's, or a lookup's upload time) replace a guessed one (never the other
    /// way round). A known `availability` sets
    /// or clears the skip reason, so a members-only upload made public later is matched then. An
    /// open video also loses an unplayable mark a lookup gave it for being members or Premium
    /// only, so the matcher looks it up again. A livestream is held as unplayable until a later
    /// lookup finds it finished.
    pub async fn record_sighting(
        &self,
        seen: &SeenVideo,
        availability: Availability,
    ) -> Result<bool, StoreError> {
        let now = unix_now();
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT INTO channel_videos (video_id, channel_id, title, published_at, \
             published_approx, duration_secs, source, first_seen, last_seen) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (video_id) DO NOTHING",
        )
        .bind(&seen.video_id)
        .bind(&seen.channel_id)
        .bind(&seen.title)
        .bind(seen.published_at)
        .bind(seen.published_approx)
        .bind(seen.duration_secs)
        .bind(seen.source)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            > 0;
        if !inserted {
            // The date moves only when there was none or an exact one replaces a guess.
            sqlx::query(
                "UPDATE channel_videos SET \
                   title = ?1, \
                   duration_secs = coalesce(?2, duration_secs), \
                   published_approx = CASE WHEN ?3 IS NOT NULL \
                     AND (published_at IS NULL OR (published_approx AND NOT ?4)) \
                     THEN ?4 ELSE published_approx END, \
                   published_at = CASE WHEN ?3 IS NOT NULL \
                     AND (published_at IS NULL OR (published_approx AND NOT ?4)) \
                     THEN ?3 ELSE published_at END, \
                   last_seen = ?5 \
                 WHERE video_id = ?6",
            )
            .bind(&seen.title)
            .bind(seen.duration_secs)
            .bind(seen.published_at)
            .bind(seen.published_approx)
            .bind(now)
            .bind(&seen.video_id)
            .execute(&mut *tx)
            .await?;
        }
        take_uploaded_at(&mut tx, &seen.video_id).await?;
        let skip = match availability {
            Availability::Unknown => None,
            Availability::Open | Availability::Live => Some(None),
            Availability::Skip(reason) => Some(Some(reason)),
        };
        if let Some(reason) = skip {
            sqlx::query("UPDATE channel_videos SET skip_reason = ? WHERE video_id = ?")
                .bind(reason)
                .bind(&seen.video_id)
                .execute(&mut *tx)
                .await?;
        }
        if matches!(availability, Availability::Open | Availability::Live) {
            sqlx::query(
                "DELETE FROM unplayable_videos \
                 WHERE video_id = ?1 AND substr(reason, 1, length(?2)) = ?2",
            )
            .bind(&seen.video_id)
            .bind(MEMBERS_ONLY)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        if availability == Availability::Live {
            self.hold_livestream(&seen.video_id).await?;
        }
        Ok(inserted)
    }

    pub async fn channel_video(&self, video_id: &str) -> Result<Option<ChannelVideo>, StoreError> {
        Ok(
            sqlx::query_as("SELECT * FROM channel_videos WHERE video_id = ?")
                .bind(video_id)
                .fetch_optional(&self.pool)
                .await?,
        )
    }

    /// Videos the matcher has not looked at, first seen at or after `since` (Unix seconds), oldest
    /// first.
    pub async fn unmatched_videos(&self, since: i64) -> Result<Vec<ChannelVideo>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT * FROM channel_videos WHERE match_state = 'new' AND first_seen >= ? \
             ORDER BY first_seen, video_id",
        )
        .bind(since)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Sets the matcher's verdict. `false` when the video is unknown.
    pub async fn set_match_state(
        &self,
        video_id: &str,
        state: MatchState,
    ) -> Result<bool, StoreError> {
        let done = sqlx::query(
            "UPDATE channel_videos SET match_state = ?, match_updated_at = ? WHERE video_id = ?",
        )
        .bind(state)
        .bind(unix_now())
        .bind(video_id)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Every known video on `channel_ids`, whatever its match state, by video id. Videos marked
    /// unplayable or with a skip reason are left out, so no matcher releases them.
    pub async fn videos_on_channels(
        &self,
        channel_ids: &[String],
    ) -> Result<Vec<VideoOnChannel>, StoreError> {
        Ok(sqlx::query_as(concat!(
            "SELECT c.video_id, c.channel_id, c.title, ",
            published_at_sql!(),
            " AS published_at, \
               (c.published_at IS NULL OR c.published_approx) AS published_approx, \
               coalesce(c.duration_secs, v.duration_secs) AS duration_secs \
             FROM channel_videos c LEFT JOIN videos v ON v.video_id = c.video_id \
             WHERE c.channel_id IN (SELECT value FROM json_each(?)) \
               AND c.video_id NOT IN (SELECT video_id FROM unplayable_videos) \
               AND c.skip_reason IS NULL \
             ORDER BY c.video_id",
        ))
        .bind(sqlx::types::Json(channel_ids))
        .fetch_all(&self.pool)
        .await?)
    }

    /// Brings the match state of every video on `channel_ids` in line with what the matchers
    /// wrote: `matched` with a release (as a single upload or one of its parts), `new` while a note
    /// says it is still being looked at, `ignored` otherwise (a capped note included). Returns how
    /// many changed.
    ///
    /// The statement holds the write lock while it runs, so it must stay quick: the parts are
    /// read out of the releases once, not again for every video.
    pub async fn refresh_match_states(&self, channel_ids: &[String]) -> Result<u64, StoreError> {
        let done = sqlx::query(
            "WITH parts AS ( \
               SELECT p.value FROM releases r, json_each(r.part_ids) p \
               WHERE r.part_ids IS NOT NULL), \
             verdict AS ( \
               SELECT c.video_id, CASE \
                 WHEN EXISTS (SELECT 1 FROM releases r WHERE r.video_id = c.video_id) \
                   OR c.video_id IN parts \
                   THEN 'matched' \
                 WHEN EXISTS (SELECT 1 FROM unmatched u \
                   WHERE u.video_id = c.video_id AND u.state <> 'capped') \
                   THEN 'new' \
                 ELSE 'ignored' END AS state \
               FROM channel_videos c \
               WHERE c.channel_id IN (SELECT value FROM json_each(?1))) \
             UPDATE channel_videos SET match_state = verdict.state, match_updated_at = ?2 \
             FROM verdict \
             WHERE channel_videos.video_id = verdict.video_id \
               AND channel_videos.match_state <> verdict.state",
        )
        .bind(sqlx::types::Json(channel_ids))
        .bind(unix_now())
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    /// Drops the watched videos no mapping can use any more: uploaded and last seen by the watcher
    /// longer than `retention` ago, with no release (alone or as a part), no pin, no note and no
    /// place in a listed playlist. The matcher's note retention is the window, so a dropped row is
    /// one that would get no note either, and a backfill that finds an old upload again keeps it
    /// for a whole window before it goes. Returns how many rows went.
    pub async fn prune_channel_videos(&self, retention: Duration) -> Result<u64, StoreError> {
        let cutoff =
            unix_now().saturating_sub(i64::try_from(retention.as_secs()).unwrap_or(i64::MAX));
        let done = sqlx::query(concat!(
            "DELETE FROM channel_videos WHERE video_id IN ( \
               SELECT c.video_id FROM channel_videos c \
               LEFT JOIN videos v ON v.video_id = c.video_id \
               WHERE c.last_seen < ?1 AND ",
            published_at_sql!(),
            " < ?1) \
             AND video_id NOT IN (SELECT video_id FROM releases) \
             AND video_id NOT IN (SELECT p.value FROM releases r, json_each(r.part_ids) p \
               WHERE r.part_ids IS NOT NULL) \
             AND video_id NOT IN (SELECT s.value FROM pins, \
               json_each('[\"' || replace(pins.video_id, ',', '\",\"') || '\"]') s) \
             AND video_id NOT IN (SELECT video_id FROM unmatched) \
             AND video_id NOT IN (SELECT video_id FROM playlist_items)"
        ))
        .bind(cutoff)
        .execute(&self.pool)
        .await?;
        Ok(done.rows_affected())
    }

    /// Every channel id named by any mapping, without repeats, sorted.
    pub async fn watched_channels(&self) -> Result<Vec<String>, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT DISTINCT c.value FROM mappings m, json_each(m.channel_ids) c \
             WHERE c.type = 'text' ORDER BY c.value",
        )
        .fetch_all(&self.pool)
        .await?)
    }

    /// When each channel was last listed in full, as Unix seconds.
    pub async fn channel_listing_times(&self) -> Result<HashMap<String, i64>, StoreError> {
        let rows: Vec<(String, i64)> =
            sqlx::query_as("SELECT channel_id, last_listing_at FROM channel_scans")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().collect())
    }

    pub async fn set_channel_listing_time(
        &self,
        channel_id: &str,
        at: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO channel_scans (channel_id, last_listing_at) VALUES (?, ?) \
             ON CONFLICT (channel_id) DO UPDATE SET last_listing_at = excluded.last_listing_at",
        )
        .bind(channel_id)
        .bind(at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Saves a channel's name, as read when a handle or link was resolved.
    pub async fn set_channel_title(&self, channel_id: &str, title: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO channel_titles (channel_id, title, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT (channel_id) DO UPDATE SET title = excluded.title, \
             updated_at = excluded.updated_at",
        )
        .bind(channel_id)
        .bind(title)
        .bind(unix_now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every saved channel name by channel id.
    pub async fn channel_titles(&self) -> Result<HashMap<String, String>, StoreError> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT channel_id, title FROM channel_titles")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{mappings::tests::drama, test_db};

    fn seen(id: &str, source: VideoSource) -> SeenVideo {
        SeenVideo {
            video_id: id.to_owned(),
            channel_id: "UC4JCksJF76g_MdzPVBJoC3Q".to_owned(),
            title: "Kabhi Main Kabhi Tum Episode 1".to_owned(),
            published_at: None,
            published_approx: false,
            duration_secs: None,
            source,
        }
    }

    #[tokio::test]
    async fn upsert_is_idempotent_and_keeps_the_match_state() {
        let (db, _dir) = test_db::fresh().await;
        let mut first = seen("0eD-GQCutAU", VideoSource::Rss);
        first.published_at = Some(1_760_000_000);
        assert!(db.upsert_seen_video(&first).await.unwrap());
        assert!(
            db.set_match_state("0eD-GQCutAU", MatchState::Matched)
                .await
                .unwrap()
        );

        // Seen again in a channel list: rough date and a duration, newer title.
        let mut again = seen("0eD-GQCutAU", VideoSource::Listing);
        again.title = "Kabhi Main Kabhi Tum Ep 1".to_owned();
        again.published_at = Some(1_759_900_000);
        again.published_approx = true;
        again.duration_secs = Some(2101);
        db.upsert_seen_video(&again).await.unwrap();
        db.upsert_seen_video(&again).await.unwrap();

        let stored = db.channel_video("0eD-GQCutAU").await.unwrap().unwrap();
        assert_eq!(stored.title, "Kabhi Main Kabhi Tum Ep 1");
        assert_eq!(stored.published_at, Some(1_760_000_000));
        assert!(!stored.published_approx);
        assert_eq!(stored.duration_secs, Some(2101));
        assert_eq!(stored.source, VideoSource::Rss);
        assert_eq!(stored.match_state, MatchState::Matched);
        assert!(stored.match_updated_at.is_some());
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM channel_videos")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn exact_date_replaces_a_guess() {
        let (db, _dir) = test_db::fresh().await;
        let mut guess = seen("ldmPWRb-XcM", VideoSource::Listing);
        guess.published_at = Some(1_759_000_000);
        guess.published_approx = true;
        db.upsert_seen_video(&guess).await.unwrap();
        let mut exact = seen("ldmPWRb-XcM", VideoSource::Rss);
        exact.published_at = Some(1_759_012_345);
        db.upsert_seen_video(&exact).await.unwrap();
        let stored = db.channel_video("ldmPWRb-XcM").await.unwrap().unwrap();
        assert_eq!(stored.published_at, Some(1_759_012_345));
        assert!(!stored.published_approx);
        assert_eq!(stored.source, VideoSource::Listing);
    }

    /// Stores a lookup of `id` that gave `timestamp` as its upload time.
    async fn look_up(db: &Db, id: &str, timestamp: Option<i64>) {
        sqlx::query(
            "INSERT INTO videos (video_id, title, upload_date, uploaded_at, updated_at) \
             VALUES (?, 't', '20251001', ?, 0) \
             ON CONFLICT (video_id) DO UPDATE SET uploaded_at = excluded.uploaded_at",
        )
        .bind(id)
        .bind(timestamp)
        .execute(db.pool())
        .await
        .unwrap();
        let mut conn = db.pool().acquire().await.unwrap();
        take_uploaded_at(&mut conn, id).await.unwrap();
    }

    #[tokio::test]
    async fn a_lookups_upload_time_replaces_a_guess_and_stays() {
        let (db, _dir) = test_db::fresh().await;
        let mut guess = seen("ldmPWRb-XcM", VideoSource::Listing);
        guess.published_at = Some(1_756_684_800);
        guess.published_approx = true;
        db.upsert_seen_video(&guess).await.unwrap();

        // A lookup with only a day leaves the guess alone.
        look_up(&db, "ldmPWRb-XcM", None).await;
        let stored = db.channel_video("ldmPWRb-XcM").await.unwrap().unwrap();
        assert_eq!(stored.published_at, Some(1_756_684_800));
        assert!(stored.published_approx);

        look_up(&db, "ldmPWRb-XcM", Some(1_759_330_123)).await;
        let stored = db.channel_video("ldmPWRb-XcM").await.unwrap().unwrap();
        assert_eq!(stored.published_at, Some(1_759_330_123));
        assert!(!stored.published_approx);

        // A later channel list guesses again: the exact time stays.
        guess.published_at = Some(1_757_000_000);
        db.upsert_seen_video(&guess).await.unwrap();
        let stored = db.channel_video("ldmPWRb-XcM").await.unwrap().unwrap();
        assert_eq!(stored.published_at, Some(1_759_330_123));
        assert!(!stored.published_approx);
    }

    #[tokio::test]
    async fn a_lookup_before_the_sighting_dates_it_and_a_feed_date_stays() {
        let (db, _dir) = test_db::fresh().await;
        look_up(&db, "ldmPWRb-XcM", Some(1_759_330_123)).await;
        let mut guess = seen("ldmPWRb-XcM", VideoSource::Listing);
        guess.published_at = Some(1_756_684_800);
        guess.published_approx = true;
        assert!(db.upsert_seen_video(&guess).await.unwrap());
        let stored = db.channel_video("ldmPWRb-XcM").await.unwrap().unwrap();
        assert_eq!(stored.published_at, Some(1_759_330_123));
        assert!(!stored.published_approx);

        // The feed's exact time is not replaced by a lookup's.
        let mut feed = seen("0eD-GQCutAU", VideoSource::Rss);
        feed.published_at = Some(1_759_330_000);
        db.upsert_seen_video(&feed).await.unwrap();
        look_up(&db, "0eD-GQCutAU", Some(1_759_330_123)).await;
        let stored = db.channel_video("0eD-GQCutAU").await.unwrap().unwrap();
        assert_eq!(stored.published_at, Some(1_759_330_000));
        assert!(!stored.published_approx);
    }

    #[tokio::test]
    async fn unmatched_lists_only_new_videos_since() {
        let (db, _dir) = test_db::fresh().await;
        for id in ["aaaaaaaaaaa", "bbbbbbbbbbb", "ccccccccccc"] {
            db.upsert_seen_video(&seen(id, VideoSource::Rss))
                .await
                .unwrap();
        }
        db.set_match_state("bbbbbbbbbbb", MatchState::Ignored)
            .await
            .unwrap();
        sqlx::query("UPDATE channel_videos SET first_seen = 100 WHERE video_id = 'ccccccccccc'")
            .execute(db.pool())
            .await
            .unwrap();
        let ids: Vec<_> = db
            .unmatched_videos(1000)
            .await
            .unwrap()
            .into_iter()
            .map(|v| v.video_id)
            .collect();
        assert_eq!(ids, ["aaaaaaaaaaa"]);
        assert_eq!(db.unmatched_videos(0).await.unwrap().len(), 2);
        assert!(
            !db.set_match_state("zzzzzzzzzzz", MatchState::Matched)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn watched_channels_are_distinct() {
        let (db, _dir) = test_db::fresh().await;
        assert!(db.watched_channels().await.unwrap().is_empty());
        db.insert_mapping(&drama()).await.unwrap();
        let mut other = drama();
        other.tvdb_id = Some(1);
        other
            .channel_ids
            .push("UCxxxxxxxxxxxxxxxxxxxxxx".to_owned());
        db.insert_mapping(&other).await.unwrap();
        assert_eq!(
            db.watched_channels().await.unwrap(),
            ["UC4JCksJF76g_MdzPVBJoC3Q", "UCxxxxxxxxxxxxxxxxxxxxxx"]
        );
    }

    #[tokio::test]
    async fn refresh_sets_the_state_from_releases_parts_and_notes() {
        let (db, _dir) = test_db::fresh().await;
        let mapping = db.insert_mapping(&drama()).await.unwrap();
        for id in [
            "single00001",
            "joined00001",
            "part0000001",
            "noted000001",
            "nothing0001",
        ] {
            db.upsert_seen_video(&seen(id, VideoSource::Rss))
                .await
                .unwrap();
        }
        let mut elsewhere = seen("elsewhere01", VideoSource::Rss);
        elsewhere.channel_id = "UCxxxxxxxxxxxxxxxxxxxxxx".to_owned();
        db.upsert_seen_video(&elsewhere).await.unwrap();
        for (episode, video, parts) in [
            (1, "single00001", None),
            (2, "joined00001", Some(r#"["joined00001","part0000001"]"#)),
        ] {
            sqlx::query(
                "INSERT INTO releases (mapping_id, video_id, season, episode, video_title, \
                 language, height, published_at, created_at, part_ids) \
                 VALUES (?, ?, 1, ?, 't', 'Urdu', 720, 0, 0, ?)",
            )
            .bind(mapping.id)
            .bind(video)
            .bind(episode)
            .bind(parts)
            .execute(db.pool())
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO unmatched (mapping_id, video_id, state, reason, created_at, updated_at) \
             VALUES (?, 'noted000001', 'unsure', 'r', 0, 0)",
        )
        .bind(mapping.id)
        .execute(db.pool())
        .await
        .unwrap();

        let channels = ["UC4JCksJF76g_MdzPVBJoC3Q".to_owned()];
        assert_eq!(db.refresh_match_states(&channels).await.unwrap(), 4);
        for (id, want) in [
            ("single00001", MatchState::Matched),
            ("joined00001", MatchState::Matched),
            ("part0000001", MatchState::Matched),
            ("noted000001", MatchState::New),
            ("nothing0001", MatchState::Ignored),
            ("elsewhere01", MatchState::New),
        ] {
            let got = db.channel_video(id).await.unwrap().unwrap().match_state;
            assert_eq!(got, want, "{id}");
        }
        assert_eq!(db.refresh_match_states(&channels).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn skipped_and_live_videos_reach_no_matcher_and_end_up_ignored() {
        let (db, _dir) = test_db::fresh().await;
        let channels = ["UC4JCksJF76g_MdzPVBJoC3Q".to_owned()];
        let on_channels = || async {
            db.videos_on_channels(&channels)
                .await
                .unwrap()
                .into_iter()
                .map(|v| v.video_id)
                .collect::<Vec<_>>()
        };
        let listed = |id| seen(id, VideoSource::Listing);
        db.record_sighting(&listed("episode0001"), Availability::Open)
            .await
            .unwrap();
        db.record_sighting(
            &seen("shortlink01", VideoSource::Rss),
            Availability::Skip(SHORT),
        )
        .await
        .unwrap();
        db.record_sighting(&listed("members0001"), Availability::Skip("members only"))
            .await
            .unwrap();
        db.record_sighting(&listed("livenow0001"), Availability::Live)
            .await
            .unwrap();
        assert_eq!(on_channels().await, ["episode0001"]);
        let stored = db.channel_video("members0001").await.unwrap().unwrap();
        assert_eq!(stored.skip_reason.as_deref(), Some("members only"));
        let held = db.unplayable_videos().await.unwrap();
        assert_eq!(held.len(), 1);
        assert_eq!(
            (
                held[0].video_id.as_str(),
                held[0].reason.as_str(),
                held[0].retry
            ),
            ("livenow0001", "livestream still processing", true)
        );

        db.refresh_match_states(&channels).await.unwrap();
        for id in ["episode0001", "shortlink01", "members0001", "livenow0001"] {
            let got = db.channel_video(id).await.unwrap().unwrap().match_state;
            assert_eq!(got, MatchState::Ignored, "{id}");
        }

        // A feed entry says nothing about members only, a later channel list showing the
        // upload open does.
        db.upsert_seen_video(&seen("members0001", VideoSource::Rss))
            .await
            .unwrap();
        assert_eq!(on_channels().await, ["episode0001"]);
        db.record_sighting(&listed("members0001"), Availability::Open)
            .await
            .unwrap();
        assert_eq!(on_channels().await, ["episode0001", "members0001"]);
    }

    #[tokio::test]
    async fn an_open_listing_clears_only_a_members_only_mark() {
        let (db, _dir) = test_db::fresh().await;
        let members =
            crate::resolver::ResolveError::MembersOnly("Join this channel".to_owned()).to_string();
        db.mark_unplayable("members0001", &members, true)
            .await
            .unwrap();
        db.mark_unplayable("gone0000001", "video unavailable: removed", false)
            .await
            .unwrap();
        let marked = || async {
            db.unplayable_videos()
                .await
                .unwrap()
                .into_iter()
                .map(|u| u.video_id)
                .collect::<Vec<_>>()
        };
        let listed = |id| seen(id, VideoSource::Listing);

        // A feed entry or a list that still shows it for members says nothing new.
        db.upsert_seen_video(&seen("members0001", VideoSource::Rss))
            .await
            .unwrap();
        db.record_sighting(&listed("members0001"), Availability::Skip("members only"))
            .await
            .unwrap();
        assert_eq!(marked().await.len(), 2);

        for id in ["members0001", "gone0000001"] {
            db.record_sighting(&listed(id), Availability::Open)
                .await
                .unwrap();
        }
        assert_eq!(marked().await, ["gone0000001"]);
    }

    #[tokio::test]
    async fn listing_times_round_trip() {
        let (db, _dir) = test_db::fresh().await;
        db.set_channel_listing_time("UC1", 10).await.unwrap();
        db.set_channel_listing_time("UC1", 20).await.unwrap();
        assert_eq!(db.channel_listing_times().await.unwrap()["UC1"], 20);
    }

    #[tokio::test]
    async fn channel_titles_are_kept_by_id() {
        let (db, _dir) = test_db::fresh().await;
        db.set_channel_title("UC1", "Old").await.unwrap();
        db.set_channel_title("UC1", "New").await.unwrap();
        assert_eq!(db.channel_titles().await.unwrap()["UC1"], "New");
    }
}
