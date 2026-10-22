//! Pattern resources (lane SMB-PS-ADJUST-STYLES-3): the global additional-layer-information blocks
//! `Patt`, `Pat2` and `Pat3` decoded to straight linear-light RGBA, so a `patternFill` layer effect
//! imports as a Folio pattern overlay whose `pattern_tile` carries the pixels (STU-RAS-157 row
//! `pattern_overlay`; STU-IO-006 row 1 "unrepresentable features baked with receipt").
//!
//! Layout (Adobe Photoshop File Formats Specification, "Patterns" and "Virtual Memory Array List";
//! every field below was confirmed on the psd-tools corpus files `patterns.psd`, `layer_effects.psd`,
//! `stroke.psd`, `layers/pattern-fill.psd|.psb` and `multichannel-pattern-fill.psd`): a block is a
//! run of patterns, each `u32` length + payload padded to 4 bytes; the payload is version `u32`
//! (1), image mode `u32`, height `u16`, width `u16`, a Unicode name (`u32` units + UTF-16BE), a
//! Pascal unique id, a 768-byte colour table for indexed mode, then the virtual memory array list:
//! version `u32` (3), length `u32`, rectangle (top, left, bottom, right `i32`), channel count `u32`,
//! and `count + 2` arrays (the colour channels, a user mask, then the transparency), each `u32`
//! written flag and, when written, `u32` length, depth `u32`, rectangle, depth `u16`, compression
//! `u8` and the samples (0 raw; 1 PackBits after `u16` row byte counts, also in PSB files).
//!
//! Mapped: RGB, grayscale and indexed patterns at 8 or 16 bits; the samples are display-encoded
//! sRGB (the importer's assumption for unprofiled pixels) decoded to linear light; the last array is
//! the alpha when written (seen only on the multichannel file: UNVERIFIED for RGB patterns with
//! transparency, D5), else opaque. Everything else is a typed loss on the pattern and the effects
//! that reference it are lost with `pattern_overlay_pattern_missing`. Hostile input (V4-01/02):
//! every length is checked against the bytes left, a side is at most [`MAX_PATTERN_SIDE`] (Folio's
//! `pattern_tile` bound), the decoded bytes of all patterns stay within `Limits::max_decoded_bytes`
//! and at most [`MAX_PATTERNS`] patterns are decoded per document, all checked before allocating.
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_accord::CancellationToken;

use crate::compression::{self, PlaneGeometry};
use crate::layers::TaggedBlock;
use crate::limits::Limits;
use crate::loss::{LossKind, key_text};
use crate::reader::Reader;
use crate::samples::{SixteenBitScale, sample_to_unit, srgb_to_linear};

/// Global block keys holding patterns.
pub(crate) const PATTERN_KEYS: [[u8; 4]; 3] = [*b"Patt", *b"Pat2", *b"Pat3"];
/// Largest pattern side (Folio `pattern_tile`, pigment `MAX_PATTERN_SIDE`).
pub const MAX_PATTERN_SIDE: u32 = 4096;
/// Most patterns decoded from one document.
pub const MAX_PATTERNS: usize = 1024;
/// Most arrays in one virtual memory array list (`count + 2` with count at most 56).
const MAX_ARRAYS: u32 = 58;

/// One decoded pattern: straight linear-light RGBA, row-major.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pattern {
    /// The PSD unique id (`Ptrn`/`Idnt` of an effect names it).
    pub psd_id: String,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[f32; 4]>,
}

/// The decoded patterns of a document and the typed notes of what was not decoded.
#[derive(Debug, Default)]
pub(crate) struct PatternSet {
    pub patterns: Vec<Pattern>,
    pub notes: Vec<(String, LossKind, &'static str)>,
}

impl PatternSet {
    pub(crate) fn find(&self, psd_id: &str) -> Option<&Pattern> {
        self.patterns.iter().find(|p| p.psd_id == psd_id)
    }
}

type Refusal = &'static str;

/// A pattern as a Folio reference: the host-minted object id of the pattern primitive and the tile
/// that carries its pixels (`StudioEffectKind::PatternOverlay::pattern_tile`).
#[derive(Clone, Debug)]
pub(crate) struct PatternRef {
    pub object_id: String,
    pub tile: hsk_studio_folio::TileRef,
}

/// The document's patterns by PSD unique id, each minted or with the code of why it is not.
#[derive(Debug, Default)]
pub(crate) struct PatternTable {
    pub entries: Vec<(String, Result<PatternRef, Refusal>)>,
}

impl PatternTable {
    /// The reference of the pattern `psd_id`: `pattern_overlay_pattern_missing` when the document
    /// holds no decodable pattern of that id, otherwise the pattern's own refusal (for example
    /// `pattern_overlay_unminted`).
    pub(crate) fn lookup(&self, psd_id: &str) -> Result<&PatternRef, Refusal> {
        match self.entries.iter().find(|(id, _)| id == psd_id) {
            Some((_, Ok(r))) => Ok(r),
            Some((_, Err(code))) => Err(code),
            None => Err("pattern_overlay_pattern_missing"),
        }
    }
}

/// Decodes every pattern of the `Patt` / `Pat2` / `Pat3` blocks in `blocks` (the document's global
/// list). A malformed pattern is skipped with a typed note; a malformed run ends its block.
pub(crate) fn decode(
    blocks: &[TaggedBlock],
    scale: SixteenBitScale,
    limits: &Limits,
    cancel: &CancellationToken,
) -> PatternSet {
    let mut set = PatternSet::default();
    let mut budget = limits.max_decoded_bytes;
    for block in blocks.iter().filter(|b| PATTERN_KEYS.contains(&b.key)) {
        let key = key_text(block.key);
        let mut r = Reader::new(&block.data);
        let mut index = 0usize;
        while r.remaining() >= 4 {
            let path = format!("layer_section/tagged/{key}/pattern[{index}]");
            index = index.saturating_add(1);
            if cancel.check().is_err() {
                set.notes.push((path, LossKind::Unsupported, "pattern_decode_canceled"));
                return set;
            }
            let Ok(len) = r.u32("pattern") else { break };
            let Ok(len) = usize::try_from(len) else { break };
            let Ok(payload) = r.bytes(len, "pattern") else {
                set.notes.push((path, LossKind::Unsupported, "pattern_truncated"));
                break;
            };
            // payload padded to a multiple of 4
            let pad = len.wrapping_neg() & 3;
            r.skip_up_to(pad);
            if set.patterns.len() >= MAX_PATTERNS {
                set.notes.push((path, LossKind::Unsupported, "patterns_over_limit"));
                break;
            }
            match pattern(payload, scale, &mut budget, limits, cancel) {
                Ok(p) if set.find(&p.psd_id).is_some() => {
                    set.notes.push((path, LossKind::Preserved, "pattern_duplicate_id_first_kept"));
                }
                Ok(p) => set.patterns.push(p),
                Err(code) => set.notes.push((path, LossKind::Unsupported, code)),
            }
        }
    }
    set
}

fn rect(r: &mut Reader<'_>) -> Result<(i32, i32, i32, i32), Refusal> {
    let mut v = [0i32; 4];
    for slot in &mut v {
        *slot = r.i32("pattern_rect").map_err(|_| "pattern_truncated")?;
    }
    Ok((v[0], v[1], v[2], v[3]))
}

fn pattern(
    payload: &[u8],
    scale: SixteenBitScale,
    budget: &mut usize,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<Pattern, Refusal> {
    let t = |_| "pattern_truncated";
    let mut r = Reader::new(payload);
    if r.u32("pattern").map_err(t)? != 1 {
        return Err("pattern_version_unsupported");
    }
    let mode = r.u32("pattern").map_err(t)?;
    let height = u32::from(r.u16("pattern").map_err(t)?);
    let width = u32::from(r.u16("pattern").map_err(t)?);
    let units = usize::try_from(r.u32("pattern_name").map_err(t)?).map_err(|_| "pattern_truncated")?;
    if units > limits.max_name_units {
        return Err("pattern_name_over_limit");
    }
    r.bytes(units.checked_mul(2).ok_or("pattern_truncated")?, "pattern_name").map_err(t)?;
    let id_len = usize::from(r.u8("pattern_id").map_err(t)?);
    let id = r.bytes(id_len, "pattern_id").map_err(t)?;
    if id.is_empty() || !id.iter().all(|c| c.is_ascii_graphic()) {
        return Err("pattern_id_invalid");
    }
    let psd_id = String::from_utf8_lossy(id).into_owned();
    let table = if mode == 2 { Some(r.bytes(768, "pattern_colour_table").map_err(t)?) } else { None };
    if !matches!(mode, 1..=3) {
        return Err("pattern_mode_unmapped");
    }
    if width == 0 || height == 0 {
        return Err("pattern_size_invalid");
    }
    if width > MAX_PATTERN_SIDE || height > MAX_PATTERN_SIDE {
        return Err("pattern_over_limit");
    }
    // Charge the decoded pixels (16 bytes each) before anything is allocated.
    let pixels = usize::try_from(u64::from(width).saturating_mul(u64::from(height))).map_err(|_| "pattern_over_limit")?;
    let bytes = pixels.checked_mul(16).ok_or("pattern_over_limit")?;
    *budget = budget.checked_sub(bytes).ok_or("patterns_over_budget")?;
    // virtual memory array list
    if r.u32("vma").map_err(t)? != 3 {
        return Err("pattern_array_version_unsupported");
    }
    let vlen = usize::try_from(r.u32("vma").map_err(t)?).map_err(|_| "pattern_truncated")?;
    let mut v = Reader::new(r.bytes(vlen, "vma").map_err(t)?);
    let (top, left, bottom, right) = rect(&mut v)?;
    let span = |a: i32, b: i32| i64::from(b).checked_sub(i64::from(a));
    if span(top, bottom) != Some(i64::from(height)) || span(left, right) != Some(i64::from(width)) {
        return Err("pattern_rect_mismatch");
    }
    let count = v.u32("vma").map_err(t)?;
    let arrays = count.checked_add(2).filter(|n| *n <= MAX_ARRAYS).ok_or("pattern_channels_over_limit")?;
    let colour_arrays: u32 = if mode == 3 { 3 } else { 1 };
    if count < colour_arrays {
        return Err("pattern_channels_missing");
    }
    let mut planes: Vec<Option<(u16, Vec<u8>)>> = Vec::new();
    for k in 0..arrays {
        if v.u32("vma_array").map_err(t)? == 0 {
            planes.push(None);
            continue;
        }
        let alen = usize::try_from(v.u32("vma_array").map_err(t)?).map_err(|_| "pattern_truncated")?;
        let mut a = Reader::new(v.bytes(alen, "vma_array").map_err(t)?);
        // Only the colour arrays and the transparency (the last) are decoded; the others are skipped
        // undecoded, so a hostile list of many written arrays costs no decoding.
        if k >= colour_arrays && k.checked_add(1) != Some(arrays) {
            planes.push(None);
            continue;
        }
        let depth = a.u32("vma_array").map_err(t)?;
        let (at, al, ab, ar) = rect(&mut a)?;
        let _depth16 = a.u16("vma_array").map_err(t)?;
        let compression = a.u8("vma_array").map_err(t)?;
        if (at, al, ab, ar) != (top, left, bottom, right) {
            return Err("pattern_channel_rect");
        }
        let depth = match depth {
            8 => 8u16,
            16 => 16,
            _ => return Err("pattern_depth_unsupported"),
        };
        let geometry = PlaneGeometry { width, height, depth, planes: 1, psb: false };
        // the decoded plane is charged before it is allocated
        let plane_bytes = compression::row_bytes(width, depth)
            .ok()
            .and_then(|row| row.checked_mul(usize::try_from(height).ok()?))
            .ok_or("pattern_over_limit")?;
        *budget = budget.checked_sub(plane_bytes).ok_or("patterns_over_budget")?;
        let samples = compression::decode(u16::from(compression), a.rest(), geometry, limits, cancel)
            .map_err(|_| "pattern_channel_undecodable")?;
        planes.push(Some((depth, samples)));
    }
    let sample = |plane: &(u16, Vec<u8>), i: usize| -> Result<f32, Refusal> {
        let (depth, bytes) = plane;
        let raw = if *depth == 8 {
            u32::from(*bytes.get(i).ok_or("pattern_truncated")?)
        } else {
            let at = i.checked_mul(2).ok_or("pattern_truncated")?;
            let hi = *bytes.get(at).ok_or("pattern_truncated")?;
            let lo = *bytes.get(at.checked_add(1).ok_or("pattern_truncated")?).ok_or("pattern_truncated")?;
            u32::from(u16::from_be_bytes([hi, lo]))
        };
        Ok(sample_to_unit(raw, *depth, scale))
    };
    let channel = |k: usize| planes.get(k).and_then(Option::as_ref).ok_or("pattern_channels_missing");
    let alpha = planes.last().and_then(Option::as_ref);
    let mut out = Vec::new();
    out.try_reserve_exact(pixels).map_err(|_| "pattern_over_limit")?;
    for i in 0..pixels {
        let rgb = match (mode, table) {
            (3, _) => [sample(channel(0)?, i)?, sample(channel(1)?, i)?, sample(channel(2)?, i)?],
            (2, Some(table)) => {
                let (_, bytes) = channel(0)?;
                let index = usize::from(*bytes.get(i).ok_or("pattern_truncated")?);
                let c = |k: usize| -> Result<f32, Refusal> {
                    let at = k.checked_mul(256).and_then(|b| b.checked_add(index)).ok_or("pattern_truncated")?;
                    Ok(sample_to_unit(u32::from(*table.get(at).ok_or("pattern_truncated")?), 8, scale))
                };
                [c(0)?, c(1)?, c(2)?]
            }
            _ => {
                let g = sample(channel(0)?, i)?;
                [g, g, g]
            }
        };
        let a = match alpha {
            Some(plane) => sample(plane, i)?,
            None => 1.0,
        };
        out.push([srgb_to_linear(rgb[0]), srgb_to_linear(rgb[1]), srgb_to_linear(rgb[2]), a]);
    }
    Ok(Pattern { psd_id, width, height, pixels: out })
}
