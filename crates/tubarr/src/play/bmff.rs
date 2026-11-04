//! Just enough ISO-BMFF (MP4) to read YouTube's fragmented files and write the boxes the virtual
//! file needs: walking boxes, the segment index (sidx), the samples a moof lists, track ids,
//! timescales and the sample table boxes of a flat moov.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BmffError {
    #[error("box {kind} at {offset} is cut short")]
    Truncated { kind: String, offset: usize },
    #[error("box at {offset} has an impossible size {size}")]
    BadSize { offset: usize, size: u64 },
    #[error("missing box {0}")]
    Missing(String),
    #[error("unsupported {0}")]
    Unsupported(String),
}

/// One box found while walking a byte slice. `size` is the full box size, which can run past the end
/// of the slice when only the start of a file has been read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoxRef {
    pub kind: [u8; 4],
    pub offset: usize,
    pub header_len: usize,
    pub size: u64,
}

impl BoxRef {
    pub fn end(&self) -> u64 {
        self.offset as u64 + self.size
    }

    /// The whole box, if it lies inside a slice of `len` bytes.
    pub fn range(&self, len: usize) -> Option<Range<usize>> {
        let end = usize::try_from(self.end()).ok()?;
        (end <= len).then_some(self.offset..end)
    }

    pub fn body(&self, len: usize) -> Option<Range<usize>> {
        self.range(len).map(|r| r.start + self.header_len..r.end)
    }
}

fn kind_name(kind: &[u8]) -> String {
    String::from_utf8_lossy(kind).into_owned()
}

fn be_u32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn be_u64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

/// The boxes laid one after another in `data`. Stops at the first header that does not fit; the
/// last box may extend past the end of `data`.
pub fn boxes(data: &[u8]) -> Result<Vec<BoxRef>, BmffError> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 8 <= data.len() {
        let small = u64::from(be_u32(data, at).unwrap_or(0));
        let mut kind = [0u8; 4];
        kind.copy_from_slice(&data[at + 4..at + 8]);
        let (size, header_len) = match small {
            0 => ((data.len() - at) as u64, 8),
            1 => match be_u64(data, at + 8) {
                Some(large) => (large, 16),
                None => break,
            },
            n => (n, 8),
        };
        if size < header_len as u64 {
            return Err(BmffError::BadSize { offset: at, size });
        }
        out.push(BoxRef {
            kind,
            offset: at,
            header_len,
            size,
        });
        match usize::try_from(size).ok().and_then(|s| at.checked_add(s)) {
            Some(next) => at = next,
            None => break,
        }
    }
    Ok(out)
}

/// The first complete box of `kind` among the boxes in `data`.
pub fn child(data: &[u8], kind: &[u8; 4]) -> Result<BoxRef, BmffError> {
    let found = boxes(data)?
        .into_iter()
        .find(|b| &b.kind == kind)
        .ok_or_else(|| BmffError::Missing(kind_name(kind)))?;
    if found.range(data.len()).is_none() {
        return Err(BmffError::Truncated {
            kind: kind_name(kind),
            offset: found.offset,
        });
    }
    Ok(found)
}

/// The byte range (within `data`) of the box at `path`, descending through containers.
pub fn find(data: &[u8], path: &[&[u8; 4]]) -> Result<Range<usize>, BmffError> {
    let mut base = 0usize;
    let mut slice = data;
    let mut range = 0..data.len();
    for kind in path {
        let b = child(slice, kind)?;
        let r = b.range(slice.len()).unwrap_or_default();
        range = base + r.start..base + r.end;
        base += r.start + b.header_len;
        slice = &data[base..range.end];
    }
    Ok(range)
}

/// Builds a box with a 32-bit size.
pub fn make_box(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    out
}

/// One entry of a segment index: a fragment's size, length in timescale ticks and the raw SAP word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidxRef {
    pub size: u32,
    pub duration: u32,
    pub sap: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidx {
    pub reference_id: u32,
    pub timescale: u32,
    pub earliest_presentation_time: u64,
    pub first_offset: u64,
    pub refs: Vec<SidxRef>,
}

impl Sidx {
    /// Parses a complete sidx box (header included), version 0 or 1.
    pub fn parse(bytes: &[u8]) -> Result<Self, BmffError> {
        let b = child(bytes, b"sidx")?;
        let body = &bytes[b.body(bytes.len()).unwrap_or_default()];
        let short = || BmffError::Truncated {
            kind: "sidx".into(),
            offset: b.offset,
        };
        let version = *body.first().ok_or_else(short)?;
        let reference_id = be_u32(body, 4).ok_or_else(short)?;
        let timescale = be_u32(body, 8).ok_or_else(short)?;
        let (ept, first_offset, at) = match version {
            0 => (
                u64::from(be_u32(body, 12).ok_or_else(short)?),
                u64::from(be_u32(body, 16).ok_or_else(short)?),
                20,
            ),
            1 => (
                be_u64(body, 12).ok_or_else(short)?,
                be_u64(body, 20).ok_or_else(short)?,
                28,
            ),
            v => return Err(BmffError::Unsupported(format!("sidx version {v}"))),
        };
        let count = be_u32(body, at).ok_or_else(short)? & 0xffff;
        let mut refs = Vec::with_capacity(count as usize);
        for k in 0..count as usize {
            let e = at + 4 + 12 * k;
            let word = be_u32(body, e).ok_or_else(short)?;
            if word & 0x8000_0000 != 0 {
                return Err(BmffError::Unsupported("hierarchical sidx".into()));
            }
            refs.push(SidxRef {
                size: word,
                duration: be_u32(body, e + 4).ok_or_else(short)?,
                sap: be_u32(body, e + 8).ok_or_else(short)?,
            });
        }
        if timescale == 0 {
            return Err(BmffError::Unsupported("sidx timescale 0".into()));
        }
        Ok(Self {
            reference_id,
            timescale,
            earliest_presentation_time: ept,
            first_offset,
            refs,
        })
    }
}

/// Offset of the version byte of a full box, so callers can find fields after it.
fn full_box_fields(b: &BoxRef) -> usize {
    b.offset + b.header_len
}

/// Timescale from a complete mdhd box.
pub fn mdhd_timescale(mdhd: &[u8]) -> Result<u32, BmffError> {
    let b = child(mdhd, b"mdhd")?;
    let at = full_box_fields(&b);
    let skip = if mdhd[at] == 1 { 16 } else { 8 };
    be_u32(mdhd, at + 4 + skip).ok_or(BmffError::Truncated {
        kind: "mdhd".into(),
        offset: b.offset,
    })
}

/// Timescale from a complete mvhd box.
pub fn mvhd_timescale(mvhd: &[u8]) -> Result<u32, BmffError> {
    let b = child(mvhd, b"mvhd")?;
    let at = full_box_fields(&b);
    let skip = if mvhd[at] == 1 { 16 } else { 8 };
    be_u32(mvhd, at + 4 + skip).ok_or(BmffError::Truncated {
        kind: "mvhd".into(),
        offset: b.offset,
    })
}

/// Writes the duration field of a complete mvhd, tkhd or mdhd box (the first box in
/// `data`). A version 0 field too small for it gets all ones, which means unknown.
pub fn set_duration(data: &mut [u8], duration: u64) -> Result<(), BmffError> {
    let b = boxes(data)?
        .into_iter()
        .next()
        .ok_or_else(|| BmffError::Missing("duration box".into()))?;
    let at = full_box_fields(&b);
    let v1 = *data.get(at).ok_or_else(|| BmffError::Truncated {
        kind: kind_name(&b.kind),
        offset: b.offset,
    })? == 1;
    // Bytes from the version byte to the duration: version and flags, times, track id.
    let skip = match (&b.kind, v1) {
        (b"mvhd" | b"mdhd", false) => 16,
        (b"mvhd" | b"mdhd", true) => 24,
        (b"tkhd", false) => 20,
        (b"tkhd", true) => 28,
        _ => {
            return Err(BmffError::Unsupported(format!(
                "duration in {}",
                kind_name(&b.kind)
            )));
        }
    };
    let field = at + skip;
    if v1 {
        data.get_mut(field..field + 8)
            .ok_or_else(|| BmffError::Truncated {
                kind: kind_name(&b.kind),
                offset: b.offset,
            })?
            .copy_from_slice(&duration.to_be_bytes());
        Ok(())
    } else {
        let value = u32::try_from(duration).unwrap_or(u32::MAX);
        put_u32(data, field, value, "duration")
    }
}

fn put_u32(data: &mut [u8], at: usize, value: u32, kind: &str) -> Result<(), BmffError> {
    data.get_mut(at..at + 4)
        .ok_or_else(|| BmffError::Truncated {
            kind: kind.into(),
            offset: at,
        })?
        .copy_from_slice(&value.to_be_bytes());
    Ok(())
}

/// Sets track_ID in a trak box (in its tkhd).
pub fn set_trak_id(trak: &mut [u8], id: u32) -> Result<(), BmffError> {
    let r = find(trak, &[b"trak", b"tkhd"])?;
    let at = r.start + 8;
    let skip = if trak[at] == 1 { 16 } else { 8 };
    put_u32(trak, at + 4 + skip, id, "tkhd")
}

/// Sets next_track_ID, the last field of mvhd.
pub fn set_mvhd_next_track(mvhd: &mut [u8], id: u32) -> Result<(), BmffError> {
    let b = child(mvhd, b"mvhd")?;
    let end = b.range(mvhd.len()).unwrap_or_default().end;
    put_u32(mvhd, end - 4, id, "mvhd")
}

/// A container box rebuilt from its children: `edit` gets each child's kind and bytes and returns
/// what takes its place (empty drops it). `container` is the complete box.
pub fn map_children(
    container: &[u8],
    mut edit: impl FnMut(&[u8; 4], &[u8]) -> Result<Vec<u8>, BmffError>,
) -> Result<Vec<u8>, BmffError> {
    let b = boxes(container)?
        .into_iter()
        .next()
        .ok_or_else(|| BmffError::Missing("container".into()))?;
    let short = || BmffError::Truncated {
        kind: kind_name(&b.kind),
        offset: b.offset,
    };
    let body = &container[b.body(container.len()).ok_or_else(short)?];
    let mut out = Vec::with_capacity(body.len());
    for c in boxes(body)? {
        let bytes = &body[c.range(body.len()).ok_or_else(short)?];
        out.extend_from_slice(&edit(&c.kind, bytes)?);
    }
    Ok(make_box(&b.kind, &out))
}

/// Builds a full box: version, flags, then `payload`.
pub fn full_box(kind: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + payload.len());
    p.push(version);
    p.extend_from_slice(&flags.to_be_bytes()[1..]);
    p.extend_from_slice(payload);
    make_box(kind, &p)
}

/// What a track's fragments fall back on when tfhd and trun leave a field out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SampleDefaults {
    pub duration: u32,
    pub size: u32,
    pub flags: u32,
}

/// The defaults in the trex of a moov holding one track.
pub fn trex_defaults(moov: &[u8]) -> Result<SampleDefaults, BmffError> {
    let r = find(moov, &[b"moov", b"mvex", b"trex"])?;
    let field = |k: usize| {
        be_u32(moov, r.start + 16 + 4 * k).ok_or(BmffError::Truncated {
            kind: "trex".into(),
            offset: r.start,
        })
    };
    Ok(SampleDefaults {
        duration: field(1)?,
        size: field(2)?,
        flags: field(3)?,
    })
}

/// One sample as a trun describes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FragmentSample {
    pub duration: u32,
    pub size: u32,
    pub sync: bool,
    /// Composition time offset: presentation time minus decode time.
    pub offset: i32,
}

/// The samples of one trun and where their data starts in the source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampleRun {
    /// The tfdt decode time of the first sample, for the first run of a traf that has one.
    pub decode_time: Option<u64>,
    pub data_offset: u64,
    pub samples: Vec<FragmentSample>,
}

/// sample_is_non_sync_sample in the sample flags.
const NON_SYNC: u32 = 0x0001_0000;

/// Reads every traf and trun of a complete moof that starts at `moof_offset` in its source file.
/// Data offsets count from the moof (or tfhd's base_data_offset), as YouTube and ffmpeg write them.
pub fn parse_moof(
    moof: &[u8],
    moof_offset: u64,
    defaults: SampleDefaults,
) -> Result<Vec<SampleRun>, BmffError> {
    let m = child(moof, b"moof")?;
    let body_range = m.body(moof.len()).unwrap_or_default();
    let body = &moof[body_range];
    let mut out = Vec::new();
    for traf in boxes(body)?.into_iter().filter(|b| &b.kind == b"traf") {
        let short = |kind: &str| BmffError::Truncated {
            kind: kind.into(),
            offset: traf.offset,
        };
        let traf = &body[traf.body(body.len()).ok_or_else(|| short("traf"))?];
        let tfhd = &traf[child(traf, b"tfhd")?.body(traf.len()).unwrap_or_default()];
        let flags = be_u32(tfhd, 0).ok_or_else(|| short("tfhd"))? & 0x00ff_ffff;
        let mut at = 8;
        let mut take = |present: bool, wide: bool| -> Result<Option<u64>, BmffError> {
            if !present {
                return Ok(None);
            }
            let v = if wide {
                be_u64(tfhd, at)
            } else {
                be_u32(tfhd, at).map(u64::from)
            };
            at += if wide { 8 } else { 4 };
            v.map(Some).ok_or_else(|| short("tfhd"))
        };
        let base = take(flags & 0x01 != 0, true)?.unwrap_or(moof_offset);
        take(flags & 0x02 != 0, false)?;
        let narrow = |v: Option<u64>, d: u32| v.map_or(d, |v| v as u32);
        let duration = narrow(take(flags & 0x08 != 0, false)?, defaults.duration);
        let size = narrow(take(flags & 0x10 != 0, false)?, defaults.size);
        let sample_flags = narrow(take(flags & 0x20 != 0, false)?, defaults.flags);

        let mut decode_time = match boxes(traf)?.into_iter().find(|b| &b.kind == b"tfdt") {
            Some(t) => {
                let t = &traf[t.body(traf.len()).ok_or_else(|| short("tfdt"))?];
                Some(match t.first() {
                    Some(1) => be_u64(t, 4),
                    _ => be_u32(t, 4).map(u64::from),
                })
                .map(|v| v.ok_or_else(|| short("tfdt")))
                .transpose()?
            }
            None => None,
        };
        let mut next_data = base;
        for trun in boxes(traf)?.into_iter().filter(|b| &b.kind == b"trun") {
            let t = &traf[trun.body(traf.len()).ok_or_else(|| short("trun"))?];
            let word = |at: usize| be_u32(t, at).ok_or_else(|| short("trun"));
            let version = t.first().copied().unwrap_or(0);
            let tflags = word(0)? & 0x00ff_ffff;
            let count = word(4)? as usize;
            let mut at = 8;
            let mut data_offset = next_data;
            if tflags & 0x001 != 0 {
                data_offset = base
                    .checked_add_signed(i64::from(word(at)? as i32))
                    .ok_or_else(|| BmffError::Unsupported("trun data offset".into()))?;
                at += 4;
            }
            let mut first_flags = None;
            if tflags & 0x004 != 0 {
                first_flags = Some(word(at)?);
                at += 4;
            }
            let mut samples = Vec::with_capacity(count);
            for k in 0..count {
                let mut field = |bit: u32| -> Result<Option<u32>, BmffError> {
                    if tflags & bit == 0 {
                        return Ok(None);
                    }
                    let v = word(at)?;
                    at += 4;
                    Ok(Some(v))
                };
                let d = field(0x100)?.unwrap_or(duration);
                let s = field(0x200)?.unwrap_or(size);
                let f = field(0x400)?;
                let f = match (k, first_flags) {
                    (0, Some(first)) => first,
                    _ => f.unwrap_or(sample_flags),
                };
                let offset = match field(0x800)? {
                    None => 0,
                    Some(o) if version == 1 => o as i32,
                    Some(o) => i32::try_from(o).map_err(|_| {
                        BmffError::Unsupported("composition offset over 2^31".into())
                    })?,
                };
                samples.push(FragmentSample {
                    duration: d,
                    size: s,
                    sync: f & NON_SYNC == 0,
                    offset,
                });
            }
            next_data = data_offset + samples.iter().map(|s| u64::from(s.size)).sum::<u64>();
            out.push(SampleRun {
                decode_time: decode_time.take(),
                data_offset,
                samples,
            });
        }
    }
    Ok(out)
}

/// An stts box from (sample count, duration) runs.
pub fn stts(runs: &[(u32, u32)]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + 8 * runs.len());
    p.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (count, delta) in runs {
        p.extend_from_slice(&count.to_be_bytes());
        p.extend_from_slice(&delta.to_be_bytes());
    }
    full_box(b"stts", 0, 0, &p)
}

/// A version 1 ctts box (signed offsets) from (sample count, offset) runs.
pub fn ctts(runs: &[(u32, i32)]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + 8 * runs.len());
    p.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (count, offset) in runs {
        p.extend_from_slice(&count.to_be_bytes());
        p.extend_from_slice(&offset.to_be_bytes());
    }
    full_box(b"ctts", 1, 0, &p)
}

/// An stss box listing the sync samples, numbered from 1.
pub fn stss(numbers: &[u32]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + 4 * numbers.len());
    p.extend_from_slice(&(numbers.len() as u32).to_be_bytes());
    for n in numbers {
        p.extend_from_slice(&n.to_be_bytes());
    }
    full_box(b"stss", 0, 0, &p)
}

/// An stsc box from (first chunk, samples per chunk) runs, all with sample description 1.
pub fn stsc(runs: &[(u32, u32)]) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + 12 * runs.len());
    p.extend_from_slice(&(runs.len() as u32).to_be_bytes());
    for (first, per_chunk) in runs {
        p.extend_from_slice(&first.to_be_bytes());
        p.extend_from_slice(&per_chunk.to_be_bytes());
        p.extend_from_slice(&1u32.to_be_bytes());
    }
    full_box(b"stsc", 0, 0, &p)
}

/// An stsz box. Samples that all have one size are written as that size alone.
pub fn stsz(sizes: &[u32]) -> Vec<u8> {
    let same = sizes.first().filter(|s| sizes.iter().all(|x| x == *s));
    let mut p = Vec::with_capacity(8 + 4 * sizes.len());
    p.extend_from_slice(&same.copied().unwrap_or(0).to_be_bytes());
    p.extend_from_slice(&(sizes.len() as u32).to_be_bytes());
    if same.is_none() {
        for s in sizes {
            p.extend_from_slice(&s.to_be_bytes());
        }
    }
    full_box(b"stsz", 0, 0, &p)
}

/// An stco box, or co64 when `wide`. Offsets must fit when not wide.
pub fn chunk_offsets(offsets: &[u64], wide: bool) -> Vec<u8> {
    let mut p = Vec::with_capacity(4 + if wide { 8 } else { 4 } * offsets.len());
    p.extend_from_slice(&(offsets.len() as u32).to_be_bytes());
    for o in offsets {
        if wide {
            p.extend_from_slice(&o.to_be_bytes());
        } else {
            p.extend_from_slice(&(*o as u32).to_be_bytes());
        }
    }
    full_box(if wide { b"co64" } else { b"stco" }, 0, 0, &p)
}

/// An edts with one version 1 elst entry: play `duration` movie ticks from media time `start`.
pub fn edts(duration: u64, start: i64) -> Vec<u8> {
    let mut p = Vec::with_capacity(24);
    p.extend_from_slice(&1u32.to_be_bytes());
    p.extend_from_slice(&duration.to_be_bytes());
    p.extend_from_slice(&start.to_be_bytes());
    p.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    make_box(b"edts", &full_box(b"elst", 1, 0, &p))
}

/// The header of an mdat holding `payload` bytes: 8 bytes, or 16 with a 64-bit size.
pub fn mdat_header(payload: u64) -> Vec<u8> {
    match u32::try_from(payload + 8) {
        Ok(size) => {
            let mut h = size.to_be_bytes().to_vec();
            h.extend_from_slice(b"mdat");
            h
        }
        Err(_) => {
            let mut h = 1u32.to_be_bytes().to_vec();
            h.extend_from_slice(b"mdat");
            h.extend_from_slice(&(payload + 16).to_be_bytes());
            h
        }
    }
}

/// What a decoder needs to know about a track's samples, from the moov holding it: per sample
/// entry its format, the fields that matter (picture size, or channels and sample rate) and its
/// decoder configuration boxes. Bitrate boxes and the compressor name are left out because they
/// differ between files that decode the same way.
pub fn sample_signature(moov: &[u8]) -> Result<Vec<u8>, BmffError> {
    let stsd = find(
        moov,
        &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd"],
    )?;
    // Full box header, then the entry count.
    let entries = &moov[stsd.start + 16..stsd.end];
    let mut sig = Vec::new();
    for entry in boxes(entries)? {
        let Some(r) = entry.body(entries.len()) else {
            return Err(BmffError::Truncated {
                kind: "sample entry".into(),
                offset: entry.offset,
            });
        };
        let body = &entries[r];
        sig.extend_from_slice(&entry.kind);
        let (fields, kids) = match &entry.kind {
            b"avc1" | b"avc3" | b"hvc1" | b"hev1" | b"vp09" | b"av01" => (24..28, 78),
            b"mp4a" | b"Opus" | b"ac-3" | b"ec-3" => (16..28, 28),
            _ => {
                sig.extend_from_slice(body);
                continue;
            }
        };
        let short = || BmffError::Truncated {
            kind: kind_name(&entry.kind),
            offset: entry.offset,
        };
        sig.extend_from_slice(body.get(fields).ok_or_else(short)?);
        let kids = body.get(kids..).ok_or_else(short)?;
        for kid in boxes(kids)? {
            let bytes = &kids[kid.range(kids.len()).ok_or_else(short)?];
            match &kid.kind {
                b"btrt" => {}
                b"esds" => {
                    sig.extend_from_slice(b"esds");
                    sig.extend_from_slice(&esds_signature(&bytes[kid.header_len..])?);
                }
                _ => sig.extend_from_slice(bytes),
            }
        }
    }
    Ok(sig)
}

/// One MPEG-4 descriptor at the start of `data`: its tag and payload.
fn descriptor(data: &[u8]) -> Option<(u8, &[u8])> {
    let tag = *data.first()?;
    let mut len = 0usize;
    for i in 1..=4 {
        let b = *data.get(i)?;
        len = (len << 7) | usize::from(b & 0x7f);
        if b & 0x80 == 0 {
            return Some((tag, data.get(i + 1..i + 1 + len)?));
        }
    }
    None
}

/// The object type and decoder specific info of an esds body (version and flags first), without
/// the buffer size and bitrates.
fn esds_signature(esds: &[u8]) -> Result<Vec<u8>, BmffError> {
    let bad = || BmffError::Unsupported("esds layout".into());
    let (tag, es) = descriptor(esds.get(4..).ok_or_else(bad)?).ok_or_else(bad)?;
    if tag != 3 {
        return Err(bad());
    }
    let flags = *es.get(2).ok_or_else(bad)?;
    let mut at = 3;
    if flags & 0x80 != 0 {
        at += 2;
    }
    if flags & 0x40 != 0 {
        at += 1 + usize::from(*es.get(at).ok_or_else(bad)?);
    }
    if flags & 0x20 != 0 {
        at += 2;
    }
    let (tag, config) = descriptor(es.get(at..).ok_or_else(bad)?).ok_or_else(bad)?;
    if tag != 4 {
        return Err(bad());
    }
    let mut sig = vec![*config.first().ok_or_else(bad)?];
    if let Some((5, info)) = config.get(13..).and_then(descriptor) {
        sig.extend_from_slice(info);
    }
    Ok(sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full(kind: &[u8; 4], version: u8, rest: &[u8]) -> Vec<u8> {
        let mut p = vec![version, 0, 0, 0];
        p.extend_from_slice(rest);
        make_box(kind, &p)
    }

    #[test]
    fn walks_boxes_including_large_size() {
        let mut data = make_box(b"ftyp", b"isom");
        let mut large = Vec::new();
        large.extend_from_slice(&1u32.to_be_bytes());
        large.extend_from_slice(b"mdat");
        large.extend_from_slice(&20u64.to_be_bytes());
        large.extend_from_slice(b"abcd");
        data.extend_from_slice(&large);
        let found = boxes(&data).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(&found[1].kind, b"mdat");
        assert_eq!(found[1].header_len, 16);
        assert_eq!(found[1].body(data.len()), Some(28..32));
    }

    #[test]
    fn partial_box_is_listed_but_not_found() {
        let mut data = make_box(b"moov", &[0u8; 100]);
        data.truncate(50);
        assert_eq!(boxes(&data).unwrap()[0].size, 108);
        assert!(matches!(
            child(&data, b"moov"),
            Err(BmffError::Truncated { .. })
        ));
        assert!(matches!(child(&data, b"sidx"), Err(BmffError::Missing(_))));
    }

    #[test]
    fn bad_size_is_an_error() {
        let mut data = 4u32.to_be_bytes().to_vec();
        data.extend_from_slice(b"free");
        assert!(matches!(boxes(&data), Err(BmffError::BadSize { .. })));
    }

    #[test]
    fn sidx_v0_parses() {
        let mut rest = Vec::new();
        rest.extend_from_slice(&7u32.to_be_bytes()); // reference id
        rest.extend_from_slice(&90000u32.to_be_bytes());
        rest.extend_from_slice(&10u32.to_be_bytes()); // ept
        rest.extend_from_slice(&5u32.to_be_bytes()); // first offset
        rest.extend_from_slice(&[0, 0, 0, 2]);
        for (size, dur) in [(1000u32, 450000u32), (2000, 449000)] {
            rest.extend_from_slice(&size.to_be_bytes());
            rest.extend_from_slice(&dur.to_be_bytes());
            rest.extend_from_slice(&0x9000_0000u32.to_be_bytes());
        }
        let v0 = full(b"sidx", 0, &rest);
        let sidx = Sidx::parse(&v0).unwrap();
        assert_eq!(sidx.reference_id, 7);
        assert_eq!(sidx.timescale, 90000);
        assert_eq!(sidx.earliest_presentation_time, 10);
        assert_eq!(sidx.first_offset, 5);
        assert_eq!(sidx.refs[1].size, 2000);
        assert_eq!(sidx.refs[1].duration, 449000);
    }

    #[test]
    fn hierarchical_sidx_is_refused() {
        let mut rest = vec![0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        rest.extend_from_slice(&0x8000_0010u32.to_be_bytes());
        rest.extend_from_slice(&[0; 8]);
        assert!(matches!(
            Sidx::parse(&full(b"sidx", 0, &rest)),
            Err(BmffError::Unsupported(_))
        ));
    }

    #[test]
    fn track_fields_are_rewritten() {
        // tkhd v1: 8 bytes creation, 8 modification, then track id.
        let mut tkhd_rest = vec![0u8; 16];
        tkhd_rest.extend_from_slice(&1u32.to_be_bytes());
        tkhd_rest.extend_from_slice(&[0u8; 60]);
        let mut trak = make_box(b"trak", &full(b"tkhd", 1, &tkhd_rest));
        set_trak_id(&mut trak, 2).unwrap();
        assert_eq!(be_u32(&trak, 8 + 12 + 16), Some(2));

        let mut mvhd = full(b"mvhd", 0, &[0u8; 96]);
        set_mvhd_next_track(&mut mvhd, 3).unwrap();
        assert_eq!(be_u32(&mvhd, mvhd.len() - 4), Some(3));

        let mut mdhd_rest = vec![0u8; 8];
        mdhd_rest.extend_from_slice(&44100u32.to_be_bytes());
        mdhd_rest.extend_from_slice(&[0u8; 8]);
        assert_eq!(
            mdhd_timescale(&full(b"mdhd", 0, &mdhd_rest)).unwrap(),
            44100
        );
    }

    #[test]
    fn moofs_list_their_samples() {
        // ffmpeg's B-frame fixture: 20 samples per moof, sizes, flags and composition offsets in
        // the trun, duration from tfhd, data right after the moof's mdat header.
        let file = include_bytes!("../../tests/fixtures/play/video-137-bframes.mp4");
        let defaults = trex_defaults(moov_of(file)).unwrap();
        assert_eq!(defaults, SampleDefaults::default());
        let moof = find(file, &[b"moof"]).unwrap();
        let runs = parse_moof(&file[moof.clone()], moof.start as u64, defaults).unwrap();
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(run.decode_time, Some(0));
        assert_eq!(run.data_offset, moof.end as u64 + 8);
        assert_eq!(run.samples.len(), 20);
        let s = run.samples[0];
        assert_eq!((s.duration, s.offset), (1024, 2048));
        assert!(s.sync && !run.samples[1].sync);
        let mdat = boxes(&file[moof.end..]).unwrap()[0];
        let total: u64 = run.samples.iter().map(|s| u64::from(s.size)).sum();
        assert_eq!(total, mdat.size - 8, "the samples fill the mdat");

        // tfhd with an explicit base offset, defaults from trex, a trun without a data offset.
        let mut tfhd = 1u32.to_be_bytes().to_vec();
        tfhd.extend_from_slice(&5000u64.to_be_bytes());
        let mut trun = 2u32.to_be_bytes().to_vec();
        trun.extend_from_slice(&(-7i32).to_be_bytes());
        trun.extend_from_slice(&3i32.to_be_bytes());
        let mut traf = full_box(b"tfhd", 0, 0x01, &tfhd);
        traf.extend_from_slice(&full_box(b"tfdt", 0, 0, &9u32.to_be_bytes()));
        traf.extend_from_slice(&full_box(b"trun", 1, 0x800, &trun));
        let moof = make_box(b"moof", &make_box(b"traf", &traf));
        let defaults = SampleDefaults {
            duration: 512,
            size: 40,
            flags: NON_SYNC,
        };
        let runs = parse_moof(&moof, 100, defaults).unwrap();
        assert_eq!(runs[0].decode_time, Some(9));
        assert_eq!(runs[0].data_offset, 5000);
        assert_eq!(
            runs[0].samples,
            [-7, 3].map(|offset| FragmentSample {
                duration: 512,
                size: 40,
                sync: false,
                offset
            })
        );
    }

    #[test]
    fn mdat_headers_grow_past_4_gib() {
        assert_eq!(mdat_header(10), [0, 0, 0, 18, b'm', b'd', b'a', b't']);
        let wide = mdat_header(u64::from(u32::MAX));
        assert_eq!(wide.len(), 16);
        assert_eq!(be_u32(&wide, 0), Some(1));
        assert_eq!(be_u64(&wide, 8), Some(u64::from(u32::MAX) + 16));
        let e = edts(6000, 2048);
        let elst = find(&e, &[b"edts", b"elst"]).unwrap();
        assert_eq!(be_u64(&e, elst.start + 16), Some(6000));
        assert_eq!(be_u64(&e, elst.start + 24), Some(2048));
    }

    fn moov_of(file: &[u8]) -> &[u8] {
        &file[find(file, &[b"moov"]).unwrap()]
    }

    #[test]
    fn sample_signature_ignores_bitrates_but_not_picture_size() {
        let v1 = include_bytes!("../../tests/fixtures/play/video-137.mp4");
        let v2 = include_bytes!("../../tests/fixtures/play/video-137-part2.mp4");
        let a1 = include_bytes!("../../tests/fixtures/play/audio-140.mp4");
        let a2 = include_bytes!("../../tests/fixtures/play/audio-140-part2.mp4");
        let stsd = |f: &[u8]| {
            let m = moov_of(f);
            m[find(m, &[b"moov", b"trak", b"mdia", b"minf", b"stbl", b"stsd"]).unwrap()].to_vec()
        };
        // The remux adds a btrt box, drops the compressor name and changes the esds bitrates.
        assert_ne!(stsd(v1), stsd(v2));
        assert_ne!(stsd(a1), stsd(a2));
        let sig = |f: &[u8]| sample_signature(moov_of(f)).unwrap();
        assert_eq!(sig(v1), sig(v2));
        assert_eq!(sig(a1), sig(a2));
        assert_ne!(sig(v1), sig(a1));

        let mut wide = v2.to_vec();
        let at = wide.windows(4).position(|w| w == b"avc1").unwrap();
        wide[at + 28..at + 30].copy_from_slice(&80u16.to_be_bytes());
        assert_ne!(sig(v1), sig(&wide));
    }

    #[test]
    fn durations_land_in_the_right_field() {
        let mut mvhd0 = full(b"mvhd", 0, &[0u8; 96]);
        mvhd0[20..24].copy_from_slice(&1000u32.to_be_bytes());
        set_duration(&mut mvhd0, 6128).unwrap();
        assert_eq!(mvhd_timescale(&mvhd0).unwrap(), 1000);
        assert_eq!(be_u32(&mvhd0, 24), Some(6128));
        set_duration(&mut mvhd0, u64::from(u32::MAX) + 1).unwrap();
        assert_eq!(
            be_u32(&mvhd0, 24),
            Some(u32::MAX),
            "too long for v0 means unknown"
        );

        let mut mdhd1 = full(b"mdhd", 1, &[0u8; 32]);
        mdhd1[28..32].copy_from_slice(&90000u32.to_be_bytes());
        set_duration(&mut mdhd1, 1 << 40).unwrap();
        assert_eq!(mdhd_timescale(&mdhd1).unwrap(), 90000);
        assert_eq!(be_u64(&mdhd1, 32), Some(1 << 40));

        let mut tkhd0 = full(b"tkhd", 0, &[0u8; 80]);
        tkhd0[20..24].copy_from_slice(&1u32.to_be_bytes());
        set_duration(&mut tkhd0, 77).unwrap();
        assert_eq!(be_u32(&tkhd0, 20), Some(1), "track id untouched");
        assert_eq!(be_u32(&tkhd0, 28), Some(77));

        let mut tkhd1 = full(b"tkhd", 1, &[0u8; 92]);
        set_duration(&mut tkhd1, 78).unwrap();
        assert_eq!(be_u64(&tkhd1, 36), Some(78));

        assert!(set_duration(&mut full(b"tfdt", 0, &[0u8; 4]), 1).is_err());
    }
}
