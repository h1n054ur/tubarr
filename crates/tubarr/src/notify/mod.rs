//! Discord cards for what a person should know: the canary failing and recovering, lookups using
//! the spare account's cookies, yt-dlp updates, grabs, and unmatched videos.
//!
//! Events come from the health channel ([`crate::health::Events`]), [`fold::Folder`] turns them
//! into [`Card`]s, and [`deliver::Delivery`] sends them from a small queue. Nothing here ever
//! blocks an event source: the channel is a broadcast, and a full queue drops the card.
//!
//! Two ways out, set in `[notify]`:
//! - `discord_webhook_file`: Tubarr builds the house card (Components V2) and posts it to the
//!   webhook with `?wait=true&with_components=true`.
//! - `card_relay_url` and `card_relay_token_file`: Tubarr posts the card spec as JSON to any HTTP
//!   endpoint with `Authorization: Bearer <token>`, and the receiver decides what to do with it.
//!   The body uses the names `card.mjs --json` reads, plus `source` and `event` to route on:
//!
//! ```json
//! {"source": "tubarr", "event": "canary_failing", "tone": "failure",
//!  "title": "YouTube lookups are failing", "status": "3 failed checks in a row",
//!  "text": ["The hourly test lookup of a known good video keeps failing, so plays will likely fail too."],
//!  "fields": [{"name": "Why", "value": "YouTube asked for a sign-in check"}],
//!  "lists": [{"name": "Releases", "items": ["..."]}],
//!  "footer": "Tubarr · canary", "at": 1791499315, "username": "Tubarr"}
//! ```
//!
//! `event` is one of `canary_failing`, `canary_recovered`, `cookies_used`, `binary_updated`,
//! `binary_update_failed`, `grabs` and `unmatched`. `status`, `text`, `fields` and `lists` are left
//! out when empty. Values from outside (release names, errors) are raw: the receiver escapes them.

pub mod card;
pub mod cards;
pub mod deliver;
pub mod fold;

pub use card::{Card, Tone};
pub use deliver::{Delivery, Endpoint, RetryPolicy};
pub use fold::{Folder, Lookups};

use tokio::sync::mpsc;

use crate::{config::NotifyTarget, health::Events, store::Db};

/// Cards waiting to be sent. Bursts fold before they get here, so this only fills while the
/// endpoint is down or rate limiting.
pub const QUEUE: usize = 32;

/// Starts the folder and the delivery task. Subscribes before returning, so no event published
/// after this call is missed.
pub fn spawn(target: &NotifyTarget, events: &Events, db: Db) -> Result<(), reqwest::Error> {
    let Some(endpoint) = Endpoint::from_target(target) else {
        tracing::error!("the Discord webhook URL cannot be parsed, cards are off");
        return Ok(());
    };
    let delivery = Delivery::new(endpoint, RetryPolicy::default())?;
    let (tx, rx) = mpsc::channel(QUEUE);
    let folder = Folder::new(db, tx);
    tokio::spawn(folder.run(events.subscribe()));
    tokio::spawn(delivery.run(rx));
    tracing::info!(
        to = match target {
            NotifyTarget::Discord { .. } => "discord",
            NotifyTarget::CardRelay { .. } => "card relay",
        },
        "cards on"
    );
    Ok(())
}
