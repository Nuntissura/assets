//! Shape layers (lane SMB-AE-MOTION-TOOLS slice 3; STU-MOT-100..104): a procedural vector graph of
//! ordered items evaluated every frame. Geometry is nib's (`hsk_studio_nib::shape_ops`, STU-MOT-104);
//! this module only orders the operators, captures paints and lowers the result through
//! [`crate::text::painted`] (Solid + coverage_u8, both backends, no new operator).
//!
//! Evaluation order (STU-MOT-102: authored order is semantic; Studio's stated rule, the After
//! Effects model as the MIT lottie-web reimplementation documents it, parity UNVERIFIED D5). Items
//! of a group are processed first to last (first = topmost in the layer panel):
//! * a path GENERATOR (rect, ellipse, polystar, path) adds a path slot;
//! * a PAINT (fill, stroke) captures every slot that exists at its position; it renders those
//!   slots' FINAL geometry, so a path modifier anywhere below the path also reaches the paint (a trim
//!   before or after the stroke trims the stroked geometry);
//! * a path MODIFIER (trim, offset, merge, pucker and bloat, round corners, twist, wiggle paths,
//!   zig zag, wiggle transform) rewrites every slot that exists at its position;
//! * a REPEATER copies every slot and every paint above it with its per-copy transform and opacity
//!   (copies above or below each other), and later items see the copies;
//! * a nested GROUP is evaluated alone, its slots and paints transformed by the group transform
//!   (`translate(position) . rotate . scale . translate(-anchor)`, opacity multiplies the paints)
//!   and appended at its position.
//!
//! Stacking: an earlier paint draws in front of a later one (panel order), the lowering draws
//! back to front. Fill: the captured slots as one path with the fill's rule, rasterised by nib.
//! Stroke: nib's stroke outline of the captured slots, rasterised by nib.
//!
//! Bounds: group depth, slot and paint counts are checked before every copy; nib bounds its own
//! work; the painted planes are bounded by pixel and byte budgets before allocation.
#![deny(clippy::arithmetic_side_effects)]
use crate::CompositeError;
use crate::composition::StaticContent;
use crate::text::painted::{DEFAULT_MAX_PAINTED_BYTES, PaintedPlane, painted_content, path_frame};
use hsk_studio_accord::CancellationToken;
use hsk_studio_nib::boolean::BooleanOptions;
use hsk_studio_nib::raster::{Affine, Gradient, GradientGeometry, GradientStop, Interpolation, Paint, RasterOptions, Spread, rasterize};
use hsk_studio_nib::shape_ops::{
    MergeMode, Polystar, RepeaterTransform, TrimMode, WiggleParams, affine_mul, anchored_transform,
    ellipse_path, merge_paths, offset_path, path_noise, polystar_path, pucker_bloat, rect_path,
    repeater, reverse_path, round_corners, transform_path, trim_many, twist, wiggle_path, zig_zag,
};
use hsk_studio_nib::stroke::{DashPattern, LineCap, LineJoin, StrokeOptions, StrokeStyle, outline_stroke};
use hsk_studio_nib::{Error as NibError, FillRule, OwnedPath};
use hsk_studio_pigment::render_plan::ColourTag;

/// Deepest group nesting.
pub const MAX_SHAPE_DEPTH: usize = 32;
/// Most path slots of one evaluation (repeater copies included).
pub const MAX_SHAPE_PATHS: usize = 4096;
/// Most paints of one evaluation (repeater copies included).
pub const MAX_SHAPE_PAINTS: usize = 4096;

/// A group transform; scale and opacity in percent.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupTransform {
    pub anchor: [f64; 2],
    pub position: [f64; 2],
    pub scale: [f64; 2],
    pub rotation: f64,
    pub opacity: f64,
}

impl GroupTransform {
    pub const IDENTITY: Self = Self { anchor: [0.0; 2], position: [0.0; 2], scale: [100.0; 2], rotation: 0.0, opacity: 100.0 };
}

/// Fill paint (opacity percent).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeFill {
    pub rgba: [f32; 4],
    pub opacity: f64,
    pub rule: FillRule,
}

/// Stroke paint (opacity percent).
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeStroke {
    pub rgba: [f32; 4],
    pub opacity: f64,
    pub width: f64,
    pub cap: LineCap,
    pub join: LineJoin,
    pub miter_limit: f64,
    pub dash: Option<DashPattern>,
}

/// A shape gradient (After Effects gradient fill / stroke, lane SMB-AE-MOTION-RECORDS slice 3):
/// linear from `start` to `end`, or radial centred at `start` with radius `|end - start|`, in the
/// coordinates of the group that holds the paint; the paint's accumulated group, repeater and
/// wiggle transforms move it with the geometry. Evaluated by nib (`raster::paint_coverage`, classic
/// straight interpolation, pad spread) over the coverage frame. AE gradient parity UNVERIFIED (D5).
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeGradient {
    pub radial: bool,
    pub start: [f64; 2],
    pub end: [f64; 2],
    /// `(offset 0..=1, straight RGBA)`, offsets non-decreasing.
    pub stops: Vec<(f64, [f32; 4])>,
}

/// Seeded temporal noise on the transform of everything above it (STU-MOT-103 wiggle transform):
/// `n_k = path_noise(seed, k, time * frequency + temporal_phase / 360)`; translate `position * (n_0,
/// n_1)`, rotate `rotation * n_2`, scale `1 + scale / 100 * (n_3, n_4)` about `anchor`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WiggleTransform {
    pub anchor: [f64; 2],
    pub position: [f64; 2],
    pub scale: [f64; 2],
    pub rotation: f64,
    pub wiggles_per_second: f64,
    pub temporal_phase: f64,
    pub seed: u64,
}

/// One item of a shape group (module docs).
#[derive(Clone, Debug, PartialEq)]
pub enum ShapeItem {
    Rect { position: [f64; 2], size: [f64; 2], roundness: f64, reversed: bool },
    Ellipse { position: [f64; 2], size: [f64; 2], reversed: bool },
    Polystar(Polystar),
    Path { path: OwnedPath, reversed: bool },
    Fill(ShapeFill),
    Stroke(ShapeStroke),
    /// Opacity in percent.
    GradientFill { gradient: ShapeGradient, opacity: f64, rule: FillRule },
    /// The stroke's `rgba` is unused; its opacity applies.
    GradientStroke { gradient: ShapeGradient, stroke: ShapeStroke },
    Trim { start: f64, end: f64, offset: f64, mode: TrimMode },
    Offset { amount: f64, join: LineJoin, miter_limit: f64, copies: u32 },
    Merge(MergeMode),
    PuckerBloat { amount: f64 },
    RoundCorners { radius: f64 },
    Twist { angle: f64, center: [f64; 2] },
    /// `time_s` is replaced by the evaluation time.
    WigglePaths(WiggleParams),
    ZigZag { size: f64, ridges: u32, smooth: bool },
    WiggleTransform(WiggleTransform),
    Repeater { copies: f64, offset: f64, transform: RepeaterTransform, above: bool },
    Group(ShapeGroup),
}

/// A shape group: ordered items plus its transform. A shape layer is its root group.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapeGroup {
    pub items: Vec<ShapeItem>,
    pub transform: GroupTransform,
}

/// What a paint draws.
#[derive(Clone, Debug, PartialEq)]
pub enum ShapePaint {
    Fill(ShapeFill),
    Stroke(ShapeStroke),
    GradientFill(ShapeGradient, FillRule),
    GradientStroke(ShapeGradient, ShapeStroke),
}

/// One evaluated paint: its final geometry (the captured slots as one path), paint and opacity
/// factor (paint opacity times group and repeater opacities).
#[derive(Clone, Debug, PartialEq)]
pub struct PaintedShape {
    pub path: OwnedPath,
    pub paint: ShapePaint,
    pub opacity: f64,
    /// Accumulated transform of the paint's gradient space.
    pub transform: Affine,
}

#[derive(Clone, Debug)]
struct PaintRec {
    slots: Vec<usize>,
    paint: ShapePaint,
    opacity: f64,
    xform: Affine,
}

#[derive(Clone, Debug, Default)]
struct State {
    slots: Vec<OwnedPath>,
    /// Front first.
    paints: Vec<PaintRec>,
}

fn nib(e: NibError) -> &'static str {
    match e {
        NibError::BudgetExceeded | NibError::Overflow => "shape_budget",
        _ => "shape_geometry",
    }
}

fn finite(v: &[f64]) -> Result<(), &'static str> {
    if v.iter().all(|x| x.is_finite()) { Ok(()) } else { Err("shape_parameter") }
}

fn add_slot(st: &mut State, path: OwnedPath) -> Result<(), &'static str> {
    if st.slots.len() >= MAX_SHAPE_PATHS {
        return Err("shape_path_count");
    }
    st.slots.push(path);
    Ok(())
}

fn map_slots(st: &mut State, f: impl Fn(&OwnedPath) -> Result<OwnedPath, NibError>) -> Result<(), &'static str> {
    for s in st.slots.iter_mut() {
        if !s.elements.is_empty() {
            *s = f(s).map_err(nib)?;
        }
    }
    Ok(())
}

fn transform_state(st: &mut State, m: &Affine) -> Result<(), &'static str> {
    for p in st.paints.iter_mut() {
        p.xform = affine_mul(m, &p.xform);
    }
    map_slots(st, |p| transform_path(p, m))
}

fn gradient_ok(g: &ShapeGradient) -> Result<(), &'static str> {
    finite(&[g.start[0], g.start[1], g.end[0], g.end[1]])?;
    let unit = |v: f32| v.is_finite() && (0.0..=1.0).contains(&v);
    if g.stops.is_empty()
        || g.stops.len() > hsk_studio_nib::raster::MAX_STOPS
        || !g.stops.iter().all(|(o, c)| o.is_finite() && (0.0..=1.0).contains(o) && c.iter().all(|v| unit(*v)))
        || g.stops.windows(2).any(|w| w[0].0 > w[1].0)
    {
        return Err("shape_gradient");
    }
    Ok(())
}

fn push_paint(st: &mut State, paint: ShapePaint, opacity: f64) -> Result<(), &'static str> {
    finite(&[opacity])?;
    if !(0.0..=100.0).contains(&opacity) {
        return Err("shape_parameter");
    }
    if st.paints.len() >= MAX_SHAPE_PAINTS {
        return Err("shape_paint_count");
    }
    let slots = (0..st.slots.len()).collect();
    st.paints.push(PaintRec { slots, paint, opacity: opacity / 100.0, xform: hsk_studio_nib::raster::IDENTITY });
    Ok(())
}

fn group_matrix(t: &GroupTransform) -> Result<Affine, &'static str> {
    finite(&[t.anchor[0], t.anchor[1], t.position[0], t.position[1], t.scale[0], t.scale[1], t.rotation, t.opacity])?;
    if !(0.0..=100.0).contains(&t.opacity) {
        return Err("shape_parameter");
    }
    Ok(anchored_transform(
        t.anchor,
        [t.position[0] - t.anchor[0], t.position[1] - t.anchor[1]],
        t.rotation,
        [t.scale[0] / 100.0, t.scale[1] / 100.0],
    ))
}

fn eval_group(g: &ShapeGroup, time_s: f64, depth: usize, cancel: &CancellationToken) -> Result<State, &'static str> {
    if depth > MAX_SHAPE_DEPTH {
        return Err("shape_depth");
    }
    let mut st = State::default();
    let bo = BooleanOptions::default();
    let so = StrokeOptions::default();
    for item in &g.items {
        cancel.check().map_err(|_| "canceled")?;
        match item {
            ShapeItem::Rect { position, size, roundness, reversed } => {
                add_slot(&mut st, rect_path(*position, *size, *roundness, *reversed).map_err(nib)?)?;
            }
            ShapeItem::Ellipse { position, size, reversed } => {
                add_slot(&mut st, ellipse_path(*position, *size, *reversed).map_err(nib)?)?;
            }
            ShapeItem::Polystar(p) => add_slot(&mut st, polystar_path(p).map_err(nib)?)?,
            ShapeItem::Path { path, reversed } => {
                let p = if *reversed { reverse_path(path).map_err(nib)? } else { transform_path(path, &hsk_studio_nib::raster::IDENTITY).map_err(nib)? };
                add_slot(&mut st, p)?;
            }
            ShapeItem::Fill(f) => push_paint(&mut st, ShapePaint::Fill(*f), f.opacity)?,
            ShapeItem::Stroke(s) => {
                finite(&[s.width, s.miter_limit])?;
                push_paint(&mut st, ShapePaint::Stroke(s.clone()), s.opacity)?;
            }
            ShapeItem::GradientFill { gradient, opacity, rule } => {
                gradient_ok(gradient)?;
                push_paint(&mut st, ShapePaint::GradientFill(gradient.clone(), *rule), *opacity)?;
            }
            ShapeItem::GradientStroke { gradient, stroke } => {
                gradient_ok(gradient)?;
                finite(&[stroke.width, stroke.miter_limit])?;
                push_paint(&mut st, ShapePaint::GradientStroke(gradient.clone(), stroke.clone()), stroke.opacity)?;
            }
            ShapeItem::Trim { start, end, offset, mode } => {
                st.slots = trim_many(&st.slots, *start, *end, *offset, *mode).map_err(nib)?;
            }
            ShapeItem::Offset { amount, join, miter_limit, copies } => {
                map_slots(&mut st, |p| offset_path(p, *amount, *join, *miter_limit, *copies, &so))?;
            }
            ShapeItem::Merge(mode) => {
                if !st.slots.is_empty() {
                    let live: Vec<OwnedPath> = st.slots.iter().filter(|p| !p.elements.is_empty()).cloned().collect();
                    let merged = merge_paths(&live, *mode, &bo).map_err(nib)?;
                    for s in st.slots.iter_mut() {
                        s.elements.clear();
                    }
                    st.slots[0] = merged;
                }
            }
            ShapeItem::PuckerBloat { amount } => map_slots(&mut st, |p| pucker_bloat(p, *amount))?,
            ShapeItem::RoundCorners { radius } => map_slots(&mut st, |p| round_corners(p, *radius))?,
            ShapeItem::Twist { angle, center } => map_slots(&mut st, |p| twist(p, *angle, *center))?,
            ShapeItem::WigglePaths(w) => {
                let w = WiggleParams { time_s, ..*w };
                map_slots(&mut st, |p| wiggle_path(p, &w))?;
            }
            ShapeItem::ZigZag { size, ridges, smooth } => map_slots(&mut st, |p| zig_zag(p, *size, *ridges, *smooth))?,
            ShapeItem::WiggleTransform(w) => {
                finite(&[w.anchor[0], w.anchor[1], w.position[0], w.position[1], w.scale[0], w.scale[1], w.rotation, w.wiggles_per_second, w.temporal_phase, time_s])?;
                let x = time_s * w.wiggles_per_second + w.temporal_phase / 360.0;
                let n: Vec<f64> = (0..5u64).map(|k| path_noise(w.seed, k, x)).collect();
                let m = anchored_transform(
                    w.anchor,
                    [w.position[0] * n[0], w.position[1] * n[1]],
                    w.rotation * n[2],
                    [1.0 + w.scale[0] / 100.0 * n[3], 1.0 + w.scale[1] / 100.0 * n[4]],
                );
                transform_state(&mut st, &m)?;
            }
            ShapeItem::Repeater { copies, offset, transform, above } => {
                let list = repeater(*copies, *offset, transform).map_err(nib)?;
                let m = st.slots.len();
                if list.len().checked_mul(m).is_none_or(|n| n > MAX_SHAPE_PATHS)
                    || list.len().checked_mul(st.paints.len()).is_none_or(|n| n > MAX_SHAPE_PAINTS)
                {
                    return Err("shape_repeater_budget");
                }
                let mut slots = Vec::new();
                for (mat, _) in &list {
                    for s in &st.slots {
                        slots.push(if s.elements.is_empty() { s.clone() } else { transform_path(s, mat).map_err(nib)? });
                    }
                }
                let order: Vec<usize> = if *above { (0..list.len()).rev().collect() } else { (0..list.len()).collect() };
                let mut paints = Vec::new();
                for c in order {
                    let base = c.checked_mul(m).ok_or("shape_repeater_budget")?;
                    for p in &st.paints {
                        let remapped = p.slots.iter().map(|s| base.checked_add(*s).ok_or("shape_repeater_budget")).collect::<Result<_, _>>()?;
                        paints.push(PaintRec { slots: remapped, paint: p.paint.clone(), opacity: p.opacity * list[c].1, xform: affine_mul(&list[c].0, &p.xform) });
                    }
                }
                st.slots = slots;
                st.paints = paints;
            }
            ShapeItem::Group(child) => {
                let next = depth.checked_add(1).ok_or("shape_depth")?;
                let mut inner = eval_group(child, time_s, next, cancel)?;
                let mat = group_matrix(&child.transform)?;
                transform_state(&mut inner, &mat)?;
                let base = st.slots.len();
                if base.checked_add(inner.slots.len()).is_none_or(|n| n > MAX_SHAPE_PATHS)
                    || st.paints.len().checked_add(inner.paints.len()).is_none_or(|n| n > MAX_SHAPE_PAINTS)
                {
                    return Err("shape_path_count");
                }
                let factor = child.transform.opacity / 100.0;
                st.slots.extend(inner.slots);
                for p in inner.paints {
                    let slots = p.slots.iter().map(|s| base.checked_add(*s).ok_or("shape_path_count")).collect::<Result<_, _>>()?;
                    st.paints.push(PaintRec { slots, paint: p.paint, opacity: p.opacity * factor, xform: p.xform });
                }
            }
        }
    }
    Ok(st)
}

/// Evaluates a shape layer's root group at `time_s` seconds: paints BACK TO FRONT with their final
/// geometry (module docs). The root group's own transform applies too.
pub fn evaluate_shape_layer(root: &ShapeGroup, time_s: f64, cancel: &CancellationToken) -> Result<Vec<PaintedShape>, &'static str> {
    if !time_s.is_finite() {
        return Err("shape_parameter");
    }
    let mut st = eval_group(root, time_s, 0, cancel)?;
    let m = group_matrix(&root.transform)?;
    transform_state(&mut st, &m)?;
    let factor = root.transform.opacity / 100.0;
    let mut out = Vec::with_capacity(st.paints.len());
    for p in st.paints.iter().rev() {
        let rule = match &p.paint {
            ShapePaint::Fill(f) => f.rule,
            ShapePaint::GradientFill(_, r) => *r,
            ShapePaint::Stroke(_) | ShapePaint::GradientStroke(..) => FillRule::NonZero,
        };
        let mut path = OwnedPath::new(rule);
        for s in &p.slots {
            path.elements.extend(st.slots.get(*s).ok_or("shape_geometry")?.elements.iter().copied());
            if path.elements.len() > hsk_studio_nib::shape_ops::MAX_OP_ELEMENTS {
                return Err("shape_budget");
            }
        }
        out.push(PaintedShape { path, paint: p.paint.clone(), opacity: p.opacity * factor, transform: p.xform });
    }
    Ok(out)
}

/// Evaluates and lowers a shape layer to static content (`None` = nothing visible).
pub fn shape_layer_content(
    layer_id: &str,
    root: &ShapeGroup,
    time_s: f64,
    space: ColourTag,
    revision: u64,
    raster: &RasterOptions,
    cancel: &CancellationToken,
) -> Result<Option<StaticContent>, CompositeError> {
    let refuse = |code: &str| {
        if code == "canceled" { CompositeError::Canceled } else { CompositeError::Unavailable(format!("{layer_id}:{code}")) }
    };
    let shapes = evaluate_shape_layer(root, time_s, cancel).map_err(refuse)?;
    let mut planes = Vec::with_capacity(shapes.len());
    let mut budget = DEFAULT_MAX_PAINTED_BYTES;
    for s in shapes {
        cancel.check().map_err(|_| CompositeError::Canceled)?;
        if s.path.elements.is_empty() || s.opacity <= 0.0 {
            continue;
        }
        let outline = |k: &ShapeStroke| {
            let style = StrokeStyle { cap: k.cap, join: k.join, miter_limit: k.miter_limit, dash: k.dash.clone(), ..StrokeStyle::new(k.width) };
            match outline_stroke(&s.path, &style, &StrokeOptions::default()) {
                Ok(r) => Ok(Some(r.path)),
                Err(NibError::EmptyPathfinderResult) => Ok(None),
                Err(e) => Err(refuse(nib(e))),
            }
        };
        let (geometry, rgba, gradient) = match &s.paint {
            ShapePaint::Fill(f) => (s.path.clone(), f.rgba, None),
            ShapePaint::GradientFill(g, _) => (s.path.clone(), [1.0; 4], Some(g)),
            ShapePaint::Stroke(k) => match outline(k)? {
                Some(p) => (p, k.rgba, None),
                None => continue,
            },
            ShapePaint::GradientStroke(g, k) => match outline(k)? {
                Some(p) => (p, [1.0; 4], Some(g)),
                None => continue,
            },
        };
        let Some(frame) = path_frame(&geometry) else { continue };
        let px = u64::from(frame.width).checked_mul(u64::from(frame.height)).ok_or(CompositeError::Overflow)?;
        if px > budget || px > raster.max_pixels {
            return Err(refuse("raster_over_limit"));
        }
        budget = budget.saturating_sub(px);
        let (coverage, _) = rasterize(&geometry, frame, raster).map_err(|e| refuse(nib(e)))?;
        let tile = match gradient {
            None => None,
            Some(g) => {
                let rect = hsk_studio_pigment::Rect { x: i64::from(frame.x), y: i64::from(frame.y), width: frame.width, height: frame.height };
                let geometry = if g.radial {
                    let r = ((g.end[0] - g.start[0]).powi(2) + (g.end[1] - g.start[1]).powi(2)).sqrt();
                    GradientGeometry::Radial { cx: g.start[0], cy: g.start[1], r, fx: g.start[0], fy: g.start[1], fr: 0.0 }
                } else {
                    GradientGeometry::Linear { x1: g.start[0], y1: g.start[1], x2: g.end[0], y2: g.end[1] }
                };
                let paint = Paint::Gradient(Gradient {
                    geometry,
                    transform: s.transform,
                    spread: Spread::Pad,
                    interpolation: Interpolation::Classic,
                    dither: false,
                    stops: g.stops.iter().map(|(offset, colour)| GradientStop { offset: *offset, colour: *colour, midpoint: None }).collect(),
                });
                Some(crate::vector_nib::painted(&paint, rect, raster.max_pixels).map_err(refuse)?)
            }
        };
        planes.push(PaintedPlane { coverage, rgba, opacity: s.opacity.clamp(0.0, 1.0) as f32, tile });
    }
    painted_content(layer_id, planes, space, revision, DEFAULT_MAX_PAINTED_BYTES)
}

/// Keyframable shape parameters (feature `motion`): every parameter is an ordinary motion
/// `Property` sampled at the frame's tick (STU-MOT-100).
#[cfg(feature = "motion")]
pub mod track {
    use super::{ShapeGroup, ShapeItem};
    use hsk_studio_accord::CancellationToken;
    use hsk_studio_motion::{EvalTick, MotionError, Property};

    /// The field a stream drives; `Group*` address the group at the item path (empty = root).
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum ShapeField {
        TrimStart,
        TrimEnd,
        TrimOffset,
        RepeaterCopies,
        RepeaterOffset,
        RepeaterRotation,
        OffsetAmount,
        PuckerAmount,
        RoundRadius,
        TwistAngle,
        ZigZagSize,
        RectSize,
        RectPosition,
        RectRoundness,
        EllipseSize,
        EllipsePosition,
        PolystarOuterRadius,
        PolystarRotation,
        FillOpacity,
        StrokeWidth,
        StrokeOpacity,
        GroupPosition,
        GroupScale,
        GroupRotation,
        GroupOpacity,
    }

    impl ShapeField {
        fn dimension(self) -> usize {
            match self {
                Self::RectSize | Self::RectPosition | Self::EllipseSize | Self::EllipsePosition | Self::GroupPosition | Self::GroupScale => 2,
                _ => 1,
            }
        }
    }

    /// One stream: item index path from the root group (each index selects an item; every index
    /// but the last must be a group) and the field.
    #[derive(Clone, Debug)]
    pub struct ShapeStream {
        pub path: Vec<usize>,
        pub field: ShapeField,
        pub property: Property,
    }

    /// A shape layer whose parameters are motion streams.
    #[derive(Clone, Debug)]
    pub struct ShapeLayerTrack {
        pub base: ShapeGroup,
        pub streams: Vec<ShapeStream>,
    }

    fn bad() -> MotionError {
        MotionError::InvalidParameter { parameter: "shape_stream" }
    }

    fn group_at<'a>(root: &'a mut ShapeGroup, path: &[usize]) -> Result<&'a mut ShapeGroup, MotionError> {
        let mut g = root;
        for i in path {
            match g.items.get_mut(*i) {
                Some(ShapeItem::Group(child)) => g = child,
                _ => return Err(MotionError::SelectionOutOfRange),
            }
        }
        Ok(g)
    }

    impl ShapeLayerTrack {
        /// The root group with every stream sampled at `tick` (`Property::sample`).
        pub fn at(&self, tick: EvalTick, cancel: &CancellationToken) -> Result<ShapeGroup, MotionError> {
            let mut root = self.base.clone();
            for s in &self.streams {
                if s.property.dimension() != s.field.dimension() {
                    return Err(MotionError::ComponentMismatch);
                }
                let v = s.property.sample(tick, cancel)?;
                use ShapeField as F;
                if matches!(s.field, F::GroupPosition | F::GroupScale | F::GroupRotation | F::GroupOpacity) {
                    let g = group_at(&mut root, &s.path)?;
                    match s.field {
                        F::GroupPosition => g.transform.position = [v[0], v[1]],
                        F::GroupScale => g.transform.scale = [v[0], v[1]],
                        F::GroupRotation => g.transform.rotation = v[0],
                        _ => g.transform.opacity = v[0],
                    }
                    continue;
                }
                let (last, parents) = s.path.split_last().ok_or(MotionError::SelectionOutOfRange)?;
                let g = group_at(&mut root, parents)?;
                let item = g.items.get_mut(*last).ok_or(MotionError::SelectionOutOfRange)?;
                match (item, s.field) {
                    (ShapeItem::Trim { start, .. }, F::TrimStart) => *start = v[0],
                    (ShapeItem::Trim { end, .. }, F::TrimEnd) => *end = v[0],
                    (ShapeItem::Trim { offset, .. }, F::TrimOffset) => *offset = v[0],
                    (ShapeItem::Repeater { copies, .. }, F::RepeaterCopies) => *copies = v[0],
                    (ShapeItem::Repeater { offset, .. }, F::RepeaterOffset) => *offset = v[0],
                    (ShapeItem::Repeater { transform, .. }, F::RepeaterRotation) => transform.rotation = v[0],
                    (ShapeItem::Offset { amount, .. }, F::OffsetAmount) => *amount = v[0],
                    (ShapeItem::PuckerBloat { amount }, F::PuckerAmount) => *amount = v[0],
                    (ShapeItem::RoundCorners { radius }, F::RoundRadius) => *radius = v[0],
                    (ShapeItem::Twist { angle, .. }, F::TwistAngle) => *angle = v[0],
                    (ShapeItem::ZigZag { size, .. }, F::ZigZagSize) => *size = v[0],
                    (ShapeItem::Rect { size, .. }, F::RectSize) => *size = [v[0], v[1]],
                    (ShapeItem::Rect { position, .. }, F::RectPosition) => *position = [v[0], v[1]],
                    (ShapeItem::Rect { roundness, .. }, F::RectRoundness) => *roundness = v[0],
                    (ShapeItem::Ellipse { size, .. }, F::EllipseSize) => *size = [v[0], v[1]],
                    (ShapeItem::Ellipse { position, .. }, F::EllipsePosition) => *position = [v[0], v[1]],
                    (ShapeItem::Polystar(p), F::PolystarOuterRadius) => p.outer_radius = v[0],
                    (ShapeItem::Polystar(p), F::PolystarRotation) => p.rotation = v[0],
                    (ShapeItem::Fill(f), F::FillOpacity) => f.opacity = v[0],
                    (ShapeItem::Stroke(k), F::StrokeWidth) => k.width = v[0],
                    (ShapeItem::Stroke(k), F::StrokeOpacity) => k.opacity = v[0],
                    _ => return Err(bad()),
                }
            }
            Ok(root)
        }
    }
}
