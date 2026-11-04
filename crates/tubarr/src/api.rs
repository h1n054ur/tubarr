//! `/api`: the one path Sonarr and Radarr call for both sides of Tubarr. Newznab requests carry
//! `t=`, SABnzbd requests carry `mode=`. [`dispatch`] routes between them and nothing else.

use std::collections::HashMap;

use axum::{
    Router,
    extract::{Request, State},
    response::{IntoResponse, Response},
    routing::get,
};

use crate::{AppState, newznab, sabnzbd};

pub fn router() -> Router<AppState> {
    Router::new().route("/api", get(dispatch).post(dispatch))
}

/// Routes an `/api` request by its query string. The SABnzbd handler gets the whole request,
/// since `mode=addfile` posts the NZB as a multipart body. Both sides check the same key.
async fn dispatch(State(state): State<AppState>, request: Request) -> Response {
    let params = query_params(request.uri().query());
    if params.contains_key("t") {
        return newznab::handle(&state, &params).await;
    }
    if params.contains_key("mode") {
        if checked_key(&state, &params).is_none() {
            return sabnzbd::key_refused(params.contains_key("apikey"));
        }
        return sabnzbd::handle(&state, request).await;
    }
    newznab::Reply::from(newznab::ApiError::Missing("t")).into_response()
}

/// Query parameters by name. A repeated name keeps its first value.
pub fn query_params(query: Option<&str>) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for (name, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        params
            .entry(name.into_owned())
            .or_insert_with(|| value.into_owned());
    }
    params
}

/// The configured API key when the request's `apikey=` matches it. `None` for a wrong or missing
/// key, and always when Tubarr has no key configured: `/api` never runs open.
pub fn checked_key<'a>(state: &'a AppState, params: &HashMap<String, String>) -> Option<&'a str> {
    let expected = state.config.api_key.as_ref()?.expose();
    let given = params.get("apikey")?;
    constant_time_eq(expected.as_bytes(), given.as_bytes()).then_some(expected)
}

/// Compares without stopping at the first difference, so timing does not leak the key.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_keeps_first_value_and_decodes() {
        let params = query_params(Some("t=tvsearch&q=Kabhi+Main%20Tum&t=caps&ep=10%2F14"));
        assert_eq!(params["t"], "tvsearch");
        assert_eq!(params["q"], "Kabhi Main Tum");
        assert_eq!(params["ep"], "10/14");
        assert!(query_params(None).is_empty());
    }

    #[test]
    fn keys_compare_exactly() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
        assert!(!constant_time_eq(b"", b"a"));
    }
}
