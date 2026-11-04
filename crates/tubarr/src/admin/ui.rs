//! The web UI, built from `web/` by `bun run build` and embedded at compile time (see `build.rs`).
//! Any GET that no other route answers gets a file from the build, or `index.html` so the
//! client-side router can take the path. Without a build the binary still compiles and answers
//! with a short "UI not built" page.

use axum::{
    http::{Method, StatusCode, Uri, header},
    response::{Html, IntoResponse, Response},
};

include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

const NOT_BUILT: &str = "<!doctype html><html><head><meta charset=\"utf-8\"><title>Tubarr</title></head>\
<body>\
<h1>Tubarr</h1><p>The web UI is not built into this binary. Run <code>bun install</code> and \
<code>bun run build</code> in <code>web/</code>, then build Tubarr again.</p></body></html>";

fn asset(path: &str) -> Option<&'static [u8]> {
    ASSETS.iter().find(|(p, _)| *p == path).map(|(_, b)| *b)
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("json" | "webmanifest") => "application/json",
        Some("woff2") => "font/woff2",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

pub async fn serve(method: Method, uri: Uri) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = uri.path().trim_start_matches('/');
    if let Some(body) = asset(path).filter(|_| !path.is_empty()) {
        // Vite puts a content hash in every file name under assets/, so those never change.
        let cache = if path.starts_with("assets/") {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        };
        return (
            [
                (header::CONTENT_TYPE, content_type(path)),
                (header::CACHE_CONTROL, cache),
            ],
            body,
        )
            .into_response();
    }
    // A missing hashed file is a real 404, not a page for the router.
    if path.starts_with("assets/") {
        return StatusCode::NOT_FOUND.into_response();
    }
    match asset("index.html") {
        Some(index) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            index,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, Html(NOT_BUILT)).into_response(),
    }
}

/// Whether this binary carries a UI build.
pub fn built() -> bool {
    asset("index.html").is_some()
}
