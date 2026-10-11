//! Import proposal: maps a parsed [`PsdDocument`] to a Folio snapshot plus the data Folio does
//! not hold (pixels as Pigment tiles, PSD-only layer extras, loss report, provenance). The result
//! is a PROPOSAL; identity minting, publication of tiles and profile resolution stay with the
//! host.
//!
//! Pixel scope: RGB and grayscale documents at 8, 16 and 32 bits map to linear-light `rgb_f32le`
//! tiles at full precision (integer samples are converted in f64 and stored as f32; nothing is
//! quantized to 8 bits). With a [`IccConverter`] and an embedded profile the converter accepts, RGB
//! 8/16-bit pixels are converted through it into its working space; otherwise sRGB is assumed
//! (8/16-bit) or the floats are taken as already linear (32-bit), and the loss report says so.
//!
//! CMYK (8/16-bit) with an embedded ICC profile (res 1039), Lab (8/16-bit, PCS-native, no profile)
//! and gray (8/16-bit) with a `kTRC` profile are display-converted through the converter's wide
//! path into the same linear working RGB tiles. This is a PROJECTION: the layer channels in the
//! [`PsdDocument`] are not rewritten, an unedited export is byte-identical, and tile edits on a
//! projected layer are a typed loss at export (tests
//! `cmyk_display_conversion_is_a_clamped_projection_and_keeps_the_original_channels`,
//! `gray_ktrc_profile_converts_and_other_gray_cases_stay_typed`). The converter output is
//! unclamped; every clamp to `0..=1` is counted (`Provenance::display`) and reported as a loss
//! entry. CMYK without a usable profile stays an unmapped raster layer with a typed reason, and
//! no default CMYK profile is invented (test
//! `cmyk_without_a_usable_profile_is_a_typed_unmapped_layer_never_defaulted`). RGB and gray with an
//! embedded profile take the same wide path, so the intent and black point compensation reach the
//! converter (tests `gray_black_point_compensation_is_applied_when_the_profile_black_is_not_zero`,
//! `rgb_intent_and_black_point_compensation_reach_prism_or_are_recorded`). Opacity, fill opacity,
//! blend mode, clipping, group isolation and the mask reference are written to the Folio layer.
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, Inspection, Resolver, Snapshot, inspect_bytes};
use hsk_studio_pigment::Grid;
use hsk_studio_prism::{hash_hex, profile_hash};
use serde_json::{Value, json};

use crate::blend::StudioBlendMode;
use crate::convert::{
    ClipSummary, IccConverter, IccRefusal, ProfileDevice, ProfileFacts, RenderingIntent, WIDE_UNAVAILABLE, WideConvertRequest,
    WideConvertSource, refusal_code,
};
use crate::document::{PsdDocument, TreeNode};
use crate::error::{PsdError, Result};
use crate::header::ColorMode;
use crate::layers::{GroupRole, PsdLayer};
use crate::limits::Limits;
use crate::loss::{LossKind, LossReport, key_text};
use crate::pixels::Plane;
use crate::resources::{RESOURCE_ICC_PROFILE, RESOURCE_ICC_UNTAGGED};
use crate::samples::{SixteenBitScale, row_major, sample_range, size_add, size_mul, srgb_to_linear, write_f32_le};

/// Pigment tile edge in pixels (columns and rows are cells of a canvas-origin grid).
pub const TILE_EDGE: u32 = 256;
/// Pixels in one grid cell.
pub(crate) const CELL_PIXELS: usize = (TILE_EDGE as usize) * (TILE_EDGE as usize);
/// Bytes of the colour plane of one tile (`rgb_f32le`, 12 bytes per pixel).
pub(crate) const COLOUR_BYTES: usize = CELL_PIXELS * 12;
/// Bytes of the straight `f32le` alpha plane of one tile.
pub(crate) const ALPHA_BYTES: usize = CELL_PIXELS * 4;
/// Bytes of one whole tile (colour plane plus alpha plane).
const TILE_BYTES: usize = COLOUR_BYTES + ALPHA_BYTES;
const TILE_FORMAT: &str = "rgb_f32le";
pub(crate) const MASK_FORMAT_U8: &str = "coverage_u8";
pub(crate) const MASK_FORMAT_U16: &str = "coverage_u16le";
const MAX_EMBEDDED_OPAQUE_BYTES: usize = 65_536;

const ADJUSTMENT_KEYS: [&[u8; 4]; 17] = [
    b"levl", b"curv", b"brit", b"blnc", b"blwh", b"hue ", b"hue2", b"selc", b"mixr", b"grdm", b"phfl", b"expA",
    b"thrs", b"nvrt", b"post", b"vibA", b"clrL",
];
const FILL_KEYS: [&[u8; 4]; 3] = [b"SoCo", b"GdFl", b"PtFl"];
const CACHED_PIXEL_KEYS: [&[u8; 4]; 8] = [b"TySh", b"tySh", b"SoLd", b"SoLE", b"PlLd", b"plLd", b"lfx2", b"lrFX"];

/// Host-owned identity and handle minting. Every identity in a proposal comes from this trait.
pub trait IdMint {
    /// `SDOC-...`
    fn document_id(&mut self) -> String;
    /// `SART-...`
    fn artboard_id(&mut self) -> String;
    /// `SLYR-...`
    fn layer_id(&mut self) -> String;
    /// `SCPF-...`: the profile the tiles are expressed in when no converter applies (linear
    /// light, sRGB primaries).
    fn linear_working_profile_id(&mut self) -> String;
    /// Non-empty handle naming where the host will publish this tile's bytes.
    fn tile_manifest_id(&mut self, layer_id: &str, object_key: &str) -> String;
}

/// Identity of the source bytes the document was parsed from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceIdentity {
    pub sha256: [u8; 32],
    pub byte_len: usize,
}

impl SourceIdentity {
    pub fn of(bytes: &[u8]) -> Self {
        Self { sha256: profile_hash(bytes), byte_len: bytes.len() }
    }
}

pub struct ProposalConfig<'a> {
    pub limits: &'a Limits,
    pub budget: Budget,
    pub resolver: &'a dyn Resolver,
    /// Interpretation of 16-bit samples (UNVERIFIED which one Photoshop uses; default full range).
    pub scale: SixteenBitScale,
    /// Rendering intent for CMYK/Lab/gray display conversion. No PSD image resource carries a
    /// display intent (Adobe file-format resource table checked 2026-10-11: 0x043A Print
    /// Information is a print descriptor, not parsed here), so `None` means perceptual and the
    /// loss report records that as an assumption.
    pub rendering_intent: Option<RenderingIntent>,
    /// Black point compensation for display conversion. `None` means on (Photoshop's Color
    /// Settings default) and is recorded as an assumption.
    pub black_point_compensation: Option<bool>,
}

impl<'a> ProposalConfig<'a> {
    pub fn new(limits: &'a Limits, budget: Budget, resolver: &'a dyn Resolver) -> Self {
        Self {
            limits,
            budget,
            resolver,
            scale: SixteenBitScale::default(),
            rendering_intent: None,
            black_point_compensation: None,
        }
    }
}

/// Linear-light overshoot beyond `0..=1` above which a clamp is reported as a gamut loss. It matches
/// Prism's clip tolerance: smaller excursions are rounding noise (16.16 colorants, f32), are still
/// clamped and counted, and are reported under their own code.
pub const GAMUT_CLAMP_TOLERANCE: f32 = 1e-3;

/// One tile's pixels: a linear-light `rgb_f32le` colour plane and a separate straight `f32le`
/// alpha plane (the Pigment tile contract), one full grid cell.
#[derive(Clone, Debug, PartialEq)]
pub struct TileBlob {
    pub layer_id: String,
    pub object_key: String,
    pub column: i64,
    pub row: i64,
    pub colour: Vec<u8>,
    pub alpha: Vec<u8>,
    pub colour_sha256_hex: String,
    pub alpha_sha256_hex: String,
    /// Digest recorded in the Folio `TileRef`: SHA-256 over `colour || alpha`.
    pub content_sha256_hex: String,
}

/// One mask coverage tile, one full grid cell: `coverage_u8` for 8-bit documents, otherwise
/// `coverage_u16le` (16-bit samples, and 32-bit floats quantized to 16 bits).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaskTileBlob {
    pub layer_id: String,
    pub object_key: String,
    pub column: i64,
    pub row: i64,
    /// `coverage_u8` or `coverage_u16le`.
    pub format: &'static str,
    pub coverage: Vec<u8>,
    /// Digest recorded in the Folio mask `TileRef`: SHA-256 over `coverage`.
    pub sha256_hex: String,
}

/// PSD-side properties Folio still has no field for. Opacity, fill opacity, blend mode, clipping,
/// group isolation and the mask reference are written to the Folio layer itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerExtras {
    pub layer_id: String,
    pub psd_index: usize,
    /// The blend key exactly as stored (byte-preserved; unknown keys stay readable here).
    pub blend_key: [u8; 4],
    /// Blend key carried by `lsct` on a group record, when present.
    pub group_blend_key: Option<[u8; 4]>,
    pub blend_clipped_elements: Option<bool>,
    pub blend_interior_elements: Option<bool>,
    pub knockout: Option<bool>,
    /// Coverage outside the mask rectangle (0 or 255); Folio `StudioMask` has no such field.
    pub mask_default_color: Option<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerMapEntry {
    pub layer_id: String,
    pub psd_index: usize,
}

/// What a CMYK, Lab or gray (profile) display conversion did. The layer channels stay as read; the
/// tiles are this projection of them.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayConversion {
    /// `cmyk`, `lab` or `gray`.
    pub source_space: &'static str,
    pub intent: RenderingIntent,
    /// No intent was configured: perceptual was assumed.
    pub intent_assumed: bool,
    pub black_point_compensation: bool,
    /// No setting was configured: black point compensation on was assumed.
    pub black_point_compensation_assumed: bool,
    /// Pixels with alpha above zero that went through the converter.
    pub pixels_converted: u64,
    /// Of those, pixels with any channel clamped to `0..=1` (any amount).
    pub pixels_clamped: u64,
    /// Of those, pixels whose overshoot exceeded [`GAMUT_CLAMP_TOLERANCE`] (a gamut clip).
    pub pixels_out_of_gamut: u64,
    /// Clamped channel values per working-RGB channel.
    pub channels_clamped: [u64; 3],
    /// Largest linear-light overshoot that was clamped.
    pub max_overshoot: f64,
    /// Input samples left their valid range (16-bit scale 32768 can exceed 1.0) and were clamped.
    pub input_range_clamped: bool,
    /// The converter's own clip report over every converted pixel (transparent ones included).
    pub converter_clip: ClipSummary,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Provenance {
    pub source_sha256_hex: String,
    pub byte_len: usize,
    pub psd_version: u16,
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    /// sRGB (or, at 32 bits, linear light) was assumed: no profile conversion was applied.
    pub assumed_profile: bool,
    /// The embedded ICC profile was applied through the converter (tiles are in its working space).
    pub icc_converted: bool,
    pub icc_sha256_hex: Option<String>,
    pub sixteen_bit_scale: SixteenBitScale,
    /// Present when CMYK, Lab or gray pixels were display-converted (the tiles are a projection).
    pub display: Option<DisplayConversion>,
}

#[derive(Clone, Debug)]
pub struct ImportProposal {
    pub snapshot: Snapshot,
    pub tiles: Vec<TileBlob>,
    pub mask_tiles: Vec<MaskTileBlob>,
    pub extras: Vec<LayerExtras>,
    pub layer_map: Vec<LayerMapEntry>,
    pub loss: LossReport,
    pub provenance: Provenance,
}

impl ImportProposal {
    /// Copies names and visibility from an edited snapshot back onto the matching PSD layers and
    /// returns how many layers changed. Nothing else is touched; see `export` for the full set of
    /// layer properties.
    pub fn apply_to_document(&self, doc: &mut PsdDocument, edited: &Snapshot) -> usize {
        let Some(info) = doc.layer_section.layer_info.as_mut() else {
            return 0;
        };
        let mut changed = 0;
        for entry in &self.layer_map {
            let Some(source) = edited.document().layers.iter().find(|l| l.layer_id == entry.layer_id) else {
                continue;
            };
            let Some(layer) = info.layers.get_mut(entry.psd_index) else {
                continue;
            };
            let hidden = !source.visible;
            if layer.name != source.name || layer.hidden() != hidden {
                layer.name.clone_from(&source.name);
                layer.flags = if hidden { layer.flags | 0x02 } else { layer.flags & !0x02 };
                changed = usize::saturating_add(changed, 1);
            }
        }
        changed
    }
}

fn studio_colour_mode(mode: ColorMode) -> u8 {
    match mode {
        ColorMode::Grayscale => 1,
        ColorMode::Rgb => 2,
        ColorMode::Cmyk => 3,
        ColorMode::Lab => 4,
        ColorMode::Bitmap => 5,
        ColorMode::Indexed => 6,
        ColorMode::Multichannel => 7,
        ColorMode::Duotone => 8,
    }
}

fn px(value: f64) -> Value {
    json!({"value": value, "unit": "px"})
}

fn port(key: &str, cardinality: &str, semantic: &str) -> Value {
    json!({"port_key": key, "value_type": "image", "cardinality": cardinality, "semantic": semantic})
}

fn address(layer_id: &str, operation: &str, port: &str) -> Value {
    json!({"layer_id": layer_id, "operation_key": operation, "port_key": port})
}

pub(crate) fn floor_div(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

/// Percent with one decimal (STU-RAS-006 precision) from a PSD 0..=255 byte; the inverse rounding
/// returns the same byte.
pub(crate) fn percent_of_byte(byte: u8) -> f64 {
    (f64::from(byte) * 100.0 / 255.0 * 10.0).round() / 10.0
}

/// The appearance values the importer derives from a PSD layer record (also what the exporter
/// compares edits against, so unchanged values are not rewritten; test
/// `export_of_an_edited_folio_document_round_trips_supported_fields_and_preserves_the_rest`).
pub(crate) struct ImportedLook {
    pub opacity: f64,
    pub fill_opacity: f64,
    pub blend: u16,
    pub clipped: bool,
    pub isolation: &'static str,
}

pub(crate) fn imported_look(layer: &PsdLayer, is_group: bool) -> ImportedLook {
    let (blend, isolation) = match (is_group, layer.blend_mode()) {
        (true, Some(StudioBlendMode::PassThrough)) => (1u16, "pass_through"),
        (_, Some(mode)) => (mode.discriminant(), "isolated"),
        (_, None) => (2u16, "isolated"),
    };
    ImportedLook {
        opacity: percent_of_byte(layer.opacity),
        fill_opacity: percent_of_byte(layer.fill_opacity().unwrap_or(255)),
        blend,
        clipped: layer.clipping == 1,
        isolation,
    }
}

/// Source of a wide display conversion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WideKind {
    Cmyk,
    Lab,
    Gray,
    Rgb,
}

impl WideKind {
    fn channels(self) -> usize {
        match self {
            Self::Cmyk => 4,
            Self::Lab | Self::Rgb => 3,
            Self::Gray => 1,
        }
    }

    fn code(self) -> &'static str {
        match self {
            Self::Cmyk => "cmyk",
            Self::Lab => "lab",
            Self::Gray => "gray",
            Self::Rgb => "rgb",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct WideRoute {
    kind: WideKind,
    intent: RenderingIntent,
    bpc: bool,
}

#[derive(Default)]
struct DisplayStats {
    calls: u64,
    pixels_converted: u64,
    pixels_clamped: u64,
    pixels_out_of_gamut: u64,
    channels_clamped: [u64; 3],
    max_overshoot: f64,
    input_range_clamped: bool,
    clip: ClipSummary,
}

struct Builder<'a, 'm, 'c> {
    doc: &'a PsdDocument,
    cfg: &'a ProposalConfig<'a>,
    mint: &'m mut dyn IdMint,
    converter: Option<&'c mut (dyn IccConverter + 'c)>,
    cancel: &'a CancellationToken,
    profile_id: String,
    pixels_mapped: bool,
    unmapped_reason: &'static str,
    icc_active: bool,
    wide: Option<WideRoute>,
    stats: DisplayStats,
    icc_range_clamped: bool,
    /// A NaN or infinite sample was replaced by 0 (typed loss `non_finite_samples_zeroed`).
    non_finite_samples: bool,
    /// An alpha sample outside `0..=1` was clamped (typed loss `alpha_range_clamped`).
    alpha_clamped: bool,
    layers: Vec<Value>,
    operations: Vec<Value>,
    edges: Vec<Value>,
    tiles: Vec<TileBlob>,
    mask_tiles: Vec<MaskTileBlob>,
    extras: Vec<LayerExtras>,
    map: Vec<LayerMapEntry>,
    loss: LossReport,
    tile_bytes: usize,
}

impl Builder<'_, '_, '_> {
    fn note(&mut self, path: String, kind: LossKind, code: &'static str) {
        self.loss.entries.push(crate::loss::LossEntry { path, kind, code });
    }

    fn record(&mut self, layer_id: &str, index: usize, layer: &PsdLayer, mask_default: Option<u8>) {
        self.extras.push(LayerExtras {
            layer_id: layer_id.to_owned(),
            psd_index: index,
            blend_key: layer.blend_key,
            group_blend_key: layer.group_blend_key(),
            blend_clipped_elements: layer.blend_clipped_elements(),
            blend_interior_elements: layer.blend_interior_elements(),
            knockout: layer.knockout(),
            mask_default_color: mask_default,
        });
        self.map.push(LayerMapEntry { layer_id: layer_id.to_owned(), psd_index: index });
    }

    /// The Folio `appearance` object. A leaf with an unknown or `pass` key does not reach here
    /// (those layers are unsupported payloads; test `unknown_blend_and_unmapped_fields_are_typed_losses`);
    /// an unknown key on a group record keeps the
    /// structural placeholder `isolated`/normal, with the typed loss entry and the raw key in the
    /// extras.
    fn appearance_json(layer: &PsdLayer, is_group: bool, mask: Option<Value>) -> Value {
        let look = imported_look(layer, is_group);
        let mut a = json!({
            "opacity": look.opacity,
            "fill_opacity": look.fill_opacity,
            "blend_mode": look.blend,
            "clipped": look.clipped,
            "isolation": look.isolation,
        });
        if let Some(mask) = mask {
            a["mask"] = mask;
        }
        a
    }

    /// Walks nodes in panel order; returns the output ports of the nodes that composite, in the
    /// same order.
    fn walk(&mut self, nodes: &[TreeNode], parent: &Value) -> Result<Vec<Value>> {
        let mut outputs = Vec::new();
        for node in nodes {
            self.cancel.check()?;
            match node {
                TreeNode::Layer { index } => {
                    if let Some(out) = self.leaf(*index, parent)? {
                        outputs.push(out);
                    }
                }
                TreeNode::Group { folder, children, .. } => outputs.push(self.group(*folder, children, parent)?),
            }
        }
        Ok(outputs)
    }

    fn group(&mut self, folder: usize, children: &[TreeNode], parent: &Value) -> Result<Value> {
        let doc = self.doc;
        let layer = &doc.layers()[folder];
        let id = self.mint.layer_id();
        let mask = self.mask_for(folder, layer, &id)?;
        let mask_default = layer.mask().map(|m| m.default_color);
        let appearance = Self::appearance_json(layer, true, mask);
        self.layers.push(json!({
            "schema_id": "hsk.studio.layer@1", "layer_id": id, "name": layer.name, "parent": parent,
            "kind": "group", "payload": {"payload_kind": "group"}, "visible": layer.visible(),
            "appearance": appearance,
        }));
        self.record(&id, folder, layer, mask_default);
        let inner_parent = json!({"node_kind": "layer", "node_id": id});
        let child_outputs = self.walk(children, &inner_parent)?;
        self.operations.push(json!({
            "layer_id": id, "operation_key": "mix", "operation_kind": "composite",
            "inputs": [port("inputs", "many", "ordered_composite")],
            "outputs": [port("out", "one", "composite_result")],
        }));
        // Ordinal 0 is the bottom-most child (first composited); panel order is top-first.
        for (ordinal, source) in child_outputs.iter().rev().enumerate() {
            self.edges.push(json!({
                "source": source, "target": address(&id, "mix", "inputs"),
                "dependency_kind": "data", "input_ordinal": ordinal,
            }));
        }
        Ok(address(&id, "mix", "out"))
    }

    fn leaf(&mut self, index: usize, parent: &Value) -> Result<Option<Value>> {
        let doc = self.doc;
        let layer = &doc.layers()[index];
        let role = layer.group_role();
        if role == GroupRole::BoundingDivider {
            return Ok(None);
        }
        let id = self.mint.layer_id();
        let adjustment = layer.blocks.iter().find(|b| ADJUSTMENT_KEYS.contains(&&b.key));
        let fill = layer.blocks.iter().find(|b| FILL_KEYS.contains(&&b.key));
        if let Some(block) = adjustment.or(fill) {
            let (kind, code) = if adjustment.is_some() {
                ("psd_adjustment", "adjustment_layer_unsupported_payload")
            } else {
                ("psd_fill", "fill_layer_unsupported_payload")
            };
            let embedded = if block.data.len() <= MAX_EMBEDDED_OPAQUE_BYTES { block.data.clone() } else { Vec::new() };
            if embedded.is_empty() && !block.data.is_empty() {
                self.note(format!("layer[{index}]"), LossKind::PreservedOpaque, "opaque_payload_not_embedded");
            }
            self.layers.push(json!({
                "schema_id": "hsk.studio.layer@1", "layer_id": id, "name": layer.name, "parent": parent,
                "kind": kind,
                "payload": {"payload_kind": "unsupported", "kind": kind,
                    "schema_id": format!("psd.tagged.{}", key_text(block.key)),
                    "encoded_bytes": embedded, "reason": code},
                "visible": layer.visible(),
            }));
            self.note(format!("layer[{index}]"), LossKind::Unsupported, code);
            self.record(&id, index, layer, None);
            return Ok(None);
        }
        if layer.blocks.iter().any(|b| CACHED_PIXEL_KEYS.contains(&&b.key)) {
            self.note(format!("layer[{index}]/pixels"), LossKind::Rasterized, "cached_pixels_display_truth");
        }
        // A leaf whose blend key is unknown, or `pass` (a group property), has no Studio mode: it
        // becomes an explicit unsupported payload instead of a silent Normal.
        let blend_problem = match layer.blend_mode() {
            None => Some("blend_mode_unknown"),
            Some(StudioBlendMode::PassThrough) => Some("pass_through_on_leaf_layer"),
            Some(_) => None,
        };
        if !self.pixels_mapped || blend_problem.is_some() {
            let reason = blend_problem.unwrap_or(self.unmapped_reason);
            self.layers.push(json!({
                "schema_id": "hsk.studio.layer@1", "layer_id": id, "name": layer.name, "parent": parent,
                "kind": "psd_raster_unmapped",
                "payload": {"payload_kind": "unsupported", "kind": "psd_raster_unmapped",
                    "schema_id": "psd.raster", "encoded_bytes": Vec::<u8>::new(), "reason": reason},
                "visible": layer.visible(),
            }));
            self.note(format!("layer[{index}]/pixels"), LossKind::Unsupported, "raster_pixels_unmapped");
            self.record(&id, index, layer, None);
            return Ok(None);
        }
        let tiles = self.tiles_for(index, layer, &id)?;
        let mask = self.mask_for(index, layer, &id)?;
        let mask_default = layer.mask().map(|m| m.default_color);
        let appearance = Self::appearance_json(layer, false, mask);
        self.layers.push(json!({
            "schema_id": "hsk.studio.layer@1", "layer_id": id, "name": layer.name, "parent": parent,
            "kind": "raster", "payload": {"payload_kind": "raster", "tiles": tiles}, "visible": layer.visible(),
            "appearance": appearance,
        }));
        self.operations.push(json!({
            "layer_id": id, "operation_key": "source", "operation_kind": "source",
            "inputs": [], "outputs": [port("out", "one", "source")],
        }));
        self.record(&id, index, layer, mask_default);
        Ok(Some(address(&id, "source", "out")))
    }

    /// Folio `StudioMask` for the layer's user mask (the real user mask when both a user and a
    /// vector mask exist): coverage tiles over the layer rectangle (the mask rectangle for a layer
    /// without pixels), `default_color` outside the mask rectangle. Flags: bit 1 disabled; bit 0
    /// ("position relative to layer") is read as linked (UNVERIFIED against Photoshop). 8-bit
    /// documents use `coverage_u8`; 16-bit samples and 32-bit floats use `coverage_u16le`.
    fn mask_for(&mut self, index: usize, layer: &PsdLayer, layer_id: &str) -> Result<Option<Value>> {
        if !self.pixels_mapped {
            return Ok(None);
        }
        let Some(mask) = layer.mask() else {
            return Ok(None);
        };
        let header = &self.doc.header;
        let depth = header.depth;
        let scale = self.cfg.scale;
        let real = layer.channels.iter().any(|c| c.id == -3) && mask.real_rect.is_some();
        let channel_id: i16 = if real { -3 } else { -2 };
        let (rect, default, flags) = if real {
            (
                mask.real_rect.unwrap_or(mask.rect),
                mask.real_default_color.unwrap_or(mask.default_color),
                mask.real_flags.unwrap_or(mask.flags),
            )
        } else {
            (mask.rect, mask.default_color, mask.flags)
        };
        if flags & 0x10 != 0 {
            self.note(format!("layer[{index}]/mask"), LossKind::PreservedOpaque, "mask_density_feather_not_stored");
        }
        let enabled = flags & 0x02 == 0;
        let linked = flags & 0x01 != 0;
        let Some(plane) = layer.decode_channel(channel_id, header, self.cfg.limits, self.cancel)? else {
            self.note(format!("layer[{index}]/mask"), LossKind::Unsupported, "mask_channel_missing");
            return Ok(None);
        };
        let wide = depth != 8;
        let format = if wide { MASK_FORMAT_U16 } else { MASK_FORMAT_U8 };
        if depth == 32 {
            self.note(format!("layer[{index}]/mask"), LossKind::Transformed, "mask_float_quantized_u16");
        }
        let region = if layer.rect.area() > 0 { layer.rect } else { rect };
        let mut refs = Vec::new();
        if region.area() > 0 {
            let grid = Grid { origin_x: 0, origin_y: 0, tile_width: TILE_EDGE, tile_height: TILE_EDGE };
            let edge = i64::from(TILE_EDGE);
            let (left, top, right, bottom) =
                (i64::from(region.left), i64::from(region.top), i64::from(region.right), i64::from(region.bottom));
            let (mleft, mtop) = (i64::from(rect.left), i64::from(rect.top));
            let (mright, mbottom) = (i64::from(rect.right), i64::from(rect.bottom));
            let mwidth = rect.width() as usize;
            let default16 = u16::from(default).saturating_mul(257);
            for row in floor_div(top, edge)..=floor_div(bottom.saturating_sub(1), edge) {
                for column in floor_div(left, edge)..=floor_div(right.saturating_sub(1), edge) {
                    self.cancel.check()?;
                    let bounds = grid.bounds(column, row).map_err(|_| PsdError::Overflow)?;
                    let x0 = bounds.x.max(left);
                    let x1 = bounds.x.saturating_add(i64::from(bounds.width)).min(right);
                    let y0 = bounds.y.max(top);
                    let y1 = bounds.y.saturating_add(i64::from(bounds.height)).min(bottom);
                    let mut coverage = vec![0u8; if wide { size_mul(CELL_PIXELS, 2)? } else { CELL_PIXELS }];
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let inside = x >= mleft && x < mright && y >= mtop && y < mbottom;
                            let dst = row_major(y.saturating_sub(bounds.y), x.saturating_sub(bounds.x), TILE_EDGE as usize)?;
                            // The mask sample of this pixel, when it lies inside the mask rectangle.
                            let source = if inside {
                                Some(row_major(y.saturating_sub(mtop), x.saturating_sub(mleft), mwidth)?)
                            } else {
                                None
                            };
                            if wide {
                                let value = match source {
                                    Some(at) => {
                                        let unit = plane.unit_at(at, scale).unwrap_or(0.0);
                                        (f64::from(unit).clamp(0.0, 1.0) * 65535.0).round() as u16
                                    }
                                    None => default16,
                                };
                                coverage
                                    .get_mut(sample_range(dst, 2)?)
                                    .ok_or(PsdError::Overflow)?
                                    .copy_from_slice(&value.to_le_bytes());
                            } else {
                                let value = match source {
                                    Some(at) => *plane.bytes.get(at).ok_or(PsdError::ChannelSize)?,
                                    None => default,
                                };
                                *coverage.get_mut(dst).ok_or(PsdError::Overflow)? = value;
                            }
                        }
                    }
                    self.tile_bytes = self.tile_bytes.saturating_add(coverage.len());
                    if self.tile_bytes > self.cfg.limits.max_decoded_bytes {
                        return Err(PsdError::PixelLimit);
                    }
                    let object_key = format!("m_{column}_{row}");
                    let digest = hash_hex(&profile_hash(&coverage));
                    refs.push(json!({
                        "object_key": object_key, "layer_id": layer_id, "column": column, "row": row,
                        "width": px(f64::from(TILE_EDGE)), "height": px(f64::from(TILE_EDGE)),
                        "format": format, "colour_profile_id": self.profile_id,
                        "schema_id": "hsk.studio.raster_tile@1",
                        "content_digest": {"algorithm": "sha256", "digest": digest},
                        "artifact_manifest_id": self.mint.tile_manifest_id(layer_id, &object_key),
                    }));
                    self.mask_tiles.push(MaskTileBlob {
                        layer_id: layer_id.to_owned(),
                        object_key,
                        column,
                        row,
                        format,
                        coverage,
                        sha256_hex: digest,
                    });
                }
            }
        }
        Ok(Some(json!({"tiles": refs, "enabled": enabled, "linked": linked})))
    }

    /// Display-converts one batch of document samples (`raw`: unit samples in channel order) to
    /// linear working RGB. The input mapping is explicit and recorded as loss entries:
    /// CMYK ink = `1 - stored` (PSD stores CMYK inverted, 255 = no ink; UNVERIFIED against a
    /// Photoshop fixture), Lab `L = unit * 100`, `a`/`b = unit * 255 - 128` (8-bit ICC legacy
    /// encoding; 16-bit keeps the same mapping per scale, `* 256` for the 32768 scale so the
    /// neutral axis sits at raw 16384; UNVERIFIED), gray as stored. The converter output is
    /// clamped to `0..=1` here and every clamp is counted for alpha-visible pixels.
    fn convert_wide_pixels(&mut self, route: WideRoute, raw: &[[f32; 4]], alphas: &[f32]) -> Result<Vec<[f32; 3]>> {
        let doc = self.doc;
        let header = &doc.header;
        let mut ranged = false;
        let mut limit = |v: f32, lo: f32, hi: f32| {
            if (lo..=hi).contains(&v) {
                v
            } else {
                ranged = true;
                v.clamp(lo, hi)
            }
        };
        let ab_scale = if header.depth == 16 && self.cfg.scale == SixteenBitScale::Photoshop32768 { 256.0 } else { 255.0 };
        let mut input: Vec<f32> = Vec::with_capacity(size_mul(raw.len(), route.kind.channels())?);
        for p in raw {
            match route.kind {
                WideKind::Cmyk => {
                    for &stored in p {
                        input.push(limit(1.0 - stored, 0.0, 1.0));
                    }
                }
                WideKind::Gray => input.push(limit(p[0], 0.0, 1.0)),
                WideKind::Rgb => {
                    for &v in &p[..3] {
                        input.push(limit(v, 0.0, 1.0));
                    }
                }
                WideKind::Lab => {
                    input.push(limit((f64::from(p[0]) * 100.0) as f32, 0.0, 100.0));
                    for &unit in &p[1..3] {
                        input.push(limit((f64::from(unit) * ab_scale - 128.0) as f32, -128.0, 128.0));
                    }
                }
            }
        }
        self.stats.input_range_clamped |= ranged;
        let source = match route.kind {
            WideKind::Lab => WideConvertSource::Lab,
            _ => WideConvertSource::Profile(doc.resources.icc_profile().unwrap_or_default()),
        };
        let converter = self.converter.as_deref_mut().ok_or(PsdError::Proposal("icc_converter_missing"))?;
        let request = WideConvertRequest {
            source,
            pixels: &input,
            intent: route.intent,
            black_point_compensation: route.bpc,
        };
        let converted = converter.convert_wide(&request, self.cancel).map_err(refusal_error)?;
        if converted.pixels.len() != raw.len() {
            return Err(PsdError::Proposal("icc_conversion_failed"));
        }
        let stats = &mut self.stats;
        stats.clip.absorb(&converted.clip, stats.calls == 0);
        stats.calls = stats.calls.saturating_add(1);
        let mut out = converted.pixels;
        for (px, &alpha) in out.iter_mut().zip(alphas) {
            let visible = alpha > 0.0;
            let mut hit = false;
            let mut worst = 0.0f32;
            for (k, v) in px.iter_mut().enumerate() {
                if !v.is_finite() {
                    return Err(PsdError::Proposal("icc_conversion_failed"));
                }
                let over = if *v < 0.0 {
                    -*v
                } else if *v > 1.0 {
                    *v - 1.0
                } else {
                    continue;
                };
                *v = v.clamp(0.0, 1.0);
                if visible {
                    hit = true;
                    worst = worst.max(over);
                    stats.channels_clamped[k] = stats.channels_clamped[k].saturating_add(1);
                }
            }
            if visible {
                stats.pixels_converted = stats.pixels_converted.saturating_add(1);
                if hit {
                    stats.pixels_clamped = stats.pixels_clamped.saturating_add(1);
                    stats.pixels_out_of_gamut =
                        stats.pixels_out_of_gamut.saturating_add(u64::from(worst > GAMUT_CLAMP_TOLERANCE));
                    stats.max_overshoot = stats.max_overshoot.max(f64::from(worst));
                }
            }
        }
        Ok(out)
    }

    fn tiles_for(&mut self, index: usize, layer: &PsdLayer, layer_id: &str) -> Result<Vec<Value>> {
        let rect = layer.rect;
        if rect.area() == 0 || layer.pixel_data_irrelevant() {
            return Ok(Vec::new());
        }
        let doc = self.doc;
        let header = &doc.header;
        let depth = header.depth;
        let scale = self.cfg.scale;
        let colour_ids: &[i16] = match header.color_mode {
            ColorMode::Rgb | ColorMode::Lab => &[0, 1, 2],
            ColorMode::Cmyk => &[0, 1, 2, 3],
            _ => &[0],
        };
        let wide = self.wide;
        let mut planes = Vec::with_capacity(colour_ids.len());
        for &id in colour_ids {
            match layer.decode_channel(id, header, self.cfg.limits, self.cancel)? {
                Some(p) => planes.push(p),
                None => {
                    self.note(format!("layer[{index}]/channels"), LossKind::Unsupported, "colour_channel_missing");
                    return Ok(Vec::new());
                }
            }
        }
        let alpha = layer.decode_channel(-1, header, self.cfg.limits, self.cancel)?;
        let width = rect.width() as usize;
        let grid = Grid { origin_x: 0, origin_y: 0, tile_width: TILE_EDGE, tile_height: TILE_EDGE };
        let edge = i64::from(TILE_EDGE);
        let (left, top) = (i64::from(rect.left), i64::from(rect.top));
        let (right, bottom) = (i64::from(rect.right), i64::from(rect.bottom));
        let mut refs = Vec::new();
        for row in floor_div(top, edge)..=floor_div(bottom.saturating_sub(1), edge) {
            for column in floor_div(left, edge)..=floor_div(right.saturating_sub(1), edge) {
                self.cancel.check()?;
                let bounds = grid.bounds(column, row).map_err(|_| PsdError::Overflow)?;
                let x0 = bounds.x.max(left);
                let x1 = bounds.x.saturating_add(i64::from(bounds.width)).min(right);
                let y0 = bounds.y.max(top);
                let y1 = bounds.y.saturating_add(i64::from(bounds.height)).min(bottom);
                let mut raw: Vec<[f32; 4]> = Vec::new();
                let mut alphas: Vec<f32> = Vec::new();
                let mut destinations: Vec<usize> = Vec::new();
                for y in y0..y1 {
                    for x in x0..x1 {
                        let src = row_major(y.saturating_sub(top), x.saturating_sub(left), width)?;
                        let channel = |p: &Plane| p.unit_at(src, scale).ok_or(PsdError::ChannelSize);
                        let mut samples = [0.0f32; 4];
                        for (slot, plane) in samples.iter_mut().zip(&planes) {
                            let v = channel(plane)?;
                            // Only 32-bit float samples can be NaN or infinite; a non-finite value is
                            // replaced before the tile (it would poison every blend downstream).
                            *slot = if v.is_finite() {
                                v
                            } else {
                                self.non_finite_samples = true;
                                0.0
                            };
                        }
                        raw.push(samples);
                        let a = match &alpha {
                            Some(p) => channel(p)?,
                            None => 1.0,
                        };
                        let a = if a.is_finite() {
                            a
                        } else {
                            self.non_finite_samples = true;
                            0.0
                        };
                        if !(0.0..=1.0).contains(&a) {
                            // Alpha above 1.0 (the 32768 scale, float alpha) or below 0.0 is clamped.
                            self.alpha_clamped = true;
                        }
                        alphas.push(a.clamp(0.0, 1.0));
                        destinations.push(row_major(
                            y.saturating_sub(bounds.y),
                            x.saturating_sub(bounds.x),
                            TILE_EDGE as usize,
                        )?);
                    }
                }
                if !alphas.iter().any(|&a| a > 0.0) {
                    continue;
                }
                let linear: Vec<[f32; 3]> = if let Some(route) = wide {
                    self.convert_wide_pixels(route, &raw, &alphas)?
                } else {
                    let encoded: Vec<[f32; 3]> = raw
                        .iter()
                        .map(|p| if planes.len() == 1 { [p[0]; 3] } else { [p[0], p[1], p[2]] })
                        .collect();
                    if self.icc_active {
                        let mut ranged = encoded;
                        for px in &mut ranged {
                            for v in px.iter_mut() {
                                if !(0.0..=1.0).contains(v) {
                                    self.icc_range_clamped = true;
                                    *v = v.clamp(0.0, 1.0);
                                }
                            }
                        }
                        let icc = doc.resources.icc_profile().unwrap_or_default();
                        let converter =
                            self.converter.as_deref_mut().ok_or(PsdError::Proposal("icc_converter_missing"))?;
                        let converted = converter.convert_rgb(icc, &ranged, self.cancel).map_err(refusal_error)?;
                        // A short or long answer would leave pixels unconverted without a trace.
                        if converted.len() != ranged.len() {
                            return Err(PsdError::Proposal("icc_conversion_failed"));
                        }
                        converted
                    } else if depth == 32 {
                        encoded
                    } else {
                        encoded.into_iter().map(|p| p.map(srgb_to_linear)).collect()
                    }
                };
                let mut colour = vec![0u8; COLOUR_BYTES];
                let mut alpha_plane = vec![0u8; ALPHA_BYTES];
                for ((dst, px), a) in destinations.iter().zip(&linear).zip(&alphas) {
                    let base = size_mul(*dst, 3)?;
                    for (k, v) in px.iter().enumerate() {
                        write_f32_le(&mut colour, size_add(base, k)?, *v)?;
                    }
                    write_f32_le(&mut alpha_plane, *dst, *a)?;
                }
                self.tile_bytes = self.tile_bytes.saturating_add(TILE_BYTES);
                if self.tile_bytes > self.cfg.limits.max_decoded_bytes {
                    return Err(PsdError::PixelLimit);
                }
                let object_key = format!("t_{column}_{row}");
                let mut both = colour.clone();
                both.extend_from_slice(&alpha_plane);
                let content = hash_hex(&profile_hash(&both));
                refs.push(json!({
                    "object_key": object_key, "layer_id": layer_id, "column": column, "row": row,
                    "width": px(f64::from(TILE_EDGE)), "height": px(f64::from(TILE_EDGE)),
                    "format": TILE_FORMAT, "colour_profile_id": self.profile_id,
                    "schema_id": "hsk.studio.raster_tile@1",
                    "content_digest": {"algorithm": "sha256", "digest": content},
                    "artifact_manifest_id": self.mint.tile_manifest_id(layer_id, &object_key),
                }));
                self.tiles.push(TileBlob {
                    layer_id: layer_id.to_owned(),
                    object_key,
                    column,
                    row,
                    colour_sha256_hex: hash_hex(&profile_hash(&colour)),
                    alpha_sha256_hex: hash_hex(&profile_hash(&alpha_plane)),
                    content_sha256_hex: content,
                    colour,
                    alpha: alpha_plane,
                });
            }
        }
        Ok(refs)
    }
}

fn refusal_error(refusal: IccRefusal) -> PsdError {
    if refusal.0 == "canceled" { PsdError::Canceled } else { PsdError::Proposal("icc_conversion_failed") }
}

/// How the document's pixels reach the tiles.
struct Routing {
    pixels_mapped: bool,
    unmapped_reason: &'static str,
    /// Legacy three-channel RGB path through `convert_rgb`.
    icc_active: bool,
    /// n-channel display conversion (CMYK, Lab, gray profile) through `convert_wide`.
    wide: Option<WideRoute>,
    intent_assumed: bool,
    bpc_assumed: bool,
    /// (path, kind, code) entries decided before layers are walked.
    notes: Vec<(&'static str, LossKind, &'static str)>,
}

/// `Ok(Err(refusal))` is the converter's typed refusal of this profile; `Err` is cancellation.
fn wide_device(
    converter: Option<&mut (dyn IccConverter + '_)>,
    icc: &[u8],
    cancel: &CancellationToken,
) -> Result<std::result::Result<ProfileFacts, IccRefusal>> {
    let Some(converter) = converter else {
        return Ok(Err(IccRefusal(WIDE_UNAVAILABLE)));
    };
    match converter.inspect_wide(icc, cancel) {
        Err(refusal) if refusal.0 == "canceled" => Err(PsdError::Canceled),
        other => Ok(other),
    }
}

/// Decides whether and how pixels map, consulting the converter about the embedded profile.
///
/// CMYK needs an embedded profile the wide path accepts as a CMYK device (no default profile is
/// invented); Lab is PCS-native and needs only a wide-capable converter; gray uses a `kTRC`
/// profile when one is embedded and accepted, otherwise the recorded sRGB assumption stays. Every
/// other outcome is a typed unmapped reason plus a loss note.
fn route(
    doc: &PsdDocument,
    mut converter: Option<&mut (dyn IccConverter + '_)>,
    cancel: &CancellationToken,
    cfg: &ProposalConfig<'_>,
) -> Result<Routing> {
    let header = &doc.header;
    let icc = doc.resources.icc_profile();
    let mode = header.color_mode;
    let wide_depth = matches!(header.depth, 8 | 16);
    let mapped_mode = matches!(mode, ColorMode::Rgb | ColorMode::Grayscale) && matches!(header.depth, 8 | 16 | 32);
    let wide_capable = converter.as_deref().is_some_and(|c| c.wide_available());
    let (intent, intent_assumed) = match cfg.rendering_intent {
        Some(intent) => (intent, false),
        None => (RenderingIntent::Perceptual, true),
    };
    let (bpc, bpc_assumed) = match cfg.black_point_compensation {
        Some(flag) => (flag, false),
        None => (true, true),
    };
    let mut routing = Routing {
        pixels_mapped: mapped_mode,
        unmapped_reason: "color_mode_or_depth_unmapped",
        icc_active: false,
        wide: None,
        intent_assumed,
        bpc_assumed,
        notes: Vec::new(),
    };
    // The black point compensation flag goes to Prism unchanged for every source kind, gray
    // included. A gray profile's black is NOT zero by definition (a `kTRC` table or parametric
    // curve can have TRC(0) > 0), so the flag is not dropped on that premise; Prism measures the
    // source black from the profile itself (ISO 18619) and is the identity when it is zero.
    let wide_route = |kind: WideKind| Some(WideRoute { kind, intent, bpc });
    let mut wide_attempted = false;
    match mode {
        ColorMode::Cmyk if wide_depth => {
            wide_attempted = wide_capable;
            routing.pixels_mapped = false;
            if !wide_capable {
                routing.unmapped_reason = "cmyk_display_conversion_unavailable";
                routing.notes.push(("document/color_mode", LossKind::Unsupported, "cmyk_display_conversion_unavailable"));
                if icc.is_none() {
                    routing.notes.push(("document/profile", LossKind::Unsupported, "cmyk_profile_missing"));
                }
            } else if let Some(icc) = icc {
                match wide_device(converter.as_deref_mut(), icc, cancel)? {
                    Ok(facts) if facts.device == ProfileDevice::Cmyk && facts.supports(intent) => {
                        routing.wide = wide_route(WideKind::Cmyk);
                    }
                    Ok(facts) if facts.device == ProfileDevice::Cmyk => {
                        // The chosen intent has no table in this profile; no other intent is substituted.
                        routing.unmapped_reason = "cmyk_intent_table_missing";
                        routing.notes.push((
                            "document/rendering_intent",
                            LossKind::Unsupported,
                            "icc_refused_missing_intent_table",
                        ));
                    }
                    Ok(_) => {
                        routing.unmapped_reason = "cmyk_profile_space_mismatch";
                        routing.notes.push(("document/profile", LossKind::Unsupported, "icc_profile_space_mismatch"));
                    }
                    Err(refusal) => {
                        routing.unmapped_reason = "cmyk_profile_refused";
                        routing.notes.push(("document/profile", LossKind::Unsupported, refusal_code(refusal)));
                    }
                }
            } else {
                routing.unmapped_reason = "cmyk_profile_missing";
                routing.notes.push(("document/profile", LossKind::Unsupported, "cmyk_profile_missing"));
            }
        }
        ColorMode::Cmyk => {
            routing.unmapped_reason = "cmyk_display_conversion_unavailable";
            routing.notes.push(("document/color_mode", LossKind::Unsupported, "cmyk_display_conversion_unavailable"));
            if icc.is_none() {
                routing.notes.push(("document/profile", LossKind::Unsupported, "cmyk_profile_missing"));
            }
        }
        ColorMode::Lab if wide_depth => {
            wide_attempted = wide_capable;
            routing.pixels_mapped = false;
            if wide_capable {
                routing.wide = wide_route(WideKind::Lab);
                if icc.is_some() {
                    routing.notes.push(("document/profile", LossKind::PreservedOpaque, "icc_profile_not_applied_for_mode"));
                }
            } else {
                routing.unmapped_reason = "lab_display_conversion_unavailable";
                routing.notes.push(("document/color_mode", LossKind::Unsupported, "lab_display_conversion_unavailable"));
            }
        }
        ColorMode::Lab => {
            routing.unmapped_reason = "lab_display_conversion_unavailable";
            routing.notes.push(("document/color_mode", LossKind::Unsupported, "lab_display_conversion_unavailable"));
        }
        // Gray and RGB with an embedded profile take the wide path, so the configured (or assumed)
        // rendering intent and black point compensation reach Prism; the legacy three-channel path
        // below cannot carry either.
        ColorMode::Grayscale | ColorMode::Rgb if wide_depth && wide_capable && icc.is_some() => {
            wide_attempted = true;
            let icc = icc.unwrap_or_default();
            let (device, kind) = if mode == ColorMode::Rgb {
                (ProfileDevice::Rgb, WideKind::Rgb)
            } else {
                (ProfileDevice::Gray, WideKind::Gray)
            };
            match wide_device(converter.as_deref_mut(), icc, cancel)? {
                Ok(facts) if facts.device == device && facts.supports(intent) => {
                    routing.wide = wide_route(kind);
                }
                Ok(facts) if facts.device == device => routing.notes.push((
                    "document/rendering_intent",
                    LossKind::Transformed,
                    "icc_refused_missing_intent_table",
                )),
                Ok(_) => routing.notes.push(("document/profile", LossKind::PreservedOpaque, "icc_profile_space_mismatch")),
                Err(refusal) => routing.notes.push(("document/profile", LossKind::Transformed, refusal_code(refusal))),
            }
        }
        _ => {}
    }
    if routing.wide.is_some() {
        routing.pixels_mapped = true;
    }
    if !wide_attempted && let (Some(icc), Some(converter)) = (icc, converter) {
        match converter.inspect(icc, cancel) {
            Err(refusal) if refusal.0 == "canceled" => return Err(PsdError::Canceled),
            Err(refusal) => {
                let kind = if mapped_mode { LossKind::Transformed } else { LossKind::Unsupported };
                routing.notes.push(("document/profile", kind, refusal_code(refusal)));
            }
            Ok(()) => {
                if mode == ColorMode::Rgb && wide_depth {
                    routing.icc_active = true;
                    // The legacy converter is relative colorimetric without black point
                    // compensation, whatever was configured or assumed: say so.
                    if intent != RenderingIntent::RelativeColorimetric {
                        routing.notes.push((
                            "document/rendering_intent",
                            LossKind::Transformed,
                            "rendering_intent_not_applied_legacy_rgb",
                        ));
                    }
                    if bpc {
                        routing.notes.push((
                            "document/black_point_compensation",
                            LossKind::Transformed,
                            "black_point_compensation_not_applied_legacy_rgb",
                        ));
                    }
                } else {
                    routing.notes.push(("document/profile", LossKind::PreservedOpaque, "icc_profile_not_applied_for_mode"));
                }
            }
        }
    }
    Ok(routing)
}

/// Maps `doc` to a proposal. The Folio snapshot is produced by Folio's own validator from the
/// wire document, so identities, parents, cycles and ports are checked there, not here.
pub fn to_proposal(
    doc: &PsdDocument,
    source: &SourceIdentity,
    cfg: &ProposalConfig<'_>,
    mint: &mut dyn IdMint,
    mut converter: Option<&mut dyn IccConverter>,
    cancel: &CancellationToken,
) -> Result<ImportProposal> {
    cancel.check()?;
    let header = &doc.header;
    let tree = doc.tree(cfg.limits)?;
    let routing = route(doc, converter.as_deref_mut(), cancel, cfg)?;
    let profile_id = match (routing.icc_active || routing.wide.is_some(), converter.as_deref()) {
        (true, Some(converter)) => converter.destination_profile_id(),
        _ => mint.linear_working_profile_id(),
    };
    let mut b = Builder {
        doc,
        cfg,
        mint,
        converter,
        cancel,
        profile_id: profile_id.clone(),
        pixels_mapped: routing.pixels_mapped,
        unmapped_reason: routing.unmapped_reason,
        icc_active: routing.icc_active,
        wide: routing.wide,
        stats: DisplayStats::default(),
        icc_range_clamped: false,
        non_finite_samples: false,
        alpha_clamped: false,
        layers: Vec::new(),
        operations: Vec::new(),
        edges: Vec::new(),
        tiles: Vec::new(),
        mask_tiles: Vec::new(),
        extras: Vec::new(),
        map: Vec::new(),
        loss: doc.loss_report(cfg.limits),
        tile_bytes: 0,
    };
    let converter_present = b.converter.is_some();
    // Panel order is top-first; graph outputs follow the group convention (bottom-most first).
    let mut outputs = b.walk(&tree, &Value::Null)?;
    outputs.reverse();

    let icc = doc.resources.icc_profile();
    let pixels_mapped = routing.pixels_mapped;
    let assumed_profile = pixels_mapped && !routing.icc_active && routing.wide.is_none();
    for (path, kind, code) in &routing.notes {
        b.note((*path).into(), *kind, code);
    }
    if b.non_finite_samples {
        b.note("document/pixels".into(), LossKind::Transformed, "non_finite_samples_zeroed");
    }
    if b.alpha_clamped {
        b.note("document/pixels".into(), LossKind::Transformed, "alpha_range_clamped");
    }
    if routing.icc_active {
        b.note("document/profile".into(), LossKind::Preserved, "icc_profile_applied_by_converter");
        if b.icc_range_clamped {
            b.note("document/profile".into(), LossKind::Transformed, "icc_input_range_clamped");
        }
        // The embedded profile was applied; the base report's "not applied" entry is stale.
        b.loss.entries.retain(|e| e.code != "icc_profile_not_applied");
    }
    let mut display = None;
    if let Some(wide) = routing.wide {
        match wide.kind {
            WideKind::Cmyk => {
                b.note("document/color_mode".into(), LossKind::Transformed, "cmyk_channels_assumed_inverted");
            }
            WideKind::Lab => {
                b.note("document/color_mode".into(), LossKind::Transformed, "lab_channels_assumed_icc_encoding");
            }
            WideKind::Gray | WideKind::Rgb => {}
        }
        if wide.kind != WideKind::Lab {
            b.note("document/profile".into(), LossKind::Preserved, "icc_profile_applied_by_converter");
            b.loss.entries.retain(|e| e.code != "icc_profile_not_applied");
        }
        if routing.intent_assumed {
            b.note("document/rendering_intent".into(), LossKind::Transformed, "rendering_intent_assumed_perceptual");
        }
        if routing.bpc_assumed {
            b.note(
                "document/black_point_compensation".into(),
                LossKind::Transformed,
                "black_point_compensation_assumed_on",
            );
        }
        if b.stats.input_range_clamped {
            let code = if wide.kind == WideKind::Lab { "lab_input_range_clamped" } else { "icc_input_range_clamped" };
            b.note("document/profile".into(), LossKind::Transformed, code);
        }
        // Any clamp to 0..=1 is reported: a gamut clip beyond the tolerance, otherwise rounding noise.
        if b.stats.pixels_out_of_gamut > 0 {
            b.note("document/display".into(), LossKind::Transformed, "display_out_of_gamut_clamped");
        } else if b.stats.pixels_clamped > 0 {
            b.note("document/display".into(), LossKind::Transformed, "display_rounding_noise_clamped");
        }
        let stats = &b.stats;
        display = Some(DisplayConversion {
            source_space: wide.kind.code(),
            intent: wide.intent,
            intent_assumed: routing.intent_assumed,
            black_point_compensation: wide.bpc,
            black_point_compensation_assumed: routing.bpc_assumed,
            pixels_converted: stats.pixels_converted,
            pixels_clamped: stats.pixels_clamped,
            pixels_out_of_gamut: stats.pixels_out_of_gamut,
            channels_clamped: stats.channels_clamped,
            max_overshoot: stats.max_overshoot,
            input_range_clamped: stats.input_range_clamped,
            converter_clip: stats.clip,
        });
    }
    if pixels_mapped {
        // The base report describes the file; these two are resolved by the mapping above.
        b.loss.entries.retain(|e| !matches!(e.code, "color_mode_unmapped" | "bit_depth_unmapped"));
    }
    if assumed_profile {
        let code = if header.depth == 32 { "float32_assumed_linear_light" } else { "profile_assumed_srgb" };
        b.note("document/profile".into(), LossKind::Transformed, code);
    }
    if pixels_mapped {
        if header.depth == 16 {
            let code = match cfg.scale {
                SixteenBitScale::Full65535 => "sixteen_bit_scale_assumed_65535",
                SixteenBitScale::Photoshop32768 => "sixteen_bit_scale_assumed_32768",
            };
            b.note("document/depth".into(), LossKind::Transformed, code);
        }
        if header.color_mode == ColorMode::Grayscale {
            b.note("document/color_mode".into(), LossKind::Transformed, "gray_replicated_to_rgb");
        }
    }
    if doc.resources.find(RESOURCE_ICC_UNTAGGED).is_some_and(|r| r.data.first() == Some(&1)) && icc.is_none() {
        b.note("document/profile".into(), LossKind::Preserved, "profile_untagged_by_author");
    }
    b.note("document/layer_extras".into(), LossKind::Preserved, "layer_extras_sidecar_only");
    if doc.resources.find(RESOURCE_ICC_PROFILE).is_some() && !pixels_mapped && !converter_present {
        b.note("document/profile".into(), LossKind::PreservedOpaque, "icc_profile_not_applied");
    }

    let artboard_id = b.mint.artboard_id();
    let document_id = b.mint.document_id();
    let profile = if pixels_mapped { Value::String(profile_id) } else { Value::Null };
    let wire = json!({
        "schema_id": "hsk.studio.document@1",
        "document_id": document_id,
        "revision": 1,
        "geometry_unit": "px",
        "colour_mode": studio_colour_mode(header.color_mode),
        "bits_per_channel": header.depth,
        "working_space_rgb": profile,
        "working_space_cmyk": null,
        "working_space_gray": null,
        "working_space_spot": null,
        "blending_space": null,
        "artboards": [{
            "schema_id": "hsk.studio.artboard@1", "artboard_id": artboard_id, "name": "canvas", "parent": null,
            "bounds": {"x": px(0.0), "y": px(0.0), "width": px(f64::from(header.width)), "height": px(f64::from(header.height))},
        }],
        "page_spreads": [],
        "layers": b.layers,
        "graph": {"schema_id": "hsk.studio.layer_graph@1", "operations": b.operations, "edges": b.edges, "outputs": outputs},
    });
    let bytes = serde_json::to_vec(&wire).map_err(|_| PsdError::Encode("proposal_json"))?;
    let snapshot = match inspect_bytes(&bytes, cfg.budget, cancel, cfg.resolver) {
        Ok(Inspection::Editable(s)) => s,
        Ok(Inspection::ReadOnly(_)) => return Err(PsdError::Proposal("folio_read_only")),
        Err(d) if d.code == hsk_studio_folio::Code::Canceled => return Err(PsdError::Canceled),
        Err(_) => return Err(PsdError::Proposal("folio_rejected")),
    };
    Ok(ImportProposal {
        snapshot,
        tiles: b.tiles,
        mask_tiles: b.mask_tiles,
        extras: b.extras,
        layer_map: b.map,
        loss: b.loss,
        provenance: Provenance {
            source_sha256_hex: hash_hex(&source.sha256),
            byte_len: source.byte_len,
            psd_version: header.version,
            width: header.width,
            height: header.height,
            depth: header.depth,
            assumed_profile,
            icc_converted: routing.icc_active || routing.wide.is_some_and(|w| w.kind != WideKind::Lab),
            icc_sha256_hex: icc.map(|bytes| hash_hex(&profile_hash(bytes))),
            sixteen_bit_scale: cfg.scale,
            display,
        },
    })
}
