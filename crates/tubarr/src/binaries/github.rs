//! The one GitHub API call we need: the latest release of a repo, asked conditionally with its ETag.

use reqwest::{Client, StatusCode, header};
use serde::{Deserialize, Serialize};

use super::{BinaryError, now_unix};

/// A release as far as we care: its tag and its assets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

impl Release {
    pub fn asset_url(&self, name: &str) -> Option<&str> {
        self.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.browser_download_url.as_str())
    }
}

pub enum Latest {
    /// 304: the release we saw last time is still the latest. Does not count against the rate limit.
    NotModified,
    Fresh {
        release: Release,
        etag: Option<String>,
    },
}

/// Asks `GET {api}/repos/{repo}/releases/latest`, unauthenticated (60 requests an hour per IP).
pub async fn latest_release(
    client: &Client,
    api: &str,
    repo: &str,
    etag: Option<&str>,
) -> Result<Latest, BinaryError> {
    let url = format!("{}/repos/{repo}/releases/latest", api.trim_end_matches('/'));
    let mut req = client
        .get(&url)
        .header(header::ACCEPT, "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(etag) = etag {
        req = req.header(header::IF_NONE_MATCH, etag);
    }
    let resp = req.send().await?;
    let status = resp.status();
    if status == StatusCode::NOT_MODIFIED {
        return Ok(Latest::NotModified);
    }
    if (status == StatusCode::FORBIDDEN || status == StatusCode::TOO_MANY_REQUESTS)
        && let Some(until) = rate_limit_until(resp.headers())
    {
        return Err(BinaryError::RateLimited { until });
    }
    if !status.is_success() {
        return Err(BinaryError::Status { url, status });
    }
    let etag = resp
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let release = resp.json::<Release>().await?;
    Ok(Latest::Fresh { release, etag })
}

/// When GitHub says we may ask again, from `retry-after` (secondary limit) or `x-ratelimit-reset`.
fn rate_limit_until(headers: &header::HeaderMap) -> Option<u64> {
    let num = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    if let Some(secs) = num("retry-after") {
        return Some(now_unix() + secs);
    }
    if num("x-ratelimit-remaining") == Some(0) {
        return num("x-ratelimit-reset");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_reads_reset_only_when_exhausted() {
        let mut h = header::HeaderMap::new();
        h.insert("x-ratelimit-remaining", "3".parse().unwrap());
        h.insert("x-ratelimit-reset", "1900000000".parse().unwrap());
        assert_eq!(rate_limit_until(&h), None);
        h.insert("x-ratelimit-remaining", "0".parse().unwrap());
        assert_eq!(rate_limit_until(&h), Some(1_900_000_000));
    }

    #[test]
    fn retry_after_wins() {
        let mut h = header::HeaderMap::new();
        h.insert("retry-after", "60".parse().unwrap());
        let until = rate_limit_until(&h).unwrap();
        assert!(until >= now_unix() + 59);
    }
}
