//! Photoshop layer records carried in a TIFF's ImageSourceData (tag 37724; SMB-CONVERT-FOLLOWUPS
//! slice 3). After the `Adobe Photoshop Document Data Block\0` signature (validated by
//! hsk-studio-interop-tiff `image_source_data`) the tag holds a run of additional-layer-information
//! blocks, padded to 4 bytes, with the layer info in `Layr`/`Lr16`/`Lr32` and the global layer mask
//! in `LMsk` (layout: Adobe Photoshop TIFF Technical Notes; cross-checked against psdtags, BSD-3,
//! `TiffImageSourceData.read`). Photoshop writes no PSD header into the tag, so the canvas (size,
//! depth, colour mode, profile) comes from the TIFF page.
//!
//! The blocks go through this crate's own PSD readers (`layers::parse_tagged_blocks`,
//! `document::read_layer_info`, `layers::read_record`, `layers::read_channel_data`): there is no
//! second layer parser. The result is a header-only [`PsdDocument`] (no image-data section; the
//! composite is the TIFF page) that [`crate::to_proposal`] maps like any PSD.
//!
//! Big-endian blocks (`8BIM`, `8B64` = PSB-style 8-byte lengths for the wide keys) are parsed.
//! Little-endian blocks (`MIB8`, `46B8`: byte-swapped signatures, keys and numbers) are refused with
//! `UnsupportedMode("image_source_little_endian")`: every reader in this crate is big-endian (test
//! `little_endian_image_source_data_is_a_typed_refusal`).
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_accord::CancellationToken;

use crate::document::{LAYER_INFO_KEYS, LayerInfoLocation, LayerSection, PsdDocument, read_layer_info};
use crate::error::{PsdError, Result};
use crate::header::{ColorMode, Header};
use crate::layers::parse_tagged_blocks;
use crate::limits::Limits;
use crate::resources::ImageResources;

const KEY_GLOBAL_MASK: [u8; 4] = *b"LMsk";

/// The TIFF page that carries the layer records: its size, sample depth, colour mode and embedded
/// profile become the header and profile of the returned document.
#[derive(Clone, Debug)]
pub struct ImageSourceCanvas {
    pub width: u32,
    pub height: u32,
    /// 8, 16 or 32.
    pub depth: u16,
    /// RGB, grayscale, CMYK or Lab.
    pub colour_mode: ColorMode,
    pub icc: Option<Vec<u8>>,
}

/// Parses the tagged blocks of a TIFF ImageSourceData tag into a header-only [`PsdDocument`].
///
/// `tagged` starts at the first block signature, i.e. the tag value after the 36-byte document
/// signature. Checked before use: the input against `Limits::max_input_bytes`, the signature
/// (`8BIM`/`8B64`; `MIB8`/`46B8` refused as little-endian), the canvas (depth, mode, dimensions,
/// `Limits::max_pixels`), and every block, record and channel length by the PSD readers. A run
/// without a layer-info block is `Proposal("image_source_no_layer_info")`. The `LMsk` block becomes
/// the global mask; other blocks stay as global tagged blocks (typed `tagged_block_opaque` loss).
pub fn image_source_document(
    tagged: &[u8],
    canvas: ImageSourceCanvas,
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<PsdDocument> {
    cancel.check()?;
    if tagged.len() > limits.max_input_bytes {
        return Err(PsdError::InputTooLarge);
    }
    let psb = match tagged.get(..4) {
        Some(b"8BIM") => false,
        Some(b"8B64") => true,
        Some(b"MIB8" | b"46B8") => return Err(PsdError::UnsupportedMode("image_source_little_endian")),
        Some(_) => return Err(PsdError::BadSignature),
        None => return Err(PsdError::Truncated("image_source_data")),
    };
    let ImageSourceCanvas { width, height, depth, colour_mode, icc } = canvas;
    if !matches!(depth, 8 | 16 | 32) {
        return Err(PsdError::BadDepth);
    }
    let channels: u16 = match colour_mode {
        ColorMode::Rgb | ColorMode::Lab => 3,
        ColorMode::Cmyk => 4,
        ColorMode::Grayscale => 1,
        _ => return Err(PsdError::UnsupportedMode("image_source_colour_mode")),
    };
    let max_dim = if psb { 300_000 } else { 30_000 };
    if width == 0 || height == 0 || width > max_dim || height > max_dim {
        return Err(PsdError::BadDimensions);
    }
    if u64::from(width).checked_mul(u64::from(height)).is_none_or(|p| p > limits.max_pixels) {
        return Err(PsdError::PixelLimit);
    }
    let header = Header { version: if psb { 2 } else { 1 }, channels, height, width, depth, color_mode: colour_mode };

    let (mut blocks, trailing) = parse_tagged_blocks(tagged, psb, true, limits)?;
    let at = blocks
        .iter()
        .position(|b| LAYER_INFO_KEYS.contains(&b.key))
        .ok_or(PsdError::Proposal("image_source_no_layer_info"))?;
    let block = blocks.remove(at);
    let mut info = read_layer_info(&block.data, &header, limits, cancel)?;
    info.location = LayerInfoLocation::Tagged { signature: block.signature, key: block.key, insert_at: at, pad: block.pad };
    let global_mask = blocks.iter().position(|b| b.key == KEY_GLOBAL_MASK).map(|i| blocks.remove(i).data);
    let layer_section = LayerSection { layer_info: Some(info), global_mask, blocks, trailing };
    Ok(PsdDocument {
        header,
        color_mode_data: Vec::new(),
        resources: ImageResources::with_icc_profile(icc),
        layer_section,
        image_data: None,
        tail: Vec::new(),
    })
}
