//! OpenID Connect sign-in end to end against a small fake provider: discovery, keys, a token
//! endpoint that checks PKCE and signs ID tokens with a test-only RSA key.

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Form, Json, Router,
    body::Body,
    extract::{ConnectInfo, State},
    http::{HeaderMap, Request, StatusCode, header},
    routing::{get, post},
};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use openidconnect::{
    AdditionalClaims, Audience, EndUserUsername, IdToken, IdTokenClaims, IssuerUrl, JsonWebKeyId,
    Nonce, PkceCodeChallenge, PkceCodeVerifier, PrivateSigningKey, StandardClaims,
    SubjectIdentifier,
    core::{
        CoreGenderClaim, CoreJsonWebKeySet, CoreJweContentEncryptionAlgorithm,
        CoreJwsSigningAlgorithm, CoreRsaPrivateSigningKey,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tower::ServiceExt;
use tubarr::{AppState, binaries::BinaryManager, config::Config, store::Db};

const CLIENT_ID: &str = "tubarr";
const TEST_KEY: &str = include_str!("fixtures/oidc/test-only-rsa.pem");

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Groups {
    groups: Vec<String>,
}

impl AdditionalClaims for Groups {}

/// What the fake signs into the ID token for one authorization code.
#[derive(Clone)]
struct Grant {
    subject: String,
    username: String,
    groups: Vec<String>,
    nonce: String,
    challenge: String,
}

#[derive(Clone)]
struct Provider {
    issuer: String,
    grants: Arc<Mutex<HashMap<String, Grant>>>,
}

fn signing_key() -> CoreRsaPrivateSigningKey {
    CoreRsaPrivateSigningKey::from_pem(TEST_KEY, Some(JsonWebKeyId::new("k1".to_owned()))).unwrap()
}

async fn discovery(State(p): State<Provider>) -> Json<Value> {
    Json(json!({
        "issuer": p.issuer,
        "authorization_endpoint": format!("{}/authorize", p.issuer),
        "token_endpoint": format!("{}/token", p.issuer),
        "jwks_uri": format!("{}/jwks", p.issuer),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
}

async fn jwks() -> Json<CoreJsonWebKeySet> {
    Json(CoreJsonWebKeySet::new(vec![
        signing_key().as_verification_key(),
    ]))
}

async fn token(
    State(p): State<Provider>,
    Form(form): Form<HashMap<String, String>>,
) -> (StatusCode, Json<Value>) {
    let refuse = |e: &str| (StatusCode::BAD_REQUEST, Json(json!({"error": e})));
    let Some(grant) = p.grants.lock().unwrap().remove(&form["code"]) else {
        return refuse("invalid_grant");
    };
    let verifier = PkceCodeVerifier::new(form.get("code_verifier").cloned().unwrap_or_default());
    if PkceCodeChallenge::from_code_verifier_sha256(&verifier).as_str() != grant.challenge {
        return refuse("invalid_grant");
    }
    let claims = IdTokenClaims::new(
        IssuerUrl::new(p.issuer.clone()).unwrap(),
        vec![Audience::new(CLIENT_ID.to_owned())],
        Utc::now() + Duration::seconds(300),
        Utc::now(),
        StandardClaims::new(SubjectIdentifier::new(grant.subject))
            .set_preferred_username(Some(EndUserUsername::new(grant.username))),
        Groups {
            groups: grant.groups,
        },
    )
    .set_nonce(Some(Nonce::new(grant.nonce)));
    let id_token = IdToken::<
        Groups,
        CoreGenderClaim,
        CoreJweContentEncryptionAlgorithm,
        CoreJwsSigningAlgorithm,
    >::new(
        claims,
        &signing_key(),
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
        None,
        None,
    )
    .unwrap();
    (
        StatusCode::OK,
        Json(json!({
            "access_token": "fake-access-token",
            "token_type": "Bearer",
            "expires_in": 300,
            "id_token": id_token.to_string(),
        })),
    )
}

async fn provider() -> Provider {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = Provider {
        issuer: format!("http://{}", listener.local_addr().unwrap()),
        grants: Arc::default(),
    };
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(jwks))
        .route("/token", post(token))
        .with_state(provider.clone());
    tokio::spawn(async move { axum::serve(listener, app).await });
    provider
}

struct Kit {
    app: axum::Router,
    provider: Provider,
    _dir: tempfile::TempDir,
}

async fn kit() -> Kit {
    let provider = provider().await;
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("oidc-secret");
    std::fs::write(&secret, "client-secret").unwrap();
    let toml = format!(
        "public_url = \"http://localhost:8480\"\ndata_dir = {:?}\n\
         [auth]\nmethod = \"forms+oidc\"\n\
         [auth.oidc]\nissuer_url = \"{}\"\nclient_id = \"{CLIENT_ID}\"\n\
         client_secret_file = {secret:?}\nallowed_groups = [\"admins\"]\n",
        dir.path(),
        provider.issuer
    );
    let config = Config::from_toml(&toml).unwrap();
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let app = tubarr::app(AppState::new(config, db, binaries).unwrap());
    Kit {
        app,
        provider,
        _dir: dir,
    }
}

async fn get_path(
    app: &axum::Router,
    path: &str,
    cookie: Option<&str>,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder().uri(path);
    if let Some(cookie) = cookie {
        request = request.header("cookie", cookie);
    }
    let mut request = request.body(Body::empty()).unwrap();
    let addr: SocketAddr = "203.0.113.7:5000".parse().unwrap();
    request.extensions_mut().insert(ConnectInfo(addr));
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn cookie_named(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|c| c.split(';').next())
        .find(|c| c.starts_with(&format!("{name}=")) && !c.ends_with('='))
        .map(str::to_owned)
}

/// Runs the browser's part: start, then the provider "approves" with `groups` and comes back.
/// `nonce` overrides the nonce the provider signs. Returns the callback reply.
async fn sign_in(
    k: &Kit,
    subject: &str,
    groups: &[&str],
    nonce: Option<&str>,
    send_state_cookie: bool,
) -> (StatusCode, HeaderMap) {
    let (status, headers, _) = get_path(&k.app, "/auth/oidc/start?next=/mappings", None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let location = url::Url::parse(headers[header::LOCATION].to_str().unwrap()).unwrap();
    assert!(
        location
            .as_str()
            .starts_with(&format!("{}/authorize", k.provider.issuer))
    );
    let query: HashMap<String, String> = location.query_pairs().into_owned().collect();
    assert_eq!(query["code_challenge_method"], "S256");
    assert_eq!(
        query["redirect_uri"],
        "http://localhost:8480/auth/oidc/callback"
    );
    let scopes: Vec<&str> = query["scope"].split(' ').collect();
    assert!(scopes.contains(&"groups"));
    assert_eq!(scopes.iter().filter(|s| **s == "openid").count(), 1);
    let state_cookie = cookie_named(&headers, "tubarr_oidc").unwrap();

    let code = format!("code-{subject}-{}", query["state"]);
    k.provider.grants.lock().unwrap().insert(
        code.clone(),
        Grant {
            subject: subject.to_owned(),
            username: "hani".to_owned(),
            groups: groups.iter().map(|g| (*g).to_owned()).collect(),
            nonce: nonce.map_or_else(|| query["nonce"].clone(), str::to_owned),
            challenge: query["code_challenge"].clone(),
        },
    );
    let path = format!("/auth/oidc/callback?code={code}&state={}", query["state"]);
    let (status, headers, _) = get_path(
        &k.app,
        &path,
        send_state_cookie.then_some(state_cookie.as_str()),
    )
    .await;
    (status, headers)
}

fn location(headers: &HeaderMap) -> &str {
    headers[header::LOCATION].to_str().unwrap()
}

#[tokio::test]
async fn allowed_group_signs_in_and_links_the_account() {
    let k = kit().await;
    let (_, _, state) = get_path(&k.app, "/auth/state", None).await;
    assert_eq!(state["methods"]["oidc"], true);
    assert_eq!(state["oidc_button_label"], "Sign in with SSO");

    let (status, headers) = sign_in(&k, "sub-1", &["users", "admins"], None, true).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&headers), "/mappings");
    let session = cookie_named(&headers, "tubarr_session").expect("signed in");
    let (_, _, state) = get_path(&k.app, "/auth/state", Some(&session)).await;
    assert_eq!(state["user"]["username"], "hani");
    assert_eq!(state["user"]["oidc_subject"], "sub-1");
    let (status, _, _) = get_path(&k.app, "/admin/api/mappings", Some(&session)).await;
    assert_eq!(status, StatusCode::OK);

    // The same subject signs in to the same account. A new subject with the same preferred
    // username gets a new account, never the existing one.
    let (_, headers) = sign_in(&k, "sub-1", &["admins"], None, true).await;
    let again = cookie_named(&headers, "tubarr_session").unwrap();
    let (_, _, state) = get_path(&k.app, "/auth/state", Some(&again)).await;
    assert_eq!(state["user"]["username"], "hani");
    let (_, headers) = sign_in(&k, "sub-2", &["admins"], None, true).await;
    let other = cookie_named(&headers, "tubarr_session").unwrap();
    let (_, _, state) = get_path(&k.app, "/auth/state", Some(&other)).await;
    assert_eq!(state["user"]["username"], "hani-2");
}

#[tokio::test]
async fn wrong_group_is_refused() {
    let k = kit().await;
    let (status, headers) = sign_in(&k, "sub-1", &["users"], None, true).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(location(&headers), "/login?error=not_allowed");
    assert!(cookie_named(&headers, "tubarr_session").is_none());
}

#[tokio::test]
async fn nonce_mismatch_is_refused() {
    let k = kit().await;
    let (_, headers) = sign_in(&k, "sub-1", &["admins"], Some("someone-elses-nonce"), true).await;
    assert_eq!(location(&headers), "/login?error=failed");
    assert!(cookie_named(&headers, "tubarr_session").is_none());
}

#[tokio::test]
async fn callback_needs_the_browser_that_started_it() {
    let k = kit().await;
    let (_, headers) = sign_in(&k, "sub-1", &["admins"], None, false).await;
    assert_eq!(location(&headers), "/login?error=expired");
    assert!(cookie_named(&headers, "tubarr_session").is_none());

    // A made-up state is refused too.
    let (_, headers, _) = get_path(
        &k.app,
        "/auth/oidc/callback?code=x&state=made-up",
        Some("tubarr_oidc=made-up"),
    )
    .await;
    assert_eq!(location(&headers), "/login?error=expired");
}

#[tokio::test]
async fn next_cannot_leave_the_site() {
    let k = kit().await;
    for next in ["//evil.example", "https://evil.example", "/\\evil.example"] {
        let path = format!("/auth/oidc/start?next={}", urlencode(next));
        let (status, headers, _) = get_path(&k.app, &path, None).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let location = url::Url::parse(headers[header::LOCATION].to_str().unwrap()).unwrap();
        let query: HashMap<String, String> = location.query_pairs().into_owned().collect();
        let state_cookie = cookie_named(&headers, "tubarr_oidc").unwrap();
        let code = format!("c-{}", query["state"]);
        k.provider.grants.lock().unwrap().insert(
            code.clone(),
            Grant {
                subject: "sub-1".to_owned(),
                username: "hani".to_owned(),
                groups: vec!["admins".to_owned()],
                nonce: query["nonce"].clone(),
                challenge: query["code_challenge"].clone(),
            },
        );
        let callback = format!("/auth/oidc/callback?code={code}&state={}", query["state"]);
        let (_, headers, _) = get_path(&k.app, &callback, Some(&state_cookie)).await;
        assert_eq!(headers[header::LOCATION], "/", "{next}");
    }
}

fn urlencode(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}
