//! Sample tables read in the background after a grab, so the first play of a video finds them on
//! disk instead of reading one range per fragment (thousands for a long podcast) before its first
//! byte.
//!
//! A grab queues each of its video ids. One worker reads them one at a time, with a rest between
//! two, through the same code a play uses, so the tables are the ones a play would read. Plays come
//! first: a background read waits before each request while any play reads file heads or tables,
//! and its lookup takes the limiter's background lane. A play that needs a table being read in the
//! background waits for that read and uses its result rather than reading it again.
//!
//! The queue is kept in the database as well, and read back at start, so a restart (an out of
//! memory kill, a redeploy) loses none of it. A failed read (YouTube refusing the stream, say) is
//! tried again later, each time after a longer wait, and given up on after [`MAX_ATTEMPTS`].

use std::{
    collections::{HashSet, VecDeque},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use tokio::sync::Notify;

use super::{PlayError, PlayState, lock, parts};
use crate::{ratelimit::Lane, resolver::ResolveCache, store::Db};

/// Videos queued for a background read at most, those waiting out a retry included. More are
/// dropped; their first play reads them.
const MAX_QUEUED: usize = 256;
/// Reads a video gets before it is given up on.
pub const MAX_ATTEMPTS: u32 = 3;

/// What the background reads have done since start. A video counts as finished or failed only
/// once it is off the stored queue.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrebuildStats {
    /// Videos taken into the queue, by a grab or read back at start.
    pub queued: u64,
    /// Videos turned away because the queue was full.
    pub dropped: u64,
    /// Videos whose tables are now on disk, read just now or found there.
    pub finished: u64,
    /// Failed reads that will be tried again.
    pub retried: u64,
    /// Videos given up on. Their first play reads the tables itself.
    pub failed: u64,
    /// Tables read from the moofs, two per video that was not cached yet.
    pub tables_read: u64,
}

/// A queued video and how many of its reads failed so far.
#[derive(Debug)]
struct Job {
    id: String,
    attempts: u32,
}

#[derive(Debug, Default)]
struct Queue {
    waiting: VecDeque<Job>,
    /// Waiting, being read or waiting out a retry, so a video is never queued twice. The same
    /// videos as the stored queue.
    pending: HashSet<String>,
    running: bool,
}

/// The queue of videos waiting for a background read, and its counters.
#[derive(Debug, Default)]
pub(super) struct Prebuilds {
    queue: Mutex<Queue>,
    /// Where the queue is kept between runs. Without it the queue lives in memory only.
    pub(super) store: Option<Db>,
    queued: AtomicU64,
    dropped: AtomicU64,
    finished: AtomicU64,
    retried: AtomicU64,
    failed: AtomicU64,
    tables_read: AtomicU64,
}

impl Prebuilds {
    pub(super) fn table_read(&self) {
        self.tables_read.fetch_add(1, Ordering::Relaxed);
    }

    /// Marks `id` as queued, unless it is already or the queue is full.
    fn claim(&self, id: &str) -> bool {
        let mut queue = lock(&self.queue);
        if queue.pending.contains(id) {
            return false;
        }
        if queue.pending.len() >= MAX_QUEUED {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            tracing::info!(video_id = id, "table queue full, the first play reads them");
            return false;
        }
        self.queued.fetch_add(1, Ordering::Relaxed);
        queue.pending.insert(id.to_owned());
        true
    }

    /// Takes `id` off the stored queue, then off the pending set, so a grab of it from then on
    /// stores it again.
    async fn done(&self, id: &str) {
        if let Some(db) = &self.store
            && let Err(e) = db.unqueue_prebuild(id).await
        {
            tracing::warn!(video_id = id, error = %e, "cannot take a video off the stored table queue");
        }
        lock(&self.queue).pending.remove(id);
    }
}

/// Counts the plays reading file heads or tables, so background reads can wait for none.
#[derive(Debug, Default)]
pub(super) struct PlayGate {
    reading: AtomicUsize,
    idle: Notify,
}

/// A play's place in [`PlayGate`], given back on drop.
pub(super) struct Reading<'a>(&'a PlayGate);

impl Drop for Reading<'_> {
    fn drop(&mut self) {
        if self.0.reading.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.0.idle.notify_waiters();
        }
    }
}

impl PlayGate {
    /// Marks a play as reading until the guard drops. Background reads get nothing.
    pub(super) fn enter(&self, lane: Lane) -> Option<Reading<'_>> {
        (lane == Lane::Play).then(|| {
            self.reading.fetch_add(1, Ordering::AcqRel);
            Reading(self)
        })
    }

    /// Returns at once for a play, and once no play is reading for a background read.
    pub(super) async fn wait(&self, lane: Lane) {
        if lane == Lane::Play {
            return;
        }
        loop {
            let idle = self.idle.notified();
            tokio::pin!(idle);
            idle.as_mut().enable();
            if self.reading.load(Ordering::Acquire) == 0 {
                return;
            }
            idle.await;
        }
    }
}

impl<C: ResolveCache + 'static> PlayState<C> {
    /// Queues the videos of a grab (one id, or a split upload's ids joined by commas) for a
    /// background read of their sample tables. Does nothing when switched off or without an
    /// `index_dir` to keep them in. Each new video is stored before it is read.
    pub async fn prebuild(self: &Arc<Self>, video_ids: &str) {
        if !self.prebuilds_on() {
            return;
        }
        let Ok(ids) = parts::parse(video_ids) else {
            return;
        };
        let p = &self.prebuilds;
        let mut jobs = Vec::new();
        for id in ids {
            if !p.claim(&id) {
                continue;
            }
            if let Some(db) = &p.store
                && let Err(e) = db.queue_prebuild(&id).await
            {
                tracing::warn!(video_id = id, error = %e, "cannot store a video in the table queue, a restart loses it");
            }
            jobs.push(Job { id, attempts: 0 });
        }
        self.enqueue(jobs);
    }

    /// Queues again what the stored queue held when Tubarr last stopped, with the failed reads
    /// counted. Called once at start.
    pub async fn resume_prebuilds(self: &Arc<Self>) {
        let Some(db) = &self.prebuilds.store else {
            return;
        };
        if !self.prebuilds_on() {
            return;
        }
        let stored = match db.queued_prebuilds().await {
            Ok(stored) => stored,
            Err(e) => {
                tracing::warn!(error = %e, "cannot read the stored table queue");
                return;
            }
        };
        let jobs: Vec<Job> = stored
            .into_iter()
            .filter(|q| self.prebuilds.claim(&q.video_id))
            .map(|q| Job {
                id: q.video_id,
                attempts: u32::try_from(q.attempts).unwrap_or(0),
            })
            .collect();
        if !jobs.is_empty() {
            tracing::info!(
                videos = jobs.len(),
                "background table reads picked up again"
            );
        }
        self.enqueue(jobs);
    }

    fn prebuilds_on(&self) -> bool {
        self.config.prebuild_tables && self.index_dir.is_some()
    }

    /// Puts claimed jobs at the end of the queue and starts the worker if it is not running.
    fn enqueue(self: &Arc<Self>, jobs: Vec<Job>) {
        let start = {
            let mut queue = lock(&self.prebuilds.queue);
            queue.waiting.extend(jobs);
            let start = !queue.running && !queue.waiting.is_empty();
            queue.running |= start;
            start
        };
        if start {
            tokio::spawn(self.clone().run_prebuilds());
        }
    }

    pub fn prebuild_stats(&self) -> PrebuildStats {
        let p = &self.prebuilds;
        let n = |c: &AtomicU64| c.load(Ordering::Relaxed);
        PrebuildStats {
            queued: n(&p.queued),
            dropped: n(&p.dropped),
            finished: n(&p.finished),
            retried: n(&p.retried),
            failed: n(&p.failed),
            tables_read: n(&p.tables_read),
        }
    }

    /// Reads the queued videos one after the other until the queue is empty.
    async fn run_prebuilds(self: Arc<Self>) {
        let p = &self.prebuilds;
        loop {
            let next = {
                let mut queue = lock(&p.queue);
                let next = queue.waiting.pop_front();
                queue.running = next.is_some();
                next
            };
            let Some(Job { id, attempts }) = next else {
                return;
            };
            match self.prebuild_one(&id).await {
                Ok(()) => {
                    p.done(&id).await;
                    p.finished.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => self.prebuild_failed(id, attempts + 1, &e).await,
            }
            tokio::time::sleep(self.config.prebuild_pause).await;
        }
    }

    /// Counts a failed read and queues the video again after a wait, or gives up on it after
    /// [`MAX_ATTEMPTS`].
    async fn prebuild_failed(self: &Arc<Self>, id: String, attempts: u32, e: &PlayError) {
        let p = &self.prebuilds;
        if attempts >= MAX_ATTEMPTS {
            tracing::info!(video_id = id, attempts, error = %e, "background table read failed for the last time, the first play reads them");
            p.done(&id).await;
            p.failed.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if let Some(db) = &p.store
            && let Err(e) = db.prebuild_failed(&id, &e.to_string()).await
        {
            tracing::warn!(video_id = id, error = %e, "cannot count a failed read in the stored table queue");
        }
        p.retried.fetch_add(1, Ordering::Relaxed);
        let wait = self.config.prebuild_retry * 2u32.pow(attempts - 1);
        tracing::info!(video_id = id, attempts, error = %e, retry_in = ?wait, "background table read failed, trying again later");
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(wait).await;
            this.enqueue(vec![Job { id, attempts }]);
        });
    }

    /// Looks the video up and reads both its tables the way a play does, unless a file built
    /// from it is still in memory.
    async fn prebuild_one(&self, id: &str) -> Result<(), PlayError> {
        let built = lock(&self.files)
            .values()
            .any(|p| p.parts.iter().any(|r| r.meta.id == id));
        if built {
            return Ok(());
        }
        self.gate.wait(Lane::Background).await;
        let resolved = self.resolver.resolve(id).await?;
        self.read_part(&resolved, Lane::Background).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A background read waits while a play reads and goes on once the last one is done. A play
    /// never waits.
    #[tokio::test]
    async fn background_reads_wait_for_plays() {
        let gate = PlayGate::default();
        assert!(gate.enter(Lane::Background).is_none());
        gate.wait(Lane::Background).await;
        let first = gate.enter(Lane::Play);
        let second = gate.enter(Lane::Play);
        gate.wait(Lane::Play).await;
        let short = Duration::from_millis(50);
        assert!(
            tokio::time::timeout(short, gate.wait(Lane::Background))
                .await
                .is_err()
        );
        let waiting = gate.wait(Lane::Background);
        tokio::pin!(waiting);
        drop(first);
        assert!(tokio::time::timeout(short, &mut waiting).await.is_err());
        drop(second);
        tokio::time::timeout(short, waiting).await.unwrap();
    }
}
