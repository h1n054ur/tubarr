//! Response headers that tell browsers what the UI may do. Every response but `/play` gets the
//! full set. `/play` gets only `nosniff`: players fetch it as plain media and need nothing else.
//!
//! The policy fits the built UI in `web/`: its scripts, styles, fonts, icons and API calls are all
//! same-origin (Lexend is bundled into the UI), so the browser loads nothing from anywhere else.
//! `data:` images are allowed for small inlined icons. No inline scripts or `style` attributes in
//! the HTML (React sets styles through the DOM, which CSP allows). Nothing may frame the UI.

use axum::{
    extract::Request,
    http::{HeaderName, HeaderValue, header},
    middleware::Next,
    response::Response,
};

pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; \
style-src 'self'; font-src 'self'; \
img-src 'self' data:; connect-src 'self'; object-src 'none'; base-uri 'self'; \
form-action 'self'; frame-ancestors 'none'";

const PERMISSIONS_POLICY: &str =
    "camera=(), microphone=(), geolocation=(), payment=(), usb=(), browsing-topics=()";

const PERMISSIONS_POLICY_HEADER: HeaderName = HeaderName::from_static("permissions-policy");

/// The full set, for the UI and every API.
pub async fn all(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    for (name, value) in [
        (header::CONTENT_SECURITY_POLICY, CONTENT_SECURITY_POLICY),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "no-referrer"),
        (PERMISSIONS_POLICY_HEADER, PERMISSIONS_POLICY),
    ] {
        headers
            .entry(name)
            .or_insert(HeaderValue::from_static(value));
    }
    response
}

/// Only `nosniff`, for `/play`.
pub async fn nosniff(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .entry(header::X_CONTENT_TYPE_OPTIONS)
        .or_insert(HeaderValue::from_static("nosniff"));
    response
}
