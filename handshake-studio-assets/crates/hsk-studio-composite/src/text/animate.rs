//! Animated text lowering (lane SMB-AE-MOTION-TOOLS slice 2, feature `motion`; STU-MOT-090..092,
//! STU-TYP-127: Type owns shaping and outlines, Motion owns the animator evaluation
//! (`hsk_studio_motion::evaluate_text_animators`), composite only places the glyphs).
//!
//! Each glyph becomes its own drawable so a per-character transform, opacity and fill can apply:
//! the glyph outline (Type, font units, y up) is placed at its pen position in document pixels
//! exactly as `hsk_studio_type::rasterize_run` places it (`x = pen_x + u_x * scale`,
//! `y = pen_y - u_y * scale`, `scale = size_pt * px_per_pt / units_per_em`), then moved by the
//! character's [`CharTransform`]:
//!
//! * tracking: every character after `i` shifts right by `tracking_i / 1000 * em_px_i`;
//! * the anchor is the character's baseline centre (`pen_x + advance / 2`, baseline y; After
//!   Effects' "character" anchor grouping as Studio reads it, UNVERIFIED D5);
//! * `p' = anchor + R(rotation) * S(scale) * (p - anchor) + position` (rotation clockwise on the
//!   y-down canvas, sin/cos from nib's fixed polynomial `sin_cos_deg`, identical on every host).
//!
//! The moved outline is rasterised by nib (`nib::raster::rasterize`, exact signed-area coverage)
//! over its own integer frame and painted with the character's fill (or the layer fill) at the
//! character's opacity ([`super::painted`]). Spaces and empty outlines draw nothing.
//!
//! Bounds: at most [`MAX_ANIMATED_GLYPHS`] glyphs, one transform per glyph, every transform value
//! finite, every glyph frame within nib's pixel budget and the summed planes within the painted
//! byte budget, all checked before the plane is allocated.
#![deny(clippy::arithmetic_side_effects)]
use super::painted::{DEFAULT_MAX_PAINTED_BYTES, PaintedPlane, painted_content, path_frame};
use super::{TextResolution, TextRun};
use crate::CompositeError;
use crate::composition::StaticContent;
use hsk_studio_accord::CancellationToken;
use hsk_studio_motion::{CharTransform, TextChar};
use hsk_studio_nib::raster::{RasterOptions, rasterize};
use hsk_studio_nib::shape::sin_cos_deg;
use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_pigment::render_plan::ColourTag;
use hsk_studio_type::{GlyphOutline, OutlineCommand, glyph_outlines};

/// Most glyphs of one animated text layer.
pub const MAX_ANIMATED_GLYPHS: usize = 1 << 16;
/// Largest absolute pixel coordinate a moved glyph may reach.
const MAX_ABS_PX: f64 = 1.0e7;

/// One placed glyph, ready to animate.
#[derive(Clone, Debug, PartialEq)]
pub struct AnimatedGlyph {
    /// Outline in document pixels (y down), at its pen position.
    pub path: OwnedPath,
    /// Baseline centre of the glyph's advance.
    pub anchor_px: [f64; 2],
    /// Pixels per em of the glyph's run (for tracking).
    pub em_px: f64,
    /// True when the outline is empty (spaces and other inkless glyphs).
    pub is_space: bool,
    pub line: u32,
}

fn pt(p: [f32; 2], pen: [f64; 2], scale: f64) -> PathPoint {
    PathPoint::new(pen[0] + f64::from(p[0]) * scale, pen[1] - f64::from(p[1]) * scale)
}

/// Places one Type outline at `pen_px` (module docs).
pub fn glyph_from_outline(
    outline: &GlyphOutline,
    pen_px: [f64; 2],
    scale: f64,
    advance_px: f64,
    em_px: f64,
    line: u32,
) -> Result<AnimatedGlyph, CompositeError> {
    let ok = |v: f64| v.is_finite() && v.abs() <= MAX_ABS_PX;
    if !(ok(pen_px[0]) && ok(pen_px[1]) && ok(advance_px) && ok(em_px) && scale.is_finite() && scale > 0.0) {
        return Err(CompositeError::Unavailable("text_animate:glyph_placement".into()));
    }
    let mut path = OwnedPath::new(FillRule::NonZero);
    for c in outline.commands() {
        path.elements.push(match *c {
            OutlineCommand::MoveTo(p) => PathElement::MoveTo(pt(p, pen_px, scale)),
            OutlineCommand::LineTo(p) => PathElement::LineTo(pt(p, pen_px, scale)),
            OutlineCommand::QuadTo(a, p) => PathElement::QuadTo(pt(a, pen_px, scale), pt(p, pen_px, scale)),
            OutlineCommand::CurveTo(a, b, p) => {
                PathElement::CubicTo(pt(a, pen_px, scale), pt(b, pen_px, scale), pt(p, pen_px, scale))
            }
            OutlineCommand::Close => PathElement::Close,
        });
    }
    Ok(AnimatedGlyph {
        is_space: path.elements.is_empty(),
        path,
        anchor_px: [pen_px[0] + advance_px / 2.0, pen_px[1]],
        em_px,
        line,
    })
}

/// Glyphs of caller-shaped runs (one run per placed line, as [`super::prepare_text`]), outlines
/// from Type, pen advance as `rasterize_run`. Line index = run index.
pub fn glyphs_from_runs(
    runs: &[TextRun<'_>],
    resolution: &TextResolution,
    cancel: &CancellationToken,
) -> Result<Vec<AnimatedGlyph>, CompositeError> {
    let total: usize = runs.iter().map(|r| r.run.glyphs().len()).fold(0, usize::saturating_add);
    if total > MAX_ANIMATED_GLYPHS {
        return Err(CompositeError::Unavailable("text_animate:glyph_count".into()));
    }
    let mut out = Vec::with_capacity(total);
    for (line, r) in runs.iter().enumerate() {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        let glyphs = r.run.glyphs();
        if glyphs.is_empty() {
            continue;
        }
        let ids: Vec<u32> = glyphs.iter().map(|g| g.glyph_id).collect();
        let outlines = glyph_outlines(&r.font, &ids, &resolution.outline_limits, cancel)
            .map_err(|e| CompositeError::Unavailable(format!("text_animate:{}", e.code())))?;
        let em_px = r.run.font_size_pt() * resolution.px_per_pt;
        let upem = r.run.units_per_em();
        if upem == 0 {
            return Err(CompositeError::Unavailable("text_animate:units_per_em".into()));
        }
        let scale = em_px / f64::from(upem);
        let (mut pen_x, mut pen_y) = (0.0f64, 0.0f64);
        let line = u32::try_from(line).map_err(|_| CompositeError::Overflow)?;
        for g in glyphs {
            let outline = outlines
                .iter()
                .find(|o| o.glyph_id() == g.glyph_id)
                .ok_or_else(|| CompositeError::Unavailable("text_animate:glyph_outline".into()))?;
            let pen = [
                r.origin_px[0] + pen_x + g.x_offset_pt * resolution.px_per_pt,
                r.origin_px[1] - (pen_y + g.y_offset_pt * resolution.px_per_pt),
            ];
            let advance = g.x_advance_pt * resolution.px_per_pt;
            out.push(glyph_from_outline(outline, pen, scale, advance, em_px, line)?);
            pen_x += advance;
            pen_y += g.y_advance_pt * resolution.px_per_pt;
        }
    }
    Ok(out)
}

/// Motion's text units of the glyphs: a word ends at a space; line = the glyph's line.
pub fn text_chars(glyphs: &[AnimatedGlyph]) -> Vec<TextChar> {
    let mut word = 0u32;
    let mut in_word = false;
    let mut line = glyphs.first().map_or(0, |g| g.line);
    glyphs
        .iter()
        .map(|g| {
            if g.line != line {
                line = g.line;
                if in_word {
                    word = word.saturating_add(1);
                    in_word = false;
                }
            }
            if g.is_space {
                if in_word {
                    word = word.saturating_add(1);
                    in_word = false;
                }
            } else {
                in_word = true;
            }
            TextChar { is_space: g.is_space, word, line: g.line }
        })
        .collect()
}

fn finite_transform(t: &CharTransform) -> bool {
    let mut v = vec![t.position[0], t.position[1], t.scale[0], t.scale[1], t.rotation, t.opacity, t.tracking];
    v.extend(t.fill.iter().flatten());
    v.iter().all(|x| x.is_finite())
}

/// The moved outline of one glyph (module docs).
pub fn move_glyph(glyph: &AnimatedGlyph, t: &CharTransform, shift_px: f64) -> Result<OwnedPath, CompositeError> {
    let (s, c) = sin_cos_deg(t.rotation);
    let ax = glyph.anchor_px[0] + shift_px;
    let ay = glyph.anchor_px[1];
    let map = |p: PathPoint| -> Result<PathPoint, CompositeError> {
        let lx = (p.x + shift_px - ax) * t.scale[0];
        let ly = (p.y - ay) * t.scale[1];
        let x = ax + lx * c - ly * s + t.position[0];
        let y = ay + lx * s + ly * c + t.position[1];
        if !(x.is_finite() && y.is_finite() && x.abs() <= MAX_ABS_PX && y.abs() <= MAX_ABS_PX) {
            return Err(CompositeError::Unavailable("text_animate:transform_range".into()));
        }
        Ok(PathPoint::new(x, y))
    };
    let mut out = OwnedPath::new(glyph.path.fill_rule);
    for e in &glyph.path.elements {
        out.elements.push(match *e {
            PathElement::MoveTo(p) => PathElement::MoveTo(map(p)?),
            PathElement::LineTo(p) => PathElement::LineTo(map(p)?),
            PathElement::QuadTo(a, p) => PathElement::QuadTo(map(a)?, map(p)?),
            PathElement::CubicTo(a, b, p) => PathElement::CubicTo(map(a)?, map(b)?, map(p)?),
            PathElement::Close => PathElement::Close,
        });
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
/// Lowers animated glyphs to static content (module docs). `transforms[i]` belongs to
/// `glyphs[i]` (from `hsk_studio_motion::evaluate_text_animators` over [`text_chars`]).
pub fn animated_text_content(
    layer_id: &str,
    glyphs: &[AnimatedGlyph],
    transforms: &[CharTransform],
    base_fill: [f32; 4],
    space: ColourTag,
    revision: u64,
    raster: &RasterOptions,
    cancel: &CancellationToken,
) -> Result<Option<StaticContent>, CompositeError> {
    if glyphs.len() > MAX_ANIMATED_GLYPHS {
        return Err(CompositeError::Unavailable(format!("{layer_id}:glyph_count")));
    }
    if transforms.len() != glyphs.len() {
        return Err(CompositeError::Unavailable(format!("{layer_id}:transform_count")));
    }
    if !transforms.iter().all(finite_transform) {
        return Err(CompositeError::Unavailable(format!("{layer_id}:transform_nonfinite")));
    }
    let mut planes = Vec::new();
    let mut shift = 0.0f64;
    let mut budget = DEFAULT_MAX_PAINTED_BYTES;
    for (g, t) in glyphs.iter().zip(transforms) {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        let this_shift = shift;
        shift += t.tracking / 1000.0 * g.em_px;
        if g.is_space || t.opacity <= 0.0 {
            continue;
        }
        let moved = move_glyph(g, t, this_shift)?;
        let Some(frame) = path_frame(&moved) else {
            continue;
        };
        let px = u64::from(frame.width).checked_mul(u64::from(frame.height)).ok_or(CompositeError::Overflow)?;
        if px > budget || px > raster.max_pixels {
            return Err(CompositeError::Unavailable(format!("{layer_id}:raster_over_limit")));
        }
        budget = budget.saturating_sub(px);
        let (coverage, _) = rasterize(&moved, frame, raster)
            .map_err(|e| CompositeError::Unavailable(format!("{layer_id}:nib_{e:?}")))?;
        let fill = match t.fill {
            Some(f) => [f[0] as f32, f[1] as f32, f[2] as f32, f[3] as f32],
            None => base_fill,
        };
        planes.push(PaintedPlane { coverage, rgba: fill, opacity: t.opacity.clamp(0.0, 1.0) as f32, tile: None });
    }
    painted_content(layer_id, planes, space, revision, DEFAULT_MAX_PAINTED_BYTES)
}
