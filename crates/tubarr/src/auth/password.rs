//! Password hashing with argon2id at the OWASP minimum (19 MiB, 2 passes, 1 lane).
//!
//! Hashing takes tens of milliseconds of CPU on purpose, so it runs on the blocking pool.

use std::sync::LazyLock;

use argon2::{
    Algorithm, Argon2, Params, Version,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};

pub const MIN_PASSWORD: usize = 8;
/// Long enough for any passphrase, short enough that hashing a huge body costs nothing extra.
pub const MAX_PASSWORD: usize = 256;
pub const MAX_USERNAME: usize = 64;

/// Verified against when the username does not exist, so a miss takes as long as a wrong password.
static DUMMY_HASH: LazyLock<Option<String>> =
    LazyLock::new(|| hash_now("tubarr-dummy-password").ok());

fn argon2() -> Argon2<'static> {
    Argon2::new(
        Algorithm::Argon2id,
        Version::V0x13,
        Params::new(
            Params::DEFAULT_M_COST,
            Params::DEFAULT_T_COST,
            Params::DEFAULT_P_COST,
            None,
        )
        .unwrap_or_default(),
    )
}

fn hash_now(password: &str) -> Result<String, argon2::password_hash::Error> {
    Ok(argon2().hash_password(password.as_bytes())?.to_string())
}

#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    #[error("password hashing failed")]
    Hash,
}

/// A PHC string for `password`, with a fresh random salt.
pub async fn hash(password: String) -> Result<String, PasswordError> {
    tokio::task::spawn_blocking(move || hash_now(&password))
        .await
        .map_err(|_| PasswordError::Hash)?
        .map_err(|_| PasswordError::Hash)
}

/// Whether `password` matches `stored`. With no stored hash it still does the work of one check
/// and answers false.
pub async fn verify(password: String, stored: Option<String>) -> bool {
    tokio::task::spawn_blocking(move || {
        let (phc, real) = match stored {
            Some(phc) => (phc, true),
            None => match DUMMY_HASH.as_ref() {
                Some(dummy) => (dummy.clone(), false),
                None => return false,
            },
        };
        let Ok(parsed) = PasswordHash::new(&phc) else {
            return false;
        };
        let matched = argon2()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok();
        matched && real
    })
    .await
    .unwrap_or(false)
}

/// Usernames are 1 to 64 visible characters with no spaces, so they are safe to show and log.
pub fn valid_username(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= MAX_USERNAME
        && name.chars().all(|c| !c.is_control() && !c.is_whitespace())
}

pub fn valid_password(password: &str) -> bool {
    (MIN_PASSWORD..=MAX_PASSWORD).contains(&password.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hash_round_trips_with_owasp_params() {
        let phc = hash("correct horse".to_owned()).await.unwrap();
        assert!(phc.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{phc}");
        assert!(verify("correct horse".to_owned(), Some(phc.clone())).await);
        assert!(!verify("wrong horse".to_owned(), Some(phc)).await);
        assert!(!verify("tubarr-dummy-password".to_owned(), None).await);
        assert!(!verify("x".to_owned(), Some("not a hash".to_owned())).await);
    }

    #[test]
    fn lengths_are_bounded() {
        assert!(valid_username("hani") && !valid_username("") && !valid_username("a b"));
        assert!(!valid_username(&"x".repeat(65)));
        assert!(valid_password("12345678") && !valid_password("1234567"));
        assert!(!valid_password(&"x".repeat(257)));
    }
}
