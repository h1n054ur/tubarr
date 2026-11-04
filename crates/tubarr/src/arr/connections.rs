//! Sonarr and Radarr connections saved from the Settings page, the connection test, and the
//! wiring check that tells a user what is still missing in each arr.
//!
//! The config file's `[sonarr]` and `[radarr]` win: a saved connection is used only for an arr the
//! file does not set. Everything that talks to an arr reads the connection in use from one shared
//! [`ArrConnections`] each time, so saving or removing one in Settings takes effect at once.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use url::Url;

use super::{App, ArrClient, ArrError, Tag};
use crate::{
    config::{ArrConnection, Config, ConnectionSource, Secret, check_http_url},
    store::{Db, SavedArr, StoreError},
};

/// The Sonarr and Radarr connections in use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InUse {
    pub sonarr: Option<ArrConnection>,
    pub radarr: Option<ArrConnection>,
}

impl InUse {
    pub fn get(&self, app: App) -> Option<&ArrConnection> {
        match app {
            App::Sonarr => self.sonarr.as_ref(),
            App::Radarr => self.radarr.as_ref(),
        }
    }

    fn slot(&mut self, app: App) -> &mut Option<ArrConnection> {
        match app {
            App::Sonarr => &mut self.sonarr,
            App::Radarr => &mut self.radarr,
        }
    }
}

/// The connections in use, shared by the arr sync, the release push, the matcher, the drafts,
/// the hooks and the admin API. Clones share one value: a change made through one is what the
/// others read next. Background loops can [`ArrConnections::watch`] for changes.
#[derive(Debug, Clone)]
pub struct ArrConnections {
    tx: Arc<watch::Sender<InUse>>,
}

impl ArrConnections {
    pub fn new(sonarr: Option<ArrConnection>, radarr: Option<ArrConnection>) -> Self {
        Self {
            tx: Arc::new(watch::Sender::new(InUse { sonarr, radarr })),
        }
    }

    /// The config file's `[sonarr]` and `[radarr]`.
    pub fn from_config(config: &Config) -> Self {
        Self::new(config.sonarr.clone(), config.radarr.clone())
    }

    /// Fills in each arr the config file does not set from the connection saved in `db`. A saved
    /// row whose URL no longer parses is skipped with a warning, so a bad row never stops the start.
    pub async fn load_saved(&self, db: &Db) -> Result<(), StoreError> {
        for app in [App::Sonarr, App::Radarr] {
            if self.get(app).is_some() {
                continue;
            }
            if let Some(saved) = db.saved_arr(app.config_key()).await? {
                match saved_connection(&saved) {
                    Some(conn) => {
                        self.set(app, Some(conn));
                    }
                    None => tracing::warn!(%app, "the saved connection has a bad URL: ignored"),
                }
            }
        }
        Ok(())
    }

    /// Both connections as they are now.
    pub fn current(&self) -> InUse {
        self.tx.borrow().clone()
    }

    pub fn get(&self, app: App) -> Option<ArrConnection> {
        self.tx.borrow().get(app).cloned()
    }

    pub fn sonarr(&self) -> Option<ArrConnection> {
        self.get(App::Sonarr)
    }

    pub fn radarr(&self) -> Option<ArrConnection> {
        self.get(App::Radarr)
    }

    /// True when the config file sets `app`: such a connection never changes while running.
    pub fn from_file(&self, app: App) -> bool {
        self.tx
            .borrow()
            .get(app)
            .is_some_and(|c| c.source == ConnectionSource::Config)
    }

    /// Puts `connection` in use for `app`, or none. A connection from the config file is kept as
    /// it is. True when something changed; watchers are woken only then.
    pub fn set(&self, app: App, connection: Option<ArrConnection>) -> bool {
        self.tx.send_if_modified(|in_use| {
            let slot = in_use.slot(app);
            if slot
                .as_ref()
                .is_some_and(|c| c.source == ConnectionSource::Config)
                || *slot == connection
            {
                return false;
            }
            *slot = connection;
            true
        })
    }

    /// A receiver that sees every change made with [`ArrConnections::set`].
    pub fn watch(&self) -> watch::Receiver<InUse> {
        self.tx.subscribe()
    }
}

/// A saved row as a connection, or `None` when its URL does not parse.
pub fn saved_connection(saved: &SavedArr) -> Option<ArrConnection> {
    let url = Url::parse(&saved.url).ok()?;
    Some(ArrConnection {
        url: check_http_url("url", url).ok()?,
        api_key: Secret::new(saved.api_key.clone()),
        source: ConnectionSource::Saved,
    })
}

/// Checks a URL typed in the Settings page. The message reads as a sentence about the URL.
pub fn parse_url(text: &str) -> Result<Url, String> {
    let url = Url::parse(text.trim())
        .map_err(|_| "The URL is not valid, e.g. http://sonarr:8989".to_owned())?;
    check_http_url("url", url).map_err(|e| match e {
        crate::config::ConfigError::Invalid { reason, .. } => format!("The URL {reason}"),
        other => other.to_string(),
    })
}

/// What `GET /api/v3/system/status` answers, the fields Tubarr reads.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SystemStatus {
    #[serde(default)]
    app_name: Option<String>,
    #[serde(default)]
    version: Option<String>,
}

/// Why a connection test failed.
#[derive(Debug, thiserror::Error)]
pub enum TestError {
    #[error(transparent)]
    Arr(#[from] ArrError),
    #[error("that address answers as {found}, not {app}")]
    WrongApp { app: App, found: String },
}

/// Asks the arr for its system status: proves the address and the key, and that the address is
/// the right arr. Returns the arr's version.
pub async fn test(app: App, connection: &ArrConnection) -> Result<String, TestError> {
    let client = ArrClient::new(app, connection)?;
    let status: SystemStatus = client.get("api/v3/system/status", &[]).await?;
    if let Some(found) = status
        .app_name
        .filter(|n| !n.eq_ignore_ascii_case(&app.to_string()))
    {
        return Err(TestError::WrongApp { app, found });
    }
    Ok(status.version.unwrap_or_else(|| "unknown".to_owned()))
}

/// What the wiring check expects to find in the arr, from the running config.
#[derive(Debug, Clone)]
pub struct Expected {
    pub public_url: Url,
    pub tag: String,
    pub indexer: String,
    pub download_client: String,
    /// The Sonarr webhook is required only with `[hooks] sonarr_token_file` set.
    pub webhook: bool,
}

impl Expected {
    pub fn from_config(config: &Config) -> Self {
        Self {
            public_url: config.public_url.clone(),
            tag: config.arr.tag.clone(),
            indexer: config.arr.indexer.clone(),
            download_client: config.arr.download_client.clone(),
            webhook: config.hooks.sonarr_token.is_some(),
        }
    }
}

/// One line of the wiring check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WiringItem {
    pub key: &'static str,
    pub label: String,
    pub state: WiringState,
    /// What was found, or what to add.
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WiringState {
    Ok,
    Missing,
    /// Missing, but only needed for a feature that is off.
    Optional,
}

/// A `{"name", "value"}` entry of an indexer's, client's or connection's settings.
#[derive(Debug, Deserialize)]
struct Field {
    name: String,
    #[serde(default)]
    value: serde_json::Value,
}

/// An indexer, download client or connection (notification) as the arrs list them.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Provider {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    implementation: Option<String>,
    #[serde(default)]
    enable: Option<bool>,
    #[serde(default)]
    tags: Vec<i64>,
    #[serde(default)]
    fields: Vec<Field>,
}

impl Provider {
    fn named(&self, name: &str) -> bool {
        self.name
            .as_deref()
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    }

    fn is(&self, implementation: &str) -> bool {
        self.implementation
            .as_deref()
            .is_some_and(|i| i.eq_ignore_ascii_case(implementation))
    }

    fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|f| f.name == name)
            .and_then(|f| f.value.as_str())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MediaManagement {
    #[serde(default)]
    import_extra_files: bool,
    #[serde(default)]
    extra_file_extensions: Option<String>,
}

/// Reads the arr's tags, indexers, download clients, media management and (Sonarr) connections
/// and says, per item, whether Tubarr's part is in place. Read only: nothing is created.
pub async fn wiring(
    app: App,
    connection: &ArrConnection,
    expected: &Expected,
) -> Result<Vec<WiringItem>, ArrError> {
    let client = ArrClient::new(app, connection)?;
    let tags: Vec<Tag> = client.get("api/v3/tag", &[]).await?;
    let indexers: Vec<Provider> = client.get("api/v3/indexer", &[]).await?;
    let clients: Vec<Provider> = client.get("api/v3/downloadclient", &[]).await?;
    let media: MediaManagement = client.get("api/v3/config/mediamanagement", &[]).await?;
    let tag_id = tags
        .iter()
        .find(|t| {
            t.label
                .as_deref()
                .is_some_and(|l| l.eq_ignore_ascii_case(&expected.tag))
        })
        .map(|t| t.id);
    let tag = &expected.tag;
    let url = &expected.public_url;
    let mut items = vec![WiringItem {
        key: "tag",
        label: format!("Tag \"{tag}\""),
        state: ok_if(tag_id.is_some()),
        detail: match tag_id {
            Some(_) => format!(
                "Found. Put it on every {} Tubarr should serve.",
                app.titles()
            ),
            None => format!(
                "Add the tag \"{tag}\" in {app} under Settings > Tags, and put it on the {} Tubarr should serve.",
                app.titles()
            ),
        },
    }];

    let name = &expected.indexer;
    let add_indexer = format!(
        "Add a Newznab indexer named \"{name}\" in {app} under Settings > Indexers: URL {url}, API path /api, Tubarr's API key, tag \"{tag}\"."
    );
    let indexer = indexers.iter().find(|i| i.named(name));
    items.push(WiringItem {
        key: "indexer",
        label: format!("Indexer \"{name}\""),
        state: ok_if(indexer.is_some_and(|i| i.is("Newznab") && has_tag(i, tag_id))),
        detail: match indexer {
            None => add_indexer,
            Some(i) if !i.is("Newznab") => {
                format!("\"{name}\" is not a Newznab indexer. {add_indexer}")
            }
            Some(i) if !has_tag(i, tag_id) => {
                format!("Put the tag \"{tag}\" on the \"{name}\" indexer.")
            }
            Some(i) => format!(
                "Found, pointing at {}.",
                i.field("baseUrl")
                    .unwrap_or("an address Tubarr cannot read")
            ),
        },
    });

    let name = &expected.download_client;
    let add_client = format!(
        "Add a SABnzbd download client named \"{name}\" in {app} under Settings > Download Clients: host and port of {url}, Tubarr's API key, tag \"{tag}\"."
    );
    let download = clients.iter().find(|c| c.named(name));
    items.push(WiringItem {
        key: "download_client",
        label: format!("Download client \"{name}\""),
        state: ok_if(
            download.is_some_and(|c| {
                c.is("Sabnzbd") && c.enable.unwrap_or(false) && has_tag(c, tag_id)
            }),
        ),
        detail: match download {
            None => add_client,
            Some(c) if !c.is("Sabnzbd") => {
                format!("\"{name}\" is not a SABnzbd client. {add_client}")
            }
            Some(c) if !c.enable.unwrap_or(false) => {
                format!("Turn on the \"{name}\" download client: every push is sent to it.")
            }
            Some(c) if !has_tag(c, tag_id) => {
                format!("Put the tag \"{tag}\" on the \"{name}\" download client.")
            }
            Some(_) => "Found and enabled.".to_owned(),
        },
    });

    let srt = media.extra_file_extensions.as_deref().is_some_and(|list| {
        list.split(',')
            .any(|e| e.trim().trim_start_matches('.').eq_ignore_ascii_case("srt"))
    });
    items.push(WiringItem {
        key: "extra_files",
        label: "Import Extra Files with srt".to_owned(),
        state: ok_if(media.import_extra_files && srt),
        detail: if media.import_extra_files && srt {
            "On, with srt in the list.".to_owned()
        } else {
            format!(
                "Turn on Import Extra Files in {app} under Settings > Media Management and add srt to its list, or captions are not imported."
            )
        },
    });

    if app == App::Sonarr {
        let notifications: Vec<Provider> = client.get("api/v3/notification", &[]).await?;
        let found = notifications
            .iter()
            .any(|n| n.is("Webhook") && n.field("url").is_some_and(|u| u.contains("hooks/sonarr")));
        items.push(WiringItem {
            key: "webhook",
            label: "Webhook to Tubarr".to_owned(),
            state: match (found, expected.webhook) {
                (true, _) => WiringState::Ok,
                (false, true) => WiringState::Missing,
                (false, false) => WiringState::Optional,
            },
            detail: if found {
                "Found.".to_owned()
            } else {
                let add = format!(
                    "add a Webhook in Sonarr under Settings > Connect with URL {url}hooks/sonarr, method POST, tag \"{tag}\" and the token from hooks.sonarr_token_file."
                );
                if expected.webhook {
                    format!("For audio only versions and episode images, {add}")
                } else {
                    format!(
                        "Needed only for audio only versions and episode images. To use them, set hooks.sonarr_token_file and {add}"
                    )
                }
            },
        });
    }
    Ok(items)
}

fn ok_if(ok: bool) -> WiringState {
    if ok {
        WiringState::Ok
    } else {
        WiringState::Missing
    }
}

/// True when the provider carries the Tubarr tag. With no such tag in the arr, the tag item
/// already says what to add, so this does not report it twice.
fn has_tag(provider: &Provider, tag_id: Option<i64>) -> bool {
    tag_id.is_none_or(|id| provider.tags.contains(&id))
}
