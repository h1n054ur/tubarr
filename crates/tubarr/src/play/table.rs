//! The sample table of one YouTube file: every sample's size, duration, sync flag and composition
//! offset, and the runs of samples (chunks) as byte ranges of the file. It is read from the moof of
//! every fragment, which takes one request per fragment, so it is kept on disk afterwards: varints
//! deflated into one file per video and format, a few hundred KB for a 40 minute episode. Deleting
//! the files is safe, the next play reads the moofs again. Each file carries the fingerprint of the
//! source file's moov and sidx it was read from, so a table is only used for the same stream.
//!
//! A table's modified time is when it was last used: a play that finds it sets it to now.
//! [`sweep`] removes the tables used longest ago once the folder is over its cap or they sit unused
//! too long. A play that loses its table that way reads the moofs again, as after a delete by
//! hand, and a play already reading a file keeps its open handle. The tables only speed plays up:
//! nothing decides from them which videos may play.

use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use flate2::{Compression, read::DeflateDecoder, write::DeflateEncoder};

use super::bmff::SampleRun;

/// First bytes of a cache file. A new layout gets a new magic and the old files are read as misses.
const MAGIC: &[u8; 4] = b"TBT2";

/// What a source file's moov and sidx hash to ([`super::vfile::TrackIndex::fingerprint`]).
pub type Fingerprint = [u8; 32];

/// A run of samples whose data lies back to back in the source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    /// Byte offset of the first sample in the source file.
    pub offset: u64,
    pub size: u64,
    pub samples: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackTable {
    pub sizes: Vec<u32>,
    pub durations: Vec<u32>,
    /// Composition offset of every sample, all 0 when the track has none.
    pub offsets: Vec<i32>,
    /// Sync samples, numbered from 1.
    pub sync: Vec<u32>,
    pub chunks: Vec<Chunk>,
    /// Sum of the durations, in media ticks.
    pub length: u64,
}

impl TrackTable {
    pub fn len(&self) -> usize {
        self.sizes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sizes.is_empty()
    }

    /// Adds the runs of one moof. A tfdt later than where the samples so far end stretches the last
    /// sample over the gap, so the flat timeline keeps the source's decode times.
    pub fn push_runs(&mut self, runs: Vec<SampleRun>) {
        for run in runs {
            if run.samples.is_empty() {
                continue;
            }
            if let Some(t) = run.decode_time
                && t > self.length
                && !self.is_empty()
            {
                self.extend_to(t);
            }
            let size: u64 = run.samples.iter().map(|s| u64::from(s.size)).sum();
            match self.chunks.last_mut() {
                Some(c) if c.offset + c.size == run.data_offset => {
                    c.size += size;
                    c.samples += run.samples.len() as u32;
                }
                _ => self.chunks.push(Chunk {
                    offset: run.data_offset,
                    size,
                    samples: run.samples.len() as u32,
                }),
            }
            for s in run.samples {
                self.sizes.push(s.size);
                self.durations.push(s.duration);
                self.offsets.push(s.offset);
                if s.sync {
                    self.sync.push(self.sizes.len() as u32);
                }
                self.length += u64::from(s.duration);
            }
        }
    }

    /// Adds the samples of `next` after these, as the next part of an episode.
    pub fn append(&mut self, next: &Self) {
        let before = self.len() as u32;
        self.sizes.extend_from_slice(&next.sizes);
        self.durations.extend_from_slice(&next.durations);
        self.offsets.extend_from_slice(&next.offsets);
        self.sync.extend(next.sync.iter().map(|n| n + before));
        self.chunks.extend_from_slice(&next.chunks);
        self.length += next.length;
    }

    /// Stretches the last sample so the track lasts `ticks`. Shorter targets change nothing.
    pub fn extend_to(&mut self, ticks: u64) {
        if let Some(last) = self.durations.last_mut()
            && ticks > self.length
        {
            let old = u64::from(*last);
            *last = u32::try_from(old + (ticks - self.length)).unwrap_or(u32::MAX);
            self.length = self.length - old + u64::from(*last);
        }
    }

    /// When the track is shown, in media ticks: its earliest presentation time (where an edit list
    /// must start it) and how long from there until the last sample is shown to its end.
    pub fn presentation(&self) -> (i64, u64) {
        let mut dts = 0i64;
        let (mut first, mut last) = (i64::MAX, 0i64);
        for (d, o) in self.durations.iter().zip(&self.offsets) {
            let pts = dts + i64::from(*o);
            first = first.min(pts);
            last = last.max(pts + i64::from(*d));
            dts += i64::from(*d);
        }
        if first == i64::MAX {
            return (0, 0);
        }
        (first, u64::try_from(last - first).unwrap_or(0))
    }

    /// The bytes of a cache file: the magic, the source's fingerprint, then the deflated table.
    pub fn encode(&self, fingerprint: &Fingerprint) -> Vec<u8> {
        let mut raw = Vec::with_capacity(self.len() * 3);
        put(&mut raw, self.len() as u64);
        for s in &self.sizes {
            put(&mut raw, u64::from(*s));
        }
        let durations = runs(&self.durations);
        put(&mut raw, durations.len() as u64);
        for (count, d) in durations {
            put(&mut raw, u64::from(count));
            put(&mut raw, u64::from(d));
        }
        let offsets = runs(&self.offsets);
        put(&mut raw, offsets.len() as u64);
        for (count, o) in offsets {
            put(&mut raw, u64::from(count));
            put(&mut raw, zigzag(i64::from(o)));
        }
        put(&mut raw, self.sync.len() as u64);
        let mut prev = 0;
        for n in &self.sync {
            put(&mut raw, u64::from(n - prev));
            prev = *n;
        }
        put(&mut raw, self.chunks.len() as u64);
        let mut end = 0u64;
        for c in &self.chunks {
            put(&mut raw, zigzag(c.offset as i64 - end as i64));
            put(&mut raw, u64::from(c.samples));
            end = c.offset + c.size;
        }
        let mut out = MAGIC.to_vec();
        out.extend_from_slice(fingerprint);
        let mut z = DeflateEncoder::new(&mut out, Compression::default());
        // Writing into a Vec cannot fail.
        let _ = z.write_all(&raw);
        let _ = z.finish();
        out
    }

    /// Reads a cache file back, with the fingerprint it was stored under. Anything that does not
    /// add up is `None`, a miss.
    pub fn decode(bytes: &[u8]) -> Option<(Fingerprint, Self)> {
        let (fingerprint, packed) = bytes.strip_prefix(MAGIC)?.split_first_chunk()?;
        let mut raw = Vec::new();
        DeflateDecoder::new(packed).read_to_end(&mut raw).ok()?;
        let mut r = Reader { data: &raw, at: 0 };
        let n = r.count(raw.len())?;
        let sizes = (0..n).map(|_| r.u32()).collect::<Option<Vec<u32>>>()?;
        let durations = r.runs(n, |r| r.u32())?;
        let offsets = r.runs(n, |r| i32::try_from(unzigzag(r.next()?)).ok())?;
        let sync_count = r.count(n)?;
        let mut sync = Vec::with_capacity(sync_count);
        let mut prev = 0u32;
        for _ in 0..sync_count {
            prev = prev.checked_add(r.u32()?)?;
            if prev == 0 || prev as usize > n {
                return None;
            }
            sync.push(prev);
        }
        let chunk_count = r.count(n)?;
        let mut chunks = Vec::with_capacity(chunk_count);
        let (mut end, mut first) = (0u64, 0usize);
        for _ in 0..chunk_count {
            let offset = u64::try_from(end as i64 + unzigzag(r.next()?)).ok()?;
            let samples = r.u32()?;
            let last = first.checked_add(samples as usize)?;
            let size = sizes.get(first..last)?.iter().map(|s| u64::from(*s)).sum();
            chunks.push(Chunk {
                offset,
                size,
                samples,
            });
            end = offset + size;
            first = last;
        }
        if first != n || r.at != raw.len() {
            return None;
        }
        let length = durations.iter().map(|d| u64::from(*d)).sum();
        let table = Self {
            sizes,
            durations,
            offsets,
            sync,
            chunks,
            length,
        };
        Some((*fingerprint, table))
    }
}

/// Where the table of one source file is kept: its video id, full format id (`140`, `140-drc` and
/// `140-1` are different streams) and size name it. Letters, digits and `-` stay as they are, any
/// other byte becomes `_` and two hex digits, so two format ids never share a name.
pub fn cache_path(dir: &Path, video_id: &str, format_id: &str, size: u64) -> PathBuf {
    let mut format = String::with_capacity(format_id.len());
    for b in format_id.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' {
            format.push(char::from(b));
        } else {
            format.push_str(&format!("_{b:02x}"));
        }
    }
    dir.join(format!("{video_id}.{format}.{size}.tbt"))
}

/// The cached table at `path` and the fingerprint it was stored under, if there is a readable one.
/// A table that was found has its modified time set to now, so [`sweep`] keeps it.
pub async fn load(path: &Path) -> Option<(Fingerprint, TrackTable)> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let cached = TrackTable::decode(&bytes);
    if cached.is_none() {
        tracing::warn!(path = %path.display(), "unreadable sample table, reading the moofs again");
    } else {
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || {
            // Opening for writing truncates nothing. A sweep may have removed it since the read.
            let touched = File::options()
                .write(true)
                .open(&path)
                .and_then(|f| f.set_modified(SystemTime::now()));
            if let Err(e) = touched {
                tracing::debug!(path = %path.display(), error = %e, "cannot mark sample table used");
            }
        });
    }
    cached
}

/// How many tables a folder holds and their bytes, for the Health page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct IndexUsage {
    pub files: u64,
    pub bytes: u64,
}

/// A temporary file this old was left by a write that never finished.
const STALE_TMP: Duration = Duration::from_secs(24 * 60 * 60);

/// The tables in `dir`, oldest use first, as `(modified, bytes, path)`, and the temporary files
/// left over from writes that died before `now - STALE_TMP`. A missing folder holds nothing.
fn scan(dir: &Path, now: SystemTime) -> (Vec<(SystemTime, u64, PathBuf)>, Vec<PathBuf>) {
    let (mut tables, mut stale) = (Vec::new(), Vec::new());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (tables, stale);
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let path = entry.path();
        let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        match path.extension().and_then(|e| e.to_str()) {
            Some("tbt") => tables.push((modified, meta.len(), path)),
            Some("tmp") if now.duration_since(modified).unwrap_or_default() > STALE_TMP => {
                stale.push(path);
            }
            _ => {}
        }
    }
    tables.sort();
    (tables, stale)
}

/// Counts the tables in `dir`. Blocking: call from `spawn_blocking`.
pub fn usage(dir: &Path) -> IndexUsage {
    let (tables, _) = scan(dir, SystemTime::now());
    IndexUsage {
        files: tables.len() as u64,
        bytes: tables.iter().map(|(_, size, _)| size).sum(),
    }
}

/// Removes the tables in `dir` not used within `max_idle`, then the ones used longest ago until the
/// rest fit in `max_bytes`, and temporary files a dead write left. A zero limit is no limit.
/// Returns what was removed. Blocking: call from `spawn_blocking`.
pub fn sweep(dir: &Path, max_bytes: u64, max_idle: Duration, now: SystemTime) -> IndexUsage {
    let (tables, stale) = scan(dir, now);
    for path in stale {
        let _ = std::fs::remove_file(path);
    }
    let mut left: u64 = tables.iter().map(|(_, size, _)| size).sum();
    let mut removed = IndexUsage::default();
    for (modified, size, path) in tables {
        let idle =
            !max_idle.is_zero() && now.duration_since(modified).unwrap_or_default() > max_idle;
        let over = max_bytes > 0 && left > max_bytes;
        if !idle && !over {
            // Oldest first: once one table stays, every newer one does too.
            break;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {
                left -= size;
                removed.files += 1;
                removed.bytes += size;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => left -= size,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "cannot remove sample table")
            }
        }
    }
    removed
}

/// Writes a table and the fingerprint of its source to `path` through a temporary file, so a
/// reader never sees half of one.
pub async fn store(
    path: &Path,
    fingerprint: &Fingerprint,
    table: &TrackTable,
) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        tokio::fs::create_dir_all(dir).await?;
    }
    let tmp = path.with_extension(format!("tbt.{}.tmp", std::process::id()));
    tokio::fs::write(&tmp, table.encode(fingerprint)).await?;
    tokio::fs::rename(&tmp, path).await
}

/// (count, value) runs of equal neighbours.
pub fn runs<T: Copy + PartialEq>(values: &[T]) -> Vec<(u32, T)> {
    let mut out: Vec<(u32, T)> = Vec::new();
    for v in values {
        match out.last_mut() {
            Some((count, last)) if last == v => *count += 1,
            _ => out.push((1, *v)),
        }
    }
    out
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    (v >> 1) as i64 ^ -((v & 1) as i64)
}

/// Appends an LEB128 varint.
fn put(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn next(&mut self) -> Option<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *self.data.get(self.at)?;
            self.at += 1;
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }

    fn u32(&mut self) -> Option<u32> {
        u32::try_from(self.next()?).ok()
    }

    /// A count no larger than `max`, so a damaged file cannot ask for a huge allocation.
    fn count(&mut self, max: usize) -> Option<usize> {
        usize::try_from(self.next()?).ok().filter(|n| *n <= max)
    }

    /// Runs expanded back into `n` values.
    fn runs<T: Copy>(
        &mut self,
        n: usize,
        value: impl Fn(&mut Self) -> Option<T>,
    ) -> Option<Vec<T>> {
        let count = self.count(n)?;
        let mut out = Vec::with_capacity(n);
        for _ in 0..count {
            let k = self.count(n - out.len())?;
            let v = value(self)?;
            out.extend(std::iter::repeat_n(v, k));
        }
        (out.len() == n).then_some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::play::bmff::FragmentSample;

    fn run(
        decode_time: Option<u64>,
        data_offset: u64,
        samples: &[(u32, u32, bool, i32)],
    ) -> SampleRun {
        SampleRun {
            decode_time,
            data_offset,
            samples: samples
                .iter()
                .map(|&(duration, size, sync, offset)| FragmentSample {
                    duration,
                    size,
                    sync,
                    offset,
                })
                .collect(),
        }
    }

    #[test]
    fn runs_join_chunks_and_gaps_stretch_the_last_sample() {
        let mut t = TrackTable::default();
        t.push_runs(vec![
            run(Some(0), 100, &[(10, 5, true, 20), (10, 7, false, 0)]),
            run(None, 112, &[(10, 3, false, -10)]),
        ]);
        t.push_runs(vec![run(Some(35), 500, &[(10, 4, true, 0)])]);
        assert_eq!(t.sizes, [5, 7, 3, 4]);
        assert_eq!(t.durations, [10, 10, 15, 10]);
        assert_eq!(t.sync, [1, 4]);
        assert_eq!(t.length, 45);
        assert_eq!(
            t.chunks,
            [
                Chunk {
                    offset: 100,
                    size: 15,
                    samples: 3
                },
                Chunk {
                    offset: 500,
                    size: 4,
                    samples: 1
                }
            ]
        );
        assert_eq!(t.presentation(), (10, 35));
        t.extend_to(60);
        assert_eq!((t.durations[3], t.length), (25, 60));
        assert_eq!(TrackTable::decode(&t.encode(&[7; 32])), Some(([7; 32], t)));
    }

    #[test]
    fn damaged_files_are_misses() {
        let mut t = TrackTable::default();
        t.push_runs(vec![run(None, 9, &[(1, 2, true, 0)])]);
        let good = t.encode(&[0; 32]);
        assert!(TrackTable::decode(&good).is_some());
        assert!(TrackTable::decode(&good[..good.len() - 2]).is_none());
        assert!(TrackTable::decode(b"TBT0xxxx").is_none());
        assert!(TrackTable::decode(b"TBT2short").is_none());
        assert!(TrackTable::decode(b"").is_none());
        let mut old = good.clone();
        old[..4].copy_from_slice(b"TBT1");
        assert!(TrackTable::decode(&old).is_none());
    }

    #[test]
    fn every_format_id_gets_its_own_file() {
        let name = |format: &str, size: u64| {
            cache_path(Path::new("/idx"), "ebioxADOBOw", format, size)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        assert_eq!(name("140", 36105504), "ebioxADOBOw.140.36105504.tbt");
        assert_eq!(
            name("140-drc", 36105504),
            "ebioxADOBOw.140-drc.36105504.tbt"
        );
        assert_eq!(name("140-2", 1), "ebioxADOBOw.140-2.1.tbt");
        assert_eq!(name("140.2/x", 1), "ebioxADOBOw.140_2e2_2fx.1.tbt");
        let ids = [
            "140", "140-drc", "140_drc", "140-2", "140_2", "140.2", "140 2",
        ];
        let names: std::collections::HashSet<_> = ids.iter().map(|id| name(id, 5)).collect();
        assert_eq!(names.len(), ids.len());
    }

    fn table_file(dir: &Path, name: &str, bytes: usize, age_days: u64, now: SystemTime) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, vec![0u8; bytes]).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(now - Duration::from_secs(age_days * 86_400))
            .unwrap();
        path
    }

    #[test]
    fn sweep_drops_idle_tables_then_the_oldest_over_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let idle = table_file(dir.path(), "a.137.1.tbt", 100, 200, now);
        let old = table_file(dir.path(), "b.137.1.tbt", 300, 20, now);
        let mid = table_file(dir.path(), "c.137.1.tbt", 300, 10, now);
        let new = table_file(dir.path(), "d.137.1.tbt", 300, 0, now);
        let writing = table_file(dir.path(), "e.137.1.tbt.7.tmp", 50, 0, now);
        let dead = table_file(dir.path(), "f.137.1.tbt.7.tmp", 50, 2, now);
        let other = table_file(dir.path(), "notes.txt", 50, 400, now);
        assert_eq!(
            usage(dir.path()),
            IndexUsage {
                files: 4,
                bytes: 1000
            }
        );

        let removed = sweep(dir.path(), 600, Duration::from_secs(180 * 86_400), now);
        assert_eq!(
            removed,
            IndexUsage {
                files: 2,
                bytes: 400
            }
        );
        assert!(!idle.exists() && !old.exists() && !dead.exists());
        assert!(mid.exists() && new.exists() && writing.exists() && other.exists());

        // Zero limits keep everything.
        assert_eq!(
            sweep(dir.path(), 0, Duration::ZERO, now),
            IndexUsage::default()
        );
        assert_eq!(usage(dir.path()).files, 2);
        assert_eq!(usage(&dir.path().join("missing")), IndexUsage::default());
    }

    #[tokio::test]
    async fn a_table_found_counts_as_used_now() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v.137.1.tbt");
        let mut table = TrackTable::default();
        table.push_runs(vec![run(Some(0), 0, &[(512, 10, true, 0)])]);
        store(&path, &[3; 32], &table).await.unwrap();
        let week_ago = SystemTime::now() - Duration::from_secs(7 * 86_400);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(week_ago)
            .unwrap();

        assert_eq!(load(&path).await, Some(([3; 32], table)));
        for _ in 0..100 {
            if std::fs::metadata(&path).unwrap().modified().unwrap() > week_ago {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the table's modified time stayed a week old");
    }

    /// A 40 minute episode with sizes as random as real ones: 1080p30 video in 5.12 s fragments and
    /// AAC audio in 10 s fragments. Prints the cache size per episode.
    #[test]
    fn a_long_episode_stays_well_under_a_megabyte() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rand = |range: u32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % u64::from(range)) as u32
        };
        let mut video = TrackTable::default();
        let mut offset = 1000u64;
        for f in 0..469u64 {
            let samples: Vec<_> = (0..154)
                .map(|k| {
                    let key = k == 0;
                    let size = if key {
                        60_000 + rand(40_000)
                    } else {
                        2_000 + rand(14_000)
                    };
                    (512, size, key, [1024, 0, 512][k % 3])
                })
                .collect();
            video.push_runs(vec![run(Some(f * 154 * 512), offset, &samples)]);
            offset += video.chunks.last().unwrap().size + 3000;
        }
        let mut audio = TrackTable::default();
        for f in 0..241u64 {
            let samples: Vec<_> = (0..431).map(|_| (1024, 300 + rand(180), true, 0)).collect();
            audio.push_runs(vec![run(Some(f * 431 * 1024), offset, &samples)]);
            offset += audio.chunks.last().unwrap().size + 3000;
        }
        let (v, a) = (video.encode(&[0; 32]).len(), audio.encode(&[0; 32]).len());
        println!("video {v} bytes, audio {a} bytes, episode {} bytes", v + a);
        assert!(v + a < 512 * 1024, "{v} + {a}");
        assert_eq!(
            TrackTable::decode(&video.encode(&[1; 32])),
            Some(([1; 32], video))
        );
    }
}
