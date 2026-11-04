//! Releases: videos the matcher accepted for an episode or film, joined with their mapping. The
//! Newznab API answers searches from here.

use sqlx::types::Json;

use super::{Db, MappingKind, PinTarget, StoreError, duplicate_or, pins::columns, unix_now};

/// The fields the matcher sets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRelease {
    pub mapping_id: i64,
    /// The video, or the first part of an episode or film split over several videos.
    pub video_id: String,
    /// Every part in play order, `video_id` first. Empty for a single upload.
    pub part_ids: Vec<String>,
    pub target: PinTarget,
    /// The film's year, for film release names.
    pub year: Option<i64>,
    /// IMDb id digits, without `tt`.
    pub imdb_id: Option<i64>,
    pub video_title: String,
    pub language: String,
    pub height: i64,
    pub size: Option<i64>,
    pub published_at: i64,
}

/// A stored release with what the Newznab API needs from its mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRelease {
    pub id: i64,
    pub mapping_id: i64,
    /// The video, or the first part (see [`NewRelease::part_ids`]).
    pub video_id: String,
    /// Every part in play order. Empty for a single upload.
    pub part_ids: Vec<String>,
    pub target: PinTarget,
    pub year: Option<i64>,
    pub imdb_id: Option<i64>,
    pub video_title: String,
    pub language: String,
    pub height: i64,
    pub size: Option<i64>,
    pub published_at: i64,
    pub created_at: i64,
    pub kind: MappingKind,
    /// The series or film title as Sonarr or Radarr has it.
    pub arr_title: String,
    pub tvdb_id: Option<i64>,
    pub tmdb_id: Option<i64>,
}

/// Which episodes of a series a search wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpisodeFilter {
    Any,
    /// A season number, or for daily shows the air year (Sonarr sends `season=2026`).
    Season(i64),
    Episode {
        season: i64,
        episode: i64,
    },
    /// `YYYY-MM-DD`, for daily shows.
    AirDate(String),
}

/// TV releases (shows and daily shows) or films.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseKind {
    Tv,
    Film,
}

#[derive(sqlx::FromRow)]
struct ReleaseRow {
    id: i64,
    mapping_id: i64,
    video_id: String,
    part_ids: Option<Json<Vec<String>>>,
    season: Option<i64>,
    episode: Option<i64>,
    air_date: Option<String>,
    year: Option<i64>,
    imdb_id: Option<i64>,
    video_title: String,
    language: String,
    height: i64,
    size: Option<i64>,
    published_at: i64,
    created_at: i64,
    kind: MappingKind,
    arr_title: String,
    tvdb_id: Option<i64>,
    tmdb_id: Option<i64>,
}

impl From<ReleaseRow> for StoredRelease {
    fn from(row: ReleaseRow) -> Self {
        // The table's CHECK allows only these three shapes.
        let target = match (row.season, row.episode, row.air_date) {
            (Some(season), Some(episode), _) => PinTarget::Episode { season, episode },
            (_, _, Some(date)) => PinTarget::AirDate(date),
            _ => PinTarget::Film,
        };
        Self {
            id: row.id,
            mapping_id: row.mapping_id,
            video_id: row.video_id,
            part_ids: row.part_ids.map(|Json(ids)| ids).unwrap_or_default(),
            target,
            year: row.year,
            imdb_id: row.imdb_id,
            video_title: row.video_title,
            language: row.language,
            height: row.height,
            size: row.size,
            published_at: row.published_at,
            created_at: row.created_at,
            kind: row.kind,
            arr_title: row.arr_title,
            tvdb_id: row.tvdb_id,
            tmdb_id: row.tmdb_id,
        }
    }
}

// sqlx only takes static SQL, so the shared parts are macros that `concat!` can splice.
macro_rules! select {
    () => {
        "SELECT r.id, r.mapping_id, r.video_id, r.part_ids, r.season, r.episode, r.air_date, \
     r.year, r.imdb_id, r.video_title, r.language, r.height, r.size, r.published_at, \
     r.created_at, m.kind, m.title AS arr_title, m.tvdb_id, m.tmdb_id \
     FROM releases r JOIN mappings m ON m.id = r.mapping_id "
    };
}

/// Newest upload first, then newest row.
macro_rules! order {
    () => {
        " ORDER BY r.published_at DESC, r.id DESC "
    };
}

/// The episode filter on parameters ?2 (season or air year), ?3 (episode) and ?4 (air date).
macro_rules! episodes {
    () => {
        " (?2 IS NULL OR r.season = ?2 OR substr(r.air_date, 1, 4) = printf('%04d', ?2)) \
     AND (?3 IS NULL OR r.episode = ?3) AND (?4 IS NULL OR r.air_date = ?4) "
    };
}

impl EpisodeFilter {
    fn binds(&self) -> (Option<i64>, Option<i64>, Option<&str>) {
        match self {
            Self::Any => (None, None, None),
            Self::Season(season) => (Some(*season), None, None),
            Self::Episode { season, episode } => (Some(*season), Some(*episode), None),
            Self::AirDate(date) => (None, None, Some(date)),
        }
    }
}

impl Db {
    pub async fn insert_release(&self, new: &NewRelease) -> Result<StoredRelease, StoreError> {
        let (season, episode, air_date) = columns(&new.target);
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO releases (mapping_id, video_id, part_ids, season, episode, air_date, year, \
             imdb_id, video_title, language, height, size, published_at, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
        )
        .bind(new.mapping_id)
        .bind(&new.video_id)
        .bind((!new.part_ids.is_empty()).then_some(Json(&new.part_ids)))
        .bind(season)
        .bind(episode)
        .bind(air_date)
        .bind(new.year)
        .bind(new.imdb_id)
        .bind(&new.video_title)
        .bind(&new.language)
        .bind(new.height)
        .bind(new.size)
        .bind(new.published_at)
        .bind(unix_now())
        .fetch_one(&self.pool)
        .await
        .map_err(|e| duplicate_or(e, || format!("release of {} for this target", new.key())))?;
        let row: ReleaseRow = sqlx::query_as(concat!(select!(), "WHERE r.id = ?"))
            .bind(id)
            .fetch_one(&self.pool)
            .await?;
        Ok(row.into())
    }

    /// Sets a release's height, size and upload time, once a lookup gave the real height in place
    /// of an assumed one or a feed the exact time in place of a guessed one. The guid hangs on the
    /// video and target only, so the arrs see the same release under its corrected name.
    pub async fn correct_release(
        &self,
        id: i64,
        height: i64,
        size: Option<i64>,
        published_at: i64,
    ) -> Result<bool, StoreError> {
        let done =
            sqlx::query("UPDATE releases SET height = ?, size = ?, published_at = ? WHERE id = ?")
                .bind(height)
                .bind(size)
                .bind(published_at)
                .bind(id)
                .execute(&self.pool)
                .await?;
        Ok(done.rows_affected() > 0)
    }

    /// Releases of the series with this TVDB id.
    pub async fn releases_for_series(
        &self,
        tvdb_id: i64,
        filter: &EpisodeFilter,
        limit: i64,
    ) -> Result<Vec<StoredRelease>, StoreError> {
        let (season, episode, air_date) = filter.binds();
        let rows: Vec<ReleaseRow> = sqlx::query_as(concat!(
            select!(),
            "WHERE m.kind <> 'film' AND m.tvdb_id = ?1 AND",
            episodes!(),
            order!(),
            "LIMIT ?5"
        ))
        .bind(tvdb_id)
        .bind(season)
        .bind(episode)
        .bind(air_date)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Releases of any of these mappings, for title searches.
    pub async fn releases_for_mappings(
        &self,
        mapping_ids: &[i64],
        filter: &EpisodeFilter,
        limit: i64,
    ) -> Result<Vec<StoredRelease>, StoreError> {
        let (season, episode, air_date) = filter.binds();
        let rows: Vec<ReleaseRow> = sqlx::query_as(concat!(
            select!(),
            "WHERE r.mapping_id IN (SELECT value FROM json_each(?1)) AND",
            episodes!(),
            order!(),
            "LIMIT ?5"
        ))
        .bind(Json(mapping_ids))
        .bind(season)
        .bind(episode)
        .bind(air_date)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// Releases of the film with this TMDB id or IMDb id (digits). Either may be missing.
    pub async fn releases_for_film(
        &self,
        tmdb_id: Option<i64>,
        imdb_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<StoredRelease>, StoreError> {
        let rows: Vec<ReleaseRow> = sqlx::query_as(concat!(
            select!(),
            "WHERE m.kind = 'film' AND (m.tmdb_id = ?1 OR r.imdb_id = ?2)",
            order!(),
            "LIMIT ?3"
        ))
        .bind(tmdb_id)
        .bind(imdb_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// The newest releases, optionally only TV or only films.
    pub async fn recent_releases(
        &self,
        kind: Option<ReleaseKind>,
        limit: i64,
    ) -> Result<Vec<StoredRelease>, StoreError> {
        let films = kind.map(|k| k == ReleaseKind::Film);
        let rows: Vec<ReleaseRow> = sqlx::query_as(concat!(
            select!(),
            "WHERE (?1 IS NULL OR (m.kind = 'film') = ?1)",
            order!(),
            "LIMIT ?2"
        ))
        .bind(films)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    /// The release the Newznab feed lists first for one episode or film, found by the arr's own
    /// id of the series (`film` false) or the film (`film` true).
    pub async fn release_for_arr_target(
        &self,
        film: bool,
        arr_id: i64,
        target: &PinTarget,
    ) -> Result<Option<StoredRelease>, StoreError> {
        let (season, episode, air_date) = columns(target);
        let row: Option<ReleaseRow> = sqlx::query_as(concat!(
            select!(),
            "WHERE m.arr_id = ?1 AND (m.kind = 'film') = ?2 AND r.season IS ?3 \
             AND r.episode IS ?4 AND r.air_date IS ?5",
            order!(),
            "LIMIT 1"
        ))
        .bind(arr_id)
        .bind(film)
        .bind(season)
        .bind(episode)
        .bind(air_date)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Into::into))
    }

    /// Every episode and film of an active mapping with an arr id that has a release, once each,
    /// as (kind, arr id, target): what a catch-up push looks at.
    pub async fn released_arr_targets(
        &self,
    ) -> Result<Vec<(MappingKind, i64, PinTarget)>, StoreError> {
        #[derive(sqlx::FromRow)]
        struct Row {
            kind: MappingKind,
            arr_id: i64,
            season: Option<i64>,
            episode: Option<i64>,
            air_date: Option<String>,
        }
        let rows: Vec<Row> = sqlx::query_as(
            "SELECT DISTINCT m.kind, m.arr_id, r.season, r.episode, r.air_date \
             FROM releases r JOIN mappings m ON m.id = r.mapping_id \
             WHERE m.arr_id IS NOT NULL AND m.active = 1 \
             ORDER BY m.id, r.season, r.episode, r.air_date",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let target = match (row.season, row.episode, row.air_date) {
                    (Some(season), Some(episode), _) => PinTarget::Episode { season, episode },
                    (_, _, Some(date)) => PinTarget::AirDate(date),
                    _ => PinTarget::Film,
                };
                (row.kind, row.arr_id, target)
            })
            .collect())
    }

    /// The newest release of a video, or of parts joined by commas in play order (the
    /// [`StoredRelease::key`] the NZB carries).
    pub async fn release_by_video(&self, key: &str) -> Result<Option<StoredRelease>, StoreError> {
        let first = key.split(',').next().unwrap_or_default();
        let rows: Vec<ReleaseRow> =
            sqlx::query_as(concat!(select!(), "WHERE r.video_id = ?", order!()))
                .bind(first)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(StoredRelease::from)
            .find(|r| r.key() == key))
    }
}

/// One video id, or the parts' ids joined by commas: what the `/play` path, the NZB and the
/// Newznab guid carry.
fn key(video_id: &str, part_ids: &[String]) -> String {
    if part_ids.is_empty() {
        video_id.to_owned()
    } else {
        part_ids.join(",")
    }
}

impl NewRelease {
    pub fn key(&self) -> String {
        key(&self.video_id, &self.part_ids)
    }
}

impl StoredRelease {
    pub fn key(&self) -> String {
        key(&self.video_id, &self.part_ids)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::store::{NewMapping, mappings::tests::drama, test_db};

    pub fn release(mapping_id: i64, video_id: &str, target: PinTarget, at: i64) -> NewRelease {
        NewRelease {
            mapping_id,
            video_id: video_id.to_owned(),
            part_ids: Vec::new(),
            target,
            year: None,
            imdb_id: None,
            video_title: format!("{video_id} on YouTube"),
            language: "Urdu".to_owned(),
            height: 1080,
            size: Some(1_500_000_000),
            published_at: at,
        }
    }

    fn ep(season: i64, episode: i64) -> PinTarget {
        PinTarget::Episode { season, episode }
    }

    fn ids(releases: &[StoredRelease]) -> Vec<&str> {
        releases.iter().map(|r| r.video_id.as_str()).collect()
    }

    async fn seeded() -> (Db, tempfile::TempDir, i64, i64, i64) {
        let (db, dir) = test_db::fresh().await;
        let show = db.insert_mapping(&drama()).await.unwrap().id;
        let daily = db
            .insert_mapping(&NewMapping {
                kind: MappingKind::Daily,
                tvdb_id: Some(9),
                title: "Daily Show".to_owned(),
                ..drama()
            })
            .await
            .unwrap()
            .id;
        let film = db
            .insert_mapping(&NewMapping {
                kind: MappingKind::Film,
                tvdb_id: None,
                tmdb_id: Some(352_173),
                title: "Theri".to_owned(),
                language: "Hindi".to_owned(),
                ..drama()
            })
            .await
            .unwrap()
            .id;
        db.insert_release(&release(show, "s1e1", ep(1, 1), 100))
            .await
            .unwrap();
        db.insert_release(&release(show, "s1e2", ep(1, 2), 200))
            .await
            .unwrap();
        db.insert_release(&release(show, "s2e1", ep(2, 1), 300))
            .await
            .unwrap();
        let date = |d: &str| PinTarget::AirDate(d.to_owned());
        db.insert_release(&release(daily, "d1014", date("2026-10-14"), 400))
            .await
            .unwrap();
        db.insert_release(&release(daily, "d1015", date("2025-10-15"), 50))
            .await
            .unwrap();
        db.insert_release(&NewRelease {
            year: Some(2016),
            imdb_id: Some(4_598_756),
            ..release(film, "theri", PinTarget::Film, 10)
        })
        .await
        .unwrap();
        (db, dir, show, daily, film)
    }

    #[tokio::test]
    async fn insert_reads_back_with_the_mapping() {
        let (db, _dir, show, _, _) = seeded().await;
        let stored = db.release_by_video("s1e2").await.unwrap().unwrap();
        assert_eq!(stored.mapping_id, show);
        assert_eq!(stored.target, ep(1, 2));
        assert_eq!(stored.arr_title, "Kabhi Main Kabhi Tum");
        assert_eq!(stored.kind, MappingKind::Show);
        assert_eq!(stored.tvdb_id, Some(452_001));
        assert_eq!(stored.size, Some(1_500_000_000));
        assert_eq!(db.release_by_video("nope").await.unwrap(), None);
    }

    #[tokio::test]
    async fn series_filters() {
        let (db, _dir, ..) = seeded().await;
        let get = async |tvdb, filter| db.releases_for_series(tvdb, &filter, 100).await.unwrap();
        assert_eq!(
            ids(&get(452_001, EpisodeFilter::Any).await),
            ["s2e1", "s1e2", "s1e1"]
        );
        assert_eq!(
            ids(&get(452_001, EpisodeFilter::Season(1)).await),
            ["s1e2", "s1e1"]
        );
        let one = EpisodeFilter::Episode {
            season: 1,
            episode: 2,
        };
        assert_eq!(ids(&get(452_001, one).await), ["s1e2"]);
        assert_eq!(
            ids(&get(9, EpisodeFilter::AirDate("2026-10-14".to_owned())).await),
            ["d1014"]
        );
        assert_eq!(ids(&get(9, EpisodeFilter::Season(2025)).await), ["d1015"]);
        assert!(get(1, EpisodeFilter::Any).await.is_empty());
        assert_eq!(
            db.releases_for_series(452_001, &EpisodeFilter::Any, 1)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn film_by_either_id() {
        let (db, _dir, ..) = seeded().await;
        let by_tmdb = db.releases_for_film(Some(352_173), None, 10).await.unwrap();
        assert_eq!(ids(&by_tmdb), ["theri"]);
        assert_eq!(by_tmdb[0].year, Some(2016));
        let by_imdb = db
            .releases_for_film(None, Some(4_598_756), 10)
            .await
            .unwrap();
        assert_eq!(ids(&by_imdb), ["theri"]);
        assert!(
            db.releases_for_film(Some(1), None, 10)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn recent_and_by_mapping() {
        let (db, _dir, show, daily, _) = seeded().await;
        let all = db.recent_releases(None, 3).await.unwrap();
        assert_eq!(ids(&all), ["d1014", "s2e1", "s1e2"]);
        let films = db
            .recent_releases(Some(ReleaseKind::Film), 10)
            .await
            .unwrap();
        assert_eq!(ids(&films), ["theri"]);
        let tv = db.recent_releases(Some(ReleaseKind::Tv), 10).await.unwrap();
        assert_eq!(tv.len(), 5);

        let some = db
            .releases_for_mappings(&[show, daily], &EpisodeFilter::Season(2), 10)
            .await
            .unwrap();
        assert_eq!(ids(&some), ["s2e1"]);
    }

    #[tokio::test]
    async fn same_target_twice_is_refused_and_mapping_delete_cascades() {
        let (db, _dir, show, ..) = seeded().await;
        let err = db
            .insert_release(&release(show, "s1e1", ep(1, 1), 1))
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Duplicate(_)), "{err}");
        // Another video for the same episode is a second candidate.
        db.insert_release(&release(show, "other", ep(1, 1), 1))
            .await
            .unwrap();

        assert!(db.delete_mapping(show).await.unwrap());
        assert_eq!(db.release_by_video("s1e1").await.unwrap(), None);
    }
}
