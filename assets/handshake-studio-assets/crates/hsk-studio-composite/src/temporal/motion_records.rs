//! Folio motion records in compositions (lane SMB-AE-MOTION-RECORDS slice 2; feature `motion`):
//! a saved `StudioMotionRecords` makes composition layers PROCEDURAL. A layer with a shape-layer
//! record draws its shape group (`crate::vector_nib::shape_layer`) and a layer with a text-animator
//! record draws its caller-placed glyphs moved by the animators (`crate::text::animate`), both
//! evaluated at the frame's composition tick: keyframe streams through motion (`Property::sample`
//! via the composition keyframe conversion), wiggle time = the tick in seconds. The layer keeps its
//! composition transform, timing, opacity, matte and masks; content coordinates are layer-space
//! pixels (content origin 0, 0).
//!
//! Conversion is total and typed: every Folio item maps to its composite item; a field the record
//! cannot carry is refused as `TemporalError::Source { code }` with a stable code. Glyphs of a text
//! layer come from the caller (shaping is Type's, layout's and the caller's: Folio stores the story,
//! not shaped runs); a text-animator layer without glyphs is refused (`text_animator_glyphs`).
//! The expression selector needs an expression engine: it is refused (`text_expression_selector`)
//! unless the caller provides one ([`ExpressionSelectorEngine`]).
#![deny(clippy::arithmetic_side_effects)]
use super::TemporalError;
use super::composition::property;
use crate::composition::{SceneContent, StaticContent};
use crate::text::animate::{AnimatedGlyph, animated_text_content, text_chars};
use crate::vector_nib::shape_layer::track::{ShapeField, ShapeLayerTrack, ShapeStream};
use crate::vector_nib::shape_layer::{
    GroupTransform, ShapeFill, ShapeGradient, ShapeGroup, ShapeItem, ShapeStroke, WiggleTransform, shape_layer_content,
};
use hsk_studio_accord::{CancellationToken, TICKS_PER_SECOND};
use hsk_studio_folio::{
    StudioLineCap, StudioLineJoin, StudioMergeMode, StudioMotionRecords, StudioRangeShape, StudioShapeField,
    StudioShapeFillRule, StudioShapeGradient, StudioShapeGroup, StudioShapeItem, StudioShapePathCommand,
    StudioShapeStrokeStyle, StudioTextAnimators, StudioTextBasedOn, StudioTextParam, StudioTextSelector, StudioTrimMode,
};
use hsk_studio_motion::{
    Animator, AnimatorParam, AnimatorProperties, AnimatorTrack, BasedOn, EvalTick, RangeSelector, RangeShape, RangeUnits,
    Selector, TextChar, WigglySelector, evaluate_text_animators,
};
use hsk_studio_nib::raster::RasterOptions;
use hsk_studio_nib::shape_ops::{MergeMode, Polystar, RepeaterTransform, TrimMode, WiggleParams};
use hsk_studio_nib::stroke::{DashPattern, LineCap, LineJoin};
use hsk_studio_nib::{FillRule, OwnedPath, PathElement, PathPoint};
use hsk_studio_pigment::render_plan::ColourTag;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The motion `Property` of a Folio keyed value through the composition keyframe conversion (the
/// component count is the value's; the stream kind is not used by the conversion).
fn keyed_property(layer_id: &str, v: &hsk_studio_folio::StudioKeyedValue) -> Result<hsk_studio_motion::Property, TemporalError> {
    property(
        layer_id,
        &hsk_studio_folio::StudioPropertyStream {
            property: hsk_studio_folio::StudioLayerProperty::Position,
            static_value: v.static_value.clone(),
            keyframes: v.keyframes.clone(),
        },
    )
}

fn refuse(layer_id: &str, code: &str) -> TemporalError {
    TemporalError::Source { layer_id: layer_id.to_owned(), code: code.to_owned() }
}

/// Per-unit amounts of an expression selector: `amount[i]` (percent) for unit `i` of `total` units
/// given the selector value so far (the caller's expression engine; STU-MOT-091 `textIndex`,
/// `textTotal`, `selectorValue`).
pub trait ExpressionSelectorEngine: Send + Sync {
    fn amounts(&self, source: &str, total: usize, selector_value: &[f64], tick: i64) -> Result<Vec<f64>, String>;
}

fn rule(r: StudioShapeFillRule) -> FillRule {
    match r {
        StudioShapeFillRule::NonZero => FillRule::NonZero,
        StudioShapeFillRule::EvenOdd => FillRule::EvenOdd,
    }
}

fn cap(c: StudioLineCap) -> LineCap {
    match c {
        StudioLineCap::Butt => LineCap::Butt,
        StudioLineCap::Round => LineCap::Round,
        StudioLineCap::Square => LineCap::Square,
    }
}

fn join(j: StudioLineJoin) -> LineJoin {
    match j {
        StudioLineJoin::Miter => LineJoin::Miter,
        StudioLineJoin::Round => LineJoin::Round,
        StudioLineJoin::Bevel => LineJoin::Bevel,
    }
}

fn rgba(v: [f64; 4]) -> [f32; 4] {
    v.map(|c| c as f32)
}

fn stroke(rgba_: [f32; 4], opacity: f64, s: &StudioShapeStrokeStyle) -> ShapeStroke {
    ShapeStroke {
        rgba: rgba_,
        opacity,
        width: s.width,
        cap: cap(s.cap),
        join: join(s.join),
        miter_limit: s.miter_limit,
        dash: (!s.dashes.is_empty()).then(|| DashPattern { lengths: s.dashes.clone(), phase: s.dash_phase }),
    }
}

fn gradient(g: &StudioShapeGradient) -> ShapeGradient {
    ShapeGradient {
        radial: g.radial,
        start: g.start,
        end: g.end,
        stops: g.stops.iter().map(|s| (s.offset, rgba(s.rgba))).collect(),
    }
}

fn path(commands: &[StudioShapePathCommand]) -> OwnedPath {
    let p = |q: [f64; 2]| PathPoint::new(q[0], q[1]);
    let mut out = OwnedPath::new(FillRule::NonZero);
    for c in commands {
        out.elements.push(match *c {
            StudioShapePathCommand::MoveTo { p: q } => PathElement::MoveTo(p(q)),
            StudioShapePathCommand::LineTo { p: q } => PathElement::LineTo(p(q)),
            StudioShapePathCommand::CubicTo { c1, c2, p: q } => PathElement::CubicTo(p(c1), p(c2), p(q)),
            StudioShapePathCommand::Close => PathElement::Close,
        });
    }
    out
}

/// The composite shape group of a Folio shape group (total; module docs).
pub fn shape_group(g: &StudioShapeGroup) -> ShapeGroup {
    use StudioShapeItem as F;
    let items = g
        .items
        .iter()
        .map(|item| match item {
            F::Rect { position, size, roundness, reversed } => ShapeItem::Rect { position: *position, size: *size, roundness: *roundness, reversed: *reversed },
            F::Ellipse { position, size, reversed } => ShapeItem::Ellipse { position: *position, size: *size, reversed: *reversed },
            F::Polystar { star, points, position, rotation, outer_radius, inner_radius, outer_roundness, inner_roundness, reversed } => ShapeItem::Polystar(Polystar {
                star: *star,
                points: *points,
                position: *position,
                rotation: *rotation,
                outer_radius: *outer_radius,
                inner_radius: *inner_radius,
                outer_roundness: *outer_roundness,
                inner_roundness: *inner_roundness,
                reversed: *reversed,
            }),
            F::Path { commands, reversed } => ShapeItem::Path { path: path(commands), reversed: *reversed },
            F::Fill { rgba: c, opacity, rule: r } => ShapeItem::Fill(ShapeFill { rgba: rgba(*c), opacity: *opacity, rule: rule(*r) }),
            F::Stroke { rgba: c, opacity, style } => ShapeItem::Stroke(stroke(rgba(*c), *opacity, style)),
            F::GradientFill { gradient: gr, opacity, rule: r } => ShapeItem::GradientFill { gradient: gradient(gr), opacity: *opacity, rule: rule(*r) },
            F::GradientStroke { gradient: gr, opacity, style } => ShapeItem::GradientStroke { gradient: gradient(gr), stroke: stroke([1.0; 4], *opacity, style) },
            F::Trim { start, end, offset, mode } => ShapeItem::Trim {
                start: *start,
                end: *end,
                offset: *offset,
                mode: match mode {
                    StudioTrimMode::Simultaneously => TrimMode::Simultaneously,
                    StudioTrimMode::Individually => TrimMode::Individually,
                },
            },
            F::Offset { amount, join: j, miter_limit, copies } => ShapeItem::Offset { amount: *amount, join: join(*j), miter_limit: *miter_limit, copies: *copies },
            F::Merge { mode } => ShapeItem::Merge(match mode {
                StudioMergeMode::Merge => MergeMode::Merge,
                StudioMergeMode::Add => MergeMode::Add,
                StudioMergeMode::Subtract => MergeMode::Subtract,
                StudioMergeMode::Intersect => MergeMode::Intersect,
                StudioMergeMode::ExcludeIntersections => MergeMode::ExcludeIntersections,
            }),
            F::PuckerBloat { amount } => ShapeItem::PuckerBloat { amount: *amount },
            F::RoundCorners { radius } => ShapeItem::RoundCorners { radius: *radius },
            F::Twist { angle, center } => ShapeItem::Twist { angle: *angle, center: *center },
            F::WigglePaths { size, detail, smooth, wiggles_per_second, correlation, temporal_phase, spatial_phase, seed } => ShapeItem::WigglePaths(WiggleParams {
                size: *size,
                detail: *detail,
                smooth: *smooth,
                time_s: 0.0,
                wiggles_per_second: *wiggles_per_second,
                correlation: *correlation,
                temporal_phase: *temporal_phase,
                spatial_phase: *spatial_phase,
                seed: *seed,
            }),
            F::ZigZag { size, ridges, smooth } => ShapeItem::ZigZag { size: *size, ridges: *ridges, smooth: *smooth },
            F::WiggleTransform { anchor, position, scale, rotation, wiggles_per_second, temporal_phase, seed } => ShapeItem::WiggleTransform(WiggleTransform {
                anchor: *anchor,
                position: *position,
                scale: *scale,
                rotation: *rotation,
                wiggles_per_second: *wiggles_per_second,
                temporal_phase: *temporal_phase,
                seed: *seed,
            }),
            F::Repeater { copies, offset, anchor, position, scale, rotation, start_opacity, end_opacity, above } => ShapeItem::Repeater {
                copies: *copies,
                offset: *offset,
                transform: RepeaterTransform {
                    anchor: *anchor,
                    position: *position,
                    scale: *scale,
                    rotation: *rotation,
                    start_opacity: *start_opacity,
                    end_opacity: *end_opacity,
                },
                above: *above,
            },
            F::Group { group } => ShapeItem::Group(shape_group(group)),
        })
        .collect();
    let t = &g.transform;
    ShapeGroup {
        items,
        transform: GroupTransform { anchor: t.anchor, position: t.position, scale: t.scale, rotation: t.rotation, opacity: t.opacity },
    }
}

fn shape_field(f: StudioShapeField) -> ShapeField {
    use StudioShapeField as S;
    match f {
        S::TrimStart => ShapeField::TrimStart,
        S::TrimEnd => ShapeField::TrimEnd,
        S::TrimOffset => ShapeField::TrimOffset,
        S::RepeaterCopies => ShapeField::RepeaterCopies,
        S::RepeaterOffset => ShapeField::RepeaterOffset,
        S::RepeaterRotation => ShapeField::RepeaterRotation,
        S::OffsetAmount => ShapeField::OffsetAmount,
        S::PuckerAmount => ShapeField::PuckerAmount,
        S::RoundRadius => ShapeField::RoundRadius,
        S::TwistAngle => ShapeField::TwistAngle,
        S::ZigZagSize => ShapeField::ZigZagSize,
        S::RectSize => ShapeField::RectSize,
        S::RectPosition => ShapeField::RectPosition,
        S::RectRoundness => ShapeField::RectRoundness,
        S::EllipseSize => ShapeField::EllipseSize,
        S::EllipsePosition => ShapeField::EllipsePosition,
        S::PolystarOuterRadius => ShapeField::PolystarOuterRadius,
        S::PolystarRotation => ShapeField::PolystarRotation,
        S::FillOpacity => ShapeField::FillOpacity,
        S::StrokeWidth => ShapeField::StrokeWidth,
        S::StrokeOpacity => ShapeField::StrokeOpacity,
        S::GroupPosition => ShapeField::GroupPosition,
        S::GroupScale => ShapeField::GroupScale,
        S::GroupRotation => ShapeField::GroupRotation,
        S::GroupOpacity => ShapeField::GroupOpacity,
    }
}

fn based_on(b: StudioTextBasedOn) -> BasedOn {
    match b {
        StudioTextBasedOn::Characters => BasedOn::Characters,
        StudioTextBasedOn::CharactersExcludingSpaces => BasedOn::CharactersExcludingSpaces,
        StudioTextBasedOn::Words => BasedOn::Words,
        StudioTextBasedOn::Lines => BasedOn::Lines,
    }
}

/// One text animator of a record: the motion animator (expression selectors removed) and its
/// expression selectors (index, basis, source) applied after the others.
#[derive(Clone, Debug)]
pub(crate) struct TextAnimatorRec {
    track: AnimatorTrack,
    expressions: Vec<(BasedOn, String)>,
}

/// A procedural composition layer.
#[derive(Clone)]
pub(crate) enum ProceduralLayer {
    Shape(ShapeLayerTrack),
    Text { animators: Vec<TextAnimatorRec>, glyphs: Vec<AnimatedGlyph>, fill: [f32; 4] },
}

impl std::fmt::Debug for ProceduralLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Shape(_) => "ProceduralLayer::Shape",
            Self::Text { .. } => "ProceduralLayer::Text",
        })
    }
}

fn text_animators(rec: &StudioTextAnimators) -> Result<Vec<TextAnimatorRec>, TemporalError> {
    let id = rec.layer_id.as_str();
    let mut out = Vec::with_capacity(rec.animators.len());
    for a in &rec.animators {
        let mut selectors = Vec::new();
        let mut expressions = Vec::new();
        for s in &a.selectors {
            match s {
                StudioTextSelector::Range { start, end, offset, index_units, based_on: b, shape, smoothness, ease_high, ease_low, amount, randomize_order, random_seed } => {
                    selectors.push(Selector::Range(RangeSelector {
                        start: *start,
                        end: *end,
                        offset: *offset,
                        units: if *index_units { RangeUnits::Index } else { RangeUnits::Percent },
                        based_on: based_on(*b),
                        shape: match shape {
                            StudioRangeShape::Square => RangeShape::Square,
                            StudioRangeShape::RampUp => RangeShape::RampUp,
                            StudioRangeShape::RampDown => RangeShape::RampDown,
                            StudioRangeShape::Triangle => RangeShape::Triangle,
                            StudioRangeShape::Round => RangeShape::Round,
                            StudioRangeShape::Smooth => RangeShape::Smooth,
                        },
                        smoothness: *smoothness,
                        ease_high: *ease_high,
                        ease_low: *ease_low,
                        amount: *amount,
                        randomize_order: *randomize_order,
                        random_seed: *random_seed,
                    }));
                }
                StudioTextSelector::Wiggly { based_on: b, max_amount, min_amount, wiggles_per_second, correlation, temporal_phase, spatial_phase, lock_dimensions, random_seed } => {
                    selectors.push(Selector::Wiggly(WigglySelector {
                        based_on: based_on(*b),
                        max_amount: *max_amount,
                        min_amount: *min_amount,
                        wiggles_per_second: *wiggles_per_second,
                        correlation: *correlation,
                        temporal_phase: *temporal_phase,
                        spatial_phase: *spatial_phase,
                        lock_dimensions: *lock_dimensions,
                        random_seed: *random_seed,
                    }));
                }
                StudioTextSelector::Expression { based_on: b, source } => {
                    expressions.push((based_on(*b), source.clone()));
                }
            }
        }
        let p = &a.properties;
        let base = Animator {
            properties: AnimatorProperties { position: p.position, scale: p.scale, rotation: p.rotation, opacity: p.opacity, fill: p.fill, tracking: p.tracking },
            selectors,
        };
        out.push(TextAnimatorRec { track: AnimatorTrack { base, streams: Vec::new() }, expressions });
    }
    for st in &rec.streams {
        let ai = usize::try_from(st.animator).map_err(|_| refuse(id, "text_stream_animator"))?;
        let folio = rec.animators.get(ai).ok_or_else(|| refuse(id, "text_stream_animator"))?;
        // Motion selector index of a Folio selector index: skip expression selectors before it.
        let si = usize::try_from(st.selector).map_err(|_| refuse(id, "text_stream_selector"))?;
        let motion_si = folio
            .selectors
            .iter()
            .take(si)
            .filter(|s| !matches!(s, StudioTextSelector::Expression { .. }))
            .count();
        let param = match st.param {
            StudioTextParam::RangeStart => AnimatorParam::RangeStart(motion_si),
            StudioTextParam::RangeEnd => AnimatorParam::RangeEnd(motion_si),
            StudioTextParam::RangeOffset => AnimatorParam::RangeOffset(motion_si),
            StudioTextParam::RangeAmount => AnimatorParam::RangeAmount(motion_si),
            StudioTextParam::WigglyMax => AnimatorParam::WigglyMax(motion_si),
            StudioTextParam::WigglyMin => AnimatorParam::WigglyMin(motion_si),
            StudioTextParam::WigglyFrequency => AnimatorParam::WigglyFrequency(motion_si),
            StudioTextParam::Position => AnimatorParam::Position,
            StudioTextParam::Scale => AnimatorParam::Scale,
            StudioTextParam::Rotation => AnimatorParam::Rotation,
            StudioTextParam::Opacity => AnimatorParam::Opacity,
            StudioTextParam::Fill => AnimatorParam::Fill,
            StudioTextParam::Tracking => AnimatorParam::Tracking,
        };
        let property = keyed_property(id, &st.value)?;
        out.get_mut(ai).ok_or_else(|| refuse(id, "text_stream_animator"))?.track.streams.push((param, property));
    }
    Ok(out)
}

/// The procedural layers of `records` (shape layers; text-animator layers with caller glyphs and
/// fill from `glyphs`).
pub(crate) fn procedural_layers(
    records: &StudioMotionRecords,
    glyphs: &BTreeMap<String, (Vec<AnimatedGlyph>, [f32; 4])>,
) -> Result<BTreeMap<String, ProceduralLayer>, TemporalError> {
    let mut out = BTreeMap::new();
    for s in &records.shape_layers {
        let mut streams = Vec::with_capacity(s.streams.len());
        for st in &s.streams {
            streams.push(ShapeStream {
                path: st.path.iter().map(|i| usize::try_from(*i).map_err(|_| refuse(&s.layer_id, "shape_stream_path"))).collect::<Result<_, _>>()?,
                field: shape_field(st.field),
                property: keyed_property(&s.layer_id, &st.value)?,
            });
        }
        out.insert(s.layer_id.clone(), ProceduralLayer::Shape(ShapeLayerTrack { base: shape_group(&s.root), streams }));
    }
    for t in &records.text_animators {
        let Some((g, fill)) = glyphs.get(&t.layer_id) else {
            return Err(refuse(&t.layer_id, "text_animator_glyphs"));
        };
        if out.contains_key(&t.layer_id) {
            return Err(refuse(&t.layer_id, "procedural_layer_twice"));
        }
        out.insert(t.layer_id.clone(), ProceduralLayer::Text { animators: text_animators(t)?, glyphs: g.clone(), fill: *fill });
    }
    Ok(out)
}

fn units_total(chars: &[TextChar], b: BasedOn) -> (Vec<Option<usize>>, usize) {
    hsk_studio_motion::units_of(chars, b)
}

impl ProceduralLayer {
    /// The layer's content at composition tick `tick`.
    pub(crate) fn content_at(
        &self,
        layer_id: &str,
        tick: i64,
        space: ColourTag,
        revision: u64,
        expressions: Option<&dyn ExpressionSelectorEngine>,
        cancel: &CancellationToken,
    ) -> Result<SceneContent, TemporalError> {
        let seconds = tick as f64 / TICKS_PER_SECOND as f64;
        let motion = |e: hsk_studio_motion::MotionError| refuse(layer_id, e.code());
        let content: Option<StaticContent> = match self {
            ProceduralLayer::Shape(track) => {
                let root = track.at(EvalTick::new(tick), cancel).map_err(motion)?;
                shape_layer_content(layer_id, &root, seconds, space, revision, &RasterOptions::default(), cancel)?
            }
            ProceduralLayer::Text { animators, glyphs, fill } => {
                let chars = text_chars(glyphs);
                let mut resolved = Vec::with_capacity(animators.len());
                let mut extra: Vec<Vec<f64>> = Vec::with_capacity(animators.len());
                for a in animators {
                    let animator = a.track.at(EvalTick::new(tick), cancel).map_err(motion)?;
                    // Expression selectors multiply the per-character amount (STU-MOT-091).
                    let mut factor = vec![1.0; chars.len()];
                    if !a.expressions.is_empty() {
                        let Some(engine) = expressions else {
                            return Err(refuse(layer_id, "text_expression_selector"));
                        };
                        let base = hsk_studio_motion::selector_amounts(&chars, &animator.selectors, seconds).map_err(motion)?;
                        for (b, source) in &a.expressions {
                            let (units, total) = units_total(&chars, *b);
                            // Selector value per unit (percent): the first character of the unit.
                            let mut value = vec![0.0; total];
                            let mut seen = vec![false; total];
                            for (u, v) in units.iter().zip(&base) {
                                if let Some(i) = *u
                                    && let (Some(slot), Some(s)) = (value.get_mut(i), seen.get_mut(i))
                                    && !*s
                                {
                                    *slot = v[0] * 100.0;
                                    *s = true;
                                }
                            }
                            let amounts = engine
                                .amounts(source, total, &value, tick)
                                .map_err(|_| refuse(layer_id, "text_expression_selector_failed"))?;
                            if amounts.len() != total || amounts.iter().any(|v| !v.is_finite()) {
                                return Err(refuse(layer_id, "text_expression_selector_output"));
                            }
                            for (f, u) in factor.iter_mut().zip(&units) {
                                *f *= u.and_then(|i| amounts.get(i)).map_or(0.0, |v| v.clamp(0.0, 100.0) / 100.0);
                            }
                        }
                    }
                    extra.push(factor);
                    resolved.push(animator);
                }
                let base_fill = fill.map(f64::from);
                let direct = animators.iter().all(|a| a.expressions.is_empty());
                let mut transforms = if direct {
                    evaluate_text_animators(&chars, &resolved, base_fill, seconds).map_err(motion)?
                } else {
                    vec![hsk_studio_motion::CharTransform::IDENTITY; chars.len()]
                };
                for (animator, factor) in resolved.iter().zip(&extra).filter(|_| !direct) {
                    let one = evaluate_text_animators(&chars, std::slice::from_ref(animator), base_fill, seconds).map_err(motion)?;
                    // Re-apply with the expression factor: evaluate the animator's effect relative to
                    // identity and scale it by the factor (linear in the amount for every property).
                    for ((t, o), f) in transforms.iter_mut().zip(&one).zip(factor) {
                        t.position[0] += o.position[0] * f;
                        t.position[1] += o.position[1] * f;
                        t.scale[0] *= 1.0 + (o.scale[0] - 1.0) * f;
                        t.scale[1] *= 1.0 + (o.scale[1] - 1.0) * f;
                        t.rotation += o.rotation * f;
                        t.opacity *= 1.0 + (o.opacity - 1.0) * f;
                        t.tracking += o.tracking * f;
                        if let Some(to) = o.fill
                            && *f > 0.0
                        {
                            let from = t.fill.unwrap_or(base_fill);
                            let mut m = [0.0; 4];
                            for i in 0..4 {
                                m[i] = from[i] + (to[i] - from[i]) * f;
                            }
                            t.fill = Some(m);
                        }
                    }
                }
                animated_text_content(layer_id, glyphs, &transforms, *fill, space, revision, &RasterOptions::default(), cancel)?
            }
        };
        Ok(content.map_or(SceneContent::None, |c| SceneContent::Static(Box::new(c))))
    }
}

/// An [`ExpressionSelectorEngine`] over motion's expression port (any `ExpressionProvider`, e.g.
/// MotionJS's `MotionJsProvider`): unit `i` evaluates `var textIndex = i + 1; var textTotal = n;
/// var selectorValue = v;` followed by the source, with `base = [v]` and one output component
/// (the amount in percent). Each unit gets `fuel_per_unit`; no host reads (`NoHost`).
pub struct ProviderSelectorEngine<P> {
    pub provider: P,
    pub fuel_per_unit: u64,
}

impl<P: hsk_studio_motion::ExpressionProvider> ExpressionSelectorEngine for ProviderSelectorEngine<P> {
    fn amounts(&self, source: &str, total: usize, selector_value: &[f64], tick: i64) -> Result<Vec<f64>, String> {
        if total > hsk_studio_motion::MAX_TEXT_UNITS || selector_value.len() != total {
            return Err("count_limit".into());
        }
        if source.len() > hsk_studio_motion::MAX_EXPRESSION_BYTES {
            return Err("expression_too_long".into());
        }
        let cancel = CancellationToken::default();
        let mut out = Vec::with_capacity(total);
        for (i, v) in selector_value.iter().enumerate() {
            let full = format!(
                "var textIndex = {}; var textTotal = {total}; var selectorValue = {v:?};\n{source}",
                i.saturating_add(1)
            );
            let base = [*v];
            let request = hsk_studio_motion::ExprRequest {
                source: &full,
                revision: 0,
                tick: EvalTick::new(tick),
                seed: 0,
                dimension: 1,
                base: Some(&base),
            };
            let mut fuel = hsk_studio_motion::Fuel::new(self.fuel_per_unit);
            let value = self
                .provider
                .evaluate(&request, &hsk_studio_motion::NoHost, &mut fuel, &cancel)
                .map_err(|e| format!("{:?}", e.code))?;
            out.push(*value.first().ok_or_else(|| "output_type".to_owned())?);
        }
        Ok(out)
    }
}

/// The procedural layers of one composition source, their colour tag and plan revision, and the
/// caller's expression-selector engine.
#[derive(Clone)]
pub struct MotionLayers {
    layers: BTreeMap<String, ProceduralLayer>,
    space: ColourTag,
    revision: u64,
    engine: Option<Arc<dyn ExpressionSelectorEngine>>,
}

impl std::fmt::Debug for MotionLayers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MotionLayers")
            .field("layers", &self.layers)
            .field("revision", &self.revision)
            .field("engine", &self.engine.is_some())
            .finish()
    }
}

impl MotionLayers {
    /// The procedural content of `layer_id` at `tick` (`None` = not a procedural layer).
    pub(super) fn content(&self, layer_id: &str, tick: i64, cancel: &CancellationToken) -> Result<Option<SceneContent>, TemporalError> {
        match self.layers.get(layer_id) {
            None => Ok(None),
            Some(p) => p.content_at(layer_id, tick, self.space, self.revision, self.engine.as_deref(), cancel).map(Some),
        }
    }
}

impl super::composition::CompositionTemporalSource {
    /// Makes the layers named by `records` procedural (module docs). `glyphs` holds the placed
    /// glyphs and layer fill of every text-animator layer; `space` and `revision` are the colour tag
    /// and revision of the produced static subtrees (the composition's own); `engine` evaluates
    /// expression selectors (`None` = they are refused).
    pub fn with_motion_records(
        mut self,
        records: &StudioMotionRecords,
        glyphs: &BTreeMap<String, (Vec<AnimatedGlyph>, [f32; 4])>,
        space: ColourTag,
        revision: u64,
        engine: Option<Arc<dyn ExpressionSelectorEngine>>,
    ) -> Result<Self, TemporalError> {
        let layers = procedural_layers(records, glyphs)?;
        self.motion = Some(Box::new(MotionLayers { layers, space, revision, engine }));
        Ok(self)
    }
}
