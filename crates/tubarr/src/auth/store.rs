//! The `users` and `sessions` tables.

use serde::Serialize;

use crate::store::{Db, StoreError, unix_now};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, sqlx::FromRow)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub display_name: Option<String>,
    pub role: String,
    #[serde(skip)]
    pub password_hash: Option<String>,
    pub oidc_issuer: Option<String>,
    pub oidc_subject: Option<String>,
    pub created_at: i64,
    pub last_login_at: Option<i64>,
    pub disabled: bool,
}

impl User {
    pub fn has_password(&self) -> bool {
        self.password_hash.is_some()
    }
}

/// A user to create.
#[derive(Debug, Clone, Default)]
pub struct NewUser {
    pub username: String,
    pub display_name: Option<String>,
    pub password_hash: Option<String>,
    /// Issuer and subject of an OpenID Connect account.
    pub oidc: Option<(String, String)>,
}

/// `SELECT <every user column> FROM users` followed by `$rest`, as one static string.
macro_rules! select_users {
    ($rest:literal) => {
        concat!(
            "SELECT id, username, display_name, role, password_hash, oidc_issuer, oidc_subject, ",
            "created_at, last_login_at, disabled FROM users",
            $rest
        )
    };
}

fn duplicate(e: sqlx::Error, what: &str) -> StoreError {
    match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            StoreError::Duplicate(what.to_owned())
        }
        _ => StoreError::Sql(e),
    }
}

/// Users and sessions on top of the shared database.
#[derive(Debug, Clone)]
pub struct AuthStore {
    db: Db,
}

impl AuthStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn user_count(&self) -> Result<i64, StoreError> {
        Ok(sqlx::query_scalar("SELECT COUNT(*) FROM users")
            .fetch_one(self.db.pool())
            .await?)
    }

    pub async fn list_users(&self) -> Result<Vec<User>, StoreError> {
        Ok(sqlx::query_as(select_users!(" ORDER BY id"))
            .fetch_all(self.db.pool())
            .await?)
    }

    pub async fn user(&self, id: i64) -> Result<Option<User>, StoreError> {
        Ok(sqlx::query_as(select_users!(" WHERE id = ?"))
            .bind(id)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn user_by_name(&self, username: &str) -> Result<Option<User>, StoreError> {
        Ok(sqlx::query_as(select_users!(" WHERE username = ?"))
            .bind(username)
            .fetch_optional(self.db.pool())
            .await?)
    }

    pub async fn user_by_oidc(
        &self,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<User>, StoreError> {
        Ok(
            sqlx::query_as(select_users!(" WHERE oidc_issuer = ? AND oidc_subject = ?"))
                .bind(issuer)
                .bind(subject)
                .fetch_optional(self.db.pool())
                .await?,
        )
    }

    pub async fn create_user(&self, user: &NewUser) -> Result<User, StoreError> {
        self.insert(user, false)
            .await?
            .ok_or_else(|| StoreError::Duplicate("user".to_owned()))
    }

    /// Creates `user` only while the table is empty, in one statement so two first-run requests
    /// cannot both win. `None` when a user already exists.
    pub async fn create_first_user(&self, user: &NewUser) -> Result<Option<User>, StoreError> {
        self.insert(user, true).await
    }

    async fn insert(&self, user: &NewUser, only_first: bool) -> Result<Option<User>, StoreError> {
        let sql = if only_first {
            "INSERT INTO users (username, display_name, password_hash, oidc_issuer, oidc_subject, \
             created_at) SELECT ?, ?, ?, ?, ?, ? WHERE NOT EXISTS (SELECT 1 FROM users) RETURNING id"
        } else {
            "INSERT INTO users (username, display_name, password_hash, oidc_issuer, oidc_subject, \
             created_at) VALUES (?, ?, ?, ?, ?, ?) RETURNING id"
        };
        let (issuer, subject) = user.oidc.clone().unzip();
        let id: Option<i64> = sqlx::query_scalar(sql)
            .bind(&user.username)
            .bind(&user.display_name)
            .bind(&user.password_hash)
            .bind(issuer)
            .bind(subject)
            .bind(unix_now())
            .fetch_optional(self.db.pool())
            .await
            .map_err(|e| duplicate(e, "user"))?;
        match id {
            Some(id) => self.user(id).await,
            None => Ok(None),
        }
    }

    /// Deletes a user and their sessions, unless they are the last enabled admin.
    /// `Ok(false)` when there is no such user.
    pub async fn delete_user(&self, id: i64) -> Result<Result<bool, LastAdmin>, StoreError> {
        // Reads before it writes: IMMEDIATE waits out a busy writer instead of failing at the
        // write.
        let mut tx = self.db.pool().begin_with("BEGIN IMMEDIATE").await?;
        let others: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE id != ? AND role = 'admin' AND disabled = 0",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if others == 0 {
            let exists: Option<i64> = sqlx::query_scalar("SELECT id FROM users WHERE id = ?")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?;
            return Ok(if exists.is_some() {
                Err(LastAdmin)
            } else {
                Ok(false)
            });
        }
        let done = sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Ok(done.rows_affected() > 0))
    }

    pub async fn set_password(&self, id: i64, hash: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
            .bind(hash)
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    pub async fn touch_login(&self, id: i64) -> Result<(), StoreError> {
        sqlx::query("UPDATE users SET last_login_at = ? WHERE id = ?")
            .bind(unix_now())
            .bind(id)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    pub async fn create_session(
        &self,
        token_hash: &str,
        user_id: i64,
        max_age_secs: i64,
    ) -> Result<(), StoreError> {
        let now = unix_now();
        sqlx::query(
            "INSERT INTO sessions (token_hash, user_id, created_at, last_seen_at, expires_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(token_hash)
        .bind(user_id)
        .bind(now)
        .bind(now)
        .bind(now.saturating_add(max_age_secs))
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// The enabled user behind a live session. Seen sessions slide their idle end forward (at
    /// most one write a minute), and expired ones are deleted.
    pub async fn session_user(
        &self,
        token_hash: &str,
        idle_secs: i64,
    ) -> Result<Option<User>, StoreError> {
        let now = unix_now();
        let row: Option<(i64, i64, i64)> = sqlx::query_as(
            "SELECT user_id, last_seen_at, expires_at FROM sessions WHERE token_hash = ?",
        )
        .bind(token_hash)
        .fetch_optional(self.db.pool())
        .await?;
        let Some((user_id, last_seen, expires_at)) = row else {
            return Ok(None);
        };
        if now >= expires_at || now - last_seen >= idle_secs {
            self.delete_session(token_hash).await?;
            return Ok(None);
        }
        if now - last_seen >= 60 {
            sqlx::query("UPDATE sessions SET last_seen_at = ? WHERE token_hash = ?")
                .bind(now)
                .bind(token_hash)
                .execute(self.db.pool())
                .await?;
        }
        Ok(self.user(user_id).await?.filter(|u| !u.disabled))
    }

    pub async fn delete_session(&self, token_hash: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
            .bind(token_hash)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    /// Ends every session of `user_id` but `keep`, e.g. after a password change.
    pub async fn delete_other_sessions(&self, user_id: i64, keep: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token_hash != ?")
            .bind(user_id)
            .bind(keep)
            .execute(self.db.pool())
            .await?;
        Ok(())
    }

    /// Drops sessions past their absolute end. Idle ones go when next presented.
    pub async fn prune_sessions(&self) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM sessions WHERE expires_at <= ?")
            .bind(unix_now())
            .execute(self.db.pool())
            .await?;
        Ok(())
    }
}

/// Refused: removing this user would leave nobody able to sign in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastAdmin;
