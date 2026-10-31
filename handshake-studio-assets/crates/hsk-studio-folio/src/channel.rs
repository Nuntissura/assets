//! Persistent alpha and spot channels of a document (lane SMB-FOLIO-CHANNEL-RECORD; STU-RAS-022,
//! STU-RAS-031, STU-RAS-144): `StudioRasterRecords.channels`, one [`StudioChannelRecord`] per stored
//! channel in Channels-panel order. Saving a selection, the alpha channels and the Channels surface
//! are this one record (STU-RAS-022: "one shared mechanism, not three"); the conversions between a
//! selection and a channel are `hsk_studio_pigment::selection_channel_roundtrip`.
//!
//! Stored kinds are the STU-RAS-144 members `masked_area_alpha`, `selected_area_alpha` and
//! `spot_colour`. The `component` kind (the colour channels) is derived from the document's colour
//! mode and pixels and is never stored here.
//!
//! Samples: every tile holds the channel tone as the Channels panel shows it and as an
//! interchange file stores it, `0` = black and `1` (`255`, `65535`, `1.0`) = white, row-major over
//! one grid cell. The kind says which region the dark, overlay-coloured tone indicates; what a
//! channel loads as a selection is declared in the Pigment module (Studio reading, D5).
//!
//! Extent: a channel covers the whole document raster, `width` x `height` pixels from the canvas
//! origin, and every channel of a set has the same extent. The tiles are the cells of one uniform
//! grid (every tile has the same pixel size) at columns `0..ceil(width / tile_width)` and rows
//! `0..ceil(height / tile_height)`, each cell exactly once; samples of an edge cell beyond the
//! extent are padding. Sample format follows `depth`: 8 = `coverage_u8`, 16 = `coverage_u16le`,
//! 32 = `coverage_f32le` ([`channel_tile_format`]). A tile's `layer_id` is its channel's id.
//!
//! Bounds (Studio-declared): at most [`MAX_CHANNELS`] channels, names of 1..=[`MAX_CHANNEL_NAME_BYTES`]
//! bytes without control characters, extents of 1..=[`MAX_CHANNEL_SIDE`] pixels per side. Display
//! colour components follow [`StudioChannelColour`]; `opacity` is the overlay opacity of an alpha
//! channel and the solidity of a spot channel, percent `0..=100`.
//!
//! Absent and empty are the same record set; an empty list is not encoded, so every record set and
//! digest from before this lane re-serialises byte-identically (test
//! `record_sets_without_channels_reserialise_byte_identically`).
#![deny(clippy::arithmetic_side_effects)]
use super::{
    Code, GeometryUnit, Resolution, ResolutionIssue, Result, TileRef, err, finite_length, id, key,
    values::Ctx,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Most channels one record set holds. The PSD header admits 1..=56 channels including the colour
/// channels (Adobe Photoshop File Formats Specification; enforced by `hsk-studio-interop-psd`
/// `Header::read`); Studio stores at most that many extra channels.
pub const MAX_CHANNELS: usize = 56;
/// Longest channel name in UTF-8 bytes.
pub const MAX_CHANNEL_NAME_BYTES: usize = 256;
/// Largest channel extent per side in pixels (the PSB canvas ceiling).
pub const MAX_CHANNEL_SIDE: u32 = 300_000;

/// Stored channel kinds (STU-RAS-144 `StudioChannel.kind` without `component`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioChannelKind {
    /// The dark tone indicates the masked region (STU-RAS-144 `masked_area_alpha = 2`).
    MaskedAreaAlpha,
    /// The dark tone indicates the selected region (STU-RAS-144 `selected_area_alpha = 3`).
    SelectedAreaAlpha,
    /// The dark tone is ink of the channel's colour (STU-RAS-144 `spot_colour = 4`).
    SpotColour,
}
impl StudioChannelKind {
    /// The STU-RAS-144 enumeration value.
    pub const fn spec_value(self) -> u8 {
        match self {
            Self::MaskedAreaAlpha => 2,
            Self::SelectedAreaAlpha => 3,
            Self::SpotColour => 4,
        }
    }
    /// One of the two alpha kinds.
    pub const fn is_alpha(self) -> bool {
        matches!(self, Self::MaskedAreaAlpha | Self::SelectedAreaAlpha)
    }
}

/// Display colour of a channel (the overlay colour of an alpha channel, the ink colour of a spot
/// channel). Components: `rgb`, `hsb` and `cmyk` in `0..=1`; `lab` with `l` in `0..=100` and
/// `a`, `b` in `-128..=127`; `gray` in `0..=1`. No colour-profile binding: the colour is a display
/// hint, not a document colour value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "model", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioChannelColour {
    Rgb { r: f64, g: f64, b: f64 },
    Hsb { h: f64, s: f64, b: f64 },
    Cmyk { c: f64, m: f64, y: f64, k: f64 },
    Lab { l: f64, a: f64, b: f64 },
    Gray { gray: f64 },
}
impl StudioChannelColour {
    fn valid(&self) -> bool {
        let unit = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
        let ab = |v: f64| v.is_finite() && (-128.0..=127.0).contains(&v);
        match *self {
            Self::Rgb { r, g, b } => [r, g, b].into_iter().all(unit),
            Self::Hsb { h, s, b } => [h, s, b].into_iter().all(unit),
            Self::Cmyk { c, m, y, k } => [c, m, y, k].into_iter().all(unit),
            Self::Lab { l, a, b } => l.is_finite() && (0.0..=100.0).contains(&l) && ab(a) && ab(b),
            Self::Gray { gray } => unit(gray),
        }
    }
}

/// One persistent alpha or spot channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioChannelRecord {
    /// Set-local stable id (`[A-Za-z0-9_-]{1,128}`); renames and reorders keep it.
    pub channel_id: String,
    pub name: String,
    pub kind: StudioChannelKind,
    pub visible: bool,
    pub colour: StudioChannelColour,
    /// Overlay opacity (alpha kinds) or solidity (spot), percent `0..=100`.
    #[schemars(range(min = 0.0, max = 100.0))]
    pub opacity: f64,
    /// Bits per sample: 8, 16 or 32.
    #[schemars(extend("enum"=[8,16,32]))]
    pub depth: u8,
    /// Document raster width in pixels.
    pub width: u32,
    /// Document raster height in pixels.
    pub height: u32,
    pub tiles: Vec<TileRef>,
}

/// The tile sample format of a channel depth (`None` for any other depth).
pub const fn channel_tile_format(depth: u8) -> Option<&'static str> {
    match depth {
        8 => Some("coverage_u8"),
        16 => Some("coverage_u16le"),
        32 => Some("coverage_f32le"),
        _ => None,
    }
}

/// Bytes per sample of a channel depth (`None` for any other depth).
pub const fn channel_sample_bytes(depth: u8) -> Option<usize> {
    match depth {
        8 => Some(1),
        16 => Some(2),
        32 => Some(4),
        _ => None,
    }
}

/// A channel name Folio accepts: 1..=[`MAX_CHANNEL_NAME_BYTES`] bytes, no control character.
pub fn channel_name_ok(name: &str) -> bool {
    !name.is_empty() && name.len() <= MAX_CHANNEL_NAME_BYTES && !name.chars().any(char::is_control)
}

/// Integer pixel size of a tile side.
fn tile_side(l: &super::Length, target: &str) -> Result<u32> {
    finite_length(l, GeometryUnit::Px, true, target)?;
    let v = l.value;
    if !(v >= 1.0 && v <= f64::from(MAX_CHANNEL_SIDE) && v.fract() == 0.0) {
        return Err(err(Code::InvalidUnit, target));
    }
    // In range and integral: exact.
    Ok(v as u32)
}

/// Grid cells per side: `ceil(extent / edge)`.
fn cells(extent: u32, edge: u32) -> u64 {
    u64::from(extent).div_ceil(u64::from(edge.max(1)))
}

fn validate_tiles(c: &StudioChannelRecord, cx: &mut Ctx<'_>) -> Result<()> {
    let t = c.channel_id.as_str();
    let tiles_target = format!("{t}/tiles");
    let Some(first) = c.tiles.first() else {
        return Err(err(Code::InvalidField, tiles_target));
    };
    let tile_w = tile_side(&first.width, &tiles_target)?;
    let tile_h = tile_side(&first.height, &tiles_target)?;
    let (cols, rows) = (cells(c.width, tile_w), cells(c.height, tile_h));
    // The cell count is checked before any tile is examined (V4-02).
    if cols.checked_mul(rows) != u64::try_from(c.tiles.len()).ok() {
        return Err(err(Code::InvalidField, tiles_target));
    }
    cx.nodes(c.tiles.len(), &tiles_target)?;
    let format = channel_tile_format(c.depth).ok_or_else(|| err(Code::InvalidColourDepth, t))?;
    let mut keys: BTreeSet<&str> = BTreeSet::new();
    let mut seen: BTreeSet<(i64, i64)> = BTreeSet::new();
    for tile in &c.tiles {
        cx.tick()?;
        if tile.layer_id != c.channel_id || !key(&tile.object_key) || !keys.insert(&tile.object_key) {
            return Err(err(Code::PayloadMismatch, &tiles_target));
        }
        if tile_side(&tile.width, &tiles_target)? != tile_w || tile_side(&tile.height, &tiles_target)? != tile_h {
            return Err(err(Code::InvalidField, &tiles_target));
        }
        let in_grid = u64::try_from(tile.column).is_ok_and(|x| x < cols) && u64::try_from(tile.row).is_ok_and(|y| y < rows);
        if !in_grid || !seen.insert((tile.column, tile.row)) {
            return Err(err(Code::InvalidField, &tiles_target));
        }
        id(&tile.colour_profile_id, "SCPF", &tiles_target)?;
        if tile.format != format {
            return Err(err(Code::InvalidColourDepth, &tiles_target));
        }
        if tile.artifact_manifest_id.is_empty()
            || tile.content_digest.algorithm.is_empty()
            || tile.content_digest.digest.is_empty()
        {
            return Err(err(Code::InvalidField, &tiles_target));
        }
        let disposition = if cx.resolver.format_supported(&tile.format) {
            cx.resolver.tile(tile)
        } else {
            Resolution::Unsupported
        };
        if disposition != Resolution::Available {
            cx.issues.push(ResolutionIssue {
                target: format!("channel/{t}/tile/{}", tile.object_key),
                disposition,
            });
        }
    }
    Ok(())
}

/// Validates a channel list: count, ids, names, kinds, colour, opacity, depth, the shared extent
/// and full grid coverage of every channel. Refusals: [`Code::DuplicateIdentity`] (a second
/// `channel_id`), [`Code::InvalidId`] (an id outside the key grammar), [`Code::InvalidColourDepth`]
/// (depth not 8/16/32, or a tile format that does not match it), [`Code::InvalidField`] (name,
/// colour, opacity, extent, a channel whose extent differs from the first one's, missing,
/// repeated or out-of-grid cells, mixed tile sizes), [`Code::PayloadMismatch`] (a tile naming
/// another channel or a repeated object key), [`Code::BudgetExceeded`] (over [`MAX_CHANNELS`] or the
/// node budget). Unresolved tiles are [`ResolutionIssue`]s.
pub(crate) fn validate_channel_list(channels: &[StudioChannelRecord], cx: &mut Ctx<'_>) -> Result<()> {
    if channels.len() > MAX_CHANNELS {
        return Err(err(Code::BudgetExceeded, "channels"));
    }
    cx.nodes(channels.len(), "channels")?;
    let mut ids = BTreeSet::new();
    let extent = channels.first().map(|c| (c.width, c.height));
    for c in channels {
        cx.tick()?;
        let t = c.channel_id.as_str();
        if !key(t) {
            return Err(err(Code::InvalidId, "channels/channel_id"));
        }
        if !ids.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        if !channel_name_ok(&c.name) {
            return Err(err(Code::InvalidField, format!("{t}/name")));
        }
        if !c.colour.valid() {
            return Err(err(Code::InvalidField, format!("{t}/colour")));
        }
        if !(c.opacity.is_finite() && (0.0..=100.0).contains(&c.opacity)) {
            return Err(err(Code::InvalidField, format!("{t}/opacity")));
        }
        if channel_tile_format(c.depth).is_none() {
            return Err(err(Code::InvalidColourDepth, t));
        }
        let side = |v: u32| (1..=MAX_CHANNEL_SIDE).contains(&v);
        if !side(c.width) || !side(c.height) || Some((c.width, c.height)) != extent {
            return Err(err(Code::InvalidField, format!("{t}/extent")));
        }
        validate_tiles(c, cx)?;
    }
    Ok(())
}

/// A budget whose node allowance is exactly what [`validate_channels`] charges for `channels`
/// (one per channel and one per tile; the other dimensions are the defaults). Callers that built
/// the list themselves (Pigment's channel operations) validate with it.
pub fn channel_list_budget(channels: &[StudioChannelRecord]) -> super::Budget {
    let tiles = channels.iter().fold(0usize, |n, c| n.saturating_add(c.tiles.len()));
    super::Budget {
        nodes: channels.len().saturating_add(tiles),
        ..super::Budget::default()
    }
}

/// Validates a channel list on its own (no document needed: channels name no layer). The same
/// rules [`super::validate_raster_records`] applies to `StudioRasterRecords.channels`; Pigment's
/// channel operations run it on every record set they return.
pub fn validate_channels(
    channels: &[StudioChannelRecord],
    b: super::Budget,
    token: &hsk_studio_accord::CancellationToken,
    resolver: &dyn super::Resolver,
) -> Result<Vec<ResolutionIssue>> {
    let mut cx = Ctx::new(b, token, resolver);
    validate_channel_list(channels, &mut cx)?;
    Ok(cx.issues)
}
