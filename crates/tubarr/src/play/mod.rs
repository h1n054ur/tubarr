//! `GET` and `HEAD /play/{video_id}`: the virtual muxed MP4 ([`vfile`]) served with Range support.
//! The id may also list an episode's parts (`/play/<part1>,<part2>`, see [`parts`]), which play as
//! one file. `/play/{video_id}/audio` serves the same episode as an M4A with the audio track only,
//! for listening to a video podcast: the same lookups, tables and cached fragments, a smaller moov.
//!
//! The header comes from memory. Building it takes every moof of both source files, read once per
//! video and kept on disk ([`table`]). The sample data is fetched from YouTube as the response
//! streams, one source fragment's data at a time, and kept in a byte-bounded LRU, because ffmpeg's
//! probing and seeking ask for the same small ranges again and again. When YouTube starts refusing
//! the stream URLs (401, 403 or 410: expired, bound to another IP, or replaced by a re-encode) the
//! lookup is refreshed once, for the part being read.
//!
//! A play that starts at the header of a file built from an old lookup first checks its URLs with
//! a one-byte read of each source, so dead URLs are looked up again before the player holds the
//! header rather than cutting its first media read. The sizes googlevideo reports in those reads
//! tell a re-encode apart even when yt-dlp gave no `filesize`.
//!
//! A grab queues its videos for a background read of their tables ([`prebuild`]), so most first
//! plays find them on disk.
//!
//! Only videos Tubarr released, pinned or grabbed play ([`PlayState::with_release_check`]). Any
//! other id gets a 404 before a lookup or a table file, so the endpoint is no YouTube relay.

pub mod bmff;
pub mod parts;
pub mod prebuild;
pub mod table;
pub mod vfile;

use std::{
    collections::HashMap,
    io,
    net::IpAddr,
    ops::Range,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{Extensions, HeaderMap, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use bytes::Bytes;
use futures_util::{StreamExt, future::try_join_all, stream};
use lru::LruCache;
use serde::Serialize;
use tokio::sync::{Semaphore, mpsc};

use crate::{
    metrics::METRICS,
    network::{self, OutboundProxy},
    ratelimit::Lane,
    resolver::{ResolveCache, ResolveError, Resolved, Resolver, StreamFormat},
    store::Db,
};
use bmff::BmffError;
use parts::PartsError;
use prebuild::{PlayGate, Prebuilds};
use table::TrackTable;
use vfile::{
    BuildError, Fragment, Layout, Parsed, PartIndex, Piece, Track, TrackData, TrackIndex,
    VirtualFile,
};

/// The client for googlevideo and the caption host. Connections stay open between fragments.
fn media_client(proxy: Option<&OutboundProxy>) -> Result<reqwest::Client, reqwest::Error> {
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(90));
    network::youtube_client(builder, proxy)?.build()
}

/// Tunables for the play endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlayConfig {
    /// Upper bound for fragments kept in memory.
    pub fragment_cache_bytes: usize,
    /// How much of each source file is read to find its moov and sidx.
    pub probe_bytes: u64,
    /// The longest a play request waits for its limiter slot before it answers 503 instead. Plays
    /// go ahead of background lookups, so one play waits at most one interval (10 s) plus jitter.
    /// 20 s also covers a second play queued at the same moment, and still leaves room for the
    /// yt-dlp run inside the half minute a player waits for its first byte.
    pub max_lookup_wait: Duration,
    /// Whether a grab queues its videos for a background read of their sample tables.
    pub prebuild_tables: bool,
    /// Rest between two background table reads, so they never come as one long burst.
    pub prebuild_pause: Duration,
    /// How long a failed background read waits before its second try. Each later try waits twice
    /// as long as the one before.
    pub prebuild_retry: Duration,
    /// First plays reading their file heads and sample tables at once. Plays of other files wait
    /// for a turn; plays of a file being built wait for that build and share it.
    pub max_builds: usize,
    /// Responses streaming sample data at once. A `GET` past it answers 503 with a `Retry-After`.
    /// 0 sets no cap.
    pub max_streams: usize,
}

impl Default for PlayConfig {
    fn default() -> Self {
        Self {
            fragment_cache_bytes: 96 * 1024 * 1024,
            probe_bytes: 64 * 1024,
            max_lookup_wait: Duration::from_secs(20),
            prebuild_tables: true,
            prebuild_pause: Duration::from_secs(2),
            prebuild_retry: Duration::from_secs(10 * 60),
            max_builds: 2,
            max_streams: 8,
        }
    }
}

/// Header reads larger than this mean the file is not what Tubarr expects.
pub const MAX_HEAD_BYTES: u64 = 16 * 1024 * 1024;
/// Virtual files kept between requests. Each holds its moov, up to about a MB for a long episode.
const MAX_FILES: usize = 16;
/// Bytes read from the start of each fragment to get its moof. A moof of a few hundred samples
/// takes a few KB; a larger one is read again whole.
const MOOF_PROBE: u64 = 16 * 1024;
/// Moof reads closer together than this are joined into one request: the fragments in between
/// (short audio ones) cost less to read than another round trip.
const MOOF_GAP: u64 = 512 * 1024;
/// Largest joined moof read.
const MOOF_SPAN: u64 = 8 * 1024 * 1024;
/// Moof reads in flight at once, per source file.
const MOOF_READS: usize = 16;
/// The `Retry-After` of a play refused because `max_streams` responses are already streaming.
const STREAMS_RETRY_AFTER: Duration = Duration::from_secs(5);
/// How long googlevideo URLs live: their `expire=` is this long after the lookup.
const URL_LIFETIME: Duration = Duration::from_secs(6 * 3600);
/// A play that starts at the header checks the URLs first once both the lookup and the last time
/// they were seen working are older than this. Within the first hour of six they rarely die, and
/// a player opens a file with several header reads, so only the first of those pays the check.
const STALE_AFTER: Duration = Duration::from_secs(3600);

#[derive(Debug, thiserror::Error)]
pub enum PlayError {
    #[error(transparent)]
    Resolve(#[from] ResolveError),
    #[error("YouTube refused the stream even after a fresh lookup")]
    Expired,
    #[error("the fresh lookup offers different formats than the stream being played")]
    FormatsChanged,
    #[error("upstream: {0}")]
    Upstream(String),
    #[error("unexpected file layout: {0}")]
    Layout(#[from] BmffError),
    #[error(transparent)]
    Parts(#[from] PartsError),
    /// The parts of a multi-part episode cannot be joined into one file.
    #[error("{0}")]
    Incompatible(String),
}

impl From<BuildError> for PlayError {
    fn from(e: BuildError) -> Self {
        match e {
            BuildError::Layout(e) => Self::Layout(e),
            m @ BuildError::Mismatch { .. } => Self::Incompatible(m.to_string()),
        }
    }
}

/// Why an upstream read failed. A refusal is told apart because it means the URL is dead and a
/// fresh lookup is needed.
#[derive(Debug)]
enum FetchError {
    Refused(StatusCode),
    Other(String),
}

/// The returned span and optional file size of one satisfied upstream byte range.
fn response_range(
    headers: &HeaderMap,
    start: u64,
    end: u64,
) -> Result<(u64, Option<u64>), FetchError> {
    let invalid = || FetchError::Other("invalid upstream Content-Range".into());
    let decimal = |value: &str| -> Option<u64> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        value.parse().ok()
    };
    let requested = end
        .checked_sub(start)
        .and_then(|n| n.checked_add(1))
        .ok_or_else(invalid)?;
    let mut values = headers.get_all(header::CONTENT_RANGE).iter();
    let value = values.next().ok_or_else(invalid)?;
    if values.next().is_some() {
        return Err(invalid());
    }
    let value = value
        .to_str()
        .map_err(|_| invalid())?
        .trim_matches([' ', '\t']);
    let (unit, range) = value.split_once(' ').ok_or_else(invalid)?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return Err(invalid());
    }
    let (bounds, total) = range.split_once('/').ok_or_else(invalid)?;
    let (first, last) = bounds.split_once('-').ok_or_else(invalid)?;
    let first = decimal(first).ok_or_else(invalid)?;
    let last = decimal(last).ok_or_else(invalid)?;
    let span = last
        .checked_sub(first)
        .and_then(|n| n.checked_add(1))
        .ok_or_else(invalid)?;
    if first != start || last > end || span > requested {
        return Err(invalid());
    }
    let total = if total == "*" {
        None
    } else {
        let total = decimal(total).ok_or_else(invalid)?;
        if total <= last {
            return Err(invalid());
        }
        Some(total)
    };
    for length in headers.get_all(header::CONTENT_LENGTH) {
        let length = length
            .to_str()
            .ok()
            .and_then(|s| decimal(s.trim_matches([' ', '\t'])));
        if length != Some(span) {
            return Err(FetchError::Other(
                "upstream Content-Length disagrees with Content-Range".into(),
            ));
        }
    }
    Ok((span, total))
}

/// What googlevideo answers for a signed URL that no longer works: 403 once it has expired or is
/// used from another IP, 401 and 410 once the video was processed again and the old file is gone.
fn is_refusal(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN | StatusCode::GONE
    )
}

impl From<FetchError> for PlayError {
    fn from(e: FetchError) -> Self {
        match e {
            FetchError::Refused(_) => Self::Expired,
            FetchError::Other(m) => Self::Upstream(m),
        }
    }
}

/// The lookups of every part (one for a plain video) and the virtual file built from them.
#[derive(Debug, Clone)]
struct Prepared {
    /// The ids joined as in the path, with `/audio` for an audio-only file: the key for
    /// [`PlayState::files`].
    key: String,
    parts: Vec<Resolved>,
    file: Arc<VirtualFile>,
    /// The source file sizes of every part, as googlevideo reported them when the file was built.
    sizes: Vec<Sizes>,
    /// When the URLs of `parts` were last seen working by a header read or a check.
    checked: Option<SystemTime>,
}

impl Prepared {
    /// Whether the URLs should be checked before the header is served: neither the lookup (dated
    /// from its `expire=`) nor the last check is recent.
    fn stale(&self, now: SystemTime) -> bool {
        let issued = self
            .parts
            .iter()
            .map(|r| r.expires_at.and_then(|e| e.checked_sub(URL_LIFETIME)))
            .min()
            .flatten();
        let since = issued.max(self.checked).unwrap_or(SystemTime::UNIX_EPOCH);
        now.duration_since(since).is_ok_and(|age| age > STALE_AFTER)
    }
}

/// The sizes of a part's video and audio source files, where googlevideo gave them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Sizes {
    video: Option<u64>,
    audio: Option<u64>,
}

impl Sizes {
    fn of(&self, track: Track) -> Option<u64> {
        match track {
            Track::Video => self.video,
            Track::Audio => self.audio,
        }
    }

    /// False only when both sides know a size and they differ.
    fn matches(&self, other: &Sizes) -> bool {
        let same = |a: Option<u64>, b: Option<u64>| a.zip(b).is_none_or(|(a, b)| a == b);
        same(self.video, other.video) && same(self.audio, other.audio)
    }
}

/// Video id, track and offset in the source file of a chunk of sample data.
type FragmentKey = (String, Track, u64);

struct FragmentCache {
    entries: LruCache<FragmentKey, Bytes>,
    bytes: usize,
    limit: usize,
}

impl FragmentCache {
    fn get(&mut self, key: &FragmentKey) -> Option<Bytes> {
        self.entries.get(key).cloned()
    }

    fn put(&mut self, key: FragmentKey, value: Bytes) {
        if value.len() > self.limit {
            return;
        }
        self.bytes += value.len();
        if let Some(old) = self.entries.put(key, value) {
            self.bytes -= old.len();
        }
        while self.bytes > self.limit {
            match self.entries.pop_lru() {
                Some((_, old)) => self.bytes -= old.len(),
                None => break,
            }
        }
    }
}

/// Everything the play endpoint needs. Shared behind an `Arc`.
pub struct PlayState<C> {
    resolver: Arc<Resolver<C>>,
    client: reqwest::Client,
    config: PlayConfig,
    files: Mutex<HashMap<String, Arc<Prepared>>>,
    /// One turn per file being built, so a burst of requests for one file builds it once while
    /// other files build alongside.
    building: Turns<String>,
    /// Turns to read heads and tables for a new file, `max_builds` of them.
    build_slots: Semaphore,
    builds: Arc<AtomicUsize>,
    builds_waiting: Arc<AtomicUsize>,
    /// Responses streaming sample data right now.
    streams: Arc<AtomicUsize>,
    fragments: Mutex<FragmentCache>,
    /// Where sample tables are kept between plays. Without it every build reads the moofs again.
    index_dir: Option<PathBuf>,
    /// One turn per table file, so a play and a background read of the same table read it once.
    tables: Turns<PathBuf>,
    /// Plays reading file heads or tables right now. Background reads wait while there are any.
    gate: PlayGate,
    prebuilds: Prebuilds,
    /// The released, pinned and grabbed videos, the only ones served. `None` serves any id.
    released: Option<Db>,
}

impl<C> std::fmt::Debug for PlayState<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlayState")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl<C: ResolveCache + 'static> PlayState<C> {
    /// `proxy` is `[network] proxy`: the relay's media and caption fetches go through it.
    pub fn new(
        resolver: Arc<Resolver<C>>,
        config: PlayConfig,
        proxy: Option<&OutboundProxy>,
    ) -> Result<Self, reqwest::Error> {
        let client = media_client(proxy)?;
        Ok(Self {
            resolver,
            client,
            config,
            files: Mutex::new(HashMap::new()),
            building: Turns::default(),
            build_slots: Semaphore::new(config.max_builds.max(1)),
            builds: Arc::default(),
            builds_waiting: Arc::default(),
            streams: Arc::default(),
            fragments: Mutex::new(FragmentCache {
                entries: LruCache::unbounded(),
                bytes: 0,
                limit: config.fragment_cache_bytes,
            }),
            index_dir: None,
            tables: Turns::default(),
            gate: PlayGate::default(),
            prebuilds: Prebuilds::default(),
            released: None,
        })
    }

    /// Keeps sample tables in `dir`, created when first needed.
    pub fn with_index_dir(mut self, dir: PathBuf) -> Self {
        self.index_dir = Some(dir);
        self
    }

    /// Keeps the queue of background table reads in `db` too, so a restart picks it up again
    /// ([`Self::resume_prebuilds`]).
    pub fn with_prebuild_store(mut self, db: Db) -> Self {
        self.prebuilds.store = Some(db);
        self
    }

    /// Serves only the videos `db` holds as released, pinned or grabbed.
    pub fn with_release_check(mut self, db: Db) -> Self {
        self.released = Some(db);
        self
    }

    /// Adds the video of every table file in `index_dir` to the released videos. A table is only
    /// ever read for a video that played or was grabbed, so this brings back the ids of `.strm`
    /// files written before the list existed whose release and grab rows are gone since. Called
    /// once at start, before `/play` serves.
    pub async fn remember_indexed_videos(&self) {
        let (Some(db), Some(dir)) = (&self.released, &self.index_dir) else {
            return;
        };
        let mut entries = match tokio::fs::read_dir(dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!(dir = %dir.display(), error = %e, "cannot list the sample tables");
                return;
            }
        };
        let mut ids = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|n| n.ends_with(".tbt")) else {
                continue;
            };
            if let Some((id, _)) = name.split_once('.')
                && crate::resolver::is_video_id(id)
            {
                ids.push(id.to_owned());
            }
        }
        ids.sort_unstable();
        ids.dedup();
        match db.remember_released(&ids).await {
            Ok(0) => {}
            Ok(added) => tracing::info!(
                added,
                "videos with sample tables added to the released ones"
            ),
            Err(e) => tracing::warn!(error = %e, "cannot add the videos with sample tables"),
        }
    }

    /// `None` when every id may play, else the response that refuses them.
    async fn refusal(&self, ids: &[String], peer: Option<IpAddr>) -> Option<Response> {
        let db = self.released.as_ref()?;
        match db.all_released(ids).await {
            Ok(true) => None,
            Ok(false) => {
                tracing::debug!(
                    video_ids = ids.join(","),
                    ?peer,
                    "play refused, not a released video"
                );
                Some((StatusCode::NOT_FOUND, "not found").into_response())
            }
            Err(e) => {
                tracing::warn!(error = %e, "play refused, cannot read the released videos");
                Some(StatusCode::SERVICE_UNAVAILABLE.into_response())
            }
        }
    }

    /// The client for YouTube, shared with the caption downloads so they reuse its connections.
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    fn lock_files(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Prepared>>> {
        self.files.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_fragments(&self) -> std::sync::MutexGuard<'_, FragmentCache> {
        self.fragments.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn remember(&self, prepared: Arc<Prepared>) {
        let mut files = self.lock_files();
        if files.len() >= MAX_FILES
            && !files.contains_key(&prepared.key)
            && let Some(old) = files.keys().next().cloned()
        {
            files.remove(&old);
        }
        files.insert(prepared.key.clone(), prepared);
    }

    fn forget(&self, key: &str) {
        self.lock_files().remove(key);
    }

    /// A built file for these lookups if one exists. Lookups with new URLs for the same formats
    /// reuse the old file, since the bytes behind them are the same. A re-encode keeps the format
    /// ids, so when yt-dlp gave no size the new URLs are asked for theirs.
    async fn reuse(&self, key: &str, parts: &[Resolved]) -> Option<Arc<Prepared>> {
        let known = self.lock_files().get(key).cloned()?;
        if known.parts == parts {
            return Some(known);
        }
        let same = known.parts.len() == parts.len()
            && known
                .parts
                .iter()
                .zip(parts)
                .all(|(a, b)| same_formats(a, b));
        if !same {
            return None;
        }
        for ((old, new), sizes) in known.parts.iter().zip(parts).zip(&known.sizes) {
            if !sizes_known(old, new) && !self.probe(new).await.ok()?.matches(sizes) {
                return None;
            }
        }
        let p = Arc::new(Prepared {
            key: key.to_owned(),
            parts: parts.to_vec(),
            checked: None,
            ..Prepared::clone(&known)
        });
        self.remember(p.clone());
        Some(p)
    }

    /// [`Self::prepare`] for a request that starts at the byte `start` gives for the file's size
    /// (`None` when it asks for none of it). One that starts in the header of a stale file has its
    /// URLs checked first; a seek into the mdat does not wait for that.
    async fn prepare_at(
        &self,
        ids: &[String],
        layout: Layout,
        start: impl Fn(u64) -> Option<u64>,
    ) -> Result<Arc<Prepared>, PlayError> {
        let p = self.prepare(ids, layout).await?;
        let in_head = start(p.file.size()).is_some_and(|s| s < p.file.head().len() as u64);
        if !in_head || !p.stale(self.resolver.now()) {
            return Ok(p);
        }
        match self.revalidate(&p).await? {
            Some(p) => Ok(p),
            None => {
                tracing::info!(
                    video_id = p.key,
                    "a source changed size, building the file again"
                );
                self.forget(&p.key);
                self.prepare(ids, layout).await
            }
        }
    }

    /// Checks the URLs of every part with a one-byte read of each source, looking a part up again
    /// through the play lane if they are refused. `None` when a source is no longer the one the
    /// file was built from.
    async fn revalidate(&self, p: &Prepared) -> Result<Option<Arc<Prepared>>, PlayError> {
        let now = self.resolver.now();
        let mut next = p.clone();
        for (i, part) in p.parts.iter().enumerate() {
            let sizes = match self.probe(part).await {
                Err(FetchError::Refused(status)) => {
                    tracing::info!(video_id = %part.meta.id, %status, "stream URL refused before the header was served, looking the video up again");
                    let fresh = match self.refreshed(part).await {
                        Err(PlayError::FormatsChanged) => return Ok(None),
                        other => other?,
                    };
                    let sizes = self.probe(&fresh).await?;
                    next.parts[i] = fresh;
                    sizes
                }
                other => other?,
            };
            if !p.sizes[i].matches(&sizes) {
                return Ok(None);
            }
        }
        next.checked = Some(now);
        let next = Arc::new(next);
        self.remember(next.clone());
        Ok(Some(next))
    }

    /// The sizes of a part's two source files, from a one-byte read of each.
    async fn probe(&self, part: &Resolved) -> Result<Sizes, FetchError> {
        let (video, audio) =
            tokio::try_join!(self.fetch(&part.video, 0, 0), self.fetch(&part.audio, 0, 0))?;
        Ok(Sizes {
            video: video.1,
            audio: audio.1,
        })
    }

    /// Looks up every part (each through the shared limiter) and builds or reuses their file.
    async fn prepare(&self, ids: &[String], layout: Layout) -> Result<Arc<Prepared>, PlayError> {
        let mut key = ids.join(",");
        if layout == Layout::AudioOnly {
            key.push_str("/audio");
        }
        let parts = self.resolve_parts(ids).await?;
        if let Some(p) = self.reuse(&key, &parts).await {
            return Ok(p);
        }
        let _building = self.building.take(&key).await;
        // A build that ran meanwhile may have refreshed refused URLs and cached the new lookup, so
        // ask again (from the cache) instead of hitting the same refusal and refreshing once more.
        let parts = self.resolve_parts(ids).await?;
        if let Some(p) = self.reuse(&key, &parts).await {
            return Ok(p);
        }
        let _slot = self.build_slot().await;
        let (parts, indexes): (Vec<Resolved>, Vec<PartIndex>) =
            try_join_all(parts.iter().map(|r| self.index_part(r)))
                .await?
                .into_iter()
                .unzip();
        let file = VirtualFile::build(&indexes, layout)?;
        let sizes = indexes
            .iter()
            .map(|i| Sizes {
                video: i.video.size,
                audio: i.audio.size,
            })
            .collect();
        tracing::info!(
            video_id = key,
            video = %parts[0].video.format_id,
            parts = parts.len(),
            size = file.size(),
            "built virtual file"
        );
        let p = Arc::new(Prepared {
            key,
            parts,
            file: Arc::new(file),
            sizes,
            checked: Some(self.resolver.now()),
        });
        self.remember(p.clone());
        Ok(p)
    }

    async fn resolve_parts(&self, ids: &[String]) -> Result<Vec<Resolved>, PlayError> {
        let wait = self.config.max_lookup_wait;
        Ok(try_join_all(
            ids.iter()
                .map(|id| self.resolver.resolve_for_play(id, wait)),
        )
        .await?)
    }

    /// The indexes of one part, looking it up again once if its URLs have expired.
    async fn index_part(&self, resolved: &Resolved) -> Result<(Resolved, PartIndex), PlayError> {
        match self.read_part(resolved, Lane::Play).await {
            Err(PlayError::Expired) => {
                tracing::info!(video_id = %resolved.meta.id, "stream URL refused while reading its header, looking the video up again");
                let fresh = self
                    .resolver
                    .refresh(resolved, self.config.max_lookup_wait)
                    .await?;
                let index = self.read_part(&fresh, Lane::Play).await?;
                Ok((fresh, index))
            }
            other => Ok((resolved.clone(), other?)),
        }
    }

    /// The indexes of one part, for a play or for a background read ([`Lane`]).
    async fn read_part(&self, resolved: &Resolved, lane: Lane) -> Result<PartIndex, PlayError> {
        let id = &resolved.meta.id;
        let (video, audio) = tokio::try_join!(
            self.read_track(id, &resolved.video, lane),
            self.read_track(id, &resolved.audio, lane)
        )?;
        Ok(PartIndex { video, audio })
    }

    async fn read_track(
        &self,
        video_id: &str,
        format: &StreamFormat,
        lane: Lane,
    ) -> Result<TrackData, PlayError> {
        let (index, size) = self.read_index(format, lane).await?;
        let table = self.read_table(video_id, format, &index, lane).await?;
        Ok(TrackData {
            moov: index.moov,
            table,
            size,
        })
    }

    /// The samples of every fragment of a source file, from the disk cache or from their moofs.
    /// One read at a time per table: a second reader waits and then finds it on disk.
    async fn read_table(
        &self,
        video_id: &str,
        format: &StreamFormat,
        index: &TrackIndex,
        lane: Lane,
    ) -> Result<TrackTable, PlayError> {
        let path = self
            .index_dir
            .as_ref()
            .map(|d| table::cache_path(d, video_id, &format.format_id, index.end()));
        let _turn = match &path {
            Some(path) => Some(self.tables.take(path).await),
            None => None,
        };
        let fingerprint = index.fingerprint();
        if let Some(path) = &path
            && let Some((stored, table)) = table::load(path).await
        {
            if stored == fingerprint {
                METRICS.table_cache_hits.inc();
                return Ok(table);
            }
            tracing::warn!(
                video_id,
                format = %format.format_id,
                path = %path.display(),
                "cached sample table was read from another stream, reading the moofs again"
            );
        }
        let started = Instant::now();
        METRICS.table_builds.inc();
        let _reading = self.gate.enter(lane);
        let defaults = bmff::trex_defaults(&index.moov)?;
        let mut reads = stream::iter(moof_spans(&index.fragments))
            .map(|(span, frags)| async move {
                self.gate.wait(lane).await;
                let bytes = self.get_range(format, span.start, span.end - 1).await?;
                Ok::<_, PlayError>((span.start, frags, bytes))
            })
            .buffered(MOOF_READS);
        let mut table = TrackTable::default();
        let mut requests = 0usize;
        while let Some(read) = reads.next().await {
            let (start, frags, bytes) = read?;
            requests += 1;
            for f in &index.fragments[frags] {
                let window = bytes.get((f.offset - start) as usize..).unwrap_or_default();
                let size = moof_size(window, f)?;
                let runs = match window.get(..size) {
                    Some(moof) => bmff::parse_moof(moof, f.offset, defaults)?,
                    None => {
                        requests += 1;
                        self.gate.wait(lane).await;
                        let end = f.offset + size as u64 - 1;
                        let moof = self.get_range(format, f.offset, end).await?;
                        bmff::parse_moof(&moof, f.offset, defaults)?
                    }
                };
                table.push_runs(runs);
            }
        }
        tracing::info!(
            video_id,
            format = %format.format_id,
            fragments = index.fragments.len(),
            samples = table.len(),
            requests,
            ms = started.elapsed().as_millis() as u64,
            background = lane == Lane::Background,
            "read sample table"
        );
        if lane == Lane::Background {
            self.prebuilds.table_read();
        }
        if let Some(path) = &path
            && let Err(e) = table::store(path, &fingerprint, &table).await
        {
            tracing::warn!(video_id, path = %path.display(), error = %e, "cannot keep the sample table");
        }
        Ok(table)
    }

    /// The index of a source file and its size as googlevideo reports it.
    async fn read_index(
        &self,
        format: &StreamFormat,
        lane: Lane,
    ) -> Result<(TrackIndex, Option<u64>), PlayError> {
        let _reading = self.gate.enter(lane);
        let mut want = self.config.probe_bytes;
        loop {
            self.gate.wait(lane).await;
            let (head, size) = self.fetch(format, 0, want - 1).await?;
            match TrackIndex::parse(&head)? {
                Parsed::Index(index) => return Ok((index, size)),
                Parsed::NeedBytes(n) if n > want && n <= MAX_HEAD_BYTES => want = n,
                Parsed::NeedBytes(n) => {
                    return Err(PlayError::Upstream(format!(
                        "format {} needs {n} header bytes, got {}",
                        format.format_id,
                        head.len()
                    )));
                }
            }
        }
    }

    /// A turn to build a new file, held until it is built or failed. Waits while `max_builds`
    /// builds run.
    async fn build_slot(&self) -> (tokio::sync::SemaphorePermit<'_>, Counted) {
        let waiting = Counted::enter(&self.builds_waiting);
        let permit = self
            .build_slots
            .acquire()
            .await
            .expect("the build semaphore is never closed");
        drop(waiting);
        (permit, Counted::enter(&self.builds))
    }

    /// A turn to stream a response, `None` while `max_streams` responses already stream.
    fn stream_slot(&self) -> Option<Counted> {
        Counted::enter_below(&self.streams, self.config.max_streams)
    }

    /// Builds and streams running now, against their caps, for the Health page.
    pub fn load(&self) -> PlayLoad {
        PlayLoad {
            builds: self.builds.load(Ordering::Relaxed),
            builds_waiting: self.builds_waiting.load(Ordering::Relaxed),
            max_builds: self.config.max_builds.max(1),
            streams: self.streams.load(Ordering::Relaxed),
            max_streams: self.config.max_streams,
        }
    }

    /// Bytes `start..=end` of a source file. A short answer is returned as is (for header reads past
    /// the end of a small file); fragment reads check the length themselves.
    async fn get_range(
        &self,
        format: &StreamFormat,
        start: u64,
        end: u64,
    ) -> Result<Bytes, FetchError> {
        Ok(self.fetch(format, start, end).await?.0)
    }

    /// [`Self::get_range`] with the size of the whole file, from the answer's Content-Range.
    async fn fetch(
        &self,
        format: &StreamFormat,
        start: u64,
        end: u64,
    ) -> Result<(Bytes, Option<u64>), FetchError> {
        let mut request = self.client.get(&format.url);
        for (name, value) in &format.http_headers {
            if let (Ok(name), Ok(value)) = (
                header::HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                request = request.header(name, value);
            }
        }
        let mut response = request
            .header(header::RANGE, format!("bytes={start}-{end}"))
            .send()
            .await
            .map_err(|e| FetchError::Other(e.without_url().to_string()))?;
        match response.status() {
            StatusCode::PARTIAL_CONTENT => {}
            s if is_refusal(s) => return Err(FetchError::Refused(s)),
            s => {
                return Err(FetchError::Other(format!(
                    "format {} answered {s} for bytes {start}-{end}",
                    format.format_id
                )));
            }
        }
        let (span, size) = response_range(response.headers(), start, end)?;
        let invalid_length =
            || FetchError::Other("upstream body length disagrees with Content-Range".into());
        let mut bytes = Vec::new();
        let mut received = 0_u64;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| FetchError::Other(e.without_url().to_string()))?
        {
            let length = u64::try_from(chunk.len()).map_err(|_| invalid_length())?;
            let next = received.checked_add(length).ok_or_else(invalid_length)?;
            if next > span || bytes.len().checked_add(chunk.len()).is_none() {
                return Err(invalid_length());
            }
            bytes.extend_from_slice(&chunk);
            received = next;
        }
        if received != span {
            return Err(invalid_length());
        }
        Ok((Bytes::from(bytes), size))
    }

    /// The sample data of one chunk, from the cache or from YouTube.
    async fn chunk(
        &self,
        current: &Mutex<Arc<Prepared>>,
        index: usize,
    ) -> Result<Bytes, PlayError> {
        let prepared = lock(current).clone();
        let f = prepared.file.chunk(index);
        let track = f.track;
        let part = &prepared.parts[f.part];
        let key = (part.meta.id.clone(), track, f.offset);
        if let Some(hit) = self.lock_fragments().get(&key) {
            return Ok(hit);
        }
        let end = f.offset + f.size - 1;
        let pick = |r: &Resolved| match track {
            Track::Video => r.video.clone(),
            Track::Audio => r.audio.clone(),
        };
        // A source of another size is a re-encode behind the same format ids: the layout the
        // player holds no longer fits it, and the next request builds the file again.
        let resized = |size: Option<u64>| {
            let known = prepared.sizes[f.part].of(track);
            if size.zip(known).is_some_and(|(s, k)| s != k) {
                self.forget(&prepared.key);
                return Err(PlayError::FormatsChanged);
            }
            Ok(())
        };
        let bytes = match self.fetch(&pick(part), f.offset, end).await {
            Err(FetchError::Refused(status)) => {
                tracing::info!(video_id = %key.0, %status, "stream URL refused, looking the video up again");
                let fresh = self.refreshed(part).await?;
                let (bytes, size) = self.fetch(&pick(&fresh), f.offset, end).await?;
                resized(size)?;
                self.swap_part(current, f.part, fresh);
                bytes
            }
            Ok((bytes, size)) => {
                resized(size)?;
                bytes
            }
            Err(e) => return Err(e.into()),
        };
        if bytes.len() as u64 != f.size {
            return Err(PlayError::Upstream(format!(
                "chunk at {} came back with {} of {} bytes",
                f.offset,
                bytes.len(),
                f.size
            )));
        }
        self.lock_fragments().put(key, bytes.clone());
        Ok(bytes)
    }

    /// New URLs for a part whose old ones expired. The layout stays, so the formats must match.
    async fn refreshed(&self, stale: &Resolved) -> Result<Resolved, PlayError> {
        let fresh = self
            .resolver
            .refresh(stale, self.config.max_lookup_wait)
            .await?;
        if fresh.video.url == stale.video.url && fresh.audio.url == stale.audio.url {
            return Err(PlayError::Expired);
        }
        if !same_formats(stale, &fresh) {
            // The fresh lookup is cached, so the next request builds the new layout.
            return Err(PlayError::FormatsChanged);
        }
        Ok(fresh)
    }

    /// Puts a refreshed part into the stream's current lookups, keeping parts other fragment reads
    /// may have refreshed meanwhile, and remembers the result for later requests.
    fn swap_part(&self, current: &Mutex<Arc<Prepared>>, part: usize, fresh: Resolved) {
        let mut current = lock(current);
        let mut next = Prepared::clone(&current);
        next.parts[part] = fresh;
        let next = Arc::new(next);
        *current = next.clone();
        self.remember(next);
    }

    /// Streams bytes `start..=end` as they arrive. One chunk is fetched ahead of the one being
    /// sent. An upstream failure mid-body ends the response early, which the client sees as a cut.
    fn body(
        self: Arc<Self>,
        prepared: Arc<Prepared>,
        start: u64,
        end: u64,
        slot: Option<Counted>,
    ) -> Body {
        let (tx, rx) = mpsc::channel::<Result<Bytes, io::Error>>(2);
        tokio::spawn(async move {
            let _slot = slot;
            let file = prepared.file.clone();
            let video_id = prepared.key.clone();
            let current = Mutex::new(prepared);
            let state = &self;
            let current = &current;
            let mut chunks = stream::iter(file.pieces(start, end))
                .map(|piece| {
                    let head = file.head().clone();
                    async move {
                        match piece {
                            Piece::Head(r) => Ok(head.slice(r)),
                            Piece::Chunk { index, skip, len } => state
                                .chunk(current, index)
                                .await
                                .map(|b| b.slice(skip..skip + len)),
                        }
                    }
                })
                .buffered(2);
            while let Some(chunk) = chunks.next().await {
                match chunk {
                    Ok(bytes) => {
                        if tx.send(Ok(bytes)).await.is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        tracing::warn!(video_id, error = %e, "stream cut");
                        let _ = tx.send(Err(io::Error::other(e.to_string()))).await;
                        return;
                    }
                }
            }
        });
        Body::from_stream(stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        }))
    }
}

/// Builds and streams running now, and their caps (`max_streams` 0 means none).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PlayLoad {
    pub builds: usize,
    /// First plays waiting for a build turn.
    pub builds_waiting: usize,
    pub max_builds: usize,
    pub streams: usize,
    pub max_streams: usize,
}

/// One lock per key, kept only while someone holds or waits for it.
struct Turns<K> {
    locks: Mutex<HashMap<K, Arc<tokio::sync::Mutex<()>>>>,
}

impl<K> Default for Turns<K> {
    fn default() -> Self {
        Self {
            locks: Mutex::new(HashMap::new()),
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone> Turns<K> {
    /// The turn at `key`, held until the returned guard drops.
    async fn take<Q>(&self, key: &Q) -> Turn<'_, K>
    where
        Q: ToOwned<Owned = K> + ?Sized,
    {
        let key = key.to_owned();
        let lock = lock(&self.locks).entry(key.clone()).or_default().clone();
        let guard = lock.clone().lock_owned().await;
        Turn {
            turns: self,
            key,
            lock,
            guard: Some(guard),
        }
    }
}

/// A turn at one key. Dropping it lets the next one in and forgets the key's lock once nobody else
/// holds or waits for it.
struct Turn<'a, K: std::hash::Hash + Eq> {
    turns: &'a Turns<K>,
    key: K,
    lock: Arc<tokio::sync::Mutex<()>>,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl<K: std::hash::Hash + Eq> Drop for Turn<'_, K> {
    fn drop(&mut self) {
        self.guard.take();
        let mut locks = lock(&self.turns.locks);
        // The map's and this turn's: nobody else is waiting.
        if Arc::strong_count(&self.lock) == 2 {
            locks.remove(&self.key);
        }
    }
}

/// One in a running count, taken off again on drop.
struct Counted(Arc<AtomicUsize>);

impl Counted {
    fn enter(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::Relaxed);
        Self(count.clone())
    }

    /// [`Self::enter`] while the count is below `cap`, any time when `cap` is 0.
    fn enter_below(count: &Arc<AtomicUsize>, cap: usize) -> Option<Self> {
        count
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (cap == 0 || n < cap).then_some(n + 1)
            })
            .ok()?;
        Some(Self(count.clone()))
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Byte ranges to read for the moofs of `fragments`: the first [`MOOF_PROBE`] bytes of each,
/// joined when close together, with the fragments each range covers.
fn moof_spans(fragments: &[Fragment]) -> Vec<(Range<u64>, Range<usize>)> {
    let mut out: Vec<(Range<u64>, Range<usize>)> = Vec::new();
    for (i, f) in fragments.iter().enumerate() {
        let end = f.offset + u64::from(f.size).min(MOOF_PROBE);
        match out.last_mut() {
            Some((span, frags))
                if f.offset.saturating_sub(span.end) <= MOOF_GAP
                    && end - span.start <= MOOF_SPAN =>
            {
                span.end = end;
                frags.end = i + 1;
            }
            _ => out.push((f.offset..end, i..i + 1)),
        }
    }
    out
}

/// Size of the moof a fragment starts with, read from the first bytes of the fragment.
fn moof_size(window: &[u8], f: &Fragment) -> Result<usize, PlayError> {
    let first = bmff::boxes(window)?.into_iter().next();
    match first {
        Some(b) if &b.kind == b"moof" && b.size <= u64::from(f.size) => Ok(b.size as usize),
        Some(b) => Err(BmffError::Unsupported(format!(
            "fragment at {} starts with {} of {} bytes",
            f.offset,
            String::from_utf8_lossy(&b.kind),
            b.size
        ))
        .into()),
        None => Err(PlayError::Upstream(format!(
            "fragment at {} came back too short to read",
            f.offset
        ))),
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Whether yt-dlp gave the size of every source of both lookups, so [`same_formats`] compared them.
fn sizes_known(a: &Resolved, b: &Resolved) -> bool {
    [&a.video, &a.audio, &b.video, &b.audio]
        .iter()
        .all(|f| f.filesize.is_some())
}

fn same_formats(a: &Resolved, b: &Resolved) -> bool {
    let same =
        |x: &StreamFormat, y: &StreamFormat| x.format_id == y.format_id && x.filesize == y.filesize;
    same(&a.video, &b.video) && same(&a.audio, &b.audio)
}

/// The router for the play endpoint, to be merged into the app router.
pub fn router<C: ResolveCache + 'static>(state: Arc<PlayState<C>>) -> Router {
    Router::new()
        .route("/play/{video_id}", get(play::<C>))
        .route("/play/{video_id}/audio", get(play_audio::<C>))
        .with_state(state)
}

async fn play<C: ResolveCache + 'static>(
    state: State<Arc<PlayState<C>>>,
    video_id: Path<String>,
    method: Method,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    serve(state, video_id, method, headers, &extensions, Layout::Muxed).await
}

async fn play_audio<C: ResolveCache + 'static>(
    state: State<Arc<PlayState<C>>>,
    video_id: Path<String>,
    method: Method,
    headers: HeaderMap,
    extensions: Extensions,
) -> Response {
    serve(
        state,
        video_id,
        method,
        headers,
        &extensions,
        Layout::AudioOnly,
    )
    .await
}

async fn serve<C: ResolveCache + 'static>(
    State(state): State<Arc<PlayState<C>>>,
    Path(video_id): Path<String>,
    method: Method,
    headers: HeaderMap,
    extensions: &Extensions,
    layout: Layout,
) -> Response {
    let range_header = headers.get(header::RANGE);
    let start = |size| match parse_range(range_header, size) {
        RangeRequest::Full => Some(0),
        RangeRequest::Partial(start, _) => Some(start),
        RangeRequest::Unsatisfiable => None,
    };
    let mut slot = None;
    let prepared = match parts::parse(&video_id) {
        Ok(ids) => {
            let peer = crate::auth::peer_ip(extensions);
            if let Some(refused) = state.refusal(&ids, peer).await {
                return refused;
            }
            if method == Method::GET {
                match state.stream_slot() {
                    Some(s) => slot = Some(s),
                    None => return busy(&video_id, &state, peer),
                }
            }
            state.prepare_at(&ids, layout, start).await
        }
        Err(e) => Err(e.into()),
    };
    let prepared = match prepared {
        Ok(p) => p,
        Err(e) => return error_response(&video_id, &e, &state),
    };
    let size = prepared.file.size();
    let range = parse_range(range_header, size);
    tracing::debug!(video_id, ?range, %method, ?layout, "play");

    let (status, start, end) = match range {
        RangeRequest::Full => (StatusCode::OK, 0, size.saturating_sub(1)),
        RangeRequest::Partial(start, end) => (StatusCode::PARTIAL_CONTENT, start, end),
        RangeRequest::Unsatisfiable => {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [
                    (header::CONTENT_RANGE, format!("bytes */{size}")),
                    (header::ACCEPT_RANGES, "bytes".to_owned()),
                ],
            )
                .into_response();
        }
    };
    let length = if size == 0 { 0 } else { end - start + 1 };
    let mut builder = Response::builder()
        .status(status)
        .header(
            header::CONTENT_TYPE,
            match layout {
                Layout::Muxed => "video/mp4",
                Layout::AudioOnly => "audio/mp4",
            },
        )
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, length);
    if status == StatusCode::PARTIAL_CONTENT {
        builder = builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{size}"));
    }
    let body = if method == Method::HEAD || length == 0 {
        Body::empty()
    } else {
        state.clone().body(prepared, start, end, slot)
    };
    builder
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// The answer to a play past `max_streams`.
fn busy<C>(video_id: &str, state: &PlayState<C>, peer: Option<IpAddr>) -> Response {
    let max_streams = state.config.max_streams;
    tracing::warn!(
        video_id,
        ?peer,
        max_streams,
        "play refused, [play] max_streams responses are already streaming"
    );
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(
            header::RETRY_AFTER,
            STREAMS_RETRY_AFTER.as_secs().to_string(),
        )],
        format!("{max_streams} streams are already playing, try again shortly"),
    )
        .into_response()
}

fn error_response<C: ResolveCache + 'static>(
    video_id: &str,
    e: &PlayError,
    state: &PlayState<C>,
) -> Response {
    let status = match e {
        PlayError::Resolve(r) => match r {
            ResolveError::InvalidId(_)
            | ResolveError::Unavailable(_)
            | ResolveError::MembersOnly(_)
            | ResolveError::NoFormats { .. }
            | ResolveError::Live => StatusCode::NOT_FOUND,
            ResolveError::AgeRestricted(_) => StatusCode::FORBIDDEN,
            ResolveError::GeoBlocked(_) => StatusCode::UNAVAILABLE_FOR_LEGAL_REASONS,
            ResolveError::Busy(wait) => {
                tracing::info!(video_id, error = %e, "play refused, its lookup would wait too long");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(header::RETRY_AFTER, wait.as_secs().max(1).to_string())],
                    e.to_string(),
                )
                    .into_response();
            }
            ResolveError::RateLimited(_) | ResolveError::BotCheck(_) => {
                let status = state.resolver.limiter().status();
                let wait = status
                    .paused_for
                    .unwrap_or(status.next_in)
                    .max(Duration::from_secs(30));
                tracing::warn!(video_id, error = %e, "play refused while YouTube throttles");
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [(header::RETRY_AFTER, wait.as_secs().to_string())],
                    e.to_string(),
                )
                    .into_response();
            }
            ResolveError::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            ResolveError::Spawn { .. } | ResolveError::Parse(_) | ResolveError::Failed { .. } => {
                StatusCode::BAD_GATEWAY
            }
        },
        PlayError::Parts(_) => StatusCode::NOT_FOUND,
        PlayError::Incompatible(_) => StatusCode::CONFLICT,
        PlayError::Expired
        | PlayError::FormatsChanged
        | PlayError::Upstream(_)
        | PlayError::Layout(_) => StatusCode::BAD_GATEWAY,
    };
    if status.is_server_error() {
        tracing::warn!(video_id, error = %e, "play failed");
    }
    (status, e.to_string()).into_response()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeRequest {
    Full,
    /// First and last byte, both inclusive.
    Partial(u64, u64),
    Unsatisfiable,
}

/// A single `bytes=` range. Headers that cannot be parsed, or ask for several ranges, are ignored
/// and the whole file is sent, as RFC 9110 allows.
fn parse_range(value: Option<&HeaderValue>, size: u64) -> RangeRequest {
    let Some(spec) = value
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().strip_prefix("bytes="))
    else {
        return RangeRequest::Full;
    };
    if spec.contains(',') {
        return RangeRequest::Full;
    }
    let Some((first, last)) = spec.split_once('-') else {
        return RangeRequest::Full;
    };
    let (first, last) = (first.trim(), last.trim());
    if first.is_empty() {
        return match last.parse::<u64>() {
            Ok(0) => RangeRequest::Unsatisfiable,
            Ok(_) if size == 0 => RangeRequest::Unsatisfiable,
            Ok(n) => RangeRequest::Partial(size.saturating_sub(n), size - 1),
            Err(_) => RangeRequest::Full,
        };
    }
    let Ok(start) = first.parse::<u64>() else {
        return RangeRequest::Full;
    };
    let end = if last.is_empty() {
        None
    } else {
        match last.parse::<u64>() {
            Ok(e) if e >= start => Some(e),
            _ => return RangeRequest::Full,
        }
    };
    if start >= size {
        return RangeRequest::Unsatisfiable;
    }
    RangeRequest::Partial(start, end.map_or(size - 1, |e| e.min(size - 1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(v: &str, size: u64) -> RangeRequest {
        parse_range(Some(&HeaderValue::from_str(v).unwrap()), size)
    }

    #[test]
    fn ranges_parse() {
        use RangeRequest::*;
        assert_eq!(parse_range(None, 100), Full);
        assert_eq!(range("bytes=0-9", 100), Partial(0, 9));
        assert_eq!(range("bytes=90-", 100), Partial(90, 99));
        assert_eq!(range("bytes=90-500", 100), Partial(90, 99));
        assert_eq!(range("bytes=-10", 100), Partial(90, 99));
        assert_eq!(range("bytes=-500", 100), Partial(0, 99));
        assert_eq!(range("bytes=100-", 100), Unsatisfiable);
        assert_eq!(range("bytes=-0", 100), Unsatisfiable);
        assert_eq!(range("bytes=5-1", 100), Full);
        assert_eq!(range("bytes=0-1,5-9", 100), Full);
        assert_eq!(range("items=0-1", 100), Full);
        assert_eq!(range("bytes=x-", 100), Full);
    }

    #[test]
    fn moof_reads_join_short_fragments_only() {
        let frag = |offset, size| Fragment { offset, size };
        // Audio-like fragments 160 KB apart share one read, 1.2 MB video fragments do not.
        let audio: Vec<Fragment> = (0..3).map(|k| frag(1000 + k * 160_000, 160_000)).collect();
        assert_eq!(
            moof_spans(&audio),
            [(1000..1000 + 320_000 + MOOF_PROBE, 0..3)]
        );
        let video: Vec<Fragment> = (0..2).map(|k| frag(k * 1_200_000, 1_200_000)).collect();
        assert_eq!(
            moof_spans(&video),
            [
                (0..MOOF_PROBE, 0..1),
                (1_200_000..1_200_000 + MOOF_PROBE, 1..2)
            ]
        );
        // A fragment smaller than the probe is read whole, and no read grows past the cap.
        assert_eq!(moof_spans(&[frag(0, 100)]), [(0..100, 0..1)]);
        let many: Vec<Fragment> = (0..100).map(|k| frag(k * 200_000, 200_000)).collect();
        let spans = moof_spans(&many);
        assert!(spans.iter().all(|(r, _)| r.end - r.start <= MOOF_SPAN));
        assert_eq!(spans.last().unwrap().1.end, 100);
        assert!(spans.windows(2).all(|w| w[0].1.end == w[1].1.start));
    }

    #[test]
    fn fragment_cache_stays_within_its_byte_limit() {
        let mut cache = FragmentCache {
            entries: LruCache::unbounded(),
            bytes: 0,
            limit: 10,
        };
        let key = |n: u64| ("id".to_owned(), Track::Video, n);
        cache.put(key(1), Bytes::from_static(b"aaaa"));
        cache.put(key(2), Bytes::from_static(b"bbbb"));
        assert!(cache.get(&key(1)).is_some());
        cache.put(key(3), Bytes::from_static(b"cccc"));
        assert_eq!(cache.bytes, 8);
        assert!(
            cache.get(&key(2)).is_none(),
            "least recently used goes first"
        );
        assert!(cache.get(&key(1)).is_some());
        cache.put(key(4), Bytes::from(vec![0u8; 11]));
        assert!(cache.get(&key(4)).is_none(), "larger than the whole cache");
    }
}
