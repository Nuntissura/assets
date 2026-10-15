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
//! a Dual Pixel delta). Not read (named in `unapplied`): black/white levels (maker note `CMT3` /
//! timed metadata), the `IAD1` active area and crop.
// V4-01: this module decodes untrusted bytes; every integer operation is checked or saturating.
#![deny(clippy::arithmetic_side_effects)]
use crate::DevelopError;
use crate::crx::{self, Coding};
use crate::dng::{DngRaw, VendorFrame};
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
fn trak(bytes: &[u8], t: &BoxRef, budget: &mut usize) -> Option<RawTrack> {
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
    let (mut entry, mut size, mut offset) = (None, None, None);
    for b in boxes(bytes, stbl.body, stbl.end, budget) {
        let body = bytes.get(b.body..b.end)?;
        match &b.kind {
            // full box: version/flags, entry count, entries
            b"stsd" if be32(body, 4)? > 0 => {
                entry = boxes(bytes, b.body.saturating_add(8), b.end, budget).first().and_then(|e| craw_entry(bytes, e, budget));
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
    let (width, height, cmp1) = entry?;
    let sample = match (offset, size) {
        (Some(o), Some(s)) if o.checked_add(s as usize).is_some_and(|e| e <= bytes.len()) => Some((o, s as usize)),
        _ => None,
    };
    Some(RawTrack { area: u64::from(width).saturating_mul(u64::from(height)), cmp1: cmp1?, sample })
}

/// The parts of the file this decoder reads: the raw tracks and the `CMT1` TIFF stream.
fn parse(bytes: &[u8]) -> (Vec<RawTrack>, Option<&[u8]>) {
    let mut budget = MAX_BOXES;
    let (mut tracks, mut cmt1) = (Vec::new(), None);
    for top in boxes(bytes, 0, bytes.len(), &mut budget) {
        if &top.kind != b"moov" {
            continue;
        }
        for b in boxes(bytes, top.body, top.end, &mut budget) {
            match &b.kind {
                b"uuid" if b.uuid == Some(CANON_UUID) => {
                    for c in boxes(bytes, b.body, b.end, &mut budget) {
                        if &c.kind == b"CMT1" {
                            cmt1 = bytes.get(c.body..c.end);
                        }
                    }
                }
                b"trak" => tracks.extend(trak(bytes, &b, &mut budget)),
                _ => {}
            }
        }
    }
    (tracks, cmt1)
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
    let (tracks, cmt1) = parse(bytes);
    // First largest raw track (a later equal-size track can be the Dual Pixel delta).
    let mut main: Option<&RawTrack> = None;
    for t in &tracks {
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
    DngRaw::from_vendor(
        VendorFrame {
            width: coding.width,
            height: coding.height,
            bits,
            cfa_pattern,
            unique_camera_model: model(cmt1),
            unapplied: UNAPPLIED.to_vec(),
            samples,
            white_level: VendorFrame::full_scale(bits).ok_or(DevelopError::UnsupportedContainer)?,
        },
        bytes,
    )
}
