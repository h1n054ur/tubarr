//! The shared token bucket that paces every YouTube lookup.
//!
//! YouTube starts answering with 429s and bot checks after a burst of lookups from one IP, so every
//! yt-dlp run waits here first. Callers reserve a token up front, which keeps them in arrival order.
//! A viewer pressing play takes the [`Lane::Play`] lane: it reserves the first slot after other
//! plays, and every background lookup still waiting moves back one slot to make room.
//!
//! [`ProcessSlots`] caps how many yt-dlp processes run at once, whatever the tokens allow, in the
//! same play-first order.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::{sync::oneshot, time::Instant};

use crate::metrics::METRICS;

/// How fast the bucket refills and how much it can hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimitConfig {
    /// One token comes back every `interval`.
    pub interval: Duration,
    /// Most tokens the bucket holds, so the largest burst after a quiet spell.
    pub burst: u32,
    /// A random delay of up to this much is added whenever a caller has to wait, so lookups do not
    /// land on an exact beat.
    pub max_jitter: Duration,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            burst: 3,
            max_jitter: Duration::from_secs(3),
        }
    }
}

/// Who is asking for a token. Both lanes draw from the same bucket, so the pace stays the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    /// A viewer is waiting on this lookup. Goes ahead of every queued background lookup.
    Play,
    /// Matcher, backfill, subtitles, canary: work nobody is watching.
    Background,
}

/// What the Health page shows about the limiter.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateLimitStatus {
    /// Whole and partial tokens ready right now.
    pub available: f64,
    /// Callers that hold a reservation and are still waiting for their turn.
    pub queued: u32,
    /// How long a new lookup would wait before it may start (jitter not included).
    pub next_in: Duration,
    /// Time left on a slow-down set by [`RateLimiter::penalize`], if any.
    pub paused_for: Option<Duration>,
}

/// A token bucket shared by every lookup. Cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct RateLimiter {
    config: RateLimitConfig,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    /// Negative while callers hold reservations for tokens that have not refilled yet.
    tokens: f64,
    updated: Instant,
    paused_until: Option<Instant>,
    /// Background callers asleep in `acquire`, whose slots a play may take.
    background_waiting: u32,
    /// Plays that went ahead of waiting background callers, counted so each sleeper can tell how
    /// many slots it moved back.
    jumps: u64,
}

impl State {
    fn refill(&mut self, now: Instant, config: &RateLimitConfig) {
        // Nothing refills during a pause, so the bucket does not burst the moment it ends.
        let from = self
            .paused_until
            .map_or(self.updated, |p| p.max(self.updated));
        let elapsed = now.saturating_duration_since(from);
        let gained = elapsed.as_secs_f64() / config.interval.as_secs_f64();
        self.tokens = (self.tokens + gained).min(f64::from(config.burst));
        self.updated = now;
    }
}

impl RateLimiter {
    /// A limiter that starts with a full bucket.
    pub fn new(config: RateLimitConfig) -> Self {
        assert!(
            !config.interval.is_zero(),
            "rate limit interval must not be zero"
        );
        Self {
            state: Mutex::new(State {
                tokens: f64::from(config.burst),
                updated: Instant::now(),
                paused_until: None,
                background_waiting: 0,
                jumps: 0,
            }),
            config,
        }
    }

    pub fn config(&self) -> RateLimitConfig {
        self.config
    }

    /// Waits until the caller may run one lookup. The token is taken even if the future is dropped
    /// while waiting, which errs on the side of fewer lookups.
    pub async fn acquire(&self) {
        self.acquire_in(Lane::Background).await;
    }

    /// Like [`Self::acquire`], in the given lane. A play reserves its slot as if the waiting
    /// background callers had not, and each of them sleeps one interval longer per play that
    /// went ahead. A pause holds plays like everything else.
    pub async fn acquire_in(&self, lane: Lane) {
        let (mut ready_at, mut seen) = {
            let mut state = self.lock();
            let now = Instant::now();
            state.refill(now, &self.config);
            let ahead = match lane {
                Lane::Play => {
                    if state.background_waiting > 0 {
                        state.jumps += 1;
                    }
                    state.tokens + f64::from(state.background_waiting)
                }
                Lane::Background => state.tokens,
            };
            state.tokens -= 1.0;
            let ready_at = self.ready_at(now, ahead - 1.0, state.paused_until);
            if lane == Lane::Background && ready_at > now {
                state.background_waiting += 1;
            }
            (ready_at, state.jumps)
        };
        let asked = Instant::now();
        if ready_at <= asked {
            return;
        }
        METRICS.limiter_waits.inc();
        let _timed = WaitTimer(asked);
        if lane == Lane::Background {
            let _waiting = Waiting(self);
            loop {
                tokio::time::sleep_until(ready_at).await;
                let jumps = self.lock().jumps;
                if jumps == seen {
                    break;
                }
                ready_at += self.config.interval * u32::try_from(jumps - seen).unwrap_or(u32::MAX);
                seen = jumps;
            }
        } else {
            tokio::time::sleep_until(ready_at).await;
        }
        tokio::time::sleep(self.jitter()).await;
    }

    /// When a caller left with `tokens` after taking its own may start.
    fn ready_at(&self, now: Instant, tokens: f64, paused_until: Option<Instant>) -> Instant {
        let mut ready_at = now;
        if tokens < 0.0 {
            ready_at += self.config.interval.mul_f64(-tokens);
        }
        paused_until.map_or(ready_at, |p| ready_at.max(p))
    }

    /// How long a play asking now would wait before it may start (jitter not included).
    pub fn play_next_in(&self) -> Duration {
        let mut state = self.lock();
        let now = Instant::now();
        state.refill(now, &self.config);
        let ahead = state.tokens + f64::from(state.background_waiting);
        self.ready_at(now, ahead - 1.0, state.paused_until)
            .saturating_duration_since(now)
    }

    /// Holds every lookup for at least `pause` and empties the bucket, so nothing bursts afterwards.
    /// Used after a 429 or a bot check.
    pub fn penalize(&self, pause: Duration) {
        let mut state = self.lock();
        let now = Instant::now();
        state.refill(now, &self.config);
        state.tokens = state.tokens.min(0.0);
        let until = now + pause;
        state.paused_until = Some(state.paused_until.map_or(until, |p| p.max(until)));
    }

    pub fn status(&self) -> RateLimitStatus {
        let mut state = self.lock();
        let now = Instant::now();
        state.refill(now, &self.config);
        let paused_for = state
            .paused_until
            .map(|p| p.saturating_duration_since(now))
            .filter(|d| !d.is_zero());
        let token_wait = if state.tokens >= 1.0 {
            Duration::ZERO
        } else {
            self.config.interval.mul_f64(1.0 - state.tokens)
        };
        RateLimitStatus {
            available: state.tokens.max(0.0),
            // Each whole token below zero is one caller still asleep in `acquire`.
            queued: (-state.tokens).ceil().max(0.0) as u32,
            next_in: token_wait.max(paused_for.unwrap_or_default()),
            paused_for,
        }
    }

    fn jitter(&self) -> Duration {
        let max = u64::try_from(self.config.max_jitter.as_millis()).unwrap_or(u64::MAX);
        if max == 0 {
            Duration::ZERO
        } else {
            Duration::from_millis(rand::random_range(0..=max))
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // The state is plain numbers, so a panic elsewhere cannot leave it inconsistent.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Adds the time since it was made to the limiter wait total when dropped, so a caller that gives
/// up still counts the time it waited.
struct WaitTimer(Instant);

impl Drop for WaitTimer {
    fn drop(&mut self) {
        let micros = u64::try_from(self.0.elapsed().as_micros()).unwrap_or(u64::MAX);
        METRICS.limiter_wait_micros.add(micros);
    }
}

/// Counts a background caller as waiting until its `acquire` finishes or is dropped.
struct Waiting<'a>(&'a RateLimiter);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.lock().background_waiting -= 1;
    }
}

/// How many yt-dlp processes may run at once. Each one, with the deno it starts for YouTube's
/// JavaScript, holds about 350 MB, so two at once took a 512 MB container over its limit. A caller
/// that finds every slot taken waits in its lane: a slot that comes free goes to the first waiting
/// play, and to a background caller only when no play waits.
#[derive(Debug)]
pub struct ProcessSlots {
    limit: usize,
    state: Mutex<SlotState>,
}

#[derive(Debug, Default)]
struct SlotState {
    running: usize,
    /// Callers waiting for a slot. There are only any while every slot is taken.
    play: VecDeque<oneshot::Sender<ProcessPermit>>,
    background: VecDeque<oneshot::Sender<ProcessPermit>>,
}

/// One running process's slot, handed to the next waiting caller on drop.
#[derive(Debug)]
pub struct ProcessPermit(Option<Arc<ProcessSlots>>);

impl Drop for ProcessPermit {
    fn drop(&mut self) {
        if let Some(slots) = self.0.take() {
            slots.release();
        }
    }
}

impl ProcessSlots {
    pub fn new(limit: usize) -> Self {
        assert!(limit > 0, "at least one yt-dlp process must be allowed");
        Self {
            limit,
            state: Mutex::new(SlotState::default()),
        }
    }

    /// Processes running right now.
    pub fn running(&self) -> usize {
        self.lock().running
    }

    /// Waits for a free slot in `lane`. A caller dropped while waiting passes its turn on.
    pub async fn acquire(self: &Arc<Self>, lane: Lane) -> ProcessPermit {
        let turn = {
            let mut state = self.lock();
            if state.running < self.limit {
                state.running += 1;
                return ProcessPermit(Some(self.clone()));
            }
            let (tx, rx) = oneshot::channel();
            match lane {
                Lane::Play => state.play.push_back(tx),
                Lane::Background => state.background.push_back(tx),
            }
            rx
        };
        // A queued sender is only ever sent on, and `self` keeps the queue alive meanwhile.
        turn.await.expect("a waiting caller always gets a slot")
    }

    /// Hands a freed slot to the first caller still waiting, plays first, or frees it.
    fn release(self: Arc<Self>) {
        loop {
            let next = {
                let mut state = self.lock();
                match state
                    .play
                    .pop_front()
                    .or_else(|| state.background.pop_front())
                {
                    Some(next) => next,
                    None => {
                        state.running -= 1;
                        return;
                    }
                }
            };
            // A caller that stopped waiting hands the permit back: disarm it and try the next one.
            match next.send(ProcessPermit(Some(self.clone()))) {
                Ok(()) => return,
                Err(mut gone) => drop(gone.0.take()),
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        // The state is a count and two queues, so a panic elsewhere cannot leave it inconsistent.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn limiter(burst: u32, jitter: Duration) -> Arc<RateLimiter> {
        Arc::new(RateLimiter::new(RateLimitConfig {
            interval: Duration::from_secs(10),
            burst,
            max_jitter: jitter,
        }))
    }

    #[tokio::test(start_paused = true)]
    async fn burst_goes_at_once_then_paces() {
        let limiter = limiter(2, Duration::ZERO);
        let start = Instant::now();
        limiter.acquire().await;
        limiter.acquire().await;
        assert_eq!(start.elapsed(), Duration::ZERO);
        limiter.acquire().await;
        assert_eq!(start.elapsed(), Duration::from_secs(10));
        limiter.acquire().await;
        assert_eq!(start.elapsed(), Duration::from_secs(20));
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_callers_are_spaced() {
        let limiter = limiter(1, Duration::ZERO);
        let start = Instant::now();
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let limiter = limiter.clone();
            tasks.push(tokio::spawn(async move {
                limiter.acquire().await;
                start.elapsed()
            }));
        }
        let mut done = Vec::new();
        for task in tasks {
            done.push(task.await.unwrap());
        }
        done.sort();
        let secs: Vec<u64> = done.iter().map(Duration::as_secs).collect();
        assert_eq!(secs, vec![0, 10, 20, 30]);
    }

    #[tokio::test(start_paused = true)]
    async fn quiet_spell_refills_up_to_burst_only() {
        let limiter = limiter(3, Duration::ZERO);
        for _ in 0..3 {
            limiter.acquire().await;
        }
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert_eq!(limiter.status().available, 3.0);
        let start = Instant::now();
        for _ in 0..3 {
            limiter.acquire().await;
        }
        assert_eq!(start.elapsed(), Duration::ZERO);
        limiter.acquire().await;
        assert_eq!(start.elapsed(), Duration::from_secs(10));
    }

    #[tokio::test(start_paused = true)]
    async fn jitter_stays_within_bounds() {
        let limiter = limiter(1, Duration::from_secs(3));
        limiter.acquire().await;
        let start = Instant::now();
        limiter.acquire().await;
        let waited = start.elapsed();
        assert!(waited >= Duration::from_secs(10), "{waited:?}");
        assert!(waited <= Duration::from_secs(13), "{waited:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn status_reports_tokens_and_wait() {
        let limiter = limiter(2, Duration::ZERO);
        let status = limiter.status();
        assert_eq!(status.available, 2.0);
        assert_eq!(status.next_in, Duration::ZERO);
        assert_eq!(status.queued, 0);

        limiter.acquire().await;
        limiter.acquire().await;
        tokio::time::sleep(Duration::from_secs(4)).await;
        let status = limiter.status();
        assert!((status.available - 0.4).abs() < 1e-9);
        assert_eq!(status.next_in, Duration::from_secs(6));

        // Two more callers queue up behind the empty bucket.
        let a = tokio::spawn({
            let l = limiter.clone();
            async move { l.acquire().await }
        });
        let b = tokio::spawn({
            let l = limiter.clone();
            async move { l.acquire().await }
        });
        tokio::task::yield_now().await;
        let status = limiter.status();
        assert_eq!(status.queued, 2);
        assert_eq!(status.available, 0.0);
        a.await.unwrap();
        b.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn penalize_holds_lookups_and_stops_the_burst() {
        let limiter = limiter(3, Duration::ZERO);
        limiter.penalize(Duration::from_secs(300));
        let status = limiter.status();
        assert_eq!(status.paused_for, Some(Duration::from_secs(300)));
        assert_eq!(status.next_in, Duration::from_secs(300));
        assert_eq!(status.available, 0.0);

        let start = Instant::now();
        limiter.acquire().await;
        assert_eq!(start.elapsed(), Duration::from_secs(300));
        assert_eq!(limiter.status().paused_for, None);
        // The bucket stays empty after the pause: the next lookup is paced, not part of a burst.
        limiter.acquire().await;
        assert_eq!(start.elapsed(), Duration::from_secs(320));
    }

    /// Starts a caller in `lane` and returns when it got its token, measured from `start`.
    fn spawn_in(
        limiter: &Arc<RateLimiter>,
        lane: Lane,
        start: Instant,
    ) -> tokio::task::JoinHandle<u64> {
        let limiter = limiter.clone();
        tokio::spawn(async move {
            limiter.acquire_in(lane).await;
            start.elapsed().as_secs()
        })
    }

    #[tokio::test(start_paused = true)]
    async fn a_play_goes_ahead_of_queued_background_lookups() {
        let limiter = limiter(1, Duration::ZERO);
        let start = Instant::now();
        limiter.acquire().await;
        let background: Vec<_> = (0..4)
            .map(|_| spawn_in(&limiter, Lane::Background, start))
            .collect();
        tokio::task::yield_now().await;
        assert_eq!(limiter.play_next_in(), Duration::from_secs(10));

        tokio::time::sleep(Duration::from_secs(3)).await;
        let play = spawn_in(&limiter, Lane::Play, start);
        assert_eq!(play.await.unwrap(), 10, "takes the next slot");
        let mut secs = Vec::new();
        for task in background {
            secs.push(task.await.unwrap());
        }
        secs.sort_unstable();
        // Each background lookup moved back one slot; the pace stays one per interval.
        assert_eq!(secs, vec![20, 30, 40, 50]);
    }

    #[tokio::test(start_paused = true)]
    async fn plays_queue_in_order_ahead_of_background() {
        let limiter = limiter(1, Duration::ZERO);
        let start = Instant::now();
        limiter.acquire().await;
        let background = spawn_in(&limiter, Lane::Background, start);
        tokio::task::yield_now().await;
        let first = spawn_in(&limiter, Lane::Play, start);
        tokio::task::yield_now().await;
        let second = spawn_in(&limiter, Lane::Play, start);
        assert_eq!(first.await.unwrap(), 10);
        assert_eq!(second.await.unwrap(), 20);
        assert_eq!(background.await.unwrap(), 30);
    }

    #[tokio::test(start_paused = true)]
    async fn a_play_waits_out_a_pause_too() {
        let limiter = limiter(3, Duration::ZERO);
        limiter.penalize(Duration::from_secs(300));
        assert_eq!(limiter.play_next_in(), Duration::from_secs(300));
        let start = Instant::now();
        limiter.acquire_in(Lane::Play).await;
        assert_eq!(start.elapsed(), Duration::from_secs(300));
    }

    #[tokio::test(start_paused = true)]
    async fn a_dropped_background_caller_stops_counting_as_waiting() {
        let limiter = limiter(1, Duration::ZERO);
        let start = Instant::now();
        limiter.acquire().await;
        let gone = spawn_in(&limiter, Lane::Background, start);
        tokio::task::yield_now().await;
        assert_eq!(limiter.play_next_in(), Duration::from_secs(10));
        gone.abort();
        let _ = gone.await;
        // Its token stays taken, so a play now comes after that slot.
        assert_eq!(limiter.play_next_in(), Duration::from_secs(20));
    }

    /// Starts a caller in `lane` that holds its process slot for `hold`, and returns when it got
    /// the slot, measured from `start`.
    fn spawn_slot(
        slots: &Arc<ProcessSlots>,
        lane: Lane,
        hold: Duration,
        start: Instant,
    ) -> tokio::task::JoinHandle<u64> {
        let slots = slots.clone();
        tokio::spawn(async move {
            let _slot = slots.acquire(lane).await;
            let got = start.elapsed().as_secs();
            tokio::time::sleep(hold).await;
            got
        })
    }

    #[tokio::test(start_paused = true)]
    async fn process_slots_cap_the_count_and_a_play_goes_first() {
        let slots = Arc::new(ProcessSlots::new(1));
        let start = Instant::now();
        let ten = Duration::from_secs(10);
        let first = spawn_slot(&slots, Lane::Background, ten, start);
        tokio::task::yield_now().await;
        let background: Vec<_> = (0..2)
            .map(|_| spawn_slot(&slots, Lane::Background, ten, start))
            .collect();
        tokio::task::yield_now().await;
        let play = spawn_slot(&slots, Lane::Play, ten, start);
        tokio::task::yield_now().await;
        assert_eq!(slots.running(), 1);
        assert_eq!(first.await.unwrap(), 0);
        assert_eq!(
            play.await.unwrap(),
            10,
            "the play takes the first free slot"
        );
        let mut secs = Vec::new();
        for task in background {
            secs.push(task.await.unwrap());
        }
        assert_eq!(secs, vec![20, 30]);
        assert_eq!(slots.running(), 0);
    }

    #[tokio::test(start_paused = true)]
    async fn a_caller_that_stops_waiting_passes_its_turn_on() {
        let slots = Arc::new(ProcessSlots::new(1));
        let start = Instant::now();
        let held = slots.acquire(Lane::Background).await;
        let gone = spawn_slot(&slots, Lane::Play, Duration::ZERO, start);
        tokio::task::yield_now().await;
        let next = spawn_slot(&slots, Lane::Background, Duration::ZERO, start);
        tokio::task::yield_now().await;
        gone.abort();
        let _ = gone.await;
        drop(held);
        assert_eq!(next.await.unwrap(), 0);
        assert_eq!(slots.running(), 0);
    }
}
