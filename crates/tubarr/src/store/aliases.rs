//! Aliases: other names a mapping goes by on YouTube. The film matcher scores them like the title,
//! and the show and daily matchers accept a video title naming the series title or any alias.
//!
//! Each alias is the user's or the arr sync's (Sonarr's alternate titles). The sync refreshes only
//! its own, so a user alias survives every sync.

use serde::Serialize;
use sqlx::{SqliteConnection, types::Json};

use super::{Db, StoreError};

impl Db {
    /// Makes `aliases` the full list for a mapping. Blank entries and repeats are dropped. An alias
    /// the arr sync added stays the sync's while it is in the list. One left out is removed, and
    /// comes back on the next sync if the arr still lists it.
    pub async fn set_aliases(&self, mapping_id: i64, aliases: &[String]) -> Result<(), StoreError> {
        let aliases = clean(aliases);
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "DELETE FROM mapping_aliases WHERE mapping_id = ? \
             AND alias NOT IN (SELECT value FROM json_each(?))",
        )
        .bind(mapping_id)
        .bind(Json(&aliases))
        .execute(&mut *tx)
        .await?;
        insert(&mut tx, mapping_id, &aliases, "user").await?;
        tx.commit().await?;
        Ok(())
    }

    /// The aliases of a mapping, sorted.
    pub async fn aliases(&self, mapping_id: i64) -> Result<Vec<String>, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT alias FROM mapping_aliases WHERE mapping_id = ? ORDER BY alias",
        )
        .bind(mapping_id)
        .fetch_all(&self.pool)
        .await?)
    }

    /// The aliases of a mapping with the source of each, sorted, for the admin UI.
    pub async fn alias_entries(&self, mapping_id: i64) -> Result<Vec<AliasEntry>, StoreError> {
        Ok(sqlx::query_as(
            "SELECT alias, source FROM mapping_aliases WHERE mapping_id = ? ORDER BY alias",
        )
        .bind(mapping_id)
        .fetch_all(&self.pool)
        .await?)
    }
}

/// One alias and where it came from: `arr` (the sync) or `user`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct AliasEntry {
    pub alias: String,
    pub source: String,
}

/// Makes `titles` the arr's aliases of a mapping: the arr's ones it no longer lists go, user
/// aliases stay (a title the user already added stays the user's).
pub(super) async fn refresh_arr_aliases(
    conn: &mut SqliteConnection,
    mapping_id: i64,
    titles: &[String],
) -> Result<(), StoreError> {
    let titles = clean(titles);
    sqlx::query(
        "DELETE FROM mapping_aliases WHERE mapping_id = ? AND source = 'arr' \
         AND alias NOT IN (SELECT value FROM json_each(?))",
    )
    .bind(mapping_id)
    .bind(Json(&titles))
    .execute(&mut *conn)
    .await?;
    insert(conn, mapping_id, &titles, "arr").await
}

fn clean(aliases: &[String]) -> Vec<String> {
    let mut out: Vec<String> = aliases
        .iter()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
        .map(str::to_owned)
        .collect();
    out.sort();
    out.dedup();
    out
}

async fn insert(
    conn: &mut SqliteConnection,
    mapping_id: i64,
    aliases: &[String],
    source: &str,
) -> Result<(), StoreError> {
    for alias in aliases {
        sqlx::query(
            "INSERT OR IGNORE INTO mapping_aliases (mapping_id, alias, source) VALUES (?, ?, ?)",
        )
        .bind(mapping_id)
        .bind(alias)
        .bind(source)
        .execute(&mut *conn)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::refresh_arr_aliases;
    use crate::store::{Db, mappings::tests::drama, test_db};

    async fn sources(db: &Db, m: i64) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT alias, source FROM mapping_aliases WHERE mapping_id = ? ORDER BY alias",
        )
        .bind(m)
        .fetch_all(&db.pool)
        .await
        .unwrap()
    }

    async fn refresh(db: &Db, m: i64, titles: &[&str]) {
        let titles: Vec<String> = titles.iter().map(|t| (*t).to_owned()).collect();
        let mut conn = db.pool.acquire().await.unwrap();
        refresh_arr_aliases(&mut conn, m, &titles).await.unwrap();
    }

    fn pair(alias: &str, source: &str) -> (String, String) {
        (alias.to_owned(), source.to_owned())
    }

    #[tokio::test]
    async fn arr_refresh_keeps_user_aliases() {
        let (db, _dir) = test_db::fresh().await;
        let m = db.insert_mapping(&drama()).await.unwrap().id;
        db.set_aliases(m, &["The Tale".to_owned()]).await.unwrap();
        refresh(&db, m, &["Dastan", "The Tale", " "]).await;
        assert_eq!(
            sources(&db, m).await,
            [pair("Dastan", "arr"), pair("The Tale", "user")]
        );
        // The arr drops both names: its own goes, the user's stays.
        refresh(&db, m, &["Daastan"]).await;
        assert_eq!(
            sources(&db, m).await,
            [pair("Daastan", "arr"), pair("The Tale", "user")]
        );
        // Saving the list from the admin UI keeps the arr's alias the arr's.
        db.set_aliases(m, &["Daastan".to_owned(), "Dastaan".to_owned()])
            .await
            .unwrap();
        assert_eq!(
            sources(&db, m).await,
            [pair("Daastan", "arr"), pair("Dastaan", "user")]
        );
    }

    #[tokio::test]
    async fn set_replaces_trims_and_cascades() {
        let (db, _dir) = test_db::fresh().await;
        let m = db.insert_mapping(&drama()).await.unwrap().id;
        let first = [
            "Policeodu".to_owned(),
            " Theri ".to_owned(),
            "Theri".to_owned(),
        ];
        db.set_aliases(m, &first).await.unwrap();
        assert_eq!(db.aliases(m).await.unwrap(), ["Policeodu", "Theri"]);
        db.set_aliases(m, &["  ".to_owned(), "Kathi".to_owned()])
            .await
            .unwrap();
        assert_eq!(db.aliases(m).await.unwrap(), ["Kathi"]);
        db.delete_mapping(m).await.unwrap();
        assert!(db.aliases(m).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn alias_needs_an_existing_mapping() {
        let (db, _dir) = test_db::fresh().await;
        assert!(db.set_aliases(42, &["x".to_owned()]).await.is_err());
    }
}
