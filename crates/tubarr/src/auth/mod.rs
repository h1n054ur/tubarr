//! Who may use the web UI and `/admin/api`.
//!
//! A request is let in by one of:
//! - an `X-Api-Key` header with Tubarr's API key (scripts, and the old UI flow),
//! - a session cookie from a password or OpenID Connect sign-in,
//! - with `auth.local_bypass`, a client on loopback or a private network. The client is the TCP
//!   peer, or, when the peer is listed in `auth.trusted_proxies`, the address that proxy names in
//!   `X-Forwarded-For` (see [`forwarded_client`]). A trusted proxy is never let in as itself, so a
//!   proxy that forwards without `X-Forwarded-For` gets nobody in. Behind a reverse proxy that is
//!   not listed, every request looks local and bypass must stay off.
//!
//! CSRF: every state-changing request that is not authenticated by `X-Api-Key` (so cookie and
//! local bypass requests, and every `POST /auth/...`) must carry `X-Requested-With: tubarr`.
//! Browsers cannot add that header cross-site without a CORS preflight, which Tubarr never
//! grants.
//!
//! `/api` (Sonarr and Radarr) keeps its `apikey=` check, `/healthz` is open, and `/play` is open
//! to whatever `auth.play_allowed_networks` allows (loopback and private addresses by default):
//! Jellyfin fetches it without credentials, so the network is what protects it.
//!
//! The same client address judges `/play`, local bypass and the sign-in and setup code throttles,
//! so behind a trusted proxy each real client is counted on its own.

mod limiter;
mod oidc;
mod password;
mod routes;
mod session;
mod settings;
mod setup_code;
pub mod store;

use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};

use axum::{
    Json,
    extract::{ConnectInfo, Request},
    http::{Extensions, HeaderMap, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use ipnet::IpNet;
use serde_json::json;

pub use routes::router;
pub use settings::{
    AuthConfig, AuthFile, DEFAULT_PLAY_NETWORKS, Method as AuthMethod, OIDC_CALLBACK_PATH,
    OidcConfig,
};
pub use setup_code::SETUP_CODE_ENV;
pub use store::{AuthStore, User};

use crate::{AppState, admin::KEY_HEADER, api::constant_time_eq, store::Db};

/// The header and value every browser write must carry.
pub const CSRF_HEADER: &str = "x-requested-with";
pub const CSRF_VALUE: &str = "tubarr";

/// Sign-in state shared by every request.
#[derive(Debug)]
pub struct Auth {
    pub config: AuthConfig,
    pub store: AuthStore,
    limiter: limiter::LoginLimiter,
    /// Wrong setup codes, counted per client address only, so nobody can lock the owner out.
    setup_limiter: limiter::LoginLimiter,
    setup_code: setup_code::SetupCode,
    oidc: Option<oidc::Oidc>,
}

impl Auth {
    /// Fails only if the HTTP client for the OpenID Connect provider cannot be built.
    pub fn new(config: AuthConfig, db: Db) -> Result<Self, reqwest::Error> {
        let oidc = config.oidc.clone().map(oidc::Oidc::new).transpose()?;
        let setup_code = setup_code::SetupCode::new(config.setup_code.as_ref().map(|c| c.expose()));
        Ok(Self {
            config,
            store: AuthStore::new(db),
            limiter: limiter::LoginLimiter::default(),
            setup_limiter: limiter::LoginLimiter::default(),
            setup_code,
            oidc,
        })
    }

    /// The client address behind the request (see [`forwarded_client`]).
    pub fn client_ip(&self, extensions: &Extensions, headers: &HeaderMap) -> Option<IpAddr> {
        let peer = peer_ip(extensions)?;
        forwarded_client(&self.config.trusted_proxies, peer, headers)
    }

    /// The address the throttles count: the client, or the TCP peer when a trusted proxy does not
    /// establish a client through `X-Forwarded-For`.
    fn throttle_ip(&self, extensions: &Extensions, headers: &HeaderMap) -> Option<IpAddr> {
        self.client_ip(extensions, headers)
            .or_else(|| peer_ip(extensions))
    }

    /// Whether `auth.local_bypass` lets this request in: on, and the client is local and not
    /// itself a trusted proxy.
    fn bypasses(&self, extensions: &Extensions, headers: &HeaderMap) -> bool {
        self.config.local_bypass
            && self
                .client_ip(extensions, headers)
                .is_some_and(|ip| is_local(ip) && !in_any(&self.config.trusted_proxies, ip))
    }

    /// The random setup code while setup is open, as the log shows it. `None` once an admin
    /// exists, and for a code from `TUBARR_SETUP_CODE`.
    pub fn setup_code(&self) -> Option<String> {
        self.setup_code.shown()
    }

    /// Run once at start: with no user yet, logs how to claim the instance, with the setup code
    /// unless it came from `TUBARR_SETUP_CODE`. With a user, or without password sign-in, setup is
    /// closed and the code is dropped.
    pub async fn announce_setup(&self) -> Result<(), crate::store::StoreError> {
        if !self.config.forms || self.store.user_count().await? > 0 {
            self.setup_code.clear();
            return Ok(());
        }
        let url = &self.config.ui_url;
        match self.setup_code.shown() {
            Some(code) => tracing::warn!(
                "No admin account yet. To claim this Tubarr, open {url} and enter the setup code {code}"
            ),
            None => tracing::warn!(
                "No admin account yet. To claim this Tubarr, open {url} and enter the setup code from {SETUP_CODE_ENV}"
            ),
        }
        Ok(())
    }
}

/// How a request got in.
#[derive(Debug, Clone)]
pub enum Principal {
    ApiKey,
    LocalBypass,
    Session { user: Box<User>, token_hash: String },
}

impl Principal {
    pub fn user(&self) -> Option<&User> {
        match self {
            Self::Session { user, .. } => Some(user),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("not signed in and no valid API key")]
    Unauthorized,
    #[error("wrong username or password")]
    BadCredentials,
    #[error("too many failed sign-ins, try again in {0} s")]
    Throttled(u64),
    #[error("missing the X-Requested-With: tubarr header")]
    Csrf,
    #[error("wrong or missing setup code, find it in Tubarr's log")]
    BadSetupCode,
    #[error("{0}")]
    Conflict(&'static str),
    #[error("{0}")]
    Invalid(String),
    #[error("{0} is not enabled")]
    Off(&'static str),
    #[error("not found")]
    NotFound,
    #[error("internal error")]
    Internal,
}

impl From<crate::store::StoreError> for AuthError {
    fn from(e: crate::store::StoreError) -> Self {
        match e {
            crate::store::StoreError::Duplicate(_) => Self::Conflict("that username is taken"),
            e => {
                tracing::error!(error = %e, "auth store error");
                Self::Internal
            }
        }
    }
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::Unauthorized | Self::BadCredentials => StatusCode::UNAUTHORIZED,
            Self::Throttled(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::Csrf | Self::BadSetupCode => StatusCode::FORBIDDEN,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Invalid(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Off(_) | Self::NotFound => StatusCode::NOT_FOUND,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let mut response = (status, Json(json!({ "error": self.to_string() }))).into_response();
        if let Self::Throttled(secs) = self {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, secs.into());
        }
        response
    }
}

/// The TCP peer, with IPv4-mapped IPv6 addresses turned back into IPv4.
pub fn peer_ip(extensions: &Extensions) -> Option<IpAddr> {
    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip().to_canonical())
}

/// Loopback, RFC 1918, IPv6 unique local and link-local addresses.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
    }
}

pub fn csrf_ok(headers: &HeaderMap) -> bool {
    headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case(CSRF_VALUE))
}

pub fn is_write(method: &Method) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Lets a request in by API key, session or local bypass, in that order. A wrong API key is
/// refused outright rather than falling back to the cookie.
pub async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    extensions: &Extensions,
) -> Result<Principal, AuthError> {
    let given_key = headers
        .get(KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty());
    if let Some(given) = given_key {
        let expected = state
            .config
            .api_key
            .as_ref()
            .ok_or(AuthError::Unauthorized)?;
        return if constant_time_eq(expected.expose().as_bytes(), given.as_bytes()) {
            Ok(Principal::ApiKey)
        } else {
            Err(AuthError::Unauthorized)
        };
    }
    if let Some(token) = session::cookie(headers, session::SESSION_COOKIE) {
        let token_hash = session::token_hash(token);
        let idle = i64::try_from(state.auth.config.session_ttl.as_secs()).unwrap_or(i64::MAX);
        if let Some(user) = state.auth.store.session_user(&token_hash, idle).await? {
            return Ok(Principal::Session {
                user: Box::new(user),
                token_hash,
            });
        }
    }
    if state.auth.bypasses(extensions, headers) {
        return Ok(Principal::LocalBypass);
    }
    Err(AuthError::Unauthorized)
}

/// [`authenticate`] plus the CSRF header on writes that did not use the API key.
pub async fn authorize(
    state: &AppState,
    method: &Method,
    headers: &HeaderMap,
    extensions: &Extensions,
) -> Result<Principal, AuthError> {
    let principal = authenticate(state, headers, extensions).await?;
    if is_write(method) && !matches!(principal, Principal::ApiKey) && !csrf_ok(headers) {
        return Err(AuthError::Csrf);
    }
    Ok(principal)
}

/// Who may fetch `/play`, from `[auth]`.
#[derive(Debug, Clone)]
pub struct PlayAccess {
    pub networks: Vec<IpNet>,
    pub trusted_proxies: Vec<IpNet>,
}

impl PlayAccess {
    pub fn from_config(config: &AuthConfig) -> Self {
        Self {
            networks: config.play_allowed_networks.clone(),
            trusted_proxies: config.trusted_proxies.clone(),
        }
    }

    pub fn client_ip(&self, peer: IpAddr, headers: &HeaderMap) -> Option<IpAddr> {
        forwarded_client(&self.trusted_proxies, peer, headers)
    }

    fn allows(&self, ip: IpAddr) -> bool {
        self.networks.iter().any(|net| net.contains(&ip))
    }

    /// Whether the client behind a request is in the allowed networks. A request without a peer
    /// address is refused.
    pub fn allows_request(&self, extensions: &Extensions, headers: &HeaderMap) -> bool {
        peer_ip(extensions)
            .and_then(|peer| self.client_ip(peer, headers))
            .is_some_and(|ip| self.allows(ip))
    }
}

const FORWARDED_FOR: &str = "x-forwarded-for";

fn in_any(nets: &[IpNet], ip: IpAddr) -> bool {
    nets.iter().any(|net| net.contains(&ip))
}

/// The client address: the TCP peer, or, when the peer is in `trusted`, the right-most
/// `X-Forwarded-For` entry that is not itself trusted. A trusted peer with no header or only
/// trusted hops gives `None`. An encountered entry that does not parse also gives `None`, since
/// nothing to its left can be believed.
pub fn forwarded_client(trusted: &[IpNet], peer: IpAddr, headers: &HeaderMap) -> Option<IpAddr> {
    if !in_any(trusted, peer) {
        return Some(peer);
    }
    let mut hops: Vec<&str> = headers
        .get_all(FORWARDED_FOR)
        .iter()
        .map(|v| v.to_str().ok())
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .collect();
    while let Some(hop) = hops.pop() {
        let client = hop.parse::<IpAddr>().ok()?.to_canonical();
        if !in_any(trusted, client) {
            return Some(client);
        }
    }
    None
}

/// Middleware for `/play`: refuses clients outside `auth.play_allowed_networks`.
pub async fn play_guard(access: Arc<PlayAccess>, request: Request, next: Next) -> Response {
    if !access.allows_request(request.extensions(), request.headers()) {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "192.168.1.1",
            "172.16.0.1",
            "::1",
            "fd00::1",
        ] {
            assert!(is_local(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "172.32.0.1", "2001:db8::1"] {
            assert!(!is_local(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn mapped_peer_is_canonical() {
        let mut ext = Extensions::new();
        let addr: SocketAddr = "[::ffff:10.0.0.5]:4000".parse().unwrap();
        ext.insert(ConnectInfo(addr));
        assert_eq!(peer_ip(&ext), Some("10.0.0.5".parse().unwrap()));
    }

    fn access(proxies: &[&str]) -> PlayAccess {
        PlayAccess {
            networks: vec!["10.0.0.0/8".parse().unwrap()],
            trusted_proxies: proxies.iter().map(|p| p.parse().unwrap()).collect(),
        }
    }

    fn forwarded(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for v in values {
            headers.append(FORWARDED_FOR, v.parse().unwrap());
        }
        headers
    }

    #[test]
    fn forwarded_for_counts_only_from_trusted_proxies() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let untrusted = access(&[]);
        let spoof = forwarded(&["10.0.0.9"]);
        assert_eq!(
            untrusted.client_ip(ip("203.0.113.7"), &spoof),
            Some(ip("203.0.113.7"))
        );

        let proxied = access(&["10.0.0.2/32", "10.0.0.3/32"]);
        // The client prepends a fake hop: only the right-most untrusted entry counts.
        let headers = forwarded(&["10.0.0.9, 203.0.113.7", "10.0.0.3"]);
        assert_eq!(
            proxied.client_ip(ip("10.0.0.2"), &headers),
            Some(ip("203.0.113.7"))
        );
        let lan = forwarded(&["::ffff:10.1.2.3"]);
        assert_eq!(
            proxied.client_ip(ip("10.0.0.2"), &lan),
            Some(ip("10.1.2.3"))
        );
        assert_eq!(proxied.client_ip(ip("10.0.0.2"), &HeaderMap::new()), None);
        assert_eq!(
            proxied.client_ip(ip("10.0.0.2"), &forwarded(&["nope"])),
            None
        );
    }

    #[test]
    fn trusted_proxy_requires_an_untrusted_client() {
        let proxied = access(&["10.0.0.2/32", "10.0.0.3/32"]);
        let peer = "10.0.0.2".parse().unwrap();
        for values in [
            vec![],
            vec!["10.0.0.3"],
            vec!["10.0.0.2, 10.0.0.3"],
            vec!["10.0.0.2", "::ffff:10.0.0.3"],
        ] {
            assert_eq!(
                proxied.client_ip(peer, &forwarded(&values)),
                None,
                "{values:?}"
            );
        }
    }

    #[test]
    fn encountered_invalid_forwarded_hops_fail_closed() {
        let proxied = access(&["10.0.0.2/32", "10.0.0.3/32"]);
        let peer = "10.0.0.2".parse().unwrap();
        for values in [
            vec![""],
            vec![" "],
            vec!["nope"],
            vec!["10.1.2.3,"],
            vec!["10.1.2.3, , 10.0.0.3"],
            vec!["10.1.2.3", "nope, 10.0.0.3"],
            vec!["10.1.2.3", ""],
        ] {
            assert_eq!(
                proxied.client_ip(peer, &forwarded(&values)),
                None,
                "{values:?}"
            );
        }
        let mut non_text = forwarded(&["10.1.2.3"]);
        non_text.append(
            FORWARDED_FOR,
            axum::http::HeaderValue::from_bytes(&[0xff]).unwrap(),
        );
        assert_eq!(proxied.client_ip(peer, &non_text), None);
    }

    #[tokio::test]
    async fn unidentified_proxy_throttles_still_count_the_tcp_peer() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::config::Config::from_toml(&format!(
            "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\n[auth]\ntrusted_proxies = [\"10.0.0.2/32\", \"10.0.0.3/32\"]\n",
            dir.path()
        ))
        .unwrap();
        let db = Db::open(&config.database_path()).await.unwrap();
        let auth = Auth::new(config.auth, db).unwrap();
        let mut ext = Extensions::new();
        ext.insert(ConnectInfo(
            "[::ffff:10.0.0.2]:4000".parse::<SocketAddr>().unwrap(),
        ));
        for values in [vec![], vec![""], vec!["nope"], vec!["10.0.0.3"]] {
            assert_eq!(
                auth.throttle_ip(&ext, &forwarded(&values)),
                Some("10.0.0.2".parse().unwrap()),
                "{values:?}"
            );
        }
        assert_eq!(
            auth.throttle_ip(&ext, &forwarded(&["203.0.113.7, 10.0.0.3"])),
            Some("203.0.113.7".parse().unwrap())
        );
    }
}
