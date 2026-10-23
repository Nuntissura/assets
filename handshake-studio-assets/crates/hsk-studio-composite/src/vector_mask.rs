//! Folio vector masks (`StudioVectorMask` + `StudioMaskAttachment::mask_id`; lane
//! SMB-COMPOSITE-VECTOR-MASK). A mask record names a source layer whose content masks the layers it
//! is attached to (STU-VEC-053 mask modes `ALPHA | VECTOR | LUMINANCE`, STU-VEC-153 `invert_mask` /
//! `mask_clip`). It is lowered like a vector clip (lane SMB-COMPOSITE-CLIP-RECORDS): the masked
//! layer's single mask slot carries a host-built coverage plane, so both backends render it through
//! existing pigment routes (no new operator).
//!
//! Mask value `m` per pixel (`0..=1`):
//! - `VECTOR` (Photoshop vector mask): nib's exact coverage of the source's outline (a vector layer
//!   or live boolean group: its geometry; a plain group: the nib union of its visible members'
//!   geometry), whatever the source's paint.
//! - `ALPHA` (SVG `mask-type: alpha`, Figma's mask reading): the alpha of the source's content.
//! - `LUMINANCE` (SVG `mask-type: luminance`, Illustrator opacity mask): `LUMA_WEIGHTS . rgb * alpha`
//!   of the source's content over transparent black (`crate::composition::LUMA_WEIGHTS`, the same
//!   reading as a luma track matte; the SVG coefficient set and colour space are UNVERIFIED, D5).
//!
//! Content (`ALPHA`, `LUMINANCE`) is the source rendered alone: a vector layer is its prepared
//! plane (fill rows composited, cut by the fill coverage, stroke rows on top, cut by its own vector
//! clip if it has one, e.g. a Figma mask shape inside a clipping frame), a plain group
//! composites its visible members bottom first (Folio layer order) with each member's blend mode and
//! `opacity * fill_opacity` (a pass-through member group is evaluated isolated), and the source's own
//! opacity applies on top. Every composite is pigment's scalar `composite_over` (the single owner of
//! the blend maths, CX-CAP-001), the same rule the renderers execute for the emitted plan.
//!
//! Then `invert_mask` gives `1 - m`; a `region` (document unit = the unit the paths are drawn in)
//! multiplies by the exact pixel-area coverage of the region rectangle (zero outside, inverted or
//! not); a vector clip on the same layer multiplies by its clip coverage. The plane is
//! `coverage_u16le` (`round(v * 65535)`, Approximate like a track matte for content masks; a pure
//! vector mask is `257 * c` of nib's `coverage_u8`, read back as exactly `c / 255`). An inverted mask
//! without region or clip uses pigment's mask invert instead, so it stays full outside the plane.
//!
//! `mask_clip` = Illustrator's Clip (black mask background): outside the source content the mask is
//! zero, which is also the SVG and Figma reading both importers store as `mask_clip: false`; so both
//! values render the same. Illustrator's unclipped reading is Folio's
//! `StudioVectorMaskParameters.reveal_outside` (lane SMB-COMPOSITE-VECTOR-MASK-2,
//! [`MaskContentMode::LuminanceRevealed`]): the luminance of the content composited over opaque
//! white, so the mask is full outside the mask art (UNVERIFIED against Illustrator, D5).
//!
//! Parameters (Folio `StudioVectorMaskParameters`, STU-RAS-016, lane SMB-COMPOSITE-VECTOR-MASK-2):
//! density is the masked layer's `MaskInput.density` (pigment's `mask_coverage`), feather is the
//! layer's envelope `mask_feather_px` (pigment's `feather_kernel` on the stored plane), both executed
//! by the backends. A vector mask is anchored to the source layer's own geometry in document space;
//! in a composition a linked mask (the default) belongs to the masked layer's static subtree and
//! moves with the layer matrix (STU-CMP-010: masks before transform), an unlinked one stays in
//! composition space as an alpha matte of its plane ([`crate::composition::unlinked_mask_matte`]).
//!
//! Sources composite cannot draw on the host (raster or text layers, image paint rows) are rendered
//! by the caller's backend: preparation records them as pending
//! ([`PreparedVectors::pending_mask_sources`], masked layers `vector_mask_source_not_rendered`
//! until then), [`crate::mask_source_lowerings`] lowers each source alone through the document's
//! own static path, and [`PreparedVectors::resolve_mask_source`] turns the rendered straight RGBA
//! into the same content values.
//!
//! Typed refusals, never silent: unresolved record or source, source kinds composite cannot draw on
//! the host (raster, text, image paint rows), nested masks inside the source (or a vector clip on a source group), a region in
//! another unit, a mask on a layer that already has a raster mask or a clip-to-base route, a mask on
//! an adjustment layer, and the budgets below (V4-02: every plane is charged before allocation).
#![deny(clippy::arithmetic_side_effects)]
use crate::vector::{MAX_CLIP_PIXELS, MAX_VECTOR_WORK, PreparedPaint, PreparedVectors, VectorPlane};
use hsk_studio_nib::OwnedPath;
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, StudioBlendMode, composite_over};

/// The record names a mask the vector records do not hold, or a source layer the document lacks.
pub const REFUSE_MASK_UNRESOLVED: &str = "vector_mask_unresolved";
/// The source (or a member of a source group) is not a vector layer, boolean group or group.
pub const REFUSE_MASK_SOURCE_KIND: &str = "vector_mask_source_kind";
/// A source member could not be prepared (its own refusal is on the member).
pub const REFUSE_MASK_SOURCE: &str = "vector_mask_source_unavailable";
/// A source member paints an image row (no decoded pixels on the host).
pub const REFUSE_MASK_SOURCE_IMAGE: &str = "vector_mask_source_image_paint";
/// A source member carries its own mask or clip-to-base route, or a source group has a vector clip.
pub const REFUSE_MASK_SOURCE_NESTED: &str = "vector_mask_source_nested";
/// The region is not finite, negative, or not in the document's geometry unit.
pub const REFUSE_MASK_REGION: &str = "vector_mask_region";
/// A mask plane or the content work is over its budget.
pub const REFUSE_MASK_BUDGET: &str = "vector_mask_budget";
/// The masked layer also has a raster mask or is clipped to a base (one mask slot).
pub const REFUSE_MASK_WITH_MASK: &str = "vector_mask_with_mask";
/// The masked layer is an adjustment layer (no extent of its own to carry a plane).
pub const REFUSE_MASK_ADJUSTMENT: &str = "vector_mask_adjustment";
/// Lowering-note key of a content (alpha or luminance) mask: 16-bit quantisation and the luma
/// reading make it Approximate.
pub const NOTE_MASK_CONTENT: &str = "vector_mask_content";
/// Lowering-note key of a pass-through group made isolated by its vector clip or mask: pigment's
/// rule and key (`RenderPlan::isolate_masked_pass_through`).
pub const NOTE_MASK_ISOLATES: &str = hsk_studio_pigment::render_plan::PASS_THROUGH_MASK_ISOLATED;
/// The masked layer waits for its source to be rendered by the caller's backend
/// ([`PreparedVectors::pending_mask_sources`]).
pub const REFUSE_MASK_SOURCE_NOT_RENDERED: &str = "vector_mask_source_not_rendered";
/// The pending source is not a graph operation of the document (nothing to root its lowering at).
pub const REFUSE_MASK_SOURCE_NOT_IN_GRAPH: &str = "vector_mask_source_not_in_graph";
/// `reveal_outside` on a mode other than `LUMINANCE` (Illustrator's reading is a luminance mask).
pub const REFUSE_MASK_REVEAL_MODE: &str = "vector_mask_reveal_outside_mode";
/// An unlinked mask in a composition with a density below 100 % (the matte route has no density).
pub const REFUSE_MASK_UNLINKED_DENSITY: &str = "vector_mask_unlinked_density";
/// An unlinked mask on a composition layer that also has a track matte (one matte slot).
pub const REFUSE_MASK_UNLINKED_MATTE: &str = "vector_mask_unlinked_with_matte";
/// Aggregate pixels of every content plane one preparation computes.
pub const MAX_MASK_CONTENT_PIXELS: u64 = 64 << 20;
/// Members (layers) of one mask source.
pub const MAX_MASK_MEMBERS: usize = 4096;

/// Which channel of the source's content becomes the mask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskContentMode {
    Alpha,
    Luminance,
    /// Illustrator's unclipped opacity mask (`reveal_outside`): the luminance of the content
    /// composited over opaque white; full (1) outside the content.
    LuminanceRevealed,
}
impl MaskContentMode {
    /// The mask value of one straight RGBA pixel of the rendered source.
    pub fn value(self, c: Rgba) -> f32 {
        let w = crate::composition::LUMA_WEIGHTS;
        let luma = |rgb: [f32; 3]| w[0] * rgb[0] + w[1] * rgb[1] + w[2] * rgb[2];
        match self {
            Self::Alpha => c.a,
            Self::Luminance => luma(c.rgb) * c.a,
            // Over opaque white: colour = c * a + (1 - a).
            Self::LuminanceRevealed => luma([
                c.rgb[0] * c.a + (1.0 - c.a),
                c.rgb[1] * c.a + (1.0 - c.a),
                c.rgb[2] * c.a + (1.0 - c.a),
            ]),
        }
    }
    /// The value outside the source content (transparent).
    pub fn outside(self) -> f32 {
        if self == Self::LuminanceRevealed { 1.0 } else { 0.0 }
    }
}

/// A mask whose source the caller's backend renders (raster, text, image paint;
/// [`PreparedVectors::pending_mask_sources`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PendingMaskSource {
    pub mask_id: String,
    pub source_layer_id: String,
    pub mode: MaskContentMode,
    /// The masked layers.
    pub layers: Vec<String>,
    /// The mask's invert / region / density / feather / link, applied once resolved.
    pub template: VectorMask,
}

/// A mask source evaluated for its content: a prepared vector layer (or boolean group), or a group
/// of members bottom first. `mode` and `opacity` are how the node composites into its parent
/// (`opacity` = layer opacity times fill opacity, `0..=1`).
#[derive(Clone, Debug, PartialEq)]
pub enum MaskNode {
    Leaf {
        layer_id: String,
        mode: StudioBlendMode,
        opacity: f32,
    },
    Group {
        mode: StudioBlendMode,
        opacity: f32,
        children: Vec<MaskNode>,
    },
}
impl MaskNode {
    fn blend(&self) -> (StudioBlendMode, f32) {
        match self {
            Self::Leaf { mode, opacity, .. } | Self::Group { mode, opacity, .. } => (*mode, *opacity),
        }
    }
}

/// The mask value before invert / region / clip.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorMaskSource {
    /// `VECTOR`: the source outline (`None` = empty: zero everywhere).
    Outline(Option<OwnedPath>),
    /// `ALPHA` / `LUMINANCE`: one value per pixel of `rect`, row-major (`outside` outside; `None`
    /// rect = no content).
    Content { rect: Option<Rect>, values: Vec<f32>, outside: f32 },
}

/// One resolved vector mask.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorMask {
    pub source: VectorMaskSource,
    pub invert: bool,
    /// `[x0, y0, x1, y1]` in plan pixels; outside it the mask is zero.
    pub region: Option<[f64; 4]>,
    /// STU-RAS-016 density, `0..=1` (pigment `MaskInput.density`).
    pub density: f32,
    /// STU-RAS-016 feather, Gaussian radius in pixels (pigment envelope `mask_feather_px`).
    pub feather_px: f32,
    /// STU-RAS-016 link: moves with the masked layer's transform (compositions).
    pub linked: bool,
}
impl VectorMask {
    /// A mask of `source` with every parameter at its default (not inverted, no region, density
    /// 1, no feather, linked).
    pub fn of(source: VectorMaskSource) -> Self {
        Self { source, invert: false, region: None, density: 1.0, feather_px: 0.0, linked: true }
    }
}

fn charge(budget: &mut u64, pixels: u64) -> Result<(), &'static str> {
    *budget = budget.checked_sub(pixels).ok_or(REFUSE_MASK_BUDGET)?;
    Ok(())
}

fn leaves<'a>(node: &'a MaskNode, out: &mut Vec<&'a str>) {
    match node {
        MaskNode::Leaf { layer_id, .. } => out.push(layer_id),
        MaskNode::Group { children, .. } => children.iter().for_each(|c| leaves(c, out)),
    }
}

fn union(a: Option<Rect>, b: Rect) -> Result<Option<Rect>, &'static str> {
    crate::evaluate_static::union(a, b).map_err(|_| REFUSE_MASK_BUDGET)
}

fn inside(r: Rect, x: i64, y: i64) -> Option<usize> {
    let dx = x.checked_sub(r.x)?;
    let dy = y.checked_sub(r.y)?;
    if dx < 0 || dy < 0 || dx >= i64::from(r.width) || dy >= i64::from(r.height) {
        return None;
    }
    let i = dy.checked_mul(i64::from(r.width))?.checked_add(dx)?;
    usize::try_from(i).ok()
}

/// Straight RGBA of a prepared vector layer at one pixel (the emitted node layout).
fn leaf_rgba(plane: &VectorPlane, x: i64, y: i64, site: PixelSite) -> Result<Rgba, &'static str> {
    let Some(rect) = plane.placement else {
        return Ok(Rgba::TRANSPARENT);
    };
    let Some(i) = inside(rect, x, y) else {
        return Ok(Rgba::TRANSPARENT);
    };
    let colour = |row: usize| -> Result<Rgba, &'static str> {
        match &plane.fills.get(row).ok_or(REFUSE_MASK_SOURCE)?.paint {
            PreparedPaint::Solid(c) => Ok(Rgba::new([c[0], c[1], c[2]], c[3])),
            PreparedPaint::Painted(t) => {
                let f = |b: &[u8], at: usize| -> Result<f32, &'static str> {
                    let end = at.checked_add(4).ok_or(REFUSE_MASK_BUDGET)?;
                    let s = b.get(at..end).ok_or(REFUSE_MASK_SOURCE)?;
                    Ok(f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
                };
                let c = i.checked_mul(12).ok_or(REFUSE_MASK_BUDGET)?;
                let a = i.checked_mul(4).ok_or(REFUSE_MASK_BUDGET)?;
                Ok(Rgba::new(
                    [
                        f(&t.colour, c)?,
                        f(&t.colour, c.checked_add(4).ok_or(REFUSE_MASK_BUDGET)?)?,
                        f(&t.colour, c.checked_add(8).ok_or(REFUSE_MASK_BUDGET)?)?,
                    ],
                    f(&t.alpha, a)?,
                ))
            }
            PreparedPaint::Image { .. } => Err(REFUSE_MASK_SOURCE_IMAGE),
        }
    };
    let over = |mode, b, s, o| composite_over(mode, b, s, o, site).map_err(|_| REFUSE_MASK_SOURCE);
    let mut rows = Rgba::TRANSPARENT;
    for (row, fill) in plane.fills.iter().enumerate() {
        if plane.is_stroke(row) {
            continue;
        }
        rows = over(fill.mode, rows, colour(row)?, fill.opacity)?;
    }
    let cov = (f64::from(*plane.coverage.get(i).ok_or(REFUSE_MASK_SOURCE)?) / 255.0) as f32;
    let mut acc = over(StudioBlendMode::Normal, Rgba::TRANSPARENT, rows, cov)?;
    for s in &plane.strokes {
        let fill = plane.fills.get(s.row).ok_or(REFUSE_MASK_SOURCE)?;
        let k = (f64::from(*s.coverage.get(i).ok_or(REFUSE_MASK_SOURCE)?) / 255.0) as f32;
        acc = over(fill.mode, acc, colour(s.row)?, fill.opacity * k)?;
    }
    Ok(acc)
}

/// The vector clips of the source's leaves over the content rect: `(layer, coverage_u8)`.
type LeafClips<'a> = Vec<(&'a str, Vec<u8>)>;

fn node_rgba(
    vectors: &PreparedVectors,
    clips: &(Rect, LeafClips<'_>),
    node: &MaskNode,
    x: i64,
    y: i64,
    site: PixelSite,
) -> Result<Rgba, &'static str> {
    match node {
        MaskNode::Leaf { layer_id, .. } => {
            let c = leaf_rgba(vectors.plane(layer_id).ok_or(REFUSE_MASK_SOURCE)?, x, y, site)?;
            // A leaf with a vector clip (e.g. a Figma mask shape in a clipping frame) is cut by it.
            match clips.1.iter().find(|(l, _)| l == layer_id) {
                Some((_, cov)) => {
                    let i = inside(clips.0, x, y).ok_or(REFUSE_MASK_SOURCE)?;
                    let k = (f64::from(*cov.get(i).ok_or(REFUSE_MASK_SOURCE)?) / 255.0) as f32;
                    composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, c, k, site)
                        .map_err(|_| REFUSE_MASK_SOURCE)
                }
                None => Ok(c),
            }
        }
        MaskNode::Group { children, .. } => {
            let mut acc = Rgba::TRANSPARENT;
            for c in children {
                let (mode, opacity) = c.blend();
                let v = node_rgba(vectors, clips, c, x, y, site)?;
                acc = composite_over(mode, acc, v, opacity, site).map_err(|_| REFUSE_MASK_SOURCE)?;
            }
            Ok(acc)
        }
    }
}

/// The content of `node` as mask values (module docs): alpha or luminance of the source rendered
/// alone, its own opacity applied. Charged against `budget` (pixels) before allocation; the work
/// (pixels x rows) is bounded by `MAX_VECTOR_WORK`.
pub fn mask_content(
    vectors: &PreparedVectors,
    node: &MaskNode,
    mode: MaskContentMode,
    budget: &mut u64,
) -> Result<VectorMaskSource, &'static str> {
    let mut ids = Vec::new();
    leaves(node, &mut ids);
    if ids.len() > MAX_MASK_MEMBERS {
        return Err(REFUSE_MASK_BUDGET);
    }
    let mut rect: Option<Rect> = None;
    let mut rows: u64 = 0;
    for id in &ids {
        if vectors.refusal(id).is_some() {
            return Err(REFUSE_MASK_SOURCE);
        }
        let plane = vectors.plane(id).ok_or(REFUSE_MASK_SOURCE)?;
        if plane.fills.iter().any(|f| matches!(f.paint, PreparedPaint::Image { .. })) {
            return Err(REFUSE_MASK_SOURCE_IMAGE);
        }
        if let Some(p) = plane.placement.filter(|_| !plane.fills.is_empty()) {
            rect = union(rect, p)?;
        }
        let n = u64::try_from(plane.fills.len()).map_err(|_| REFUSE_MASK_BUDGET)?;
        rows = rows.checked_add(n.checked_add(1).ok_or(REFUSE_MASK_BUDGET)?).ok_or(REFUSE_MASK_BUDGET)?;
    }
    let Some(r) = rect else {
        return Ok(VectorMaskSource::Content { rect: None, values: Vec::new(), outside: mode.outside() });
    };
    let pixels = u64::from(r.width).checked_mul(u64::from(r.height)).ok_or(REFUSE_MASK_BUDGET)?;
    if pixels > MAX_CLIP_PIXELS || pixels.checked_mul(rows).is_none_or(|w| w > MAX_VECTOR_WORK) {
        return Err(REFUSE_MASK_BUDGET);
    }
    charge(budget, pixels)?;
    let mut leaf_clips: LeafClips<'_> = Vec::new();
    for id in &ids {
        if let Some(clip) = vectors.clip(id) {
            let outline = clip.map_err(|_| REFUSE_MASK_SOURCE)?;
            charge(budget, pixels)?;
            leaf_clips.push((id, crate::vector_nib::coverage(outline, r, MAX_CLIP_PIXELS)?));
        }
    }
    let clips = (r, leaf_clips);
    let n = usize::try_from(pixels).map_err(|_| REFUSE_MASK_BUDGET)?;
    let (_, own) = node.blend();
    let mut values = Vec::with_capacity(n);
    let x1 = r.x.checked_add(i64::from(r.width)).ok_or(REFUSE_MASK_BUDGET)?;
    let y1 = r.y.checked_add(i64::from(r.height)).ok_or(REFUSE_MASK_BUDGET)?;
    for y in r.y..y1 {
        for x in r.x..x1 {
            let site = PixelSite { x, y, seed: 0 };
            let c = node_rgba(vectors, &clips, node, x, y, site)?;
            let c = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT, c, own, site)
                .map_err(|_| REFUSE_MASK_SOURCE)?;
            values.push(mode.value(c));
        }
    }
    Ok(VectorMaskSource::Content { rect: Some(r), values, outside: mode.outside() })
}

/// Conservative pixel bounds of the mask's source (outline or content); `None` = empty. A
/// non-finite outline is `vector_geometry`; bounds beyond +-9e15 are `vector_mask_budget`.
pub fn source_bounds(mask: &VectorMask) -> Result<Option<Rect>, &'static str> {
    match &mask.source {
        VectorMaskSource::Content { rect, .. } => Ok(*rect),
        VectorMaskSource::Outline(p) => {
            // `f64::min`/`max` skip NaN, so every coordinate is checked, not only the box.
            let finite = |q: &hsk_studio_nib::PathPoint| q.x.is_finite() && q.y.is_finite();
            let all_finite = p.iter().flat_map(|p| &p.elements).all(|e| match e {
                hsk_studio_nib::PathElement::MoveTo(q) | hsk_studio_nib::PathElement::LineTo(q) => finite(q),
                hsk_studio_nib::PathElement::QuadTo(c, q) => finite(c) && finite(q),
                hsk_studio_nib::PathElement::CubicTo(c1, c2, q) => finite(c1) && finite(c2) && finite(q),
                hsk_studio_nib::PathElement::Close => true,
            });
            if !all_finite {
                return Err(crate::vector::REFUSE_VECTOR_GEOMETRY);
            }
            let Some([x0, y0, x1, y1]) = p.as_ref().and_then(crate::vector_nib::path_bounds) else {
                return Ok(None);
            };
            let (fx, fy, gx, gy) = (x0.floor(), y0.floor(), x1.ceil(), y1.ceil());
            if !(fx.is_finite() && fy.is_finite() && gx.is_finite() && gy.is_finite()) {
                return Err(crate::vector::REFUSE_VECTOR_GEOMETRY);
            }
            let ok = |v: f64| v.abs() < 9.0e15;
            if !(ok(fx) && ok(fy) && ok(gx) && ok(gy)) {
                return Err(REFUSE_MASK_BUDGET);
            }
            let (w, h) = (gx - fx, gy - fy);
            if w > f64::from(u32::MAX) || h > f64::from(u32::MAX) {
                return Err(REFUSE_MASK_BUDGET);
            }
            // Finite, integral and in range by the checks above.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let r = Rect { x: fx as i64, y: fy as i64, width: w as u32, height: h as u32 };
            Ok(Some(r).filter(|r| r.width > 0 && r.height > 0))
        }
    }
}

/// Where an unlinked mask (lane SMB-COMPOSITE-DOCGRAPH-2) can differ from its value outside the
/// plane: [`source_bounds`] united with the region's pixel rect, grown by pigment's
/// `feather_support` of the mask feather on every side, so a plane over it holds the whole
/// feathered mask (backends clamp the feather taps at the plane border; past this rect the stored
/// value is the constant [`outside_value`], so the clamp reads the true value). `None` = empty.
pub fn unlinked_rect(mask: &VectorMask) -> Result<Option<Rect>, &'static str> {
    let mut rect = source_bounds(mask)?;
    if let Some([x0, y0, x1, y1]) = mask.region {
        let (fx, fy, gx, gy) = (x0.floor(), y0.floor(), x1.ceil(), y1.ceil());
        let ok = |v: f64| v.is_finite() && v.abs() < 9.0e15;
        if !(ok(fx) && ok(fy) && ok(gx) && ok(gy)) || gx - fx > f64::from(u32::MAX) || gy - fy > f64::from(u32::MAX) {
            return Err(REFUSE_MASK_REGION);
        }
        // Finite, integral and in range by the checks above.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let r = Rect { x: fx as i64, y: fy as i64, width: (gx - fx) as u32, height: (gy - fy) as u32 };
        if r.width > 0 && r.height > 0 {
            rect = union(rect, r)?;
        }
    }
    let Some(r) = rect else {
        return Ok(None);
    };
    if mask.feather_px <= 0.0 {
        return Ok(Some(r));
    }
    let support = hsk_studio_pigment::coverage_math::feather_support(mask.feather_px)
        .map_err(|_| REFUSE_MASK_BUDGET)?;
    let twice = support.checked_mul(2).ok_or(REFUSE_MASK_BUDGET)?;
    Ok(Some(Rect {
        x: r.x.checked_sub(i64::from(support)).ok_or(REFUSE_MASK_BUDGET)?,
        y: r.y.checked_sub(i64::from(support)).ok_or(REFUSE_MASK_BUDGET)?,
        width: r.width.checked_add(twice).ok_or(REFUSE_MASK_BUDGET)?,
        height: r.height.checked_add(twice).ok_or(REFUSE_MASK_BUDGET)?,
    }))
}

/// The mask's value outside [`source_bounds`] and the region after invert and region (what a
/// plane holds there before pigment's own invert flag): `0`, or `1` for an inverted outline or
/// content without region and for a `reveal_outside` luminance mask without invert.
pub fn outside_value(mask: &VectorMask) -> f64 {
    let outside = match &mask.source {
        VectorMaskSource::Content { outside, .. } => f64::from(*outside),
        VectorMaskSource::Outline(_) => 0.0,
    };
    if mask.region.is_some() {
        return 0.0;
    }
    if mask.invert { 1.0 - outside } else { outside }
}

/// The masked layer's plane over `rect` (module docs): `(bytes, inverted_by_pigment)`, format
/// `coverage_u16le`. `clip`: the layer's vector clip outline (`Some(None)` = an empty clip).
pub(crate) fn plane(
    mask: &VectorMask,
    clip: Option<Option<&OwnedPath>>,
    rect: Rect,
) -> Result<(Vec<u8>, bool), &'static str> {
    let pixels = u64::from(rect.width)
        .checked_mul(u64::from(rect.height))
        .filter(|p| *p <= MAX_CLIP_PIXELS)
        .ok_or(REFUSE_MASK_BUDGET)?;
    let n = usize::try_from(pixels).map_err(|_| REFUSE_MASK_BUDGET)?;
    let outline = match &mask.source {
        VectorMaskSource::Outline(p) => Some(crate::vector_nib::coverage(p.as_ref(), rect, MAX_CLIP_PIXELS)?),
        VectorMaskSource::Content { .. } => None,
    };
    let clip = match clip {
        Some(p) => Some(crate::vector_nib::coverage(p, rect, MAX_CLIP_PIXELS)?),
        None => None,
    };
    // Pure invert: pigment's mask invert (full coverage outside the plane, like the source's
    // zero outside its bounds inverted).
    let outside = match &mask.source {
        VectorMaskSource::Content { outside, .. } => f64::from(*outside),
        VectorMaskSource::Outline(_) => 0.0,
    };
    let flag = mask.invert && mask.region.is_none() && clip.is_none() && outside == 0.0;
    let mut out = Vec::with_capacity(n.checked_mul(2).ok_or(REFUSE_MASK_BUDGET)?);
    let x1 = rect.x.checked_add(i64::from(rect.width)).ok_or(REFUSE_MASK_BUDGET)?;
    let y1 = rect.y.checked_add(i64::from(rect.height)).ok_or(REFUSE_MASK_BUDGET)?;
    let mut i = 0usize;
    for y in rect.y..y1 {
        for x in rect.x..x1 {
            let mut v = match (&outline, &mask.source) {
                (Some(c), _) => f64::from(*c.get(i).ok_or(REFUSE_MASK_BUDGET)?) / 255.0,
                (None, VectorMaskSource::Content { rect: Some(r), values, .. }) => match inside(*r, x, y) {
                    Some(j) => f64::from(*values.get(j).ok_or(REFUSE_MASK_BUDGET)?),
                    None => outside,
                },
                (None, _) => outside,
            };
            if mask.invert && !flag {
                v = 1.0 - v;
            }
            if let Some([rx0, ry0, rx1, ry1]) = mask.region {
                // Exact area of the pixel square inside the region rectangle.
                #[allow(clippy::cast_precision_loss)]
                let (px, py) = (x as f64, y as f64);
                let ox = (rx1.min(px + 1.0) - rx0.max(px)).max(0.0);
                let oy = (ry1.min(py + 1.0) - ry0.max(py)).max(0.0);
                v *= ox * oy;
            }
            if let Some(c) = &clip {
                v *= f64::from(*c.get(i).ok_or(REFUSE_MASK_BUDGET)?) / 255.0;
            }
            // Clamped into 0..=65535 first.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let q = (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
            out.extend_from_slice(&q.to_le_bytes());
            i = i.checked_add(1).ok_or(REFUSE_MASK_BUDGET)?;
        }
    }
    Ok((out, flag))
}
