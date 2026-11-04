//! OpenID Connect sign-in: authorization code flow with PKCE, a nonce and a state, checked
//! against the provider's discovery document and keys.

use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::Mutex,
    time::{Duration, Instant},
};

use openidconnect::{
    AdditionalClaims, AsyncHttpClient, AuthorizationCode, Client, ClientId, ClientSecret,
    CsrfToken, EmptyExtraTokenFields, EndpointMaybeSet, EndpointNotSet, EndpointSet, HttpRequest,
    HttpResponse, IdTokenFields, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, StandardErrorResponse, StandardTokenResponse, TokenResponse,
    core::{
        CoreAuthDisplay, CoreAuthPrompt, CoreAuthenticationFlow, CoreErrorResponseType,
        CoreGenderClaim, CoreJsonWebKey, CoreJweContentEncryptionAlgorithm,
        CoreJwsSigningAlgorithm, CoreProviderMetadata, CoreRevocableToken,
        CoreRevocationErrorResponse, CoreTokenIntrospectionResponse, CoreTokenType,
    },
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex as AsyncMutex;

use super::settings::OidcConfig;

/// How long a started sign-in may take to come back.
pub const PENDING_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_PENDING: usize = 1000;
/// Discovery (and with it the signing keys) is fetched again after this long.
const DISCOVERY_TTL: Duration = Duration::from_secs(60 * 60);

/// Every claim of the ID token, so the configured groups claim can be read whatever its name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtraClaims {
    #[serde(flatten)]
    pub claims: serde_json::Map<String, serde_json::Value>,
}

impl AdditionalClaims for ExtraClaims {}

type TokenFields = IdTokenFields<
    ExtraClaims,
    EmptyExtraTokenFields,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJwsSigningAlgorithm,
>;
type TokenReply = StandardTokenResponse<TokenFields, CoreTokenType>;

type OidcClient = Client<
    ExtraClaims,
    CoreAuthDisplay,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJsonWebKey,
    CoreAuthPrompt,
    StandardErrorResponse<CoreErrorResponseType>,
    TokenReply,
    CoreTokenIntrospectionResponse,
    CoreRevocableToken,
    CoreRevocationErrorResponse,
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    #[error("OpenID Connect discovery failed: {0}")]
    Discovery(String),
    #[error("the sign-in expired or did not start here")]
    UnknownState,
    #[error("the provider refused the code exchange: {0}")]
    Exchange(String),
    #[error("the provider sent no ID token")]
    NoIdToken,
    #[error("the ID token did not verify: {0}")]
    Claims(String),
}

/// What the provider vouched for.
#[derive(Debug, Clone)]
pub struct Identity {
    pub issuer: String,
    pub subject: String,
    pub preferred_username: Option<String>,
    pub email: Option<String>,
    pub name: Option<String>,
    pub groups: Vec<String>,
}

struct Pending {
    nonce: Nonce,
    verifier: PkceCodeVerifier,
    next: String,
    created: Instant,
}

/// The HTTP client the flow uses. It never follows redirects, as the openidconnect docs require.
struct Http(reqwest::Client);

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct HttpError(String);

impl<'c> AsyncHttpClient<'c> for Http {
    type Error = HttpError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + 'c>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            let request =
                reqwest::Request::try_from(request).map_err(|e| HttpError(e.to_string()))?;
            let response = self
                .0
                .execute(request)
                .await
                .map_err(|e| HttpError(e.to_string()))?;
            let mut builder = openidconnect::http::Response::builder().status(response.status());
            for (name, value) in response.headers() {
                builder = builder.header(name, value);
            }
            let body = response
                .bytes()
                .await
                .map_err(|e| HttpError(e.to_string()))?;
            builder
                .body(body.to_vec())
                .map_err(|e| HttpError(e.to_string()))
        })
    }
}

pub struct Oidc {
    config: OidcConfig,
    http: Http,
    client: AsyncMutex<Option<(Instant, OidcClient)>>,
    pending: Mutex<HashMap<String, Pending>>,
}

impl std::fmt::Debug for Oidc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Oidc")
            .field("issuer", &self.config.issuer_url)
            .finish_non_exhaustive()
    }
}

impl Oidc {
    pub fn new(config: OidcConfig) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            config,
            http: Http(http),
            client: AsyncMutex::new(None),
            pending: Mutex::new(HashMap::new()),
        })
    }

    pub fn config(&self) -> &OidcConfig {
        &self.config
    }

    /// The client for the provider, discovered on first use and again once an hour, so a provider
    /// that is down at start does not stop Tubarr.
    async fn client(&self) -> Result<OidcClient, OidcError> {
        let mut cached = self.client.lock().await;
        if let Some((at, client)) = cached.as_ref()
            && at.elapsed() < DISCOVERY_TTL
        {
            return Ok(client.clone());
        }
        let issuer = IssuerUrl::new(self.config.issuer_url.clone())
            .map_err(|e| OidcError::Discovery(e.to_string()))?;
        let metadata = CoreProviderMetadata::discover_async(issuer, &self.http)
            .await
            .map_err(|e| OidcError::Discovery(error_chain(&e)))?;
        let redirect = RedirectUrl::from_url(self.config.redirect_url.clone());
        let client = OidcClient::from_provider_metadata(
            metadata,
            ClientId::new(self.config.client_id.clone()),
            Some(ClientSecret::new(
                self.config.client_secret.expose().to_owned(),
            )),
        )
        .set_redirect_uri(redirect);
        *cached = Some((Instant::now(), client.clone()));
        Ok(client)
    }

    /// The provider URL to send the browser to, and the state that must come back with it.
    /// `next` is where to land afterwards, already checked by the caller.
    pub async fn start(&self, next: String) -> Result<(String, String), OidcError> {
        let client = self.client().await?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request = client.authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        );
        // The crate always sends `openid` itself, so adding the configured one would repeat it.
        for scope in self.config.scopes.iter().filter(|s| *s != "openid") {
            request = request.add_scope(Scope::new(scope.clone()));
        }
        let (url, state, nonce) = request.set_pkce_challenge(challenge).url();
        let mut pending = self.lock_pending();
        pending.retain(|_, p| p.created.elapsed() < PENDING_TTL);
        if pending.len() >= MAX_PENDING
            && let Some(oldest) = pending
                .iter()
                .min_by_key(|(_, p)| p.created)
                .map(|(k, _)| k.clone())
        {
            pending.remove(&oldest);
        }
        pending.insert(
            state.secret().clone(),
            Pending {
                nonce,
                verifier,
                next,
                created: Instant::now(),
            },
        );
        Ok((url.to_string(), state.secret().clone()))
    }

    /// Checks a callback: the state must be one this server started (taken, so it works once),
    /// the code is exchanged with the PKCE verifier, and the ID token must verify with the nonce.
    /// Returns the identity and where to land.
    pub async fn finish(&self, state: &str, code: &str) -> Result<(Identity, String), OidcError> {
        let pending = self
            .lock_pending()
            .remove(state)
            .filter(|p| p.created.elapsed() < PENDING_TTL)
            .ok_or(OidcError::UnknownState)?;
        let client = self.client().await?;
        let reply = client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .map_err(|e| OidcError::Exchange(e.to_string()))?
            .set_pkce_verifier(pending.verifier)
            .request_async(&self.http)
            .await
            .map_err(|e| OidcError::Exchange(error_chain(&e)))?;
        let id_token = reply.id_token().ok_or(OidcError::NoIdToken)?;
        let claims = id_token
            .claims(&client.id_token_verifier(), &pending.nonce)
            .map_err(|e| OidcError::Claims(e.to_string()))?;
        let extra = &claims.additional_claims().claims;
        let groups = match extra.get(&self.config.groups_claim) {
            Some(serde_json::Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            Some(serde_json::Value::String(one)) => vec![one.clone()],
            _ => Vec::new(),
        };
        let identity = Identity {
            issuer: claims.issuer().as_str().to_owned(),
            subject: claims.subject().as_str().to_owned(),
            preferred_username: claims.preferred_username().map(|u| u.as_str().to_owned()),
            email: claims.email().map(|e| e.as_str().to_owned()),
            name: claims
                .name()
                .and_then(|n| n.get(None))
                .map(|n| n.as_str().to_owned()),
            groups,
        };
        Ok((identity, pending.next))
    }

    /// Whether `groups` passes `allowed_groups` (always, when none are configured).
    pub fn group_allowed(&self, groups: &[String]) -> bool {
        self.config.allowed_groups.is_empty()
            || groups
                .iter()
                .any(|g| self.config.allowed_groups.contains(g))
    }

    fn lock_pending(&self) -> std::sync::MutexGuard<'_, HashMap<String, Pending>> {
        self.pending.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}
