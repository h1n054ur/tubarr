//! Session tokens and the cookies that carry them.

use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, header};
use sha2::{Digest, Sha256};

pub const SESSION_COOKIE: &str = "tubarr_session";
/// Ties an OpenID Connect callback to the browser that started the sign-in.
pub const OIDC_COOKIE: &str = "tubarr_oidc";
pub const OIDC_COOKIE_PATH: &str = "/auth/oidc";

/// 256 random bits, hex encoded. `rand`'s thread generator is a CSPRNG seeded from the OS.
pub fn new_token() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

/// What the database keeps instead of the token itself.
pub fn token_hash(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// The value of cookie `name` in the request, if any.
pub fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v)
        .filter(|v| !v.is_empty())
}

/// A `Set-Cookie` value. `max_age` of zero deletes the cookie.
pub fn set_cookie(
    name: &str,
    value: &str,
    path: &str,
    max_age: Duration,
    secure: bool,
) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    let text = format!(
        "{name}={value}; Path={path}; Max-Age={}; HttpOnly; SameSite=Lax{secure}",
        max_age.as_secs()
    );
    // Names, hex tokens and fixed paths only, so the value is always a valid header.
    HeaderValue::from_str(&text).unwrap_or_else(|_| HeaderValue::from_static(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_and_hashed() {
        let a = new_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, new_token());
        assert_eq!(token_hash(&a).len(), 64);
        assert_ne!(token_hash(&a), a);
    }

    #[test]
    fn finds_cookie_among_others() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; tubarr_session=abc; b=2"),
        );
        assert_eq!(cookie(&headers, SESSION_COOKIE), Some("abc"));
        assert_eq!(cookie(&headers, "c"), None);
    }

    #[test]
    fn cookie_flags() {
        let v = set_cookie("n", "v", "/", Duration::from_secs(60), true);
        assert_eq!(
            v.to_str().unwrap(),
            "n=v; Path=/; Max-Age=60; HttpOnly; SameSite=Lax; Secure"
        );
    }
}
