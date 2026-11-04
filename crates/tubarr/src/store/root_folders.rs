//! What root folder routing keeps per mapping: the user's override, the arr's current root folder
//! and the last move Tubarr asked for. See [`crate::arr::root_folders`].

use super::{ArrSource, Db, MappingKind, StoreError, unix_now};

/// One active synced mapping as routing sees it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct RootFolderRow {
    pub id: i64,
    pub kind: MappingKind,
    pub title: String,
    pub language: String,
    #[sqlx(json)]
    pub channel_ids: Vec<String>,
    pub arr_id: Option<i64>,
    /// The user's override: `none` or a path. `None` follows the config.
    #[sqlx(rename = "root_folder")]
    pub override_folder: Option<String>,
    pub current: Option<String>,
    pub attempted: Option<String>,
    pub result: Option<String>,
    pub result_at: Option<i64>,
}

macro_rules! rows_sql {
    () => {
        "SELECT m.id, m.kind, m.title, m.language, m.channel_ids, m.arr_id, \
     m.root_folder, s.current, s.attempted, s.result, s.result_at \
     FROM mappings m LEFT JOIN root_folder_state s ON s.mapping_id = m.id "
    };
}

impl Db {
    /// Active mappings from Sonarr or Radarr (all of them, or only `id`), for a routing pass.
    pub async fn root_folder_rows(
        &self,
        id: Option<i64>,
    ) -> Result<Vec<RootFolderRow>, StoreError> {
        let sql = concat!(
            rows_sql!(),
            "WHERE m.active = 1 AND m.arr_id IS NOT NULL AND (?1 IS NULL OR m.id = ?1) ",
            "ORDER BY m.id"
        );
        Ok(sqlx::query_as(sql).bind(id).fetch_all(&self.pool).await?)
    }

    /// One mapping's routing state, active or not, for the admin API.
    pub async fn root_folder_state(&self, id: i64) -> Result<Option<RootFolderRow>, StoreError> {
        Ok(sqlx::query_as(concat!(rows_sql!(), "WHERE m.id = ?"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// Every mapping's routing state, for the admin API.
    pub async fn root_folder_states(&self) -> Result<Vec<RootFolderRow>, StoreError> {
        Ok(sqlx::query_as(rows_sql!()).fetch_all(&self.pool).await?)
    }

    /// Stores the root folder each title of `source` sits in, as the arr reported it: `(key,
    /// rootFolderPath)` with the TVDB id for Sonarr and the TMDB id for Radarr.
    pub async fn set_current_root_folders(
        &self,
        source: ArrSource,
        folders: &[(i64, Option<String>)],
    ) -> Result<(), StoreError> {
        let select = match source {
            ArrSource::Sonarr => "SELECT id FROM mappings WHERE kind <> 'film' AND tvdb_id = ?",
            ArrSource::Radarr => "SELECT id FROM mappings WHERE kind = 'film' AND tmdb_id = ?",
        };
        // Reads before it writes: IMMEDIATE waits out a busy writer instead of failing at the
        // write.
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        for (key, folder) in folders {
            let Some(id): Option<i64> = sqlx::query_scalar(select)
                .bind(key)
                .fetch_optional(&mut *tx)
                .await?
            else {
                continue;
            };
            sqlx::query(
                "INSERT INTO root_folder_state (mapping_id, current) VALUES (?, ?) \
                 ON CONFLICT (mapping_id) DO UPDATE SET current = excluded.current",
            )
            .bind(id)
            .bind(folder)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Records a routing result. `attempted` is set only when a move was asked for (it then also
    /// counts as asked, see the migration), and `current` when the arr took the move.
    pub async fn record_root_folder_result(
        &self,
        id: i64,
        attempted: Option<&str>,
        result: &str,
        current: Option<&str>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO root_folder_state (mapping_id, current, attempted, result, result_at) \
             VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT (mapping_id) DO UPDATE SET \
             current = coalesce(?2, current), attempted = coalesce(?3, attempted), \
             result = ?4, result_at = ?5",
        )
        .bind(id)
        .bind(current)
        .bind(attempted)
        .bind(result)
        .bind(unix_now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Sets the user's override (`none`, a path, or `None` to follow the config again).
    pub async fn set_root_folder_override(
        &self,
        id: i64,
        folder: Option<&str>,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE mappings SET root_folder = ?, updated_at = ? WHERE id = ?")
            .bind(folder)
            .bind(unix_now())
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ArrEntry, test_db};

    fn show(key: i64) -> ArrEntry {
        ArrEntry {
            kind: MappingKind::Show,
            key,
            arr_id: key,
            title: format!("Show {key}"),
            language: "Urdu".to_owned(),
            year: None,
            imdb_id: None,
            aliases: Vec::new(),
        }
    }

    #[tokio::test]
    async fn current_attempted_and_override_round_trip() {
        let (db, _dir) = test_db::fresh().await;
        db.sync_arr_mappings(ArrSource::Sonarr, &[show(1), show(2)])
            .await
            .unwrap();
        db.set_current_root_folders(
            ArrSource::Sonarr,
            &[(1, Some("/tv/".to_owned())), (99, Some("/x".to_owned()))],
        )
        .await
        .unwrap();
        let rows = db.root_folder_rows(None).await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].current.as_deref(), Some("/tv/"));
        assert_eq!(rows[1].current, None);

        let id = rows[0].id;
        db.record_root_folder_result(id, Some("/pk"), "moved", Some("/pk/"))
            .await
            .unwrap();
        db.record_root_folder_result(id, None, "missing", None)
            .await
            .unwrap();
        db.set_root_folder_override(id, Some("none")).await.unwrap();
        let row = db.root_folder_rows(Some(id)).await.unwrap().remove(0);
        assert_eq!(row.current.as_deref(), Some("/pk/"));
        assert_eq!(row.attempted.as_deref(), Some("/pk"));
        assert_eq!(row.result.as_deref(), Some("missing"));
        assert_eq!(row.override_folder.as_deref(), Some("none"));
    }
}
