//! When each channel is read next. Pure bookkeeping, so the timing can be tested with paused time.

use std::{collections::HashMap, time::Duration};

use tokio::time::Instant;

/// Feed failures back off up to this long: a feed that keeps failing is checked a few times a day.
pub const MAX_FEED_BACKOFF: Duration = Duration::from_secs(6 * 60 * 60);

/// Next due time per channel.
#[derive(Debug, Default)]
pub struct Schedule {
    due: HashMap<String, Instant>,
}

impl Schedule {
    /// Keeps only `channels` and returns the ones not scheduled yet, in the order given. The
    /// caller schedules those.
    pub fn sync(&mut self, channels: &[String]) -> Vec<String> {
        self.due.retain(|c, _| channels.contains(c));
        channels
            .iter()
            .filter(|c| !self.due.contains_key(*c))
            .cloned()
            .collect()
    }

    pub fn set(&mut self, channel: &str, at: Instant) {
        self.due.insert(channel.to_owned(), at);
    }

    pub fn due_at(&self, channel: &str) -> Option<Instant> {
        self.due.get(channel).copied()
    }

    /// The channel due first. Ties go to the smaller id, so the order is stable.
    pub fn next(&self) -> Option<(String, Instant)> {
        self.due
            .iter()
            .min_by(|a, b| a.1.cmp(b.1).then_with(|| a.0.cmp(b.0)))
            .map(|(c, at)| (c.clone(), *at))
    }

    pub fn is_empty(&self) -> bool {
        self.due.is_empty()
    }
}

/// Time until the next feed read: the interval plus jitter, doubled for every failure in a row up
/// to [`MAX_FEED_BACKOFF`]. `jitter` is the random part, picked by the caller.
pub fn feed_delay(interval: Duration, failures: u32, jitter: Duration) -> Duration {
    let base = if failures == 0 {
        interval
    } else {
        interval
            .saturating_mul(2u32.saturating_pow(failures.min(16)))
            .min(MAX_FEED_BACKOFF.max(interval))
    };
    base + jitter
}

/// When the channels whose feed keeps failing get their short fallback lists. Channels that start
/// failing together are spread over one interval, and two fallback lists are always at least an
/// hour divided by the hourly cap apart, so an outage on every feed at once never bursts lookups.
#[derive(Debug)]
pub struct FallbackPlan {
    due: Schedule,
    interval: Duration,
    spacing: Duration,
    last_run: Option<Instant>,
}

impl FallbackPlan {
    /// A list about every `interval` per channel, at most `per_hour` (at least 1) an hour in all.
    pub fn new(interval: Duration, per_hour: u32) -> Self {
        Self {
            due: Schedule::default(),
            interval,
            spacing: Duration::from_secs(60 * 60) / per_hour.max(1),
            last_run: None,
        }
    }

    /// Keeps only `failing`. Channels new to it get their first turns spread evenly over one
    /// interval from `now`, the first at once.
    pub fn sync(&mut self, failing: &[String], now: Instant) {
        let new = self.due.sync(failing);
        let n = u32::try_from(new.len()).unwrap_or(u32::MAX);
        for (i, channel) in (0u32..).zip(new) {
            self.due.set(&channel, now + self.interval / n * i);
        }
    }

    /// The channel due first and when it may run: its turn, but never sooner than the spacing
    /// after the last fallback list.
    pub fn next(&self) -> Option<(String, Instant)> {
        let (channel, due) = self.due.next()?;
        let at = self
            .last_run
            .map_or(due, |last| due.max(last + self.spacing));
        Some((channel, at))
    }

    /// Records a list of `channel` started at `at`. Its next turn is one interval plus `jitter`
    /// later.
    pub fn ran(&mut self, channel: &str, at: Instant, jitter: Duration) {
        self.last_run = Some(at);
        self.due.set(channel, at + self.interval + jitter);
    }

    /// Moves `channel`'s turn to `at` without counting a list, as while lookups are paused.
    pub fn postpone(&mut self, channel: &str, at: Instant) {
        self.due.set(channel, at);
    }
}

/// First listing times for channels without a recent one. A channel listed less than `interval`
/// ago keeps its turn. The rest (new channels, or overdue after downtime) are spread evenly over
/// one interval after `start_delay`, so they never all run at once.
pub fn first_listings(
    channels: &[String],
    last_listed: &HashMap<String, i64>,
    now_unix: i64,
    now: Instant,
    interval: Duration,
    start_delay: Duration,
) -> Vec<(String, Instant)> {
    let interval_secs = i64::try_from(interval.as_secs()).unwrap_or(i64::MAX);
    let mut out = Vec::new();
    let mut spread = Vec::new();
    for channel in channels {
        match last_listed.get(channel) {
            Some(&last) if last.saturating_add(interval_secs) > now_unix => {
                let wait = u64::try_from(last + interval_secs - now_unix).unwrap_or(0);
                out.push((channel.clone(), now + Duration::from_secs(wait)));
            }
            _ => spread.push(channel),
        }
    }
    let n = u32::try_from(spread.len()).unwrap_or(u32::MAX);
    for (i, channel) in (0u32..).zip(spread) {
        let offset = interval / n * i;
        out.push((channel.clone(), now + start_delay + offset));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("UC{i:022}")).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn new_channels_are_spread_over_the_interval() {
        let now = Instant::now();
        let channels = ids(4);
        let plan = first_listings(
            &channels,
            &HashMap::new(),
            1_000_000,
            now,
            24 * HOUR,
            Duration::from_secs(120),
        );
        let offsets: Vec<_> = plan.iter().map(|(_, at)| *at - now).collect();
        let two_min = Duration::from_secs(120);
        assert_eq!(
            offsets,
            [
                two_min,
                6 * HOUR + two_min,
                12 * HOUR + two_min,
                18 * HOUR + two_min
            ]
        );

        // Driving the schedule: each channel comes up in turn, then a day after its run.
        let mut schedule = Schedule::default();
        assert_eq!(schedule.sync(&channels), channels);
        for (c, at) in plan {
            schedule.set(&c, at);
        }
        let mut ran = Vec::new();
        for _ in 0..5 {
            let (channel, at) = schedule.next().unwrap();
            tokio::time::sleep_until(at).await;
            ran.push((channel.clone(), Instant::now() - now));
            schedule.set(&channel, Instant::now() + 24 * HOUR);
        }
        let hours: Vec<_> = ran.iter().map(|(_, d)| d.as_secs() / 3600).collect();
        assert_eq!(hours, [0, 6, 12, 18, 24]);
        assert_eq!(ran[4].0, channels[0]);
    }

    #[tokio::test(start_paused = true)]
    async fn recent_listings_keep_their_turn_and_overdue_ones_are_spread() {
        let now = Instant::now();
        let channels = ids(3);
        let now_unix = 10 * 86_400;
        let last = HashMap::from([
            (channels[0].clone(), now_unix - 3600), // listed an hour ago
            (channels[1].clone(), now_unix - 3 * 86_400), // overdue after downtime
        ]);
        let plan: HashMap<_, _> =
            first_listings(&channels, &last, now_unix, now, 24 * HOUR, Duration::ZERO)
                .into_iter()
                .collect();
        assert_eq!(plan[&channels[0]] - now, 23 * HOUR);
        assert_eq!(plan[&channels[1]] - now, Duration::ZERO);
        assert_eq!(plan[&channels[2]] - now, 12 * HOUR);
    }

    #[test]
    fn sync_drops_removed_channels() {
        let mut schedule = Schedule::default();
        let now = Instant::now();
        let channels = ids(2);
        schedule.sync(&channels);
        schedule.set(&channels[0], now);
        schedule.set(&channels[1], now + HOUR);
        assert!(schedule.sync(&channels[1..]).is_empty());
        assert_eq!(schedule.next().unwrap().0, channels[1]);
        assert!(schedule.sync(&[]).is_empty());
        assert!(schedule.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_lists_are_spread_and_capped_with_many_failing_channels() {
        let start = Instant::now();
        let half = Duration::from_secs(30 * 60);
        let mut plan = FallbackPlan::new(half, 20);
        let channels = ids(100);
        plan.sync(&channels, start);
        let mut runs: Vec<(String, Duration)> = Vec::new();
        while let Some((channel, at)) = plan.next() {
            if at - start >= 3 * HOUR {
                break;
            }
            tokio::time::sleep_until(at).await;
            runs.push((channel.clone(), Instant::now() - start));
            plan.ran(&channel, Instant::now(), Duration::ZERO);
        }
        // Three minutes apart, so 20 an hour however many channels fail.
        assert_eq!(runs.len(), 60);
        for pair in runs.windows(2) {
            assert_eq!(pair[1].1 - pair[0].1, Duration::from_secs(180));
        }
        for hour in 0..3 {
            let in_hour = runs
                .iter()
                .filter(|(_, at)| at.as_secs() / 3600 == hour)
                .count();
            assert_eq!(in_hour, 20);
        }
        // Each channel had one turn before any had a second.
        let mut seen: Vec<_> = runs.iter().map(|(c, _)| c.clone()).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 60);
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_lists_follow_the_interval_and_stop_for_recovered_channels() {
        let start = Instant::now();
        let half = Duration::from_secs(30 * 60);
        let mut plan = FallbackPlan::new(half, 30);
        let channels = ids(3);
        plan.sync(&channels, start);
        // Spread over one interval: at once, after 10 and after 20 minutes.
        let mut firsts = Vec::new();
        for _ in 0..3 {
            let (channel, at) = plan.next().unwrap();
            tokio::time::sleep_until(at).await;
            firsts.push(Instant::now() - start);
            plan.ran(&channel, Instant::now(), Duration::ZERO);
        }
        let ten = Duration::from_secs(600);
        assert_eq!(firsts, [Duration::ZERO, ten, 2 * ten]);
        // The first channel comes round again one interval after its list.
        assert_eq!(plan.next().unwrap(), (channels[0].clone(), start + half));

        // Its feed works again: it drops out, and the others keep their turns.
        plan.sync(&channels[1..], Instant::now());
        assert_eq!(
            plan.next().unwrap(),
            (channels[1].clone(), start + ten + half)
        );
        plan.sync(&[], Instant::now());
        assert_eq!(plan.next(), None);

        // A channel failing again later starts at once, but never closer than the spacing.
        plan.sync(&channels[..1], Instant::now());
        assert_eq!(
            plan.next().unwrap(),
            (
                channels[0].clone(),
                start + 2 * ten + Duration::from_secs(120)
            )
        );
    }

    #[test]
    fn feed_failures_back_off() {
        let half = Duration::from_secs(30 * 60);
        let j = Duration::from_secs(7);
        assert_eq!(feed_delay(half, 0, j), half + j);
        assert_eq!(feed_delay(half, 1, Duration::ZERO), HOUR);
        assert_eq!(feed_delay(half, 2, Duration::ZERO), 2 * HOUR);
        assert_eq!(feed_delay(half, 4, Duration::ZERO), 6 * HOUR);
        assert_eq!(feed_delay(half, 40, Duration::ZERO), 6 * HOUR);
    }
}
