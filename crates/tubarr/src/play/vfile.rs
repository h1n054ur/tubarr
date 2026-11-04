//! The virtual muxed MP4: YouTube's video-only and audio-only fragmented files joined into one flat
//! (progressive) MP4 without touching the media data.
//!
//! The file is an ftyp, a complete moov up front, then one mdat. The moov is the video file's with
//! the audio trak added as track 2, mvex dropped and a full sample table in each trak (stts, ctts
//! when the source has composition offsets, stss for video, stsc, stsz, stco or co64), built from
//! the samples the source fragments list in their moofs ([`TrackTable`]). The mdat holds the sample
//! data of every fragment of both tracks ordered by start time, each fragment's data one chunk, so
//! any byte range of the file maps onto header bytes and byte ranges of the two YouTube files.
//! Players seek with the sample tables alone and never have to guess where a fragment starts,
//! which Samsung's AVPlay does in a fragmented file.
//!
//! An episode uploaded in several parts becomes one file the same way: the moov of the first part
//! with the sample tables of all parts joined, and each later part placed where the longer track of
//! the part before it ends (the last sample of the shorter track is stretched over the gap). Parts
//! must decode alike (same codecs, picture size, audio layout and timescales), since one sample
//! description serves them all.

use std::{cmp::Ordering, ops::Range};

use bytes::Bytes;
use sha2::{Digest, Sha256};

use super::{
    bmff::{self, BmffError, Sidx},
    table::{Fingerprint, TrackTable, runs},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Track {
    Video,
    Audio,
}

/// Which tracks a virtual file carries: both, or the audio alone for listening.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layout {
    Muxed,
    AudioOnly,
}

impl Layout {
    /// The tracks in file order: the n-th is track n + 1.
    fn tracks(self) -> &'static [Track] {
        match self {
            Self::Muxed => &[Track::Video, Track::Audio],
            Self::AudioOnly => &[Track::Audio],
        }
    }

    /// The ftyp: a plain ISO MP4 with H.264 as ffmpeg writes one, or an M4A.
    fn ftyp(self) -> Vec<u8> {
        match self {
            Self::Muxed => bmff::make_box(b"ftyp", b"isom\0\0\x02\0isomiso2avc1mp41"),
            Self::AudioOnly => bmff::make_box(b"ftyp", b"M4A \0\0\x02\0M4A isomiso2mp41"),
        }
    }
}

/// One fragment (moof + mdat) of a source file, as its sidx lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fragment {
    /// Byte offset in the source file.
    pub offset: u64,
    pub size: u32,
}

/// What the start of one source file says about it.
#[derive(Debug, Clone)]
pub struct TrackIndex {
    pub moov: Vec<u8>,
    pub fragments: Vec<Fragment>,
}

/// Result of reading the start of a file: either the index or how many bytes are needed for it.
#[derive(Debug)]
pub enum Parsed {
    Index(TrackIndex),
    NeedBytes(u64),
}

impl TrackIndex {
    /// Reads moov and sidx from the first bytes of a file.
    pub fn parse(head: &[u8]) -> Result<Parsed, BmffError> {
        let mut moov = None;
        for b in bmff::boxes(head)? {
            let wanted = matches!(&b.kind, b"ftyp" | b"moov" | b"sidx");
            if !wanted && matches!(&b.kind, b"moof" | b"mdat") {
                return Err(BmffError::Missing("sidx before the first fragment".into()));
            }
            let Some(range) = b.range(head.len()) else {
                return Ok(Parsed::NeedBytes(b.end()));
            };
            match &b.kind {
                b"moov" => moov = Some(head[range].to_vec()),
                b"sidx" => {
                    let sidx = Sidx::parse(&head[range])?;
                    let moov = moov.ok_or_else(|| BmffError::Missing("moov".into()))?;
                    return Ok(Parsed::Index(Self::from_sidx(moov, &sidx, b.end())));
                }
                _ => {}
            }
        }
        Err(BmffError::Missing("sidx".into()))
    }

    fn from_sidx(moov: Vec<u8>, sidx: &Sidx, sidx_end: u64) -> Self {
        let mut offset = sidx_end + sidx.first_offset;
        let fragments = sidx
            .refs
            .iter()
            .map(|r| {
                let f = Fragment {
                    offset,
                    size: r.size,
                };
                offset += u64::from(r.size);
                f
            })
            .collect();
        Self { moov, fragments }
    }

    /// A hash of the moov and of every fragment's byte range. Two streams that share a format id
    /// and size (YouTube's plain and DRC mixes can) still differ here, so a cached sample table is
    /// only used for the stream it was read from.
    pub fn fingerprint(&self) -> Fingerprint {
        let mut hash = Sha256::new();
        hash.update((self.moov.len() as u64).to_be_bytes());
        hash.update(&self.moov);
        for f in &self.fragments {
            hash.update(f.offset.to_be_bytes());
            hash.update(f.size.to_be_bytes());
        }
        hash.finalize().into()
    }

    /// Where the last fragment ends: the size of the source file, as far as playing it goes.
    pub fn end(&self) -> u64 {
        self.fragments
            .last()
            .map_or(0, |f| f.offset + u64::from(f.size))
    }
}

/// One track of one part: its moov and every sample its fragments hold.
#[derive(Debug, Clone)]
pub struct TrackData {
    pub moov: Vec<u8>,
    pub table: TrackTable,
    /// The source file's size as googlevideo reports it, if it did.
    pub size: Option<u64>,
}

/// Video and audio of one part.
#[derive(Debug, Clone)]
pub struct PartIndex {
    pub video: TrackData,
    pub audio: TrackData,
}

impl PartIndex {
    fn track(&self, track: Track) -> &TrackData {
        match track {
            Track::Video => &self.video,
            Track::Audio => &self.audio,
        }
    }
}

/// One chunk of the mdat: a byte range of a source file and where it sits in the virtual file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub pos: u64,
    pub part: usize,
    pub track: Track,
    /// Byte offset in the source file.
    pub offset: u64,
    pub size: u64,
}

/// A part of a byte range of the virtual file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// Bytes of the in-memory header: ftyp, moov and the mdat header.
    Head(Range<usize>),
    /// `len` bytes from `skip` into chunk `index`.
    Chunk {
        index: usize,
        skip: usize,
        len: usize,
    },
}

#[derive(Debug, Clone)]
pub struct VirtualFile {
    head: Bytes,
    chunks: Vec<Placed>,
    size: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error(transparent)]
    Layout(#[from] BmffError),
    #[error("part {part} cannot follow part 1: its {what} differs")]
    Mismatch { part: usize, what: String },
}

/// One track of the virtual file: the joined sample table and which part each chunk comes from.
struct Joined {
    table: TrackTable,
    parts: Vec<usize>,
    /// Decode time of each chunk's first sample, in media ticks.
    starts: Vec<u64>,
    timescale: u32,
}

impl VirtualFile {
    /// Joins the parts in order. One part gives the plain virtual file. The moov is that of the
    /// first track of `layout`, so an audio-only file takes the audio source's.
    pub fn build(parts: &[PartIndex], layout: Layout) -> Result<Self, BuildError> {
        let first = parts
            .first()
            .ok_or_else(|| BmffError::Missing("parts".into()))?;
        let kinds = layout.tracks();
        for (k, part) in parts.iter().enumerate().skip(1) {
            check_compatible(first, part, k + 1, kinds)?;
        }
        let base_moov = &first.track(kinds[0]).moov;
        let movie_scale =
            bmff::mvhd_timescale(&base_moov[bmff::find(base_moov, &[b"moov", b"mvhd"])?])?;
        if movie_scale == 0 {
            return Err(BmffError::Unsupported("mvhd timescale 0".into()).into());
        }
        let tracks = kinds
            .iter()
            .map(|&t| joined(parts, t))
            .collect::<Result<Vec<_>, _>>()?;
        let slot = |t: Track| kinds.iter().position(|&k| k == t).unwrap_or_default();
        let pick = |t: Track| &tracks[slot(t)];

        let mut order: Vec<(Track, usize)> = kinds
            .iter()
            .flat_map(|&t| (0..pick(t).starts.len()).map(move |i| (t, i)))
            .collect();
        // Compare start times across timescales exactly. On a tie audio goes first.
        order.sort_by(|&(ta, ia), &(tb, ib)| {
            let lhs = u128::from(pick(ta).starts[ia]) * u128::from(pick(tb).timescale);
            let rhs = u128::from(pick(tb).starts[ib]) * u128::from(pick(ta).timescale);
            lhs.cmp(&rhs).then_with(|| match (ta, tb) {
                (Track::Audio, Track::Video) => Ordering::Less,
                (Track::Video, Track::Audio) => Ordering::Greater,
                _ => ia.cmp(&ib),
            })
        });
        // Chunk positions inside the mdat payload, per track in chunk order.
        let mut within: Vec<Vec<u64>> = tracks.iter().map(|j| vec![0; j.starts.len()]).collect();
        let mut payload = 0u64;
        for &(t, i) in &order {
            within[slot(t)][i] = payload;
            payload += pick(t).table.chunks[i].size;
        }

        let ftyp = layout.ftyp();
        let moov_for = |base: u64, wide: bool| {
            let traks: Vec<FlatTrak<'_>> = kinds
                .iter()
                .enumerate()
                .map(|(n, &t)| FlatTrak {
                    moov: &first.track(t).moov,
                    joined: &tracks[n],
                    offsets: within[n].iter().map(|p| base + p).collect(),
                })
                .collect();
            flat_moov(base_moov, &traks, movie_scale, wide)
        };
        let narrow_len = moov_for(0, false)?.len() as u64;
        let wide = ftyp.len() as u64 + narrow_len + 16 + payload > u64::from(u32::MAX);
        let mdat = bmff::mdat_header(payload);
        let moov_len = if wide {
            moov_for(0, true)?.len() as u64
        } else {
            narrow_len
        };
        let base = ftyp.len() as u64 + moov_len + mdat.len() as u64;
        let moov = moov_for(base, wide)?;
        debug_assert_eq!(moov.len() as u64, moov_len);

        let mut head = Vec::with_capacity(base as usize);
        head.extend_from_slice(&ftyp);
        head.extend_from_slice(&moov);
        head.extend_from_slice(&mdat);
        let chunks = order
            .into_iter()
            .map(|(t, i)| {
                let j = pick(t);
                let c = j.table.chunks[i];
                Placed {
                    pos: base + within[slot(t)][i],
                    part: j.parts[i],
                    track: t,
                    offset: c.offset,
                    size: c.size,
                }
            })
            .collect();
        Ok(Self {
            head: Bytes::from(head),
            chunks,
            size: base + payload,
        })
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    pub fn head(&self) -> &Bytes {
        &self.head
    }

    pub fn chunk(&self, index: usize) -> Placed {
        self.chunks[index]
    }

    /// The pieces that make up bytes `start..=end`, in order. `end` must be below [`Self::size`].
    pub fn pieces(&self, start: u64, end: u64) -> Vec<Piece> {
        let mut out = Vec::new();
        let head_len = self.head.len() as u64;
        if start < head_len {
            out.push(Piece::Head(
                start as usize..(end + 1).min(head_len) as usize,
            ));
        }
        let first = self
            .chunks
            .partition_point(|p| p.pos <= start)
            .saturating_sub(1);
        for (index, p) in self.chunks.iter().enumerate().skip(first) {
            if p.pos > end {
                break;
            }
            let chunk_end = p.pos + p.size;
            if chunk_end <= start {
                continue;
            }
            let lo = start.max(p.pos);
            let hi = (end + 1).min(chunk_end);
            out.push(Piece::Chunk {
                index,
                skip: (lo - p.pos) as usize,
                len: (hi - lo) as usize,
            });
        }
        out
    }
}

/// Refuses a part that one moov cannot describe together with the first part.
fn check_compatible(
    first: &PartIndex,
    part: &PartIndex,
    number: usize,
    tracks: &[Track],
) -> Result<(), BuildError> {
    let mismatch = |what: &str| BuildError::Mismatch {
        part: number,
        what: what.to_owned(),
    };
    for &track in tracks {
        let name = match track {
            Track::Video => "video",
            Track::Audio => "audio",
        };
        let (a, b) = (&first.track(track).moov, &part.track(track).moov);
        if bmff::sample_signature(a)? != bmff::sample_signature(b)? {
            return Err(mismatch(&format!("{name} format")));
        }
        if media_timescale(a)? != media_timescale(b)? {
            return Err(mismatch(&format!("{name} timescale")));
        }
    }
    Ok(())
}

fn media_timescale(moov: &[u8]) -> Result<u32, BmffError> {
    let scale =
        bmff::mdhd_timescale(&moov[bmff::find(moov, &[b"moov", b"trak", b"mdia", b"mdhd"])?])?;
    if scale == 0 {
        return Err(BmffError::Unsupported("mdhd timescale 0".into()));
    }
    Ok(scale)
}

/// The samples of one track across the parts. Each later part starts where the longer track of the
/// part before it ends, so audio and video stay in step at every join.
fn joined(parts: &[PartIndex], track: Track) -> Result<Joined, BmffError> {
    let timescale = media_timescale(&parts[0].track(track).moov)?;
    let mut table = TrackTable::default();
    let mut owners = Vec::new();
    let mut end = 0u64;
    for (k, part) in parts.iter().enumerate() {
        table.extend_to(end);
        let own = &part.track(track).table;
        table.append(own);
        owners.extend(std::iter::repeat_n(k, own.chunks.len()));
        // The part's length in seconds is the longer of its two tracks, as length / timescale.
        let length = |d: &TrackData| -> Result<(u64, u32), BmffError> {
            Ok((d.table.length, media_timescale(&d.moov)?))
        };
        let ((v, vs), (a, as_)) = (length(&part.video)?, length(&part.audio)?);
        let (ticks, scale) = if u128::from(v) * u128::from(as_) >= u128::from(a) * u128::from(vs) {
            (v, vs)
        } else {
            (a, as_)
        };
        let shift = (u128::from(ticks) * u128::from(timescale)).div_ceil(u128::from(scale));
        end += u64::try_from(shift).unwrap_or(u64::MAX);
    }
    let mut starts = Vec::with_capacity(table.chunks.len());
    let mut dts = 0u64;
    let mut sample = 0usize;
    for c in &table.chunks {
        starts.push(dts);
        let next = sample + c.samples as usize;
        dts += table.durations[sample..next]
            .iter()
            .map(|d| u64::from(*d))
            .sum::<u64>();
        sample = next;
    }
    Ok(Joined {
        table,
        parts: owners,
        starts,
        timescale,
    })
}

/// A length in ticks of `from`, in ticks of `to`, rounded to the nearest.
fn rescale(ticks: u64, from: u32, to: u32) -> u64 {
    let t = u128::from(ticks) * u128::from(to) + u128::from(from / 2);
    u64::try_from(t / u128::from(from)).unwrap_or(u64::MAX)
}

/// One trak of the flat moov: the source moov it comes from, its joined samples and where each of
/// its chunks sits in the virtual file.
struct FlatTrak<'a> {
    moov: &'a [u8],
    joined: &'a Joined,
    offsets: Vec<u64>,
}

/// The flat moov: `moov`'s boxes with mvex dropped and its trak replaced by `traks`, numbered 1, 2
/// in order, each with a full sample table, and every duration (mvhd, tkhd, mdhd) set to the
/// virtual file's own length, so a moov that carried the length of one part, or of nothing, never
/// contradicts the samples.
fn flat_moov(
    moov: &[u8],
    traks: &[FlatTrak<'_>],
    movie_scale: u32,
    wide: bool,
) -> Result<Vec<u8>, BmffError> {
    let shown = |j: &Joined| rescale(j.table.presentation().1, j.timescale, movie_scale);
    let movie = traks.iter().map(|t| shown(t.joined)).max().unwrap_or(0);
    let flat = traks
        .iter()
        .zip(1u32..)
        .map(|(t, id)| {
            flat_trak(
                &t.moov[bmff::find(t.moov, &[b"moov", b"trak"])?],
                id,
                t.joined,
                &t.offsets,
                wide,
                shown(t.joined),
            )
        })
        .collect::<Result<Vec<_>, BmffError>>()?
        .concat();
    bmff::map_children(moov, |kind, bytes| {
        Ok(match kind {
            b"mvhd" => {
                let mut mvhd = bytes.to_vec();
                bmff::set_mvhd_next_track(&mut mvhd, traks.len() as u32 + 1)?;
                bmff::set_duration(&mut mvhd, movie)?;
                mvhd
            }
            b"trak" => flat.clone(),
            b"mvex" => Vec::new(),
            _ => bytes.to_vec(),
        })
    })
}

/// A source trak with its id, durations and sample table filled in. An edit list is written when
/// the first sample is presented after time 0 (composition offsets), so playback starts at it; any
/// edit list of the source, written for its empty fragmented layout, is dropped.
fn flat_trak(
    trak: &[u8],
    id: u32,
    j: &Joined,
    offsets: &[u64],
    wide: bool,
    shown: u64,
) -> Result<Vec<u8>, BmffError> {
    let (earliest, _) = j.table.presentation();
    bmff::map_children(trak, |kind, bytes| {
        Ok(match kind {
            b"tkhd" => {
                let mut tkhd = make_tkhd(bytes, id, shown)?;
                if earliest != 0 {
                    tkhd.extend_from_slice(&bmff::edts(shown, earliest));
                }
                tkhd
            }
            b"edts" => Vec::new(),
            b"mdia" => bmff::map_children(bytes, |kind, bytes| {
                Ok(match kind {
                    b"mdhd" => {
                        let mut mdhd = bytes.to_vec();
                        bmff::set_duration(&mut mdhd, j.table.length)?;
                        mdhd
                    }
                    b"minf" => bmff::map_children(bytes, |kind, bytes| {
                        Ok(match kind {
                            b"stbl" => stbl(bytes, &j.table, offsets, wide)?,
                            _ => bytes.to_vec(),
                        })
                    })?,
                    _ => bytes.to_vec(),
                })
            })?,
            _ => bytes.to_vec(),
        })
    })
}

fn make_tkhd(tkhd: &[u8], id: u32, duration: u64) -> Result<Vec<u8>, BmffError> {
    // set_trak_id finds the tkhd inside a trak, so wrap it for the call.
    let mut trak = bmff::make_box(b"trak", tkhd);
    bmff::set_trak_id(&mut trak, id)?;
    let mut tkhd = trak[8..].to_vec();
    bmff::set_duration(&mut tkhd, duration)?;
    Ok(tkhd)
}

/// The source stbl's stsd followed by the sample tables of `table`.
fn stbl(src: &[u8], table: &TrackTable, offsets: &[u64], wide: bool) -> Result<Vec<u8>, BmffError> {
    let stsd = &src[bmff::find(src, &[b"stbl", b"stsd"])?];
    let mut kids = stsd.to_vec();
    kids.extend_from_slice(&bmff::stts(&runs(&table.durations)));
    if table.offsets.iter().any(|o| *o != 0) {
        kids.extend_from_slice(&bmff::ctts(&runs(&table.offsets)));
    }
    if table.sync.len() != table.len() {
        kids.extend_from_slice(&bmff::stss(&table.sync));
    }
    let per_chunk: Vec<u32> = table.chunks.iter().map(|c| c.samples).collect();
    let mut stsc: Vec<(u32, u32)> = Vec::new();
    for (k, n) in per_chunk.iter().enumerate() {
        if stsc.last().is_none_or(|(_, last)| last != n) {
            stsc.push((k as u32 + 1, *n));
        }
    }
    kids.extend_from_slice(&bmff::stsc(&stsc));
    kids.extend_from_slice(&bmff::stsz(&table.sizes));
    kids.extend_from_slice(&bmff::chunk_offsets(offsets, wide));
    Ok(bmff::make_box(b"stbl", &kids))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::play::table::Chunk;

    #[test]
    fn fragments_follow_the_sidx() {
        let sidx = Sidx {
            reference_id: 1,
            timescale: 90,
            earliest_presentation_time: 0,
            first_offset: 16,
            refs: [100, 200]
                .map(|size| bmff::SidxRef {
                    size,
                    duration: 90,
                    sap: 0x9000_0000,
                })
                .to_vec(),
        };
        let idx = TrackIndex::from_sidx(Vec::new(), &sidx, 1000);
        assert_eq!(idx.fragments.len(), 2);
        assert_eq!(idx.fragments[0].offset, 1016);
        assert_eq!(idx.fragments[1].offset, 1116);
        assert_eq!(idx.end(), 1316);

        // Same size and moov, other fragment ranges: another stream.
        let mut other = sidx.clone();
        other.refs.swap(0, 1);
        let other = TrackIndex::from_sidx(Vec::new(), &other, 1000);
        assert_eq!(other.end(), idx.end());
        assert_ne!(other.fingerprint(), idx.fingerprint());
        let moov = TrackIndex::from_sidx(vec![1], &sidx, 1000);
        assert_ne!(moov.fingerprint(), idx.fingerprint());
        assert_eq!(idx.clone().fingerprint(), idx.fingerprint());
    }

    #[test]
    fn parse_asks_for_more_bytes_when_moov_is_cut() {
        let mut head = bmff::make_box(b"ftyp", b"isom");
        head.extend_from_slice(&bmff::make_box(b"moov", &[0u8; 500]));
        head.truncate(100);
        match TrackIndex::parse(&head).unwrap() {
            Parsed::NeedBytes(n) => assert_eq!(n, 12 + 508),
            Parsed::Index(_) => panic!("cannot be complete"),
        }
    }

    #[test]
    fn pieces_cover_the_range_exactly() {
        let placed = |pos, size| Placed {
            pos,
            part: 0,
            track: Track::Video,
            offset: 0,
            size,
        };
        let file = VirtualFile {
            head: Bytes::from_static(&[0u8; 10]),
            chunks: vec![placed(10, 3), placed(13, 5), placed(18, 5)],
            size: 23,
        };
        let chunk = |index, skip, len| Piece::Chunk { index, skip, len };
        assert_eq!(file.pieces(0, 3), vec![Piece::Head(0..4)]);
        assert_eq!(
            file.pieces(8, 14),
            vec![Piece::Head(8..10), chunk(0, 0, 3), chunk(1, 0, 2)]
        );
        assert_eq!(file.pieces(19, 22), vec![chunk(2, 1, 4)]);
        assert_eq!(file.pieces(22, 22), vec![chunk(2, 4, 1)]);
        let total: usize = file
            .pieces(0, 22)
            .iter()
            .map(|p| match p {
                Piece::Head(r) => r.len(),
                Piece::Chunk { len, .. } => *len,
            })
            .sum();
        assert_eq!(total, 23);
    }

    #[test]
    fn stsc_runs_and_sync_samples() {
        let table = TrackTable {
            sizes: vec![1, 2, 3, 4, 5],
            durations: vec![10, 10, 10, 20, 20],
            offsets: vec![0; 5],
            sync: vec![1, 4],
            chunks: vec![
                Chunk {
                    offset: 0,
                    size: 3,
                    samples: 2,
                },
                Chunk {
                    offset: 9,
                    size: 3,
                    samples: 1,
                },
                Chunk {
                    offset: 20,
                    size: 9,
                    samples: 2,
                },
            ],
            length: 70,
        };
        let stsd = bmff::full_box(b"stsd", 0, 0, &0u32.to_be_bytes());
        let src = bmff::make_box(b"stbl", &stsd);
        let out = stbl(&src, &table, &[100, 200, 300], false).unwrap();
        let kinds: Vec<[u8; 4]> = bmff::boxes(&out[8..])
            .unwrap()
            .iter()
            .map(|b| b.kind)
            .collect();
        assert_eq!(
            kinds,
            [*b"stsd", *b"stts", *b"stss", *b"stsc", *b"stsz", *b"stco"]
        );
        let body = |kind: &[u8; 4]| {
            let r = bmff::find(&out, &[b"stbl", kind]).unwrap();
            out[r.start + 12..r.end].to_vec()
        };
        let words = |b: Vec<u8>| -> Vec<u32> {
            b.chunks(4)
                .map(|w| u32::from_be_bytes(w.try_into().unwrap()))
                .collect()
        };
        assert_eq!(words(body(b"stts")), [2, 3, 10, 2, 20]);
        assert_eq!(words(body(b"stss")), [2, 1, 4]);
        assert_eq!(words(body(b"stsc")), [3, 1, 2, 1, 2, 1, 1, 3, 2, 1]);
        assert_eq!(words(body(b"stsz")), [0, 5, 1, 2, 3, 4, 5]);
        assert_eq!(words(body(b"stco")), [3, 100, 200, 300]);
        let wide = stbl(&src, &table, &[1 << 33, 2, 3], true).unwrap();
        let r = bmff::find(&wide, &[b"stbl", b"co64"]).unwrap();
        assert_eq!(
            &wide[r.start + 16..r.start + 24],
            &(1u64 << 33).to_be_bytes()
        );
    }
}
