//! Pins: a video Hani picked by hand for one episode or film. A pin wins over the matcher.

use sqlx::SqliteConnection;

use super::{Db, StoreError, unix_now};

/// What a pin stands for, matching the mapping's kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinTarget {
    Episode {
        season: i64,
        episode: i64,
    },
    /// `YYYY-MM-DD`, for daily shows.
    AirDate(String),
    Film,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub id: i64,
    pub mapping_id: i64,
    pub target: PinTarget,
    /// One video id, or the parts of a split upload joined by commas in play order.
    pub video_id: String,
    pub created_at: i64,
}

#[derive(sqlx::FromRow)]
struct PinRow {
    id: i64,
    mapping_id: i64,
    season: Option<i64>,
    episode: Option<i64>,
    air_date: Option<String>,
    video_id: String,
    created_at: i64,
}

impl From<PinRow> for Pin {
    fn from(row: PinRow) -> Self {
        // The table's CHECK allows only these three shapes.
        let target = match (row.season, row.episode, row.air_date) {
            (Some(season), Some(episode), _) => PinTarget::Episode { season, episode },
            (_, _, Some(date)) => PinTarget::AirDate(date),
            _ => PinTarget::Film,
        };
        Self {
            id: row.id,
            mapping_id: row.mapping_id,
            target,
            video_id: row.video_id,
            created_at: row.created_at,
        }
    }
}

pub(super) fn columns(target: &PinTarget) -> (Option<i64>, Option<i64>, Option<&str>) {
    match target {
        PinTarget::Episode { season, episode } => (Some(*season), Some(*episode), None),
        PinTarget::AirDate(date) => (None, None, Some(date)),
        PinTarget::Film => (None, None, None),
    }
}

/// [`Db::set_pin`] on a connection the caller holds, for a pin written inside a larger
/// transaction.
pub async fn replace_pin(
    conn: &mut SqliteConnection,
    mapping_id: i64,
    target: &PinTarget,
    video_id: &str,
) -> Result<Pin, StoreError> {
    let (season, episode, air_date) = columns(target);
    sqlx::query(
        "DELETE FROM pins WHERE mapping_id = ? AND season IS ? AND episode IS ? AND air_date IS ?",
    )
    .bind(mapping_id)
    .bind(season)
    .bind(episode)
    .bind(air_date)
    .execute(&mut *conn)
    .await?;
    let row: PinRow = sqlx::query_as(
        "INSERT INTO pins (mapping_id, season, episode, air_date, video_id, created_at) \
         VALUES (?, ?, ?, ?, ?, ?) RETURNING *",
    )
    .bind(mapping_id)
    .bind(season)
    .bind(episode)
    .bind(air_date)
    .bind(video_id)
    .bind(unix_now())
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.into())
}

impl Db {
    /// Pins `video_id` (or part ids joined by commas) to `target` of a mapping, replacing any
    /// earlier pin for the same target.
    pub async fn set_pin(
        &self,
        mapping_id: i64,
        target: &PinTarget,
        video_id: &str,
    ) -> Result<Pin, StoreError> {
        let mut tx = self.pool.begin().await?;
        let pin = replace_pin(&mut tx, mapping_id, target, video_id).await?;
        tx.commit().await?;
        Ok(pin)
    }

    /// The pin for one target of a mapping, if Hani set one.
    pub async fn pin_for(
        &self,
        mapping_id: i64,
        target: &PinTarget,
    ) -> Result<Option<Pin>, StoreError> {
        let (season, episode, air_date) = columns(target);
        let row: Option<PinRow> = sqlx::query_as(
            "SELECT * FROM pins WHERE mapping_id = ? AND season IS ? AND episode IS ? AND air_date IS ?",
        )
        .bind(mapping_id)
        .bind(season)
        .bind(episode)
        .bind(air_date)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(Pin::from))
    }

    /// Every pin of a mapping, by season, episode and date.
    pub async fn pins(&self, mapping_id: i64) -> Result<Vec<Pin>, StoreError> {
        let rows: Vec<PinRow> = sqlx::query_as(
            "SELECT * FROM pins WHERE mapping_id = ? ORDER BY season, episode, air_date",
        )
        .bind(mapping_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Pin::from).collect())
    }

    /// Removes a pin. `false` when no such pin existed.
    pub async fn delete_pin(&self, id: i64) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM pins WHERE id = ?")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{mappings::tests::drama, test_db};

    #[tokio::test]
    async fn set_replaces_and_reads_back() {
        let (db, _dir) = test_db::fresh().await;
        let mapping = db.insert_mapping(&drama()).await.unwrap();
        let ep1 = PinTarget::Episode {
            season: 1,
            episode: 1,
        };
        let date = PinTarget::AirDate("2026-10-14".to_owned());

        db.set_pin(mapping.id, &ep1, "wrongVideo1").await.unwrap();
        let pin = db.set_pin(mapping.id, &ep1, "0eD-GQCutAU").await.unwrap();
        db.set_pin(mapping.id, &date, "dailyVideo1").await.unwrap();
        db.set_pin(mapping.id, &PinTarget::Film, "filmVideo01")
            .await
            .unwrap();

        assert_eq!(db.pin_for(mapping.id, &ep1).await.unwrap(), Some(pin));
        assert_eq!(
            db.pin_for(mapping.id, &date)
                .await
                .unwrap()
                .unwrap()
                .video_id,
            "dailyVideo1"
        );
        let ep2 = PinTarget::Episode {
            season: 1,
            episode: 2,
        };
        assert_eq!(db.pin_for(mapping.id, &ep2).await.unwrap(), None);
        assert_eq!(db.pins(mapping.id).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn delete_and_cascade() {
        let (db, _dir) = test_db::fresh().await;
        let mapping = db.insert_mapping(&drama()).await.unwrap();
        let ep = PinTarget::Episode {
            season: 1,
            episode: 3,
        };
        let pin = db.set_pin(mapping.id, &ep, "abc").await.unwrap();
        assert!(db.delete_pin(pin.id).await.unwrap());
        assert!(!db.delete_pin(pin.id).await.unwrap());

        db.set_pin(mapping.id, &ep, "abc").await.unwrap();
        db.delete_mapping(mapping.id).await.unwrap();
        assert!(db.pins(mapping.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn pin_needs_an_existing_mapping() {
        let (db, _dir) = test_db::fresh().await;
        assert!(db.set_pin(42, &PinTarget::Film, "abc").await.is_err());
    }
}
