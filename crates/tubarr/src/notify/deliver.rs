//! Sends cards one at a time from a small queue: Discord's rate limits are honoured, failures are
//! retried with backoff, and a card that keeps failing is dropped with a log line. The webhook URL
//! holds a token, so logs name only the host, the status and Discord's error code.

use std::time::Duration;

use reqwest::{StatusCode, header::HeaderMap};
use tokio::{sync::mpsc, time::Instant};
use url::Url;

use super::card::Card;
use crate::config::{NotifyTarget, Secret};

/// Discord's code for a deleted webhook. Calling it again only counts against the invalid
/// request limit, so delivery stops.
const UNKNOWN_WEBHOOK: u64 = 10015;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts per card, the first included.
    pub tries: u32,
    /// Wait after the first failure, doubled after each further one.
    pub backoff: Duration,
    /// A 429 asking for a longer wait than this drops the card.
    pub max_wait: Duration,
    pub timeout: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            tries: 4,
            backoff: Duration::from_secs(2),
            max_wait: Duration::from_secs(60),
            timeout: Duration::from_secs(10),
        }
    }
}

/// Where cards go. `Debug` never shows the webhook or the token.
#[derive(Clone)]
pub enum Endpoint {
    Discord(Url),
    CardRelay { url: Url, token: Secret },
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Endpoint::Discord(_) => f.write_str("Discord(***)"),
            Endpoint::CardRelay { url, .. } => write!(f, "CardRelay({url})"),
        }
    }
}

impl Endpoint {
    /// `None` only if a checked webhook somehow fails to parse.
    pub fn from_target(target: &NotifyTarget) -> Option<Self> {
        match target {
            NotifyTarget::Discord { webhook } => {
                Url::parse(webhook.expose()).ok().map(Endpoint::Discord)
            }
            NotifyTarget::CardRelay { url, token } => Some(Endpoint::CardRelay {
                url: url.clone(),
                token: token.clone(),
            }),
        }
    }

    fn host(&self) -> String {
        let url = match self {
            Endpoint::Discord(url) | Endpoint::CardRelay { url, .. } => url,
        };
        url.host_str().unwrap_or_default().to_owned()
    }
}

pub struct Delivery {
    client: reqwest::Client,
    endpoint: Endpoint,
    policy: RetryPolicy,
    /// Discord said the bucket is empty: the next card waits until then.
    paced_until: Option<Instant>,
}

enum Outcome {
    Sent,
    Dropped,
    /// The webhook is gone, nothing more can be sent.
    Halt,
}

impl Delivery {
    pub fn new(endpoint: Endpoint, policy: RetryPolicy) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(policy.timeout)
            .user_agent(concat!("Tubarr/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            endpoint,
            policy,
            paced_until: None,
        })
    }

    /// Sends every card from `queue` until it closes or the webhook turns out deleted.
    pub async fn run(mut self, mut queue: mpsc::Receiver<Card>) {
        while let Some(card) = queue.recv().await {
            if let Some(until) = self.paced_until.take() {
                tokio::time::sleep_until(until).await;
            }
            if let Outcome::Halt = self.deliver(&card).await {
                tracing::error!(
                    host = %self.endpoint.host(),
                    "Discord says the webhook no longer exists, cards stay off until notify.discord_webhook_file holds a new one and Tubarr restarts"
                );
                return;
            }
        }
    }

    async fn deliver(&mut self, card: &Card) -> Outcome {
        let host = self.endpoint.host();
        let event = card.event;
        for attempt in 1..=self.policy.tries {
            let last = attempt == self.policy.tries;
            let wait = match self.post(card).await {
                Ok(response) => {
                    let status = response.status();
                    let headers = response.headers().clone();
                    self.note_bucket(&headers);
                    if status.is_success() {
                        tracing::debug!(event, %host, attempt, "card sent");
                        return Outcome::Sent;
                    }
                    let body: serde_json::Value = response.json().await.unwrap_or_default();
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        let wait = retry_after(&headers, &body);
                        if wait > self.policy.max_wait {
                            tracing::warn!(event, %host, wait_secs = wait.as_secs_f64(), "rate limited for too long, dropping the card");
                            return Outcome::Dropped;
                        }
                        tracing::info!(event, %host, wait_secs = wait.as_secs_f64(), "rate limited, waiting");
                        wait
                    } else if status.is_server_error() {
                        tracing::warn!(event, %host, status = status.as_u16(), attempt, "card not sent, retrying");
                        self.backoff(attempt)
                    } else {
                        let code = body.get("code").and_then(serde_json::Value::as_u64);
                        if matches!(self.endpoint, Endpoint::Discord(_))
                            && code == Some(UNKNOWN_WEBHOOK)
                        {
                            return Outcome::Halt;
                        }
                        tracing::warn!(event, %host, status = status.as_u16(), code, "card refused, dropping it");
                        return Outcome::Dropped;
                    }
                }
                Err(e) => {
                    // Without the URL: it holds the webhook token.
                    let e = e.without_url();
                    tracing::warn!(event, %host, attempt, error = %e, "card not sent, retrying");
                    self.backoff(attempt)
                }
            };
            if !last {
                tokio::time::sleep(wait).await;
            }
        }
        tracing::warn!(
            event,
            %host,
            tries = self.policy.tries,
            "dropping a card that could not be sent"
        );
        Outcome::Dropped
    }

    async fn post(&self, card: &Card) -> Result<reqwest::Response, reqwest::Error> {
        match &self.endpoint {
            Endpoint::Discord(webhook) => {
                let mut url = webhook.clone();
                url.query_pairs_mut()
                    .append_pair("wait", "true")
                    .append_pair("with_components", "true");
                self.client.post(url).json(&card.to_discord()).send().await
            }
            Endpoint::CardRelay { url, token } => {
                self.client
                    .post(url.clone())
                    .bearer_auth(token.expose())
                    .json(card)
                    .send()
                    .await
            }
        }
    }

    fn backoff(&self, attempt: u32) -> Duration {
        self.policy.backoff * 2u32.saturating_pow(attempt - 1)
    }

    /// `X-RateLimit-Remaining: 0` means the next call would hit a 429, so wait for the reset.
    fn note_bucket(&mut self, headers: &HeaderMap) {
        let remaining = header_f64(headers, "x-ratelimit-remaining");
        let reset_after = header_f64(headers, "x-ratelimit-reset-after");
        if let (Some(r), Some(after)) = (remaining, reset_after)
            && r <= 0.0
        {
            self.paced_until = Some(Instant::now() + secs(after));
        }
    }
}

fn header_f64(headers: &HeaderMap, name: &str) -> Option<f64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

/// Discord's `retry_after` (seconds, fractional) from the body, else the `Retry-After` header,
/// else a second.
fn retry_after(headers: &HeaderMap, body: &serde_json::Value) -> Duration {
    body.get("retry_after")
        .and_then(serde_json::Value::as_f64)
        .or_else(|| header_f64(headers, "retry-after"))
        .map_or(Duration::from_secs(1), secs)
}

fn secs(value: f64) -> Duration {
    Duration::try_from_secs_f64(value.max(0.0)).unwrap_or(Duration::MAX)
}
