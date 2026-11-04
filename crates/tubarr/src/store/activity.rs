//! Activity events: the searches, arr pushes and play starts the Activity page lists. The rows hold
//! each event as JSON; `crate::activity` turns them into events and back.

use std::time::Duration;

use super::{Db, StoreError, unix_now};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct StoredEvent {
    /// Unix seconds.
    pub at: i64,
    /// `search`, `arr_push` or `play`.
    pub kind: String,
    pub data: String,
}

impl Db {
    /// Writes `events` in one transaction, in the order given.
    pub async fn insert_activity(&self, events: &[StoredEvent]) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        for e in events {
            sqlx::query("INSERT INTO activity_events (at, kind, data) VALUES (?, ?, ?)")
                .bind(e.at)
                .bind(&e.kind)
                .bind(&e.data)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// The newest events first, optionally only one kind: at most `limit` after skipping `offset`.
    pub async fn activity(
        &self,
        kind: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<StoredEvent>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT at, kind, data FROM activity_events WHERE (?1 IS NULL OR kind = ?1) \
             ORDER BY id DESC LIMIT ?2 OFFSET ?3",
        )
        .bind(kind)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?)
    }

    /// How many events [`Db::activity`] pages through for `kind`.
    pub async fn count_activity(&self, kind: Option<&str>) -> Result<i64, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT count(*) FROM activity_events WHERE (?1 IS NULL OR kind = ?1)",
        )
        .bind(kind)
        .fetch_one(&self.pool)
        .await?)
    }

    /// Drops the events older than `retention`. Returns how many went.
    pub async fn prune_activity(&self, retention: Duration) -> Result<u64, StoreError> {
        let cutoff =
            unix_now().saturating_sub(i64::try_from(retention.as_secs()).unwrap_or(i64::MAX));
        let done = sqlx::query("DELETE FROM activity_events WHERE at < ?")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_db;

    fn event(at: i64, kind: &str) -> StoredEvent {
        StoredEvent {
            at,
            kind: kind.to_owned(),
            data: format!(r#"{{"kind":"{kind}"}}"#),
        }
    }

    #[tokio::test]
    async fn pages_newest_first_and_prunes_by_age() {
        let (db, _dir) = test_db::fresh().await;
        let now = unix_now();
        db.insert_activity(&[
            event(now - 40 * 86_400, "play"),
            event(now - 2, "search"),
            event(now - 1, "play"),
        ])
        .await
        .unwrap();
        db.insert_activity(&[event(now, "arr_push")]).await.unwrap();

        let kinds = |rows: Vec<StoredEvent>| rows.into_iter().map(|e| e.kind).collect::<Vec<_>>();
        assert_eq!(
            kinds(db.activity(None, 10, 0).await.unwrap()),
            ["arr_push", "play", "search", "play"]
        );
        assert_eq!(
            kinds(db.activity(None, 2, 1).await.unwrap()),
            ["play", "search"]
        );
        let plays = db.activity(Some("play"), 10, 1).await.unwrap();
        assert_eq!(plays[0].at, now - 40 * 86_400);
        assert_eq!(db.count_activity(Some("play")).await.unwrap(), 2);
        assert_eq!(db.count_activity(None).await.unwrap(), 4);

        let gone = db
            .prune_activity(Duration::from_secs(30 * 86_400))
            .await
            .unwrap();
        assert_eq!(gone, 1);
        assert_eq!(db.count_activity(None).await.unwrap(), 3);
    }
}
