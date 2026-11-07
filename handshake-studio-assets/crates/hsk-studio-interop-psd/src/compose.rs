//! A new layered PSD/PSB from layer pixels (lane SMB-DOC-EXPORT-1; STU-RAS-042/043, scenario photo-retouch
//! step 108): the writer for a document that did not come from a PSD, where [`crate::export_psd`] needs the
//! original file as its base. The caller lists the layer records bottom-most first (the file's own order), with a
//! group as its hidden end marker ([`ComposeRecord::GroupEnd`], `lsct` 3), its children, then its folder record
//! ([`ComposeRecord::GroupOpen`], `lsct` 1); the merged image is the caller's composite. Everything goes through
//! this crate's model and [`crate::write_psd`] (no second serializer): raw (compression 0) channels, a `luni`
//! Unicode name beside the Latin-1 Pascal name, `iOpa` for a fill opacity below 255, the ICC profile as resource
//! 1039. RGB only, 8 or 16 bits; width or height above 30000 writes PSB (version 2), above 300000 is refused.
//! Typed refusals before allocation: depth, canvas and layer rectangles against [`Limits::max_pixels`], the layer
//! count, sample buffers of the wrong length, unbalanced groups, and the whole file against
//! [`Limits::max_input_bytes`]. Test: hsk-studio-harness `tests/doc_export.rs` (layered export re-read and reopened).
#![deny(clippy::arithmetic_side_effects)]

use crate::blend::{StudioBlendMode, psd_key_for};
use crate::document::{ImageData, LayerInfo, LayerInfoLocation, LayerSection, PsdDocument};
use crate::error::{PsdError, Result};
use crate::header::{ColorMode, Header};
use crate::layers::{self, KEY_SECTION_DIVIDER, KEY_UNICODE_NAME, PsdChannel, PsdLayer, Rect, TaggedBlock};
use crate::limits::Limits;
use crate::resources::ImageResources;
use crate::writer::write_psd;

/// Interleaved straight RGBA samples (colour sRGB-encoded as the file's profile says; alpha unassociated).
#[derive(Clone, Debug, PartialEq)]
pub enum ComposeSamples {
    U8(Vec<u8>),
    U16(Vec<u16>),
}

impl ComposeSamples {
    fn len(&self) -> usize {
        match self {
            Self::U8(v) => v.len(),
            Self::U16(v) => v.len(),
        }
    }

    fn depth(&self) -> u16 {
        match self {
            Self::U8(_) => 8,
            Self::U16(_) => 16,
        }
    }

    /// Big-endian plane `c` (0..4) of `count` pixels.
    fn plane(&self, c: usize, count: usize) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(count.checked_mul(usize::from(self.depth() / 8)).ok_or(PsdError::Overflow)?);
        match self {
            Self::U8(v) => out.extend(v.iter().skip(c).step_by(4).take(count)),
            Self::U16(v) => {
                for s in v.iter().skip(c).step_by(4).take(count) {
                    out.extend_from_slice(&s.to_be_bytes());
                }
            }
        }
        Ok(out)
    }
}

/// Appearance shared by every record kind.
#[derive(Clone, Debug, PartialEq)]
pub struct ComposeLook {
    pub name: String,
    /// 0..=255.
    pub opacity: u8,
    /// 0..=255 (`iOpa`, written when below 255).
    pub fill_opacity: u8,
    pub blend: StudioBlendMode,
    pub visible: bool,
    /// Clipped to the layer below.
    pub clipped: bool,
}

/// One layer record, in file order (bottom-most first).
#[derive(Clone, Debug, PartialEq)]
pub enum ComposeRecord {
    /// A pixel layer at `left`, `top` (canvas pixels; may lie partly or wholly outside the canvas).
    Pixels { look: ComposeLook, left: i32, top: i32, width: u32, height: u32, rgba: ComposeSamples },
    /// The hidden end marker of a group (below its children).
    GroupEnd,
    /// A group's folder record (above its children); `PassThrough` blends as `pass`.
    GroupOpen { look: ComposeLook },
}

/// The file to write.
#[derive(Clone, Debug, PartialEq)]
pub struct ComposeRequest {
    pub width: u32,
    pub height: u32,
    /// The merged composite, `width x height` RGBA at the file depth.
    pub merged: ComposeSamples,
    /// Write the merged alpha (the layer count goes negative: the first alpha channel is the merged transparency).
    pub merged_alpha: bool,
    pub icc: Option<Vec<u8>>,
    pub records: Vec<ComposeRecord>,
}

const MAX_PSD_SIDE: u32 = 30_000;
const MAX_PSB_SIDE: u32 = 300_000;

fn pixel_count(width: u32, height: u32, limits: &Limits) -> Result<usize> {
    let n = u64::from(width).checked_mul(u64::from(height)).ok_or(PsdError::Overflow)?;
    if n > limits.max_pixels {
        return Err(PsdError::PixelLimit);
    }
    usize::try_from(n).map_err(|_| PsdError::Overflow)
}

fn luni(name: &str, limits: &Limits) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().take(limits.max_name_units).collect();
    // At most max_name_units (u32 range by construction of the limit) units.
    let mut data = u32::try_from(units.len()).unwrap_or(u32::MAX).to_be_bytes().to_vec();
    for u in &units {
        data.extend_from_slice(&u.to_be_bytes());
    }
    while !data.len().is_multiple_of(4) {
        data.push(0);
    }
    data
}

fn record(look: &ComposeLook, rect: Rect, channels: Vec<PsdChannel>, section: Option<u32>, limits: &Limits) -> Result<PsdLayer> {
    let blend_key = match (section, look.blend) {
        (Some(1), StudioBlendMode::PassThrough) => *b"pass",
        (_, mode) => psd_key_for(mode).filter(|k| k != b"pass").ok_or(PsdError::Encode("compose_blend_mode"))?,
    };
    let name_pascal: Vec<u8> = look.name.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).take(255).collect();
    let pad = pad_to_four(name_pascal.len().wrapping_add(1));
    let mut blocks = Vec::new();
    if let Some(kind) = section {
        let mut data = kind.to_be_bytes().to_vec();
        data.extend_from_slice(b"8BIM");
        data.extend_from_slice(&blend_key);
        blocks.push(TaggedBlock { signature: *b"8BIM", key: KEY_SECTION_DIVIDER, data, pad: Vec::new() });
    }
    if look.fill_opacity < u8::MAX {
        blocks.push(TaggedBlock { signature: *b"8BIM", key: *b"iOpa", data: vec![look.fill_opacity, 0, 0, 0], pad: Vec::new() });
    }
    blocks.push(TaggedBlock { signature: *b"8BIM", key: KEY_UNICODE_NAME, data: luni(&look.name, limits), pad: Vec::new() });
    Ok(PsdLayer {
        rect,
        channels,
        blend_signature: *b"8BIM",
        blend_key,
        opacity: look.opacity,
        clipping: u8::from(look.clipped),
        flags: if look.visible { 0 } else { 0x02 },
        filler: 0,
        mask_raw: Vec::new(),
        blending_ranges_raw: Vec::new(),
        name: look.name.clone(),
        name_pascal,
        name_pad: vec![0; pad],
        blocks,
        extra_trailing: Vec::new(),
        little_endian: false,
    })
}

/// Zero bytes that bring `len` to a multiple of 4.
const fn pad_to_four(len: usize) -> usize {
    [0, 3, 2, 1][len & 3]
}

fn empty_channels() -> Vec<PsdChannel> {
    [-1i16, 0, 1, 2].into_iter().map(|id| PsdChannel { id, compression: 0, data: Vec::new() }).collect()
}

/// Writes the layered file. The output re-reads through [`crate::read_psd`] (hsk-studio-harness `tests/doc_export.rs`).
pub fn compose_psd(req: &ComposeRequest, limits: &Limits) -> Result<Vec<u8>> {
    let depth = req.merged.depth();
    if req.width == 0 || req.height == 0 || req.width > MAX_PSB_SIDE || req.height > MAX_PSB_SIDE {
        return Err(PsdError::BadDimensions);
    }
    if req.records.len() > limits.max_layers || req.records.len() > usize::from(i16::MAX.unsigned_abs()) {
        return Err(PsdError::LayerLimit);
    }
    let canvas = pixel_count(req.width, req.height, limits)?;
    let per = usize::from(depth / 8);
    // V4-02: the running aggregate of every plane this file will hold, checked before any plane is built.
    let mut total = canvas.checked_mul(4).and_then(|n| n.checked_mul(per)).ok_or(PsdError::Overflow)?;
    let mut open_groups = 0usize;
    for r in &req.records {
        match r {
            ComposeRecord::Pixels { width, height, rgba, .. } => {
                if rgba.depth() != depth {
                    return Err(PsdError::BadDepth);
                }
                let n = pixel_count(*width, *height, limits)?;
                if n.checked_mul(4) != Some(rgba.len()) {
                    return Err(PsdError::ChannelSize);
                }
                total = n.checked_mul(4).and_then(|b| b.checked_mul(per)).and_then(|b| total.checked_add(b)).ok_or(PsdError::Overflow)?;
            }
            ComposeRecord::GroupEnd => open_groups = open_groups.checked_add(1).ok_or(PsdError::Overflow)?,
            ComposeRecord::GroupOpen { .. } => open_groups = open_groups.checked_sub(1).ok_or(PsdError::GroupUnbalanced)?,
        }
        if open_groups > limits.max_group_depth {
            return Err(PsdError::GroupDepth);
        }
        if total > limits.max_input_bytes {
            return Err(PsdError::InputTooLarge);
        }
    }
    if open_groups != 0 {
        return Err(PsdError::GroupUnbalanced);
    }
    if req.merged.len() != canvas.checked_mul(4).ok_or(PsdError::Overflow)? {
        return Err(PsdError::ChannelSize);
    }
    let version = if req.width > MAX_PSD_SIDE || req.height > MAX_PSD_SIDE { 2 } else { 1 };
    let header = Header { version, channels: if req.merged_alpha { 4 } else { 3 }, height: req.height, width: req.width, depth, color_mode: ColorMode::Rgb };
    let mut psd_layers = Vec::with_capacity(req.records.len());
    for r in &req.records {
        psd_layers.push(match r {
            ComposeRecord::Pixels { look, left, top, width, height, rgba } => {
                let n = pixel_count(*width, *height, limits)?;
                let right = i32::try_from(i64::from(*left).checked_add(i64::from(*width)).ok_or(PsdError::Overflow)?).map_err(|_| PsdError::LayerRect)?;
                let bottom = i32::try_from(i64::from(*top).checked_add(i64::from(*height)).ok_or(PsdError::Overflow)?).map_err(|_| PsdError::LayerRect)?;
                let rect = Rect { top: *top, left: *left, bottom, right };
                let channels = [(-1i16, 3usize), (0, 0), (1, 1), (2, 2)]
                    .into_iter()
                    .map(|(id, c)| Ok(PsdChannel { id, compression: 0, data: rgba.plane(c, n)? }))
                    .collect::<Result<Vec<_>>>()?;
                record(look, rect, channels, None, limits)?
            }
            ComposeRecord::GroupEnd => {
                let look = ComposeLook { name: "</Layer group>".into(), opacity: u8::MAX, fill_opacity: u8::MAX, blend: StudioBlendMode::Normal, visible: true, clipped: false };
                record(&look, Rect::default(), empty_channels(), Some(3), limits)?
            }
            ComposeRecord::GroupOpen { look } => record(look, Rect::default(), empty_channels(), Some(1), limits)?,
        });
    }
    // Layer info padded to a multiple of 4 bytes: count, records, channel payloads (each with its compression).
    let mut size = 2usize;
    for l in &psd_layers {
        let mut scratch = Vec::new();
        layers::encode_record(l, &header, &mut scratch)?;
        size = size.checked_add(scratch.len()).ok_or(PsdError::Overflow)?;
        for c in &l.channels {
            size = size.checked_add(c.data.len()).and_then(|s| s.checked_add(2)).ok_or(PsdError::Overflow)?;
        }
    }
    let trailing = vec![0u8; pad_to_four(size)];
    let mut image = Vec::with_capacity(canvas.checked_mul(usize::from(header.channels)).and_then(|n| n.checked_mul(per)).ok_or(PsdError::Overflow)?);
    for c in 0..usize::from(header.channels) {
        image.extend(req.merged.plane(c, canvas)?);
    }
    let doc = PsdDocument {
        header,
        color_mode_data: Vec::new(),
        resources: ImageResources::with_icc_profile(req.icc.clone()),
        layer_section: LayerSection {
            layer_info: (!psd_layers.is_empty()).then(|| LayerInfo { layers: psd_layers, merged_alpha: req.merged_alpha, trailing, location: LayerInfoLocation::Primary }),
            global_mask: Some(Vec::new()),
            blocks: Vec::new(),
            trailing: Vec::new(),
        },
        image_data: Some(ImageData { compression: 0, data: image }),
        tail: Vec::new(),
    };
    write_psd(&doc, limits)
}
