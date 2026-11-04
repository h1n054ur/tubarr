//! Grabs: the download jobs Sonarr and Radarr see through the SABnzbd API.

use std::time::Duration;

use super::{Db, StoreError, duplicate_or, unix_now};

#[derive(Debug, Clone, Copy, PartialEq, Eq, sqlx::Type)]
#[sqlx(rename_all = "lowercase")]
pub enum GrabStatus {
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewGrab {
    pub nzo_id: String,
    pub release_name: String,
    pub video_id: String,
    pub category: String,
    pub status: GrabStatus,
    pub storage_path: String,
    /// Why a failed grab failed, for SABnzbd's `fail_message`.
    pub fail_message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Grab {
    pub nzo_id: String,
    pub release_name: String,
    pub video_id: String,
    pub category: String,
    pub status: GrabStatus,
    pub storage_path: String,
    pub created_at: i64,
    pub fail_message: Option<String>,
}

impl Db {
    pub async fn insert_grab(&self, new: &NewGrab) -> Result<Grab, StoreError> {
        sqlx::query_as(
            "INSERT INTO grabs (nzo_id, release_name, video_id, category, status, storage_path, \
             created_at, fail_message) VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING *",
        )
        .bind(&new.nzo_id)
        .bind(&new.release_name)
        .bind(&new.video_id)
        .bind(&new.category)
        .bind(new.status)
        .bind(&new.storage_path)
        .bind(unix_now())
        .bind(&new.fail_message)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| duplicate_or(e, || format!("grab {}", new.nzo_id)))
    }

    pub async fn grab(&self, nzo_id: &str) -> Result<Option<Grab>, StoreError> {
        Ok(sqlx::query_as("SELECT * FROM grabs WHERE nzo_id = ?")
            .bind(nzo_id)
            .fetch_optional(&self.pool)
            .await?)
    }

    /// The newest grabs first, optionally only one category: at most `limit` (-1 for all) after
    /// skipping `offset`.
    pub async fn grabs(
        &self,
        category: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<Grab>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT * FROM grabs WHERE (?1 IS NULL OR category = ?1) \
             ORDER BY created_at DESC, rowid DESC LIMIT ?2 OFFSET ?3",
        )
        .bind(category)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?)
    }

    /// How many grabs [`Db::grabs`] pages through for `category`.
    pub async fn count_grabs(&self, category: Option<&str>) -> Result<i64, StoreError> {
        Ok(
            sqlx::query_scalar("SELECT count(*) FROM grabs WHERE (?1 IS NULL OR category = ?1)")
                .bind(category)
                .fetch_one(&self.pool)
                .await?,
        )
    }

    /// Drops the grabs made longer than `retention` ago, completed and failed alike. Their
    /// `.strm` files stay: only the history Sonarr and Radarr poll forgets them. Returns how many
    /// went.
    pub async fn prune_grabs(&self, retention: Duration) -> Result<u64, StoreError> {
        let cutoff =
            unix_now().saturating_sub(i64::try_from(retention.as_secs()).unwrap_or(i64::MAX));
        let done = sqlx::query("DELETE FROM grabs WHERE created_at < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }

    /// Removes a grab from history. `false` when no such grab existed.
    pub async fn delete_grab(&self, nzo_id: &str) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM grabs WHERE nzo_id = ?")
            .bind(nzo_id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_db;

    fn grab(nzo_id: &str, category: &str) -> NewGrab {
        NewGrab {
            nzo_id: nzo_id.to_owned(),
            release_name: "Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC2.0.H.264.URDU-TUBARR"
                .to_owned(),
            video_id: "0eD-GQCutAU".to_owned(),
            category: category.to_owned(),
            status: GrabStatus::Completed,
            storage_path: format!("/downloads/tubarr/{category}/{nzo_id}"),
            fail_message: None,
        }
    }

    #[tokio::test]
    async fn insert_list_and_delete() {
        let (db, _dir) = test_db::fresh().await;
        let first = db.insert_grab(&grab("tubarr_1", "tv")).await.unwrap();
        db.insert_grab(&grab("tubarr_2", "movies")).await.unwrap();
        db.insert_grab(&grab("tubarr_3", "tv")).await.unwrap();

        assert_eq!(db.grab("tubarr_1").await.unwrap(), Some(first));
        let all: Vec<_> = db
            .grabs(None, 10, 0)
            .await
            .unwrap()
            .into_iter()
            .map(|g| g.nzo_id)
            .collect();
        assert_eq!(all, ["tubarr_3", "tubarr_2", "tubarr_1"]);
        let tv: Vec<_> = db
            .grabs(Some("tv"), 1, 1)
            .await
            .unwrap()
            .into_iter()
            .map(|g| g.nzo_id)
            .collect();
        assert_eq!(tv, ["tubarr_1"]);
        assert_eq!(db.count_grabs(Some("tv")).await.unwrap(), 2);
        assert_eq!(db.count_grabs(None).await.unwrap(), 3);

        assert!(db.delete_grab("tubarr_1").await.unwrap());
        assert!(!db.delete_grab("tubarr_1").await.unwrap());
        assert_eq!(db.grab("tubarr_1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn duplicate_nzo_id_is_refused() {
        let (db, _dir) = test_db::fresh().await;
        db.insert_grab(&grab("tubarr_1", "tv")).await.unwrap();
        let err = db.insert_grab(&grab("tubarr_1", "tv")).await.unwrap_err();
        assert!(matches!(err, StoreError::Duplicate(_)), "{err}");
    }
}
