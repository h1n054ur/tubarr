//! The video ids `/play` may serve: every id Tubarr put into a release, a pin or a written grab.
//! Triggers on those tables fill it (migration 0035) and nothing removes from it.

use super::{Db, StoreError};

impl Db {
    /// `true` when every id in `ids` was released, pinned or grabbed. One indexed lookup.
    pub async fn all_released(&self, ids: &[String]) -> Result<bool, StoreError> {
        let mut wanted: Vec<&str> = ids.iter().map(String::as_str).collect();
        wanted.sort_unstable();
        wanted.dedup();
        let found: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM released_videos \
             WHERE video_id IN (SELECT value FROM json_each(?))",
        )
        .bind(sqlx::types::Json(&wanted))
        .fetch_one(&self.pool)
        .await?;
        Ok(usize::try_from(found).is_ok_and(|n| n == wanted.len()))
    }

    /// Adds ids that played before this list existed. Returns how many were new.
    pub async fn remember_released(&self, ids: &[String]) -> Result<u64, StoreError> {
        let mut tx = self.pool.begin().await?;
        let mut added = 0;
        for id in ids {
            added += sqlx::query("INSERT OR IGNORE INTO released_videos (video_id) VALUES (?)")
                .bind(id)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        }
        tx.commit().await?;
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use crate::store::{GrabStatus, NewGrab, test_db};

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    #[tokio::test]
    async fn grabs_count_once_written_and_stay_after_removal() {
        let (db, _dir) = test_db::fresh().await;
        let grab = |nzo: &str, video: &str, status| NewGrab {
            nzo_id: nzo.to_owned(),
            release_name: "r".to_owned(),
            video_id: video.to_owned(),
            category: "tv".to_owned(),
            status,
            storage_path: String::new(),
            fail_message: None,
        };
        db.insert_grab(&grab("a", "aaaaaaaaaaa,bbbbbbbbbbb", GrabStatus::Completed))
            .await
            .unwrap();
        db.insert_grab(&grab("b", "ccccccccccc", GrabStatus::Failed))
            .await
            .unwrap();
        assert!(db.delete_grab("a").await.unwrap());
        assert!(
            db.all_released(&ids(&["bbbbbbbbbbb", "aaaaaaaaaaa", "aaaaaaaaaaa"]))
                .await
                .unwrap()
        );
        assert!(!db.all_released(&ids(&["ccccccccccc"])).await.unwrap());
        assert!(
            !db.all_released(&ids(&["aaaaaaaaaaa", "ddddddddddd"]))
                .await
                .unwrap()
        );
        assert_eq!(
            db.remember_released(&ids(&["ddddddddddd", "aaaaaaaaaaa"]))
                .await
                .unwrap(),
            1
        );
        assert!(db.all_released(&ids(&["ddddddddddd"])).await.unwrap());
    }
}
