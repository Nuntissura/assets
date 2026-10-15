//! Canon CR3 container decode: ISO base media file boxes (ISO/IEC 14496-12) -> the full-size raw
//! track -> its `CMP1` coding descriptor and first sample -> `crx` decode of the whole stored sensor
//! mosaic (masked borders included).
//!
//! Adapted from LightCraft `crates/meta/src/cr3.rs` (box walk, track/sample-entry/CMP1 reading) and
//! `crates/raw/src/vendor/cr3.rs` (main-track choice, CFA codes) (https://github.com/storytold/lightcraft
//! at af4c5e102c9f97529f582003b99c21b89067e906; MIT OR Apache-2.0; attribution in this crate's
//! `NOTICE`). LightCraft states its CR3 layout facts come from Laurent Clevy's public prose CR3
//! notes and black-box inspection; no copyleft decoder source was read for this module.
//!
//! Layout used: `ftyp` major brand `crx `; `moov` holds a Canon `uuid` box
//! (85c0b687-820f-11e0-8111-f4ce462b6a48) whose `CMT1` child is a TIFF stream with IFD0 (Model), then
//! one `trak` per stream; a raw track's `stsd` entry is `CRAW` (VisualSampleEntry + 4 bytes, then
//! child boxes incl. `CMP1`); its first sample's location comes from `stsz` and `co64`/`stco`. The
//! main raw track is the first largest `CRAW` entry with a `CMP1` (a later equal-size track can be
//! a Dual Pixel delta). Levels: Canon ColorData (maker note 0x4001 in the `CTMD` timed metadata,
//! else `CMT3`) black and white by ColorData version. Not read (named in `unapplied`): the `IAD1`
//! active area and crop.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::crx::{self, Coding};
use crate::dng::{DngRaw, VendorFrame};
use crate::levels;
use crate::pipeline::{Bayer, CfaPattern};
use crate::tiffep::{self, Tiff};

pub const UNAPPLIED: [&str; 3] = ["cr3_black_level_unread", "cr3_white_level_unread", "cr3_active_area_unread"];

const CANON_UUID: [u8; 16] = [0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48];
const MAX_DEPTH: usize = 12;
const MAX_BOXES: usize = 4096;
/// A `VisualSampleEntry` (78 bytes after the box header) plus 4 bytes Canon adds before children.
const CRAW_CHILDREN_AT: usize = 82;

pub fn is_cr3(bytes: &[u8]) -> bool {
    bytes.get(4..12) == Some(&b"ftypcrx "[..])
}

struct BoxRef {
    kind: [u8; 4],
    /// Start of the payload (after the header and, for `uuid`, the 16-byte uuid).
    body: usize,
    end: usize,
    uuid: Option<[u8; 16]>,
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn be64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// Boxes directly inside `[from, to)`; stops at the first malformed header. `budget` bounds the
/// total number of boxes visited over the whole file.
fn boxes(bytes: &[u8], from: usize, to: usize, budget: &mut usize) -> Vec<BoxRef> {
    let mut out = Vec::new();
    let mut at = from;
    let to = to.min(bytes.len());
    while at.checked_add(8).is_some_and(|e| e <= to) && *budget > 0 {
        *budget = budget.saturating_sub(1);
        let (Some(size), Some(kind)) = (be32(bytes, at), bytes.get(at.saturating_add(4)..at.saturating_add(8))) else { break };
        let Ok(kind4) = <[u8; 4]>::try_from(kind) else { break };
        let (len, mut hdr) = match size {
            0 => (to.saturating_sub(at), 8usize),
            1 => match be64(bytes, at.saturating_add(8)).and_then(|v| usize::try_from(v).ok()) {
                Some(l) => (l, 16),
                None => break,
            },
            n => (n as usize, 8),
        };
        let Some(end) = at.checked_add(len).filter(|&e| e <= to && len >= hdr) else { break };
        let mut uuid = None;
        if &kind4 == b"uuid" {
            let start = at.saturating_add(hdr);
            let Some(u) = start.checked_add(16).filter(|&e| e <= end).and_then(|e| bytes.get(start..e)) else { break };
            uuid = <[u8; 16]>::try_from(u).ok();
            hdr = hdr.saturating_add(16);
        }
        out.push(BoxRef { kind: kind4, body: at.saturating_add(hdr), end, uuid });
        at = end;
    }
    out
}

/// One raw track: displayed size, CMP1 payload range and its first sample range.
struct RawTrack {
    area: u64,
    cmp1: (usize, usize),
    sample: Option<(usize, usize)>,
}

/// `CRAW` sample entry: width, height and the CMP1 payload range (offset, length) when present.
type CrawEntry = (u16, u16, Option<(usize, usize)>);

fn craw_entry(bytes: &[u8], e: &BoxRef, budget: &mut usize) -> Option<CrawEntry> {
    if &e.kind != b"CRAW" {
        return None;
    }
    let body = bytes.get(e.body..e.end)?;
    let (width, height) = (be16(body, 24)?, be16(body, 26)?);
    let mut cmp1 = None;
    for c in boxes(bytes, e.body.saturating_add(CRAW_CHILDREN_AT), e.end, budget) {
        if &c.kind == b"CMP1" {
            cmp1 = Some((c.body, c.end.saturating_sub(c.body)));
        }
    }
    Some((width, height, cmp1))
}

/// Descend `trak` -> `mdia` -> `minf` -> `stbl`; read the sample entry and first sample location.
/// A track this decoder reads: a raw `CRAW` track, or the `CTMD` timed-metadata track's first sample.
enum Track {
    Raw(RawTrack),
    Ctmd(Option<(usize, usize)>),
}

fn trak(bytes: &[u8], t: &BoxRef, budget: &mut usize) -> Option<Track> {
    let mut stbl = None;
    let mut stack = vec![(t.body, t.end, 0usize)];
    while let Some((from, to, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            continue;
        }
        for b in boxes(bytes, from, to, budget) {
            match &b.kind {
                b"mdia" | b"minf" => stack.push((b.body, b.end, depth.saturating_add(1))),
                b"stbl" => stbl = Some(b),
                _ => {}
            }
        }
    }
    let stbl = stbl?;
    let (mut entry, mut ctmd, mut size, mut offset) = (None, false, None, None);
    for b in boxes(bytes, stbl.body, stbl.end, budget) {
        let body = bytes.get(b.body..b.end)?;
        match &b.kind {
            // full box: version/flags, entry count, entries
            b"stsd" if be32(body, 4)? > 0 => {
                let first = boxes(bytes, b.body.saturating_add(8), b.end, budget);
                ctmd = first.first().is_some_and(|e| &e.kind == b"CTMD");
                entry = first.first().and_then(|e| craw_entry(bytes, e, budget));
            }
            // version/flags, sample_size (0 = per-sample table), count, sizes
            b"stsz" => {
                size = match be32(body, 4)? {
                    0 if be32(body, 8)? > 0 => be32(body, 12),
                    0 => None,
                    n if be32(body, 8)? > 0 => Some(n),
                    _ => None,
                }
            }
            b"co64" if be32(body, 4)? > 0 => offset = be64(body, 8).and_then(|v| usize::try_from(v).ok()),
            b"stco" if be32(body, 4)? > 0 => offset = be32(body, 8).map(|v| v as usize),
            _ => {}
        }
    }
    let sample = match (offset, size) {
        (Some(o), Some(s)) if o.checked_add(s as usize).is_some_and(|e| e <= bytes.len()) => Some((o, s as usize)),
        _ => None,
    };
    if ctmd {
        return Some(Track::Ctmd(sample));
    }
    let (width, height, cmp1) = entry?;
    Some(Track::Raw(RawTrack { area: u64::from(width).saturating_mul(u64::from(height)), cmp1: cmp1?, sample }))
}

/// The parts of the file this decoder reads.
#[derive(Default)]
struct Parts<'a> {
    tracks: Vec<RawTrack>,
    /// `CMT1` (TIFF stream: IFD0) and `CMT3` (TIFF stream: the Canon maker note).
    cmt1: Option<&'a [u8]>,
    cmt3: Option<&'a [u8]>,
    /// First sample of the `CTMD` timed-metadata track.
    ctmd: Option<&'a [u8]>,
}

fn parse(bytes: &[u8]) -> Parts<'_> {
    let mut budget = MAX_BOXES;
    let mut parts = Parts::default();
    for top in boxes(bytes, 0, bytes.len(), &mut budget) {
        if &top.kind != b"moov" {
            continue;
        }
        for b in boxes(bytes, top.body, top.end, &mut budget) {
            match &b.kind {
                b"uuid" if b.uuid == Some(CANON_UUID) => {
                    for c in boxes(bytes, b.body, b.end, &mut budget) {
                        match &c.kind {
                            b"CMT1" => parts.cmt1 = bytes.get(c.body..c.end),
                            b"CMT3" => parts.cmt3 = bytes.get(c.body..c.end),
                            _ => {}
                        }
                    }
                }
                b"trak" => match trak(bytes, &b, &mut budget) {
                    Some(Track::Raw(t)) => parts.tracks.push(t),
                    Some(Track::Ctmd(Some((at, len)))) if parts.ctmd.is_none() => {
                        parts.ctmd = at.checked_add(len).and_then(|end| bytes.get(at..end));
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
    parts
}

fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

/// Canon maker-note TIFF blocks in the CTMD sample: records (le32 length >= 12, le16 type at 4,
/// items from byte 12) of types 7..9 hold items (le32 length >= 8, le32 tag, data) whose tag 0x927C
/// data is a maker-note TIFF with block-relative offsets (LightCraft `timed_exif`).
fn timed_maker_notes(sample: &[u8]) -> Vec<&[u8]> {
    let (mut out, mut budget, mut at) = (Vec::new(), MAX_BOXES, 0usize);
    while budget > 0 {
        budget = budget.saturating_sub(1);
        let Some(len) = le32(sample, at).map(|v| v as usize) else { break };
        let Some(end) = at.checked_add(len).filter(|&e| len >= 12 && e <= sample.len()) else { break };
        let Some(record) = sample.get(at..end) else { break };
        if matches!(le16(record, 4), Some(7..=9)) {
            let mut item_at = 12usize;
            while budget > 0 {
                budget = budget.saturating_sub(1);
                let Some(item_len) = le32(record, item_at).map(|v| v as usize) else { break };
                let Some(item_end) = item_at.checked_add(item_len).filter(|&e| item_len >= 8 && e <= record.len()) else { break };
                if le32(record, item_at.saturating_add(4)) == Some(0x927c)
                    && let Some(data) = record.get(item_at.saturating_add(8)..item_end)
                {
                    out.push(data);
                }
                item_at = item_end;
            }
        }
        at = end;
    }
    out
}

/// Canon ColorData (maker note 0x4001) black (R, G, G, B channel order) and white, by ColorData
/// version (word index offsets per the ExifTool Canon ColorData tables, as LightCraft reads them:
/// versions 16-19 black 329 white 797; 32/33 black 343 white 811; 34/48 black 363 white 641; 64/65
/// black 383 white 661; 66 black 383, no white). Timed-metadata maker notes win over `CMT3`.
fn color_data_levels(parts: &Parts<'_>, bits: u16) -> (Option<[f64; 4]>, Option<u32>) {
    let mut notes: Vec<&[u8]> = parts.ctmd.map(timed_maker_notes).unwrap_or_default();
    notes.extend(parts.cmt3);
    for note in notes {
        let Ok((tiff, first)) = Tiff::parse(note) else { continue };
        let Ok(ifd) = tiff.ifd(first) else { continue };
        let Some(values) = ifd.find(0x4001).and_then(|e| tiff.u32s(e).ok()) else { continue };
        let (black_at, white_at) = match values.first() {
            Some(16..=19) => (329usize, Some(797usize)),
            Some(32 | 33) => (343, Some(811)),
            Some(34 | 48) => (363, Some(641)),
            Some(64 | 65) => (383, Some(661)),
            Some(66) => (383, None),
            _ => return (None, None),
        };
        let full = VendorFrame::full_scale(bits).unwrap_or(0);
        let black = values
            .get(black_at..black_at.saturating_add(4))
            .and_then(|v| <[u32; 4]>::try_from(v).ok())
            .filter(|v| v.iter().any(|b| *b > 0) && v.iter().all(|b| *b < full / 2))
            .map(|v| v.map(f64::from));
        let max_black = black.map_or(0.0, |b| b.iter().copied().fold(0.0, f64::max));
        let white = white_at
            .and_then(|at| values.get(at).copied())
            .filter(|w| f64::from(*w) > max_black && *w >= full / 2 && *w <= full);
        return (black, white);
    }
    (None, None)
}

fn cfa(code: u8) -> Option<CfaPattern> {
    Some(CfaPattern::Bayer(match code {
        0 => Bayer::Rggb,
        1 => Bayer::Grbg,
        2 => Bayer::Gbrg,
        3 => Bayer::Bggr,
        _ => return None,
    }))
}

fn model(cmt1: Option<&[u8]>) -> Option<String> {
    let (tiff, first) = Tiff::parse(cmt1?).ok()?;
    let ifd = tiff.ifd(first).ok()?;
    tiffep::ascii(&tiff, &ifd, tiffep::MODEL).ok().flatten()
}

pub fn decode(bytes: &[u8]) -> Result<DngRaw, DevelopError> {
    let parts = parse(bytes);
    // First largest raw track (a later equal-size track can be the Dual Pixel delta).
    let mut main: Option<&RawTrack> = None;
    for t in &parts.tracks {
        if main.is_none_or(|m| t.area > m.area) {
            main = Some(t);
        }
    }
    let main = main.ok_or(DevelopError::UnsupportedContainer)?;
    let (at, len) = main.cmp1;
    let coding = Coding::parse(bytes.get(at..at.checked_add(len).ok_or(DevelopError::Overflow)?).ok_or(DevelopError::InvalidContainer)?)?;
    let cfa_pattern = cfa(coding.cfa_pattern).ok_or(DevelopError::UnsupportedPattern)?;
    let (offset, size) = main.sample.ok_or(DevelopError::InvalidContainer)?;
    let sample = bytes.get(offset..offset.checked_add(size).ok_or(DevelopError::Overflow)?).ok_or(DevelopError::InvalidContainer)?;
    let samples = crx::decode(sample, &coding)?;
    let bits = u16::from(coding.bit_depth);
    let full = VendorFrame::full_scale(bits).ok_or(DevelopError::UnsupportedContainer)?;
    // Levels from ColorData: black per channel mapped to the CMP1 CFA's 2x2 site order (sites are
    // anchored at the stored frame origin, as the CMP1 CFA code is); white from ColorData, else
    // what the frame proves (saturation plateau or >= 16 samples at full scale), else unread.
    let (black, tagged_white) = color_data_levels(&parts, bits);
    let mut applied = Vec::new();
    let (black_repeat, black_level) = match black {
        Some([r, g1, g2, b]) => {
            applied.push("cr3_black_level_unread");
            let sites = match coding.cfa_pattern {
                1 => [g1, r, b, g2],
                2 => [g1, b, r, g2],
                3 => [b, g1, g2, r],
                _ => [r, g1, g2, b],
            };
            ((2, 2), sites.to_vec())
        }
        None => ((1, 1), vec![0.0]),
    };
    let white = tagged_white.or_else(|| {
        levels::measured_white(&samples, coding.width, coding.height, levels::Region::whole(coding.width, coding.height), full)
    });
    if white.is_some() {
        applied.push("cr3_white_level_unread");
    }
    DngRaw::from_vendor(
        VendorFrame {
            width: coding.width,
            height: coding.height,
            bits,
            cfa_pattern,
            unique_camera_model: model(parts.cmt1),
            unapplied: levels::remaining(&UNAPPLIED, &applied),
            samples,
            white_level: white.unwrap_or(full),
            black_repeat,
            black_level,
        },
        bytes,
    )
}
