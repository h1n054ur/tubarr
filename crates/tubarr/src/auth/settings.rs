//! The `[auth]` config section: how people sign in to the web UI.

use std::{path::PathBuf, time::Duration};

use ipnet::IpNet;
use serde::Deserialize;
use url::Url;

use super::setup_code::{MIN_PRESET_LEN, SETUP_CODE_ENV, normalize};
use crate::config::{ConfigError, Secret, check_http_url, read_secret};

/// Browsers cap cookie lifetimes at 400 days.
const MAX_SESSION_AGE: Duration = Duration::from_secs(400 * 24 * 60 * 60);
const MIN_SESSION_TTL: Duration = Duration::from_secs(5 * 60);

/// Where the provider sends people back, under the UI's base URL.
pub const OIDC_CALLBACK_PATH: &str = "auth/oidc/callback";

/// Who may fetch `/play` when `play_allowed_networks` is unset or empty: loopback, private
/// (RFC 1918, IPv6 unique local), link-local and the CGNAT range Tailscale and NetBird use.
pub const DEFAULT_PLAY_NETWORKS: [&str; 9] = [
    "127.0.0.0/8",
    "::1/128",
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "100.64.0.0/10",
    "fc00::/7",
    "fe80::/10",
    "169.254.0.0/16",
];

/// Which sign-in methods the UI offers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub enum Method {
    #[default]
    #[serde(rename = "forms")]
    Forms,
    #[serde(rename = "oidc")]
    Oidc,
    #[serde(rename = "forms+oidc")]
    FormsAndOidc,
}

/// `[auth]` as written.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct AuthFile {
    method: Method,
    /// A session ends after this long without a request.
    #[serde(with = "humantime_serde")]
    session_ttl: Duration,
    /// A session ends this long after sign-in, however active.
    #[serde(with = "humantime_serde")]
    session_max_age: Duration,
    /// Unset: on when the UI URL is https.
    cookie_secure: Option<bool>,
    local_bypass: bool,
    /// The URL people open the UI at, when it is not `public_url` (which is the address Jellyfin
    /// and the arrs use). The OpenID Connect redirect is built from it.
    ui_url: Option<Url>,
    /// Unset or empty: [`DEFAULT_PLAY_NETWORKS`].
    play_allowed_networks: Option<Vec<IpNet>>,
    /// Reverse proxies whose `X-Forwarded-For` names the client for `/play`, local bypass and the
    /// sign-in throttles.
    trusted_proxies: Vec<IpNet>,
    oidc: Option<OidcFile>,
}

impl Default for AuthFile {
    fn default() -> Self {
        Self {
            method: Method::Forms,
            session_ttl: Duration::from_secs(30 * 24 * 60 * 60),
            session_max_age: Duration::from_secs(90 * 24 * 60 * 60),
            cookie_secure: None,
            local_bypass: false,
            ui_url: None,
            play_allowed_networks: None,
            trusted_proxies: Vec::new(),
            oidc: None,
        }
    }
}

/// `[auth.oidc]` as written.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct OidcFile {
    /// Kept as written: the ID token's `iss` must match it exactly, trailing slash included.
    issuer_url: String,
    client_id: String,
    client_secret_file: PathBuf,
    #[serde(default = "default_scopes")]
    scopes: Vec<String>,
    #[serde(default)]
    allowed_groups: Vec<String>,
    #[serde(default = "default_groups_claim")]
    groups_claim: String,
    #[serde(default = "default_button_label")]
    button_label: String,
    auto_create_users: Option<bool>,
}

fn default_scopes() -> Vec<String> {
    ["openid", "profile", "email", "groups"]
        .map(str::to_owned)
        .to_vec()
}

fn default_groups_claim() -> String {
    "groups".to_owned()
}

fn default_button_label() -> String {
    "Sign in with SSO".to_owned()
}

/// The checked `[auth]` settings.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// Username and password sign-in.
    pub forms: bool,
    pub oidc: Option<OidcConfig>,
    pub session_ttl: Duration,
    pub session_max_age: Duration,
    pub cookie_secure: bool,
    /// Requests from loopback and private clients skip sign-in.
    pub local_bypass: bool,
    /// Who may fetch `/play`. Never empty: unset means [`DEFAULT_PLAY_NETWORKS`].
    pub play_allowed_networks: Vec<IpNet>,
    /// True when `play_allowed_networks` was left unset (or empty) and the default applies.
    pub play_networks_default: bool,
    /// Peers whose `X-Forwarded-For` is believed about the client address.
    pub trusted_proxies: Vec<IpNet>,
    /// Where people open the UI: `auth.ui_url`, else `public_url`.
    pub ui_url: Url,
    /// The first-run setup code from `TUBARR_SETUP_CODE`. Unset: a random one at start.
    pub setup_code: Option<Secret>,
}

impl AuthConfig {
    /// The start-up warning for an install that relies on the default `/play` networks.
    pub fn play_networks_warning(&self) -> Option<String> {
        self.play_networks_default.then(|| {
            format!(
                "auth.play_allowed_networks is not set, so /play only answers loopback and private \
                 addresses ({}). Set it to the networks Jellyfin fetches from, or to \
                 [\"0.0.0.0/0\", \"::/0\"] to allow everyone",
                DEFAULT_PLAY_NETWORKS.join(", ")
            )
        })
    }
}

fn default_play_networks() -> Vec<IpNet> {
    DEFAULT_PLAY_NETWORKS
        .iter()
        .map(|net| net.parse().expect("valid default network"))
        .collect()
}

#[derive(Debug, Clone)]
pub struct OidcConfig {
    pub issuer_url: String,
    pub client_id: String,
    pub client_secret: Secret,
    pub scopes: Vec<String>,
    /// Empty lets in anyone the provider signs in (subject to `auto_create_users`).
    pub allowed_groups: Vec<String>,
    pub groups_claim: String,
    pub button_label: String,
    pub auto_create_users: bool,
    pub redirect_url: Url,
}

fn invalid(key: &'static str, reason: &str) -> ConfigError {
    ConfigError::Invalid {
        key,
        reason: reason.to_owned(),
    }
}

impl AuthFile {
    pub(crate) fn check(self, public_url: &Url) -> Result<AuthConfig, ConfigError> {
        if self.session_ttl < MIN_SESSION_TTL {
            return Err(invalid("auth.session_ttl", "must be at least 5m"));
        }
        if self.session_max_age < self.session_ttl || self.session_max_age > MAX_SESSION_AGE {
            return Err(invalid(
                "auth.session_max_age",
                "must be at least auth.session_ttl and at most 400 days",
            ));
        }
        let ui_url = match self.ui_url {
            Some(url) => check_http_url("auth.ui_url", url)?,
            None => public_url.clone(),
        };
        let wants_oidc = matches!(self.method, Method::Oidc | Method::FormsAndOidc);
        let oidc = match (self.oidc, wants_oidc) {
            (Some(file), true) => Some(file.check(&ui_url)?),
            (None, true) => {
                return Err(invalid(
                    "auth.oidc",
                    "is required when auth.method includes oidc",
                ));
            }
            (Some(_), false) => {
                return Err(invalid(
                    "auth.oidc",
                    "is set but auth.method does not include oidc",
                ));
            }
            (None, false) => None,
        };
        let play_allowed_networks = self.play_allowed_networks.filter(|n| !n.is_empty());
        let play_networks_default = play_allowed_networks.is_none();
        Ok(AuthConfig {
            forms: matches!(self.method, Method::Forms | Method::FormsAndOidc),
            oidc,
            session_ttl: self.session_ttl,
            session_max_age: self.session_max_age,
            cookie_secure: self.cookie_secure.unwrap_or(ui_url.scheme() == "https"),
            local_bypass: self.local_bypass,
            play_allowed_networks: play_allowed_networks.unwrap_or_else(default_play_networks),
            play_networks_default,
            trusted_proxies: self.trusted_proxies,
            ui_url,
            setup_code: None,
        })
    }
}

impl AuthConfig {
    /// Uses `code` as the first-run setup code instead of a random one. It must have at least
    /// [`MIN_PRESET_LEN`] letters or digits, not counting spaces and dashes.
    pub fn with_setup_code(mut self, code: &str) -> Result<Self, ConfigError> {
        let normalized = normalize(code);
        if normalized.len() < MIN_PRESET_LEN
            || !normalized.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Err(invalid(
                SETUP_CODE_ENV,
                &format!("must be at least {MIN_PRESET_LEN} letters or digits"),
            ));
        }
        self.setup_code = Some(Secret::new(code.to_owned()));
        Ok(self)
    }

    /// [`Self::with_setup_code`] with `TUBARR_SETUP_CODE`, when it is set.
    pub(crate) fn with_env_setup_code(self) -> Result<Self, ConfigError> {
        match std::env::var(SETUP_CODE_ENV) {
            Ok(code) => self.with_setup_code(&code),
            Err(_) => Ok(self),
        }
    }
}

impl OidcFile {
    fn check(self, ui_url: &Url) -> Result<OidcConfig, ConfigError> {
        let issuer = Url::parse(&self.issuer_url)
            .map_err(|e| invalid("auth.oidc.issuer_url", &e.to_string()))?;
        if !matches!(issuer.scheme(), "http" | "https") || !issuer.has_host() {
            return Err(invalid(
                "auth.oidc.issuer_url",
                "must be an http(s) URL with a host",
            ));
        }
        if self.client_id.trim().is_empty() {
            return Err(invalid("auth.oidc.client_id", "must not be empty"));
        }
        if !self.scopes.iter().any(|s| s == "openid") {
            return Err(invalid("auth.oidc.scopes", "must include openid"));
        }
        if self.groups_claim.trim().is_empty() {
            return Err(invalid("auth.oidc.groups_claim", "must not be empty"));
        }
        let client_secret = read_secret("auth.oidc.client_secret_file", &self.client_secret_file)?;
        let redirect_url = ui_url
            .join(OIDC_CALLBACK_PATH)
            .map_err(|e| invalid("auth.ui_url", &e.to_string()))?;
        let auto_create_users = self
            .auto_create_users
            .unwrap_or(!self.allowed_groups.is_empty());
        Ok(OidcConfig {
            issuer_url: self.issuer_url,
            client_id: self.client_id,
            client_secret,
            scopes: self.scopes,
            allowed_groups: self.allowed_groups,
            groups_claim: self.groups_claim,
            button_label: self.button_label,
            auto_create_users,
            redirect_url,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(toml_text: &str) -> Result<AuthConfig, ConfigError> {
        let file: AuthFile = toml::from_str(toml_text).unwrap();
        file.check(&Url::parse("http://tubarr.lan:8480/").unwrap())
    }

    #[test]
    fn defaults_are_forms_only() {
        let auth = check("").unwrap();
        assert!(auth.forms && auth.oidc.is_none());
        assert!(!auth.cookie_secure && !auth.local_bypass);
        assert_eq!(auth.session_ttl, Duration::from_secs(30 * 24 * 3600));
        assert!(auth.play_networks_default);
        assert_eq!(
            auth.play_allowed_networks.len(),
            DEFAULT_PLAY_NETWORKS.len()
        );
        assert!(auth.trusted_proxies.is_empty());
        let warning = auth.play_networks_warning().unwrap();
        assert!(
            warning.contains("192.168.0.0/16") && warning.contains("0.0.0.0/0"),
            "{warning}"
        );
        let empty = check("play_allowed_networks = []").unwrap();
        assert!(empty.play_networks_default && empty.play_networks_warning().is_some());
    }

    #[test]
    fn oidc_builds_redirect_from_ui_url() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("secret");
        std::fs::write(&secret, "s3cret\n").unwrap();
        let auth = check(&format!(
            "method = \"forms+oidc\"\nui_url = \"https://tubarr.example.dev\"\n\
             [oidc]\nissuer_url = \"https://auth.example.dev/application/o/tubarr/\"\n\
             client_id = \"tubarr\"\nclient_secret_file = {secret:?}\nallowed_groups = [\"admins\"]\n"
        ))
        .unwrap();
        assert!(auth.forms && auth.cookie_secure);
        let oidc = auth.oidc.unwrap();
        assert_eq!(
            oidc.redirect_url.as_str(),
            "https://tubarr.example.dev/auth/oidc/callback"
        );
        assert!(oidc.auto_create_users);
        assert_eq!(oidc.groups_claim, "groups");
        assert_eq!(oidc.client_secret.expose(), "s3cret");
    }

    #[test]
    fn oidc_method_needs_its_section() {
        let err = check("method = \"oidc\"").unwrap_err().to_string();
        assert!(err.contains("auth.oidc"), "{err}");
        assert!(toml::from_str::<AuthFile>("method = \"password\"").is_err());
        let err = check("session_ttl = \"1m\"").unwrap_err().to_string();
        assert!(err.contains("auth.session_ttl"), "{err}");
    }

    #[test]
    fn networks_parse_as_cidr() {
        let auth = check("play_allowed_networks = [\"198.51.100.0/24\", \"fd00::/8\"]").unwrap();
        assert_eq!(auth.play_allowed_networks.len(), 2);
        assert!(!auth.play_networks_default && auth.play_networks_warning().is_none());
        let proxies = check("trusted_proxies = [\"172.16.0.5/32\"]").unwrap();
        assert_eq!(proxies.trusted_proxies.len(), 1);
        assert!(toml::from_str::<AuthFile>("play_allowed_networks = [\"nope\"]").is_err());
    }
}
