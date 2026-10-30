//! Raster Gradient tool, Paint Bucket and Edit > Fill in the viewer (lane SMB-PS-GRADIENT-BUCKET-1;
//! STU-RAS-025, STU-RAS-160, STU-RAS-161, STU-SHL-155 tools `gradient` and `flood_fill`), driven by
//! hsk-studio-pigment `fill_tools`. Headless [`Viewer`] methods first ([`Viewer::fill_gradient`],
//! [`Viewer::flood_fill`], [`Viewer::fill_selection`]); the egui tools only call them through the
//! canvas actions [`Viewer::fill_drag`] (Gradient: a drag) and [`Viewer::fill_click`] (Paint
//! Bucket: a click), with the option row `fill.*` and the `fill.fill_selection` button.
//!
//! Each fill is ONE undoable chronicle `tiles` edit (op `fill_gradient`, `flood_fill` or
//! `fill_selection` in the operation log), staged like the brush: the layer's pixels over the
//! canvas, pigment's replacement planes and coverage (clipped to the selection), then pigment
//! `masked_replace` per 64 x 64 block of every touched tile; empty 256-pixel cells of a layer on
//! the importers' grid get new tiles. Non-pixel layers (vector, text, group, fill, adjustment,
//! placed asset) are refused naming their kind; nothing is rasterized implicitly (STU-RAS-170,
//! STU-RAS-117).
//!
//! Studio readings (Photoshop behaviour UNVERIFIED, D5):
//! - The tools work on display-encoded values: the linear-light tiles are encoded with the sRGB
//!   transfer, pigment interpolates the gradient, measures the bucket tolerance (8-bit display
//!   levels, as the magic wand) and blends there, and the replacement is decoded back to linear
//!   light. Tile values above 1 are clamped by that encoding where a pixel is filled.
//! - The foreground colour is the brush colour; the default gradient is foreground to
//!   transparent (colour stops foreground at 0 and 4096, transparency 100 % to 0 %). The window
//!   has no background colour, so no stop defers to one.
//! - Sample merged reads the rendered composite (the frame the viewer shows).
//! - Pattern content is offered only when the document carries a resolved pattern tile (a fill
//!   layer's or a pattern overlay's `pattern_tile`); it is tiled from the document origin at 100 %
//!   (the source record's scale and phase are not applied). The record carries no pattern name,
//!   so the label is the pattern id. Without one the headless call refuses `pattern_unavailable`.
//! - Gradients on layer masks are not offered: the viewer has no mask edit target.
#![deny(clippy::arithmetic_side_effects)]
use super::Viewer;
use super::brush_mix::{BLOCK, BYTE_LIMIT, MixTarget, Quiet, plane as window, put, tile_view};
use super::edit::{self, Tool};
use super::widget;
use hsk_studio_accord::{CancellationToken, DomainId};
use hsk_studio_folio::{Payload, StudioEffectKind, StudioFill, StudioRasterRecords, TileRef};
use hsk_studio_observe::TileAddress;
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::brush::{BrushLimits, crop_to};
use hsk_studio_pigment::caller::{MemoryAdmission, SerializedOwner};
use hsk_studio_pigment::fill_tools::{
    self as pf, ColourStop, DragLine, FillContent, FillPaint, FillResult, FloodOptions, GradientForm, GradientGeometry, PatternTile,
    RasterGradient, SMOOTHNESS_FULL, TransparencyStop,
};
use hsk_studio_pigment::render_plan::MaskFormat;
use hsk_studio_pigment::select_tools::ToolLimits;
use hsk_studio_pigment::selection::ColourView;
use hsk_studio_pigment::{Grid, Mask, Outcome, Rect, Request, TileItem, masked_replace};
use hsk_studio_prism::{DisplayEncoding, profile_hash};
use serde_json::{Value, json};

/// Largest canvas the tools read (16 bytes per pixel).
pub const MAX_REGION_BYTES: u64 = 256 * 1024 * 1024;
/// Largest pattern side resolved from a document (pigment's pattern ceiling).
pub const MAX_PATTERN_SIDE: u32 = 4096;

/// Widget ids of the option row.
pub const GEOMETRY: &str = "fill.geometry";
pub const MODE: &str = "fill.mode";
pub const OPACITY: &str = "fill.opacity";
pub const REVERSE: &str = "fill.reverse";
pub const DITHER: &str = "fill.dither";
pub const TOLERANCE: &str = "fill.tolerance";
pub const CONTIGUOUS: &str = "fill.contiguous";
pub const ANTI_ALIAS: &str = "fill.anti_alias";
pub const SAMPLE_MERGED: &str = "fill.sample_merged";
pub const CONTENT: &str = "fill.content";
pub const FILL_SELECTION: &str = "fill.fill_selection";

/// Blend modes the option row offers (the tool modes behind and clear included).
pub const MODES: [StudioBlendMode; 29] = [
    StudioBlendMode::Normal,
    StudioBlendMode::Dissolve,
    StudioBlendMode::Behind,
    StudioBlendMode::Clear,
    StudioBlendMode::Darken,
    StudioBlendMode::Multiply,
    StudioBlendMode::ColourBurn,
    StudioBlendMode::LinearBurn,
    StudioBlendMode::DarkerColour,
    StudioBlendMode::Lighten,
    StudioBlendMode::Screen,
    StudioBlendMode::ColourDodge,
    StudioBlendMode::LinearDodgeAdd,
    StudioBlendMode::LighterColour,
    StudioBlendMode::Overlay,
    StudioBlendMode::SoftLight,
    StudioBlendMode::HardLight,
    StudioBlendMode::VividLight,
    StudioBlendMode::LinearLight,
    StudioBlendMode::PinLight,
    StudioBlendMode::HardMix,
    StudioBlendMode::Difference,
    StudioBlendMode::Exclusion,
    StudioBlendMode::Subtract,
    StudioBlendMode::Divide,
    StudioBlendMode::Hue,
    StudioBlendMode::Saturation,
    StudioBlendMode::Colour,
    StudioBlendMode::Luminosity,
];

/// What the bucket and Edit > Fill paint.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum FillSource {
    /// The foreground (brush) colour.
    #[default]
    Foreground,
    /// A pattern of the document, by id.
    Pattern(String),
}

/// Settings of the Gradient tool, the Paint Bucket and Edit > Fill.
#[derive(Clone, Debug, PartialEq)]
pub struct FillSettings {
    pub geometry: GradientGeometry,
    pub mode: StudioBlendMode,
    /// Percent `0..=100`.
    pub opacity_pct: f64,
    pub reverse: bool,
    pub dither: bool,
    /// Explicit stop lists in display-encoded colour (`None` = foreground to transparent).
    pub stops: Option<(Vec<ColourStop>, Vec<TransparencyStop>)>,
    /// Bucket tolerance in 8-bit display levels.
    pub tolerance: f64,
    pub contiguous: bool,
    pub anti_alias: bool,
    pub sample_merged: bool,
    pub content: FillSource,
}
impl Default for FillSettings {
    /// Linear, normal, 100 %, tolerance 32 (the wand's Studio default), contiguous and anti-alias
    /// on (STU-RAS-119), sample merged off, the foreground colour.
    fn default() -> Self {
        Self {
            geometry: GradientGeometry::Linear,
            mode: StudioBlendMode::Normal,
            opacity_pct: 100.0,
            reverse: false,
            dither: false,
            stops: None,
            tolerance: 32.0,
            contiguous: true,
            anti_alias: true,
            sample_merged: false,
            content: FillSource::Foreground,
        }
    }
}

/// Hover text of the tools.
pub fn tool_hover(tool: Tool) -> &'static str {
    match tool {
        Tool::Gradient => "Drag across the selected raster layer (or the selection) to draw a gradient from the foreground colour to transparent (geometry, mode, opacity below)",
        Tool::FloodFill => "Click the selected raster layer to fill similar contiguous colour with the foreground colour (tolerance, contiguous, anti-alias, sample merged below)",
        _ => "",
    }
}

/// sRGB-encodes linear `rgb_f32le` bytes through Prism's display encoding (IEC 61966-2-1,
/// clamped to `[0, 1]`); a non-finite sample is refused.
fn encode_rgb(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(bytes.len());
    for px in bytes.chunks_exact(12) {
        let c = [0usize, 4, 8].map(|i| px.get(i..i.saturating_add(4)).and_then(|b| b.try_into().ok()).map_or(f32::NAN, f32::from_le_bytes));
        let e = DisplayEncoding::Srgb.encode(c).map_err(|e| format!("sRGB encode: {}", e.code()))?;
        for v in e {
            out.extend_from_slice(&v.to_le_bytes());
        }
    }
    Ok(out)
}

/// IEC 61966-2-1 decode into linear light (Prism exposes the encode only; the brush's 8-bit
/// `edit::linear(u8)` would quantize the replacement).
fn decode(v: f32) -> f32 {
    let c = v.clamp(0.0, 1.0);
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn f32s(bytes: &[u8]) -> impl Iterator<Item = f32> + '_ {
    bytes.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
}

/// A pattern resolved from the document: display-encoded straight RGBA.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedPattern {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<[f32; 4]>,
}

/// The pattern tiles a record set carries (fill-layer patterns, then pattern overlays), first per
/// id.
fn pattern_refs(records: Option<&StudioRasterRecords>) -> Vec<(String, TileRef)> {
    let mut out: Vec<(String, TileRef)> = Vec::new();
    let mut push = |id: &str, tile: &TileRef| {
        if !out.iter().any(|(i, _)| i == id) {
            out.push((id.to_string(), tile.clone()));
        }
    };
    for l in records.map(|r| r.layers.as_slice()).unwrap_or(&[]) {
        if let Some(f) = &l.fill_layer
            && let StudioFill::Pattern { pattern, pattern_tile: Some(tile), .. } = &f.fill
        {
            push(&pattern.object_id, tile);
        }
        for e in l.effects.as_ref().map(|s| s.effects.as_slice()).unwrap_or(&[]) {
            if let StudioEffectKind::PatternOverlay { pattern, pattern_tile: Some(tile), .. } = &e.params {
                push(&pattern.object_id, tile);
            }
        }
    }
    out
}

/// Decodes a pattern tile body (`rgb_f32le` + straight `f32le` alpha) into display-encoded RGBA.
fn decode_pattern(id: &str, tile: &TileRef, body: &[u8]) -> Result<ResolvedPattern, String> {
    let side = |l: &hsk_studio_folio::Length| (l.value.is_finite() && l.value >= 1.0 && l.value <= f64::from(MAX_PATTERN_SIDE) && l.value.fract() == 0.0).then_some(l.value as u32);
    let (Some(w), Some(h)) = (side(&tile.width), side(&tile.height)) else {
        return Err(format!("pattern_unavailable: pattern {id} tile size is not whole pixels up to {MAX_PATTERN_SIDE}"));
    };
    let n = usize::try_from(w).ok().zip(usize::try_from(h).ok()).and_then(|(a, b)| a.checked_mul(b)).ok_or("pattern_unavailable: size")?;
    let colour_len = n.checked_mul(12).ok_or("pattern_unavailable: size")?;
    if tile.format != "rgb_f32le" || Some(body.len()) != colour_len.checked_add(n.checked_mul(4).ok_or("pattern_unavailable: size")?) {
        return Err(format!("pattern_unavailable: pattern {id} tile body is {} bytes ({})", body.len(), tile.format));
    }
    let (colour, alpha) = body.split_at(colour_len);
    let rgb: Vec<f32> = f32s(&encode_rgb(colour).map_err(|e| format!("pattern_unavailable: {e}"))?).collect();
    let pixels = rgb
        .chunks_exact(3)
        .zip(f32s(alpha))
        .map(|(c, a)| [c[0], c[1], c[2], if a.is_finite() { a.clamp(0.0, 1.0) } else { 0.0 }])
        .collect();
    Ok(ResolvedPattern { id: id.to_string(), name: id.to_string(), width: w, height: h, pixels })
}

/// The pixels of a raster layer over the canvas, display-encoded, with what committing needs.
struct Target {
    canvas: Rect,
    tiles: Vec<TileRef>,
    rects: Vec<Rect>,
    on_cell_grid: bool,
    profile: Option<String>,
    document: DomainId,
    layer: DomainId,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}

/// Offset of `inner`'s top-left inside `outer`.
fn offset(outer: Rect, inner: Rect) -> Option<(usize, usize)> {
    Some((usize::try_from(inner.x.checked_sub(outer.x)?).ok()?, usize::try_from(inner.y.checked_sub(outer.y)?).ok()?))
}

/// One block through pigment `masked_replace` with per-pixel replacement planes: the brush's
/// engine route ([`super::brush_mix::replace_block`], its tile views and sink), kept as its own
/// request because the brush runner builds its replacement from ONE colour
/// (`paint_replacement(block, rgb)`) and has no parameter for replacement planes.
#[allow(clippy::too_many_arguments)]
fn replace_block(target: &MixTarget<'_>, tile: &TileRef, block: Rect, colour: &[u8], alpha: &[u8], rc: &[u8], ra: &[u8], mask: &[u8]) -> Result<(Vec<u8>, Vec<u8>), String> {
    let key = format!("{}-fx{}y{}", tile.object_key, block.x, block.y);
    let view: TileRef = serde_json::from_value(json!({
        "object_key": key, "layer_id": tile.layer_id, "column": 0, "row": 0,
        "width": {"value": block.width, "unit": "px"}, "height": {"value": block.height, "unit": "px"},
        "format": "rgb_f32le", "colour_profile_id": target.profile.as_str(), "schema_id": "hsk.studio.raster_tile@1",
        "content_digest": tile.content_digest, "artifact_manifest_id": tile.artifact_manifest_id,
    }))
    .map_err(|e| format!("block reference: {e}"))?;
    let cs = u64::from(block.width).checked_mul(12).ok_or("stride")?;
    let as_ = u64::from(block.width).checked_mul(4).ok_or("stride")?;
    let items = [TileItem { before: tile_view(&view, block, colour, alpha, cs, as_, target.profile), replacement: tile_view(&view, block, rc, ra, cs, as_, target.profile), expected_property_revision: 0 }];
    let request = Request {
        transport_version: 1,
        operation: "masked_replace",
        document_id: target.document,
        layer_id: target.layer,
        command_id: "harness-fill",
        correlation_id: 1,
        actor: target.actor,
        base_revision: 0,
        cancel_epoch: 1,
        grid: Grid { origin_x: block.x, origin_y: block.y, tile_width: block.width, tile_height: block.height },
        rectangle: block,
        mask: Mask { bounds: block, sample_format: "coverage_u16le", bytes: mask, stride_bytes: u64::from(block.width).checked_mul(2).ok_or("stride")?, sha256: profile_hash(mask) },
        tiles: &items,
        byte_limit: BYTE_LIMIT,
    };
    let address = TileAddress::new(target.layer.clone(), &key, 0, 0).map_err(|_| "block address".to_string())?;
    let admission = MemoryAdmission::new(BYTE_LIMIT).map_err(|e| format!("admission {e:?}"))?;
    let mut owner = SerializedOwner::new(target.document.clone(), target.layer.clone(), target.actor.clone(), 1, vec![(address, 0)], admission, Quiet).map_err(|e| format!("owner {e:?}"))?;
    match masked_replace(&request, &mut owner, &CancellationToken::default()) {
        Outcome::Accepted(patch) => match patch.updates.first() {
            Some(update) => Ok((update.after.colour.to_vec(), update.after.alpha.to_vec())),
            None => Ok((colour.to_vec(), alpha.to_vec())),
        },
        Outcome::Rejected(failure) => Err(format!("masked_replace refused: {:?}", failure.error)),
        Outcome::ReconciliationRequired { .. } => Err("masked_replace: reconciliation required".into()),
    }
}

/// Applies a fill result (replacement colour already linear in `linear`) to one tile body at
/// document rectangle `rect`, block by block. `Ok(None)` when nothing in the tile changes.
fn apply_tile(target: &MixTarget<'_>, tile: &TileRef, before: &[u8], rect: Rect, result: &FillResult, linear: &[u8]) -> Result<Option<Vec<u8>>, String> {
    let (w, h) = (usize::try_from(rect.width).map_err(|_| "width")?, usize::try_from(rect.height).map_err(|_| "height")?);
    let n = w.checked_mul(h).ok_or("tile size")?;
    let colour_len = n.checked_mul(12).ok_or("tile size")?;
    if Some(before.len()) != colour_len.checked_add(n.checked_mul(4).ok_or("tile size")?) {
        return Err(format!("tile body is {} bytes", before.len()));
    }
    let Some(touched) = rect.intersect(result.bounds).map_err(|e| format!("{e:?}"))? else {
        return Ok(None);
    };
    let limits = BrushLimits::default();
    let (cs, as_) = (w.checked_mul(12).ok_or("stride")?, w.checked_mul(4).ok_or("stride")?);
    let rw = usize::try_from(result.bounds.width).map_err(|_| "width")?;
    let (rcs, ras) = (rw.checked_mul(12).ok_or("stride")?, rw.checked_mul(4).ok_or("stride")?);
    let mut out = before.to_vec();
    let mut changed = false;
    let block = i64::from(BLOCK);
    let (ex, ey) = touched.end().map_err(|e| format!("{e:?}"))?;
    let first = |v: i64| v.checked_div_euclid(block).and_then(|b| b.checked_mul(block));
    let mut y = first(touched.y).ok_or("block row")?;
    while y < ey {
        let mut x = first(touched.x).ok_or("block column")?;
        while x < ex {
            if let Some(b) = (Rect { x, y, width: BLOCK, height: BLOCK }).intersect(rect).map_err(|e| format!("{e:?}"))? {
                let mask_plane = crop_to(&result.coverage, b, &limits).map_err(|e| e.code().to_string())?;
                if mask_plane.data.iter().any(|v| *v != 0) {
                    let mask = mask_plane.to_mask_bytes(MaskFormat::CoverageU16Le).map_err(|e| format!("{e:?}"))?;
                    let at = offset(rect, b).ok_or("block offset")?;
                    let colour = window(&out[..colour_len], cs, 12, b, at).ok_or("block colour")?;
                    let alpha = window(&out[colour_len..], as_, 4, b, at).ok_or("block alpha")?;
                    // Replacement: the result where it lies, the tile's own pixels elsewhere
                    // (their coverage is 0).
                    let (mut rc, mut ra) = (colour.clone(), alpha.clone());
                    if let Some(inner) = b.intersect(result.bounds).map_err(|e| format!("{e:?}"))? {
                        let src_at = offset(result.bounds, inner).ok_or("result offset")?;
                        let dst_at = offset(b, inner).ok_or("block offset")?;
                        let bw = usize::try_from(b.width).map_err(|_| "width")?;
                        let c = window(linear, rcs, 12, inner, src_at).ok_or("result colour")?;
                        let a = window(&result.alpha, ras, 4, inner, src_at).ok_or("result alpha")?;
                        put(&mut rc, bw.checked_mul(12).ok_or("stride")?, 12, inner, dst_at, &c).ok_or("replacement colour")?;
                        put(&mut ra, bw.checked_mul(4).ok_or("stride")?, 4, inner, dst_at, &a).ok_or("replacement alpha")?;
                    }
                    let (after_c, after_a) = replace_block(target, tile, b, &colour, &alpha, &rc, &ra, &mask)?;
                    if after_c != colour || after_a != alpha {
                        let (c_part, a_part) = out.split_at_mut(colour_len);
                        put(c_part, cs, 12, b, at, &after_c).ok_or("block write")?;
                        put(a_part, as_, 4, b, at, &after_a).ok_or("block write")?;
                        changed = true;
                    }
                }
            }
            x = x.checked_add(block).ok_or("block column")?;
        }
        y = y.checked_add(block).ok_or("block row")?;
    }
    Ok((changed && out != before).then_some(out))
}

impl Viewer {
    /// The fill tools' settings.
    pub fn fill_tools_settings(&self) -> &FillSettings {
        &self.edit.fill
    }

    /// Replaces the fill tools' settings.
    pub fn set_fill_tools(&mut self, settings: FillSettings) {
        self.edit.fill = settings;
    }

    /// Patterns the document resolves, as `(id, label)` (empty when it carries none).
    pub fn fill_patterns(&self) -> Vec<(String, String)> {
        pattern_refs(self.records.as_ref()).into_iter().map(|(id, _)| (id.clone(), id)).collect()
    }

    fn resolve_pattern(&self, label: &str, id: &str) -> Result<ResolvedPattern, String> {
        let (_, tile) = pattern_refs(self.records.as_ref())
            .into_iter()
            .find(|(i, _)| i == id)
            .ok_or_else(|| format!("{label}: pattern_unavailable: the document resolves no pattern {id}"))?;
        let body = match self.assets.get(&tile.content_digest.digest) {
            Some(b) => b.clone(),
            None => self.run.render_assets().map_err(|e| format!("{label}: pattern_unavailable: {e}"))?.remove(&tile.content_digest.digest).ok_or_else(|| format!("{label}: pattern_unavailable: tile bytes of {id} missing"))?,
        };
        decode_pattern(id, &tile, &body).map_err(|e| format!("{label}: {e}"))
    }

    /// The raster layer `layer_id` over the canvas (refusing every other kind, STU-RAS-170).
    fn fill_target(&self, label: &str, layer_id: &str) -> Result<Target, String> {
        let snap = self.run.snapshot().ok_or_else(|| format!("{label}: nothing open"))?;
        let doc = snap.document();
        let layer = doc.layers.iter().find(|l| l.layer_id == layer_id).ok_or_else(|| format!("{label}: unknown layer {layer_id}"))?;
        let Payload::Raster { tiles } = &layer.payload else {
            return Err(format!("{label}: layer_not_pixel: {layer_id} is a {} layer; rasterize it first (STU-RAS-170)", layer.kind));
        };
        let record = self.records.as_ref().and_then(|r| r.layers.iter().find(|l| l.layer_id == layer_id));
        if record.is_some_and(|r| r.placed_asset.is_some()) {
            return Err(format!("{label}: layer_not_pixel: {layer_id} is a placed asset layer; rasterize it first (STU-RAS-117)"));
        }
        if record.is_some_and(|r| r.fill_layer.is_some()) {
            return Err(format!("{label}: layer_not_pixel: {layer_id} is a fill layer; rasterize it first (STU-RAS-170)"));
        }
        let rects = edit::tile_rects(tiles).ok_or_else(|| format!("{label}: tile sizes are not whole pixels up to 256"))?;
        let on_cell_grid = tiles.iter().all(|t| t.width.value == edit::CELL as f64 && t.height.value == edit::CELL as f64);
        let profile = edit::new_tile_profile(doc, tiles);
        let canvas = self.canvas()?;
        let n = canvas.area();
        if n.checked_mul(16).is_none_or(|b| b > MAX_REGION_BYTES) {
            return Err(format!("{label}: budget_exceeded (canvas {canvas:?})"));
        }
        let n = usize::try_from(n).map_err(|_| format!("{label}: overflow"))?;
        let cw = usize::try_from(canvas.width).map_err(|_| format!("{label}: overflow"))?;
        let mut colour = vec![0u8; n.saturating_mul(12)];
        let mut alpha = vec![0u8; n.saturating_mul(4)];
        let mut fetched = None;
        for (t, r) in tiles.iter().zip(&rects) {
            let Some(hit) = r.intersect(canvas).ok().flatten() else {
                continue;
            };
            let body = match self.assets.get(&t.content_digest.digest) {
                Some(b) => b.clone(),
                None => {
                    if fetched.is_none() {
                        fetched = Some(self.run.render_assets().map_err(|e| format!("{label}: tile bytes {e}"))?);
                    }
                    fetched.as_ref().and_then(|m| m.get(&t.content_digest.digest)).cloned().ok_or_else(|| format!("{label}: tile bytes missing"))?
                }
            };
            let (tw, th) = (r.width as usize, r.height as usize);
            let colour_len = tw.saturating_mul(th).saturating_mul(12);
            if body.len() != colour_len.saturating_add(tw.saturating_mul(th).saturating_mul(4)) {
                return Err(format!("{label}: tile body is {} bytes", body.len()));
            }
            let src_at = offset(*r, hit).ok_or_else(|| format!("{label}: tile offset"))?;
            let dst_at = offset(canvas, hit).ok_or_else(|| format!("{label}: tile offset"))?;
            let c = window(&body[..colour_len], tw.saturating_mul(12), 12, hit, src_at).ok_or_else(|| format!("{label}: tile colour"))?;
            let a = window(&body[colour_len..], tw.saturating_mul(4), 4, hit, src_at).ok_or_else(|| format!("{label}: tile alpha"))?;
            put(&mut colour, cw.saturating_mul(12), 12, hit, dst_at, &c).ok_or_else(|| format!("{label}: tile copy"))?;
            put(&mut alpha, cw.saturating_mul(4), 4, hit, dst_at, &a).ok_or_else(|| format!("{label}: tile copy"))?;
        }
        let colour = encode_rgb(&colour).map_err(|e| format!("{label}: {e}"))?;
        Ok(Target {
            canvas,
            tiles: tiles.clone(),
            rects,
            on_cell_grid,
            profile,
            document: DomainId::parse(&doc.document_id).map_err(|_| format!("{label}: document id"))?,
            layer: DomainId::parse(layer_id).map_err(|_| format!("{label}: layer id"))?,
            colour,
            alpha,
        })
    }

    /// Commits a fill result to `layer_id` as one chronicle `tiles` edit named `label`.
    fn commit_fill(&mut self, label: &str, layer_id: &str, t: Target, result: FillResult) -> Result<u64, String> {
        let linear: Vec<u8> = f32s(&result.colour).flat_map(|v| decode(v).to_le_bytes()).collect();
        let actor = crate::harness::default_actor();
        let mut edited = t.tiles.clone();
        let mut staged = Vec::new();
        for (i, (tile, rect)) in t.tiles.iter().zip(&t.rects).enumerate() {
            if rect.intersect(result.bounds).ok().flatten().is_none() {
                continue;
            }
            let before = match self.assets.get(&tile.content_digest.digest) {
                Some(bytes) => bytes.clone(),
                None => self.run.render_assets().map_err(|e| format!("{label}: tile bytes {e}"))?.remove(&tile.content_digest.digest).ok_or_else(|| format!("{label}: tile bytes missing"))?,
            };
            let profile = DomainId::parse(&tile.colour_profile_id).map_err(|_| format!("{label}: tile colour profile id"))?;
            let target = MixTarget { document: &t.document, layer: &t.layer, profile: &profile, actor: &actor };
            let Some(after) = apply_tile(&target, tile, &before, *rect, &result, &linear).map_err(|e| format!("{label}: {e}"))? else {
                continue;
            };
            let digest = self.run.stage_tile(after.clone()).map_err(|e| format!("{label}: stage {e}"))?;
            staged.push((digest.digest.clone(), after));
            if let Some(slot) = edited.get_mut(i) {
                slot.content_digest = digest;
            }
        }
        // Empty 256-pixel cells the fill covers get new tiles (transparent before), like the brush.
        let mut created = 0usize;
        if t.on_cell_grid && let Ok((ex, ey)) = result.bounds.end() {
            let cell = i64::try_from(edit::CELL).unwrap_or(256);
            let first = |v: i64| v.checked_div_euclid(cell).unwrap_or(0);
            let last = |v: i64| v.checked_sub(1).and_then(|v| v.checked_div_euclid(cell)).unwrap_or(0);
            let empty = vec![0u8; edit::COLOUR_CELL.saturating_add(edit::ALPHA_CELL)];
            let placeholder = hsk_studio_folio::ContentDigest { algorithm: "sha256".into(), digest: "0".repeat(64) };
            for row in first(result.bounds.y)..=last(ey) {
                for column in first(result.bounds.x)..=last(ex) {
                    if t.tiles.iter().any(|tile| tile.column == column && tile.row == row) {
                        continue;
                    }
                    let (Some(x), Some(y)) = (column.checked_mul(cell), row.checked_mul(cell)) else {
                        continue;
                    };
                    let rect = Rect { x, y, width: edit::CELL as u32, height: edit::CELL as u32 };
                    let profile = t.profile.as_deref().ok_or_else(|| format!("{label}: no colour profile for new tiles (no raster tile and no assigned RGB working space)"))?;
                    let mut tile = edit::new_tile(layer_id, column, row, profile, &placeholder, &edited)?;
                    let profile_id = DomainId::parse(profile).map_err(|_| format!("{label}: colour profile id"))?;
                    let target = MixTarget { document: &t.document, layer: &t.layer, profile: &profile_id, actor: &actor };
                    let Some(after) = apply_tile(&target, &tile, &empty, rect, &result, &linear).map_err(|e| format!("{label}: {e}"))? else {
                        continue;
                    };
                    let digest = self.run.stage_tile(after.clone()).map_err(|e| format!("{label}: stage {e}"))?;
                    tile.content_digest = digest.clone();
                    edited.push(tile);
                    staged.push((digest.digest.clone(), after));
                    created = created.saturating_add(1);
                }
            }
        }
        if staged.is_empty() {
            return Err(format!("{label}: the fill changed no pixel"));
        }
        let count = staged.len();
        self.assets.extend(staged);
        if !self.run.set_tiles(label, layer_id, edited) {
            return Err(format!("{label} on {layer_id} refused: {}", self.op_error()));
        }
        let revision = self.run.snapshot().map_or(0, |s| s.document().revision);
        self.dirty = true;
        self.say(format!("{label} on {layer_id} ({count} tile(s), {created} new, revision {revision})"));
        Ok(revision)
    }

    fn fill_paint(opts: &FillSettings) -> FillPaint {
        FillPaint { mode: opts.mode, opacity: (opts.opacity_pct / 100.0) as f32, seed: 0 }
    }

    fn foreground(&self) -> [f32; 3] {
        self.edit.brush.rgb8.map(|v| f32::from(v) / 255.0)
    }

    /// Runs `f` and reports its error on the status line.
    fn reported(&mut self, f: impl FnOnce(&mut Self) -> Result<u64, String>) -> Result<u64, String> {
        let result = f(self);
        if let Err(e) = &result {
            self.say(e.clone());
        }
        result
    }

    /// Gradient tool: a gradient on the raster layer `layer_id` along the drag `from -> to`
    /// (document pixels), clipped to the selection; one undoable step. Returns the new revision.
    pub fn fill_gradient(&mut self, layer_id: &str, from: [f64; 2], to: [f64; 2], opts: &FillSettings) -> Result<u64, String> {
        self.reported(|v| {
            let label = "fill_gradient";
            let t = v.fill_target(label, layer_id)?;
            let fg = v.foreground();
            let (colour, alpha) = match &opts.stops {
                Some((c, a)) => (c.clone(), a.clone()),
                None => (
                    vec![ColourStop { location: 0, midpoint: 50.0, colour: fg }, ColourStop { location: pf::LOCATION_END, midpoint: 50.0, colour: fg }],
                    vec![TransparencyStop { location: 0, midpoint: 50.0, opacity: 100.0 }, TransparencyStop { location: pf::LOCATION_END, midpoint: 50.0, opacity: 0.0 }],
                ),
            };
            let gradient = RasterGradient {
                form: GradientForm::Stops,
                geometry: opts.geometry,
                colour_stops: &colour,
                transparency_stops: &alpha,
                smoothness: SMOOTHNESS_FULL,
                reverse: opts.reverse,
                dither: opts.dither,
                dither_seed: 0,
            };
            let view = ColourView { bounds: t.canvas, colour: &t.colour, alpha: &t.alpha };
            let result = pf::gradient_fill(&view, v.edit.selection.as_ref(), t.canvas, DragLine { from, to }, &gradient, &Self::fill_paint(opts), &ToolLimits::default())
                .map_err(|e| format!("{label}: {}", e.code()))?;
            v.commit_fill(label, layer_id, t, result)
        })
    }

    /// The bucket / Edit > Fill content of `opts`.
    fn fill_content(&self, label: &str, opts: &FillSettings) -> Result<Option<ResolvedPattern>, String> {
        match &opts.content {
            FillSource::Foreground => Ok(None),
            FillSource::Pattern(id) => self.resolve_pattern(label, id).map(Some),
        }
    }

    /// The rendered composite in display encoding: `(bounds, colour, alpha)`.
    fn merged_frame(&mut self, label: &str) -> Result<(Rect, Vec<u8>, Vec<u8>), String> {
        self.render_if_dirty();
        let frame = self.frame.as_ref().ok_or_else(|| format!("{label}: sample_merged: no rendered frame"))?;
        let n = (frame.width as usize).saturating_mul(frame.height as usize);
        if frame.rgba8.len() != n.saturating_mul(4) {
            return Err(format!("{label}: sample_merged: the frame lives only in the display texture"));
        }
        let mut colour = Vec::with_capacity(n.saturating_mul(12));
        let mut alpha = Vec::with_capacity(n.saturating_mul(4));
        for p in frame.rgba8.chunks_exact(4) {
            for c in &p[..3] {
                colour.extend_from_slice(&(f32::from(*c) / 255.0).to_le_bytes());
            }
            alpha.extend_from_slice(&(f32::from(p[3]) / 255.0).to_le_bytes());
        }
        Ok((Rect { x: frame.x, y: frame.y, width: frame.width, height: frame.height }, colour, alpha))
    }

    /// Paint Bucket: fills the region similar to document pixel `(x, y)` of the raster layer
    /// `layer_id` (or of the composite with sample merged), clipped to the selection; one undoable
    /// step. Returns the new revision.
    pub fn flood_fill(&mut self, layer_id: &str, x: f64, y: f64, opts: &FillSettings) -> Result<u64, String> {
        self.reported(|v| {
            let label = "flood_fill";
            let t = v.fill_target(label, layer_id)?;
            let pattern = v.fill_content(label, opts)?;
            let merged = if opts.sample_merged { Some(v.merged_frame(label)?) } else { None };
            let merged_view = merged.as_ref().map(|(b, c, a)| ColourView { bounds: *b, colour: c, alpha: a });
            let content = match &pattern {
                Some(p) => FillContent::Pattern(PatternTile { id: &p.id, name: &p.name, width: p.width, height: p.height, pixels: &p.pixels, origin: [0, 0] }),
                None => FillContent::Colour(v.foreground()),
            };
            let view = ColourView { bounds: t.canvas, colour: &t.colour, alpha: &t.alpha };
            let flood = FloodOptions { tolerance: opts.tolerance, contiguous: opts.contiguous, anti_alias: opts.anti_alias };
            let result = pf::flood_fill(&view, merged_view.as_ref(), [x, y], &flood, &content, &Self::fill_paint(opts), v.edit.selection.as_ref(), t.canvas, &ToolLimits::default())
                .map_err(|e| format!("{label}: {}", e.code()))?;
            v.commit_fill(label, layer_id, t, result)
        })
    }

    /// Edit > Fill: fills the selection (everything without one) of the raster layer `layer_id`
    /// with the foreground colour or a document pattern; one undoable step.
    pub fn fill_selection(&mut self, layer_id: &str, opts: &FillSettings) -> Result<u64, String> {
        self.reported(|v| {
            let label = "fill_selection";
            let t = v.fill_target(label, layer_id)?;
            let pattern = v.fill_content(label, opts)?;
            let content = match &pattern {
                Some(p) => FillContent::Pattern(PatternTile { id: &p.id, name: &p.name, width: p.width, height: p.height, pixels: &p.pixels, origin: [0, 0] }),
                None => FillContent::Colour(v.foreground()),
            };
            let view = ColourView { bounds: t.canvas, colour: &t.colour, alpha: &t.alpha };
            let result = pf::fill_selection(&view, v.edit.selection.as_ref(), t.canvas, &content, &Self::fill_paint(opts), &ToolLimits::default())
                .map_err(|e| format!("{label}: {}", e.code()))?;
            v.commit_fill(label, layer_id, t, result)
        })
    }

    /// The layer the canvas actions fill: the selected one.
    fn fill_layer(&mut self, label: &str) -> Result<String, String> {
        match self.selected.clone() {
            Some(l) => Ok(l),
            None => {
                let e = format!("{label}: select a raster layer in the layer panel first");
                self.say(e.clone());
                Err(e)
            }
        }
    }

    /// A finished canvas drag of the Gradient tool, document pixels, with the current settings.
    pub fn fill_drag(&mut self, from: [f64; 2], to: [f64; 2]) -> Result<u64, String> {
        let layer = self.fill_layer("fill_gradient")?;
        let s = self.edit.fill.clone();
        self.fill_gradient(&layer, from, to, &s)
    }

    /// A canvas click of the Paint Bucket at document point `(x, y)` with the current settings.
    pub fn fill_click(&mut self, x: f64, y: f64) -> Result<u64, String> {
        let layer = self.fill_layer("flood_fill")?;
        let s = self.edit.fill.clone();
        self.flood_fill(&layer, x, y, &s)
    }

    /// Structured fill-tool settings for agents (part of `state_json` `edit.fill`).
    pub fn fill_tools_json(&self) -> Value {
        let s = &self.edit.fill;
        json!({
            "geometry": s.geometry.key(), "mode": s.mode.key(), "opacity_pct": s.opacity_pct, "reverse": s.reverse, "dither": s.dither,
            "custom_stops": s.stops.is_some(), "tolerance": s.tolerance, "contiguous": s.contiguous, "anti_alias": s.anti_alias,
            "sample_merged": s.sample_merged,
            "content": match &s.content { FillSource::Foreground => json!("foreground"), FillSource::Pattern(id) => json!({"pattern": id}) },
            "patterns": self.fill_patterns().into_iter().map(|(id, _)| id).collect::<Vec<_>>(),
        })
    }

    /// Option row of the Gradient tool and the Paint Bucket (stable `fill.*` ids), shown while one
    /// of them is the active tool.
    pub(super) fn fill_options_ui(&mut self, ui: &mut egui::Ui, drawn: &mut Vec<String>) {
        if !matches!(self.edit.tool, Tool::Gradient | Tool::FloodFill) {
            return;
        }
        let mut s = self.edit.fill.clone();
        ui.separator();
        super::wrap::wrap_measured(ui, widget::id(GEOMETRY), |ui| {
            egui::ComboBox::from_label("gradient").selected_text(s.geometry.key()).show_ui(ui, |ui| {
                for g in GradientGeometry::ALL {
                    if g != GradientGeometry::ShapeBurst {
                        ui.selectable_value(&mut s.geometry, g, g.key());
                    }
                }
            });
        });
        drawn.push(GEOMETRY.into());
        super::wrap::wrap_measured(ui, widget::id(MODE), |ui| {
            egui::ComboBox::from_label("fill mode").selected_text(s.mode.key()).show_ui(ui, |ui| {
                for m in MODES {
                    ui.selectable_value(&mut s.mode, m, m.key());
                }
            });
        });
        drawn.push(MODE.into());
        super::wrap::wrap_measured(ui, widget::id(OPACITY), |ui| ui.add(egui::Slider::new(&mut s.opacity_pct, 0.0..=100.0).text("fill opacity")));
        drawn.push(OPACITY.into());
        super::wrap::wrap_measured(ui, widget::id(REVERSE), |ui| ui.checkbox(&mut s.reverse, "reverse"));
        drawn.push(REVERSE.into());
        super::wrap::wrap_measured(ui, widget::id(DITHER), |ui| ui.checkbox(&mut s.dither, "dither"));
        drawn.push(DITHER.into());
        super::wrap::wrap_measured(ui, widget::id(TOLERANCE), |ui| ui.add(egui::Slider::new(&mut s.tolerance, 0.0..=255.0).text("bucket tolerance")));
        drawn.push(TOLERANCE.into());
        super::wrap::wrap_measured(ui, widget::id(CONTIGUOUS), |ui| ui.checkbox(&mut s.contiguous, "contiguous"));
        drawn.push(CONTIGUOUS.into());
        super::wrap::wrap_measured(ui, widget::id(ANTI_ALIAS), |ui| ui.checkbox(&mut s.anti_alias, "anti-alias"));
        drawn.push(ANTI_ALIAS.into());
        super::wrap::wrap_measured(ui, widget::id(SAMPLE_MERGED), |ui| ui.checkbox(&mut s.sample_merged, "all layers"));
        drawn.push(SAMPLE_MERGED.into());
        let patterns = self.fill_patterns();
        if !patterns.is_empty() {
            let current = match &s.content {
                FillSource::Foreground => "foreground".to_string(),
                FillSource::Pattern(id) => format!("pattern {id}"),
            };
            super::wrap::wrap_measured(ui, widget::id(CONTENT), |ui| {
                egui::ComboBox::from_label("fill with").selected_text(current).show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.content, FillSource::Foreground, "foreground");
                    for (id, name) in &patterns {
                        ui.selectable_value(&mut s.content, FillSource::Pattern(id.clone()), format!("pattern {name}"));
                    }
                });
            });
            drawn.push(CONTENT.into());
        }
        let fill = super::wrap::wrap_item(ui, widget::id(FILL_SELECTION), "Fill selection", egui::TextStyle::Button, |ui| ui.button("Fill selection"))
            .on_hover_text("Edit > Fill: fill the selection (or the whole layer) of the selected raster layer with the fill content")
            .clicked();
        drawn.push(FILL_SELECTION.into());
        self.edit.fill = s;
        if fill && let Ok(layer) = self.fill_layer("fill_selection") {
            let opts = self.edit.fill.clone();
            let _ = self.fill_selection(&layer, &opts);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hsk_studio_folio::{StudioEffectFill, StudioFillBounds, StudioFillLayer, StudioRasterLayerRecord};

    fn tile(digest: &str, w: u32, h: u32) -> TileRef {
        serde_json::from_value(json!({
            "object_key": "pattern", "layer_id": "layer", "column": 0, "row": 0,
            "width": {"value": w, "unit": "px"}, "height": {"value": h, "unit": "px"},
            "format": "rgb_f32le", "colour_profile_id": "profile", "schema_id": "hsk.studio.raster_tile@1",
            "content_digest": {"algorithm": "sha256", "digest": digest}, "artifact_manifest_id": "package/layer/pattern",
        }))
        .unwrap()
    }

    #[test]
    fn a_fill_layer_pattern_tile_resolves_for_the_bucket() {
        // A 2x1 pattern: linear 0 and 1 (encoded 0 and 1), alpha 1 and 0.5.
        let mut body = Vec::new();
        for v in [0.0f32, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0, 0.5] {
            body.extend_from_slice(&v.to_le_bytes());
        }
        let mut records = StudioRasterRecords::new("doc");
        records.layers.push(StudioRasterLayerRecord {
            layer_id: "fill-1".into(),
            adjustment: None,
            mask: None,
            advanced_blending: None,
            effects: None,
            smart_filters: None,
            placed_asset: None,
            fill_layer: Some(StudioFillLayer {
                bounds: StudioFillBounds { x: 0, y: 0, width: 2, height: 1 },
                fill: StudioFill::Pattern {
                    pattern: StudioEffectFill { schema_id: "hsk.studio.pattern@1".into(), object_id: "pat-7".into() },
                    pattern_tile: Some(tile("d1", 2, 1)),
                    scale: 100.0,
                    phase_x: 0.0,
                    phase_y: 0.0,
                    link_with_layer: false,
                },
            }),
        });
        let refs = pattern_refs(Some(&records));
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].0, "pat-7");
        let p = decode_pattern("pat-7", &refs[0].1, &body).unwrap();
        assert_eq!((p.width, p.height), (2, 1));
        let want = [[0.0, 0.0, 0.0, 1.0], [1.0, 1.0, 1.0, 0.5]];
        assert_eq!(p.pixels.len(), 2);
        for (got, want) in p.pixels.iter().zip(want) {
            assert!(got.iter().zip(want).all(|(a, b)| (a - b).abs() <= 1e-6), "{got:?} {want:?}");
        }
        // A wrong body length or a non-whole size is unavailable, typed.
        assert!(decode_pattern("pat-7", &refs[0].1, &body[..8]).unwrap_err().contains("pattern_unavailable"));
        assert!(decode_pattern("pat-7", &tile("d1", 0, 1), &body).unwrap_err().contains("pattern_unavailable"));
        // No records: nothing to offer.
        assert!(pattern_refs(None).is_empty());
    }
}
