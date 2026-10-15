//! Document -> pixels for the desktop viewer (feature `viewer`): the snapshot is lowered by
//! hsk-studio-composite into a pigment render plan and executed by hsk-studio-render-gpu, or by
//! hsk-studio-render-cpu when no GPU adapter or device is available (or the GPU refuses the plan).
//! Output is 8-bit sRGB-encoded RGBA with straight alpha for display.
//!
//! View-only document root (never committed): a document's graph lists its root layers as several
//! outputs and an imported document leaves `blending_space` unassigned, while
//! `lower_to_render_plan` renders ONE root node of a document with a bound blending profile. The
//! viewer therefore renders a view copy of the snapshot that wraps the root outputs in one
//! isolated, normal, 100 % group ([`VIEW_ROOT_LAYER`]) and binds `blending_space` to the
//! document's RGB working profile when it is unassigned. Both facts are listed in
//! [`Frame::notes`].
//!
//! Raster records (lane SMB-HARNESS-RENDER-RECORDS): the document's Folio `StudioRasterRecords`
//! (the package's records import entry: adjustments, mask parameters, advanced blending, effect
//! stacks, smart filters) go through composite's adapter (`evaluate_with_inputs` +
//! `lower_with_inputs` with `RenderInputs.records`), and the envelope composite builds is the one
//! the backend executes; a frame with an envelope uses its default output stage (unit range),
//! noted `records_envelope`. Layer masks and smart-filter masks are served as stitched coverage
//! planes (the mask tiles over the union of their placements, hashed as composite expects).
//! Without records the frame is exactly the records-free path of before.
#![deny(clippy::arithmetic_side_effects)]

use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{self as composite, BlendSpace, CompositeError, Exactness, NodeAddr, PreparedText, PreparedVectors, TileInfo};
use hsk_studio_folio::{self as folio, Inspection, ProfileBinding, Snapshot, TileRef};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, DisplayTransform, ExecuteRequest, NoVector, OutputStage, OutputTarget, PlanExtensions, PlanSources,
    RenderPlan, SampleFormat, SampleRange, SinkError, SourceKey, TilePlanes, TileSink,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Layer id of the view-only root group (never written to a document or package).
pub const VIEW_ROOT_LAYER: &str = "SLYR-ffffffff-ffff-7fff-bfff-ffffffffffff";
/// Pigment tile edge used by the Studio importers (`hsk_studio_interop_psd::TILE_EDGE`).
const CELL: usize = 256;
/// Largest canvas the viewer renders (pixels); larger documents are refused, never truncated.
pub const MAX_VIEW_PIXELS: u64 = 64 * 1024 * 1024;
/// Working memory the viewer grants one render.
const MAX_WORKING_BYTES: u64 = 2 << 30;

/// Which backend to try first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefer {
    /// render-gpu, falling back to render-cpu when the GPU is unavailable or refuses the plan.
    Gpu,
    /// render-cpu only.
    Cpu,
}

/// A host GPU display path (lane SMB-RENDER-GPU-SHARED-DEVICE): the plan renders on the host's own
/// wgpu device straight into the texture the UI draws, with no read-back and no upload. Feature
/// `desktop` implements it over eframe's wgpu device ([`super::display::WgpuDisplay`]); window-free
/// callers pass none and keep the read-back path.
pub trait GpuDisplay {
    /// Render `plan` into the host's display texture and return its egui texture id.
    fn present(&mut self, plan: &RenderPlan<NoVector>, request: &ExecuteRequest, sources: &dyn PlanSources, cancel: &CancellationToken) -> Result<egui::TextureId, String>;
}

/// A rendered view.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Document pixel of the frame's top-left corner (the artboard origin).
    pub x: i64,
    pub y: i64,
    /// `width * height * 4` bytes, sRGB-encoded, straight (unmultiplied) alpha. Empty when the frame
    /// lives only in the host's display texture (`texture` is set).
    pub rgba8: Vec<u8>,
    /// `gpu`, `gpu-shared` (rendered into the display texture on the host's device) or `cpu`.
    pub backend: &'static str,
    /// The host display texture holding the frame (`gpu-shared`), `None` for read-back frames.
    pub texture: Option<egui::TextureId>,
    /// Document revision the frame shows.
    pub revision: u64,
    /// View assumptions and fallbacks (stable codes with detail).
    pub notes: Vec<String>,
}

/// Why a frame could not be produced (stable `code`, human `detail`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderFail {
    pub code: &'static str,
    pub detail: String,
}

impl RenderFail {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self { code, detail: detail.into() }
    }
}

impl std::fmt::Display for RenderFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

fn composite_fail(e: CompositeError) -> RenderFail {
    RenderFail::new("composite", format!("{e:?}"))
}

/// The viewer's plan envelope: extended sample range with the display transform Clip as the
/// output stage (lane SMB-RENDER-SCENE-DISPLAY; F-R31-3). Scene-referred values outside [0, 1] (a
/// developed camera raw with strong gains) are clipped for display only; the document keeps the
/// unclipped values and exports keep their own clamping. The backends' deprecated
/// `with_display_transform(Clip)` aliases executed a default-stage plan as exactly this envelope.
pub(crate) fn preview_extensions() -> PlanExtensions {
    PlanExtensions::default().with_output(OutputStage::new(SampleRange::Extended, Some(DisplayTransform::Clip)))
}

/// Tile bytes by lowercase SHA-256 hex (the package's asset map).
pub type Assets = BTreeMap<String, Vec<u8>>;

/// 8-bit sRGB straight RGBA of linear straight pixels, exactly as the viewer encodes a frame (for
/// callers that render the same plan themselves and compare).
pub fn encode_rgba8(pixels: &[[f32; 4]]) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels.len().saturating_mul(4));
    for [r, g, b, a] in pixels {
        out.extend([encode(*r), encode(*g), encode(*b), unit8(*a)]);
    }
    out
}

/// A mask's coverage tiles stitched into one plane over the union of their placements (the plane
/// composite hashes through `TileInfoSource::mask_sha256` and the backend reads).
struct StitchedMask {
    bounds: Rect,
    format: &'static str,
    stride: u64,
    bytes: Vec<u8>,
    sha256: [u8; 32],
}

fn stitch_mask(assets: &Assets, tiles: &[TileRef]) -> Option<StitchedMask> {
    let first = tiles.first()?;
    let (format, bpp): (&'static str, usize) = match first.format.as_str() {
        "coverage_u8" => ("coverage_u8", 1),
        "coverage_u16le" => ("coverage_u16le", 2),
        _ => return None,
    };
    if tiles.iter().any(|t| t.format != format) {
        return None;
    }
    let side = |l: &folio::Length| (l.value.is_finite() && l.value >= 1.0 && l.value <= 65_536.0 && l.value.fract() == 0.0).then_some(l.value as u32);
    let (mut gw, mut gh) = (0u32, 0u32);
    for t in tiles {
        gw = gw.max(side(&t.width)?);
        gh = gh.max(side(&t.height)?);
    }
    let rect_of = |t: &TileRef| -> Option<Rect> {
        Some(Rect { x: t.column.checked_mul(i64::from(gw))?, y: t.row.checked_mul(i64::from(gh))?, width: side(&t.width)?, height: side(&t.height)? })
    };
    let mut bounds: Option<Rect> = None;
    for t in tiles {
        let r = rect_of(t)?;
        bounds = Some(match bounds {
            None => r,
            Some(b) => {
                let x0 = b.x.min(r.x);
                let y0 = b.y.min(r.y);
                let x1 = b.x.checked_add(i64::from(b.width))?.max(r.x.checked_add(i64::from(r.width))?);
                let y1 = b.y.checked_add(i64::from(b.height))?.max(r.y.checked_add(i64::from(r.height))?);
                Rect { x: x0, y: y0, width: u32::try_from(x1.checked_sub(x0)?).ok()?, height: u32::try_from(y1.checked_sub(y0)?).ok()? }
            }
        });
    }
    let bounds = bounds?;
    if bounds.area() > MAX_VIEW_PIXELS {
        return None;
    }
    let bw = usize::try_from(bounds.width).ok()?;
    let stride = bw.checked_mul(bpp)?;
    let mut bytes = vec![0u8; stride.checked_mul(usize::try_from(bounds.height).ok()?)?];
    for t in tiles {
        let r = rect_of(t)?;
        let body = assets.get(&t.content_digest.digest)?;
        let row = usize::try_from(r.width).ok()?.checked_mul(bpp)?;
        if body.len() != row.checked_mul(usize::try_from(r.height).ok()?)? {
            return None;
        }
        let dx = usize::try_from(r.x.checked_sub(bounds.x)?).ok()?.checked_mul(bpp)?;
        for y in 0..usize::try_from(r.height).ok()? {
            let dy = usize::try_from(r.y.checked_sub(bounds.y)?).ok()?.checked_add(y)?;
            let at = dy.checked_mul(stride)?.checked_add(dx)?;
            let src = y.checked_mul(row)?;
            bytes.get_mut(at..at.checked_add(row)?)?.copy_from_slice(body.get(src..src.checked_add(row)?)?);
        }
    }
    let sha256 = hsk_studio_prism::profile_hash(&bytes);
    Some(StitchedMask { bounds, format, stride: u64::try_from(stride).ok()?, bytes, sha256 })
}

/// Renders the document `snap` with the tile bytes in `assets` (read-back path).
pub fn render(snap: &Snapshot, assets: &Assets, prefer: Prefer, gpu: Option<&hsk_studio_render_gpu::GpuBackend>, cancel: &CancellationToken) -> Result<Frame, RenderFail> {
    render_to(snap, assets, prefer, gpu, None, cancel)
}

/// [`render`] with the document's text layers drawn from `text` (lane SMB-HARNESS-UI-3: composite
/// `evaluate_with_text` + `lower_to_render_plan_with_text`, the text masks served to the backend).
pub fn render_with_text(snap: &Snapshot, assets: &Assets, text: &PreparedText, prefer: Prefer, gpu: Option<&hsk_studio_render_gpu::GpuBackend>, cancel: &CancellationToken) -> Result<Frame, RenderFail> {
    render_inner(snap, assets, Some(text), prefer, gpu, None, cancel)
}

/// [`render`] that first tries the host display path when `display` is set and `prefer` is
/// [`Prefer::Gpu`]; a display failure is noted (`gpu_shared_fallback`) and the read-back path runs.
pub fn render_to(
    snap: &Snapshot,
    assets: &Assets,
    prefer: Prefer,
    gpu: Option<&hsk_studio_render_gpu::GpuBackend>,
    display: Option<&mut dyn GpuDisplay>,
    cancel: &CancellationToken,
) -> Result<Frame, RenderFail> {
    render_inner(snap, assets, None, prefer, gpu, display, cancel)
}

/// [`render_to`] with optional prepared text.
pub fn render_inner(
    snap: &Snapshot,
    assets: &Assets,
    text: Option<&PreparedText>,
    prefer: Prefer,
    gpu: Option<&hsk_studio_render_gpu::GpuBackend>,
    display: Option<&mut dyn GpuDisplay>,
    cancel: &CancellationToken,
) -> Result<Frame, RenderFail> {
    render_full(snap, assets, text, None, prefer, gpu, display, cancel)
}

/// [`render_inner`] with optional prepared vector layers (lane SMB-ILLUSTRATOR-PEN: composite
/// `evaluate_with_inputs` + `lower_with_inputs`, the vector coverage planes and painted rows served
/// to the backend). Without vector layers in the document this is exactly [`render_inner`].
#[allow(clippy::too_many_arguments)]
pub fn render_full(
    snap: &Snapshot,
    assets: &Assets,
    text: Option<&PreparedText>,
    vectors: Option<&PreparedVectors>,
    prefer: Prefer,
    gpu: Option<&hsk_studio_render_gpu::GpuBackend>,
    display: Option<&mut dyn GpuDisplay>,
    cancel: &CancellationToken,
) -> Result<Frame, RenderFail> {
    render_full_with_records(snap, assets, text, vectors, None, prefer, gpu, display, cancel)
}

/// [`render_full`] with the document's raster records (lane SMB-HARNESS-RENDER-RECORDS; module
/// docs): `None` or an empty set renders exactly as [`render_full`].
#[allow(clippy::too_many_arguments)]
pub fn render_full_with_records(
    snap: &Snapshot,
    assets: &Assets,
    text: Option<&PreparedText>,
    vectors: Option<&PreparedVectors>,
    records: Option<&folio::StudioRasterRecords>,
    prefer: Prefer,
    gpu: Option<&hsk_studio_render_gpu::GpuBackend>,
    display: Option<&mut dyn GpuDisplay>,
    cancel: &CancellationToken,
) -> Result<Frame, RenderFail> {
    let mut notes = Vec::new();
    let view = view_snapshot(snap, cancel, &mut notes)?;
    let doc = view.document();
    let artboard = doc.artboards.first().ok_or_else(|| RenderFail::new("no_artboard", "the document has no artboard to frame the view"))?;
    let extent = extent_of(&artboard.bounds)?;
    let profile_sha256 = hsk_studio_prism::profile_hash(hsk_studio_prism::WORKING_PROFILE_LINEAR_SRGB);
    let space = ColourTag { profile_sha256, blend_space: BlendSpace::LinearLight };
    let target = OutputTarget { extent, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space };

    let plan = composite::lower(&view, doc.revision, BlendSpace::LinearLight, &composite::Limits::default(), cancel).map_err(composite_fail)?;
    let root = NodeAddr { layer_id: VIEW_ROOT_LAYER.to_string(), operation_key: "mix".into(), port_key: "out".into() };
    let tiles = TileTable::new(assets);
    let empty = PreparedText::default();
    let has_text = doc.layers.iter().any(|l| matches!(l.payload, folio::Payload::Text { .. }));
    let has_vector = doc.layers.iter().any(|l| matches!(l.payload, folio::Payload::Vector { .. }));
    let mut inputs_lowering: Option<composite::InputsLowering> = None;
    // Raster records of the document, bound to the view copy's document id.
    let records = records.filter(|r| !r.layers.is_empty() || r.global_light.is_some()).map(|r| {
        let mut r = r.clone();
        r.document_id = doc.document_id.clone();
        r
    });
    let vectors = vectors.filter(|_| has_vector);
    let (lowering, text_masks) = if vectors.is_some() || records.is_some() {
        let inputs = composite::RenderInputs { records: records.as_ref(), text: if has_text { Some(text.unwrap_or(&empty)) } else { None }, vectors };
        let report = composite::evaluate_with_inputs(&plan, &view, &inputs, cancel).map_err(composite_fail)?;
        let lowered = composite::lower_with_inputs(&plan, &report, &view, &inputs, &root, target, &tiles).map_err(composite_fail)?;
        let lowering = composite::RenderLowering { plan: lowered.plan.clone(), sources: lowered.sources.clone(), mask_sources: lowered.mask_sources.clone(), exactness: lowered.exactness.clone() };
        let masks = has_text.then(|| (lowered.text_masks.clone(), text.unwrap_or(&empty)));
        inputs_lowering = Some(lowered);
        (lowering, masks)
    } else if has_text {
        let prepared = text.unwrap_or(&empty);
        let report = composite::evaluate_with_text(&plan, &view, prepared, cancel).map_err(composite_fail)?;
        let lowered = composite::lower_to_render_plan_with_text(&plan, &report, &view, &root, target, &tiles, prepared).map_err(composite_fail)?;
        (lowered.lowering, Some((lowered.text_masks, prepared)))
    } else {
        let report = composite::evaluate(&plan, &view, cancel).map_err(composite_fail)?;
        (composite::lower_to_render_plan(&plan, &report, &view, &root, target, &tiles).map_err(composite_fail)?, None)
    };
    let vector = inputs_lowering.as_ref().zip(vectors);
    // The envelope composite built from the records; without one the viewer's preview stage.
    let envelope = inputs_lowering.as_ref().filter(|_| records.is_some()).map(|l| l.extensions.clone()).filter(|e| !e.is_empty());
    let extensions = match &envelope {
        Some(e) => {
            notes.push("records_envelope: the raster records' envelope with the default output stage (unit range)".into());
            e.clone()
        }
        None => preview_extensions(),
    };
    let masks: Vec<(SourceKey, StitchedMask)> = lowering.mask_sources.iter().filter_map(|(k, t)| Some((*k, stitch_mask(assets, t)?))).collect();
    let sources = Sources { tiles: &tiles, table: &lowering.sources, space, text: text_masks.as_ref().map(|(m, p)| (m, *p)), vector, masks: &masks };
    let request = ExecuteRequest { expected_revision: lowering.plan.revision, min_exactness: Exactness::Approximate, deadline: None, max_working_bytes: MAX_WORKING_BYTES };
    if prefer == Prefer::Gpu
        && envelope.is_none()
        && let Some(display) = display
    {
        match display.present(&lowering.plan, &request, &sources, cancel) {
            Ok(texture) => {
                return Ok(Frame { width: extent.width, height: extent.height, x: extent.x, y: extent.y, rgba8: Vec::new(), backend: "gpu-shared", texture: Some(texture), revision: doc.revision, notes });
            }
            Err(e) => notes.push(format!("gpu_shared_fallback: {e}; read-back path used")),
        }
    }
    let mut sink = Canvas::new(extent)?;

    let mut backend = "cpu";
    let mut done = false;
    if prefer == Prefer::Gpu {
        match gpu {
            None => notes.push("gpu_not_created: render-cpu used".into()),
            Some(gpu) => match gpu.execute_extended_detailed(&lowering.plan, &extensions, &request, &sources, cancel, &mut sink) {
                Ok((receipt, _)) => {
                    if receipt.exactness.operators.iter().any(|o| o.key == hsk_studio_render_gpu::DISPLAY_CLIP_OPERATOR) {
                        notes.push(format!("{}: samples outside [0, 1] clipped for display (render-gpu DisplayTransform::Clip, Approximate)", hsk_studio_render_gpu::DISPLAY_CLIP_OPERATOR));
                    }
                    backend = "gpu";
                    done = true;
                }
                Err(e) => {
                    notes.push(format!("gpu_fallback: {e:?}; render-cpu used"));
                    sink = Canvas::new(extent)?;
                }
            },
        }
    }
    if !done {
        // Scene-referred values outside [0, 1] (a developed camera raw with strong gains) are shown
        // through the plan's output stage, display transform Clip ([`preview_extensions`]; lane
        // SMB-RENDER-SCENE-DISPLAY, recorded in the receipt ledger); the document keeps the unclipped values.
        let cpu = hsk_studio_render_cpu::Renderer::new(hsk_studio_render_cpu::DEFAULT_CHUNK_SIDE)
            .map_err(|e| RenderFail::new("render_cpu", format!("{e:?}")))?;
        let receipt = cpu
            .execute_extended_detailed(&lowering.plan, &extensions, &request, &sources, cancel, &mut sink)
            .map_err(|e| RenderFail::new("render_cpu", format!("{e:?}")))?;
        if receipt.exactness.operators.iter().any(|o| o.key == hsk_studio_render_cpu::DISPLAY_CLIP_OPERATOR) {
            notes.push(format!("{}: samples outside [0, 1] clipped for display (render-cpu DisplayTransform::Clip, Approximate)", hsk_studio_render_cpu::DISPLAY_CLIP_OPERATOR));
        }
    }
    Ok(Frame { width: extent.width, height: extent.height, x: extent.x, y: extent.y, rgba8: sink.to_rgba8(), backend, texture: None, revision: doc.revision, notes })
}

fn extent_of(bounds: &folio::Bounds) -> Result<Rect, RenderFail> {
    let whole = |v: f64| v.is_finite() && v.fract() == 0.0 && v.abs() < f64::from(u32::MAX);
    let b = [bounds.x.value, bounds.y.value, bounds.width.value, bounds.height.value];
    if !b.iter().all(|v| whole(*v)) || b[2] < 1.0 || b[3] < 1.0 {
        return Err(RenderFail::new("artboard_bounds", format!("{b:?} are not whole positive pixels")));
    }
    // Checked above: finite, integral and within u32 range.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let rect = Rect { x: b[0] as i64, y: b[1] as i64, width: b[2] as u32, height: b[3] as u32 };
    if u64::from(rect.width).saturating_mul(u64::from(rect.height)) > MAX_VIEW_PIXELS {
        return Err(RenderFail::new("canvas_too_large", format!("{}x{} exceeds {MAX_VIEW_PIXELS} pixels", rect.width, rect.height)));
    }
    Ok(rect)
}

/// The view copy described in the module docs.
pub fn view_snapshot(snap: &Snapshot, cancel: &CancellationToken, notes: &mut Vec<String>) -> Result<Snapshot, RenderFail> {
    let doc = snap.document();
    if doc.layers.iter().any(|l| l.layer_id == VIEW_ROOT_LAYER) {
        return Err(RenderFail::new("view_root_id_taken", VIEW_ROOT_LAYER));
    }
    let mut wire: Value = serde_json::from_slice(snap.encoded_bytes()).map_err(|e| RenderFail::new("document_json", e.to_string()))?;
    if matches!(doc.blending_space, ProfileBinding::Unassigned(())) {
        let ProfileBinding::Assigned(rgb) = &doc.working_space_rgb else {
            return Err(RenderFail::new("no_working_profile", "blending_space and working_space_rgb are unassigned"));
        };
        wire["blending_space"] = json!(rgb);
        notes.push(format!("view_blending_space_assumed: unassigned blending_space bound to working_space_rgb {rgb} (view only)"));
    }
    let outputs = wire["graph"]["outputs"].as_array().cloned().unwrap_or_default();
    if outputs.is_empty() {
        return Err(RenderFail::new("no_graph_outputs", "the document graph has no outputs to render"));
    }
    if let Some(layers) = wire["layers"].as_array_mut() {
        for layer in layers.iter_mut() {
            if layer["parent"].is_null() {
                layer["parent"] = json!({"node_kind": "layer", "node_id": VIEW_ROOT_LAYER});
            }
        }
        layers.insert(
            0,
            json!({
                "schema_id": "hsk.studio.layer@1", "layer_id": VIEW_ROOT_LAYER, "name": "view root", "parent": null,
                "kind": "group", "payload": {"payload_kind": "group"}, "visible": true,
                "appearance": {"opacity": 100.0, "fill_opacity": 100.0, "blend_mode": folio::BLEND_NORMAL, "clipped": false, "isolation": "isolated"},
            }),
        );
    }
    let port = |key: &str, cardinality: &str, semantic: &str| json!({"port_key": key, "value_type": "image", "cardinality": cardinality, "semantic": semantic});
    if let Some(ops) = wire["graph"]["operations"].as_array_mut() {
        ops.push(json!({
            "layer_id": VIEW_ROOT_LAYER, "operation_key": "mix", "operation_kind": "composite",
            "inputs": [port("inputs", "many", "ordered_composite")], "outputs": [port("out", "one", "composite_result")],
        }));
    }
    if let Some(edges) = wire["graph"]["edges"].as_array_mut() {
        // Graph outputs are bottom-most first, the group input order (ordinal 0 composites first).
        for (ordinal, source) in outputs.iter().enumerate() {
            edges.push(json!({
                "source": source, "target": {"layer_id": VIEW_ROOT_LAYER, "operation_key": "mix", "port_key": "inputs"},
                "dependency_kind": "data", "input_ordinal": ordinal,
            }));
        }
    }
    wire["graph"]["outputs"] = json!([{"layer_id": VIEW_ROOT_LAYER, "operation_key": "mix", "port_key": "out"}]);
    notes.push(format!("view_root_group: {} root output(s) wrapped in an isolated normal group (view only)", outputs.len()));
    let has_masks = doc.layers.iter().any(|l| l.appearance.as_ref().is_some_and(|a| a.mask.is_some()));
    if has_masks {
        notes.push("layer_masks_not_resolved: masked layers are refused by the composite lowering in this slice".into());
    }
    let bytes = serde_json::to_vec(&wire).map_err(|e| RenderFail::new("document_json", e.to_string()))?;
    match folio::inspect_bytes(&bytes, folio::Budget::default(), cancel, &ViewResolver) {
        Ok(Inspection::Editable(s)) => Ok(s),
        Ok(Inspection::ReadOnly(r)) => Err(RenderFail::new("view_document_read_only", format!("{:?}", r.diagnostic()))),
        Err(d) => Err(RenderFail::new("view_document_rejected", format!("{d:?}"))),
    }
}

/// The view copy only re-states what the opened session already resolved.
struct ViewResolver;
impl folio::Resolver for ViewResolver {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    /// The adjustment primitive is bound (adjustment layers render their records; lane
    /// SMB-HARNESS-RENDER-RECORDS); other primitive layers stay unbound.
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        kind == "adjustment" && schema_id == folio::ADJUSTMENT_SCHEMA
    }
}

/// One tile asset split into its planes (`rgb_f32le` cell, then the straight `f32le` alpha cell).
struct TilePlaneBytes<'a> {
    colour: &'a [u8],
    alpha: &'a [u8],
    colour_sha256: [u8; 32],
    alpha_sha256: [u8; 32],
}

const COLOUR_CELL: usize = CELL * CELL * 12;
const ALPHA_CELL: usize = CELL * CELL * 4;

struct TileTable<'a> {
    planes: BTreeMap<&'a str, TilePlaneBytes<'a>>,
    assets: &'a Assets,
}

impl<'a> TileTable<'a> {
    fn new(assets: &'a Assets) -> Self {
        let planes = assets
            .iter()
            .filter(|(_, bytes)| bytes.len() == COLOUR_CELL.saturating_add(ALPHA_CELL))
            .map(|(digest, bytes)| {
                let (colour, alpha) = bytes.split_at(COLOUR_CELL);
                let entry = TilePlaneBytes {
                    colour,
                    alpha,
                    colour_sha256: hsk_studio_prism::profile_hash(colour),
                    alpha_sha256: hsk_studio_prism::profile_hash(alpha),
                };
                (digest.as_str(), entry)
            })
            .collect();
        Self { planes, assets }
    }
}

impl composite::TileInfoSource for TileTable<'_> {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        if tile.format != SampleFormat::RgbF32Le.key() {
            return None;
        }
        let p = self.planes.get(tile.content_digest.digest.as_str())?;
        Some(TileInfo { format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, colour_sha256: p.colour_sha256, alpha_sha256: p.alpha_sha256 })
    }
    fn mask_sha256(&self, tiles: &[TileRef]) -> Option<[u8; 32]> {
        stitch_mask(self.assets, tiles).map(|m| m.sha256)
    }
}

struct Sources<'a> {
    tiles: &'a TileTable<'a>,
    table: &'a composite::SourceTable,
    space: ColourTag,
    /// Text masks of the lowering and the prepared planes behind them.
    text: Option<(&'a composite::TextMaskTable, &'a PreparedText)>,
    /// The inputs lowering and the prepared vector planes (vector coverage, stroke masks, painted rows).
    vector: Option<(&'a composite::InputsLowering, &'a PreparedVectors)>,
    /// Stitched coverage planes of the lowering's mask sources (layer and smart-filter masks).
    masks: &'a [(SourceKey, StitchedMask)],
}

impl PlanSources for Sources<'_> {
    fn tile(&self, key: SourceKey) -> Option<TilePlanes<'_>> {
        if let Some((lowering, vectors)) = self.vector
            && let Some(painted) = lowering.vector_paint(key, vectors, self.space)
        {
            return Some(painted);
        }
        let (_, tile) = self.table.iter().find(|(k, _)| *k == key)?;
        let p = self.tiles.planes.get(tile.content_digest.digest.as_str())?;
        let side = |l: &folio::Length| (l.value.is_finite() && l.value >= 0.0 && l.value <= CELL as f64).then_some(l.value as u32);
        Some(TilePlanes {
            width: side(&tile.width)?,
            height: side(&tile.height)?,
            colour: p.colour,
            colour_stride_bytes: (CELL * 12) as u64,
            alpha: p.alpha,
            alpha_stride_bytes: (CELL * 4) as u64,
            colour_sha256: p.colour_sha256,
            alpha_sha256: p.alpha_sha256,
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: self.space,
        })
    }
    fn mask(&self, key: SourceKey) -> Option<Mask<'_>> {
        if let Some((masks, prepared)) = self.text
            && let Some((_, layer)) = masks.iter().find(|(k, _)| *k == key)
        {
            return prepared.plane(layer)?.mask();
        }
        if let Some((_, m)) = self.masks.iter().find(|(k, _)| *k == key) {
            return Some(Mask { bounds: m.bounds, sample_format: m.format, bytes: &m.bytes, stride_bytes: m.stride, sha256: m.sha256 });
        }
        let (lowering, vectors) = self.vector?;
        lowering.vector_mask(key, vectors)
    }
}

/// Collects output chunks into one straight-alpha linear canvas.
struct Canvas {
    extent: Rect,
    /// `width * height` pixels of linear `[r, g, b, a]`.
    pixels: Vec<[f32; 4]>,
}

impl Canvas {
    fn new(extent: Rect) -> Result<Self, RenderFail> {
        let n = usize::try_from(u64::from(extent.width).saturating_mul(u64::from(extent.height))).map_err(|_| RenderFail::new("canvas_too_large", "pixel count"))?;
        Ok(Self { extent, pixels: vec![[0.0; 4]; n] })
    }

    fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.pixels.len().saturating_mul(4));
        for [r, g, b, a] in &self.pixels {
            out.extend([encode(*r), encode(*g), encode(*b), unit8(*a)]);
        }
        out
    }
}

fn read_f32(bytes: &[u8], at: usize) -> Option<f32> {
    let end = at.checked_add(4)?;
    Some(f32::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

impl TileSink for Canvas {
    fn accept(&mut self, rect: Rect, colour: &[u8], alpha: &[u8]) -> Result<(), SinkError> {
        let w = usize::try_from(rect.width).map_err(|_| SinkError::Rejected)?;
        let h = usize::try_from(rect.height).map_err(|_| SinkError::Rejected)?;
        let x0 = usize::try_from(rect.x.checked_sub(self.extent.x).ok_or(SinkError::Rejected)?).map_err(|_| SinkError::Rejected)?;
        let y0 = usize::try_from(rect.y.checked_sub(self.extent.y).ok_or(SinkError::Rejected)?).map_err(|_| SinkError::Rejected)?;
        let cw = usize::try_from(self.extent.width).map_err(|_| SinkError::Rejected)?;
        let ch = usize::try_from(self.extent.height).map_err(|_| SinkError::Rejected)?;
        if x0.checked_add(w).is_none_or(|e| e > cw) || y0.checked_add(h).is_none_or(|e| e > ch) {
            return Err(SinkError::Rejected);
        }
        for y in 0..h {
            for x in 0..w {
                let i = y.checked_mul(w).and_then(|r| r.checked_add(x)).ok_or(SinkError::Rejected)?;
                let c = i.checked_mul(12).ok_or(SinkError::Rejected)?;
                let px = [
                    read_f32(colour, c).ok_or(SinkError::Rejected)?,
                    read_f32(colour, c.checked_add(4).ok_or(SinkError::Rejected)?).ok_or(SinkError::Rejected)?,
                    read_f32(colour, c.checked_add(8).ok_or(SinkError::Rejected)?).ok_or(SinkError::Rejected)?,
                    read_f32(alpha, i.checked_mul(4).ok_or(SinkError::Rejected)?).ok_or(SinkError::Rejected)?,
                ];
                let at = y0.checked_add(y).and_then(|r| r.checked_mul(cw)).and_then(|r| r.checked_add(x0)).and_then(|r| r.checked_add(x)).ok_or(SinkError::Rejected)?;
                *self.pixels.get_mut(at).ok_or(SinkError::Rejected)? = px;
            }
        }
        Ok(())
    }
}

/// Linear light -> 8-bit sRGB transfer (IEC 61966-2-1), clamped to the unit range.
fn encode(v: f32) -> u8 {
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    let e = if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    unit8(e)
}

fn unit8(v: f32) -> u8 {
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    // In 0.0..=255.0 after the clamp.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let b = (v * 255.0).round() as u8;
    b
}
