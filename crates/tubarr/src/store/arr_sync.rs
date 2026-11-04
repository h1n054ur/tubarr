//! Writing what Sonarr and Radarr report into the mappings table.
//!
//! The arr owns the title, kind, year, IMDb id and its own id. Tubarr owns everything else
//! (channels, regex, excludes, minimum duration, and the language once the row exists), so the
//! sync never touches those.

use sqlx::types::Json;

use super::{Db, MappingKind, StoreError, aliases::refresh_arr_aliases, unix_now};

/// Which arr a sync came from. Sonarr rows are shows and daily shows keyed by TVDB id, Radarr rows
/// are films keyed by TMDB id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrSource {
    Sonarr,
    Radarr,
}

/// One tagged series or film as the arr reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrEntry {
    /// Show or daily for Sonarr, film for Radarr.
    pub kind: MappingKind,
    /// TVDB id for Sonarr, TMDB id for Radarr.
    pub key: i64,
    pub arr_id: i64,
    /// Exactly as the arr has it: release names must start with it.
    pub title: String,
    /// The arr's original language. Only used when the mapping is first created. A show on
    /// English or Unknown takes its channel's language later (see `Db::languages_from_channels`).
    pub language: String,
    pub year: Option<i64>,
    pub imdb_id: Option<String>,
    /// Other names the arr lists (Sonarr's alternate titles), kept as the mapping's arr aliases.
    pub aliases: Vec<String>,
}

/// What one sync changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncCounts {
    /// Mappings created or changed (including ones made active again).
    pub changed: u64,
    /// Mappings marked inactive because their title is no longer tagged.
    pub deactivated: u64,
}

/// The upsert for one source, keyed on `$key` (a unique column). The WHERE on the update skips
/// rows that already match, so unchanged titles keep their updated_at, and a row of the other
/// source holding the same id is left alone.
macro_rules! upsert_sql {
    ($key:literal, $kind:literal) => {
        concat!(
            "INSERT INTO mappings (kind, ", $key, ", title, language, language_source, year, ",
            "imdb_id, arr_id, active, created_at, updated_at) ",
            "VALUES (?, ?, ?, ?, 'arr', ?, ?, ?, 1, ?, ?) ",
            "ON CONFLICT (", $key, ") DO UPDATE SET kind = excluded.kind, title = excluded.title, ",
            "year = excluded.year, imdb_id = excluded.imdb_id, arr_id = excluded.arr_id, ",
            "active = 1, updated_at = excluded.updated_at ",
            "WHERE mappings.", $kind, " AND (mappings.kind IS NOT excluded.kind ",
            "OR mappings.title IS NOT excluded.title OR mappings.year IS NOT excluded.year ",
            "OR mappings.imdb_id IS NOT excluded.imdb_id OR mappings.arr_id IS NOT excluded.arr_id ",
            "OR mappings.active = 0)"
        )
    };
}

/// Marks the source's synced rows missing from a JSON array of keys inactive.
macro_rules! deactivate_sql {
    ($key:literal, $kind:literal) => {
        concat!(
            "UPDATE mappings SET active = 0, updated_at = ? WHERE active = 1 ",
            "AND arr_id IS NOT NULL AND ",
            $kind,
            " ",
            "AND ",
            $key,
            " NOT IN (SELECT value FROM json_each(?))"
        )
    };
}

impl Db {
    /// Upserts `entries` as the complete tagged set of `source`, in one transaction. Mappings the
    /// arr made earlier that are missing from `entries` become inactive. Mappings made by hand
    /// (no `arr_id`) are never deactivated.
    pub async fn sync_arr_mappings(
        &self,
        source: ArrSource,
        entries: &[ArrEntry],
    ) -> Result<SyncCounts, StoreError> {
        let (upsert, deactivate, row) = match source {
            ArrSource::Sonarr => (
                upsert_sql!("tvdb_id", "kind <> 'film'"),
                deactivate_sql!("tvdb_id", "kind <> 'film'"),
                "SELECT id FROM mappings WHERE tvdb_id = ? AND kind <> 'film'",
            ),
            ArrSource::Radarr => (
                upsert_sql!("tmdb_id", "kind = 'film'"),
                deactivate_sql!("tmdb_id", "kind = 'film'"),
                "SELECT id FROM mappings WHERE tmdb_id = ? AND kind = 'film'",
            ),
        };
        let now = unix_now();
        let mut counts = SyncCounts::default();
        let mut tx = self.pool.begin().await?;
        for entry in entries {
            let done = sqlx::query(upsert)
                .bind(entry.kind)
                .bind(entry.key)
                .bind(&entry.title)
                .bind(&entry.language)
                .bind(entry.year)
                .bind(&entry.imdb_id)
                .bind(entry.arr_id)
                .bind(now)
                .bind(now)
                .execute(&mut *tx)
                .await?;
            counts.changed += done.rows_affected();
            // Alias changes are not counted: the mapping's own fields did not change.
            let id: Option<i64> = sqlx::query_scalar(row)
                .bind(entry.key)
                .fetch_optional(&mut *tx)
                .await?;
            if let Some(id) = id {
                refresh_arr_aliases(&mut tx, id, &entry.aliases).await?;
            }
        }
        let keys: Vec<i64> = entries.iter().map(|e| e.key).collect();
        counts.deactivated = sqlx::query(deactivate)
            .bind(now)
            .bind(Json(keys))
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        Ok(counts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{mappings::tests::drama, test_db};

    fn series(tvdb: i64, title: &str) -> ArrEntry {
        ArrEntry {
            kind: MappingKind::Show,
            key: tvdb,
            arr_id: tvdb / 1000,
            title: title.to_owned(),
            language: "English".to_owned(),
            year: Some(2024),
            imdb_id: None,
            aliases: vec![],
        }
    }

    #[tokio::test]
    async fn unchanged_rows_are_not_rewritten() {
        let (db, _dir) = test_db::fresh().await;
        let entries = [series(1000, "A"), series(2000, "B")];
        let first = db
            .sync_arr_mappings(ArrSource::Sonarr, &entries)
            .await
            .unwrap();
        assert_eq!(first.changed, 2);
        let again = db
            .sync_arr_mappings(ArrSource::Sonarr, &entries)
            .await
            .unwrap();
        assert_eq!(again, SyncCounts::default());
    }

    #[tokio::test]
    async fn hand_made_rows_and_films_survive_a_sonarr_sync() {
        let (db, _dir) = test_db::fresh().await;
        let manual = db.insert_mapping(&drama()).await.unwrap();
        let film = ArrEntry {
            kind: MappingKind::Film,
            key: 352_173,
            arr_id: 7,
            title: "Theri".to_owned(),
            language: "Tamil".to_owned(),
            year: Some(2016),
            imdb_id: Some("tt4807012".to_owned()),
            aliases: vec![],
        };
        db.sync_arr_mappings(ArrSource::Radarr, &[film])
            .await
            .unwrap();

        let counts = db.sync_arr_mappings(ArrSource::Sonarr, &[]).await.unwrap();
        assert_eq!(counts.deactivated, 0);
        assert!(db.mapping(manual.id).await.unwrap().unwrap().active);
        assert!(db.mapping_by_tmdb(352_173).await.unwrap().unwrap().active);
    }

    #[tokio::test]
    async fn a_film_row_with_the_same_tvdb_id_is_left_alone() {
        let (db, _dir) = test_db::fresh().await;
        let mut film = drama();
        film.kind = MappingKind::Film;
        film.tmdb_id = Some(1);
        let film = db.insert_mapping(&film).await.unwrap();
        db.sync_arr_mappings(ArrSource::Sonarr, &[series(452_001, "Other")])
            .await
            .unwrap();
        assert_eq!(db.mapping(film.id).await.unwrap(), Some(film));
    }
}
