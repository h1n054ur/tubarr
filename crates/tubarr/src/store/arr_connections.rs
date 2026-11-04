//! Sonarr and Radarr connections saved from the Settings page. The config file's `[sonarr]` and
//! `[radarr]` win over these, see [`crate::arr::connections`].

use super::{Db, StoreError, unix_now};

/// A saved connection as stored: the arr's base URL and its API key.
#[derive(Clone, PartialEq, Eq)]
pub struct SavedArr {
    pub url: String,
    pub api_key: String,
}

impl std::fmt::Debug for SavedArr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SavedArr")
            .field("url", &self.url)
            .field("api_key", &"***")
            .finish()
    }
}

impl Db {
    /// The connection saved for `app` (`sonarr` or `radarr`), if any.
    pub async fn saved_arr(&self, app: &str) -> Result<Option<SavedArr>, StoreError> {
        let row: Option<(String, String)> =
            sqlx::query_as("SELECT url, api_key FROM arr_connections WHERE app = ?")
                .bind(app)
                .fetch_optional(&self.pool)
                .await?;
        Ok(row.map(|(url, api_key)| SavedArr { url, api_key }))
    }

    /// Saves or replaces the connection for `app`.
    pub async fn save_arr(&self, app: &str, saved: &SavedArr) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO arr_connections (app, url, api_key, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (app) DO UPDATE SET url = excluded.url, api_key = excluded.api_key, \
             updated_at = excluded.updated_at",
        )
        .bind(app)
        .bind(&saved.url)
        .bind(&saved.api_key)
        .bind(unix_now())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Removes the connection saved for `app`. True when there was one.
    pub async fn forget_arr(&self, app: &str) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM arr_connections WHERE app = ?")
            .bind(app)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }
}
