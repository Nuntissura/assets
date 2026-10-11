//! Import proposal: maps a parsed [`PsdDocument`] to a Folio snapshot plus the data Folio does
//! not hold (pixels as Pigment tiles, per-layer appearance, loss report, provenance). The result
//! is a PROPOSAL; identity minting, publication of tiles and profile resolution stay with the
//! host.
//!
//! Slice scope: 8-bit RGB or grayscale documents map pixels; every other mode or depth keeps the
//! hierarchy and reports the raster layers as unmapped. Folio v1 has no layer opacity, blend,
//! clipping or mask fields, so those travel in [`LayerAppearance`] and are reported once.

use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Budget, Inspection, Resolver, Snapshot, inspect_bytes};
use hsk_studio_pigment::Grid;
use hsk_studio_prism::{hash_hex, profile_hash};
use serde_json::{Value, json};

use crate::blend::BlendMode;
use crate::document::{PsdDocument, TreeNode};
use crate::error::{PsdError, Result};
use crate::header::ColorMode;
use crate::layers::{GroupRole, PsdLayer};
use crate::limits::Limits;
use crate::loss::{LossKind, LossReport, key_text};
use crate::resources::{RESOURCE_ICC_PROFILE, RESOURCE_ICC_UNTAGGED};

/// Pigment tile edge in pixels (columns and rows are cells of a canvas-origin grid).
pub const TILE_EDGE: u32 = 256;
const TILE_FORMAT: &str = "rgb_f32le";
const MAX_EMBEDDED_OPAQUE_BYTES: usize = 65_536;

const ADJUSTMENT_KEYS: [&[u8; 4]; 17] = [
    b"levl", b"curv", b"brit", b"blnc", b"blwh", b"hue ", b"hue2", b"selc", b"mixr", b"grdm", b"phfl", b"expA",
    b"thrs", b"nvrt", b"post", b"vibA", b"clrL",
];
const FILL_KEYS: [&[u8; 4]; 3] = [b"SoCo", b"GdFl", b"PtFl"];
const CACHED_PIXEL_KEYS: [&[u8; 4]; 8] = [b"TySh", b"tySh", b"SoLd", b"SoLE", b"PlLd", b"plLd", b"lfx2", b"lrFX"];

/// Host-owned identity and handle minting. The importer never invents identities.
pub trait IdMint {
    /// `SDOC-...`
    fn document_id(&mut self) -> String;
    /// `SART-...`
    fn artboard_id(&mut self) -> String;
    /// `SLYR-...`
    fn layer_id(&mut self) -> String;
    /// `SCPF-...`: the profile the tiles are expressed in (linear light, sRGB primaries).
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
}

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

/// Layer properties Folio v1 cannot hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerAppearance {
    pub layer_id: String,
    pub psd_index: usize,
    pub opacity: u8,
    pub blend_key: [u8; 4],
    /// `None` for a key outside the 28 known ones (typed loss, never Normal).
    pub blend_mode: Option<BlendMode>,
    pub clipped: bool,
    pub blend_clipped_elements: Option<bool>,
    pub blend_interior_elements: Option<bool>,
    pub knockout: Option<bool>,
    pub group_blend_key: Option<[u8; 4]>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayerMapEntry {
    pub layer_id: String,
    pub psd_index: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    pub source_sha256_hex: String,
    pub byte_len: usize,
    pub psd_version: u16,
    pub width: u32,
    pub height: u32,
    /// sRGB was assumed (no usable profile was applied); see the loss report.
    pub assumed_profile: bool,
    pub icc_sha256_hex: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ImportProposal {
    pub snapshot: Snapshot,
    pub tiles: Vec<TileBlob>,
    pub appearance: Vec<LayerAppearance>,
    pub layer_map: Vec<LayerMapEntry>,
    pub loss: LossReport,
    pub provenance: Provenance,
}

impl ImportProposal {
    /// Copies names and visibility from an edited snapshot back onto the matching PSD layers and
    /// returns how many layers changed. Nothing else is touched.
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
                changed += 1;
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

/// `sRGB` encoded 8-bit value to linear light (IEC 61966-2-1).
fn srgb_lut() -> [f32; 256] {
    std::array::from_fn(|i| {
        let v = i as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    })
}

fn floor_div(a: i64, b: i64) -> i64 {
    a.div_euclid(b)
}

struct Builder<'a, 'm> {
    doc: &'a PsdDocument,
    cfg: &'a ProposalConfig<'a>,
    mint: &'m mut dyn IdMint,
    cancel: &'a CancellationToken,
    profile_id: String,
    pixels_mapped: bool,
    lut: [f32; 256],
    layers: Vec<Value>,
    operations: Vec<Value>,
    edges: Vec<Value>,
    tiles: Vec<TileBlob>,
    appearance: Vec<LayerAppearance>,
    map: Vec<LayerMapEntry>,
    loss: LossReport,
    tile_bytes: usize,
}

impl Builder<'_, '_> {
    fn note(&mut self, path: String, kind: LossKind, code: &'static str) {
        self.loss.entries.push(crate::loss::LossEntry { path, kind, code });
    }

    fn record_appearance(&mut self, layer_id: &str, index: usize, layer: &PsdLayer) {
        self.appearance.push(LayerAppearance {
            layer_id: layer_id.to_owned(),
            psd_index: index,
            opacity: layer.opacity,
            blend_key: layer.blend_key,
            blend_mode: layer.blend_mode(),
            clipped: layer.clipping == 1,
            blend_clipped_elements: layer.blend_clipped_elements(),
            blend_interior_elements: layer.blend_interior_elements(),
            knockout: layer.knockout(),
            group_blend_key: layer.group_blend_key(),
        });
        self.map.push(LayerMapEntry { layer_id: layer_id.to_owned(), psd_index: index });
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
        self.layers.push(json!({
            "schema_id": "hsk.studio.layer@1", "layer_id": id, "name": layer.name, "parent": parent,
            "kind": "group", "payload": {"payload_kind": "group"}, "visible": layer.visible(),
        }));
        self.record_appearance(&id, folder, layer);
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
            let (kind, code) =
                if adjustment.is_some() { ("psd_adjustment", "adjustment_layer_unsupported_payload") } else { ("psd_fill", "fill_layer_unsupported_payload") };
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
            self.record_appearance(&id, index, layer);
            return Ok(None);
        }
        if layer.blocks.iter().any(|b| CACHED_PIXEL_KEYS.contains(&&b.key)) {
            self.note(format!("layer[{index}]/pixels"), LossKind::Rasterized, "cached_pixels_display_truth");
        }
        if !self.pixels_mapped {
            self.layers.push(json!({
                "schema_id": "hsk.studio.layer@1", "layer_id": id, "name": layer.name, "parent": parent,
                "kind": "psd_raster_unmapped",
                "payload": {"payload_kind": "unsupported", "kind": "psd_raster_unmapped",
                    "schema_id": "psd.raster", "encoded_bytes": Vec::<u8>::new(), "reason": "color_mode_or_depth_unmapped"},
                "visible": layer.visible(),
            }));
            self.note(format!("layer[{index}]/pixels"), LossKind::Unsupported, "raster_pixels_unmapped");
            self.record_appearance(&id, index, layer);
            return Ok(None);
        }
        let tiles = self.tiles_for(index, layer, &id)?;
        self.layers.push(json!({
            "schema_id": "hsk.studio.layer@1", "layer_id": id, "name": layer.name, "parent": parent,
            "kind": "raster", "payload": {"payload_kind": "raster", "tiles": tiles}, "visible": layer.visible(),
        }));
        self.operations.push(json!({
            "layer_id": id, "operation_key": "source", "operation_kind": "source",
            "inputs": [], "outputs": [port("out", "one", "source")],
        }));
        self.record_appearance(&id, index, layer);
        Ok(Some(address(&id, "source", "out")))
    }

    fn tiles_for(&mut self, index: usize, layer: &PsdLayer, layer_id: &str) -> Result<Vec<Value>> {
        let rect = layer.rect;
        if rect.area() == 0 || layer.pixel_data_irrelevant() {
            return Ok(Vec::new());
        }
        let header = &self.doc.header;
        let colour_ids: &[i16] = if header.color_mode == ColorMode::Rgb { &[0, 1, 2] } else { &[0] };
        let mut planes = Vec::with_capacity(3);
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
        for row in floor_div(top, edge)..=floor_div(bottom - 1, edge) {
            for column in floor_div(left, edge)..=floor_div(right - 1, edge) {
                self.cancel.check()?;
                let bounds = grid.bounds(column, row).map_err(|_| PsdError::Overflow)?;
                let x0 = bounds.x.max(left);
                let x1 = (bounds.x + i64::from(bounds.width)).min(right);
                let y0 = bounds.y.max(top);
                let y1 = (bounds.y + i64::from(bounds.height)).min(bottom);
                let cell = (TILE_EDGE as usize) * (TILE_EDGE as usize);
                let mut colour = vec![0u8; cell * 12];
                let mut alpha_plane = vec![0u8; cell * 4];
                let mut any_alpha = false;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let src = (y - top) as usize * width + (x - left) as usize;
                        let dst = (y - bounds.y) as usize * TILE_EDGE as usize + (x - bounds.x) as usize;
                        let rgb = match planes.as_slice() {
                            [g] => [g.bytes[src]; 3],
                            [r, g, b] => [r.bytes[src], g.bytes[src], b.bytes[src]],
                            _ => return Err(PsdError::ChannelSize),
                        };
                        for (k, v) in rgb.iter().enumerate() {
                            colour[dst * 12 + k * 4..dst * 12 + k * 4 + 4].copy_from_slice(&self.lut[usize::from(*v)].to_le_bytes());
                        }
                        let a = alpha.as_ref().map_or(1.0, |p| f32::from(p.bytes[src]) / 255.0);
                        any_alpha |= a > 0.0;
                        alpha_plane[dst * 4..dst * 4 + 4].copy_from_slice(&a.to_le_bytes());
                    }
                }
                if !any_alpha {
                    continue;
                }
                self.tile_bytes = self.tile_bytes.saturating_add(colour.len() + alpha_plane.len());
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

/// Maps `doc` to a proposal. The Folio snapshot is produced by Folio's own validator from the
/// wire document, so identities, parents, cycles and ports are checked there, not here.
pub fn to_proposal(
    doc: &PsdDocument,
    source: &SourceIdentity,
    cfg: &ProposalConfig<'_>,
    mint: &mut dyn IdMint,
    cancel: &CancellationToken,
) -> Result<ImportProposal> {
    cancel.check()?;
    let header = &doc.header;
    let tree = doc.tree(cfg.limits)?;
    let pixels_mapped = header.depth == 8 && matches!(header.color_mode, ColorMode::Rgb | ColorMode::Grayscale);
    let profile_id = mint.linear_working_profile_id();
    let mut b = Builder {
        doc,
        cfg,
        mint,
        cancel,
        profile_id: profile_id.clone(),
        pixels_mapped,
        lut: srgb_lut(),
        layers: Vec::new(),
        operations: Vec::new(),
        edges: Vec::new(),
        tiles: Vec::new(),
        appearance: Vec::new(),
        map: Vec::new(),
        loss: doc.loss_report(cfg.limits),
        tile_bytes: 0,
    };
    let outputs = b.walk(&tree, &Value::Null)?;

    let icc = doc.resources.icc_profile();
    let assumed_profile = pixels_mapped;
    if pixels_mapped {
        b.note("document/profile".into(), LossKind::Transformed, "profile_assumed_srgb");
        if header.color_mode == ColorMode::Grayscale {
            b.note("document/color_mode".into(), LossKind::Transformed, "gray_replicated_to_rgb");
        }
    }
    if doc.resources.find(RESOURCE_ICC_UNTAGGED).is_some_and(|r| r.data.first() == Some(&1)) && icc.is_none() {
        b.note("document/profile".into(), LossKind::Preserved, "profile_untagged_by_author");
    }
    b.note("document/appearance".into(), LossKind::Preserved, "opacity_blend_clipping_sidecar_only");
    if doc.resources.find(RESOURCE_ICC_PROFILE).is_some() && !pixels_mapped {
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
        appearance: b.appearance,
        layer_map: b.map,
        loss: b.loss,
        provenance: Provenance {
            source_sha256_hex: hash_hex(&source.sha256),
            byte_len: source.byte_len,
            psd_version: header.version,
            width: header.width,
            height: header.height,
            assumed_profile,
            icc_sha256_hex: icc.map(|bytes| hash_hex(&profile_hash(bytes))),
        },
    })
}
