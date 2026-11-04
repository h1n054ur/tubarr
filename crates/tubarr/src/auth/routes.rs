//! `/auth/*` (sign-in, used by the UI's login page) and `/admin/api/users`.
//!
//! The OpenID Connect callback is a browser navigation, so it answers with redirects: to the
//! `next` path on success, else to `/login?error=<code>` with `expired`, `not_allowed` or
//! `failed`.

use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Redirect, Response},
    routing::{delete, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    AuthError, Principal, authenticate, authorize, csrf_ok,
    oidc::Identity,
    password, session,
    store::{LastAdmin, NewUser, User},
};
use crate::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/state", get(auth_state))
        .route("/auth/setup", post(setup))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/password", post(change_password))
        .route("/auth/oidc/start", get(oidc_start))
        .route("/auth/oidc/callback", get(oidc_callback))
        .route("/admin/api/users", get(list_users).post(create_user))
        .route("/admin/api/users/{id}", delete(delete_user))
}

fn require_csrf(headers: &HeaderMap) -> Result<(), AuthError> {
    if csrf_ok(headers) {
        Ok(())
    } else {
        Err(AuthError::Csrf)
    }
}

/// Starts a session for `user`: a fresh token every time, so a sign-in never reuses an id the
/// browser already had. The old session, if any, is ended.
async fn start_session(
    state: &AppState,
    headers: &HeaderMap,
    user: &User,
) -> Result<HeaderValue, AuthError> {
    let auth = &state.auth;
    if let Some(old) = session::cookie(headers, session::SESSION_COOKIE) {
        auth.store.delete_session(&session::token_hash(old)).await?;
    }
    auth.store.prune_sessions().await?;
    let token = session::new_token();
    let max_age = auth.config.session_max_age;
    auth.store
        .create_session(
            &session::token_hash(&token),
            user.id,
            i64::try_from(max_age.as_secs()).unwrap_or(i64::MAX),
        )
        .await?;
    auth.store.touch_login(user.id).await?;
    Ok(session::set_cookie(
        session::SESSION_COOKIE,
        &token,
        "/",
        max_age,
        auth.config.cookie_secure,
    ))
}

fn signed_in(cookie: HeaderValue, user: &User) -> Response {
    let mut response = Json(json!({ "user": user })).into_response();
    response.headers_mut().insert(header::SET_COOKIE, cookie);
    response
}

async fn auth_state(State(state): State<AppState>, parts: Parts) -> Result<Response, AuthError> {
    let auth = &state.auth;
    let principal = authenticate(&state, &parts.headers, &parts.extensions)
        .await
        .ok();
    let users = auth.store.user_count().await?;
    Ok(Json(json!({
        "setup_required": auth.config.forms && users == 0,
        "methods": {
            "password": auth.config.forms,
            "oidc": auth.oidc.is_some(),
        },
        "oidc_button_label": auth.oidc.as_ref().map(|o| o.config().button_label.clone()),
        "user": principal.as_ref().and_then(Principal::user),
        "via": principal.as_ref().map(|p| match p {
            Principal::ApiKey => "api_key",
            Principal::LocalBypass => "local_bypass",
            Principal::Session { .. } => "session",
        }),
    }))
    .into_response())
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Deserialize)]
struct SetupBody {
    #[serde(flatten)]
    credentials: Credentials,
    #[serde(default)]
    setup_code: String,
}

fn checked_new_user(username: &str, password: &str) -> Result<(), AuthError> {
    if !password::valid_username(username) {
        return Err(AuthError::Invalid(format!(
            "username must be 1 to {} characters with no spaces",
            password::MAX_USERNAME
        )));
    }
    if !password::valid_password(password) {
        return Err(AuthError::Invalid(format!(
            "password must be {} to {} characters",
            password::MIN_PASSWORD,
            password::MAX_PASSWORD
        )));
    }
    Ok(())
}

fn clean_display_name(name: Option<String>) -> Option<String> {
    name.map(|n| n.trim().chars().take(128).collect::<String>())
        .filter(|n| !n.is_empty())
}

/// Creates the first admin and signs them in, given the setup code from the log. Refused once
/// any user exists.
async fn setup(
    State(state): State<AppState>,
    parts: Parts,
    Json(SetupBody {
        credentials: body,
        setup_code,
    }): Json<SetupBody>,
) -> Result<Response, AuthError> {
    let headers = &parts.headers;
    require_csrf(headers)?;
    let auth = &state.auth;
    if !auth.config.forms {
        return Err(AuthError::Off("password sign-in"));
    }
    if auth.store.user_count().await? > 0 {
        return Err(AuthError::Conflict("setup is already done"));
    }
    let ip = auth.throttle_ip(&parts.extensions, headers);
    if let Some(wait) = auth.setup_limiter.check_ip(ip) {
        return Err(AuthError::Throttled(wait.as_secs().max(1)));
    }
    if !auth.setup_code.matches(&setup_code) {
        auth.setup_limiter.failed_ip(ip);
        tracing::warn!(ip = ?ip, "setup refused: wrong or missing setup code");
        return Err(AuthError::BadSetupCode);
    }
    auth.setup_limiter.succeeded_ip(ip);
    let username = body.username.trim();
    checked_new_user(username, &body.password)?;
    let hash = password::hash(body.password)
        .await
        .map_err(|_| AuthError::Internal)?;
    let user = state
        .auth
        .store
        .create_first_user(&NewUser {
            username: username.to_owned(),
            display_name: clean_display_name(body.display_name),
            password_hash: Some(hash),
            oidc: None,
        })
        .await?
        .ok_or(AuthError::Conflict("setup is already done"))?;
    auth.setup_code.clear();
    tracing::info!(username = %user.username, "first admin created, setup code dropped");
    let cookie = start_session(&state, headers, &user).await?;
    Ok(signed_in(cookie, &user))
}

async fn login(
    State(state): State<AppState>,
    parts: Parts,
    Json(body): Json<Credentials>,
) -> Result<Response, AuthError> {
    require_csrf(&parts.headers)?;
    let auth = &state.auth;
    if !auth.config.forms {
        return Err(AuthError::Off("password sign-in"));
    }
    let ip = auth.throttle_ip(&parts.extensions, &parts.headers);
    let username = body.username.trim();
    // Out-of-range input fails like a wrong password, without spending a hash on it.
    if !password::valid_username(username) || body.password.len() > password::MAX_PASSWORD * 4 {
        return Err(AuthError::BadCredentials);
    }
    if let Some(wait) = auth.limiter.check(ip, username) {
        return Err(AuthError::Throttled(wait.as_secs().max(1)));
    }
    // Many failures for this name from many addresses: slowed, never refused.
    tokio::time::sleep(auth.limiter.delay(username)).await;
    let user = auth.store.user_by_name(username).await?;
    let stored = user.as_ref().and_then(|u| u.password_hash.clone());
    let matched = password::verify(body.password, stored).await;
    match user.filter(|u| matched && !u.disabled) {
        Some(user) => {
            auth.limiter.succeeded(ip, username);
            tracing::info!(username = %user.username, "signed in with a password");
            let cookie = start_session(&state, &parts.headers, &user).await?;
            Ok(signed_in(cookie, &user))
        }
        None => {
            auth.limiter.failed(ip, username);
            tracing::warn!(ip = ?ip, "failed sign-in");
            Err(AuthError::BadCredentials)
        }
    }
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, AuthError> {
    require_csrf(&headers)?;
    if let Some(token) = session::cookie(&headers, session::SESSION_COOKIE) {
        state
            .auth
            .store
            .delete_session(&session::token_hash(token))
            .await?;
    }
    let clear = session::set_cookie(
        session::SESSION_COOKIE,
        "",
        "/",
        Duration::ZERO,
        state.auth.config.cookie_secure,
    );
    Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, clear)]).into_response())
}

#[derive(Deserialize)]
struct PasswordChange {
    current_password: String,
    new_password: String,
}

/// Changes the signed-in user's own password and ends their other sessions.
async fn change_password(
    State(state): State<AppState>,
    parts: Parts,
    Json(body): Json<PasswordChange>,
) -> Result<Response, AuthError> {
    let principal = authorize(&state, &Method::POST, &parts.headers, &parts.extensions).await?;
    require_csrf(&parts.headers)?;
    let Principal::Session { user, token_hash } = principal else {
        return Err(AuthError::Invalid(
            "only a signed-in user can change their password".to_owned(),
        ));
    };
    if !user.has_password() {
        return Err(AuthError::Invalid(
            "this account signs in through SSO and has no password".to_owned(),
        ));
    }
    checked_new_user(&user.username, &body.new_password)?;
    let ip = state.auth.throttle_ip(&parts.extensions, &parts.headers);
    if let Some(wait) = state.auth.limiter.check(ip, &user.username) {
        return Err(AuthError::Throttled(wait.as_secs().max(1)));
    }
    if !password::verify(body.current_password, user.password_hash.clone()).await {
        state.auth.limiter.failed(ip, &user.username);
        return Err(AuthError::BadCredentials);
    }
    let hash = password::hash(body.new_password)
        .await
        .map_err(|_| AuthError::Internal)?;
    state.auth.store.set_password(user.id, &hash).await?;
    state
        .auth
        .store
        .delete_other_sessions(user.id, &token_hash)
        .await?;
    tracing::info!(username = %user.username, "password changed");
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Only same-site paths: `/x`, never `//host`, `/\host` or a full URL.
fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(n)
            if n.starts_with('/')
                && n.is_ascii()
                && !n.starts_with("//")
                && !n.contains('\\')
                && !n.chars().any(char::is_control) =>
        {
            n.to_owned()
        }
        _ => "/".to_owned(),
    }
}

#[derive(Deserialize)]
struct StartQuery {
    next: Option<String>,
}

async fn oidc_start(
    State(state): State<AppState>,
    Query(query): Query<StartQuery>,
) -> Result<Response, AuthError> {
    let oidc = state.auth.oidc.as_ref().ok_or(AuthError::Off("SSO"))?;
    let (url, csrf_state) = oidc
        .start(safe_next(query.next.as_deref()))
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "cannot start SSO sign-in");
            AuthError::Internal
        })?;
    let cookie = session::set_cookie(
        session::OIDC_COOKIE,
        &csrf_state,
        session::OIDC_COOKIE_PATH,
        super::oidc::PENDING_TTL,
        state.auth.config.cookie_secure,
    );
    Ok((
        StatusCode::SEE_OTHER,
        [
            (
                header::LOCATION,
                HeaderValue::from_str(&url).map_err(|_| AuthError::Internal)?,
            ),
            (header::SET_COOKIE, cookie),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
    )
        .into_response())
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

async fn oidc_callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let fail = |code: &str| {
        let clear = session::set_cookie(
            session::OIDC_COOKIE,
            "",
            session::OIDC_COOKIE_PATH,
            Duration::ZERO,
            state.auth.config.cookie_secure,
        );
        (
            [(header::SET_COOKIE, clear)],
            Redirect::to(&format!("/login?error={code}")),
        )
            .into_response()
    };
    let Some(oidc) = state.auth.oidc.as_ref() else {
        return fail("failed");
    };
    if let Some(error) = &query.error {
        tracing::warn!(error, "SSO provider refused the sign-in");
        return fail("failed");
    }
    let (Some(code), Some(csrf_state)) = (&query.code, &query.state) else {
        return fail("failed");
    };
    // The state must also match the cookie set by /auth/oidc/start in this browser, so a callback
    // link from someone else cannot sign this browser in to their account.
    let cookie_state = session::cookie(&headers, session::OIDC_COOKIE).unwrap_or_default();
    if !crate::api::constant_time_eq(cookie_state.as_bytes(), csrf_state.as_bytes()) {
        return fail("expired");
    }
    let (identity, next) = match oidc.finish(csrf_state, code).await {
        Ok(found) => found,
        Err(super::oidc::OidcError::UnknownState) => return fail("expired"),
        Err(e) => {
            tracing::warn!(error = %e, "SSO sign-in failed");
            return fail("failed");
        }
    };
    if !oidc.group_allowed(&identity.groups) {
        tracing::warn!(subject = %identity.subject, "SSO user is in none of the allowed groups");
        return fail("not_allowed");
    }
    let user = match oidc_user(&state, &identity).await {
        Ok(Some(user)) => user,
        Ok(None) => {
            tracing::warn!(subject = %identity.subject, "SSO user has no Tubarr account");
            return fail("not_allowed");
        }
        Err(_) => return fail("failed"),
    };
    let cookie = match start_session(&state, &headers, &user).await {
        Ok(cookie) => cookie,
        Err(_) => return fail("failed"),
    };
    tracing::info!(username = %user.username, "signed in with SSO");
    let clear = session::set_cookie(
        session::OIDC_COOKIE,
        "",
        session::OIDC_COOKIE_PATH,
        Duration::ZERO,
        state.auth.config.cookie_secure,
    );
    let mut response = Redirect::to(&next).into_response();
    response.headers_mut().append(header::SET_COOKIE, cookie);
    response.headers_mut().append(header::SET_COOKIE, clear);
    response
}

/// The account for an identity: linked by issuer and subject, else created when allowed.
async fn oidc_user(state: &AppState, identity: &Identity) -> Result<Option<User>, AuthError> {
    let store = &state.auth.store;
    if let Some(user) = store
        .user_by_oidc(&identity.issuer, &identity.subject)
        .await?
    {
        return Ok((!user.disabled).then_some(user));
    }
    let auto_create = state
        .auth
        .oidc
        .as_ref()
        .is_some_and(|o| o.config().auto_create_users);
    if !auto_create {
        return Ok(None);
    }
    let base = identity
        .preferred_username
        .clone()
        .or_else(|| {
            identity
                .email
                .as_deref()
                .and_then(|e| e.split('@').next())
                .map(str::to_owned)
        })
        .filter(|n| password::valid_username(n))
        .unwrap_or_else(|| "user".to_owned());
    // Never link to an existing password account by name: that would let the provider take it.
    for attempt in 1..=20 {
        let username = if attempt == 1 {
            base.clone()
        } else {
            format!("{base}-{attempt}")
        };
        let created = store
            .create_user(&NewUser {
                username,
                display_name: clean_display_name(identity.name.clone()),
                password_hash: None,
                oidc: Some((identity.issuer.clone(), identity.subject.clone())),
            })
            .await;
        match created {
            Ok(user) => {
                tracing::info!(username = %user.username, "created a user from SSO");
                return Ok(Some(user));
            }
            Err(crate::store::StoreError::Duplicate(_)) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(AuthError::Conflict("no free username for this SSO user"))
}

/// A user as the admin API shows it: whether a password is set, never the hash.
#[derive(Serialize)]
struct UserView {
    #[serde(flatten)]
    user: User,
    has_password: bool,
}

impl From<User> for UserView {
    fn from(user: User) -> Self {
        let has_password = user.has_password();
        Self { user, has_password }
    }
}

async fn list_users(
    State(state): State<AppState>,
    parts: Parts,
) -> Result<Json<Vec<UserView>>, AuthError> {
    authorize(&state, &parts.method, &parts.headers, &parts.extensions).await?;
    let users = state.auth.store.list_users().await?;
    Ok(Json(users.into_iter().map(UserView::from).collect()))
}

#[derive(Deserialize)]
struct NewUserBody {
    username: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    /// Links the user to this subject at the configured provider.
    #[serde(default)]
    oidc_subject: Option<String>,
}

async fn create_user(
    State(state): State<AppState>,
    parts: Parts,
    Json(body): Json<NewUserBody>,
) -> Result<Response, AuthError> {
    authorize(&state, &parts.method, &parts.headers, &parts.extensions).await?;
    let username = body.username.trim().to_owned();
    if !password::valid_username(&username) {
        return Err(AuthError::Invalid(format!(
            "username must be 1 to {} characters with no spaces",
            password::MAX_USERNAME
        )));
    }
    let oidc = match body.oidc_subject.filter(|s| !s.trim().is_empty()) {
        Some(subject) => {
            let issuer = state
                .auth
                .oidc
                .as_ref()
                .ok_or(AuthError::Off("SSO"))?
                .config()
                .issuer_url
                .clone();
            Some((issuer, subject.trim().to_owned()))
        }
        None => None,
    };
    let password_hash = match body.password {
        Some(password) => {
            checked_new_user(&username, &password)?;
            Some(
                password::hash(password)
                    .await
                    .map_err(|_| AuthError::Internal)?,
            )
        }
        None if oidc.is_some() => None,
        None => {
            return Err(AuthError::Invalid(
                "a user needs a password or an oidc_subject".to_owned(),
            ));
        }
    };
    let user = state
        .auth
        .store
        .create_user(&NewUser {
            username,
            display_name: clean_display_name(body.display_name),
            password_hash,
            oidc,
        })
        .await?;
    tracing::info!(username = %user.username, "user created");
    Ok((StatusCode::CREATED, Json(UserView::from(user))).into_response())
}

async fn delete_user(
    State(state): State<AppState>,
    Path(id): Path<i64>,
    parts: Parts,
) -> Result<Response, AuthError> {
    authorize(&state, &parts.method, &parts.headers, &parts.extensions).await?;
    match state.auth.store.delete_user(id).await? {
        Ok(true) => Ok(StatusCode::NO_CONTENT.into_response()),
        Ok(false) => Err(AuthError::NotFound),
        Err(LastAdmin) => Err(AuthError::Conflict("cannot delete the last admin")),
    }
}

#[cfg(test)]
mod tests {
    use super::{User, UserView};

    #[test]
    fn user_view_says_whether_a_password_is_set_but_never_the_hash() {
        let user = User {
            id: 1,
            username: "hani".to_owned(),
            display_name: None,
            role: "admin".to_owned(),
            password_hash: Some("$argon2id$secret".to_owned()),
            oidc_issuer: Some("https://auth.example".to_owned()),
            oidc_subject: Some("sub".to_owned()),
            created_at: 0,
            last_login_at: None,
            disabled: false,
        };
        let with = serde_json::to_value(UserView::from(user.clone())).unwrap();
        assert_eq!(with["has_password"], true);
        assert_eq!(with["username"], "hani");
        assert!(!with.to_string().contains("argon2"));
        let sso_only = User {
            password_hash: None,
            ..user
        };
        let without = serde_json::to_value(UserView::from(sso_only)).unwrap();
        assert_eq!(without["has_password"], false);
    }
}
