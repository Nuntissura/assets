//! Coverage-painted static content (lane SMB-AE-MOTION-TOOLS): a list of nib coverage planes, each
//! painted with one straight RGBA colour at one opacity, lowered bottom first into ONE isolated
//! group of `Solid` layers masked by their own `coverage_u8` plane, as a [`StaticContent`] a
//! composition scene layer draws ([`crate::SceneContent::Static`]). The same existing pigment
//! routes as text and vector layers (Solid + coverage mask), so both backends render it without a
//! new operator. Used by animated text (per-glyph planes, `text::animate`) and shape layers
//! (per-paint planes, `vector_nib::shape_layer`).
//!
//! Bounds (V4-02): the plane count and the summed plane bytes are checked before anything is
//! copied; every colour channel and opacity must be finite and in `[0, 1]`.
#![deny(clippy::arithmetic_side_effects)]
use crate::CompositeError;
use crate::composition::{OwnedMask, StaticContent};
use hsk_studio_nib::raster::{CoveragePlane, PixelFrame};
use hsk_studio_nib::{OwnedPath, PathElement, PathPoint};
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::render_plan::{
    AlphaConvention, ColourTag, Group, Layer, MaskFormat, MaskInput, OutputTarget, PlanExtensions,
    PlanLimits, PlanNode, PlanNodeId, RenderPlan, SampleFormat, SourceKey,
};
use hsk_studio_pigment::Rect;
use std::collections::BTreeMap;

/// Most painted planes of one content.
pub const MAX_PAINTED_PLANES: usize = 16_384;
/// Default bound on the summed coverage bytes of one content.
pub const DEFAULT_MAX_PAINTED_BYTES: u64 = 1 << 28;

/// One painted coverage plane.
#[derive(Clone, Debug, PartialEq)]
pub struct PaintedPlane {
    pub coverage: CoveragePlane,
    /// Straight RGBA, each channel in `[0, 1]`.
    pub rgba: [f32; 4],
    /// Layer opacity in `[0, 1]`.
    pub opacity: f32,
}

fn rect_of(c: &CoveragePlane) -> Rect {
    Rect {
        x: i64::from(c.frame.x),
        y: i64::from(c.frame.y),
        width: c.frame.width,
        height: c.frame.height,
    }
}

fn union(a: Rect, b: Rect) -> Result<Rect, CompositeError> {
    let ax1 = a.x.checked_add(i64::from(a.width)).ok_or(CompositeError::Overflow)?;
    let ay1 = a.y.checked_add(i64::from(a.height)).ok_or(CompositeError::Overflow)?;
    let bx1 = b.x.checked_add(i64::from(b.width)).ok_or(CompositeError::Overflow)?;
    let by1 = b.y.checked_add(i64::from(b.height)).ok_or(CompositeError::Overflow)?;
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let w = ax1.max(bx1).checked_sub(x0).ok_or(CompositeError::Overflow)?;
    let h = ay1.max(by1).checked_sub(y0).ok_or(CompositeError::Overflow)?;
    Ok(Rect {
        x: x0,
        y: y0,
        width: u32::try_from(w).map_err(|_| CompositeError::Overflow)?,
        height: u32::try_from(h).map_err(|_| CompositeError::Overflow)?,
    })
}

/// Integer pixel frame enclosing every point of `path` (control points included; at least 1 x 1),
/// `None` for an empty path or a frame outside `i32` / `u32`.
pub fn path_frame(path: &OwnedPath) -> Option<PixelFrame> {
    let mut b: Option<(f64, f64, f64, f64)> = None;
    let mut add = |p: PathPoint| {
        b = Some(match b {
            None => (p.x, p.y, p.x, p.y),
            Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
        });
    };
    for e in &path.elements {
        match *e {
            PathElement::MoveTo(p) | PathElement::LineTo(p) => add(p),
            PathElement::QuadTo(a, p) => {
                add(a);
                add(p);
            }
            PathElement::CubicTo(a, c, p) => {
                add(a);
                add(c);
                add(p);
            }
            PathElement::Close => {}
        }
    }
    let (x0, y0, x1, y1) = b?;
    let (fx, fy) = (x0.floor(), y0.floor());
    let (w, h) = ((x1.ceil() - fx).max(1.0), (y1.ceil() - fy).max(1.0));
    if !(fx.is_finite() && fy.is_finite() && w.is_finite() && h.is_finite())
        || fx.abs() > f64::from(i32::MAX)
        || fy.abs() > f64::from(i32::MAX)
        || w > f64::from(u32::MAX)
        || h > f64::from(u32::MAX)
    {
        return None;
    }
    Some(PixelFrame { x: fx as i32, y: fy as i32, width: w as u32, height: h as u32 })
}

/// Lowers `planes` (bottom first) into static content over the union of their frames. Planes that
/// are empty (zero area, no covered pixel or opacity 0) are dropped; `None` when nothing remains.
pub fn painted_content(
    layer_id: &str,
    planes: Vec<PaintedPlane>,
    space: ColourTag,
    revision: u64,
    max_bytes: u64,
) -> Result<Option<StaticContent>, CompositeError> {
    if planes.len() > MAX_PAINTED_PLANES {
        return Err(CompositeError::Unavailable(format!("{layer_id}:painted_plane_count")));
    }
    let mut total = 0u64;
    for p in &planes {
        let bytes = u64::from(p.coverage.frame.width)
            .checked_mul(u64::from(p.coverage.frame.height))
            .ok_or(CompositeError::Overflow)?;
        total = total.checked_add(bytes).ok_or(CompositeError::Overflow)?;
        if total > max_bytes {
            return Err(CompositeError::Unavailable(format!("{layer_id}:painted_over_limit")));
        }
        if usize::try_from(bytes).ok() != Some(p.coverage.data.len()) {
            return Err(CompositeError::Unavailable(format!("{layer_id}:painted_plane_size")));
        }
        let unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
        if !p.rgba.iter().all(|v| unit(*v)) || !unit(p.opacity) {
            return Err(CompositeError::Unavailable(format!("{layer_id}:painted_colour")));
        }
    }
    let kept: Vec<PaintedPlane> = planes
        .into_iter()
        .filter(|p| p.opacity > 0.0 && p.coverage.data.iter().any(|v| *v > 0))
        .collect();
    let Some(first) = kept.first() else {
        return Ok(None);
    };
    let mut rect = rect_of(&first.coverage);
    for p in &kept {
        rect = union(rect, rect_of(&p.coverage))?;
    }
    let mut nodes = Vec::with_capacity(kept.len().saturating_add(1));
    let mut layers = Vec::with_capacity(kept.len());
    let mut owned_masks = BTreeMap::new();
    for (i, p) in kept.into_iter().enumerate() {
        let placement = rect_of(&p.coverage);
        let key = SourceKey(u32::try_from(i).map_err(|_| CompositeError::Overflow)?);
        let sha256 = hsk_studio_prism::profile_hash(&p.coverage.data);
        nodes.push(PlanNode::Solid { rect: placement, rgba: p.rgba });
        layers.push(Layer {
            input: PlanNodeId(u32::try_from(i).map_err(|_| CompositeError::Overflow)?),
            mode: StudioBlendMode::Normal,
            opacity: p.opacity,
            fill_opacity: 1.0,
            mask: Some(MaskInput {
                key,
                placement,
                format: MaskFormat::CoverageU8,
                sha256,
                inverted: false,
                density: 1.0,
            }),
        });
        owned_masks.insert(
            key,
            OwnedMask {
                bounds: placement,
                sample_format: "coverage_u8".into(),
                stride_bytes: u64::from(placement.width),
                bytes: p.coverage.data,
                sha256,
            },
        );
    }
    let root = PlanNodeId(u32::try_from(nodes.len()).map_err(|_| CompositeError::Overflow)?);
    nodes.push(PlanNode::Group(Group { isolated: true, layers }));
    let plan = RenderPlan {
        revision,
        target: OutputTarget {
            extent: rect,
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            space,
        },
        nodes,
        root,
    };
    plan.validate(&PlanLimits::default()).map_err(CompositeError::Plan)?;
    Ok(Some(StaticContent {
        plan,
        extensions: PlanExtensions::default(),
        rect,
        tiles: BTreeMap::new(),
        masks: BTreeMap::new(),
        owned_masks,
        owned_tiles: BTreeMap::new(),
    }))
}
