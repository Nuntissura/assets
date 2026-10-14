//! Text layer lowering (lane SMB-COMPOSITE-TEXT; type brief slice 2, STU-TYP-127: Type owns
//! outline extraction and rasterisation, composite only places the result).
//!
//! A text layer (Folio `kind = "text"`, `Payload::Text`) is drawn from caller-shaped runs: the
//! caller shapes the story with `hsk_studio_type::TextEngine` (layout owns line breaking; a
//! paragraph arrives as one run per placed line) and supplies each run with its resolved font
//! bytes and its pen origin in document pixels, plus the fill colour resolved from the story's
//! style (Folio stores no fill or font record for a story yet: SPEC GAP, the caller resolves it).
//! [`prepare_text`] extracts outlines and rasterises every run with Type at the caller's document
//! resolution (`px_per_pt`), and unions the runs of one layer with saturation into one
//! `coverage_u8` plane. The lowering then emits, for that layer, an isolated group holding one
//! `Solid { rect: plane, rgba: fill }` layer masked by the plane (`MaskFormat::CoverageU8`); the
//! referencing layer in the parent group carries the authored appearance (opacity, blend mode,
//! layer mask) exactly as for a raster layer, so text needs no new pigment operator and both
//! backends render it through the existing Solid and coverage-mask routes.
//!
//! Refusals are typed and never dropped: everything Type refuses (colour, bitmap, SVG and CFF2
//! faces; runs with explicit variation axes; glyph ids the font lacks; outline and raster limits)
//! is `Unavailable("<layer>:<type code>")`; a fill outside `[0, 1]` is `<layer>:text_fill`; a layer
//! plane above `max_layer_pixels` is `<layer>:raster_over_limit`, refused before allocation.
//! Documents without text are lowered by the unchanged static path (byte-identical plans).
use crate::evaluate_static::{EvalReport, evaluate_with};
use crate::lower_plan::{LoweredPlan, NodeAddr};
use crate::lower_render_plan::{RenderLowering, TileInfoSource, lower_inner, text_root_plan};
use crate::CompositeError;
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::Snapshot;
use hsk_studio_pigment::render_plan::{OutputTarget, SourceKey};
use hsk_studio_pigment::{Mask, Rect};
use hsk_studio_type::{
    Error as TypeError, FontBytes, GlyphCoverage, GlyphError, OutlineLimits, RasterLimits, Run,
    glyph_outlines, rasterize_run,
};

/// One shaped run of a text layer, ready to draw.
pub struct TextRun<'a> {
    pub run: &'a Run<'a>,
    /// The run's resolved font bytes (`FontResource` = `run.resolved()`; Type re-checks the hash).
    pub font: FontBytes<'a>,
    /// Pen origin on the baseline, document pixels (fractional = subpixel positioning).
    pub origin_px: [f64; 2],
}

/// The drawable content of one Folio text layer.
pub struct TextLayerInput<'a> {
    pub layer_id: &'a str,
    pub runs: &'a [TextRun<'a>],
    /// Straight RGBA in the plan's blend space, each channel in `[0, 1]`.
    pub fill_rgba: [f32; 4],
}

/// Document resolution and the caps applied to text work.
#[derive(Clone, Copy, Debug)]
pub struct TextResolution {
    /// Device pixels per point (`ppi / 72`).
    pub px_per_pt: f64,
    pub outline_limits: OutlineLimits,
    pub raster_limits: RasterLimits,
    /// Pixels of one layer's union plane, checked before it is allocated.
    pub max_layer_pixels: u64,
}
impl TextResolution {
    /// Default limits at `ppi` pixels per inch.
    pub fn at_ppi(ppi: f64) -> Self {
        Self {
            px_per_pt: ppi / 72.0,
            outline_limits: OutlineLimits::default(),
            raster_limits: RasterLimits::default(),
            max_layer_pixels: 64 << 20,
        }
    }
}

/// One text layer's coverage plane (`coverage_u8`, row-major, stride = width) and fill.
#[derive(Clone, Debug, PartialEq)]
pub struct TextPlane {
    pub layer_id: String,
    /// `None` when the layer has no ink (only spaces, or no runs).
    pub placement: Option<Rect>,
    pub coverage: Vec<u8>,
    pub sha256: [u8; 32],
    pub fill_rgba: [f32; 4],
}
impl TextPlane {
    /// The plane as pigment's mask (what a `PlanSources::mask` implementation returns).
    pub fn mask(&self) -> Option<Mask<'_>> {
        let bounds = self.placement?;
        Some(Mask {
            bounds,
            sample_format: "coverage_u8",
            bytes: &self.coverage,
            stride_bytes: u64::from(bounds.width),
            sha256: self.sha256,
        })
    }
}

/// Rasterised text of a document, keyed by layer id (sorted, unique).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PreparedText {
    planes: Vec<TextPlane>,
}
impl PreparedText {
    pub fn plane(&self, layer_id: &str) -> Option<&TextPlane> {
        self.planes
            .binary_search_by(|p| p.layer_id.as_str().cmp(layer_id))
            .ok()
            .map(|i| &self.planes[i])
    }
    pub fn planes(&self) -> &[TextPlane] {
        &self.planes
    }
}

/// `SourceKey` -> text layer id whose plane is that key's mask.
pub type TextMaskTable = Vec<(SourceKey, String)>;

/// [`crate::lower_to_render_plan`] for a document with text layers.
#[derive(Clone, Debug, PartialEq)]
pub struct TextRenderLowering {
    pub lowering: RenderLowering,
    pub text_masks: TextMaskTable,
}
impl TextRenderLowering {
    /// The coverage mask behind `key` when it is a text mask.
    pub fn text_mask<'p>(&self, key: SourceKey, text: &'p PreparedText) -> Option<Mask<'p>> {
        let (_, layer) = self.text_masks.iter().find(|(k, _)| *k == key)?;
        text.plane(layer)?.mask()
    }
}

fn refusal(layer: &str, error: GlyphError) -> CompositeError {
    match error {
        GlyphError::Type(TypeError::Canceled) => CompositeError::Canceled,
        GlyphError::Type(e) => CompositeError::Unavailable(format!("{layer}:type_{e:?}")),
        other => CompositeError::Unavailable(format!("{layer}:{}", other.code())),
    }
}

/// Outlines and rasterises every run of every text layer with Type, then unions each layer's runs
/// (saturating add) into one plane. Refusals are typed (module docs); nothing is clamped.
pub fn prepare_text(
    layers: &[TextLayerInput<'_>],
    resolution: &TextResolution,
    cancel: &CancellationToken,
) -> Result<PreparedText, CompositeError> {
    if !(resolution.px_per_pt.is_finite() && resolution.px_per_pt > 0.0) {
        return Err(CompositeError::UnsupportedDescriptor("text_resolution".into()));
    }
    let mut planes = Vec::with_capacity(layers.len());
    for layer in layers {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        let id = layer.layer_id;
        if !layer.fill_rgba.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)) {
            return Err(CompositeError::Unavailable(format!("{id}:text_fill")));
        }
        let mut runs: Vec<GlyphCoverage> = Vec::with_capacity(layer.runs.len());
        for r in layer.runs {
            if r.run.glyphs().is_empty() {
                continue;
            }
            let ids: Vec<u32> = r.run.glyphs().iter().map(|g| g.glyph_id).collect();
            let outlines = glyph_outlines(&r.font, &ids, &resolution.outline_limits, cancel)
                .map_err(|e| refusal(id, e))?;
            let coverage = rasterize_run(
                r.run,
                &outlines,
                r.origin_px,
                resolution.px_per_pt,
                &resolution.raster_limits,
                cancel,
            )
            .map_err(|e| refusal(id, e))?;
            if coverage.width() > 0 && coverage.height() > 0 {
                runs.push(coverage);
            }
        }
        planes.push(union_plane(id, &runs, layer.fill_rgba, resolution.max_layer_pixels)?);
    }
    planes.sort_by(|a, b| a.layer_id.cmp(&b.layer_id));
    if planes.windows(2).any(|w| w[0].layer_id == w[1].layer_id) {
        return Err(CompositeError::UnsupportedDescriptor("duplicate_text_layer".into()));
    }
    Ok(PreparedText { planes })
}

fn union_plane(
    id: &str,
    runs: &[GlyphCoverage],
    fill_rgba: [f32; 4],
    max_pixels: u64,
) -> Result<TextPlane, CompositeError> {
    let over = || CompositeError::Unavailable(format!("{id}:raster_over_limit"));
    let mut bounds: Option<(i64, i64, i64, i64)> = None;
    for c in runs {
        let x1 = c.x().checked_add(i64::from(c.width())).ok_or_else(over)?;
        let y1 = c.y().checked_add(i64::from(c.height())).ok_or_else(over)?;
        bounds = Some(match bounds {
            None => (c.x(), c.y(), x1, y1),
            Some((a, b, d, e)) => (a.min(c.x()), b.min(c.y()), d.max(x1), e.max(y1)),
        });
    }
    let Some((x0, y0, x1, y1)) = bounds else {
        return Ok(TextPlane {
            layer_id: id.to_owned(),
            placement: None,
            coverage: Vec::new(),
            sha256: hsk_studio_prism::profile_hash(&[]),
            fill_rgba,
        });
    };
    let width = u32::try_from(x1.checked_sub(x0).ok_or_else(over)?).map_err(|_| over())?;
    let height = u32::try_from(y1.checked_sub(y0).ok_or_else(over)?).map_err(|_| over())?;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .filter(|p| *p <= max_pixels)
        .ok_or_else(over)?;
    let mut coverage = vec![0u8; usize::try_from(pixels).map_err(|_| over())?];
    let stride = width as usize;
    for c in runs {
        let dx = (c.x() - x0) as usize;
        let dy = (c.y() - y0) as usize;
        let w = c.width() as usize;
        for (row, src) in c.data().chunks(w).enumerate() {
            let start = (dy + row) * stride + dx;
            for (dst, s) in coverage[start..start + w].iter_mut().zip(src) {
                *dst = dst.saturating_add(*s);
            }
        }
    }
    Ok(TextPlane {
        layer_id: id.to_owned(),
        placement: Some(Rect {
            x: x0,
            y: y0,
            width,
            height,
        }),
        sha256: hsk_studio_prism::profile_hash(&coverage),
        coverage,
        fill_rgba,
    })
}

/// [`crate::evaluate`] where text layers with a prepared plane are Ready with the plane as their
/// extent (an inkless layer is Ready and empty); a text layer missing from `text` stays
/// Unavailable (`text_layer_not_prepared`).
pub fn evaluate_with_text(
    plan: &LoweredPlan,
    snap: &Snapshot,
    text: &PreparedText,
    cancel: &CancellationToken,
) -> Result<EvalReport, CompositeError> {
    evaluate_with(plan, snap, None, Some(text), cancel)
}

/// [`crate::lower_to_render_plan`] where every reachable text layer lowers to an isolated group
/// `[Layer { Solid(plane rect, fill), Normal, mask: coverage_u8 plane }]` and the parent layer
/// carries its authored appearance. Serve the text masks with [`TextRenderLowering::text_mask`].
pub fn lower_to_render_plan_with_text(
    plan: &LoweredPlan,
    report: &EvalReport,
    snap: &Snapshot,
    root: &NodeAddr,
    target: OutputTarget,
    info: &dyn TileInfoSource,
    text: &PreparedText,
) -> Result<TextRenderLowering, CompositeError> {
    let (lowering, _, _, text_masks) =
        lower_inner(plan, report, snap, None, root, target, info, Some(text))?;
    Ok(TextRenderLowering {
        lowering,
        text_masks,
    })
}

/// One prepared text layer as a standalone render plan (no Folio document): exactly the plan
/// [`lower_to_render_plan_with_text`] returns for that layer as a neutral root. For hosts that
/// preview or re-render one text layer (and for the renderer parity tests, which cannot depend on
/// Folio). `Unavailable <layer>:text_layer_not_prepared | text_layer_empty`.
pub fn text_layer_render_plan(
    text: &PreparedText,
    layer_id: &str,
    revision: u64,
    target: OutputTarget,
) -> Result<TextRenderLowering, CompositeError> {
    let plane = text.plane(layer_id).ok_or_else(|| {
        CompositeError::Unavailable(format!("{layer_id}:text_layer_not_prepared"))
    })?;
    let (lowering, text_masks) = text_root_plan(plane, revision, target)?;
    Ok(TextRenderLowering {
        lowering,
        text_masks,
    })
}
