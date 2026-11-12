//! Built-in sign-in: first-run setup, password login, sessions, CSRF, the API key fallback and
//! the network rules for local bypass and `/play`.

use std::net::SocketAddr;

use axum::{
    body::Body,
    extract::ConnectInfo,
    http::{HeaderMap, Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use tubarr::{AppState, binaries::BinaryManager, config::Config, store::Db};

const KEY: &str = "test-admin-key";
const PASSWORD: &str = "correct horse battery";

struct Kit {
    app: axum::Router,
    state: AppState,
    /// The setup code a fresh instance would log.
    code: String,
    _dir: tempfile::TempDir,
}

async fn kit(extra: &str) -> Kit {
    kit_at("http://localhost:8480", extra).await
}

async fn kit_at(public_url: &str, extra: &str) -> Kit {
    kit_with(public_url, extra, None).await
}

/// `preset` stands in for `TUBARR_SETUP_CODE`.
async fn kit_with(public_url: &str, extra: &str, preset: Option<&str>) -> Kit {
    let dir = tempfile::tempdir().unwrap();
    let key_file = dir.path().join("key");
    std::fs::write(&key_file, KEY).unwrap();
    let toml = format!(
        "public_url = \"{public_url}\"\ndata_dir = {:?}\napi_key_file = {key_file:?}\n{extra}",
        dir.path()
    );
    let mut config = Config::from_toml(&toml).unwrap();
    if let Some(preset) = preset {
        config.auth = config.auth.with_setup_code(preset).unwrap();
    }
    let db = Db::open(&config.database_path()).await.unwrap();
    let binaries = std::sync::Arc::new(BinaryManager::new(config.binaries.clone()).unwrap());
    let state = AppState::new(config, db, binaries).unwrap();
    let code = match preset {
        Some(preset) => preset.to_owned(),
        None => state
            .auth
            .setup_code()
            .expect("a fresh instance has a setup code"),
    };
    let app = tubarr::app(state.clone());
    Kit {
        app,
        state,
        code,
        _dir: dir,
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Value,
}

impl Reply {
    /// The `name=value` part of the session cookie this reply sets.
    fn session(&self) -> Option<String> {
        self.set_cookie()
            .and_then(|c| c.split(';').next().map(str::to_owned))
            .filter(|c| c != "tubarr_session=")
    }

    fn set_cookie(&self) -> Option<&str> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|c| c.starts_with("tubarr_session="))
    }
}

struct Call<'a> {
    method: &'a str,
    path: &'a str,
    body: Option<Value>,
    headers: Vec<(&'a str, String)>,
    peer: Option<&'a str>,
}

fn call<'a>(method: &'a str, path: &'a str) -> Call<'a> {
    Call {
        method,
        path,
        body: None,
        headers: Vec::new(),
        peer: Some("203.0.113.7:5000"),
    }
}

impl<'a> Call<'a> {
    fn json(mut self, body: Value) -> Self {
        self.body = Some(body);
        self
    }
    fn header(mut self, name: &'a str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }
    /// The header every browser write carries.
    fn csrf(self) -> Self {
        self.header("x-requested-with", "tubarr")
    }
    fn cookie(self, cookie: &str) -> Self {
        self.header("cookie", cookie)
    }
    fn peer(mut self, peer: &'a str) -> Self {
        self.peer = Some(peer);
        self
    }
    async fn send(self, app: &axum::Router) -> Reply {
        let mut request = Request::builder().method(self.method).uri(self.path);
        for (name, value) in &self.headers {
            request = request.header(*name, value);
        }
        let body = match &self.body {
            Some(body) => {
                request = request.header("content-type", "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let mut request = request.body(body).unwrap();
        if let Some(peer) = self.peer {
            let addr: SocketAddr = peer.parse().unwrap();
            request.extensions_mut().insert(ConnectInfo(addr));
        }
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Reply {
            status,
            headers,
            body,
        }
    }
}

async fn setup(k: &Kit) -> String {
    let reply = call("POST", "/auth/setup")
        .csrf()
        .json(json!({"username": "hani", "password": PASSWORD, "setup_code": k.code}))
        .send(&k.app)
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    reply.session().expect("setup signs in")
}

async fn login(app: &axum::Router, username: &str, password: &str) -> Reply {
    call("POST", "/auth/login")
        .csrf()
        .json(json!({"username": username, "password": password}))
        .send(app)
        .await
}

#[tokio::test]
async fn setup_runs_once_and_signs_in_with_safe_cookie_flags() {
    let k = kit("").await;
    let state = call("GET", "/auth/state").send(&k.app).await;
    assert_eq!(state.body["setup_required"], true);
    assert_eq!(state.body["methods"]["password"], true);
    assert_eq!(state.body["methods"]["oidc"], false);
    assert!(state.body["user"].is_null());

    let refused = call("POST", "/auth/setup")
        .json(json!({"username": "hani", "password": PASSWORD}))
        .send(&k.app)
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let short = call("POST", "/auth/setup")
        .csrf()
        .json(json!({"username": "hani", "password": "short", "setup_code": k.code}))
        .send(&k.app)
        .await;
    assert_eq!(short.status, StatusCode::UNPROCESSABLE_ENTITY);

    let reply = call("POST", "/auth/setup")
        .csrf()
        .json(json!({"username": "hani", "password": PASSWORD, "setup_code": k.code}))
        .send(&k.app)
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["user"]["username"], "hani");
    assert!(reply.body["user"].get("password_hash").is_none());
    let flags = reply.set_cookie().unwrap();
    assert!(flags.contains("HttpOnly"), "{flags}");
    assert!(flags.contains("SameSite=Lax"), "{flags}");
    assert!(flags.contains("Path=/;"), "{flags}");
    assert!(!flags.contains("Secure"), "{flags}");
    let cookie = reply.session().unwrap();

    let again = call("POST", "/auth/setup")
        .csrf()
        .json(json!({"username": "other", "password": PASSWORD, "setup_code": k.code}))
        .send(&k.app)
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    assert_eq!(k.state.auth.setup_code(), None, "the code is dropped");

    let state = call("GET", "/auth/state")
        .cookie(&cookie)
        .send(&k.app)
        .await;
    assert_eq!(state.body["setup_required"], false);
    assert_eq!(state.body["user"]["username"], "hani");
    assert_eq!(state.body["via"], "session");
}

async fn setup_with(k: &Kit, code: Option<&str>, peer: &'static str) -> Reply {
    let mut body = json!({"username": "hani", "password": PASSWORD});
    if let Some(code) = code {
        body["setup_code"] = json!(code);
    }
    call("POST", "/auth/setup")
        .csrf()
        .peer(peer)
        .json(body)
        .send(&k.app)
        .await
}

#[tokio::test]
async fn setup_needs_the_code_from_the_log() {
    let k = kit("").await;
    let shown = &k.code;
    assert_eq!(shown.len(), 29, "six groups of four: {shown}");
    for code in [None, Some(""), Some("ABCD-EFGH-JKMN-PQRS-TUVW-XYZ2")] {
        let refused = setup_with(&k, code, "203.0.113.7:5000").await;
        assert_eq!(refused.status, StatusCode::FORBIDDEN, "{code:?}");
        assert!(refused.body["error"].is_string(), "{:?}", refused.body);
        assert!(refused.session().is_none());
    }
    let state = call("GET", "/auth/state").send(&k.app).await;
    assert_eq!(state.body["setup_required"], true);

    // Case, spaces and dashes don't matter.
    let typed = shown.to_lowercase().replace('-', " ");
    let reply = setup_with(&k, Some(&typed), "203.0.113.7:5000").await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    assert_eq!(k.state.auth.setup_code(), None);
    let again = setup_with(&k, Some(shown), "203.0.113.7:5000").await;
    assert_eq!(again.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn wrong_setup_codes_are_throttled_per_address() {
    let k = kit("").await;
    for _ in 0..5 {
        let reply = setup_with(&k, Some("WRONG-CODE"), "203.0.113.7:5000").await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
    }
    // Even the right code waits now, from that address only.
    let reply = setup_with(&k, Some(&k.code), "203.0.113.7:5000").await;
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(reply.headers.contains_key(header::RETRY_AFTER));
    let other = setup_with(&k, Some(&k.code), "198.51.100.4:5000").await;
    assert_eq!(other.status, StatusCode::OK, "{:?}", other.body);
}

/// Log lines written while the guard lives, on this thread.
#[derive(Clone, Default)]
struct Logs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Logs {
    fn capture(&self) -> tracing::subscriber::DefaultGuard {
        let logs = self.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || logs.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::set_default(subscriber)
    }

    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
}

#[tokio::test]
async fn the_log_shows_the_random_code_and_the_url() {
    let k = kit_at("http://tubarr.lan:8480", "").await;
    let logs = Logs::default();
    let guard = logs.capture();
    k.state.auth.announce_setup().await.unwrap();
    drop(guard);
    let text = logs.text();
    assert!(text.contains("WARN"), "{text}");
    assert!(text.contains(&k.code), "{text}");
    assert!(text.contains("http://tubarr.lan:8480/"), "{text}");
    assert_eq!(text.lines().count(), 1, "{text}");
}

#[tokio::test]
async fn a_code_from_the_env_works_and_is_never_logged() {
    let preset = "my-own-setup-code-42";
    let k = kit_with("http://localhost:8480", "", Some(preset)).await;
    assert_eq!(k.state.auth.setup_code(), None);
    let logs = Logs::default();
    let guard = logs.capture();
    k.state.auth.announce_setup().await.unwrap();
    let wrong = setup_with(&k, Some("my-own-setup-code-43"), "203.0.113.7:5000").await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    let reply = setup_with(&k, Some(preset), "203.0.113.7:5000").await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);
    drop(guard);
    let text = logs.text();
    assert!(text.contains("TUBARR_SETUP_CODE"), "{text}");
    for form in [preset, "MYOWNSETUPCODE42", "my-own-setup-code-43"] {
        assert!(
            !text.to_lowercase().contains(&form.to_lowercase()),
            "{text}"
        );
    }
}

#[test]
fn a_short_env_code_is_refused() {
    let config = Config::from_toml("public_url = \"http://localhost:8480\"").unwrap();
    let err = config
        .auth
        .with_setup_code("abc-123")
        .unwrap_err()
        .to_string();
    assert!(err.contains("TUBARR_SETUP_CODE"), "{err}");
}

#[tokio::test]
async fn an_instance_with_an_admin_drops_the_code_and_logs_nothing() {
    let k = kit("").await;
    setup(&k).await;
    // A restart on the same database, which now has an admin.
    let restarted = AppState::new(
        (*k.state.config).clone(),
        k.state.db.clone(),
        k.state.binaries.clone(),
    )
    .unwrap();
    assert!(restarted.auth.setup_code().is_some());
    let logs = Logs::default();
    let guard = logs.capture();
    restarted.auth.announce_setup().await.unwrap();
    drop(guard);
    assert_eq!(logs.text(), "");
    assert_eq!(restarted.auth.setup_code(), None);
}

#[tokio::test]
async fn https_ui_gets_secure_cookies() {
    let k = kit_at("https://tubarr.example.dev", "").await;
    let reply = call("POST", "/auth/setup")
        .csrf()
        .json(json!({"username": "hani", "password": PASSWORD, "setup_code": k.code}))
        .send(&k.app)
        .await;
    assert!(reply.set_cookie().unwrap().ends_with("; Secure"));
}

#[tokio::test]
async fn login_succeeds_fails_generically_and_rotates_the_session() {
    let k = kit("").await;
    let first = setup(&k).await;

    let wrong = login(&k.app, "hani", "wrong password").await;
    let unknown = login(&k.app, "nobody", PASSWORD).await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong.body, unknown.body);
    assert!(wrong.session().is_none());

    let reply = call("POST", "/auth/login")
        .csrf()
        .cookie(&first)
        .json(json!({"username": "HANI", "password": PASSWORD}))
        .send(&k.app)
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let second = reply.session().unwrap();
    assert_ne!(first, second);

    // The session the browser had before signing in is gone.
    let old = call("GET", "/admin/api/mappings")
        .cookie(&first)
        .send(&k.app)
        .await;
    assert_eq!(old.status, StatusCode::UNAUTHORIZED);
    let new = call("GET", "/admin/api/mappings")
        .cookie(&second)
        .send(&k.app)
        .await;
    assert_eq!(new.status, StatusCode::OK);
    let health = call("GET", "/api/health")
        .cookie(&second)
        .send(&k.app)
        .await;
    assert_ne!(health.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn repeated_failures_are_throttled() {
    let k = kit("").await;
    setup(&k).await;
    for _ in 0..5 {
        assert_eq!(
            login(&k.app, "hani", "wrong password").await.status,
            StatusCode::UNAUTHORIZED
        );
    }
    // Even the right password waits now.
    let reply = login(&k.app, "hani", PASSWORD).await;
    assert_eq!(reply.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(reply.headers.contains_key(header::RETRY_AFTER));
    assert!(reply.session().is_none());
}

#[tokio::test]
async fn a_bad_source_cannot_lock_a_user_out_from_elsewhere() {
    let k = kit("").await;
    setup(&k).await;
    for _ in 0..8 {
        let reply = call("POST", "/auth/login")
            .csrf()
            .peer("198.51.100.4:5000")
            .json(json!({"username": "hani", "password": "wrong password"}))
            .send(&k.app)
            .await;
        assert_ne!(reply.status, StatusCode::OK);
    }
    let attacker = call("POST", "/auth/login")
        .csrf()
        .peer("198.51.100.4:5000")
        .json(json!({"username": "hani", "password": PASSWORD}))
        .send(&k.app)
        .await;
    assert_eq!(attacker.status, StatusCode::TOO_MANY_REQUESTS);
    let owner = login(&k.app, "hani", PASSWORD).await;
    assert_eq!(owner.status, StatusCode::OK, "{:?}", owner.body);
}

async fn proxied_login(k: &Kit, client: &str, password: &str) -> StatusCode {
    call("POST", "/auth/login")
        .csrf()
        .peer("172.16.0.2:5000")
        .header("x-forwarded-for", client)
        .json(json!({"username": "hani", "password": password}))
        .send(&k.app)
        .await
        .status
}

#[tokio::test]
async fn throttles_count_each_client_behind_a_trusted_proxy() {
    let k = kit("[auth]\ntrusted_proxies = [\"172.16.0.2/32\"]\n").await;
    for _ in 0..5 {
        let reply = call("POST", "/auth/setup")
            .csrf()
            .peer("172.16.0.2:5000")
            .header("x-forwarded-for", "198.51.100.4")
            .json(json!({"username": "hani", "password": PASSWORD, "setup_code": "WRONG-CODE"}))
            .send(&k.app)
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN);
    }
    let reply = call("POST", "/auth/setup")
        .csrf()
        .peer("172.16.0.2:5000")
        .header("x-forwarded-for", "203.0.113.9")
        .json(json!({"username": "hani", "password": PASSWORD, "setup_code": k.code}))
        .send(&k.app)
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{:?}", reply.body);

    for _ in 0..5 {
        let status = proxied_login(&k, "198.51.100.4", "wrong password").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    assert_eq!(
        proxied_login(&k, "198.51.100.4", PASSWORD).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    assert_eq!(
        proxied_login(&k, "203.0.113.9", PASSWORD).await,
        StatusCode::OK
    );
    // A client cannot dodge its counter by naming another address: the proxy appends the real one.
    assert_eq!(
        proxied_login(&k, "203.0.113.9, 198.51.100.4", PASSWORD).await,
        StatusCode::TOO_MANY_REQUESTS
    );

    // Without trusted_proxies the proxy is the one client everybody shares.
    let plain = kit("").await;
    setup(&plain).await;
    for _ in 0..5 {
        proxied_login(&plain, "198.51.100.4", "wrong password").await;
    }
    assert_eq!(
        proxied_login(&plain, "203.0.113.9", PASSWORD).await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

#[tokio::test]
async fn local_bypass_judges_the_client_behind_a_trusted_proxy() {
    let k = kit("[auth]\nlocal_bypass = true\ntrusted_proxies = [\"172.16.0.2/32\"]\n").await;
    let via = |forwarded: Option<&'static str>, peer: &'static str| {
        let mut c = call("GET", "/admin/api/mappings").peer(peer);
        if let Some(f) = forwarded {
            c = c.header("x-forwarded-for", f);
        }
        c
    };
    let lan = via(Some("192.168.1.5"), "172.16.0.2:4000")
        .send(&k.app)
        .await;
    assert_eq!(lan.status, StatusCode::OK);
    let internet = via(Some("203.0.113.7"), "172.16.0.2:4000")
        .send(&k.app)
        .await;
    assert_eq!(internet.status, StatusCode::UNAUTHORIZED);
    // The proxy itself is never let in, so one that drops X-Forwarded-For opens nothing.
    let bare = via(None, "172.16.0.2:4000").send(&k.app).await;
    assert_eq!(bare.status, StatusCode::UNAUTHORIZED);
    // An untrusted peer cannot claim a private address.
    let spoof = via(Some("192.168.1.5"), "198.51.100.4:4000")
        .send(&k.app)
        .await;
    assert_eq!(spoof.status, StatusCode::UNAUTHORIZED);
    // A LAN peer that is not a proxy still gets in directly.
    let direct = via(None, "192.168.1.9:4000").send(&k.app).await;
    assert_eq!(direct.status, StatusCode::OK);
}

#[tokio::test]
async fn ui_and_api_carry_security_headers_and_play_only_nosniff() {
    let k = kit("").await;
    for path in [
        "/",
        "/mappings",
        "/auth/state",
        "/admin/api/mappings",
        "/healthz",
    ] {
        let reply = call("GET", path).send(&k.app).await;
        let h = &reply.headers;
        assert_eq!(
            h["content-security-policy"],
            tubarr::security_headers::CONTENT_SECURITY_POLICY,
            "{path}"
        );
        assert!(
            h["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
        assert!(
            !h["content-security-policy"]
                .to_str()
                .unwrap()
                .contains("https:"),
            "the UI loads nothing from another host"
        );
        assert_eq!(h["x-content-type-options"], "nosniff", "{path}");
        assert_eq!(h["x-frame-options"], "DENY", "{path}");
        assert_eq!(h["referrer-policy"], "no-referrer", "{path}");
        assert!(
            h["permissions-policy"]
                .to_str()
                .unwrap()
                .contains("camera=()")
        );
    }
    let play = call("GET", "/play/0eD-GQCutAU")
        .peer("192.168.1.20:4000")
        .send(&k.app)
        .await;
    assert_eq!(play.headers["x-content-type-options"], "nosniff");
    for name in [
        "content-security-policy",
        "x-frame-options",
        "referrer-policy",
    ] {
        assert!(!play.headers.contains_key(name), "{name}");
    }
}

#[tokio::test]
async fn logout_ends_the_session() {
    let k = kit("").await;
    let cookie = setup(&k).await;
    let no_csrf = call("POST", "/auth/logout")
        .cookie(&cookie)
        .send(&k.app)
        .await;
    assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
    let reply = call("POST", "/auth/logout")
        .csrf()
        .cookie(&cookie)
        .send(&k.app)
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(reply.set_cookie().unwrap().contains("Max-Age=0"));
    let after = call("GET", "/admin/api/mappings")
        .cookie(&cookie)
        .send(&k.app)
        .await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn session_writes_need_the_csrf_header_but_the_api_key_does_not() {
    let k = kit("").await;
    let cookie = setup(&k).await;
    let pin = json!({"mapping_id": 999, "video_id": "0eD-GQCutAU"});

    let refused = call("POST", "/admin/api/pins")
        .cookie(&cookie)
        .json(pin.clone())
        .send(&k.app)
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert!(
        refused.body["error"]
            .as_str()
            .unwrap()
            .contains("X-Requested-With")
    );

    // Past the auth check, the handler itself answers (the mapping does not exist).
    let with_header = call("POST", "/admin/api/pins")
        .cookie(&cookie)
        .csrf()
        .json(pin.clone())
        .send(&k.app)
        .await;
    assert!(
        !matches!(
            with_header.status,
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED
        ),
        "{}",
        with_header.status
    );
    let by_key = call("POST", "/admin/api/pins")
        .header("x-api-key", KEY)
        .json(pin)
        .send(&k.app)
        .await;
    assert_eq!(by_key.status, with_header.status);
}

#[tokio::test]
async fn api_key_still_opens_the_admin_api() {
    let k = kit("").await;
    let ok = call("GET", "/admin/api/mappings")
        .header("x-api-key", KEY)
        .send(&k.app)
        .await;
    assert_eq!(ok.status, StatusCode::OK);
    let wrong = call("GET", "/admin/api/mappings")
        .header("x-api-key", "nope")
        .send(&k.app)
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let none = call("GET", "/admin/api/mappings").send(&k.app).await;
    assert_eq!(none.status, StatusCode::UNAUTHORIZED);
    // The old UI sends an empty key when it has none.
    let empty = call("GET", "/admin/api/mappings")
        .header("x-api-key", "")
        .send(&k.app)
        .await;
    assert_eq!(empty.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn arr_api_takes_only_its_key() {
    let k = kit("").await;
    let cookie = setup(&k).await;
    let by_cookie = call("GET", "/api?mode=version")
        .cookie(&cookie)
        .send(&k.app)
        .await;
    assert_eq!(by_cookie.body["status"], false);
    let by_key = call("GET", &format!("/api?mode=version&apikey={KEY}"))
        .send(&k.app)
        .await;
    assert!(by_key.body["version"].is_string(), "{:?}", by_key.body);
}

#[tokio::test]
async fn local_bypass_lets_private_peers_in() {
    let k = kit("[auth]\nlocal_bypass = true\n").await;
    let lan = call("GET", "/admin/api/mappings")
        .peer("192.168.1.5:4000")
        .send(&k.app)
        .await;
    assert_eq!(lan.status, StatusCode::OK);
    let state = call("GET", "/auth/state")
        .peer("127.0.0.1:4000")
        .send(&k.app)
        .await;
    assert_eq!(state.body["via"], "local_bypass");
    let outside = call("GET", "/admin/api/mappings")
        .peer("198.51.100.4:4000")
        .send(&k.app)
        .await;
    assert_eq!(outside.status, StatusCode::UNAUTHORIZED);
    // Bypassed writes still need the CSRF header, or any web page could post to the LAN.
    let write = call("POST", "/admin/api/pins")
        .peer("192.168.1.5:4000")
        .json(json!({"mapping_id": 1, "video_id": "x"}))
        .send(&k.app)
        .await;
    assert_eq!(write.status, StatusCode::FORBIDDEN);

    let off = kit("").await;
    let lan = call("GET", "/admin/api/mappings")
        .peer("192.168.1.5:4000")
        .send(&off.app)
        .await;
    assert_eq!(lan.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn play_networks_limit_who_may_play() {
    let k = kit("[auth]\nplay_allowed_networks = [\"203.0.113.0/24\"]\n").await;
    let outside = call("GET", "/play/0eD-GQCutAU")
        .peer("198.51.100.4:4000")
        .send(&k.app)
        .await;
    assert_eq!(outside.status, StatusCode::FORBIDDEN);
    let inside = call("GET", "/play/0eD-GQCutAU")
        .peer("203.0.113.1:4000")
        .send(&k.app)
        .await;
    assert_ne!(inside.status, StatusCode::FORBIDDEN);

    let open = kit("[auth]\nplay_allowed_networks = [\"0.0.0.0/0\", \"::/0\"]\n").await;
    let anyone = call("GET", "/play/0eD-GQCutAU")
        .peer("198.51.100.4:4000")
        .send(&open.app)
        .await;
    assert_ne!(anyone.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn play_defaults_to_private_networks() {
    let k = kit("").await;
    for public in ["198.51.100.4:4000", "[2001:db8::7]:4000"] {
        let reply = call("GET", "/play/0eD-GQCutAU")
            .peer(public)
            .send(&k.app)
            .await;
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{public}");
    }
    for private in [
        "127.0.0.1:4000",
        "172.16.0.5:4000",
        "172.18.0.3:4000",
        "192.168.1.20:4000",
        "100.101.102.103:4000",
        "[fd7a:115c:a1e0::1]:4000",
        "[::1]:4000",
    ] {
        let reply = call("GET", "/play/0eD-GQCutAU")
            .peer(private)
            .send(&k.app)
            .await;
        assert_ne!(reply.status, StatusCode::FORBIDDEN, "{private}");
    }
}

#[tokio::test]
async fn play_believes_forwarded_for_only_from_trusted_proxies() {
    let k = kit("[auth]\ntrusted_proxies = [\"172.16.0.2/32\"]\n").await;
    let internet = call("GET", "/play/0eD-GQCutAU")
        .peer("172.16.0.2:4000")
        .header("x-forwarded-for", "203.0.113.7")
        .send(&k.app)
        .await;
    assert_eq!(internet.status, StatusCode::FORBIDDEN);
    let lan = call("GET", "/play/0eD-GQCutAU")
        .peer("172.16.0.2:4000")
        .header("x-forwarded-for", "192.168.1.20")
        .send(&k.app)
        .await;
    assert_ne!(lan.status, StatusCode::FORBIDDEN);
    // An untrusted peer cannot claim a private address.
    let spoof = call("GET", "/play/0eD-GQCutAU")
        .peer("198.51.100.4:4000")
        .header("x-forwarded-for", "172.16.0.5")
        .send(&k.app)
        .await;
    assert_eq!(spoof.status, StatusCode::FORBIDDEN);
    // Without trusted_proxies, the proxy's own private address is what counts.
    let plain = kit("").await;
    let through = call("GET", "/play/0eD-GQCutAU")
        .peer("172.16.0.2:4000")
        .header("x-forwarded-for", "203.0.113.7")
        .send(&plain.app)
        .await;
    assert_ne!(through.status, StatusCode::FORBIDDEN);
}

const PROXY_AUTH: &str = "[auth]\ntrusted_proxies = [\"172.16.0.2/32\", \"172.16.0.3/32\"]\n";

async fn proxy_play_status(k: &Kit, peer: &str, forwarded: &[&str]) -> StatusCode {
    let mut request = call("GET", "/play/not-an-id").peer(peer);
    for value in forwarded {
        request = request.header("x-forwarded-for", *value);
    }
    request.send(&k.app).await.status
}

#[tokio::test]
async fn trusted_proxy_without_client_identity_cannot_play() {
    let k = kit(PROXY_AUTH).await;
    for method in ["GET", "HEAD"] {
        for path in ["/play/not-an-id", "/play/not-an-id/audio"] {
            let reply = call(method, path)
                .peer("172.16.0.2:4000")
                .send(&k.app)
                .await;
            assert_eq!(reply.status, StatusCode::FORBIDDEN, "{method} {path}");
        }
    }
}

#[tokio::test]
async fn trusted_proxy_with_invalid_or_empty_xff_cannot_play() {
    let k = kit(PROXY_AUTH).await;
    for values in [
        vec![""],
        vec![" "],
        vec!["nope"],
        vec!["192.168.1.20,"],
        vec!["192.168.1.20, , 172.16.0.3"],
        vec!["192.168.1.20", "nope, 172.16.0.3"],
        vec!["192.168.1.20", ""],
    ] {
        assert_eq!(
            proxy_play_status(&k, "172.16.0.2:4000", &values).await,
            StatusCode::FORBIDDEN,
            "{values:?}"
        );
    }
}

#[tokio::test]
async fn all_trusted_xff_chain_cannot_play() {
    let k = kit(PROXY_AUTH).await;
    for values in [
        vec!["172.16.0.3"],
        vec!["172.16.0.2, 172.16.0.3"],
        vec!["172.16.0.2", "172.16.0.3"],
        vec!["::ffff:172.16.0.3"],
    ] {
        assert_eq!(
            proxy_play_status(&k, "172.16.0.2:4000", &values).await,
            StatusCode::FORBIDDEN,
            "{values:?}"
        );
    }
}

#[tokio::test]
async fn trusted_proxy_uses_rightmost_untrusted_hop() {
    let k = kit(PROXY_AUTH).await;
    for chain in [
        "192.168.1.20, 203.0.113.7, 172.16.0.3",
        "nope, 203.0.113.7, 172.16.0.3",
    ] {
        assert_eq!(
            proxy_play_status(&k, "172.16.0.2:4000", &[chain]).await,
            StatusCode::FORBIDDEN,
            "{chain}"
        );
    }
    for chain in [
        "203.0.113.7, 192.168.1.20, 172.16.0.3",
        "nope, 192.168.1.20, 172.16.0.3",
        ", 192.168.1.20, 172.16.0.3",
    ] {
        assert_eq!(
            proxy_play_status(&k, "172.16.0.2:4000", &[chain]).await,
            StatusCode::NOT_FOUND,
            "{chain}"
        );
    }
}

#[tokio::test]
async fn trusted_proxy_respects_repeated_xff_fields() {
    let k = kit(PROXY_AUTH).await;
    assert_eq!(
        proxy_play_status(
            &k,
            "172.16.0.2:4000",
            &["192.168.1.20, 203.0.113.7", "172.16.0.3"]
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        proxy_play_status(
            &k,
            "172.16.0.2:4000",
            &["203.0.113.7", "192.168.1.20, 172.16.0.3"]
        )
        .await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn trusted_proxy_canonicalizes_ipv4_mapped_peer_and_hops() {
    let k = kit(PROXY_AUTH).await;
    let peer = "[::ffff:172.16.0.2]:4000";
    assert_eq!(
        proxy_play_status(&k, peer, &[]).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        proxy_play_status(&k, peer, &["::ffff:172.16.0.3"]).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        proxy_play_status(&k, peer, &["::ffff:203.0.113.7, ::ffff:172.16.0.3"]).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        proxy_play_status(&k, peer, &["::ffff:192.168.1.20, ::ffff:172.16.0.3"]).await,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn untrusted_peer_cannot_spoof_private_xff() {
    let k = kit(PROXY_AUTH).await;
    assert_eq!(
        proxy_play_status(&k, "203.0.113.7:4000", &["192.168.1.20", "172.16.0.3"]).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn direct_private_jellyfin_needs_no_xff() {
    let k = kit(PROXY_AUTH).await;
    for peer in ["192.168.1.20:4000", "[::ffff:192.168.1.20]:4000"] {
        assert_eq!(
            proxy_play_status(&k, peer, &[]).await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            proxy_play_status(&k, peer, &["nope"]).await,
            StatusCode::NOT_FOUND,
            "an untrusted direct peer ignores XFF"
        );
    }
}

#[tokio::test]
async fn local_bypass_never_uses_unidentified_trusted_proxy() {
    let k = kit(&format!("{PROXY_AUTH}local_bypass = true\n")).await;
    for values in [vec![], vec![""], vec!["nope"], vec!["172.16.0.3"]] {
        let mut request = call("GET", "/admin/api/mappings").peer("172.16.0.2:4000");
        for value in &values {
            request = request.header("x-forwarded-for", *value);
        }
        assert_eq!(
            request.send(&k.app).await.status,
            StatusCode::UNAUTHORIZED,
            "{values:?}"
        );
    }
}

#[tokio::test]
async fn users_are_managed_and_the_last_admin_stays() {
    let k = kit("").await;
    let cookie = setup(&k).await;
    let created = call("POST", "/admin/api/users")
        .cookie(&cookie)
        .csrf()
        .json(json!({"username": "arshia", "password": "another good one"}))
        .send(&k.app)
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(created.body["has_password"], true);
    assert!(created.body.get("password_hash").is_none());
    let id = created.body["id"].as_i64().unwrap();
    let dup = call("POST", "/admin/api/users")
        .header("x-api-key", KEY)
        .json(json!({"username": "Arshia", "password": "another good one"}))
        .send(&k.app)
        .await;
    assert_eq!(dup.status, StatusCode::CONFLICT);
    assert_eq!(
        login(&k.app, "arshia", "another good one").await.status,
        StatusCode::OK
    );

    let list = call("GET", "/admin/api/users")
        .cookie(&cookie)
        .send(&k.app)
        .await;
    assert_eq!(list.body.as_array().unwrap().len(), 2);
    for user in list.body.as_array().unwrap() {
        assert_eq!(user["has_password"], true);
        assert!(user.get("password_hash").is_none());
    }

    let gone = call("DELETE", &format!("/admin/api/users/{id}"))
        .cookie(&cookie)
        .csrf()
        .send(&k.app)
        .await;
    assert_eq!(gone.status, StatusCode::NO_CONTENT);
    let first = list.body[0]["id"].as_i64().unwrap();
    let last = call("DELETE", &format!("/admin/api/users/{first}"))
        .header("x-api-key", KEY)
        .send(&k.app)
        .await;
    assert_eq!(last.status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn password_change_checks_the_old_one_and_ends_other_sessions() {
    let k = kit("").await;
    let here = setup(&k).await;
    let elsewhere = login(&k.app, "hani", PASSWORD).await.session().unwrap();

    let wrong = call("POST", "/auth/password")
        .cookie(&here)
        .csrf()
        .json(json!({"current_password": "nope nope", "new_password": "a brand new one"}))
        .send(&k.app)
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let ok = call("POST", "/auth/password")
        .cookie(&here)
        .csrf()
        .json(json!({"current_password": PASSWORD, "new_password": "a brand new one"}))
        .send(&k.app)
        .await;
    assert_eq!(ok.status, StatusCode::NO_CONTENT);

    let still = call("GET", "/admin/api/mappings")
        .cookie(&here)
        .send(&k.app)
        .await;
    assert_eq!(still.status, StatusCode::OK);
    let ended = call("GET", "/admin/api/mappings")
        .cookie(&elsewhere)
        .send(&k.app)
        .await;
    assert_eq!(ended.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        login(&k.app, "hani", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(&k.app, "hani", "a brand new one").await.status,
        StatusCode::OK
    );
}
