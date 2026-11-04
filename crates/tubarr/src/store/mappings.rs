//! Mappings: which Sonarr series or Radarr film comes from which YouTube channels, and the rules
//! that pick the right video.

use sqlx::types::Json;

use super::{Db, StoreError, duplicate_or, unix_now};

/// How videos of a mapping are matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum MappingKind {
    /// Season and episode numbers, found in the video title.
    Show,
    /// Listed on TVDB by air date, matched by upload date.
    Daily,
    Film,
}

/// What a show's official playlist is for.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, sqlx::Type, serde::Serialize, serde::Deserialize,
)]
#[sqlx(rename_all = "lowercase")]
#[serde(rename_all = "lowercase")]
pub enum PlaylistMode {
    /// Its videos are candidates for the title matcher, and titles give the episode number.
    /// Official playlists often hold promos, gaps or a wrong order, so this is the default.
    #[default]
    Source,
    /// The place in the playlist gives the episode number, for titles without one.
    Order,
}

/// The fields a caller sets. Shows and daily shows need `tvdb_id`, films need `tmdb_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewMapping {
    pub kind: MappingKind,
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub title: String,
    pub language: String,
    pub channel_ids: Vec<String>,
    pub episode_regex: Option<String>,
    pub excludes: Vec<String>,
    pub min_duration_secs: Option<i64>,
}

/// A stored mapping.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Mapping {
    pub id: i64,
    pub kind: MappingKind,
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
    pub title: String,
    pub language: String,
    #[sqlx(json)]
    pub channel_ids: Vec<String>,
    pub episode_regex: Option<String>,
    #[sqlx(json)]
    pub excludes: Vec<String>,
    pub min_duration_secs: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// `false` once the title lost its tag in Sonarr or Radarr, or left it. See [`crate::arr`].
    pub active: bool,
    /// Sonarr series id or Radarr movie id, set by the sync. `None` for a mapping made by hand.
    pub arr_id: Option<i64>,
    pub year: Option<i64>,
    pub imdb_id: Option<String>,
    /// Daily shows: the channel's offset from UTC in minutes. `None` means Pakistan (+05:00).
    pub utc_offset_mins: Option<i64>,
    /// The channel's IANA time zone, like "America/Chicago". Wins over `utc_offset_mins`.
    pub timezone: Option<String>,
    /// The show's official playlist: more videos for the title matcher, or the episode order.
    pub playlist_id: Option<String>,
    /// The season the playlist holds, in `order` mode.
    pub playlist_season: i64,
    /// The episode number of the playlist's first video, in `order` mode.
    pub playlist_offset: i64,
    pub playlist_mode: PlaylistMode,
    /// How many back episodes to release: the newest this many uploaded before `created_at`.
    /// Everything uploaded later always is. `None` releases every back episode.
    pub backfill_limit: Option<i64>,
    /// Shows: match by the running number in the title (`#2569`) against the number in TVDB's
    /// episode titles, for podcasts TVDB numbers by year.
    pub title_number: bool,
    /// Shows and daily shows: keep an `<episode> - Audio only.strm` next to each imported
    /// episode, see [`crate::hooks`].
    pub audio_version: bool,
    /// Shows and daily shows: save each imported episode's YouTube thumbnail next to it as
    /// `<episode>-thumb.jpg`, Jellyfin's episode image, see [`crate::hooks::thumbs`].
    pub episode_thumbnails: bool,
}

/// Matching options outside the title rules, set apart from [`NewMapping`] since most mappings
/// never need them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchOptions {
    pub utc_offset_mins: Option<i64>,
    pub timezone: Option<String>,
    pub playlist_id: Option<String>,
    pub playlist_season: i64,
    pub playlist_offset: i64,
    pub playlist_mode: PlaylistMode,
    pub title_number: bool,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self {
            utc_offset_mins: None,
            timezone: None,
            playlist_id: None,
            playlist_season: 1,
            playlist_offset: 1,
            playlist_mode: PlaylistMode::Source,
            title_number: false,
        }
    }
}

impl Mapping {
    /// A video podcast: a show matched by its running number, which is what the podcast preset
    /// turns on. The preset is not stored, so this is the rule the UI and `kind=podcast` follow.
    pub fn is_podcast(&self) -> bool {
        self.kind == MappingKind::Show && self.title_number
    }
}

impl NewMapping {
    fn describe(&self) -> String {
        match (self.tvdb_id, self.tmdb_id) {
            (Some(id), _) => format!("a mapping for TVDB id {id}"),
            (None, Some(id)) => format!("a mapping for TMDB id {id}"),
            (None, None) => "this mapping".to_owned(),
        }
    }
}

impl Db {
    pub async fn insert_mapping(&self, new: &NewMapping) -> Result<Mapping, StoreError> {
        let now = unix_now();
        sqlx::query_as(
            "INSERT INTO mappings (kind, tvdb_id, tmdb_id, title, language, channel_ids, \
             episode_regex, excludes, min_duration_secs, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             RETURNING *",
        )
        .bind(new.kind)
        .bind(new.tvdb_id)
        .bind(new.tmdb_id)
        .bind(&new.title)
        .bind(&new.language)
        .bind(Json(&new.channel_ids))
        .bind(&new.episode_regex)
        .bind(Json(&new.excludes))
        .bind(new.min_duration_secs)
        .bind(now)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| duplicate_or(e, || new.describe()))
    }

    /// Replaces every field of mapping `id`. `None` when no such mapping exists.
    pub async fn update_mapping(
        &self,
        id: i64,
        new: &NewMapping,
    ) -> Result<Option<Mapping>, StoreError> {
        sqlx::query_as(
            "UPDATE mappings SET kind = ?, tvdb_id = ?, tmdb_id = ?, title = ?, language = ?, \
             channel_ids = ?, episode_regex = ?, excludes = ?, min_duration_secs = ?, \
             updated_at = ? WHERE id = ? \
             RETURNING *",
        )
        .bind(new.kind)
        .bind(new.tvdb_id)
        .bind(new.tmdb_id)
        .bind(&new.title)
        .bind(&new.language)
        .bind(Json(&new.channel_ids))
        .bind(&new.episode_regex)
        .bind(Json(&new.excludes))
        .bind(new.min_duration_secs)
        .bind(unix_now())
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| duplicate_or(e, || new.describe()))
    }

    /// Sets the matching options of mapping `id`. `None` when no such mapping exists.
    pub async fn set_match_options(
        &self,
        id: i64,
        options: &MatchOptions,
    ) -> Result<Option<Mapping>, StoreError> {
        Ok(sqlx::query_as(
            "UPDATE mappings SET utc_offset_mins = ?, timezone = ?, playlist_id = ?, playlist_season = ?, \
             playlist_offset = ?, playlist_mode = ?, title_number = ?, updated_at = ? \
             WHERE id = ? RETURNING *",
        )
        .bind(options.utc_offset_mins)
        .bind(&options.timezone)
        .bind(&options.playlist_id)
        .bind(options.playlist_season)
        .bind(options.playlist_offset)
        .bind(options.playlist_mode)
        .bind(options.title_number)
        .bind(unix_now())
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Sets how many back episodes mapping `id` releases, `None` for all of them. `None` when no
    /// such mapping exists.
    pub async fn set_backfill_limit(
        &self,
        id: i64,
        limit: Option<i64>,
    ) -> Result<Option<Mapping>, StoreError> {
        Ok(sqlx::query_as(
            "UPDATE mappings SET backfill_limit = ?, updated_at = ? WHERE id = ? RETURNING *",
        )
        .bind(limit)
        .bind(unix_now())
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    pub async fn mapping(&self, id: i64) -> Result<Option<Mapping>, StoreError> {
        Ok(sqlx::query_as("SELECT * FROM mappings WHERE id = ?")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn mapping_by_tvdb(&self, tvdb_id: i64) -> Result<Option<Mapping>, StoreError> {
        Ok(sqlx::query_as("SELECT * FROM mappings WHERE tvdb_id = ?")
            .bind(tvdb_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    pub async fn mapping_by_tmdb(&self, tmdb_id: i64) -> Result<Option<Mapping>, StoreError> {
        Ok(sqlx::query_as("SELECT * FROM mappings WHERE tmdb_id = ?")
            .bind(tmdb_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// The show or daily show mapped to Sonarr series `series_id` (Sonarr's id, not the TVDB id).
    pub async fn mapping_by_sonarr_id(
        &self,
        series_id: i64,
    ) -> Result<Option<Mapping>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT * FROM mappings WHERE arr_id = ? AND kind IN ('show', 'daily') \
             ORDER BY id LIMIT 1",
        )
        .bind(series_id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Turns the audio only versions of mapping `id` on or off. `None` when no such mapping exists.
    pub async fn set_audio_version(
        &self,
        id: i64,
        on: bool,
    ) -> Result<Option<Mapping>, StoreError> {
        Ok(sqlx::query_as(
            "UPDATE mappings SET audio_version = ?, updated_at = ? WHERE id = ? RETURNING *",
        )
        .bind(on)
        .bind(unix_now())
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Turns the episode thumbnails of mapping `id` on or off. `None` when no such mapping exists.
    pub async fn set_episode_thumbnails(
        &self,
        id: i64,
        on: bool,
    ) -> Result<Option<Mapping>, StoreError> {
        Ok(sqlx::query_as(
            "UPDATE mappings SET episode_thumbnails = ?, updated_at = ? WHERE id = ? RETURNING *",
        )
        .bind(on)
        .bind(unix_now())
        .bind(id)
        .fetch_optional(&self.pool)
        .await?)
    }

    /// Every mapping, ordered by title.
    pub async fn mappings(&self) -> Result<Vec<Mapping>, StoreError> {
        Ok(
            sqlx::query_as("SELECT * FROM mappings ORDER BY title COLLATE NOCASE, id")
                .fetch_all(&self.pool)
                .await?,
        )
    }

    /// Deletes a mapping and its pins. `false` when no such mapping existed.
    pub async fn delete_mapping(&self, id: i64) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM mappings WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::store::test_db;

    pub fn drama() -> NewMapping {
        NewMapping {
            kind: MappingKind::Show,
            tvdb_id: Some(452_001),
            tmdb_id: None,
            title: "Kabhi Main Kabhi Tum".to_owned(),
            language: "Urdu".to_owned(),
            channel_ids: vec!["UC4JCksJF76g_MdzPVBJoC3Q".to_owned()],
            episode_regex: Some(r"Ep(?:isode)?\s*(?P<episode>\d+)".to_owned()),
            excludes: vec!["Teaser".to_owned(), "Promo".to_owned()],
            min_duration_secs: Some(25 * 60),
        }
    }

    fn film() -> NewMapping {
        NewMapping {
            kind: MappingKind::Film,
            tvdb_id: None,
            tmdb_id: Some(352_173),
            title: "Theri".to_owned(),
            language: "Hindi".to_owned(),
            channel_ids: vec![],
            episode_regex: None,
            excludes: vec![],
            min_duration_secs: Some(80 * 60),
        }
    }

    #[tokio::test]
    async fn insert_and_read_back() {
        let (db, _dir) = test_db::fresh().await;
        let stored = db.insert_mapping(&drama()).await.unwrap();
        assert_eq!(stored.title, "Kabhi Main Kabhi Tum");
        assert_eq!(stored.kind, MappingKind::Show);
        assert_eq!(stored.excludes, ["Teaser", "Promo"]);

        assert_eq!(db.mapping(stored.id).await.unwrap(), Some(stored.clone()));
        assert_eq!(db.mapping_by_tvdb(452_001).await.unwrap(), Some(stored));
        assert_eq!(db.mapping_by_tvdb(1).await.unwrap(), None);

        let theri = db.insert_mapping(&film()).await.unwrap();
        assert_eq!(db.mapping_by_tmdb(352_173).await.unwrap(), Some(theri));
        let titles: Vec<_> = db
            .mappings()
            .await
            .unwrap()
            .into_iter()
            .map(|m| m.title)
            .collect();
        assert_eq!(titles, ["Kabhi Main Kabhi Tum", "Theri"]);
    }

    #[tokio::test]
    async fn match_options_default_and_survive_an_update() {
        let (db, _dir) = test_db::fresh().await;
        let stored = db.insert_mapping(&drama()).await.unwrap();
        assert_eq!(stored.utc_offset_mins, None);
        assert_eq!((stored.playlist_season, stored.playlist_offset), (1, 1));
        assert_eq!(stored.playlist_mode, PlaylistMode::Source);
        assert!(!stored.title_number);
        let options = MatchOptions {
            utc_offset_mins: Some(330),
            timezone: Some("Asia/Kolkata".to_owned()),
            playlist_id: Some("PLtvf".to_owned()),
            playlist_season: 2,
            playlist_offset: 0,
            playlist_mode: PlaylistMode::Order,
            title_number: true,
        };
        let set = db
            .set_match_options(stored.id, &options)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(set.playlist_id.as_deref(), Some("PLtvf"));
        let edited = db
            .update_mapping(stored.id, &drama())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(edited.utc_offset_mins, Some(330));
        assert_eq!(edited.timezone.as_deref(), Some("Asia/Kolkata"));
        assert_eq!((edited.playlist_season, edited.playlist_offset), (2, 0));
        assert_eq!(edited.playlist_mode, PlaylistMode::Order);
        assert!(edited.title_number);
        assert!(db.set_match_options(999, &options).await.unwrap().is_none());
        let bad = MatchOptions {
            utc_offset_mins: Some(2000),
            ..MatchOptions::default()
        };
        assert!(db.set_match_options(stored.id, &bad).await.is_err());
    }

    #[tokio::test]
    async fn the_backfill_limit_starts_unset_and_survives_an_update() {
        let (db, _dir) = test_db::fresh().await;
        let stored = db.insert_mapping(&drama()).await.unwrap();
        assert_eq!(stored.backfill_limit, None);
        let set = db
            .set_backfill_limit(stored.id, Some(5))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(set.backfill_limit, Some(5));
        let edited = db
            .update_mapping(stored.id, &drama())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(edited.backfill_limit, Some(5));
        let cleared = db
            .set_backfill_limit(stored.id, None)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cleared.backfill_limit, None);
        assert!(db.set_backfill_limit(stored.id, Some(-1)).await.is_err());
        assert!(db.set_backfill_limit(999, Some(5)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn duplicate_tvdb_id_is_refused() {
        let (db, _dir) = test_db::fresh().await;
        db.insert_mapping(&drama()).await.unwrap();
        let err = db.insert_mapping(&drama()).await.unwrap_err();
        assert!(
            matches!(err, StoreError::Duplicate(ref w) if w.contains("452001")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn kind_needs_the_matching_id() {
        let (db, _dir) = test_db::fresh().await;
        let mut film_without_tmdb = film();
        film_without_tmdb.tmdb_id = None;
        assert!(db.insert_mapping(&film_without_tmdb).await.is_err());
        let mut show_without_tvdb = drama();
        show_without_tvdb.tvdb_id = None;
        assert!(db.insert_mapping(&show_without_tvdb).await.is_err());
    }

    #[tokio::test]
    async fn update_and_delete() {
        let (db, _dir) = test_db::fresh().await;
        let stored = db.insert_mapping(&drama()).await.unwrap();
        let mut changed = drama();
        changed.kind = MappingKind::Daily;
        changed.excludes.push("Highlights".to_owned());
        let updated = db
            .update_mapping(stored.id, &changed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(updated.kind, MappingKind::Daily);
        assert_eq!(updated.excludes.len(), 3);
        assert_eq!(updated.created_at, stored.created_at);
        assert!(db.update_mapping(9999, &changed).await.unwrap().is_none());

        assert!(db.delete_mapping(stored.id).await.unwrap());
        assert!(!db.delete_mapping(stored.id).await.unwrap());
        assert!(db.mapping(stored.id).await.unwrap().is_none());
    }
}
