//! Discord cards: folding windows on paused time, and delivery against a fake Discord webhook
//! (rate limits, retries, a deleted webhook, the card relay, and no webhook token in the logs).

use std::{
    collections::VecDeque,
    io::Write,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::{sync::mpsc, task::JoinHandle, time::Instant};
use tubarr::{
    binaries::Tool,
    config::{Config, NotifyTarget},
    health::{Events, HealthEvent},
    notify::{
        Card, Delivery, Endpoint, Folder, Lookups, RetryPolicy, Tone,
        fold::{COOKIES_WINDOW, DIGEST_EVERY, GRABS_WINDOW},
    },
    resolver::{ErrorKind, Step},
    store::UnmatchedState,
};
use url::Url;

const VIDEO: &str = "0eD-GQCutAU";
const TOKEN: &str = "SECRET-webhook-token_123";

// ---------------------------------------------------------------- folding (paused time)

/// Lookups without a database, so paused time only moves when the folder waits on a timer.
#[derive(Default)]
struct FakeLookups {
    digests: Mutex<VecDeque<Vec<(UnmatchedState, i64)>>>,
}

impl Lookups for FakeLookups {
    async fn title(&self, video_id: &str) -> Option<String> {
        video_id
            .starts_with("known")
            .then(|| "Kabhi Main Kabhi Tum".to_owned())
    }

    async fn unmatched_waiting(&self) -> Vec<(UnmatchedState, i64)> {
        self.digests.lock().unwrap().pop_front().unwrap_or_default()
    }
}

struct Folding {
    events: Events,
    cards: mpsc::Receiver<Card>,
    task: JoinHandle<()>,
}

fn folding(lookups: FakeLookups) -> Folding {
    let events = Events::new();
    let (tx, cards) = mpsc::channel(32);
    let task = tokio::spawn(Folder::new(lookups, tx).run(events.subscribe()));
    Folding {
        events,
        cards,
        task,
    }
}

impl Folding {
    /// Lets the folder handle what was published, without moving the clock.
    async fn settle(&self) {
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
    }

    fn none_yet(&mut self) {
        assert!(self.cards.try_recv().is_err(), "no card expected yet");
    }

    /// The next card and how long the clock ran until it came.
    async fn next(&mut self) -> (Card, Duration) {
        let start = Instant::now();
        let card = self.cards.recv().await.expect("a card");
        (card, start.elapsed())
    }
}

fn cookies() -> HealthEvent {
    HealthEvent::CookiesUsed {
        video_id: VIDEO.to_owned(),
    }
}

#[tokio::test(start_paused = true)]
async fn cookies_fold_to_one_card_an_hour_with_a_count() {
    let mut f = folding(FakeLookups::default());
    f.events.publish(cookies());
    f.settle().await;
    let (first, waited) = f.next().await;
    assert_eq!(waited, Duration::ZERO);
    assert_eq!(first.event, "cookies_used");
    assert_eq!(first.tone, Tone::Warning);
    assert_eq!(first.status.as_deref(), Some("1 lookup just now"));

    tokio::time::advance(Duration::from_secs(10 * 60)).await;
    for _ in 0..3 {
        f.events.publish(cookies());
    }
    f.settle().await;
    f.none_yet();
    let (folded, waited) = f.next().await;
    assert_eq!(waited, COOKIES_WINDOW - Duration::from_secs(10 * 60));
    assert_eq!(
        folded.status.as_deref(),
        Some("3 more lookups in the last hour")
    );

    // A quiet hour later the next use is a card of its own again.
    tokio::time::advance(COOKIES_WINDOW).await;
    f.events.publish(cookies());
    f.settle().await;
    let (again, waited) = f.next().await;
    assert_eq!(waited, Duration::ZERO);
    assert_eq!(again.status.as_deref(), Some("1 lookup just now"));
    f.task.abort();
}

fn grab(i: usize) -> HealthEvent {
    let video_id = if i.is_multiple_of(2) {
        format!("known{i}")
    } else {
        format!("other{i}")
    };
    HealthEvent::GrabCompleted {
        video_id,
        release_name: format!("Kabhi.Main.Kabhi.Tum.S01E{i:02}.1080p.WEB-DL.AAC.H.264-Tubarr"),
        category: if i == 1 { "movies" } else { "tv" }.to_owned(),
    }
}

#[tokio::test(start_paused = true)]
async fn grabs_fold_per_ten_minutes_listing_ten() {
    let mut f = folding(FakeLookups::default());
    for i in 0..6 {
        f.events.publish(grab(i));
    }
    f.settle().await;
    tokio::time::advance(Duration::from_secs(60)).await;
    for i in 6..12 {
        f.events.publish(grab(i));
    }
    f.settle().await;
    f.none_yet();
    let (card, waited) = f.next().await;
    assert_eq!(waited, GRABS_WINDOW - Duration::from_secs(60));
    assert_eq!(card.event, "grabs");
    assert_eq!(card.tone, Tone::Info);
    assert_eq!(card.title, "12 releases sent to Sonarr or Radarr");
    let items = &card.lists[0].items;
    assert_eq!(items.len(), 11);
    assert_eq!(
        items[0],
        "Kabhi Main Kabhi Tum · TV · Kabhi.Main.Kabhi.Tum.S01E00.1080p.WEB-DL.AAC.H.264-Tubarr"
    );
    assert_eq!(
        items[1],
        "Film · Kabhi.Main.Kabhi.Tum.S01E01.1080p.WEB-DL.AAC.H.264-Tubarr"
    );
    assert_eq!(items[10], "and 2 more");

    // One grab alone is its own window.
    f.events.publish(grab(2));
    f.settle().await;
    let (card, waited) = f.next().await;
    assert_eq!(waited, GRABS_WINDOW);
    assert_eq!(card.title, "Release sent to Sonarr or Radarr");
    f.task.abort();
}

#[tokio::test(start_paused = true)]
async fn unmatched_digest_is_daily_and_skips_empty_days() {
    let lookups = FakeLookups::default();
    lookups.digests.lock().unwrap().extend([
        Vec::new(),
        vec![(UnmatchedState::Unsure, 5), (UnmatchedState::Pending, 2)],
    ]);
    let mut f = folding(lookups);
    let (card, waited) = f.next().await;
    assert_eq!(waited, 2 * DIGEST_EVERY);
    assert_eq!(card.event, "unmatched");
    assert_eq!(card.tone, Tone::Warning);
    assert_eq!(card.title, "7 videos waiting for a match");
    assert_eq!(
        card.lists[0].items,
        [
            "Not sure which episode or film it is: 5",
            "Waiting for video details from YouTube: 2"
        ]
    );
    f.task.abort();
}

#[tokio::test(start_paused = true)]
async fn health_events_become_cards_at_once() {
    let mut f = folding(FakeLookups::default());
    f.events.publish(HealthEvent::CanaryFailing {
        video_id: VIDEO.to_owned(),
        failures: 3,
        error: "YouTube asked for a bot check: ERROR: Sign in to confirm you're not a bot"
            .to_owned(),
        kind: ErrorKind::BotCheck,
        step: Some(Step::PoToken),
        forced_update: true,
    });
    f.events.publish(HealthEvent::BinaryUpdated {
        tool: Tool::YtDlp,
        from: Some("2026.08.19".to_owned()),
        to: "2026.10.10".to_owned(),
    });
    f.events.publish(HealthEvent::BinaryUpdateFailed {
        tool: Tool::YtDlp,
        error: "GitHub rate limit".to_owned(),
    });
    f.events.publish(HealthEvent::CanaryRecovered {
        video_id: VIDEO.to_owned(),
        step: Step::Plain,
        after_failures: 4,
    });
    f.settle().await;

    let failing = f.cards.try_recv().unwrap();
    assert_eq!(failing.tone, Tone::Failure);
    assert_eq!(failing.title, "YouTube lookups are failing");
    assert_eq!(failing.status.as_deref(), Some("3 failed checks in a row"));
    let field = |card: &Card, name: &str| {
        card.fields
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.value.clone())
            .unwrap_or_else(|| panic!("no field {name}"))
    };
    assert_eq!(
        field(&failing, "Last step tried"),
        "Lookup with a proof of origin token"
    );
    assert_eq!(field(&failing, "Why"), "YouTube asked for a sign-in check");
    assert!(field(&failing, "What Tubarr is doing").starts_with("Updating yt-dlp now"));

    let updated = f.cards.try_recv().unwrap();
    assert_eq!(
        (updated.tone, updated.title.as_str()),
        (Tone::Info, "yt-dlp updated")
    );
    assert_eq!(updated.status.as_deref(), Some("2026.08.19 → 2026.10.10"));

    let failed = f.cards.try_recv().unwrap();
    assert_eq!(
        (failed.tone, failed.title.as_str()),
        (Tone::Failure, "yt-dlp update failed")
    );

    let recovered = f.cards.try_recv().unwrap();
    assert_eq!(
        (recovered.tone, recovered.title.as_str()),
        (Tone::Success, "YouTube lookups work again")
    );
    assert_eq!(recovered.status.as_deref(), Some("after 4 failed checks"));
    f.task.abort();
}

/// No em or en dashes, no internal names in anything a reader sees.
#[tokio::test(start_paused = true)]
async fn card_text_is_plain() {
    let mut f = folding(FakeLookups::default());
    f.events.publish(HealthEvent::CanaryFailing {
        video_id: VIDEO.to_owned(),
        failures: 5,
        error: "e".to_owned(),
        kind: ErrorKind::RateLimited,
        step: None,
        forced_update: false,
    });
    f.events.publish(cookies());
    f.settle().await;
    for _ in 0..2 {
        let card = f.cards.try_recv().unwrap();
        let text = serde_json::to_string(&card).unwrap();
        for bad in [
            "\u{2014}",
            "\u{2013}",
            "bot check",
            "PoToken",
            "po_token",
            "canary lookup",
        ] {
            assert!(!text.contains(bad), "{bad:?} in {text}");
        }
    }
    f.task.abort();
}

// ---------------------------------------------------------------- delivery (fake Discord)

#[derive(Debug, Clone)]
struct Captured {
    path_and_query: String,
    authorization: Option<String>,
    body: Value,
}

/// Status, headers and JSON body of one canned answer.
type Reply = (StatusCode, Vec<(&'static str, String)>, Value);

#[derive(Clone, Default)]
struct Fake {
    captured: Arc<Mutex<Vec<Captured>>>,
    /// Answers in order. When empty, 200 with Discord's message JSON.
    replies: Arc<Mutex<VecDeque<Reply>>>,
}

impl Fake {
    fn reply(&self, status: StatusCode, headers: &[(&'static str, &str)], body: Value) {
        self.replies.lock().unwrap().push_back((
            status,
            headers.iter().map(|(k, v)| (*k, (*v).to_owned())).collect(),
            body,
        ));
    }

    fn captured(&self) -> Vec<Captured> {
        self.captured.lock().unwrap().clone()
    }

    async fn start(&self) -> String {
        let app = Router::new()
            .route("/{*path}", post(capture))
            .with_state(self.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    }
}

async fn capture(State(fake): State<Fake>, request: Request) -> Response {
    let path_and_query = request
        .uri()
        .path_and_query()
        .map(ToString::to_string)
        .unwrap_or_default();
    let authorization = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = request.into_body().collect().await.unwrap().to_bytes();
    fake.captured.lock().unwrap().push(Captured {
        path_and_query,
        authorization,
        body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    });
    let reply = fake.replies.lock().unwrap().pop_front();
    match reply {
        Some((status, headers, body)) => {
            let mut map = HeaderMap::new();
            for (k, v) in headers {
                map.insert(k, HeaderValue::from_str(&v).unwrap());
            }
            (status, map, Json(body)).into_response()
        }
        None => Json(json!({ "id": "1" })).into_response(),
    }
}

fn quick() -> RetryPolicy {
    RetryPolicy {
        tries: 3,
        backoff: Duration::from_millis(20),
        max_wait: Duration::from_secs(5),
        timeout: Duration::from_secs(5),
    }
}

fn webhook(base: &str) -> Url {
    Url::parse(&format!("{base}/api/webhooks/123456789/{TOKEN}")).unwrap()
}

fn card(n: u32) -> Card {
    Card::new("test", Tone::Info, format!("Card {n}"), 1_791_499_315).footer("test")
}

/// Runs a delivery task over `cards` and waits for it to finish.
async fn deliver(endpoint: Endpoint, policy: RetryPolicy, cards: Vec<Card>) {
    let (tx, rx) = mpsc::channel(32);
    let task = tokio::spawn(Delivery::new(endpoint, policy).unwrap().run(rx));
    for c in cards {
        tx.send(c).await.unwrap();
    }
    drop(tx);
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("delivery finishes")
        .unwrap();
}

#[tokio::test]
async fn canary_card_reaches_discord_in_house_shape() {
    let fake = Fake::default();
    let base = fake.start().await;
    let events = Events::new();
    let (tx, rx) = mpsc::channel(32);
    let folder = tokio::spawn(Folder::new(FakeLookups::default(), tx).run(events.subscribe()));
    let delivery = tokio::spawn(
        Delivery::new(Endpoint::Discord(webhook(&base)), quick())
            .unwrap()
            .run(rx),
    );
    events.publish(HealthEvent::CanaryFailing {
        video_id: VIDEO.to_owned(),
        failures: 3,
        error: "YouTube asked for a bot check: ERROR: [youtube] 0eD-GQCutAU: Sign in".to_owned(),
        kind: ErrorKind::BotCheck,
        step: Some(Step::Cookies),
        forced_update: false,
    });
    // Closing the channel ends the folder, which closes the queue and ends delivery.
    drop(events);
    tokio::time::timeout(Duration::from_secs(10), async {
        folder.await.unwrap();
        delivery.await.unwrap();
    })
    .await
    .unwrap();

    let got = fake.captured();
    assert_eq!(got.len(), 1);
    assert_eq!(
        got[0].path_and_query,
        format!("/api/webhooks/123456789/{TOKEN}?wait=true&with_components=true")
    );
    let body = &got[0].body;
    assert_eq!(body["flags"], 32768);
    assert_eq!(body["username"], "Tubarr");
    assert_eq!(body["allowed_mentions"], json!({ "parse": [] }));
    assert!(body.get("embeds").is_none() && body.get("content").is_none());
    let container = &body["components"][0];
    assert_eq!(container["type"], 17);
    assert_eq!(container["accent_color"], 0x7f1d1d);
    let parts = container["components"].as_array().unwrap();
    let head = parts[0]["content"].as_str().unwrap();
    assert!(
        head.starts_with("### 🔴 YouTube lookups are failing\n**3 failed checks in a row**\n"),
        "{head}"
    );
    assert!(
        head.contains("**Last step tried** · Lookup with the spare account's cookies"),
        "{head}"
    );
    assert!(
        head.contains("**Why** · YouTube asked for a sign\\-in check"),
        "{head}"
    );
    assert!(
        head.contains("**What Tubarr is doing** · yt\\-dlp was already updated in the last day"),
        "{head}"
    );
    // Outside text is escaped, so `[youtube]` cannot become a link.
    assert!(head.contains("\\[youtube\\]"), "{head}");
    let footer = parts.last().unwrap()["content"].as_str().unwrap();
    let re = regex::Regex::new(r"^-# Tubarr · canary · <t:\d{10}:f>$").unwrap();
    assert!(re.is_match(footer), "{footer}");
}

#[tokio::test]
async fn rate_limits_are_waited_out() {
    let fake = Fake::default();
    let base = fake.start().await;
    fake.reply(
        StatusCode::TOO_MANY_REQUESTS,
        &[("retry-after", "1")],
        json!({ "message": "You are being rate limited.", "retry_after": 0.3, "global": false }),
    );
    // A send that empties the bucket makes the next card wait for the reset.
    fake.reply(
        StatusCode::OK,
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset-after", "0.4"),
        ],
        json!({ "id": "1" }),
    );
    let start = std::time::Instant::now();
    deliver(
        Endpoint::Discord(webhook(&base)),
        quick(),
        vec![card(1), card(2)],
    )
    .await;
    let elapsed = start.elapsed();
    let got = fake.captured();
    assert_eq!(got.len(), 3);
    assert_eq!(got[0].body, got[1].body);
    assert!(elapsed >= Duration::from_millis(700), "{elapsed:?}");
    assert!(
        elapsed < Duration::from_secs(3),
        "body retry_after wins over the header: {elapsed:?}"
    );
}

#[tokio::test]
async fn a_long_rate_limit_drops_the_card() {
    let fake = Fake::default();
    let base = fake.start().await;
    fake.reply(
        StatusCode::TOO_MANY_REQUESTS,
        &[],
        json!({ "retry_after": 120.0, "global": true }),
    );
    deliver(
        Endpoint::Discord(webhook(&base)),
        quick(),
        vec![card(1), card(2)],
    )
    .await;
    let got = fake.captured();
    assert_eq!(got.len(), 2);
    assert_ne!(got[0].body, got[1].body);
}

#[tokio::test]
async fn server_errors_retry_with_backoff_then_drop() {
    let fake = Fake::default();
    let base = fake.start().await;
    // Card 1: two failures, then sent. Card 2: fails every try and is dropped. Card 3: sent.
    for _ in 0..2 {
        fake.reply(StatusCode::BAD_GATEWAY, &[], json!({}));
    }
    fake.reply(StatusCode::OK, &[], json!({ "id": "1" }));
    for _ in 0..3 {
        fake.reply(StatusCode::INTERNAL_SERVER_ERROR, &[], json!({}));
    }
    let start = std::time::Instant::now();
    deliver(
        Endpoint::Discord(webhook(&base)),
        quick(),
        vec![card(1), card(2), card(3)],
    )
    .await;
    let titles: Vec<String> = fake
        .captured()
        .iter()
        .map(|c| {
            c.body["components"][0]["components"][0]["content"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    let n = |i: u32| {
        titles
            .iter()
            .filter(|t| t.ends_with(&format!("Card {i}")))
            .count()
    };
    assert_eq!((n(1), n(2), n(3)), (3, 3, 1));
    // 20 + 40 ms for card 1, 20 + 40 ms for card 2, no wait after the last try.
    assert!(start.elapsed() >= Duration::from_millis(120));
}

#[tokio::test]
async fn other_client_errors_drop_without_retry() {
    let fake = Fake::default();
    let base = fake.start().await;
    fake.reply(
        StatusCode::BAD_REQUEST,
        &[],
        json!({ "code": 50035, "message": "Invalid Form Body" }),
    );
    deliver(
        Endpoint::Discord(webhook(&base)),
        quick(),
        vec![card(1), card(2)],
    )
    .await;
    assert_eq!(fake.captured().len(), 2);
}

#[tokio::test]
async fn a_deleted_webhook_stops_delivery() {
    let fake = Fake::default();
    let base = fake.start().await;
    fake.reply(
        StatusCode::NOT_FOUND,
        &[],
        json!({ "code": 10015, "message": "Unknown Webhook" }),
    );
    let (tx, rx) = mpsc::channel(32);
    let task = tokio::spawn(
        Delivery::new(Endpoint::Discord(webhook(&base)), quick())
            .unwrap()
            .run(rx),
    );
    tx.send(card(1)).await.unwrap();
    // The task ends on its own while the queue is still open.
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("delivery stops")
        .unwrap();
    assert!(tx.send(card(2)).await.is_err());
    assert_eq!(fake.captured().len(), 1);
}

#[tokio::test]
async fn card_relay_gets_the_spec_with_the_token() {
    let fake = Fake::default();
    let base = fake.start().await;
    let token = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(token.path(), "relay-token\n").unwrap();
    let config = Config::from_toml(&format!(
        "public_url = \"http://tubarr:8480\"\n[notify]\ncard_relay_url = \"{base}/tubarr\"\ncard_relay_token_file = {:?}\n",
        token.path()
    ))
    .unwrap();
    let target = config.notify.as_ref().unwrap();
    assert!(matches!(target, NotifyTarget::CardRelay { .. }));
    let card = Card::new(
        "grabs",
        Tone::Info,
        "Release sent to Sonarr or Radarr",
        1_791_499_315,
    )
    .list(
        "Releases",
        vec!["Theri · Film · Theri.2016.1080p".to_owned()],
    )
    .footer("grabs");
    deliver(Endpoint::from_target(target).unwrap(), quick(), vec![card]).await;
    let got = fake.captured();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].path_and_query, "/tubarr");
    assert_eq!(got[0].authorization.as_deref(), Some("Bearer relay-token"));
    assert_eq!(
        got[0].body,
        json!({
            "source": "tubarr", "event": "grabs", "tone": "info",
            "title": "Release sent to Sonarr or Radarr",
            "lists": [{ "name": "Releases", "items": ["Theri · Film · Theri.2016.1080p"] }],
            "footer": "Tubarr · grabs", "at": 1_791_499_315_u64, "username": "Tubarr"
        })
    );
}

/// Log output captured for one test.
#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Every log line of this test binary. A global subscriber, since a scoped one misses callsites
/// that other tests running in parallel registered first.
fn logs() -> &'static LogBuf {
    static LOGS: std::sync::OnceLock<LogBuf> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = LogBuf::default();
        let writer = logs.clone();
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .init();
        logs
    })
}

#[tokio::test]
async fn the_webhook_token_never_reaches_the_logs() {
    let logs = logs();

    // A server that fails, then a port nobody listens on (a connection error names the URL).
    let fake = Fake::default();
    let base = fake.start().await;
    for _ in 0..3 {
        fake.reply(StatusCode::SERVICE_UNAVAILABLE, &[], json!({}));
    }
    fake.reply(StatusCode::NOT_FOUND, &[], json!({ "code": 10015 }));
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", l.local_addr().unwrap())
    };
    deliver(Endpoint::Discord(webhook(&closed)), quick(), vec![card(1)]).await;
    deliver(
        Endpoint::Discord(webhook(&base)),
        quick(),
        vec![card(2), card(3)],
    )
    .await;

    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("card not sent, retrying"), "{text}");
    assert!(
        text.contains("dropping a card that could not be sent"),
        "{text}"
    );
    assert!(text.contains("webhook no longer exists"), "{text}");
    assert!(text.contains("host=127.0.0.1"), "{text}");
    assert!(!text.contains(TOKEN), "{text}");
    assert!(!text.contains("123456789"), "{text}");
    assert!(!format!("{:?}", Endpoint::Discord(webhook(&base))).contains(TOKEN));
}
