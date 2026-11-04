//! Loading and checking the TOML config file.
//!
//! The file is parsed as written, then checked and turned into a [`Config`].
//! Secrets are never written in the file: keys ending in `_file` name a file holding the value,
//! read once at start and kept in a [`Secret`] that never prints.

use std::{
    collections::BTreeMap,
    fmt,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Deserialize;
use url::Url;

use crate::{
    arr::RootFolderDefaults,
    auth::{AuthConfig, AuthFile},
    channels::{self, ChannelInfo, Channels},
    drafts::{DraftSettings, Drafts},
    matcher::FilmSettings,
    play::{MAX_HEAD_BYTES, PlayConfig},
    ratelimit::RateLimitConfig,
    resolver::{self, CachePolicy},
    store::BackupPolicy,
};

/// The port Tubarr listens on when the config does not say otherwise.
pub const DEFAULT_LISTEN: &str = "0.0.0.0:8480";

/// Stream addresses from YouTube expire after about 6 hours, so a cached lookup must not outlive that.
const MAX_CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// A lookup normally takes seconds. Anything near this is stuck.
const MAX_LOOKUP_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// A longer pause would look like a dead service rather than a slow one.
const MAX_THROTTLE_PAUSE: Duration = Duration::from_secs(24 * 60 * 60);

/// Reading every tagged title more often than this only loads Sonarr and Radarr.
const MIN_SYNC_INTERVAL: Duration = Duration::from_secs(60);
const MIN_PUSH_INTERVAL: Duration = Duration::from_secs(1);
/// A cache smaller than a few 1080p fragments (about 3 MB each) would refetch on every seek.
const MIN_FRAGMENT_CACHE: u64 = 8 * 1024 * 1024;

/// The box Tubarr runs on has a few GB of memory shared with the *arr apps.
const MAX_FRAGMENT_CACHE: u64 = 1024 * 1024 * 1024;
/// Each build reads up to 16 moofs of each source file at once, so eight builds already hold some
/// 250 requests open at googlevideo.
const MAX_BUILDS: u32 = 8;

/// Smaller reads would need a second request for the index of almost every file.
const MIN_PROBE: u64 = 4 * 1024;

/// Sonarr and Radarr wait on the grab reply, and captions are written before it.
const MAX_SUBTITLE_TIMEOUT: Duration = Duration::from_secs(60);

/// A canary more often than this spends lookups that plays and grabs need.
const MIN_CANARY_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// Tubarr serves 1080p at most, so no release is named higher.
pub(crate) const MAX_ASSUMED_HEIGHT: u32 = 1080;

/// Name of the SQLite file inside `data_dir`.
const DATABASE_FILE: &str = "tubarr.db";

/// The checked settings Tubarr runs with.
#[derive(Debug, Clone)]
pub struct Config {
    /// Address the HTTP server binds to.
    pub listen: SocketAddr,
    /// Base URL Jellyfin uses to reach Tubarr, written into every `.strm` file. Always ends in `/`.
    pub public_url: Url,
    /// Folder for the SQLite database. The binaries have their own `binaries.bin_dir`.
    pub data_dir: PathBuf,
    /// Absolute folder where grabs write their `.strm` and `.srt` files. Reported to Sonarr and
    /// Radarr as SABnzbd's `complete_dir`, so it must be the same path inside their containers.
    pub complete_dir: PathBuf,
    /// How long a resolved stream lookup stays in the cache.
    pub cache_ttl: Duration,
    /// A yt-dlp run is killed after this long.
    pub lookup_timeout: Duration,
    pub rate_limit: RateLimit,
    pub binaries: Binaries,
    pub watcher: Watcher,
    pub play: Play,
    pub subtitles: Subtitles,
    pub sonarr: Option<ArrConnection>,
    pub radarr: Option<ArrConnection>,
    pub arr: ArrSettings,
    pub films: FilmSettings,
    pub drafts: Drafts,
    /// What Tubarr knows about each official channel (`[channels]`).
    pub channels: Channels,
    /// Root folders by language for titles whose channel sets none (`[root_folders]`).
    pub root_folders: RootFolderDefaults,
    pub fallback: Fallback,
    pub canary: Canary,
    pub matcher: Matcher,
    pub activity: ActivitySettings,
    pub newznab: Newznab,
    /// The key Sonarr and Radarr send as `apikey=` on `/api`. Without one, `/api` refuses every
    /// request rather than run open.
    pub api_key: Option<Secret>,
    /// Where Discord cards go. Off when `[notify]` names nothing.
    pub notify: Option<NotifyTarget>,
    /// How people sign in to the web UI.
    pub auth: AuthConfig,
    /// The Sonarr webhook that keeps audio only versions in step (`[hooks]`).
    pub hooks: Hooks,
    /// Jellyfin, which gets each audio only file merged into its episode as a version
    /// (`[jellyfin]`). Off when not set.
    pub jellyfin: Option<JellyfinConfig>,
    /// The database copy taken before migrations (`[backup]`).
    pub backup: BackupPolicy,
    /// The Prometheus endpoint (`[metrics]`).
    pub metrics: MetricsConfig,
}

/// The `[metrics]` section as written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct MetricsFile {
    /// Serves `GET /metrics`. Off by default.
    enabled: bool,
    /// A file holding the token a scraper sends as `Authorization: Bearer`. Without it the API key
    /// is the token, and with neither only `auth.play_allowed_networks` may scrape.
    token_file: Option<PathBuf>,
}

/// The Prometheus endpoint, checked.
#[derive(Debug, Clone, Default)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub token: Option<Secret>,
}

/// The `[jellyfin]` section as written.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
struct JellyfinFile {
    url: Url,
    /// A file holding a Jellyfin API key (Dashboard, API Keys).
    api_key_file: PathBuf,
    /// The user Tubarr asks for an episode's playback info as. Without it, the first
    /// administrator.
    user_id: Option<String>,
    /// `[Tubarr path, Jellyfin path]` prefix pairs for the media library, when Jellyfin mounts it
    /// elsewhere. A path under none of them is the same in both.
    #[serde(default)]
    library_paths: Vec<(PathBuf, PathBuf)>,
    /// The wait before the second look for an episode Jellyfin has not scanned yet. Each further
    /// wait doubles.
    #[serde(default = "default_merge_retry", with = "humantime_serde")]
    merge_retry: Duration,
    /// Looks for the episode before the merge is given up on. With the 30s default, 7 looks span
    /// about 30 minutes.
    #[serde(default = "default_merge_attempts")]
    merge_attempts: u32,
}

fn default_merge_retry() -> Duration {
    Duration::from_secs(30)
}

fn default_merge_attempts() -> u32 {
    7
}

/// Jellyfin, checked.
#[derive(Debug, Clone)]
pub struct JellyfinConfig {
    pub url: Url,
    pub api_key: Secret,
    pub user_id: Option<String>,
    pub library_paths: Vec<(PathBuf, PathBuf)>,
    pub merge_retry: Duration,
    pub merge_attempts: u32,
}

/// The `[hooks]` section as written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct HooksFile {
    /// A file holding the token Sonarr sends to `/hooks/sonarr` as `Authorization: Bearer`.
    sonarr_token_file: Option<PathBuf>,
    /// `[sonarr path, Tubarr path]` prefix pairs for the media library.
    library_paths: Vec<(PathBuf, PathBuf)>,
    /// Where episode thumbnails come from. Only tests point this elsewhere.
    thumbnail_url: Option<Url>,
}

/// The Sonarr webhook, checked.
#[derive(Debug, Clone)]
pub struct Hooks {
    /// Without it `/hooks/sonarr` refuses every request.
    pub sonarr_token: Option<Secret>,
    /// How a path Sonarr reports maps to the same file in Tubarr's container: the first pair whose
    /// Sonarr prefix holds the path wins. A path under none of them is never touched.
    pub library_paths: Vec<(PathBuf, PathBuf)>,
    /// YouTube's thumbnail host, `https://i.ytimg.com/`.
    pub thumbnail_url: Url,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            sonarr_token: None,
            library_paths: Vec::new(),
            thumbnail_url: Url::parse("https://i.ytimg.com/")
                .expect("default thumbnail URL is valid"),
        }
    }
}

/// The `[notify]` section as written: a Discord webhook, or an HTTP JSON webhook relay.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Notify {
    /// A file holding a Discord channel webhook URL. The URL holds a token, so it is a secret.
    pub discord_webhook_file: Option<PathBuf>,
    /// The card relay endpoint for Tubarr, e.g. `http://192.0.2.10:9940/tubarr`.
    pub card_relay_url: Option<Url>,
    /// A file holding the token sent to the relay as `Authorization: Bearer`.
    pub card_relay_token_file: Option<PathBuf>,
}

/// Where cards are delivered, checked.
#[derive(Debug, Clone)]
pub enum NotifyTarget {
    /// Tubarr builds the card and posts it to this webhook URL.
    Discord { webhook: Secret },
    /// Tubarr posts the card spec and the relay builds and sends the card.
    CardRelay { url: Url, token: Secret },
}

/// What a lookup tries after YouTube answers a plain one with a 429, a bot check or a sign-in
/// request. Each step is off until configured, and they run in this order.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Fallback {
    /// A bgutil PO token HTTP server, e.g. `http://bgutil:4416`. yt-dlp asks it for tokens through
    /// the bgutil plugin, which must be installed (see `plugin_dir`).
    pub po_token_url: Option<Url>,
    /// Folder passed to yt-dlp as `--plugin-dirs` on the PO token step. Put the release's
    /// `bgutil-ytdlp-pot-provider.zip` in it, the same version as the server.
    pub plugin_dir: Option<PathBuf>,
    /// A Netscape cookies file of a spare YouTube account, the last resort. It is never written:
    /// each lookup gets a temporary copy, since yt-dlp writes cookies back.
    pub cookies_file: Option<PathBuf>,
    /// After a fallback step works, lookups start there for this long, then try plain again.
    #[serde(with = "humantime_serde")]
    pub sticky_for: Duration,
    /// A fallback step's answer counts only with H.264 video at least this tall. Plain answers
    /// are taken as they are.
    pub min_height: u32,
}

impl Default for Fallback {
    fn default() -> Self {
        Self {
            po_token_url: None,
            plugin_dir: None,
            cookies_file: None,
            sticky_for: resolver::Fallback::default().sticky_for,
            min_height: resolver::Fallback::default().min_height,
        }
    }
}

/// A lookup of a known good video every so often, so trouble shows before a play fails.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Canary {
    /// The video to look up. No canary runs without one.
    pub video_id: Option<String>,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    /// Up to this much random time is added to each wait.
    #[serde(with = "humantime_serde")]
    pub jitter: Duration,
}

impl Default for Canary {
    fn default() -> Self {
        Self {
            video_id: None,
            interval: Duration::from_secs(60 * 60),
            jitter: Duration::from_secs(10 * 60),
        }
    }
}

/// How `/play` relays streams.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Play {
    /// Bytes of YouTube fragments kept in memory, so the small back-and-forth reads ffmpeg makes
    /// while probing and seeking are not fetched again.
    pub fragment_cache_bytes: u64,
    /// Bytes read from the start of each YouTube file to find its moov and segment index.
    pub probe_bytes: u64,
    /// Absolute folder for the sample tables read from each video's fragments, a few hundred KB per
    /// video, so later plays skip those reads. Safe to delete. Defaults to `.play-index` inside
    /// `complete_dir`.
    pub index_dir: Option<PathBuf>,
    /// Read each grabbed video's sample tables in the background, so its first play starts without
    /// reading every fragment. Plays always go first.
    pub prebuild_tables: bool,
    /// Bytes the sample tables in `index_dir` may take. Above it the ones used longest ago are
    /// removed. 0 sets no cap.
    pub index_max_bytes: u64,
    /// A sample table not used for this long is removed. 0 keeps tables however old.
    #[serde(with = "humantime_serde")]
    pub index_max_idle: Duration,
    /// First plays reading their sample tables at once. More wait for a turn; plays of the same
    /// video share its read.
    pub max_builds: u32,
    /// Plays streaming at once. One past it answers 503 with a `Retry-After`. 0 sets no cap.
    pub max_streams: u32,
}

/// 2 GiB: a 40 minute 1080p episode's two tables take about 270 KB, so the cap holds some eight
/// thousand episodes, more than a household plays in a year.
const DEFAULT_INDEX_MAX_BYTES: u64 = 2 << 30;

/// Half a year, as the matcher's notes: a table rebuilt after that costs one slower first play.
const DEFAULT_INDEX_MAX_IDLE: Duration = Duration::from_secs(180 * 86_400);

impl Default for Play {
    fn default() -> Self {
        let d = PlayConfig::default();
        Self {
            fragment_cache_bytes: d.fragment_cache_bytes as u64,
            probe_bytes: d.probe_bytes,
            index_dir: None,
            prebuild_tables: d.prebuild_tables,
            index_max_bytes: DEFAULT_INDEX_MAX_BYTES,
            index_max_idle: DEFAULT_INDEX_MAX_IDLE,
            max_builds: d.max_builds as u32,
            max_streams: d.max_streams as u32,
        }
    }
}

/// Which captions a grab writes next to its `.strm`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Subtitles {
    /// ISO 639-1 codes. Each becomes `<release>.<code>.srt` when YouTube has captions in it.
    pub languages: Vec<String>,
    /// Use YouTube's speech-to-text captions where no uploaded subtitles exist. Only the track in
    /// the spoken language counts, never YouTube's machine translations of it.
    pub automatic: bool,
    /// The lookup and every download together, before the grab is reported done. What is not in
    /// by then is skipped.
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}

impl Default for Subtitles {
    fn default() -> Self {
        Self {
            languages: ["en", "ur", "hi"].map(String::from).to_vec(),
            automatic: true,
            timeout: Duration::from_secs(15),
        }
    }
}

/// The shared YouTube lookup budget: a token bucket refilled with `lookups` tokens every `interval`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RateLimit {
    pub lookups: u32,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    /// Most lookups allowed back to back after a quiet spell.
    pub burst: u32,
    /// Up to this much random delay is added to a lookup that has to wait.
    #[serde(with = "humantime_serde")]
    pub jitter: Duration,
    /// How long every lookup is held after YouTube answers with a 429 or a bot check.
    #[serde(with = "humantime_serde")]
    pub throttle_pause: Duration,
    /// Most yt-dlp processes running at once, plays first. Each one with its deno holds about
    /// 350 MB.
    pub processes: usize,
}

impl Default for RateLimit {
    fn default() -> Self {
        // The live test ran 30 lookups at one every 10 seconds without a single rate limit. Two
        // yt-dlp runs at once took a 512 MB container over its limit (2026-10-24).
        Self {
            lookups: 1,
            interval: Duration::from_secs(10),
            burst: 3,
            jitter: Duration::from_secs(3),
            throttle_pause: Duration::from_secs(15 * 60),
            processes: 1,
        }
    }
}

/// Where yt-dlp and deno come from and how they are kept current.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Binaries {
    /// Folder Tubarr downloads both binaries into and updates them in. It lives on the config
    /// volume, so updates need no image rebuild.
    pub bin_dir: PathBuf,
    /// A yt-dlp installed some other way. When set, Tubarr runs it as it is and never downloads or
    /// updates it.
    pub yt_dlp: Option<PathBuf>,
    /// The same for deno, yt-dlp's JavaScript runtime for YouTube's challenges.
    pub deno: Option<PathBuf>,
    /// Which yt-dlp release channel to follow.
    pub yt_dlp_channel: Channel,
    /// Least time between two update checks of the same binary.
    #[serde(with = "humantime_serde")]
    pub check_interval: Duration,
    /// GitHub API base URL. Only tests point this elsewhere.
    pub github_api: Url,
}

impl Binaries {
    /// The path Tubarr runs for yt-dlp: the explicit one, else the managed copy in `bin_dir`.
    pub fn yt_dlp_path(&self) -> PathBuf {
        self.yt_dlp
            .clone()
            .unwrap_or_else(|| self.bin_dir.join("yt-dlp"))
    }

    /// The path Tubarr runs for deno: the explicit one, else the managed copy in `bin_dir`.
    pub fn deno_path(&self) -> PathBuf {
        self.deno
            .clone()
            .unwrap_or_else(|| self.bin_dir.join("deno"))
    }
}

impl Default for Binaries {
    fn default() -> Self {
        Self {
            bin_dir: PathBuf::from("/config/bin"),
            yt_dlp: None,
            deno: None,
            yt_dlp_channel: Channel::Stable,
            check_interval: Duration::from_secs(24 * 60 * 60),
            github_api: Url::parse("https://api.github.com/")
                .expect("default GitHub API URL is valid"),
        }
    }
}

/// How the watcher looks for new uploads on mapped channels.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Watcher {
    /// Each channel's RSS feed is read about this often.
    #[serde(with = "humantime_serde")]
    pub rss_interval: Duration,
    /// Up to this much random time is added to each feed's next read.
    #[serde(with = "humantime_serde")]
    pub rss_jitter: Duration,
    /// Least time between two feed reads, so channels are never read back to back.
    #[serde(with = "humantime_serde")]
    pub rss_spacing: Duration,
    /// Each channel's full video list is read about this often, spread over the interval.
    #[serde(with = "humantime_serde")]
    pub listing_interval: Duration,
    /// How many of the newest videos a full list reads.
    pub listing_size: u32,
    /// How many of the newest videos a fallback list reads. A channel whose feed has failed for
    /// longer than `rss_interval` is listed this way about every `rss_interval` instead, until a
    /// feed read works again.
    pub fallback_listing_size: u32,
    /// Most fallback lists an hour across all channels, spaced evenly. 0 turns them off.
    pub fallback_per_hour: u32,
    /// Each official playlist a mapping names is listed about this often. A playlist never listed
    /// before goes within minutes.
    #[serde(with = "humantime_serde")]
    pub playlist_interval: Duration,
    /// Days a show without a playlist gets a daily channel search for its back episodes, from
    /// when it is mapped or its channels or aliases change. 0 turns the search off.
    pub backfill_days: u32,
    /// Least time between two searches for the same show.
    #[serde(with = "humantime_serde")]
    pub backfill_interval: Duration,
    /// Least time between two shows' searches, so they are spread out.
    #[serde(with = "humantime_serde")]
    pub backfill_spacing: Duration,
    /// How many results one search reads.
    pub backfill_size: u32,
    /// Feed base URL. Its origin is also the site channel handles and links are resolved on. Only
    /// tests point this elsewhere.
    pub feed_url: Url,
}

impl Default for Watcher {
    fn default() -> Self {
        Self {
            rss_interval: Duration::from_secs(30 * 60),
            rss_jitter: Duration::from_secs(5 * 60),
            rss_spacing: Duration::from_secs(5),
            listing_interval: Duration::from_secs(24 * 60 * 60),
            listing_size: 100,
            fallback_listing_size: 15,
            fallback_per_hour: 30,
            playlist_interval: Duration::from_secs(24 * 60 * 60),
            backfill_days: 14,
            backfill_interval: Duration::from_secs(24 * 60 * 60),
            backfill_spacing: Duration::from_secs(10 * 60),
            backfill_size: 400,
            feed_url: Url::parse("https://www.youtube.com/feeds/videos.xml")
                .expect("default feed URL is valid"),
        }
    }
}

/// yt-dlp release channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    /// Daily builds from yt-dlp/yt-dlp-nightly-builds, for when YouTube changes faster than releases.
    Nightly,
}

/// Which Sonarr series and Radarr films Tubarr serves, and how often it asks.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ArrSettings {
    /// Tag label on the titles Tubarr serves, the same tag its indexer and client carry.
    pub tag: String,
    /// Time between two reads of the tagged titles. The first read runs at start.
    #[serde(with = "humantime_serde")]
    pub sync_interval: Duration,
    /// Hand each episode or film Tubarr releases for the first time straight to Sonarr or
    /// Radarr (their release push API), if the arr still wants it. Their RSS sync reads only the
    /// newest releases, so a back episode a backfill found would otherwise wait for a search by
    /// hand. `auto_search` is the old name and still read.
    #[serde(alias = "auto_search")]
    pub auto_push: bool,
    /// Least time between two pushes to the same arr. Each push is one release.
    #[serde(with = "humantime_serde")]
    pub push_interval: Duration,
    /// The name of Tubarr's indexer in Sonarr and Radarr, sent with each push.
    pub indexer: String,
    /// The name of Tubarr's download client in Sonarr and Radarr. Every push is pinned to it, so
    /// the arr never sends a pushed release to another client.
    pub download_client: String,
    /// Grabs older than this leave the download history Sonarr and Radarr poll. Their files stay.
    /// 0 keeps every grab.
    #[serde(with = "humantime_serde")]
    pub grab_retention: Duration,
}

impl Default for ArrSettings {
    fn default() -> Self {
        Self {
            tag: "youtube".to_owned(),
            sync_interval: Duration::from_secs(15 * 60),
            auto_push: true,
            push_interval: Duration::from_secs(5),
            indexer: "Tubarr".to_owned(),
            download_client: "Tubarr".to_owned(),
            grab_retention: Duration::from_secs(30 * 86_400),
        }
    }
}

/// When the show matcher runs and how much it may ask YouTube.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Matcher {
    /// A run also starts after each watcher find and each arr sync. This is the fallback.
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    /// Most resolver lookups one run makes for videos waiting on their duration (or height, when
    /// no height is assumed).
    pub lookups_per_run: u32,
    /// The height a release is named with while the video's real height is unknown, so an upload
    /// whose duration the channel list gave is released without a lookup. A channel's own
    /// `assumed_height` wins. 0 waits for a lookup instead.
    pub assumed_height: u32,
    /// Unmatched notes are kept for videos uploaded within this long. Older ones are dropped,
    /// except notes waiting on a lookup (`pending`), so the table stays bounded on a channel with
    /// years of uploads. 0 keeps every note.
    #[serde(with = "humantime_serde")]
    pub note_retention: Duration,
}

impl Default for Matcher {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30 * 60),
            lookups_per_run: 3,
            assumed_height: 1080,
            note_retention: DEFAULT_NOTE_RETENTION,
        }
    }
}

/// Half a year: a pin on an unsure upload is usually made within weeks of the note, and a weekly
/// show keeps about 26 uploads' worth of notes.
const DEFAULT_NOTE_RETENTION: Duration = Duration::from_secs(180 * 86_400);

/// How long the Activity page's searches, arr pushes and play starts are kept (`[activity]`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ActivitySettings {
    /// Events older than this are dropped by housekeeping. 0 keeps every event.
    #[serde(with = "humantime_serde")]
    pub retention: Duration,
}

impl Default for ActivitySettings {
    fn default() -> Self {
        Self {
            retention: Duration::from_secs(30 * 86_400),
        }
    }
}

/// What the Newznab answers carry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Newznab {
    /// Send each release's size. Off by default: Sonarr and Radarr check a size against their
    /// global quality size table, which expects far bigger files than YouTube's H.264 (a 38 min
    /// 1080p episode of 420 MB was refused under a 570 MB minimum). With no size they skip the
    /// check (`AcceptableSizeSpecification`: size 0 is unknown).
    pub report_size: bool,
}

/// How to reach a Sonarr or Radarr instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrConnection {
    pub url: Url,
    pub api_key: Secret,
    pub source: ConnectionSource,
}

/// Where an arr connection comes from. The config file wins over one saved in the web UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionSource {
    /// `[sonarr]` or `[radarr]` in the config file, read only in the UI.
    Config,
    /// Saved from the Settings page into the database.
    Saved,
}

/// A secret value. It has no `Display`, and `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a value read from somewhere other than a secret file, e.g. the database.
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    /// The value itself, for building a request. Never log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// Why a config could not be loaded. Every message names the file or the key at fault.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config file {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid config file {path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
    #[error("config key `{key}`: {reason}")]
    Invalid { key: &'static str, reason: String },
    #[error("config key `{key}`: cannot read secret file {path}: {source}")]
    SecretRead {
        key: &'static str,
        path: String,
        source: std::io::Error,
    },
}

/// The file as written. Every field but `public_url` has a default.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct ConfigFile {
    listen: SocketAddr,
    public_url: Option<Url>,
    data_dir: PathBuf,
    complete_dir: PathBuf,
    #[serde(with = "humantime_serde")]
    cache_ttl: Duration,
    #[serde(with = "humantime_serde")]
    lookup_timeout: Duration,
    rate_limit: RateLimit,
    binaries: Binaries,
    watcher: Watcher,
    play: Play,
    subtitles: Subtitles,
    sonarr: Option<ArrFile>,
    radarr: Option<ArrFile>,
    arr: ArrSettings,
    films: FilmSettings,
    drafts: DraftSettings,
    channels: BTreeMap<String, ChannelInfo>,
    root_folders: RootFolderDefaults,
    fallback: Fallback,
    canary: Canary,
    matcher: Matcher,
    activity: ActivitySettings,
    newznab: Newznab,
    api_key_file: Option<PathBuf>,
    notify: Notify,
    auth: AuthFile,
    hooks: HooksFile,
    jellyfin: Option<JellyfinFile>,
    backup: BackupPolicy,
    metrics: MetricsFile,
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            listen: DEFAULT_LISTEN
                .parse()
                .expect("default listen address is valid"),
            public_url: None,
            data_dir: PathBuf::from("/config"),
            complete_dir: PathBuf::from("/downloads/tubarr"),
            cache_ttl: Duration::from_secs(5 * 60 * 60),
            lookup_timeout: Duration::from_secs(60),
            rate_limit: RateLimit::default(),
            binaries: Binaries::default(),
            watcher: Watcher::default(),
            play: Play::default(),
            subtitles: Subtitles::default(),
            sonarr: None,
            radarr: None,
            arr: ArrSettings::default(),
            films: FilmSettings::default(),
            drafts: DraftSettings::default(),
            channels: BTreeMap::new(),
            root_folders: RootFolderDefaults::default(),
            fallback: Fallback::default(),
            canary: Canary::default(),
            matcher: Matcher::default(),
            activity: ActivitySettings::default(),
            newznab: Newznab::default(),
            api_key_file: None,
            notify: Notify::default(),
            auth: AuthFile::default(),
            hooks: HooksFile::default(),
            jellyfin: None,
            backup: BackupPolicy::default(),
            metrics: MetricsFile::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArrFile {
    url: Url,
    api_key_file: PathBuf,
}

impl Config {
    /// Parses and checks a config from TOML text. Secret files it names are read from disk.
    pub fn from_toml(text: &str) -> Result<Self, ConfigError> {
        Self::parse(text, "(inline)")
    }

    /// Reads, parses and checks the config file at `path`. A missing file counts as an empty one,
    /// which fails on the required `public_url`. `TUBARR_SETUP_CODE` sets the setup code.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let shown = path.display().to_string();
        let mut config = match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text, &shown),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::parse("", &shown),
            Err(source) => Err(ConfigError::Read {
                path: shown,
                source,
            }),
        }?;
        config.auth = config.auth.with_env_setup_code()?;
        Ok(config)
    }

    /// Path of the SQLite database file.
    pub fn database_path(&self) -> PathBuf {
        self.data_dir.join(DATABASE_FILE)
    }

    /// The limiter settings: `lookups` per `interval` becomes one token every `interval / lookups`.
    pub fn rate_limit_config(&self) -> RateLimitConfig {
        let r = &self.rate_limit;
        RateLimitConfig {
            interval: r.interval / r.lookups,
            burst: r.burst,
            max_jitter: r.jitter,
        }
    }

    /// How long resolved lookups are cached.
    pub fn cache_policy(&self) -> CachePolicy {
        CachePolicy {
            ttl: self.cache_ttl,
            ..CachePolicy::default()
        }
    }

    /// The play endpoint's settings.
    pub fn play_config(&self) -> PlayConfig {
        PlayConfig {
            // Checked against MAX_FRAGMENT_CACHE, so it fits in usize.
            fragment_cache_bytes: usize::try_from(self.play.fragment_cache_bytes)
                .unwrap_or(usize::MAX),
            probe_bytes: self.play.probe_bytes,
            prebuild_tables: self.play.prebuild_tables,
            max_builds: self.play.max_builds as usize,
            max_streams: self.play.max_streams as usize,
            ..PlayConfig::default()
        }
    }

    /// Where the play endpoint keeps sample tables.
    pub fn play_index_dir(&self) -> PathBuf {
        self.play
            .index_dir
            .clone()
            .unwrap_or_else(|| self.complete_dir.join(".play-index"))
    }

    /// The resolver's fallback steps.
    pub fn fallback_steps(&self) -> resolver::Fallback {
        resolver::Fallback {
            po_token: self
                .fallback
                .po_token_url
                .clone()
                .map(|base_url| resolver::PoToken {
                    base_url,
                    plugin_dir: self.fallback.plugin_dir.clone(),
                }),
            cookies_file: self.fallback.cookies_file.clone(),
            sticky_for: self.fallback.sticky_for,
            min_height: self.fallback.min_height,
        }
    }

    fn parse(text: &str, shown: &str) -> Result<Self, ConfigError> {
        let file: ConfigFile = toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: shown.to_owned(),
            source,
        })?;
        file.check()
    }
}

impl ConfigFile {
    fn check(mut self) -> Result<Config, ConfigError> {
        // No default: a guess here would write .strm links Jellyfin cannot reach.
        let public_url = self.public_url.ok_or_else(|| {
            invalid(
                "public_url",
                "is required: the URL Jellyfin uses to reach Tubarr, e.g. \"http://10.0.0.5:8480\"",
            )
        })?;
        let public_url = check_http_url("public_url", public_url)?;
        if self.data_dir.as_os_str().is_empty() {
            return Err(invalid("data_dir", "must not be empty"));
        }
        if !self.complete_dir.is_absolute() {
            return Err(invalid(
                "complete_dir",
                "must be an absolute path (Sonarr and Radarr see it as SABnzbd's complete_dir)",
            ));
        }
        if self.cache_ttl.is_zero() || self.cache_ttl > MAX_CACHE_TTL {
            return Err(invalid(
                "cache_ttl",
                "must be more than 0 and at most 6h (YouTube stream addresses expire after about 6 hours)",
            ));
        }
        if self.lookup_timeout.is_zero() || self.lookup_timeout > MAX_LOOKUP_TIMEOUT {
            return Err(invalid(
                "lookup_timeout",
                "must be more than 0 and at most 10m",
            ));
        }
        if self.rate_limit.lookups == 0 {
            return Err(invalid("rate_limit.lookups", "must be at least 1"));
        }
        if self.rate_limit.interval.is_zero() {
            return Err(invalid("rate_limit.interval", "must be more than 0"));
        }
        if self.rate_limit.burst == 0 {
            return Err(invalid("rate_limit.burst", "must be at least 1"));
        }
        if self.rate_limit.processes == 0 {
            return Err(invalid("rate_limit.processes", "must be at least 1"));
        }
        if self.rate_limit.jitter > self.rate_limit.interval {
            return Err(invalid(
                "rate_limit.jitter",
                "must not be longer than rate_limit.interval",
            ));
        }
        if self.rate_limit.throttle_pause.is_zero()
            || self.rate_limit.throttle_pause > MAX_THROTTLE_PAUSE
        {
            return Err(invalid(
                "rate_limit.throttle_pause",
                "must be more than 0 and at most 24h",
            ));
        }
        let mut binaries = self.binaries;
        if binaries.bin_dir.as_os_str().is_empty() {
            return Err(invalid("binaries.bin_dir", "must not be empty"));
        }
        if binaries
            .yt_dlp
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err(invalid("binaries.yt_dlp", "must not be empty"));
        }
        if binaries
            .deno
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err(invalid("binaries.deno", "must not be empty"));
        }
        if binaries.check_interval.is_zero() {
            return Err(invalid("binaries.check_interval", "must be more than 0"));
        }
        binaries.github_api = check_http_url("binaries.github_api", binaries.github_api)?;
        let watcher = self.watcher.check()?;
        if !(MIN_FRAGMENT_CACHE..=MAX_FRAGMENT_CACHE).contains(&self.play.fragment_cache_bytes) {
            return Err(invalid(
                "play.fragment_cache_bytes",
                &format!(
                    "must be between {MIN_FRAGMENT_CACHE} (8 MiB) and {MAX_FRAGMENT_CACHE} (1 GiB)"
                ),
            ));
        }
        if !(1..=MAX_BUILDS).contains(&self.play.max_builds) {
            return Err(invalid(
                "play.max_builds",
                &format!("must be between 1 and {MAX_BUILDS}"),
            ));
        }
        if !(MIN_PROBE..=MAX_HEAD_BYTES).contains(&self.play.probe_bytes) {
            return Err(invalid(
                "play.probe_bytes",
                &format!("must be between {MIN_PROBE} (4 KiB) and {MAX_HEAD_BYTES} (16 MiB)"),
            ));
        }
        if self
            .play
            .index_dir
            .as_ref()
            .is_some_and(|d| !d.is_absolute())
        {
            return Err(invalid("play.index_dir", "must be an absolute path"));
        }
        let mut seen = Vec::new();
        for lang in &self.subtitles.languages {
            if lang.len() != 2 || !lang.bytes().all(|b| b.is_ascii_lowercase()) {
                return Err(invalid(
                    "subtitles.languages",
                    &format!("{lang:?} is not a two-letter ISO 639-1 code like \"ur\""),
                ));
            }
            if seen.contains(&lang) {
                return Err(invalid(
                    "subtitles.languages",
                    &format!("{lang:?} is listed twice"),
                ));
            }
            seen.push(lang);
        }
        if self.subtitles.timeout.is_zero() || self.subtitles.timeout > MAX_SUBTITLE_TIMEOUT {
            return Err(invalid(
                "subtitles.timeout",
                "must be more than 0 and at most 60s",
            ));
        }
        let sonarr = self
            .sonarr
            .map(|a| a.check("sonarr.url", "sonarr.api_key_file"))
            .transpose()?;
        let radarr = self
            .radarr
            .map(|a| a.check("radarr.url", "radarr.api_key_file"))
            .transpose()?;
        // Sonarr and Radarr store tags in lower case.
        let tag = self.arr.tag.trim().to_lowercase();
        if tag.is_empty() {
            return Err(invalid("arr.tag", "must not be empty"));
        }
        if self.arr.sync_interval < MIN_SYNC_INTERVAL {
            return Err(invalid("arr.sync_interval", "must be at least 1m"));
        }
        if self.arr.push_interval < MIN_PUSH_INTERVAL {
            return Err(invalid("arr.push_interval", "must be at least 1s"));
        }
        let indexer = self.arr.indexer.trim().to_owned();
        if indexer.is_empty() {
            return Err(invalid("arr.indexer", "must not be empty"));
        }
        let download_client = self.arr.download_client.trim().to_owned();
        if download_client.is_empty() {
            return Err(invalid("arr.download_client", "must not be empty"));
        }
        let arr = ArrSettings {
            tag,
            sync_interval: self.arr.sync_interval,
            auto_push: self.arr.auto_push,
            push_interval: self.arr.push_interval,
            indexer,
            download_client,
            grab_retention: self.arr.grab_retention,
        };
        let films = self
            .films
            .check()
            .map_err(|(key, reason)| invalid(key, &reason))?;
        let channels =
            channels::from_file(self.channels, std::mem::take(&mut self.drafts.channels))?;
        let root_folders = self.root_folders.check()?;
        let drafts = self.drafts.check()?;
        let fallback = self.fallback.check()?;
        let canary = self.canary.check()?;
        if self.backup.keep == 0 {
            return Err(invalid("backup.keep", "must be at least 1"));
        }
        if self.matcher.interval < Duration::from_secs(60) {
            return Err(invalid("matcher.interval", "must be at least 1m"));
        }
        // Each lookup takes a token from the budget plays need too.
        if self.matcher.lookups_per_run > 20 {
            return Err(invalid("matcher.lookups_per_run", "must be at most 20"));
        }
        if self.matcher.assumed_height > MAX_ASSUMED_HEIGHT {
            return Err(invalid("matcher.assumed_height", "must be at most 1080"));
        }
        let api_key = self
            .api_key_file
            .map(|path| read_secret("api_key_file", &path))
            .transpose()?;
        let notify = self.notify.check()?;
        let auth = self.auth.check(&public_url)?;
        let hooks = self.hooks.check()?;
        let jellyfin = self.jellyfin.map(JellyfinFile::check).transpose()?;
        let metrics = MetricsConfig {
            enabled: self.metrics.enabled,
            token: self
                .metrics
                .token_file
                .map(|path| read_secret("metrics.token_file", &path))
                .transpose()?,
        };

        Ok(Config {
            listen: self.listen,
            public_url,
            data_dir: self.data_dir,
            complete_dir: self.complete_dir,
            cache_ttl: self.cache_ttl,
            lookup_timeout: self.lookup_timeout,
            rate_limit: self.rate_limit,
            binaries,
            watcher,
            play: self.play,
            subtitles: self.subtitles,
            sonarr,
            radarr,
            arr,
            films,
            drafts,
            channels,
            root_folders,
            fallback,
            canary,
            matcher: self.matcher,
            activity: self.activity,
            newznab: self.newznab,
            api_key,
            notify,
            auth,
            hooks,
            jellyfin,
            backup: self.backup,
            metrics,
        })
    }
}

impl JellyfinFile {
    fn check(self) -> Result<JellyfinConfig, ConfigError> {
        let url = check_http_url("jellyfin.url", self.url)?;
        for (tubarr, jellyfin) in &self.library_paths {
            if !tubarr.is_absolute() || !jellyfin.is_absolute() {
                return Err(invalid(
                    "jellyfin.library_paths",
                    "every pair must be two absolute paths: [\"<as Tubarr sees it>\", \"<as Jellyfin sees it>\"]",
                ));
            }
        }
        if self.merge_retry.is_zero() || self.merge_retry > Duration::from_secs(60 * 60) {
            return Err(invalid(
                "jellyfin.merge_retry",
                "must be more than 0 and at most 1h",
            ));
        }
        if !(1..=12).contains(&self.merge_attempts) {
            return Err(invalid(
                "jellyfin.merge_attempts",
                "must be between 1 and 12",
            ));
        }
        let user_id = self.user_id.filter(|u| !u.trim().is_empty());
        if user_id
            .as_deref()
            .is_some_and(|u| !u.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        {
            return Err(invalid(
                "jellyfin.user_id",
                "must be a Jellyfin user id (hex, as in Dashboard, Users, the user's address)",
            ));
        }
        Ok(JellyfinConfig {
            url,
            api_key: read_secret("jellyfin.api_key_file", &self.api_key_file)?,
            user_id,
            library_paths: self.library_paths,
            merge_retry: self.merge_retry,
            merge_attempts: self.merge_attempts,
        })
    }
}

impl HooksFile {
    fn check(self) -> Result<Hooks, ConfigError> {
        for (sonarr, tubarr) in &self.library_paths {
            if !sonarr.is_absolute() || !tubarr.is_absolute() {
                return Err(invalid(
                    "hooks.library_paths",
                    "every pair must be two absolute paths: [\"<as Sonarr sees it>\", \"<as Tubarr sees it>\"]",
                ));
            }
        }
        if self.sonarr_token_file.is_some() && self.library_paths.is_empty() {
            return Err(invalid(
                "hooks.library_paths",
                "is required with hooks.sonarr_token_file: Tubarr only touches files under these paths",
            ));
        }
        let sonarr_token = self
            .sonarr_token_file
            .map(|path| read_secret("hooks.sonarr_token_file", &path))
            .transpose()?;
        let thumbnail_url = match self.thumbnail_url {
            Some(url) => check_http_url("hooks.thumbnail_url", url)?,
            None => Hooks::default().thumbnail_url,
        };
        Ok(Hooks {
            sonarr_token,
            library_paths: self.library_paths,
            thumbnail_url,
        })
    }
}

impl Fallback {
    fn check(mut self) -> Result<Self, ConfigError> {
        self.po_token_url = self
            .po_token_url
            .map(|url| check_http_url("fallback.po_token_url", url))
            .transpose()?;
        if self.plugin_dir.is_some() && self.po_token_url.is_none() {
            return Err(invalid(
                "fallback.plugin_dir",
                "only makes sense together with fallback.po_token_url",
            ));
        }
        for (key, path) in [
            ("fallback.plugin_dir", &self.plugin_dir),
            ("fallback.cookies_file", &self.cookies_file),
        ] {
            if path.as_ref().is_some_and(|p| !p.is_absolute()) {
                return Err(invalid(key, "must be an absolute path"));
            }
        }
        if self.sticky_for.is_zero() || self.sticky_for > MAX_THROTTLE_PAUSE {
            return Err(invalid(
                "fallback.sticky_for",
                "must be more than 0 and at most 24h",
            ));
        }
        // 1080p is the tallest format Tubarr serves, so a higher floor would refuse every answer.
        if self.min_height > 1080 {
            return Err(invalid("fallback.min_height", "must be at most 1080"));
        }
        Ok(self)
    }
}

impl Notify {
    fn check(self) -> Result<Option<NotifyTarget>, ConfigError> {
        match (
            self.discord_webhook_file,
            self.card_relay_url,
            self.card_relay_token_file,
        ) {
            (None, None, None) => Ok(None),
            (Some(path), None, None) => {
                let webhook = read_secret("notify.discord_webhook_file", &path)?;
                // The message never repeats the value: it holds the webhook's token.
                if !is_discord_webhook(webhook.expose()) {
                    return Err(invalid(
                        "notify.discord_webhook_file",
                        "the file must hold a Discord webhook URL like https://discord.com/api/webhooks/<id>/<token>",
                    ));
                }
                Ok(Some(NotifyTarget::Discord { webhook }))
            }
            (None, Some(url), Some(token_file)) => {
                if !matches!(url.scheme(), "http" | "https") || !url.has_host() {
                    return Err(invalid(
                        "notify.card_relay_url",
                        "must be an http:// or https:// URL",
                    ));
                }
                if !url.username().is_empty() || url.password().is_some() {
                    return Err(invalid(
                        "notify.card_relay_url",
                        "must not hold a user name or password (the token goes in card_relay_token_file)",
                    ));
                }
                let token = read_secret("notify.card_relay_token_file", &token_file)?;
                Ok(Some(NotifyTarget::CardRelay { url, token }))
            }
            (None, Some(_), None) => Err(invalid(
                "notify.card_relay_token_file",
                "is required with notify.card_relay_url",
            )),
            (None, None, Some(_)) => Err(invalid(
                "notify.card_relay_url",
                "is required with notify.card_relay_token_file",
            )),
            (Some(_), _, _) => Err(invalid(
                "notify",
                "set either discord_webhook_file or the card relay, not both",
            )),
        }
    }
}

/// `https://discord.com/api/webhooks/<id>/<token>`, also on the ptb and canary hosts.
fn is_discord_webhook(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    let host_ok = matches!(
        url.host_str(),
        Some(
            "discord.com"
                | "ptb.discord.com"
                | "canary.discord.com"
                | "discordapp.com"
                | "ptb.discordapp.com"
                | "canary.discordapp.com"
        )
    );
    let segments: Vec<&str> = url.path().trim_end_matches('/').split('/').collect();
    let path_ok = matches!(
        segments[..],
        ["", "api", "webhooks", id, token]
            if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())
                && !token.is_empty()
                && token.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );
    url.scheme() == "https" && host_ok && path_ok && url.query().is_none()
}

impl Canary {
    fn check(self) -> Result<Self, ConfigError> {
        if let Some(id) = &self.video_id
            && !resolver::is_video_id(id)
        {
            return Err(invalid(
                "canary.video_id",
                &format!("{id:?} is not an 11 character YouTube video id"),
            ));
        }
        if self.interval < MIN_CANARY_INTERVAL {
            return Err(invalid("canary.interval", "must be at least 10m"));
        }
        if self.jitter > self.interval {
            return Err(invalid(
                "canary.jitter",
                "must not be longer than canary.interval",
            ));
        }
        Ok(self)
    }
}

impl Watcher {
    // Lower bounds keep a typo from hammering YouTube: it flags an address after a burst.
    fn check(self) -> Result<Self, ConfigError> {
        if self.rss_interval < Duration::from_secs(5 * 60) {
            return Err(invalid("watcher.rss_interval", "must be at least 5m"));
        }
        if self.rss_jitter > self.rss_interval {
            return Err(invalid(
                "watcher.rss_jitter",
                "must not be longer than watcher.rss_interval",
            ));
        }
        if self.rss_spacing < Duration::from_secs(1) {
            return Err(invalid("watcher.rss_spacing", "must be at least 1s"));
        }
        if self.listing_interval < Duration::from_secs(60 * 60) {
            return Err(invalid("watcher.listing_interval", "must be at least 1h"));
        }
        if self.playlist_interval < Duration::from_secs(60 * 60) {
            return Err(invalid("watcher.playlist_interval", "must be at least 1h"));
        }
        if self.backfill_days > 365 {
            return Err(invalid("watcher.backfill_days", "must be 365 or less"));
        }
        if self.backfill_interval < Duration::from_secs(60 * 60) {
            return Err(invalid("watcher.backfill_interval", "must be at least 1h"));
        }
        if self.backfill_spacing < Duration::from_secs(60) {
            return Err(invalid("watcher.backfill_spacing", "must be at least 1m"));
        }
        if !(1..=500).contains(&self.backfill_size) {
            return Err(invalid(
                "watcher.backfill_size",
                "must be between 1 and 500",
            ));
        }
        if !(1..=1000).contains(&self.listing_size) {
            return Err(invalid(
                "watcher.listing_size",
                "must be between 1 and 1000",
            ));
        }
        if !(1..=100).contains(&self.fallback_listing_size) {
            return Err(invalid(
                "watcher.fallback_listing_size",
                "must be between 1 and 100",
            ));
        }
        if self.fallback_per_hour > 120 {
            return Err(invalid("watcher.fallback_per_hour", "must be 120 or less"));
        }
        if !matches!(self.feed_url.scheme(), "http" | "https") || !self.feed_url.has_host() {
            return Err(invalid(
                "watcher.feed_url",
                "must be an http:// or https:// URL",
            ));
        }
        Ok(self)
    }
}

impl ArrFile {
    fn check(
        self,
        url_key: &'static str,
        key_file_key: &'static str,
    ) -> Result<ArrConnection, ConfigError> {
        let url = check_http_url(url_key, self.url)?;
        let api_key = read_secret(key_file_key, &self.api_key_file)?;
        Ok(ArrConnection {
            url,
            api_key,
            source: ConnectionSource::Config,
        })
    }
}

fn invalid(key: &'static str, reason: &str) -> ConfigError {
    ConfigError::Invalid {
        key,
        reason: reason.to_owned(),
    }
}

/// Accepts plain http(s) base URLs and makes sure the path ends in `/`, so joining a relative path
/// keeps any prefix (`http://host/tubarr` + `play/x` gives `http://host/tubarr/play/x`).
pub(crate) fn check_http_url(key: &'static str, mut url: Url) -> Result<Url, ConfigError> {
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid(key, "must start with http:// or https://"));
    }
    if !url.has_host() {
        return Err(invalid(key, "must name a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid(key, "must not hold a user name or password"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(invalid(key, "must not have a query or fragment"));
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

pub(crate) fn read_secret(key: &'static str, path: &Path) -> Result<Secret, ConfigError> {
    let text = std::fs::read_to_string(path).map_err(|source| ConfigError::SecretRead {
        key,
        path: path.display().to_string(),
        source,
    })?;
    let value = text.trim();
    if value.is_empty() {
        return Err(ConfigError::Invalid {
            key,
            reason: format!("secret file {} is empty", path.display()),
        });
    }
    Ok(Secret(value.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn secret_file(value: &str) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(value.as_bytes()).unwrap();
        file
    }

    const MINIMAL: &str = "public_url = \"http://tubarr:8480\"\n";

    /// The error for `toml` after the required `public_url`.
    fn error_text(toml: &str) -> String {
        Config::from_toml(&format!("{MINIMAL}{toml}"))
            .unwrap_err()
            .to_string()
    }

    #[test]
    fn empty_file_needs_public_url() {
        let text = Config::from_toml("").unwrap_err().to_string();
        assert!(
            text.contains("`public_url`") && text.contains("required"),
            "{text}"
        );
    }

    #[test]
    fn only_public_url_gives_defaults_for_the_rest() {
        let config = Config::from_toml(MINIMAL).unwrap();
        assert_eq!(config.listen, DEFAULT_LISTEN.parse().unwrap());
        assert_eq!(config.public_url.as_str(), "http://tubarr:8480/");
        assert_eq!(config.database_path(), Path::new("/config/tubarr.db"));
        assert_eq!(config.complete_dir, Path::new("/downloads/tubarr"));
        assert_eq!(
            config.play_index_dir(),
            Path::new("/downloads/tubarr/.play-index")
        );
        assert_eq!(config.cache_ttl, Duration::from_secs(5 * 3600));
        assert_eq!(config.rate_limit, RateLimit::default());
        assert_eq!(config.binaries, Binaries::default());
        assert_eq!(config.watcher, Watcher::default());
        assert_eq!(config.films, FilmSettings::default());
        assert!(config.films.channels.is_empty());
        assert_eq!(config.watcher.rss_interval, Duration::from_secs(30 * 60));
        assert_eq!(config.watcher.listing_size, 100);
        assert_eq!(config.watcher.fallback_listing_size, 15);
        assert_eq!(config.watcher.fallback_per_hour, 30);
        assert_eq!(config.lookup_timeout, Duration::from_secs(60));
        assert_eq!(
            config.rate_limit.throttle_pause,
            Duration::from_secs(15 * 60)
        );
        // The defaults match what the limiter and resolver use on their own.
        assert_eq!(config.rate_limit_config(), RateLimitConfig::default());
        assert_eq!(config.cache_policy(), CachePolicy::default());
        assert_eq!(config.play_config(), PlayConfig::default());
        assert_eq!(
            config.binaries.yt_dlp_path(),
            Path::new("/config/bin/yt-dlp")
        );
        assert_eq!(config.binaries.deno_path(), Path::new("/config/bin/deno"));
        assert!(config.sonarr.is_none());
        assert!(config.radarr.is_none());
        assert_eq!(config.arr.tag, "youtube");
        assert_eq!(config.backup, BackupPolicy::default());
        assert_eq!(config.arr.sync_interval, Duration::from_secs(15 * 60));
        assert!(config.arr.auto_push);
        assert_eq!(config.arr.push_interval, Duration::from_secs(5));
        assert_eq!(config.arr.indexer, "Tubarr");
        assert_eq!(config.arr.download_client, "Tubarr");
        assert_eq!(config.watcher.backfill_size, 400);
        assert_eq!(config.matcher.interval, Duration::from_secs(30 * 60));
        assert_eq!(config.matcher.lookups_per_run, 3);
        assert_eq!(config.matcher.assumed_height, 1080);
        assert_eq!(
            config.matcher.note_retention,
            Duration::from_secs(180 * 86_400)
        );
        assert_eq!(config.play.index_max_bytes, 2 << 30);
        assert_eq!(
            config.play.index_max_idle,
            Duration::from_secs(180 * 86_400)
        );
        assert_eq!(config.arr.grab_retention, Duration::from_secs(30 * 86_400));
        assert_eq!(config.activity.retention, Duration::from_secs(30 * 86_400));
        assert!(!config.newznab.report_size);
        assert!(config.api_key.is_none());
        assert_eq!(config.subtitles.languages, ["en", "ur", "hi"]);
        assert!(config.subtitles.automatic);
        assert_eq!(config.subtitles.timeout, Duration::from_secs(15));
    }

    #[test]
    fn jellyfin_is_off_until_set_and_its_key_comes_from_a_file() {
        assert!(Config::from_toml(MINIMAL).unwrap().jellyfin.is_none());
        let key = secret_file("jf-key\n");
        let config = Config::from_toml(&format!(
            "{MINIMAL}[jellyfin]\nurl = \"http://jellyfin:8096\"\napi_key_file = \"{}\"\n",
            key.path().display()
        ))
        .unwrap();
        let j = config.jellyfin.unwrap();
        assert_eq!(j.url.as_str(), "http://jellyfin:8096/");
        assert_eq!(j.api_key.expose(), "jf-key");
        assert_eq!(
            (j.user_id, j.merge_retry, j.merge_attempts),
            (None, Duration::from_secs(30), 7)
        );
        let bad = |extra: &str| {
            error_text(&format!(
                "[jellyfin]\nurl = \"http://jellyfin:8096\"\napi_key_file = \"{}\"\n{extra}",
                key.path().display()
            ))
        };
        assert!(bad("user_id = \"me\"").contains("jellyfin.user_id"));
        assert!(bad("merge_attempts = 0").contains("jellyfin.merge_attempts"));
        assert!(bad("library_paths = [[\"media\", \"/m\"]]").contains("jellyfin.library_paths"));
        let text = error_text("[jellyfin]\nurl = \"http://jf\"\napi_key_file = \"/nonexistent\"\n");
        assert!(text.contains("jellyfin.api_key_file"), "{text}");
    }

    #[test]
    fn hooks_need_a_token_file_and_absolute_library_paths() {
        let off = Config::from_toml(MINIMAL).unwrap();
        assert!(off.hooks.sonarr_token.is_none() && off.hooks.library_paths.is_empty());

        let token = secret_file("hook-token\n");
        let config = Config::from_toml(&format!(
            "{MINIMAL}[hooks]\nsonarr_token_file = \"{}\"\nlibrary_paths = [[\"/mnt/media\", \"/media\"]]\n",
            token.path().display()
        ))
        .unwrap();
        assert_eq!(config.hooks.sonarr_token.unwrap().expose(), "hook-token");
        assert_eq!(
            config.hooks.library_paths,
            [(PathBuf::from("/mnt/media"), PathBuf::from("/media"))]
        );

        let text = error_text(&format!(
            "[hooks]\nsonarr_token_file = \"{}\"\n",
            token.path().display()
        ));
        assert!(text.contains("hooks.library_paths"), "{text}");
        let text = error_text("[hooks]\nlibrary_paths = [[\"mnt/media\", \"/media\"]]\n");
        assert!(text.contains("absolute"), "{text}");
        let text = error_text(
            "[hooks]\nsonarr_token_file = \"/nonexistent/token\"\nlibrary_paths = [[\"/a\", \"/b\"]]\n",
        );
        assert!(text.contains("hooks.sonarr_token_file"), "{text}");
    }

    #[test]
    fn api_key_comes_from_its_file() {
        let key = secret_file("tubarr-key\n");
        let toml = format!("{MINIMAL}api_key_file = \"{}\"", key.path().display());
        let config = Config::from_toml(&toml).unwrap();
        assert_eq!(config.api_key.unwrap().expose(), "tubarr-key");

        let text = error_text("api_key_file = \"/nonexistent/key\"");
        assert!(text.contains("`api_key_file`"), "{text}");
    }

    #[test]
    fn full_file_is_read() {
        let sonarr_key = secret_file("sonarr-key\n");
        let radarr_key = secret_file("radarr-key");
        let toml = format!(
            r#"
            listen = "127.0.0.1:9000"
            public_url = "http://10.0.0.5:9000/tubarr"
            data_dir = "/data"
            complete_dir = "/downloads/yt"
            cache_ttl = "2h 30m"
            lookup_timeout = "90s"

            [rate_limit]
            lookups = 2
            interval = "30s"
            burst = 5
            jitter = "2s"
            throttle_pause = "5m"
            processes = 2

            [binaries]
            bin_dir = "/data/bin"
            yt_dlp = "/opt/yt-dlp"
            yt_dlp_channel = "nightly"
            check_interval = "12h"

            [watcher]
            rss_interval = "1h"
            listing_interval = "12h"
            listing_size = 50
            playlist_interval = "2d"
            backfill_days = 7
            backfill_size = 100
            [play]
            fragment_cache_bytes = 33554432
            probe_bytes = 131072
            index_dir = "/data/index"
            prebuild_tables = false
            max_builds = 3
            max_streams = 0

            [subtitles]
            languages = ["ur"]
            automatic = false
            timeout = "5s"

            [sonarr]
            url = "http://sonarr:8989"
            api_key_file = "{}"

            [radarr]
            url = "https://radarr.example/base/"
            api_key_file = "{}"

            [arr]
            tag = " YouTube "
            sync_interval = "5m"
            auto_search = false
            push_interval = "10s"
            indexer = " Tubarr YT "
            download_client = "Tubarr YT"
            "#,
            sonarr_key.path().display(),
            radarr_key.path().display()
        );
        let config = Config::from_toml(&toml).unwrap();
        assert_eq!(config.listen, "127.0.0.1:9000".parse().unwrap());
        assert_eq!(config.public_url.as_str(), "http://10.0.0.5:9000/tubarr/");
        assert_eq!(config.database_path(), Path::new("/data/tubarr.db"));
        assert_eq!(config.complete_dir, Path::new("/downloads/yt"));
        assert_eq!(config.cache_ttl, Duration::from_secs(2 * 3600 + 30 * 60));
        assert_eq!(
            config.rate_limit,
            RateLimit {
                lookups: 2,
                interval: Duration::from_secs(30),
                burst: 5,
                jitter: Duration::from_secs(2),
                throttle_pause: Duration::from_secs(300),
                processes: 2,
            }
        );
        assert_eq!(config.lookup_timeout, Duration::from_secs(90));
        assert_eq!(
            config.rate_limit_config(),
            RateLimitConfig {
                interval: Duration::from_secs(15),
                burst: 5,
                max_jitter: Duration::from_secs(2),
            }
        );
        assert_eq!(
            config.cache_policy().ttl,
            Duration::from_secs(2 * 3600 + 30 * 60)
        );
        assert_eq!(config.binaries.yt_dlp_path(), Path::new("/opt/yt-dlp"));
        assert_eq!(config.binaries.deno_path(), Path::new("/data/bin/deno"));
        assert_eq!(config.binaries.yt_dlp_channel, Channel::Nightly);
        assert_eq!(
            config.binaries.check_interval,
            Duration::from_secs(12 * 3600)
        );
        assert_eq!(config.watcher.rss_interval, Duration::from_secs(3600));
        assert_eq!(
            config.watcher.listing_interval,
            Duration::from_secs(12 * 3600)
        );
        assert_eq!(config.watcher.listing_size, 50);
        assert_eq!(
            (config.watcher.backfill_days, config.watcher.backfill_size),
            (7, 100)
        );
        assert_eq!(
            config.watcher.backfill_interval,
            Duration::from_secs(24 * 3600)
        );
        assert_eq!(
            config.watcher.playlist_interval,
            Duration::from_secs(48 * 3600)
        );
        assert_eq!(
            config.play_config(),
            PlayConfig {
                fragment_cache_bytes: 32 * 1024 * 1024,
                probe_bytes: 128 * 1024,
                prebuild_tables: false,
                max_builds: 3,
                max_streams: 0,
                ..PlayConfig::default()
            }
        );
        assert_eq!(config.play_index_dir(), Path::new("/data/index"));
        assert_eq!(
            config.subtitles,
            Subtitles {
                languages: vec!["ur".to_owned()],
                automatic: false,
                timeout: Duration::from_secs(5),
            }
        );
        let sonarr = config.sonarr.unwrap();
        assert_eq!(sonarr.url.as_str(), "http://sonarr:8989/");
        assert_eq!(sonarr.api_key.expose(), "sonarr-key");
        let radarr = config.radarr.unwrap();
        assert_eq!(radarr.url.as_str(), "https://radarr.example/base/");
        assert_eq!(radarr.api_key.expose(), "radarr-key");
        assert_eq!(config.arr.tag, "youtube");
        assert_eq!(config.arr.sync_interval, Duration::from_secs(300));
        // `auto_search` is the old name of `auto_push`.
        assert!(!config.arr.auto_push);
        assert_eq!(config.arr.push_interval, Duration::from_secs(10));
        assert_eq!(config.arr.indexer, "Tubarr YT");
        assert_eq!(config.arr.download_client, "Tubarr YT");
    }

    #[test]
    fn public_url_prefix_survives_joining() {
        let config = Config::from_toml("public_url = \"http://host:1/tubarr\"").unwrap();
        assert_eq!(
            config.public_url.join("play/abc").unwrap().as_str(),
            "http://host:1/tubarr/play/abc"
        );
    }

    #[test]
    fn secrets_never_print() {
        let key = secret_file("hunter2");
        let toml = format!(
            "{MINIMAL}[sonarr]\nurl = \"http://s\"\napi_key_file = \"{}\"",
            key.path().display()
        );
        let config = Config::from_toml(&toml).unwrap();
        assert!(!format!("{config:?}").contains("hunter2"));
    }

    #[test]
    fn inline_api_key_is_refused() {
        let text = error_text("[sonarr]\nurl = \"http://s\"\napi_key = \"abc\"");
        assert!(text.contains("api_key"), "{text}");
    }

    #[test]
    fn unknown_keys_are_refused() {
        assert!(error_text("lisen = \"127.0.0.1:9000\"").contains("lisen"));
    }

    #[test]
    fn films_section_is_read() {
        let toml = format!(
            "{MINIMAL}\n[films]\nchannels = [\"UCgoldmines\", \"UCshemaroo\"]\n\
             min_duration = \"90m\"\nsimilarity = 0.95\n"
        );
        let films = Config::from_toml(&toml).unwrap().films;
        assert_eq!(films.channels, ["UCgoldmines", "UCshemaroo"]);
        assert_eq!(films.min_duration, Duration::from_secs(90 * 60));
        assert!((films.similarity - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn bad_values_name_their_key() {
        let cases = [
            ("data_dir = \"\"", "data_dir"),
            ("complete_dir = \"downloads\"", "complete_dir"),
            ("cache_ttl = \"0s\"", "cache_ttl"),
            ("cache_ttl = \"7h\"", "cache_ttl"),
            ("cache_ttl = \"soon\"", "cache_ttl"),
            ("[rate_limit]\nlookups = 0", "rate_limit.lookups"),
            ("[rate_limit]\ninterval = \"0s\"", "rate_limit.interval"),
            ("[rate_limit]\nburst = 0", "rate_limit.burst"),
            ("[rate_limit]\nprocesses = 0", "rate_limit.processes"),
            ("[rate_limit]\njitter = \"11s\"", "rate_limit.jitter"),
            (
                "[rate_limit]\nthrottle_pause = \"0s\"",
                "rate_limit.throttle_pause",
            ),
            (
                "[rate_limit]\nthrottle_pause = \"2days\"",
                "rate_limit.throttle_pause",
            ),
            ("[subtitles]\nlanguages = [\"urd\"]", "subtitles.languages"),
            ("[subtitles]\nlanguages = [\"UR\"]", "subtitles.languages"),
            (
                "[subtitles]\nlanguages = [\"ur\", \"ur\"]",
                "subtitles.languages",
            ),
            ("[subtitles]\ntimeout = \"0s\"", "subtitles.timeout"),
            ("[subtitles]\ntimeout = \"2m\"", "subtitles.timeout"),
            ("lookup_timeout = \"0s\"", "lookup_timeout"),
            ("lookup_timeout = \"1h\"", "lookup_timeout"),
            ("[binaries]\nyt_dlp = \"\"", "binaries.yt_dlp"),
            ("[binaries]\ndeno = \"\"", "binaries.deno"),
            ("[binaries]\nbin_dir = \"\"", "binaries.bin_dir"),
            (
                "[binaries]\ncheck_interval = \"0s\"",
                "binaries.check_interval",
            ),
            (
                "[binaries]\ngithub_api = \"ftp://x\"",
                "binaries.github_api",
            ),
            ("[binaries]\nyt_dlp_channel = \"beta\"", "yt_dlp_channel"),
            ("[watcher]\nrss_interval = \"1m\"", "watcher.rss_interval"),
            ("[watcher]\nrss_jitter = \"1h\"", "watcher.rss_jitter"),
            ("[watcher]\nrss_spacing = \"0s\"", "watcher.rss_spacing"),
            (
                "[watcher]\nlisting_interval = \"10m\"",
                "watcher.listing_interval",
            ),
            ("[watcher]\nlisting_size = 0", "watcher.listing_size"),
            (
                "[watcher]\nfallback_listing_size = 0",
                "watcher.fallback_listing_size",
            ),
            (
                "[watcher]\nfallback_per_hour = 500",
                "watcher.fallback_per_hour",
            ),
            ("[watcher]\nbackfill_days = 1000", "watcher.backfill_days"),
            ("[watcher]\nbackfill_size = 0", "watcher.backfill_size"),
            (
                "[watcher]\nbackfill_interval = \"10m\"",
                "watcher.backfill_interval",
            ),
            (
                "[watcher]\nbackfill_spacing = \"1s\"",
                "watcher.backfill_spacing",
            ),
            (
                "[channels.UC4JCksJF76g_MdzPVBJoC3Q]\nlisting_size = 0",
                "channels",
            ),
            (
                "[watcher]\nplaylist_interval = \"10m\"",
                "watcher.playlist_interval",
            ),
            ("[watcher]\nfeed_url = \"ftp://x\"", "watcher.feed_url"),
            (
                "[play]\nfragment_cache_bytes = 1000",
                "play.fragment_cache_bytes",
            ),
            (
                "[play]\nfragment_cache_bytes = 2147483648",
                "play.fragment_cache_bytes",
            ),
            ("[play]\nmax_builds = 0", "play.max_builds"),
            ("[play]\nmax_builds = 9", "play.max_builds"),
            ("[play]\nprobe_bytes = 100", "play.probe_bytes"),
            ("[play]\nprobe_bytes = 33554432", "play.probe_bytes"),
            ("[play]\nprobe = 1", "probe"),
            ("[play]\nindex_dir = \"index\"", "play.index_dir"),
            (
                "[radarr]\nurl = \"radarr:7878\"\napi_key_file = \"/x\"",
                "radarr.url",
            ),
            ("[sonarr]\napi_key_file = \"/x\"", "url"),
            ("[arr]\ntag = \" \"", "arr.tag"),
            ("[arr]\nsync_interval = \"30s\"", "arr.sync_interval"),
            ("[arr]\npush_interval = \"500ms\"", "arr.push_interval"),
            ("[arr]\ndownload_client = \" \"", "arr.download_client"),
            ("[films]\nsimilarity = 1.5", "films.similarity"),
            ("[films]\nmin_duration = \"10s\"", "films.min_duration"),
            ("[films]\nchannels = [\"UCa\", \"UCa\"]", "films.channels"),
            ("[matcher]\ninterval = \"10s\"", "matcher.interval"),
            ("[matcher]\nlookups_per_run = 21", "matcher.lookups_per_run"),
            ("[backup]\nkeep = 0", "backup.keep"),
            ("[matcher]\nassumed_height = 2160", "matcher.assumed_height"),
            (
                "[channels.UC4JCksJF76g_MdzPVBJoC3Q]\nassumed_height = 1440",
                "channels",
            ),
        ];
        for (toml, key) in cases {
            let text = error_text(toml);
            assert!(text.contains(key), "{toml:?} gave {text:?}");
        }
    }

    #[test]
    fn bad_public_url_names_the_key() {
        for url in ["ftp://host", "http://host/?a=1", "http://u:p@host"] {
            let text = Config::from_toml(&format!("public_url = \"{url}\""))
                .unwrap_err()
                .to_string();
            assert!(
                text.contains("config key `public_url`"),
                "{url} gave {text:?}"
            );
        }
    }

    #[test]
    fn missing_or_empty_secret_file_names_the_key() {
        let text = error_text("[sonarr]\nurl = \"http://s\"\napi_key_file = \"/nonexistent/key\"");
        assert!(text.contains("sonarr.api_key_file"), "{text}");

        let empty = secret_file("  \n");
        let toml = format!(
            "[radarr]\nurl = \"http://r\"\napi_key_file = \"{}\"",
            empty.path().display()
        );
        let text = error_text(&toml);
        assert!(text.contains("radarr.api_key_file"), "{text}");
        assert!(text.contains("empty"), "{text}");
    }

    #[test]
    fn missing_file_needs_public_url() {
        let text = Config::load(Path::new("/nonexistent/tubarr.toml"))
            .unwrap_err()
            .to_string();
        assert!(text.contains("`public_url`"), "{text}");
    }

    #[test]
    fn parse_errors_name_the_file() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(b"listen = 5").unwrap();
        let text = Config::load(file.path()).unwrap_err().to_string();
        assert!(text.contains(&file.path().display().to_string()), "{text}");
        assert!(text.contains("listen"), "{text}");
    }

    #[test]
    fn fallback_and_canary_are_off_by_default_and_read() {
        let config = Config::from_toml(MINIMAL).unwrap();
        assert_eq!(config.fallback, Fallback::default());
        assert_eq!(config.canary, Canary::default());
        assert!(config.fallback_steps().po_token.is_none());

        let config = Config::from_toml(&format!(
            "{MINIMAL}[fallback]\npo_token_url = \"http://bgutil:4416\"\nplugin_dir = \"/config/yt-dlp-plugins\"\ncookies_file = \"/secrets/cookies.txt\"\nsticky_for = \"30m\"\nmin_height = 480\n[canary]\nvideo_id = \"0eD-GQCutAU\"\ninterval = \"30m\"\njitter = \"5m\"\n"
        ))
        .unwrap();
        let steps = config.fallback_steps();
        let po = steps.po_token.unwrap();
        assert_eq!(po.base_url.as_str(), "http://bgutil:4416/");
        assert_eq!(po.plugin_dir.unwrap(), Path::new("/config/yt-dlp-plugins"));
        assert_eq!(
            steps.cookies_file.unwrap(),
            Path::new("/secrets/cookies.txt")
        );
        assert_eq!(steps.sticky_for, Duration::from_secs(30 * 60));
        assert_eq!(steps.min_height, 480);
        assert_eq!(Fallback::default().min_height, 720);
        assert_eq!(config.canary.video_id.as_deref(), Some("0eD-GQCutAU"));
        assert_eq!(config.canary.interval, Duration::from_secs(30 * 60));
    }

    #[test]
    fn bad_fallback_and_canary_values_name_their_key() {
        let cases = [
            (
                "[fallback]\npo_token_url = \"ftp://x\"",
                "fallback.po_token_url",
            ),
            ("[fallback]\nplugin_dir = \"/p\"", "fallback.plugin_dir"),
            (
                "[fallback]\npo_token_url = \"http://b:4416\"\nplugin_dir = \"p\"",
                "fallback.plugin_dir",
            ),
            (
                "[fallback]\ncookies_file = \"c.txt\"",
                "fallback.cookies_file",
            ),
            ("[fallback]\nsticky_for = \"0s\"", "fallback.sticky_for"),
            ("[fallback]\nmin_height = 1440", "fallback.min_height"),
            ("[canary]\nvideo_id = \"short\"", "canary.video_id"),
            ("[canary]\ninterval = \"1m\"", "canary.interval"),
            (
                "[canary]\ninterval = \"10m\"\njitter = \"11m\"",
                "canary.jitter",
            ),
        ];
        for (toml, key) in cases {
            let text = error_text(toml);
            assert!(text.contains(&format!("`{key}`")), "{toml}: {text}");
        }
    }

    #[test]
    fn notify_is_off_by_default_and_takes_one_target() {
        assert!(Config::from_toml(MINIMAL).unwrap().notify.is_none());

        let hook = secret_file("https://discord.com/api/webhooks/123456789/abc-DEF_9\n");
        let config = Config::from_toml(&format!(
            "{MINIMAL}[notify]\ndiscord_webhook_file = {:?}\n",
            hook.path()
        ))
        .unwrap();
        assert!(matches!(
            &config.notify,
            Some(NotifyTarget::Discord { webhook }) if webhook.expose().ends_with("/abc-DEF_9")
        ));
        assert!(!format!("{config:?}").contains("abc-DEF_9"));

        let token = secret_file("relay-token");
        let config = Config::from_toml(&format!(
            "{MINIMAL}[notify]\ncard_relay_url = \"http://192.0.2.10:9940/tubarr\"\ncard_relay_token_file = {:?}\n",
            token.path()
        ))
        .unwrap();
        let Some(NotifyTarget::CardRelay { url, token: t }) = &config.notify else {
            panic!("{:?}", config.notify);
        };
        // The endpoint is used as written, no slash added.
        assert_eq!(url.as_str(), "http://192.0.2.10:9940/tubarr");
        assert_eq!(t.expose(), "relay-token");
    }

    #[test]
    fn bad_notify_settings_name_their_key_without_the_url() {
        let not_discord = secret_file("https://example.com/api/webhooks/1/secret-token");
        let text = error_text(&format!(
            "[notify]\ndiscord_webhook_file = {:?}\n",
            not_discord.path()
        ));
        assert!(text.contains("`notify.discord_webhook_file`"), "{text}");
        assert!(!text.contains("secret-token"), "{text}");

        let http = secret_file("http://discord.com/api/webhooks/1/tok");
        let text = error_text(&format!(
            "[notify]\ndiscord_webhook_file = {:?}\n",
            http.path()
        ));
        assert!(text.contains("`notify.discord_webhook_file`"), "{text}");

        let token = secret_file("t");
        let cases = [
            (
                "[notify]\ncard_relay_url = \"http://relay/tubarr\"\n".to_owned(),
                "notify.card_relay_token_file",
            ),
            (
                format!("[notify]\ncard_relay_token_file = {:?}\n", token.path()),
                "notify.card_relay_url",
            ),
            (
                format!(
                    "[notify]\ncard_relay_url = \"ftp://relay/x\"\ncard_relay_token_file = {:?}\n",
                    token.path()
                ),
                "notify.card_relay_url",
            ),
            (
                format!(
                    "[notify]\ndiscord_webhook_file = {:?}\ncard_relay_url = \"http://relay/x\"\ncard_relay_token_file = {:?}\n",
                    token.path(),
                    token.path()
                ),
                "notify",
            ),
        ];
        for (toml, key) in cases {
            let text = error_text(&toml);
            assert!(text.contains(&format!("`{key}`")), "{toml}: {text}");
        }
    }
}
