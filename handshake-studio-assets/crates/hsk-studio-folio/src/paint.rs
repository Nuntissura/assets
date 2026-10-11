//! Paint, stroke and appearance-row records (spec STU-VEC-015, STU-VEC-020, STU-VEC-022,
//! STU-VEC-134..137, STU-VEC-143..145, STU-DS-034b, STU-DS-134/135): a solid or gradient paint, a
//! stroke geometry record and the ordered appearance stack that a `PAINT` style entry holds. Folio
//! stores and validates only; rasterising a gradient, stroking and blending belong to the render and
//! pigment crates.
//!
//! A stack lives in a standalone `StudioPaintStyle` (an `SSTY` style-registry entry) that a node names
//! through `StudioNodeDesign::style_refs` (the STU-DS-136 fields); `StudioLayer` and `LayerAppearance`
//! are untouched, so every struct-literal constructor of them keeps compiling.
//!
//! Units: a stroke weight, dash and dash offset are Folio `Length` values in any unit; the spec unit
//! is the document unit and a standalone style does not know it, so the consumer converts. Gradient
//! geometry numbers are in gradient space (dimensionless); `transform` maps gradient space to
//! document geometry units.
//!
//! Declared gaps, not invented here: paint kinds `none`, `process_cmyk`, `gray`, `spot`, `lab`,
//! `pattern`, `image`, `video` (STU-VEC-134) and gradient kinds `angular`, `diamond`, `freeform`,
//! `mesh` (STU-VEC-135) and the raster `StudioGradient` form (noise, separate transparency stop list,
//! STU-RAS-160) are not stored; effect rows and the per-row effect scope (STU-VEC-023) are not
//! stored; stroke per-side weights, `stroke_cap`/`stroke_join` per network vertex, dash cap, dash
//! corner adjustment, gap colour and arrowheads (STU-VEC-143..146) are not stored; gradient
//! application mode on strokes (`within`/`along`/`across`, STU-VEC-016) is not stored. Two members
//! are carried that the spec lists no field for, because SVG and PDF gradients and dashes need them:
//! the gradient `spread` and the stroke `dash_offset` (STU-VEC-145 states no dash phase).
use super::{
    BLEND_LAST, BLEND_PASS_THROUGH, BLEND_TOOL_ONLY, Code, Length, Result, err, id,
    values::{Ctx, StudioColor, StudioPublishing, percent, slash_path},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

schema_token!(StyleRegistrySchema, "hsk.studio.style_registry@1");

/// Gradient stops per gradient (a Folio hard ceiling; the spec declares none).
pub const MAX_GRADIENT_STOPS: usize = 1024;
/// Appearance rows per style (a Folio hard ceiling; the spec declares none).
pub const MAX_APPEARANCE_ROWS: usize = 256;
/// Dash and gap lengths per stroke (a Folio hard ceiling; STU-VEC-145 states an unbounded array).
pub const MAX_DASH_ENTRIES: usize = 256;

/// STU-VEC-137 gradient interpolation: `classic` is captured value 0, `perceptual` is 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GradientInterpolation {
    Classic,
    Perceptual,
}

/// What a gradient does outside its first and last stop (not a spec member; see the module note).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GradientSpread {
    Pad,
    Reflect,
    Repeat,
}

/// STU-VEC-136 gradient stop. `position` and `opacity` are percent; `midpoint` is the percent skew
/// toward the next stop (hard range `13..=87`), or explicit null for none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGradientStop {
    #[schemars(range(min = 0.0, max = 100.0))]
    pub position: f64,
    #[serde(deserialize_with = "super::required")]
    #[schemars(required)]
    pub midpoint: Option<f64>,
    #[schemars(range(min = 0.0, max = 100.0))]
    pub opacity: f64,
    pub colour: StudioColor,
}

/// STU-VEC-135 `gradient.linear` / `gradient.radial` geometry, in gradient space. A radial gradient
/// is the circle `(cx, cy, r)` with focal circle `(fx, fy, fr)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "gradient_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioGradientGeometry {
    Linear {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    },
    Radial {
        cx: f64,
        cy: f64,
        r: f64,
        fx: f64,
        fy: f64,
        fr: f64,
    },
}

/// Vector gradient: geometry, one affine, a spread, the STU-VEC-137 controls and an ordered stop
/// list (at least two stops, positions non-decreasing).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioGradient {
    pub geometry: StudioGradientGeometry,
    /// `[a, b, c, d, e, f]`: point = `[a c e; b d f] * [x y 1]`; finite and invertible.
    pub transform: [f64; 6],
    pub spread: GradientSpread,
    pub interpolation: GradientInterpolation,
    pub dither: bool,
    pub stops: Vec<StudioGradientStop>,
}

/// A paint (STU-VEC-134): the `solid` colour or a `gradient`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "paint_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioPaint {
    Solid { colour: StudioColor },
    Gradient { gradient: StudioGradient },
}

/// STU-VEC-143 `stroke_cap`; captured wire values 1..3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StrokeCap {
    Butt,
    Round,
    Projecting,
}
impl StrokeCap {
    pub const fn wire(self) -> u8 {
        match self {
            Self::Butt => 1,
            Self::Round => 2,
            Self::Projecting => 3,
        }
    }
    pub const fn from_wire(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Butt),
            2 => Some(Self::Round),
            3 => Some(Self::Projecting),
            _ => None,
        }
    }
}

/// STU-VEC-143 `stroke_join`; captured wire values 1..3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StrokeJoin {
    Miter,
    Round,
    Bevel,
}
impl StrokeJoin {
    pub const fn wire(self) -> u8 {
        match self {
            Self::Miter => 1,
            Self::Round => 2,
            Self::Bevel => 3,
        }
    }
    pub const fn from_wire(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Miter),
            2 => Some(Self::Round),
            3 => Some(Self::Bevel),
            _ => None,
        }
    }
}

/// STU-VEC-143 `stroke_align`; the captured tokens are uppercase and carry no integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StrokeAlign {
    Center,
    Inside,
    Outside,
}

/// Stroke geometry (STU-VEC-143, STU-VEC-144, STU-VEC-145).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioStrokeStyle {
    /// Uniform weight, non-negative.
    pub weight: Length,
    pub cap: StrokeCap,
    pub join: StrokeJoin,
    pub align: StrokeAlign,
    /// Ratio, `1..=500` (STU-VEC-144).
    #[schemars(range(min = 1.0, max = 500.0))]
    pub miter_limit: f64,
    /// Alternating dash and gap lengths, non-negative and an even count; empty is a solid line.
    pub dash: Vec<Length>,
    /// Dash phase (not in STU-VEC-145; see the module note).
    pub dash_offset: Length,
}

/// One row of the appearance stack (STU-VEC-022), bottom to top in paint order. `opacity` is percent;
/// `blend_mode` is the `StudioBlendMode` discriminant (`2..=35` without the tool-only and the
/// group-only members).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "row_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioAppearanceRow {
    Fill {
        paint: StudioPaint,
        #[schemars(range(min = 0.0, max = 100.0))]
        opacity: f64,
        #[schemars(range(min = 2, max = 35))]
        blend_mode: u16,
        visible: bool,
    },
    Stroke {
        paint: StudioPaint,
        stroke: StudioStrokeStyle,
        #[schemars(range(min = 0.0, max = 100.0))]
        opacity: f64,
        #[schemars(range(min = 2, max = 35))]
        blend_mode: u16,
        visible: bool,
    },
}

/// STU-DS-136 closed set of the fields of a node that hold a style reference.
pub const STYLE_REF_FIELDS: [&str; 6] = [
    "fill_style_id",
    "stroke_style_id",
    "text_style_id",
    "effect_style_id",
    "grid_style_id",
    "background_style_id",
];

/// STU-DS-134 style kind. Only `PAINT` entries are modelled by [`StudioPaintStyle`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StyleKind {
    Paint,
    Text,
    Effect,
    Grid,
}

/// A `PAINT` entry of the style registry (STU-DS-135, STU-DS-034b): the saved appearance stack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPaintStyle {
    pub schema_id: StyleRegistrySchema,
    /// `SSTY-{uuid_v7}`.
    pub style_id: String,
    pub kind: StyleKind,
    /// Slash-path name.
    pub name: String,
    pub publishing: StudioPublishing,
    /// Bottom to top.
    pub rows: Vec<StudioAppearanceRow>,
}

fn length_ok(l: &Length, non_negative: bool) -> bool {
    l.value.is_finite() && (!non_negative || l.value >= 0.0)
}

fn gradient(g: &StudioGradient, target: &str, c: &mut Ctx) -> Result<()> {
    let bad = |what: &str| err(Code::InvalidField, format!("{target}/{what}"));
    let finite = match &g.geometry {
        StudioGradientGeometry::Linear { x1, y1, x2, y2 } => {
            [x1, y1, x2, y2].into_iter().all(|v| v.is_finite())
        }
        StudioGradientGeometry::Radial {
            cx,
            cy,
            r,
            fx,
            fy,
            fr,
        } => {
            [cx, cy, fx, fy].into_iter().all(|v| v.is_finite())
                && [r, fr].into_iter().all(|v| v.is_finite() && *v >= 0.0)
        }
    };
    if !finite {
        return Err(bad("geometry"));
    }
    let [a, b, cc, d, e, f] = g.transform;
    if ![a, b, cc, d, e, f].into_iter().all(f64::is_finite) || a * d - b * cc == 0.0 {
        return Err(bad("transform"));
    }
    if !(2..=MAX_GRADIENT_STOPS).contains(&g.stops.len()) {
        return Err(bad("stops"));
    }
    c.nodes(g.stops.len(), target)?;
    let mut previous = 0.0f64;
    for (n, s) in g.stops.iter().enumerate() {
        c.tick()?;
        let stop = format!("{target}/stops/{n}");
        if !percent(s.position) || s.position < previous {
            return Err(err(Code::InvalidField, format!("{stop}/position")));
        }
        previous = s.position;
        if !percent(s.opacity) {
            return Err(err(Code::InvalidAppearance, format!("{stop}/opacity")));
        }
        if s.midpoint
            .is_some_and(|m| !m.is_finite() || !(13.0..=87.0).contains(&m))
        {
            return Err(err(Code::InvalidField, format!("{stop}/midpoint")));
        }
        s.colour.validate(&stop, c)?;
    }
    Ok(())
}

fn paint(p: &StudioPaint, target: &str, c: &mut Ctx) -> Result<()> {
    match p {
        StudioPaint::Solid { colour } => colour.validate(target, c),
        StudioPaint::Gradient { gradient: g } => gradient(g, target, c),
    }
}

fn stroke(s: &StudioStrokeStyle, target: &str, c: &mut Ctx) -> Result<()> {
    let bad = |what: &str| err(Code::InvalidField, format!("{target}/{what}"));
    if !length_ok(&s.weight, true) {
        return Err(bad("weight"));
    }
    if !s.miter_limit.is_finite() || !(1.0..=500.0).contains(&s.miter_limit) {
        return Err(bad("miter_limit"));
    }
    if s.dash.len() > MAX_DASH_ENTRIES
        || !s.dash.len().is_multiple_of(2)
        || !s.dash.iter().all(|l| length_ok(l, true))
    {
        return Err(bad("dash"));
    }
    if !length_ok(&s.dash_offset, false) {
        return Err(bad("dash_offset"));
    }
    c.nodes(s.dash.len() / 2, target)
}

fn row(r: &StudioAppearanceRow, target: &str, c: &mut Ctx) -> Result<()> {
    let (p, stroke_style, opacity, blend_mode) = match r {
        StudioAppearanceRow::Fill {
            paint,
            opacity,
            blend_mode,
            ..
        } => (paint, None, *opacity, *blend_mode),
        StudioAppearanceRow::Stroke {
            paint,
            stroke,
            opacity,
            blend_mode,
            ..
        } => (paint, Some(stroke), *opacity, *blend_mode),
    };
    if !percent(opacity) {
        return Err(err(Code::InvalidAppearance, format!("{target}/opacity")));
    }
    if !(1..=BLEND_LAST).contains(&blend_mode) {
        return Err(err(Code::UnknownBlendMode, format!("{target}/blend_mode")));
    }
    if BLEND_TOOL_ONLY.contains(&blend_mode) || blend_mode == BLEND_PASS_THROUGH {
        return Err(err(
            Code::InapplicableBlendMode,
            format!("{target}/blend_mode"),
        ));
    }
    paint(p, target, c)?;
    stroke_style.map_or(Ok(()), |s| stroke(s, target, c))
}

pub(crate) fn validate_style(s: &StudioPaintStyle, c: &mut Ctx) -> Result<()> {
    c.tick()?;
    let sid = s.style_id.as_str();
    id(sid, "SSTY", "style_id")?;
    if s.kind != StyleKind::Paint {
        return Err(err(Code::PayloadMismatch, format!("{sid}/kind")));
    }
    if !slash_path(&s.name) {
        return Err(err(Code::InvalidField, format!("{sid}/name")));
    }
    c.text(&s.name, sid)?;
    if s.rows.len() > MAX_APPEARANCE_ROWS {
        return Err(err(Code::BudgetExceeded, format!("{sid}/rows")));
    }
    c.nodes(1 + s.rows.len(), sid)?;
    s.publishing.validate(sid, c)?;
    for (n, r) in s.rows.iter().enumerate() {
        row(r, &format!("{sid}/rows/{n}"), c)?;
    }
    Ok(())
}
