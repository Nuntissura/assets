//! Swatches, brushes and envelopes of the vector record set (lane SMB-ILLUSTRATOR-TOOLS-2;
//! STU-COL-170..174, STU-VEC-019, STU-VEC-051, STU-VEC-026/027, STU-VEC-150, STU-VEC-152,
//! STU-VEC-162/163, STU-VEC-055, STU-VEC-168/169). Every member is optional in
//! `StudioVectorRecords` and skipped when empty, so record sets written before them are
//! byte-identical. Folio stores and validates only: colour conversion is Prism's (callers), brush
//! and envelope geometry is Nib's, painting is composite's.
//!
//! * `StudioSwatch` (`hsk.studio.swatch@1`, `SSWT` ids): kinds `process`, `global`, `spot`,
//!   `tint`, `gradient`. A colour swatch carries `color_model` (`RGB` | `CMYK` | `LAB` | `GRAY`)
//!   and `components` (RGB/GRAY/CMYK in 0..1, CMYK 1 = full ink as in ICC; LAB L* 0..100,
//!   a*/b* -128..128) and an optional `profile_ref` (`SCPF`; absent = the document working space
//!   of that model). A spot swatch carries its ink (`ink_name`, an optional measured Lab
//!   definition, an optional alternate space and value; neither = no process approximation). A
//!   tint names a base colour swatch and `tint_value` percent. A gradient swatch holds a
//!   `StudioGradient`.
//! * `StudioSwatchPaint` binds row `row` of PAINT style `style_id` to a swatch (STU-COL-172 global
//!   propagation), with an optional per-use `tint` percent; the row's own paint is the resolved
//!   stand-in the caller keeps current (like the pattern binding). One binding per row, and a row
//!   bound to a pattern or an image takes no swatch.
//! * `StudioVectorBrush` (set-local key id): one of the five STU-VEC-162 kinds with its STU-VEC-152
//!   parameters (hard ranges enforced where captured; `StudioBrushParam` keeps a second value under
//!   any non-fixed dynamics, STU-VEC-150), a STU-VEC-163 colorization and its art: `SVPT` paths
//!   with a colour (art and scatter), or five NAMED pattern tile slots. `StudioBrushStroke` binds a
//!   stroke row to a brush, with an optional per-path `scale_override` percent (STU-VEC-027).
//! * `StudioEnvelope` wraps a vector layer: a STU-VEC-168 warp (bend and distortions stored as
//!   ratios), a mesh grid (`(rows + 1) * (columns + 1)` points in document units, rows and
//!   columns 1..=64) or a top object (another vector layer that has no envelope of its own), with
//!   the STU-VEC-169 options.
//!
//! Declared gaps: swatch kinds `mixed_ink` and `mixed_ink_group`, swatch groups as records, the
//! reserved swatches and palette scopes (STU-COL-175..178); swatch references from gradient stops;
//! the captured warp integer mapping; mesh-reset `maintain_envelope_shape`; brush image/custom kinds.
use super::*;
use crate::{StudioColor, StudioGradient};

schema_token!(SwatchSchema, "hsk.studio.swatch@1");

/// Most entries of each member.
pub const TOOLS_MAX_ENTRIES: usize = 4096;
/// Most art paths of one brush (all pattern slots together).
pub const BRUSH_MAX_ART: usize = 256;
/// Most mesh rows or columns of an envelope.
pub const ENVELOPE_MAX_MESH: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioSwatchKind {
    Process,
    Global,
    Spot,
    Tint,
    Gradient,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum StudioColorModel {
    Rgb,
    Cmyk,
    Lab,
    Gray,
}

impl StudioColorModel {
    /// Component count.
    pub const fn channels(self) -> usize {
        match self {
            Self::Rgb | Self::Lab => 3,
            Self::Cmyk => 4,
            Self::Gray => 1,
        }
    }
    /// True when `c` holds this model's components in range.
    pub fn accepts(self, c: &[f64]) -> bool {
        c.len() == self.channels()
            && c.iter().enumerate().all(|(i, v)| {
                v.is_finite()
                    && match self {
                        Self::Lab if i == 0 => (0.0..=100.0).contains(v),
                        Self::Lab => (-128.0..=128.0).contains(v),
                        _ => (0.0..=1.0).contains(v),
                    }
            })
    }
}

/// A spot ink (STU-COL-173).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSpotInk {
    pub ink_name: String,
    /// Measured definition (Lab D50), used for screen and proof simulation when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_lab: Option<[f64; 3]>,
    /// Process approximation (absent = `no_alternate`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternate_space: Option<StudioColorModel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternate_color_value: Vec<f64>,
}

/// `StudioSwatch` (STU-COL-170).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSwatch {
    pub schema_id: SwatchSchema,
    pub swatch_id: String,
    pub name: String,
    pub swatch_kind: StudioSwatchKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_model: Option<StudioColorModel>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spot: Option<StudioSpotInk>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_swatch_ref: Option<String>,
    /// Percent of the base (tint swatches).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint_value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gradient: Option<StudioGradient>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_ref: Option<String>,
}

/// A PAINT-style row bound to a swatch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSwatchPaint {
    pub style_id: String,
    pub row: u32,
    pub swatch_id: String,
    /// Per-use tint percent (STU-COL-172: kept across swatch edits).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tint: Option<f64>,
}

/// STU-VEC-150 dynamics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioBrushDynamics {
    #[default]
    Fixed,
    WidthProfile,
    Random,
    Pressure,
    StylusWheel,
    Tilt,
    Bearing,
    Rotation,
}

fn is_fixed(d: &StudioBrushDynamics) -> bool {
    *d == StudioBrushDynamics::Fixed
}

/// A brush parameter: `value` (the minimum under non-fixed dynamics) and `max`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioBrushParam {
    pub value: f64,
    #[serde(default, skip_serializing_if = "is_fixed")]
    pub dynamics: StudioBrushDynamics,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

impl StudioBrushParam {
    pub const fn fixed(value: f64) -> Self {
        Self { value, dynamics: StudioBrushDynamics::Fixed, max: None }
    }
    fn within(&self, lo: f64, hi: f64) -> bool {
        let fixed_ok = self.dynamics == StudioBrushDynamics::Fixed || self.max.is_some();
        fixed_ok && std::iter::once(self.value).chain(self.max).all(|v| v.is_finite() && (lo..=hi).contains(&v))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioRotationRelative {
    Page,
    Path,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioArtScale {
    Proportional,
    StretchToStrokeLength,
    StretchBetweenGuides,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioPatternFit {
    StretchToFit,
    AddSpaceToFit,
    ApproximatePath,
}

/// STU-VEC-163 colorization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioBrushColorization {
    None,
    Tints,
    TintsAndShades,
    HueShift,
}

/// One art path of a brush and its colour.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioBrushArt {
    pub path_id: String,
    pub colour: StudioColor,
}

fn no_art(v: &[StudioBrushArt]) -> bool {
    v.is_empty()
}

/// The five named pattern tile slots (STU-VEC-152).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPatternTiles {
    #[serde(default, skip_serializing_if = "no_art")]
    pub side: Vec<StudioBrushArt>,
    #[serde(default, skip_serializing_if = "no_art")]
    pub outer_corner: Vec<StudioBrushArt>,
    #[serde(default, skip_serializing_if = "no_art")]
    pub inner_corner: Vec<StudioBrushArt>,
    #[serde(default, skip_serializing_if = "no_art")]
    pub start: Vec<StudioBrushArt>,
    #[serde(default, skip_serializing_if = "no_art")]
    pub end: Vec<StudioBrushArt>,
}

impl StudioPatternTiles {
    pub fn all(&self) -> impl Iterator<Item = &StudioBrushArt> {
        self.side.iter().chain(&self.outer_corner).chain(&self.inner_corner).chain(&self.start).chain(&self.end)
    }
}

/// STU-VEC-162 brush kinds with their STU-VEC-152 parameters.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "brush_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioBrushKind {
    Calligraphic { nib_angle: StudioBrushParam, nib_roundness: StudioBrushParam, nib_size: StudioBrushParam },
    Scatter {
        scatter_size: StudioBrushParam,
        scatter_spacing: StudioBrushParam,
        scatter_offset: StudioBrushParam,
        scatter_rotation: StudioBrushParam,
        scatter_rotation_relative_to: StudioRotationRelative,
        #[serde(default)]
        seed: u64,
        art: Vec<StudioBrushArt>,
    },
    Art {
        art_width: StudioBrushParam,
        art_scale_mode: StudioArtScale,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        art_guide_start: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        art_guide_end: Option<f64>,
        flip_along: bool,
        flip_across: bool,
        art: Vec<StudioBrushArt>,
    },
    Pattern {
        pattern_brush_scale: StudioBrushParam,
        fit_mode: StudioPatternFit,
        flip_along: bool,
        flip_across: bool,
        show_auto_generated_corner_tiles: bool,
        tiles: StudioPatternTiles,
    },
    Bristle { bristle_size: f64, bristle_length: f64, bristle_density: f64 },
}

/// `StudioVectorBrush` (STU-VEC-026).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioVectorBrush {
    pub brush_id: String,
    pub name: String,
    pub kind: StudioBrushKind,
    pub colorization: StudioBrushColorization,
}

impl StudioVectorBrush {
    /// Every art path of the brush.
    pub fn art(&self) -> Vec<&StudioBrushArt> {
        match &self.kind {
            StudioBrushKind::Scatter { art, .. } | StudioBrushKind::Art { art, .. } => art.iter().collect(),
            StudioBrushKind::Pattern { tiles, .. } => tiles.all().collect(),
            _ => Vec::new(),
        }
    }
}

/// A stroke row painted with a brush.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioBrushStroke {
    pub style_id: String,
    pub row: u32,
    pub brush_id: String,
    /// Per-path scale override percent (STU-VEC-027), applied to the brush's width, size or scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale_override: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioWarpStyle {
    Arc,
    ArcLower,
    ArcUpper,
    Arch,
    Bulge,
    ShellLower,
    ShellUpper,
    Flag,
    Wave,
    Fish,
    Rise,
    Fisheye,
    Inflate,
    Squeeze,
    Twist,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioWarpAxis {
    Horizontal,
    Vertical,
}

/// What wraps the layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "envelope_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioEnvelopeShape {
    /// Ratios -1..=1 (the authored percent / 100, STU-VEC-168).
    Warp { warp_style: StudioWarpStyle, warp_axis: StudioWarpAxis, warp_bend: f64, warp_distortion_horizontal: f64, warp_distortion_vertical: f64 },
    Mesh { rows: u32, columns: u32, points: Vec<[f64; 2]> },
    TopObject { source_layer_id: String },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioEnvelopeRaster {
    #[default]
    ClippingMask,
    Transparency,
}

fn is_false(v: &bool) -> bool {
    !*v
}
fn is_clipping(v: &StudioEnvelopeRaster) -> bool {
    *v == StudioEnvelopeRaster::ClippingMask
}

/// `StudioEnvelope` (STU-VEC-169 options).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEnvelope {
    pub layer_id: String,
    pub envelope: StudioEnvelopeShape,
    /// Percent 0..=100.
    pub envelope_fidelity: f64,
    #[serde(default, skip_serializing_if = "is_clipping")]
    pub envelope_raster_mode: StudioEnvelopeRaster,
    #[serde(default, skip_serializing_if = "is_false")]
    pub anti_alias: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub distort_appearance: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub distort_linear_gradient_fills: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub distort_pattern_fills: bool,
}

fn text_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control)
}

fn validate_swatch(s: &StudioSwatch, t: &str) -> Result<()> {
    check(text_ok(&s.name), Code::InvalidField, t, "name")?;
    if let Some(p) = &s.profile_ref {
        id(p, "SCPF", &format!("{t}/profile_ref"))?;
    }
    if let Some(g) = &s.group_ref {
        check(key(g), Code::InvalidField, t, "group_ref")?;
    }
    let colour = s.color_model.is_some_and(|m| m.accepts(&s.components));
    let none = s.color_model.is_none() && s.components.is_empty() && s.profile_ref.is_none();
    let (spot, tint, grad) = (s.spot.is_some(), s.base_swatch_ref.is_some() || s.tint_value.is_some(), s.gradient.is_some());
    let shape_ok = match s.swatch_kind {
        StudioSwatchKind::Process | StudioSwatchKind::Global => colour && !spot && !tint && !grad,
        StudioSwatchKind::Spot => none && spot && !tint && !grad,
        StudioSwatchKind::Tint => none && !spot && !grad && s.base_swatch_ref.is_some() && s.tint_value.is_some_and(|v| v.is_finite() && (0.0..=100.0).contains(&v)),
        StudioSwatchKind::Gradient => none && !spot && !tint && grad,
    };
    check(shape_ok, Code::InvalidField, t, "swatch_kind")?;
    if let Some(ink) = &s.spot {
        check(text_ok(&ink.ink_name), Code::InvalidField, t, "spot/ink_name")?;
        let lab_ok = ink.measured_lab.is_none_or(|l| StudioColorModel::Lab.accepts(&l));
        check(lab_ok, Code::InvalidField, t, "spot/measured_lab")?;
        let alt_ok = match ink.alternate_space {
            Some(m) => m.accepts(&ink.alternate_color_value),
            None => ink.alternate_color_value.is_empty(),
        };
        check(alt_ok, Code::InvalidField, t, "spot/alternate_color_value")?;
    }
    if let Some(g) = &s.gradient {
        // Validated through a one-row PAINT style (Folio owns the gradient rules).
        let probe = StudioPaintStyle {
            schema_id: crate::StyleRegistrySchema::V1,
            style_id: "SSTY-00000000-0000-7000-8000-000000000000".into(),
            kind: crate::StyleKind::Paint,
            name: "swatch".into(),
            publishing: crate::StudioPublishing { description: String::new(), description_markdown: String::new(), documentation_links: Vec::new(), publish_key: "swatch".into(), is_remote: false },
            rows: vec![crate::StudioAppearanceRow::Fill { paint: crate::StudioPaint::Gradient { gradient: g.clone() }, opacity: 100.0, blend_mode: 2, visible: true }],
        };
        crate::validate_paint_style(&probe, Budget::default(), &CancellationToken::default(), &Unresolved).map_err(|_| err(Code::InvalidField, format!("{t}/gradient")))?;
    }
    Ok(())
}

fn validate_art(art: &[StudioBrushArt], t: &str) -> Result<()> {
    for a in art {
        id(&a.path_id, "SVPT", &format!("{t}/art"))?;
        let c = &a.colour;
        check([c.r, c.g, c.b].iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)), Code::InvalidField, t, "art/colour")?;
    }
    Ok(())
}

fn validate_brush(b: &StudioVectorBrush, t: &str) -> Result<()> {
    set_id(&b.brush_id, t, "brush_id")?;
    check(text_ok(&b.name), Code::InvalidField, t, "name")?;
    let art = b.art();
    check(art.len() <= BRUSH_MAX_ART, Code::BudgetExceeded, t, "art")?;
    let open = |p: &StudioBrushParam| p.within(f64::MIN, f64::MAX);
    match &b.kind {
        StudioBrushKind::Calligraphic { nib_angle, nib_roundness, nib_size } => {
            check(open(nib_angle) && open(nib_roundness) && open(nib_size), Code::InvalidField, t, "kind")?;
        }
        StudioBrushKind::Scatter { scatter_size, scatter_spacing, scatter_offset, scatter_rotation, art, .. } => {
            let ok = scatter_size.within(1.0, 10_000.0) && scatter_spacing.within(1.0, 10_000.0) && scatter_offset.within(-10_000.0, 10_000.0) && scatter_rotation.within(-360.0, 360.0);
            check(ok, Code::InvalidField, t, "kind")?;
            check(!art.is_empty(), Code::InvalidField, t, "art")?;
            validate_art(art, t)?;
        }
        StudioBrushKind::Art { art_width, art_scale_mode, art_guide_start, art_guide_end, art, .. } => {
            check(art_width.within(f64::MIN_POSITIVE, f64::MAX), Code::InvalidField, t, "art_width")?;
            let guides = match art_scale_mode {
                StudioArtScale::StretchBetweenGuides => matches!((art_guide_start, art_guide_end), (Some(a), Some(z)) if a.is_finite() && z.is_finite() && a < z),
                _ => art_guide_start.is_none() && art_guide_end.is_none(),
            };
            check(guides, Code::InvalidField, t, "art_guide")?;
            check(!art.is_empty(), Code::InvalidField, t, "art")?;
            validate_art(art, t)?;
        }
        StudioBrushKind::Pattern { pattern_brush_scale, tiles, .. } => {
            check(pattern_brush_scale.within(f64::MIN_POSITIVE, f64::MAX), Code::InvalidField, t, "pattern_brush_scale")?;
            check(!tiles.side.is_empty(), Code::InvalidField, t, "tiles/side")?;
            for slot in [&tiles.side, &tiles.outer_corner, &tiles.inner_corner, &tiles.start, &tiles.end] {
                validate_art(slot, t)?;
            }
        }
        StudioBrushKind::Bristle { bristle_size, bristle_length, bristle_density } => {
            check([bristle_size, bristle_length, bristle_density].iter().all(|v| v.is_finite() && **v > 0.0), Code::InvalidField, t, "kind")?;
        }
    }
    Ok(())
}

/// The members of this module inside `validate_vector_records`.
pub(super) fn validate_tools(
    r: &StudioVectorRecords,
    layers: &BTreeMap<&str, &StudioLayer>,
    design: Option<&StudioDesignRecords>,
    cx: &mut Ctx,
) -> Result<()> {
    for (n, what) in [(r.swatches.len(), "swatches"), (r.swatch_paints.len(), "swatch_paints"), (r.brushes.len(), "brushes"), (r.brush_strokes.len(), "brush_strokes"), (r.envelopes.len(), "envelopes")] {
        check(n <= TOOLS_MAX_ENTRIES, Code::BudgetExceeded, what, "count")?;
        cx.nodes(n, what)?;
    }
    let mut swatches: BTreeMap<&str, &StudioSwatch> = BTreeMap::new();
    for s in &r.swatches {
        cx.tick()?;
        let t = s.swatch_id.as_str();
        id(t, "SSWT", "swatches/swatch_id")?;
        if swatches.insert(t, s).is_some() {
            return Err(err(Code::DuplicateIdentity, t));
        }
        validate_swatch(s, t)?;
    }
    for s in &r.swatches {
        if let Some(base) = &s.base_swatch_ref {
            let b = swatches.get(base.as_str()).ok_or_else(|| err(Code::DanglingReference, format!("{}/base_swatch_ref", s.swatch_id)))?;
            // A tint tracks a colour swatch (no chains, no gradients).
            check(matches!(b.swatch_kind, StudioSwatchKind::Process | StudioSwatchKind::Global | StudioSwatchKind::Spot), Code::InvalidField, &s.swatch_id, "base_swatch_ref")?;
        }
    }
    let styles: BTreeMap<&str, &StudioPaintStyle> = design.map(|x| x.paint_styles.iter().map(|s| (s.style_id.as_str(), s)).collect()).unwrap_or_default();
    let row_of = |style_id: &str, row: u32, t: &str| -> Result<&crate::StudioAppearanceRow> {
        id(style_id, "SSTY", t)?;
        let style = styles.get(style_id).ok_or_else(|| err(Code::DanglingReference, t))?;
        usize::try_from(row).ok().and_then(|i| style.rows.get(i)).ok_or_else(|| err(Code::DanglingReference, format!("{t}/row")))
    };
    let taken: BTreeSet<(&str, u32)> = r.pattern_paints.iter().map(|p| (p.style_id.as_str(), p.row)).chain(r.image_paints.iter().map(|p| (p.style_id.as_str(), p.row))).collect();
    let mut bound: BTreeSet<(&str, u32)> = BTreeSet::new();
    for p in &r.swatch_paints {
        cx.tick()?;
        let t = format!("{}/rows/{}", p.style_id, p.row);
        row_of(&p.style_id, p.row, &t)?;
        check(!taken.contains(&(p.style_id.as_str(), p.row)) && bound.insert((p.style_id.as_str(), p.row)), Code::DuplicateIdentity, &t, "swatch_id")?;
        let sw = swatches.get(p.swatch_id.as_str()).ok_or_else(|| err(Code::DanglingReference, format!("{t}/swatch_id")))?;
        check(p.tint.is_none_or(|v| v.is_finite() && (0.0..=100.0).contains(&v)), Code::InvalidField, &t, "tint")?;
        check(p.tint.is_none() || sw.swatch_kind != StudioSwatchKind::Gradient, Code::InvalidField, &t, "tint")?;
    }
    let mut brushes: BTreeMap<&str, &StudioVectorBrush> = BTreeMap::new();
    for b in &r.brushes {
        cx.tick()?;
        validate_brush(b, &b.brush_id)?;
        if brushes.insert(b.brush_id.as_str(), b).is_some() {
            return Err(err(Code::DuplicateIdentity, &b.brush_id));
        }
    }
    let mut stroked: BTreeSet<(&str, u32)> = BTreeSet::new();
    for s in &r.brush_strokes {
        cx.tick()?;
        let t = format!("{}/rows/{}", s.style_id, s.row);
        let row = row_of(&s.style_id, s.row, &t)?;
        check(matches!(row, crate::StudioAppearanceRow::Stroke { .. }), Code::InvalidField, &t, "row")?;
        check(stroked.insert((s.style_id.as_str(), s.row)), Code::DuplicateIdentity, &t, "brush_id")?;
        set_id(&s.brush_id, &t, "brush_id")?;
        check(brushes.contains_key(s.brush_id.as_str()), Code::DanglingReference, &t, "brush_id")?;
        check(s.scale_override.is_none_or(|v| v.is_finite() && v > 0.0 && v <= 100_000.0), Code::InvalidField, &t, "scale_override")?;
    }
    let mut wrapped: BTreeSet<&str> = BTreeSet::new();
    for e in &r.envelopes {
        cx.tick()?;
        let t = e.layer_id.as_str();
        let l = layer(layers, t, "envelopes/layer_id")?;
        check(matches!(l.payload, Payload::Vector { .. }), Code::PayloadMismatch, t, "layer_id")?;
        check(wrapped.insert(t), Code::DuplicateIdentity, t, "layer_id")?;
        check(e.envelope_fidelity.is_finite() && (0.0..=100.0).contains(&e.envelope_fidelity), Code::InvalidField, t, "envelope_fidelity")?;
        match &e.envelope {
            StudioEnvelopeShape::Warp { warp_bend, warp_distortion_horizontal, warp_distortion_vertical, .. } => {
                let ok = [warp_bend, warp_distortion_horizontal, warp_distortion_vertical].iter().all(|v| v.is_finite() && (-1.0..=1.0).contains(*v));
                check(ok, Code::InvalidField, t, "envelope")?;
            }
            StudioEnvelopeShape::Mesh { rows, columns, points } => {
                let dims = (1..=ENVELOPE_MAX_MESH).contains(rows) && (1..=ENVELOPE_MAX_MESH).contains(columns);
                check(dims, Code::InvalidField, t, "envelope/rows")?;
                let want = usize::try_from(rows.saturating_add(1).saturating_mul(columns.saturating_add(1))).unwrap_or(usize::MAX);
                check(points.len() == want && points.iter().all(|p| p.iter().all(|v| v.is_finite())), Code::InvalidField, t, "envelope/points")?;
            }
            StudioEnvelopeShape::TopObject { source_layer_id } => {
                let s = layer(layers, source_layer_id, &format!("{t}/source_layer_id"))?;
                check(matches!(s.payload, Payload::Vector { .. }) && source_layer_id.as_str() != t, Code::PayloadMismatch, t, "source_layer_id")?;
            }
        }
    }
    // A top object is not itself wrapped (no envelope chains or cycles).
    for e in &r.envelopes {
        if let StudioEnvelopeShape::TopObject { source_layer_id } = &e.envelope {
            check(!wrapped.contains(source_layer_id.as_str()), Code::InvalidField, &e.layer_id, "source_layer_id")?;
        }
    }
    Ok(())
}
