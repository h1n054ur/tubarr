//! A small MP4 box walker for the play tests, separate from the one under test.

pub fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

pub fn be64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}

/// (kind, start, end) of the boxes in `b[from..to]`. Reads 64-bit sizes.
pub fn walk(b: &[u8], from: usize, to: usize) -> Vec<(String, usize, usize)> {
    let mut out = Vec::new();
    let mut at = from;
    while at + 8 <= to {
        let size = match be32(b, at) {
            1 => be64(b, at + 8) as usize,
            n => n as usize,
        };
        out.push((
            String::from_utf8_lossy(&b[at + 4..at + 8]).into_owned(),
            at,
            at + size,
        ));
        at += size;
    }
    out
}

pub fn kid(b: &[u8], parent: (usize, usize), kind: &str) -> (usize, usize) {
    find_kid(b, parent, kind).unwrap_or_else(|| panic!("no {kind}"))
}

pub fn find_kid(b: &[u8], parent: (usize, usize), kind: &str) -> Option<(usize, usize)> {
    walk(b, parent.0 + 8, parent.1)
        .into_iter()
        .find(|k| k.0 == kind)
        .map(|k| (k.1, k.2))
}

pub fn kids(b: &[u8], parent: (usize, usize), kind: &str) -> Vec<(usize, usize)> {
    walk(b, parent.0 + 8, parent.1)
        .into_iter()
        .filter(|k| k.0 == kind)
        .map(|k| (k.1, k.2))
        .collect()
}

pub fn top(b: &[u8], kind: &str) -> (usize, usize) {
    walk(b, 0, b.len())
        .into_iter()
        .find(|k| k.0 == kind)
        .map(|k| (k.1, k.2))
        .unwrap_or_else(|| panic!("no {kind}"))
}

pub fn tkhd_id(b: &[u8], trak: (usize, usize)) -> u32 {
    let tkhd = kid(b, trak, "tkhd");
    let skip = if b[tkhd.0 + 8] == 1 { 16 } else { 8 };
    be32(b, tkhd.0 + 12 + skip)
}

pub fn timescale(b: &[u8], trak: (usize, usize)) -> u32 {
    let mdhd = kid(b, kid(b, trak, "mdia"), "mdhd");
    let skip = if b[mdhd.0 + 8] == 1 { 16 } else { 8 };
    be32(b, mdhd.0 + 12 + skip)
}

/// All mdat payloads of a source file, joined.
pub fn source_media(src: &[u8]) -> Vec<u8> {
    walk(src, 0, src.len())
        .into_iter()
        .filter(|k| k.0 == "mdat")
        .flat_map(|k| src[k.1 + 8..k.2].to_vec())
        .collect()
}

/// Number of samples the truns of a source file list.
pub fn source_samples(src: &[u8]) -> usize {
    walk(src, 0, src.len())
        .into_iter()
        .filter(|k| k.0 == "moof")
        .flat_map(|m| kids(src, (m.1, m.2), "traf"))
        .flat_map(|t| kids(src, t, "trun"))
        .map(|r| be32(src, r.0 + 12) as usize)
        .sum()
}

/// A source file's timescale and length in its own ticks, from its (version 1) sidx.
pub fn source_length(src: &[u8]) -> (u32, u64) {
    let sidx = top(src, "sidx");
    let at = sidx.0 + 8;
    assert_eq!(src[at], 1);
    let count = be32(src, at + 28) & 0xffff;
    let ticks = (0..count as usize)
        .map(|k| u64::from(be32(src, at + 36 + 12 * k)))
        .sum();
    (be32(src, at + 8), ticks)
}

/// The sample table of one trak of a flat file, expanded per sample.
#[derive(Debug)]
pub struct Samples {
    pub id: u32,
    pub timescale: u32,
    /// Absolute offset and size of every sample.
    pub data: Vec<(u64, u32)>,
    pub durations: Vec<u32>,
    /// Sync sample numbers from 1, `None` without an stss (every sample syncs).
    pub sync: Option<Vec<u32>>,
    /// Absolute chunk offsets.
    pub chunks: Vec<u64>,
    pub has_ctts: bool,
}

impl Samples {
    pub fn ticks(&self) -> u64 {
        self.durations.iter().map(|d| u64::from(*d)).sum()
    }

    pub fn bytes(&self, file: &[u8]) -> Vec<u8> {
        self.data
            .iter()
            .flat_map(|&(at, size)| file[at as usize..at as usize + size as usize].to_vec())
            .collect()
    }
}

fn entries(b: &[u8], full: (usize, usize)) -> (usize, usize) {
    (be32(b, full.0 + 12) as usize, full.0 + 16)
}

/// Reads a trak's stbl the way a progressive player does.
pub fn samples(b: &[u8], trak: (usize, usize)) -> Samples {
    let stbl = kid(b, kid(b, kid(b, trak, "mdia"), "minf"), "stbl");
    let (n, at) = entries(b, kid(b, stbl, "stts"));
    let mut durations = Vec::new();
    for k in 0..n {
        let e = at + 8 * k;
        durations.extend(std::iter::repeat_n(be32(b, e + 4), be32(b, e) as usize));
    }
    let stsz = kid(b, stbl, "stsz");
    let fixed = be32(b, stsz.0 + 12);
    let count = be32(b, stsz.0 + 16) as usize;
    let sizes: Vec<u32> = (0..count)
        .map(|k| {
            if fixed != 0 {
                fixed
            } else {
                be32(b, stsz.0 + 20 + 4 * k)
            }
        })
        .collect();
    let chunks: Vec<u64> = match find_kid(b, stbl, "stco") {
        Some(stco) => {
            let (n, at) = entries(b, stco);
            (0..n).map(|k| u64::from(be32(b, at + 4 * k))).collect()
        }
        None => {
            let (n, at) = entries(b, kid(b, stbl, "co64"));
            (0..n).map(|k| be64(b, at + 8 * k)).collect()
        }
    };
    let (n, at) = entries(b, kid(b, stbl, "stsc"));
    let runs: Vec<(usize, usize)> = (0..n)
        .map(|k| {
            (
                be32(b, at + 12 * k) as usize,
                be32(b, at + 12 * k + 4) as usize,
            )
        })
        .collect();
    let mut data = Vec::with_capacity(count);
    for (c, offset) in chunks.iter().enumerate() {
        let per = runs.iter().rev().find(|r| r.0 <= c + 1).unwrap().1;
        let mut pos = *offset;
        for _ in 0..per {
            let size = sizes[data.len()];
            data.push((pos, size));
            pos += u64::from(size);
        }
    }
    assert_eq!(data.len(), count, "stsc covers every sample");
    assert_eq!(durations.len(), count, "stts covers every sample");
    let sync = find_kid(b, stbl, "stss").map(|s| {
        let (n, at) = entries(b, s);
        (0..n).map(|k| be32(b, at + 4 * k)).collect()
    });
    Samples {
        id: tkhd_id(b, trak),
        timescale: timescale(b, trak),
        data,
        durations,
        sync,
        chunks,
        has_ctts: find_kid(b, stbl, "ctts").is_some(),
    }
}

/// Checks the flat layout (ftyp, moov, one mdat running to the end, nothing fragmented) and reads
/// both tracks. Every chunk lies inside the mdat and the chunks of both tracks tile it exactly.
pub fn flat_tracks(b: &[u8]) -> [Samples; 2] {
    flat_samples(b).try_into().expect("two tracks")
}

/// [`flat_tracks`] for any number of tracks.
pub fn flat_samples(b: &[u8]) -> Vec<Samples> {
    let kinds: Vec<String> = walk(b, 0, b.len()).into_iter().map(|k| k.0).collect();
    assert_eq!(kinds, ["ftyp", "moov", "mdat"]);
    let moov = top(b, "moov");
    assert!(find_kid(b, moov, "mvex").is_none(), "no mvex");
    let mdat = top(b, "mdat");
    assert_eq!(mdat.1, b.len());
    let header = if be32(b, mdat.0) == 1 { 16 } else { 8 };
    let tracks: Vec<Samples> = kids(b, moov, "trak")
        .into_iter()
        .map(|t| samples(b, t))
        .collect();
    let mut spans: Vec<(u64, u64)> = tracks
        .iter()
        .flat_map(|t| t.data.iter().map(|&(at, size)| (at, at + u64::from(size))))
        .collect();
    spans.sort_unstable();
    let mut pos = (mdat.0 + header) as u64;
    for (start, end) in spans {
        assert_eq!(start, pos, "samples tile the mdat");
        pos = end;
    }
    assert_eq!(pos, b.len() as u64);
    tracks
}

/// mvhd duration, then tkhd and mdhd duration per track, of a flat file (all version 0).
pub fn moov_durations(file: &[u8]) -> (u64, Vec<(u64, u64)>) {
    let moov = top(file, "moov");
    let mvhd = kid(file, moov, "mvhd");
    let traks = kids(file, moov, "trak")
        .into_iter()
        .map(|t| {
            let tkhd = kid(file, t, "tkhd");
            let mdhd = kid(file, kid(file, t, "mdia"), "mdhd");
            (
                u64::from(be32(file, tkhd.0 + 28)),
                u64::from(be32(file, mdhd.0 + 24)),
            )
        })
        .collect();
    (u64::from(be32(file, mvhd.0 + 24)), traks)
}

/// Runs ffprobe on `url` and returns the container duration in seconds, after checking both
/// streams are there. Needs ffprobe on the PATH.
pub async fn probe_duration(url: &str, codecs: &[&str]) -> f64 {
    let out = tokio::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration:stream=codec_name",
            "-of",
            "compact",
            url,
        ])
        .output()
        .await
        .expect("ffprobe on the PATH");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let found: Vec<&str> = text
        .lines()
        .filter_map(|l| l.strip_prefix("stream|codec_name="))
        .collect();
    assert_eq!(found, codecs, "{text}");
    text.lines()
        .find_map(|l| l.strip_prefix("format|duration="))
        .unwrap()
        .parse()
        .unwrap()
}

/// Decodes `url` with ffmpeg from `seek` seconds and fails on any error. Needs ffmpeg on the PATH.
pub async fn decode_from(url: &str, seek: &str) {
    let out = tokio::process::Command::new("ffmpeg")
        .args(["-v", "error", "-ss", seek, "-i", url, "-f", "null", "-"])
        .output()
        .await
        .expect("ffmpeg on the PATH");
    let errors = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success() && errors.is_empty(),
        "seek {seek}: {errors}"
    );
}
