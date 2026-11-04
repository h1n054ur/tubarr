//! Login throttling, kept in memory (a restart forgets). Failures are counted three ways:
//! - per client address: past a few, that address waits before any sign-in, doubling each time up
//!   to a cap,
//! - per username from one address: the same waits, so a source guessing one account is stopped
//!   without touching anyone else who signs in as that user,
//! - per username from everywhere: only a short delay before answering, never a refusal. Spread
//!   over many addresses a guesser is slowed, but nobody can lock an account out by failing on
//!   purpose.

use std::{
    collections::HashMap,
    net::IpAddr,
    sync::Mutex,
    time::{Duration, Instant},
};

/// Free tries before an address, or a username from one address, has to wait.
const FREE_FAILURES: u32 = 5;
const FIRST_WAIT: Duration = Duration::from_secs(1);
const MAX_WAIT: Duration = Duration::from_secs(15 * 60);
/// Failures for one username, from all addresses together, before its sign-ins are slowed.
const USER_FREE_FAILURES: u32 = 20;
const USER_FIRST_DELAY: Duration = Duration::from_millis(250);
/// The longest a sign-in for a username under attack is held before it is answered.
pub const USER_MAX_DELAY: Duration = Duration::from_secs(4);
/// A counter with no failure for this long starts over.
const FORGET_AFTER: Duration = Duration::from_secs(60 * 60);
/// Past this many tracked keys, stale ones are swept before adding more.
const SWEEP_AT: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Ip(IpAddr),
    UserFrom(String, Option<IpAddr>),
    User(String),
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    failures: u32,
    last: Instant,
    blocked_until: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct LoginLimiter {
    entries: Mutex<HashMap<Key, Entry>>,
}

/// The counters that can refuse a try: the address and, for a sign-in, the username from it.
fn blocking_keys(ip: Option<IpAddr>, username: Option<&str>) -> Vec<Key> {
    let mut keys: Vec<Key> = username
        .map(|u| Key::UserFrom(u.to_lowercase(), ip))
        .into_iter()
        .collect();
    keys.extend(ip.map(Key::Ip));
    keys
}

/// `base` doubled for each failure past `free`, capped at `max`. Zero up to `free`.
fn backoff(failures: u32, free: u32, base: Duration, max: Duration) -> Duration {
    match failures.checked_sub(free) {
        Some(doublings) => base.saturating_mul(1 << doublings.min(20)).min(max),
        None => Duration::ZERO,
    }
}

impl LoginLimiter {
    /// How long the caller must still wait, if a counter blocks this try.
    pub fn check(&self, ip: Option<IpAddr>, username: &str) -> Option<Duration> {
        self.check_keys(&blocking_keys(ip, Some(username)))
    }

    /// How long to hold a sign-in for `username` before answering it, while failures for that
    /// name pile up from many addresses. Never more than [`USER_MAX_DELAY`].
    pub fn delay(&self, username: &str) -> Duration {
        let now = Instant::now();
        self.lock()
            .get(&Key::User(username.to_lowercase()))
            .filter(|e| now.duration_since(e.last) < FORGET_AFTER)
            .map_or(Duration::ZERO, |e| {
                backoff(
                    e.failures,
                    USER_FREE_FAILURES,
                    USER_FIRST_DELAY,
                    USER_MAX_DELAY,
                )
            })
    }

    pub fn failed(&self, ip: Option<IpAddr>, username: &str) {
        let mut keys = blocking_keys(ip, Some(username));
        keys.push(Key::User(username.to_lowercase()));
        self.failed_keys(keys);
    }

    /// Clears the address and the username from it. The count across all addresses stays, so a
    /// guesser elsewhere is still slowed: it fades once failures stop.
    pub fn succeeded(&self, ip: Option<IpAddr>, username: &str) {
        self.succeeded_keys(blocking_keys(ip, Some(username)));
    }

    /// [`Self::check`] by client address alone.
    pub fn check_ip(&self, ip: Option<IpAddr>) -> Option<Duration> {
        self.check_keys(&blocking_keys(ip, None))
    }

    pub fn failed_ip(&self, ip: Option<IpAddr>) {
        self.failed_keys(blocking_keys(ip, None));
    }

    pub fn succeeded_ip(&self, ip: Option<IpAddr>) {
        self.succeeded_keys(blocking_keys(ip, None));
    }

    fn check_keys(&self, keys: &[Key]) -> Option<Duration> {
        let now = Instant::now();
        let entries = self.lock();
        keys.iter()
            .filter_map(|k| entries.get(k)?.blocked_until)
            .map(|until| until.saturating_duration_since(now))
            .filter(|d| !d.is_zero())
            .max()
    }

    fn failed_keys(&self, keys: Vec<Key>) {
        let now = Instant::now();
        let mut entries = self.lock();
        if entries.len() >= SWEEP_AT {
            entries.retain(|_, e| now.duration_since(e.last) < FORGET_AFTER);
        }
        for key in keys {
            let blocks = !matches!(key, Key::User(_));
            let entry = entries.entry(key).or_insert(Entry {
                failures: 0,
                last: now,
                blocked_until: None,
            });
            if now.duration_since(entry.last) >= FORGET_AFTER {
                entry.failures = 0;
            }
            entry.failures += 1;
            entry.last = now;
            if blocks && entry.failures >= FREE_FAILURES {
                let wait = backoff(entry.failures, FREE_FAILURES, FIRST_WAIT, MAX_WAIT);
                entry.blocked_until = Some(now + wait);
            }
        }
    }

    fn succeeded_keys(&self, keys: Vec<Key>) {
        let mut entries = self.lock();
        for key in keys {
            entries.remove(&key);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<Key, Entry>> {
        // Plain counters: a panic elsewhere cannot leave them inconsistent.
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn blocks_after_free_failures_and_doubles() {
        let limiter = LoginLimiter::default();
        let ip = ip("10.0.0.9");
        for _ in 0..FREE_FAILURES - 1 {
            limiter.failed(ip, "hani");
            assert_eq!(limiter.check(ip, "hani"), None);
        }
        limiter.failed(ip, "hani");
        let first = limiter.check(ip, "hani").unwrap();
        assert!(first <= FIRST_WAIT);
        limiter.failed(ip, "hani");
        assert!(limiter.check(ip, "hani").unwrap() > first);
        // The address is blocked for other names too.
        assert!(limiter.check(ip, "someone").is_some());
        limiter.succeeded(ip, "hani");
        assert_eq!(limiter.check(ip, "hani"), None);
    }

    #[test]
    fn failures_from_one_address_never_lock_the_user_out_elsewhere() {
        let limiter = LoginLimiter::default();
        let attacker = ip("198.51.100.4");
        for _ in 0..50 {
            limiter.failed(attacker, "HANI");
        }
        assert!(limiter.check(attacker, "hani").is_some());
        assert_eq!(limiter.check(ip("10.0.0.9"), "hani"), None);
        assert_eq!(limiter.check(None, "hani"), None);
    }

    #[test]
    fn many_addresses_only_slow_a_username_up_to_the_cap() {
        let limiter = LoginLimiter::default();
        let from = |n: u32| ip(&format!("203.0.113.{n}"));
        for n in 0..USER_FREE_FAILURES {
            limiter.failed(from(n), "hani");
        }
        assert_eq!(limiter.delay("hani"), USER_FIRST_DELAY);
        assert_eq!(limiter.delay("other"), Duration::ZERO);
        for n in 0..100 {
            limiter.failed(from(n + USER_FREE_FAILURES), "hani");
        }
        assert_eq!(limiter.delay("Hani"), USER_MAX_DELAY);
        // Slowed, never refused, and a sign-in elsewhere does not reset the slowdown.
        assert_eq!(limiter.check(ip("10.0.0.9"), "hani"), None);
        limiter.succeeded(ip("10.0.0.9"), "hani");
        assert_eq!(limiter.delay("hani"), USER_MAX_DELAY);
    }

    #[test]
    fn by_address_alone_other_addresses_stay_free() {
        let limiter = LoginLimiter::default();
        let ip9 = ip("10.0.0.9");
        for _ in 0..FREE_FAILURES {
            limiter.failed_ip(ip9);
        }
        assert!(limiter.check_ip(ip9).is_some());
        assert_eq!(limiter.check_ip(ip("10.0.0.10")), None);
        assert_eq!(limiter.check_ip(None), None);
        limiter.succeeded_ip(ip9);
        assert_eq!(limiter.check_ip(ip9), None);
    }
}
