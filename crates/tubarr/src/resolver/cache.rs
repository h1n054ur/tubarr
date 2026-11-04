//! Where resolved lookups are kept between plays.

use std::{
    collections::HashMap,
    future::Future,
    sync::Mutex,
    time::{Duration, SystemTime},
};

use super::Resolved;

/// Storage for resolved lookups. The in-memory one lives here, the database one is
/// [`Db`](crate::store::Db).
pub trait ResolveCache: Send + Sync {
    /// The entry for `video_id` if it is still valid at `now`.
    fn get(&self, video_id: &str, now: SystemTime)
    -> impl Future<Output = Option<Resolved>> + Send;

    /// Stores `resolved` until `valid_until`, replacing any older entry. `now` lets a store prune
    /// what has already expired.
    fn put(
        &self,
        resolved: Resolved,
        now: SystemTime,
        valid_until: SystemTime,
    ) -> impl Future<Output = ()> + Send;
}

/// How long a lookup stays usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePolicy {
    /// Upper bound for any entry.
    pub ttl: Duration,
    /// Entries end this long before the stream URLs' own `expire=`, so a play that starts just
    /// before the end still has time to run.
    pub expiry_margin: Duration,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(5 * 3600),
            expiry_margin: Duration::from_secs(30 * 60),
        }
    }
}

impl CachePolicy {
    /// When an entry resolved at `now` stops being served, or `None` if it should not be cached.
    pub fn valid_until(&self, resolved: &Resolved, now: SystemTime) -> Option<SystemTime> {
        let mut until = now + self.ttl;
        if let Some(expires_at) = resolved.expires_at {
            until = until.min(expires_at.checked_sub(self.expiry_margin)?);
        }
        (until > now).then_some(until)
    }
}

/// A process-local cache. Expired entries are dropped when read or when a new entry is stored.
#[derive(Debug, Default)]
pub struct MemoryCache {
    entries: Mutex<HashMap<String, (Resolved, SystemTime)>>,
}

impl MemoryCache {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (Resolved, SystemTime)>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl ResolveCache for MemoryCache {
    async fn get(&self, video_id: &str, now: SystemTime) -> Option<Resolved> {
        let mut entries = self.lock();
        match entries.get(video_id) {
            Some((resolved, until)) if *until > now => Some(resolved.clone()),
            Some(_) => {
                entries.remove(video_id);
                None
            }
            None => None,
        }
    }

    async fn put(&self, resolved: Resolved, now: SystemTime, valid_until: SystemTime) {
        let mut entries = self.lock();
        entries.retain(|_, (_, until)| *until > now);
        entries.insert(resolved.meta.id.clone(), (resolved, valid_until));
    }
}

#[cfg(test)]
mod tests {
    use std::time::UNIX_EPOCH;

    use super::*;
    use crate::resolver::{StreamFormat, VideoMeta};

    fn resolved(id: &str, expires_at: Option<SystemTime>) -> Resolved {
        let stream = |format_id: &str| StreamFormat {
            format_id: format_id.to_owned(),
            url: String::new(),
            ext: None,
            width: None,
            height: None,
            filesize: None,
            filesize_approx: None,
            http_headers: Default::default(),
        };
        Resolved {
            meta: VideoMeta {
                id: id.to_owned(),
                title: String::new(),
                duration: None,
                upload_date: None,
                timestamp: None,
                channel_id: None,
            },
            video: stream("137"),
            audio: stream("140"),
            subtitles: Vec::new(),
            expires_at,
        }
    }

    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn ttl_applies_without_an_expiry() {
        let now = UNIX_EPOCH + 1000 * HOUR;
        let policy = CachePolicy::default();
        assert_eq!(
            policy.valid_until(&resolved("a", None), now),
            Some(now + 5 * HOUR)
        );
    }

    #[test]
    fn url_expiry_minus_margin_wins_when_sooner() {
        let now = UNIX_EPOCH + 1000 * HOUR;
        let policy = CachePolicy::default();
        let r = resolved("a", Some(now + 3 * HOUR));
        assert_eq!(
            policy.valid_until(&r, now),
            Some(now + 3 * HOUR - Duration::from_secs(1800))
        );
        // YouTube's usual six hours leave the five hour TTL in charge.
        let r = resolved("a", Some(now + 6 * HOUR));
        assert_eq!(policy.valid_until(&r, now), Some(now + 5 * HOUR));
    }

    #[test]
    fn nearly_expired_urls_are_not_cached() {
        let now = UNIX_EPOCH + 1000 * HOUR;
        let policy = CachePolicy::default();
        let r = resolved("a", Some(now + Duration::from_secs(600)));
        assert_eq!(policy.valid_until(&r, now), None);
    }

    #[tokio::test]
    async fn memory_cache_expires_entries() {
        let cache = MemoryCache::new();
        let now = SystemTime::now();
        cache.put(resolved("old", None), now, now + HOUR).await;
        cache.put(resolved("a", None), now, now + HOUR).await;
        assert!(cache.get("a", now).await.is_some());
        assert!(
            cache
                .get("a", now + HOUR - Duration::from_secs(1))
                .await
                .is_some()
        );
        assert!(cache.get("a", now + HOUR).await.is_none());
        // The expired entry was dropped on that read.
        assert!(!cache.lock().contains_key("a"));
        assert!(cache.get("b", now).await.is_none());
        // Storing prunes entries that expired without being read.
        cache
            .put(resolved("b", None), now + 2 * HOUR, now + 3 * HOUR)
            .await;
        assert_eq!(cache.lock().len(), 1);
    }
}
