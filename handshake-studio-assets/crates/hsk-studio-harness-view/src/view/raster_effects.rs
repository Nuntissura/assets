//! Smart filter effects and adjustment layers as typed commands (lane SMB-PS-FILTER-ADJUST-COMMANDS-1; [STU-FX-007],
//! [STU-FX-012c], [STU-FX-104], [STU-FX-105], [STU-FX-123], [STU-RAS-035], [STU-RAS-037], [STU-RAS-103]..[STU-RAS-112]).
//! egui-free and drawing nothing: the rows `effect.*` and `adjustment.*` of `action::commands::photo` reach
//! [`Viewer::effect_command`] and [`Viewer::adjustment_command`] through `Viewer::run_command`.
//!
//! # Manual (no-context operator or model)
//!
//! A raster layer's smart filter stack is the Folio `StudioSmartFilterStack` of its raster record in the session
//! entry `raster_records` (module `place`); composite renders it live (pigment `SmartFilterStack`), the layer's
//! pixels stay as they are, and save, reopen and export carry it. Every mutating command is ONE undo step named by
//! its command id and attributed to the viewer's run context (the caller's); a refusal is a typed code and changes
//! nothing.
//!
//! Steps (review F5, lead ruling 2026-11-13, option b): `effect.apply`, `update`, `reorder`, `toggle`, `remove` and
//! `adjustment.edit` are ENTRY-ONLY steps (`Run::set_tiles_and_entry` without tiles): the session's history step, not a
//! document revision. They pass no chronicle provider and no CommitPort, the document revision stays as it was
//! (test `effect_and_adjustment_edits_are_entry_only_steps_and_leave_the_revision`), and a package preview rendered
//! for that revision is not marked stale by them (no preview-staleness mechanism is reachable from the viewer:
//! NOT_PROVEN, next-lane candidate "revisioned raster-records step"). `adjustment.create` (layer and entry, one
//! chronicle step) and `adjustment.apply` (tiles, one chronicle step) are revisions.
//!
//! - `effect.parameters {filter_kind?}` (no document needed): the parameter specs of every kind, or one, without
//!   running anything ([STU-RAS-110]), as [STU-RAS-103] records (`hsk.studio.parameter_spec@1`; the raster rows
//!   govern this lane's filters, lead ruling Q2: no [STU-FX-103] `hsk.studio.effect_parameter@1` record is emitted).
//!   A numeric parameter carries as separate values `hard_min`, `hard_max` (what the engine accepts; outside is a
//!   refusal, never a clamp), `soft_min`, `soft_max` (what a control presents; a value beyond them inside the hard
//!   range is legal, [STU-FX-105]), `default`, `unit` (a [STU-RAS-105] token; `table_unit` keeps a source's own
//!   word where the engine reads it otherwise), `precision`, `step`, `coarse_step`, `fine_step` (the [STU-RAS-108]
//!   derivation), `observed_min`, `observed_max` (evidence only, never a bound, [STU-RAS-107]; `absent` for every
//!   kind here) and `bound_classification` (`declared`, `observed`, `derived`, `unknown` per value, [STU-RAS-112]).
//!   A value no source declared is the token `unknown` ([STU-RAS-106]): nothing is invented, clamped or narrowed to
//!   an observation. An enumeration lists every member with its integer discriminant ([STU-RAS-109]: the 14.9.7
//!   tables' where they declare one, else Studio's own contiguous numbering from 1, classified `derived`). Each kind
//!   also carries its `input_schema` ([STU-RAS-110]): JSON Schema with the known hard bounds as `minimum` /
//!   `maximum` / `exclusiveMinimum` (none fabricated for an `unknown` bound) and the soft bounds, default, unit,
//!   precision, steps and classifications as `x-` annotations; `effect_apply_input_schema` is the whole row's
//!   (`oneOf` by `filter_kind`). The result carries `findings` of the `STUDIO_EFFECT_PARAM_CONTRACT` check
//!   ([STU-FX-123], [STU-RAS-111]; [`parameter_contract_findings`]) over every spec it lists: empty for the
//!   shipped table. `declared` values come from the 14.9.7 tables the kernels cite (Fast Box Blur, Directional Blur,
//!   Radial Blur, Solarize, Mosaic, Minimax, Dust & Scratches); `derived` hard bounds are the engine's own (pigment
//!   `filter_math` Studio readings, a type's domain, a mathematical domain such as `> 0`, `hard_min_exclusive`).
//! - `effect.apply {layer, filter_kind, params?, opacity?, enabled?, at?}` (`STUDIO_EFFECT_APPLY`): adds an entry
//!   to the stack of raster layer `layer` (the stack is created when absent) at render position `at` (default:
//!   last, applied last). Each parameter is checked against its spec on the value the record stores (the 32-bit
//!   float for a float field, review F8): an unknown key is `parameter_unknown`, a wrong type `parameter_invalid`,
//!   a value outside the hard bounds `effect_param_out_of_range` naming the parameter, the value and both hard
//!   bounds ([STU-FX-104]); an omitted parameter takes its declared default (the radial blur centre: the centre of
//!   the layer's content bounds), and one whose default is `unknown` is `parameter_missing`. `opacity` is percent
//!   `0..=100` (default 100; outside, `effect_param_out_of_range` with `param: opacity`). The entry then passes
//!   pigment's check (`SmartFilterOp::check`) and the render's own admission over the layer (review F1): the stack
//!   renders only unit-range content (pigment `RgbaPlane::validate`, the stack's first check), else
//!   `effect_value_range_unsupported`; and the entry must fit pigment's budget over the layer's content bounds
//!   inside the canvas (`FilterKernel::admit`, `filter_math::filter_window_cost` against `StackLimits::default()`,
//!   so a box blur above `MAX_BOX_KERNEL_TAPS` taps refuses), else `effect_render_budget_exceeded {needed, limit}`;
//!   a liquify mesh must cover exactly those bounds, and its vertex lists hold at most Folio's node budget of the
//!   raster records (4096; `budget_exceeded` before decoding). A layer whose tiles the viewer does not decode
//!   (not `rgb_f32le`) is budget-checked over its bounds only. Returns `{layer_id, instance_id, filter_kind,
//!   index, defaulted, revision}`.
//! - `effect.update {layer, instance, params?, opacity?}` (`STUDIO_EFFECT_UPDATE`): the given parameters over the
//!   entry's current ones, checked and admitted as above; the kind stays. An opacity-only update and a liquify
//!   update without a new `mesh` do not decode the mesh again (review F9).
//! - `effect.reorder {layer, instance, to}` (`STUDIO_EFFECT_REORDER`), `effect.toggle {layer, instance?, enabled}`
//!   (`STUDIO_EFFECT_TOGGLE`; without `instance`: the stack's master switch), `effect.remove {layer, instance}`
//!   (`STUDIO_EFFECT_REMOVE`; the last entry removes the stack when it has no stack mask).
//! - `effect.read {layer, full?}`: the layer's content bounds and its stack: each entry's kind, parameters, opacity,
//!   enable, whether it holds a mask, and `out_of_soft_range` (the parameters whose value lies outside a known soft
//!   range, [STU-FX-105]). A liquify mesh is summarised (`origin_x`, `origin_y`, `width`, `height`, `spacing`,
//!   `cols`, `rows`, `vertices`) unless `full` is set.
//! - `adjustment.create {adjustment, name?, clipped?}`: a new `adjustment` layer above the selected layer holding
//!   the Folio `StudioAdjustment` record (tag `kind`, [STU-RAS-151]; payload `hsk.studio.adjustment@1` naming its
//!   own id, minted from the block `7ede-9300`: the first counter the document and the entry do not hold), clipped
//!   to the layer below when `clipped`; layer and record are one step. Any open document takes one.
//! - `adjustment.edit {layer, adjustment}`: the adjustment layer's record replaced (any kind: Layer > Change Layer
//!   Content); re-editable parameters ([STU-RAS-035]).
//! - `adjustment.apply {layer, adjustment}`: the explicit destructive apply-in-place form ([STU-RAS-035]), one job
//!   admitted through the viewer's bound JobPort (`overloaded`, `canceled`, `stale_revision` refuse before any
//!   pixel is read; the result carries the settlement as `job`). Each texel with alpha > 0 becomes what the
//!   adjustment layer's composite makes of it over a lone layer: pigment `composite_adjustment_in` with the range,
//!   working depth and blend of the viewer's records envelope (`OutputStage::default()`, Normal, coverage 1), alpha
//!   kept (review F7). So a layer holding values outside 0..=1 refuses `adjust_out_of_domain`, as the composite
//!   does. A placed asset layer refuses `layer_not_pixel` (rasterize it first).
//!
//! An adjustment record is checked by Folio (structure, declared bounds) and by the engine before anything is
//! written: composite's conversion (`lower_extensions::adjustment_op`, its codes such as `colour_lookup_unresolved`)
//! and pigment's `prepare` (`adjust_invalid_parameter` naming the parameter, `adjust_unsupported`).
//!
//! Refusal codes: `not_open`, `layer_unknown`, `layer_not_pixel`, `layer_not_adjustment`, `layer_empty`,
//! `tile_format_unsupported`, `effect_instance_unknown`, `effect_stack_full` (Folio's `MAX_SMART_FILTERS`),
//! `index_out_of_range`, `nothing_to_change`, `parameter_unknown`, `parameter_missing`, `parameter_invalid`,
//! `effect_param_out_of_range`, `effect_value_range_unsupported`, `effect_render_budget_exceeded`, pigment's
//! `filter_parameter`, composite's and pigment's adjustment codes, `overloaded`, `stale_revision`,
//! `folio_<code>`, `budget_exceeded`, `canceled` and the run's own codes.
//!
//! Cuts (named, not silent): per-filter and stack masks (`STUDIO_EFFECT_MASK_EDIT`), selection-scoped apply,
//! destructive filter forms and bake (`STUDIO_EFFECT_BAKE`) are the next slice; the sandbox ->
//! `StudioValidationDescriptor` -> `PromotionGate` route of [STU-FX-007] is not built, and the effect edits are
//! entry-only steps (above); the [STU-FX-103] / [STU-FX-106] `hsk.studio.effect_parameter@1` record and its
//! `bound_state` (a spec conflict with [STU-RAS-104] / [STU-RAS-106], SD proposal in the lane notes); the
//! adjustment layer's built-in mask ([STU-RAS-035]: `adjustment.create` makes none); adjustment presets and
//! default-valued one-click creation (most adjustment defaults are `unknown`, [STU-RAS-153]); the `ocio`, `normals`,
//! `tone_stretch`, `desaturate`, `equalize` and auto kinds have no Folio `StudioAdjustment` member yet; adjustment
//! parameter specs (the [STU-RAS-151] table) are not emitted, and pigment's adjustment ranges are narrower than the
//! spec's `unknown` hard bounds on several kinds (vibrance, colour balance, photo filter density, ...): a pigment
//! defect, named; the catalog row's own `help --json` parameters carry no schema bounds (schemars is not an
//! approved harness-core dependency): the bounds are in `effect.parameters`. Photoshop parity of every kernel and
//! default is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::commands::CommandRefusal;
use hsk_studio_folio::{
    self as folio, MAX_LIQUIFY_VERTICES, MAX_SMART_FILTERS, StudioAdjustment, StudioRasterLayerRecord, StudioRasterRecords, StudioSmartFilter,
    StudioSmartFilterKind, StudioSmartFilterStack,
};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::adjust_math::{AdjustError, PreparedAdjustment, composite_adjustment_in};
use hsk_studio_pigment::blend_math::{Rgba, StudioBlendMode};
use hsk_studio_pigment::filter_math::filter_window_cost;
use hsk_studio_pigment::render_plan::OutputStage;
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::smart_filter::{SmartFilterOp, StackFilter, StackLimits};
use serde_json::{Map, Value, json};

/// Schema id of a parameter spec record ([STU-RAS-103]).
pub const PARAMETER_SPEC_SCHEMA: &str = "hsk.studio.parameter_spec@1";
/// The contract check over parameter specs ([STU-FX-123], [STU-RAS-111]).
pub const PARAM_CONTRACT_CHECK: &str = "STUDIO_EFFECT_PARAM_CONTRACT";
/// The closed [STU-RAS-105] unit vocabulary (`document_unit` is deferred and not used here).
pub const UNITS: &[&str] = &[
    "percent",
    "pixels",
    "degrees",
    "points",
    "picas",
    "inches",
    "millimetres",
    "centimetres",
    "pixels_per_inch",
    "relative_distance",
    "normalized_0_1",
    "normalized_signed_1",
    "kelvin",
    "stops_ev",
    "levels_0_255",
    "gradient_position_0_4096",
    "milliseconds",
    "count",
    "none",
];
/// The [STU-RAS-103] fields every numeric spec carries, each a separate value.
const SPEC_FIELDS: [&str; 13] =
    ["hard_min", "hard_max", "soft_min", "soft_max", "default", "unit", "precision", "step", "coarse_step", "fine_step", "observed_min", "observed_max", "bound_classification"];
const BOUND_FIELDS: [&str; 4] = ["hard_min", "hard_max", "soft_min", "soft_max"];
const UNKNOWN: &str = "unknown";
const ABSENT: &str = "absent";

/// [STU-RAS-112] derivation classification of one value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classification {
    Declared,
    Observed,
    Derived,
    Unknown,
}

impl Classification {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Observed => "observed",
            Self::Derived => "derived",
            Self::Unknown => UNKNOWN,
        }
    }
}

/// One bound: its value (`None` = `unknown`), its classification, and whether the bound itself is excluded
/// (a mathematical domain such as `> 0`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bound {
    pub value: Option<f64>,
    pub class: Classification,
    pub open: bool,
}

const UNK: Bound = Bound { value: None, class: Classification::Unknown, open: false };

const fn dec(v: f64) -> Bound {
    Bound { value: Some(v), class: Classification::Declared, open: false }
}

const fn der(v: f64) -> Bound {
    Bound { value: Some(v), class: Classification::Derived, open: false }
}

/// A derived open lower bound (`> v`).
const fn above(v: f64) -> Bound {
    Bound { value: Some(v), class: Classification::Derived, open: true }
}

/// A parameter's factory value ([STU-RAS-103] `default`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DefaultValue {
    Number(f64, Classification),
    Token(&'static str, Classification),
    /// `fraction` of the layer's content bounds on one axis (`y` false: x), as a source declares it; the command
    /// resolves it from the layer (review F10: the Radial Blur table's centre, (50 %, 50 %) of the layer).
    LayerFraction { fraction: f64, y: bool },
    Unknown,
}

/// The integer type of an integer parameter (its domain is checked as a type, not as a bound).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Int {
    U8,
    U32,
    U64,
    I32,
}

/// One enumeration member: its token, its [STU-RAS-109] integer discriminant and where that number comes from.
pub type Member = (&'static str, u16, Classification);

/// What a parameter holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamType {
    Float,
    Int(Int),
    Bool,
    Enum(&'static [Member]),
    /// The 25 `i32` weights of a 5x5 convolution, row-major.
    Weights,
    /// A liquify mesh (Folio `StudioLiquifyMesh`).
    Mesh,
}

/// One parameter of a smart filter kind ([STU-RAS-103]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectParam {
    pub key: &'static str,
    pub ty: ParamType,
    pub hard: [Bound; 2],
    pub soft: [Bound; 2],
    pub default: DefaultValue,
    /// A [STU-RAS-105] token; `None` = `unknown`.
    pub unit: Option<&'static str>,
    /// The source table's own unit word where the engine reads the value in another unit (review F10).
    pub table_unit: Option<&'static str>,
    /// Decimal places; `None` = `unknown`.
    pub precision: Option<u8>,
    /// `observed_min`, `observed_max` ([STU-RAS-107]); `None` = `absent`.
    pub observed: Option<[f64; 2]>,
    /// Zero is outside the domain (a convolution scale divides by it).
    pub nonzero: bool,
}

/// The parameter specs of one Folio `StudioSmartFilterKind`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterSpec {
    pub kind: &'static str,
    /// Where the bounds come from.
    pub source: &'static str,
    pub params: &'static [EffectParam],
}

const fn p(key: &'static str, ty: ParamType, hard: [Bound; 2], soft: [Bound; 2], default: DefaultValue, unit: Option<&'static str>, precision: Option<u8>) -> EffectParam {
    EffectParam { key, ty, hard, soft, default, unit, table_unit: None, precision, observed: None, nonzero: false }
}

/// A float parameter with only engine (derived or unknown) hard bounds: soft, default and precision `unknown`.
const fn engine(key: &'static str, hard: [Bound; 2], unit: Option<&'static str>) -> EffectParam {
    p(key, ParamType::Float, hard, [UNK, UNK], DefaultValue::Unknown, unit, None)
}

/// An integer parameter with engine bounds (precision 0 from the type).
const fn int(key: &'static str, t: Int, hard: [Bound; 2], unit: Option<&'static str>) -> EffectParam {
    p(key, ParamType::Int(t), hard, [UNK, UNK], DefaultValue::Unknown, unit, Some(0))
}

const fn other(key: &'static str, ty: ParamType, default: DefaultValue) -> EffectParam {
    p(key, ty, [UNK, UNK], [UNK, UNK], default, None, None)
}

const D: Classification = Classification::Declared;
const DERIVED: Classification = Classification::Derived;
const PX: Option<&str> = Some("pixels");
const PCT: Option<&str> = Some("percent");
const DEG: Option<&str> = Some("degrees");
const LEVELS: Option<&str> = Some("levels_0_255");
const COUNT: Option<&str> = Some("count");
const NO_UNIT: Option<&str> = None;

/// 14.9.7 tables (1-based, declared).
const DIMENSIONS: &[Member] = &[("horizontal_and_vertical", 1, D), ("horizontal", 2, D), ("vertical", 3, D)];
const RADIAL: &[Member] = &[("spin", 1, D), ("zoom", 2, D)];
/// Studio's own enumerations (review F6, lead ruling: contiguous from 1, Folio member order).
const NOISE: &[Member] = &[("uniform", 1, DERIVED), ("gaussian", 2, DERIVED)];
const PRESERVE: &[Member] = &[("squareness", 1, DERIVED), ("roundness", 2, DERIVED)];
/// [STU-RAS-141] resample members 2..=4.
const RESAMPLE: &[Member] = &[("nearest_neighbour", 2, D), ("bilinear", 3, D), ("bicubic", 4, D)];

const PIGMENT_FILTER: &str = "pigment filter_math (Studio readings; Photoshop ranges UNVERIFIED, D5): hard bounds derived from the engine";
const PIGMENT_KERNEL: &str = "pigment filter_kernels: hard bounds derived from the engine's domain (no table declares them)";
const NO_PARAMS: &[EffectParam] = &[];
const MORPH: &[EffectParam] = &[
    p("radius_px", ParamType::Int(Int::U32), [dec(0.0), dec(32_000.0)], [dec(0.0), dec(127.0)], DefaultValue::Number(0.0, D), PX, Some(0)),
    other("preserve", ParamType::Enum(PRESERVE), DefaultValue::Unknown),
];

/// The 25 kinds in Folio order.
pub const FILTER_SPECS: &[FilterSpec] = &[
    FilterSpec { kind: "gaussian_blur", source: "pigment blur_math MAX_BLUR_RADIUS_PX (Studio engineering bound)", params: &[engine("radius_px", [der(0.0), der(1000.0)], PX)] },
    FilterSpec {
        kind: "unsharp_mask",
        source: PIGMENT_FILTER,
        params: &[engine("amount_pct", [der(1.0), der(500.0)], PCT), engine("radius_px", [der(0.1), der(1000.0)], PX), int("threshold_levels", Int::U8, [der(0.0), der(255.0)], LEVELS)],
    },
    FilterSpec { kind: "high_pass", source: PIGMENT_FILTER, params: &[engine("radius_px", [der(0.1), der(1000.0)], PX)] },
    FilterSpec { kind: "median", source: PIGMENT_FILTER, params: &[int("radius_px", Int::U32, [der(1.0), der(500.0)], PX)] },
    FilterSpec {
        kind: "add_noise",
        source: PIGMENT_FILTER,
        params: &[
            engine("amount_pct", [der(0.1), der(400.0)], PCT),
            other("distribution", ParamType::Enum(NOISE), DefaultValue::Unknown),
            other("monochromatic", ParamType::Bool, DefaultValue::Unknown),
            int("seed", Int::U64, [UNK, UNK], COUNT),
        ],
    },
    FilterSpec {
        kind: "liquify",
        source: "Folio StudioLiquifyMesh and pigment LiquifyMesh::validate (structure, not a range); the mesh covers the layer's content bounds",
        params: &[other("mesh", ParamType::Mesh, DefaultValue::Unknown), other("resample", ParamType::Enum(RESAMPLE), DefaultValue::Unknown)],
    },
    FilterSpec {
        kind: "box_blur",
        source: "14.9.7 Fast Box Blur table (ADBE Box Blur2); budget: 2 floor(radius + 0.5) iterations + 1 taps at most MAX_BOX_KERNEL_TAPS (6001), an admission refusal, not a bound",
        params: &[
            p("radius_px", ParamType::Float, [dec(0.0), dec(30_000.0)], [dec(0.0), dec(127.0)], DefaultValue::Number(0.0, D), PX, Some(1)),
            p("iterations", ParamType::Int(Int::U32), [dec(1.0), dec(50.0)], [dec(1.0), dec(5.0)], DefaultValue::Number(3.0, D), COUNT, Some(0)),
            other("dimensions", ParamType::Enum(DIMENSIONS), DefaultValue::Token("horizontal_and_vertical", D)),
        ],
    },
    FilterSpec {
        kind: "motion_blur",
        source: "14.9.7 Directional Blur table (ADBE Motion Blur)",
        params: &[
            p("angle_deg", ParamType::Float, [UNK, UNK], [UNK, UNK], DefaultValue::Number(0.0, D), DEG, None),
            p("length_px", ParamType::Float, [dec(0.0), dec(1000.0)], [dec(0.0), dec(20.0)], DefaultValue::Number(0.0, D), PX, Some(1)),
        ],
    },
    FilterSpec {
        kind: "radial_blur",
        source: "14.9.7 Radial Blur table (ADBE Radial Blur): the centre's default is (50 %, 50 %) of the layer, resolved over its content bounds",
        params: &[
            p("centre_x_px", ParamType::Float, [UNK, UNK], [UNK, UNK], DefaultValue::LayerFraction { fraction: 0.5, y: false }, PX, None),
            p("centre_y_px", ParamType::Float, [UNK, UNK], [UNK, UNK], DefaultValue::LayerFraction { fraction: 0.5, y: true }, PX, None),
            p("amount", ParamType::Float, [dec(0.0), dec(1000.0)], [dec(0.0), dec(100.0)], DefaultValue::Number(10.0, D), NO_UNIT, Some(1)),
            other("kind", ParamType::Enum(RADIAL), DefaultValue::Token("spin", D)),
        ],
    },
    FilterSpec { kind: "find_edges", source: PIGMENT_KERNEL, params: &[other("invert", ParamType::Bool, DefaultValue::Unknown)] },
    FilterSpec {
        kind: "custom_convolution",
        source: "pigment filter_kernels: any i32 weights and offset, scale != 0 (division domain)",
        params: &[
            other("weights", ParamType::Weights, DefaultValue::Unknown),
            EffectParam { nonzero: true, ..int("scale", Int::I32, [UNK, UNK], COUNT) },
            int("offset", Int::I32, [UNK, UNK], COUNT),
        ],
    },
    FilterSpec { kind: "sharpen", source: PIGMENT_KERNEL, params: NO_PARAMS },
    FilterSpec { kind: "sharpen_more", source: PIGMENT_KERNEL, params: NO_PARAMS },
    FilterSpec { kind: "basic_blur", source: PIGMENT_KERNEL, params: NO_PARAMS },
    FilterSpec { kind: "blur_more", source: PIGMENT_KERNEL, params: NO_PARAMS },
    FilterSpec { kind: "sharpen_edges", source: PIGMENT_KERNEL, params: NO_PARAMS },
    FilterSpec { kind: "despeckle", source: PIGMENT_KERNEL, params: NO_PARAMS },
    FilterSpec {
        kind: "emboss",
        source: "pigment filter_kernels (STU-FX-128: the Emboss catalogue row has no typed parameters): height and amount >= 0, any finite angle",
        params: &[engine("angle_deg", [UNK, UNK], DEG), engine("height_px", [der(0.0), UNK], PX), engine("amount_pct", [der(0.0), UNK], PCT)],
    },
    FilterSpec {
        kind: "solarize",
        source: "14.9.7 Solarize table (Threshold 0..254; soft and default not declared)",
        params: &[p("threshold_levels", ParamType::Int(Int::U8), [dec(0.0), dec(254.0)], [UNK, UNK], DefaultValue::Unknown, LEVELS, Some(0))],
    },
    FilterSpec {
        kind: "mosaic",
        source: "14.9.7 Mosaic table rows 1, 2 (ADBE Mosaic)",
        params: &[
            p("horizontal_blocks", ParamType::Int(Int::U32), [dec(1.0), dec(4000.0)], [dec(1.0), dec(200.0)], DefaultValue::Number(10.0, D), COUNT, Some(0)),
            p("vertical_blocks", ParamType::Int(Int::U32), [dec(1.0), dec(4000.0)], [dec(1.0), dec(200.0)], DefaultValue::Number(10.0, D), COUNT, Some(0)),
        ],
    },
    FilterSpec { kind: "maximum", source: "14.9.7 Minimax table (Radius); preserve is Studio's (STU-FX-022)", params: MORPH },
    FilterSpec { kind: "minimum", source: "14.9.7 Minimax table (Radius); preserve is Studio's (STU-FX-022)", params: MORPH },
    FilterSpec {
        kind: "dust_and_scratches",
        source: "14.9.7 Dust & Scratches table (soft equal to hard as the table declares); the table's threshold unit `pixel` is kept as table_unit: the engine compares a 0..1 channel difference (D5 question)",
        params: &[
            p("radius_px", ParamType::Int(Int::U32), [dec(0.0), dec(255.0)], [dec(0.0), dec(255.0)], DefaultValue::Number(1.0, D), PX, Some(0)),
            EffectParam {
                table_unit: Some("pixel"),
                ..p("threshold", ParamType::Float, [dec(0.0), dec(1.0)], [dec(0.0), dec(1.0)], DefaultValue::Number(0.0, D), Some("normalized_0_1"), Some(1))
            },
        ],
    },
    FilterSpec {
        kind: "surface_blur",
        source: "pigment filter_kernels: radius any u32, threshold > 0 (domain)",
        params: &[int("radius_px", Int::U32, [der(0.0), UNK], PX), engine("threshold_levels", [above(0.0), UNK], LEVELS)],
    },
    FilterSpec {
        kind: "bilateral_blur",
        source: "pigment filter_kernels: radius any u32, range sigma > 0 (domain)",
        params: &[int("radius_px", Int::U32, [der(0.0), UNK], PX), engine("range_sigma", [above(0.0), UNK], NO_UNIT)],
    },
];

/// The spec table of `kind`.
pub fn filter_spec(kind: &str) -> Option<&'static FilterSpec> {
    FILTER_SPECS.iter().find(|s| s.kind == kind)
}

fn bound_json(b: Bound) -> Value {
    b.value.map_or_else(|| json!(UNKNOWN), |v| json!(v))
}

/// [STU-RAS-108] step family: `10^-precision` (1 when precision is `unknown`), coarse x10, fine /10 no finer than
/// `10^-precision`.
fn steps(precision: Option<u8>) -> [f64; 3] {
    match precision {
        Some(p) => {
            let step = 10f64.powi(i32::from(p)).recip();
            [step, step * 10.0, step]
        }
        None => [1.0, 10.0, 0.1],
    }
}

/// `n` as JSON: an integer for an integer parameter (the tables state whole numbers), else a number.
fn number_json(ty: ParamType, n: f64) -> Value {
    match ty {
        // The declared integer defaults are small whole numbers (0..=10 in the table).
        ParamType::Int(_) if n.fract() == 0.0 && n.abs() < 9.0e15 => json!(n as i64),
        _ => json!(n),
    }
}

fn numeric(ty: ParamType) -> Option<&'static str> {
    match ty {
        ParamType::Float => Some("number"),
        ParamType::Int(_) => Some("integer"),
        _ => None,
    }
}

fn default_spec(p: &EffectParam) -> (Value, Classification) {
    match p.default {
        DefaultValue::Number(v, c) => (number_json(p.ty, v), c),
        DefaultValue::Token(t, c) => (json!(t), c),
        DefaultValue::LayerFraction { fraction, y } => (json!({"fraction_of_layer_content_bounds": fraction, "axis": if y { "y" } else { "x" }}), D),
        DefaultValue::Unknown => (json!(UNKNOWN), Classification::Unknown),
    }
}

/// One parameter's spec record as the command surface shows it.
pub fn param_spec_json(index: usize, p: &EffectParam) -> Value {
    let (default, default_class) = default_spec(p);
    let Some(kind) = numeric(p.ty) else {
        let mut spec = json!({"param_key": p.key, "param_index": index, "default": default, "default_classification": default_class.token()});
        match p.ty {
            ParamType::Bool => spec["kind"] = json!("boolean"),
            ParamType::Enum(options) => {
                spec["kind"] = json!("enumeration");
                spec["options"] = Value::Array(options.iter().map(|(t, d, c)| json!({"token": t, "discriminant": d, "discriminant_classification": c.token()})).collect());
            }
            ParamType::Weights => {
                spec["kind"] = json!("integer_list");
                spec["length"] = json!(25);
                spec["item_kind"] = json!("i32");
            }
            _ => spec["kind"] = json!("record"),
        }
        return spec;
    };
    let [step, coarse, fine] = steps(p.precision);
    let mut spec = json!({
        "schema_id": PARAMETER_SPEC_SCHEMA, "param_key": p.key, "param_index": index, "kind": kind,
        "hard_min": bound_json(p.hard[0]), "hard_max": bound_json(p.hard[1]), "soft_min": bound_json(p.soft[0]), "soft_max": bound_json(p.soft[1]),
        "hard_min_exclusive": p.hard[0].open,
        "default": default, "unit": p.unit.unwrap_or(UNKNOWN), "precision": p.precision.map_or_else(|| json!(UNKNOWN), |v| json!(v)),
        "step": step, "coarse_step": coarse, "fine_step": fine,
        "observed_min": p.observed.map_or_else(|| json!(ABSENT), |o| json!(o[0])), "observed_max": p.observed.map_or_else(|| json!(ABSENT), |o| json!(o[1])),
        "bound_classification": {
            "hard_min": p.hard[0].class.token(), "hard_max": p.hard[1].class.token(), "soft_min": p.soft[0].class.token(), "soft_max": p.soft[1].class.token(),
            "default": default_class.token(), "step": "derived",
        },
    });
    if p.nonzero {
        spec["excludes"] = json!([0]);
    }
    if let Some(t) = p.table_unit {
        spec["table_unit"] = json!(t);
    }
    if let ParamType::Int(t) = p.ty {
        spec["integer_type"] = json!(int_name(t));
    }
    spec
}

const fn int_name(t: Int) -> &'static str {
    match t {
        Int::U8 => "u8",
        Int::U32 => "u32",
        Int::U64 => "u64",
        Int::I32 => "i32",
    }
}

/// [STU-RAS-110] input schema of one parameter: JSON Schema, the known hard bounds as schema bounds (none for an
/// `unknown` one), the rest of the spec as `x-` annotations.
fn param_input_schema(p: &EffectParam) -> Value {
    let spec = param_spec_json(0, p);
    match p.ty {
        ParamType::Float | ParamType::Int(_) => {
            let mut s = json!({"type": if matches!(p.ty, ParamType::Int(_)) { "integer" } else { "number" }});
            if let Some(v) = p.hard[0].value {
                s[if p.hard[0].open { "exclusiveMinimum" } else { "minimum" }] = json!(v);
            }
            if let Some(v) = p.hard[1].value {
                s["maximum"] = json!(v);
            }
            if p.nonzero {
                s["not"] = json!({"const": 0});
            }
            if spec["default"].is_number() {
                s["default"] = spec["default"].clone();
            }
            for (field, annotation) in [("soft_min", "x-soft-min"), ("soft_max", "x-soft-max"), ("unit", "x-unit"), ("precision", "x-precision"), ("step", "x-step"), ("coarse_step", "x-coarse-step"), ("fine_step", "x-fine-step"), ("bound_classification", "x-bound-classification"), ("integer_type", "x-integer-type"), ("table_unit", "x-table-unit")] {
                if let Some(v) = spec.get(field) {
                    s[annotation] = v.clone();
                }
            }
            if spec["default"].is_object() {
                s["x-default"] = spec["default"].clone();
            }
            s
        }
        ParamType::Bool => json!({"type": "boolean"}),
        ParamType::Enum(options) => {
            let mut s = json!({"type": "string", "enum": options.iter().map(|(t, _, _)| *t).collect::<Vec<_>>(), "x-discriminants": spec["options"].clone()});
            if spec["default"] != UNKNOWN {
                s["default"] = spec["default"].clone();
            }
            s
        }
        ParamType::Weights => json!({"type": "array", "minItems": 25, "maxItems": 25, "items": {"type": "integer", "minimum": -2_147_483_648_i64, "maximum": 2_147_483_647_i64}}),
        ParamType::Mesh => json!({"type": "object", "x-record": "hsk.studio StudioLiquifyMesh {origin_x, origin_y, width, height, spacing, cols, rows, displacement, freeze}"}),
    }
}

/// [STU-RAS-110] the input schema of `params` for one kind.
fn kind_input_schema(spec: &FilterSpec) -> Value {
    let mut properties = Map::new();
    let mut required = Vec::new();
    for p in spec.params {
        properties.insert(p.key.into(), param_input_schema(p));
        if p.default == DefaultValue::Unknown {
            required.push(p.key);
        }
    }
    json!({"type": "object", "additionalProperties": false, "properties": properties, "required": required})
}

/// The `STUDIO_EFFECT_PARAM_CONTRACT` check ([STU-FX-123], [STU-RAS-111]) over one numeric spec record (an
/// enumeration passes when it lists its members, each with an integer discriminant). Findings, each
/// `{param_key, code}`: `field_missing:<field>` (a [STU-RAS-103] field absent), `classification_missing:<bound>`,
/// `classification_mismatch:<bound>` (a number classified `unknown`, or `unknown` classified otherwise),
/// `hard_soft_equated` (soft equal to hard without `declared` on all four, [STU-RAS-104]),
/// `hard_bound_observed:<bound>` (a hard bound classified `observed`: a clamp to an observation, [STU-RAS-107]),
/// `observed_used_as_bound:<bound>` (a bound equal to its observed value without `declared`), `unit_not_in_vocabulary`,
/// `step_disagrees` (not the [STU-RAS-108] derivation), `default_outside_hard`, `options_missing`,
/// `discriminant_missing:<token>` ([STU-RAS-109]).
pub fn parameter_contract_findings(spec: &Value) -> Vec<Value> {
    let key = spec["param_key"].as_str().unwrap_or("");
    let mut out = Vec::new();
    let mut find = |code: String| out.push(json!({"param_key": key, "code": code}));
    match spec["kind"].as_str() {
        Some("number" | "integer") => {}
        Some("enumeration") => {
            match spec["options"].as_array().filter(|o| !o.is_empty()) {
                None => find("options_missing".into()),
                Some(options) => {
                    for o in options.iter().filter(|o| o["discriminant"].as_u64().is_none()) {
                        find(format!("discriminant_missing:{}", o["token"].as_str().unwrap_or("?")));
                    }
                }
            }
            return out;
        }
        _ => return out,
    }
    for field in SPEC_FIELDS {
        if spec.get(field).is_none() {
            find(format!("field_missing:{field}"));
        }
    }
    let class = |b: &str| spec["bound_classification"][b].as_str();
    for b in BOUND_FIELDS {
        match class(b) {
            None => find(format!("classification_missing:{b}")),
            Some(c) => {
                let is_number = spec[b].is_number();
                if (c == UNKNOWN) == is_number || (!is_number && spec[b] != UNKNOWN) {
                    find(format!("classification_mismatch:{b}"));
                }
                if b.starts_with("hard") && c == "observed" {
                    find(format!("hard_bound_observed:{b}"));
                }
            }
        }
    }
    let num = |f: &str| spec[f].as_f64();
    if let (Some(h0), Some(h1), Some(s0), Some(s1)) = (num("hard_min"), num("hard_max"), num("soft_min"), num("soft_max"))
        && h0 == s0
        && h1 == s1
        && !BOUND_FIELDS.iter().all(|b| class(b) == Some("declared"))
    {
        find("hard_soft_equated".into());
    }
    for (b, o) in [("hard_min", "observed_min"), ("soft_min", "observed_min"), ("hard_max", "observed_max"), ("soft_max", "observed_max")] {
        if let (Some(v), Some(ob)) = (num(b), num(o))
            && v == ob
            && class(b) != Some("declared")
        {
            find(format!("observed_used_as_bound:{b}"));
        }
    }
    if spec["unit"].as_str().is_none_or(|u| u != UNKNOWN && !UNITS.contains(&u)) {
        find("unit_not_in_vocabulary".into());
    }
    let precision = match &spec["precision"] {
        Value::String(s) if s == UNKNOWN => Some(None),
        v => v.as_u64().and_then(|p| u8::try_from(p).ok()).map(Some),
    };
    let derived = precision.map(steps);
    if derived.is_none_or(|d| [num("step"), num("coarse_step"), num("fine_step")] != [Some(d[0]), Some(d[1]), Some(d[2])]) {
        find("step_disagrees".into());
    }
    if let Some(d) = num("default")
        && (num("hard_min").is_some_and(|h| d < h) || num("hard_max").is_some_and(|h| d > h))
    {
        find("default_outside_hard".into());
    }
    out
}

/// `effect.parameters`: the specs and input schemas of `kind` (or every kind) and the contract findings over them.
pub fn parameters(kind: Option<&str>) -> Result<Value, CommandRefusal> {
    let specs: Vec<&FilterSpec> = match kind {
        Some(k) => vec![filter_spec(k).ok_or_else(|| unknown_kind(k))?],
        None => FILTER_SPECS.iter().collect(),
    };
    let mut findings = Vec::new();
    let mut kinds = Vec::with_capacity(specs.len());
    let mut variants = Vec::with_capacity(specs.len());
    for s in specs {
        let params: Vec<Value> = s.params.iter().enumerate().map(|(i, p)| param_spec_json(i, p)).collect();
        for spec in &params {
            for f in parameter_contract_findings(spec) {
                findings.push(json!({"filter_kind": s.kind, "finding": f}));
            }
        }
        let schema = kind_input_schema(s);
        variants.push(json!({"properties": {"filter_kind": {"const": s.kind}, "params": schema}}));
        kinds.push(json!({"filter_kind": s.kind, "source": s.source, "parameters": params, "input_schema": schema}));
    }
    let apply = json!({
        "type": "object", "required": ["layer", "filter_kind"],
        "properties": {"layer": {"type": "string"}, "filter_kind": {"type": "string"}, "params": {"type": "object"},
            "opacity": {"type": "number", "minimum": 0, "maximum": 100, "default": 100}, "enabled": {"type": "boolean", "default": true}, "at": {"type": "integer", "minimum": 0}},
        "oneOf": variants,
    });
    Ok(json!({"schema_id": PARAMETER_SPEC_SCHEMA, "check": PARAM_CONTRACT_CHECK, "findings": findings, "kinds": kinds, "effect_apply_input_schema": apply}))
}

// ---- parameter decoding --------------------------------------------------------------------------------------

fn invalid(key: &str, expected: &str) -> CommandRefusal {
    CommandRefusal::new("parameter_invalid", json!({"key": key, "expected": expected}))
}

fn missing(key: &str) -> CommandRefusal {
    CommandRefusal::new("parameter_missing", json!({"key": key}))
}

fn unknown_kind(kind: &str) -> CommandRefusal {
    let kinds: Vec<&str> = FILTER_SPECS.iter().map(|s| s.kind).collect();
    CommandRefusal::new("parameter_invalid", json!({"key": "filter_kind", "value": kind, "expected": kinds.join("|")}))
}

fn given<'a>(params: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    params.get(key).filter(|v| !v.is_null())
}

fn text(params: &Map<String, Value>, key: &str) -> Result<String, CommandRefusal> {
    match given(params, key) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(Value::Number(n)) => Ok(n.to_string()),
        Some(_) => Err(invalid(key, "text")),
        None => Err(missing(key)),
    }
}

fn opt_text(params: &Map<String, Value>, key: &str) -> Result<Option<String>, CommandRefusal> {
    if given(params, key).is_some() { text(params, key).map(Some) } else { Ok(None) }
}

fn opt_number(params: &Map<String, Value>, key: &str) -> Result<Option<f64>, CommandRefusal> {
    let Some(v) = given(params, key) else { return Ok(None) };
    let n = match v {
        Value::String(s) => s.parse::<f64>().ok(),
        other => other.as_f64(),
    };
    n.filter(|n| n.is_finite()).map(Some).ok_or_else(|| invalid(key, "finite number"))
}

fn opt_flag(params: &Map<String, Value>, key: &str) -> Result<Option<bool>, CommandRefusal> {
    match given(params, key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(Value::String(s)) if s == "true" => Ok(Some(true)),
        Some(Value::String(s)) if s == "false" => Ok(Some(false)),
        Some(_) => Err(invalid(key, "boolean")),
    }
}

fn opt_index(params: &Map<String, Value>, key: &str) -> Result<Option<usize>, CommandRefusal> {
    let Some(v) = given(params, key) else { return Ok(None) };
    let n = match v {
        Value::String(s) => s.parse::<u64>().ok(),
        other => other.as_u64(),
    };
    n.and_then(|n| usize::try_from(n).ok()).map(Some).ok_or_else(|| invalid(key, "index"))
}

/// A record parameter: a JSON object, or its JSON text (a `--param` value).
fn opt_object(params: &Map<String, Value>, key: &str, expected: &str) -> Result<Option<Map<String, Value>>, CommandRefusal> {
    match given(params, key) {
        None => Ok(None),
        Some(Value::Object(m)) => Ok(Some(m.clone())),
        Some(Value::String(s)) if s.trim_start().starts_with('{') => match serde_json::from_str(s) {
            Ok(Value::Object(m)) => Ok(Some(m)),
            _ => Err(invalid(key, expected)),
        },
        Some(_) => Err(invalid(key, expected)),
    }
}

fn out_of_range(kind: &str, p: &EffectParam, value: &Value) -> CommandRefusal {
    let mut detail = json!({"filter_kind": kind, "param": p.key, "value": value, "hard_min": bound_json(p.hard[0]), "hard_max": bound_json(p.hard[1])});
    if p.hard[0].open {
        detail["hard_min_exclusive"] = json!(true);
    }
    if p.nonzero {
        detail["excludes"] = json!([0]);
    }
    CommandRefusal::new("effect_param_out_of_range", detail)
}

/// `v` inside the hard bounds of `p` (an `unknown` bound does not limit, [STU-RAS-106]).
fn within_hard(p: &EffectParam, v: f64) -> bool {
    let lo = p.hard[0].value.is_none_or(|h| if p.hard[0].open { v > h } else { v >= h });
    let hi = p.hard[1].value.is_none_or(|h| v <= h);
    lo && hi && !(p.nonzero && v == 0.0)
}

const fn int_domain(t: Int) -> (i128, i128) {
    match t {
        Int::U8 => (0, 255),
        Int::U32 => (0, 4_294_967_295),
        Int::U64 => (0, 18_446_744_073_709_551_615),
        Int::I32 => (-2_147_483_648, 2_147_483_647),
    }
}

fn as_i128(v: &Value) -> Option<i128> {
    v.as_u64().map(i128::from).or_else(|| v.as_i64().map(i128::from))
}

/// One parameter value checked against its spec; the value Folio's record takes.
fn check_param(kind: &str, p: &EffectParam, v: &Value) -> Result<Value, CommandRefusal> {
    let key = format!("params.{}", p.key);
    match p.ty {
        ParamType::Float => {
            let n = v.as_f64().filter(|n| n.is_finite()).ok_or_else(|| invalid(&key, "finite number"))?;
            if n.abs() > f64::from(f32::MAX) {
                return Err(invalid(&key, "a number a 32-bit float holds"));
            }
            // Review F8: the bound is checked on the value the record stores (a value that rounds to 0 in 32 bits
            // is the 0 the engine sees).
            let stored = f64::from(n as f32);
            if !within_hard(p, stored) {
                return Err(out_of_range(kind, p, v));
            }
            Ok(json!(stored))
        }
        ParamType::Int(t) => {
            let (lo, hi) = int_domain(t);
            let n = as_i128(v).filter(|n| (lo..=hi).contains(n)).ok_or_else(|| invalid(&key, &format!("integer in {lo}..={hi}")))?;
            // Integers of these widths are exact in an f64 up to 2^53; beyond it no hard bound here applies.
            let as_float = n as f64;
            if !within_hard(p, as_float) {
                return Err(out_of_range(kind, p, v));
            }
            Ok(v.clone())
        }
        ParamType::Bool => v.as_bool().map(Value::Bool).ok_or_else(|| invalid(&key, "boolean")),
        ParamType::Enum(options) => {
            let tokens: Vec<&str> = options.iter().map(|(t, _, _)| *t).collect();
            v.as_str().filter(|t| tokens.contains(t)).map(|t| json!(t)).ok_or_else(|| invalid(&key, &tokens.join("|")))
        }
        ParamType::Weights => {
            let items = v.as_array().filter(|a| a.len() == 25).ok_or_else(|| invalid(&key, "25 integers"))?;
            let (lo, hi) = int_domain(Int::I32);
            if items.iter().any(|w| as_i128(w).is_none_or(|w| !(lo..=hi).contains(&w))) {
                return Err(invalid(&key, "25 integers in the i32 range"));
            }
            Ok(v.clone())
        }
        ParamType::Mesh => {
            let m = v.as_object().ok_or_else(|| invalid(&key, "a liquify mesh {origin_x, origin_y, width, height, spacing, cols, rows, displacement, freeze}"))?;
            mesh_lists_within(&key, m, mesh_cap())?;
            Ok(v.clone())
        }
    }
}

/// The most vertices a liquify mesh of a stored stack can hold: Folio's `MAX_LIQUIFY_VERTICES`, bounded by the node
/// budget the raster records are validated with at every edit (`Budget::default().nodes`: stack entries and mesh
/// vertices count against it), so a longer list could never be stored.
fn mesh_cap() -> usize {
    usize::try_from(MAX_LIQUIFY_VERTICES).unwrap_or(usize::MAX).min(folio::Budget::default().nodes)
}

/// Hostile size (V4-02, review F9): a mesh's vertex lists longer than `cap` ([`mesh_cap`]) are refused
/// `budget_exceeded` before Folio decodes them.
fn mesh_lists_within(key: &str, m: &Map<String, Value>, cap: usize) -> Result<(), CommandRefusal> {
    for list in ["displacement", "freeze"] {
        if m.get(list).and_then(Value::as_array).is_some_and(|a| a.len() > cap) {
            return Err(CommandRefusal::new("budget_exceeded", json!({"key": format!("{key}.{list}"), "at_most": cap})));
        }
    }
    Ok(())
}

/// A declared default as the parameter's JSON value (an integer parameter's as an integer); a layer fraction
/// resolved over `content` (the layer's content bounds), `None` without them.
fn default_json(p: &EffectParam, content: Option<Rect>) -> Option<Value> {
    match p.default {
        DefaultValue::Number(n, _) => Some(number_json(p.ty, n)),
        DefaultValue::Token(t, _) => Some(json!(t)),
        DefaultValue::LayerFraction { fraction, y } => content.map(|b| {
            let (origin, size) = if y { (b.y, b.height) } else { (b.x, b.width) };
            json!(origin as f64 + f64::from(size) * fraction)
        }),
        DefaultValue::Unknown => None,
    }
}

/// The Folio kind block of `kind` from `given` parameters (each checked), and the keys that took their declared
/// default (a layer fraction over `content`). Pigment then checks the entry as it will render it
/// (`SmartFilterOp::check`).
fn decode_kind(kind: &str, given_params: &Map<String, Value>, content: Option<Rect>) -> Result<(StudioSmartFilterKind, Vec<&'static str>), CommandRefusal> {
    let spec = filter_spec(kind).ok_or_else(|| unknown_kind(kind))?;
    if let Some(k) = given_params.keys().find(|k| !spec.params.iter().any(|p| p.key == k.as_str())) {
        return Err(CommandRefusal::new("parameter_unknown", json!({"key": format!("params.{k}"), "filter_kind": kind})));
    }
    let mut block = Map::new();
    block.insert("filter_kind".into(), json!(kind));
    let mut defaulted = Vec::new();
    for p in spec.params {
        let value = match given(given_params, p.key) {
            Some(v) => v.clone(),
            None => {
                let d = default_json(p, content).ok_or_else(|| CommandRefusal::new("parameter_missing", json!({"key": format!("params.{}", p.key), "filter_kind": kind, "default": default_spec(p).0})))?;
                defaulted.push(p.key);
                d
            }
        };
        block.insert(p.key.into(), check_param(kind, p, &value)?);
    }
    let parsed: StudioSmartFilterKind =
        serde_json::from_value(Value::Object(block)).map_err(|e| CommandRefusal::new("parameter_invalid", json!({"key": "params", "filter_kind": kind, "message": e.to_string()})))?;
    engine_op(&parsed)?;
    Ok((parsed, defaulted))
}

/// The entry as pigment lowers it, refused when pigment's own check refuses it.
fn engine_op(kind: &StudioSmartFilterKind) -> Result<StackFilter, CommandRefusal> {
    let probe = StudioSmartFilterStack {
        enabled: true,
        stack_mask: None,
        filters: vec![StudioSmartFilter { instance_id: "probe".into(), enabled: true, opacity: 100.0, mask: None, params: kind.clone() }],
    };
    let refused = || CommandRefusal::new("filter_parameter", json!({"filter_kind": kind_tag(kind), "message": "the engine refused the entry (pigment SmartFilterOp::check)"}));
    let op = SmartFilterOp::from_record(&probe, |_| None).map_err(|_| refused())?;
    if !op.check() {
        return Err(refused());
    }
    op.entries.into_iter().next().map(|e| e.filter).ok_or_else(refused)
}

fn kind_tag(kind: &StudioSmartFilterKind) -> String {
    serde_json::to_value(kind).ok().and_then(|v| v["filter_kind"].as_str().map(str::to_owned)).unwrap_or_default()
}

/// The layer a stack renders over (review F1): its pixels over its tiles' bounds (`None` for tiles in a format the
/// viewer does not decode) and those bounds cut to the canvas (the content plane render-cpu filters over), as
/// `place::layer_content` reads them.
pub(super) struct LayerContent {
    pub(super) plane: Option<RgbaPlane>,
    pub(super) bounds: Option<Rect>,
}

/// The render's own admission of one entry over `content` (review F1, lead ruling Q1): the stack's unit-range check
/// on the content (pigment `RgbaPlane::validate`), then the entry's cost over the content bounds against pigment's
/// stack limits (`FilterKernel::admit`; `filter_window_cost` for a core filter), and a liquify mesh over exactly
/// those bounds. No layer content: nothing renders, nothing to admit.
fn admit(layer: &str, op: &StackFilter, kind: &str, content: Option<&LayerContent>) -> Result<(), CommandRefusal> {
    let Some(content) = content else { return Ok(()) };
    if content.plane.as_ref().is_some_and(|p| p.validate().is_err()) {
        return Err(CommandRefusal::new("effect_value_range_unsupported", json!({"layer": layer, "filter_kind": kind, "message": "the smart filter stack renders unit-range content (0..=1); this layer holds other values"})));
    }
    let Some(bounds) = content.bounds else { return Ok(()) };
    let limits = StackLimits::default().filter;
    let budget = |needed: u64, limit: u64| CommandRefusal::new("effect_render_budget_exceeded", json!({"layer": layer, "filter_kind": kind, "needed": needed, "limit": limit}));
    let filter_refusal = |e: hsk_studio_pigment::filter_math::FilterError| match e {
        hsk_studio_pigment::filter_math::FilterError::OverBudget { needed, limit } => budget(needed, limit),
        hsk_studio_pigment::filter_math::FilterError::Overflow => budget(u64::MAX, limits.max_work),
        other => CommandRefusal::new(other.code(), json!({"layer": layer, "filter_kind": kind})),
    };
    match op {
        StackFilter::Kernel(k) => k.admit(bounds, limits).map_err(filter_refusal),
        StackFilter::Filter(f) => {
            let (bytes, work) = filter_window_cost(f, bounds).map_err(filter_refusal)?;
            if bytes > limits.max_bytes {
                return Err(budget(bytes, limits.max_bytes));
            }
            if work > limits.max_work {
                return Err(budget(work, limits.max_work));
            }
            Ok(())
        }
        StackFilter::Liquify { mesh, .. } => {
            if mesh.bounds == bounds {
                Ok(())
            } else {
                Err(CommandRefusal::new(
                    "parameter_invalid",
                    json!({"key": "params.mesh", "expected": "a mesh over the layer's content bounds", "content_bounds": rect_json(bounds), "filter_kind": kind}),
                ))
            }
        }
    }
}

fn rect_json(r: Rect) -> Value {
    json!({"x": r.x, "y": r.y, "width": r.width, "height": r.height})
}

/// The kind tag and the parameters (without the tag) of a Folio kind block; a liquify mesh summarised unless `full`
/// (review F9).
fn kind_params(kind: &StudioSmartFilterKind, full: bool) -> (String, Map<String, Value>) {
    if let StudioSmartFilterKind::Liquify { mesh, resample } = kind
        && !full
    {
        let mut map = Map::new();
        map.insert("resample".into(), serde_json::to_value(resample).unwrap_or(Value::Null));
        map.insert(
            "mesh".into(),
            json!({"summary": true, "origin_x": mesh.origin_x, "origin_y": mesh.origin_y, "width": mesh.width, "height": mesh.height, "spacing": mesh.spacing, "cols": mesh.cols, "rows": mesh.rows, "vertices": mesh.displacement.len()}),
        );
        return ("liquify".into(), map);
    }
    let mut map = match serde_json::to_value(kind) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let tag = map.remove("filter_kind").and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    (tag, map)
}

/// The parameters of an entry whose value lies outside a known soft range ([STU-FX-105] out-of-soft-range state).
fn out_of_soft_range(kind: &str, params: &Map<String, Value>) -> Vec<&'static str> {
    let Some(spec) = filter_spec(kind) else { return Vec::new() };
    spec.params
        .iter()
        .filter(|p| numeric(p.ty).is_some())
        .filter(|p| {
            params.get(p.key).and_then(Value::as_f64).is_some_and(|v| p.soft[0].value.is_some_and(|s| v < s) || p.soft[1].value.is_some_and(|s| v > s))
        })
        .map(|p| p.key)
        .collect()
}

/// Stack-entry opacity, percent `0..=100` ([STU-FX-104] shape, review F10).
fn opacity(v: f64) -> Result<f64, CommandRefusal> {
    if (0.0..=100.0).contains(&v) {
        Ok(v)
    } else {
        Err(CommandRefusal::new("effect_param_out_of_range", json!({"param": "opacity", "value": v, "hard_min": 0.0, "hard_max": 100.0})))
    }
}

fn nothing() -> CommandRefusal {
    CommandRefusal::new("nothing_to_change", json!({}))
}

fn instance_unknown(layer: &str, instance: &str) -> CommandRefusal {
    CommandRefusal::new("effect_instance_unknown", json!({"layer": layer, "instance": instance}))
}

/// The raster layer `layer_id` of `doc`, or the refusal.
fn raster_layer(doc: &folio::StudioDocument, layer_id: &str) -> Result<(), CommandRefusal> {
    let layer = doc.layers.iter().find(|l| l.layer_id == layer_id).ok_or_else(|| CommandRefusal::new("layer_unknown", json!({"layer": layer_id})))?;
    if matches!(layer.payload, folio::Payload::Raster { .. }) { Ok(()) } else { Err(CommandRefusal::new("layer_not_pixel", json!({"layer": layer_id, "kind": layer.kind}))) }
}

/// The record of `layer_id`, created empty when absent.
fn record_mut<'a>(records: &'a mut StudioRasterRecords, layer_id: &str) -> &'a mut StudioRasterLayerRecord {
    let at = match records.layers.iter().position(|r| r.layer_id == layer_id) {
        Some(i) => i,
        None => {
            records.layers.push(StudioRasterLayerRecord::new(layer_id));
            records.layers.len().saturating_sub(1)
        }
    };
    &mut records.layers[at]
}

/// The stack of `layer_id` and the position of `instance` in it.
fn entry_mut<'a>(records: &'a mut StudioRasterRecords, layer_id: &str, instance: &str) -> Result<(&'a mut StudioSmartFilterStack, usize), CommandRefusal> {
    let stack = records.layers.iter_mut().find(|r| r.layer_id == layer_id).and_then(|r| r.smart_filters.as_mut()).ok_or_else(|| instance_unknown(layer_id, instance))?;
    let at = stack.filters.iter().position(|f| f.instance_id == instance).ok_or_else(|| instance_unknown(layer_id, instance))?;
    Ok((stack, at))
}

/// Drops an emptied stack (no entry, no stack mask) and then an emptied record.
fn tidy(records: &mut StudioRasterRecords, layer_id: &str) {
    if let Some(r) = records.layers.iter_mut().find(|r| r.layer_id == layer_id)
        && r.smart_filters.as_ref().is_some_and(|s| s.filters.is_empty() && s.stack_mask.is_none())
    {
        r.smart_filters = None;
    }
    records.layers.retain(|r| r.layer_id != layer_id || *r != StudioRasterLayerRecord::new(layer_id));
}

/// The first `fx-<n>` the stack does not hold.
fn mint_instance(stack: &StudioSmartFilterStack) -> String {
    let mut n: u64 = 1;
    loop {
        let id = format!("fx-{n}");
        if !stack.filters.iter().any(|f| f.instance_id == id) {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

// ---- adjustments ----------------------------------------------------------------------------------------------

fn adjustment(params: &Map<String, Value>) -> Result<StudioAdjustment, CommandRefusal> {
    let expected = "a Folio StudioAdjustment record {kind: ..., ...}";
    let record = opt_object(params, "adjustment", expected)?.ok_or_else(|| missing("adjustment"))?;
    serde_json::from_value(Value::Object(record)).map_err(|e| CommandRefusal::new("parameter_invalid", json!({"key": "adjustment", "expected": expected, "message": e.to_string()})))
}

fn adjustment_kind(a: &StudioAdjustment) -> String {
    serde_json::to_value(a).ok().and_then(|v| v["kind"].as_str().map(str::to_owned)).unwrap_or_default()
}

fn adjust_refusal(e: AdjustError) -> CommandRefusal {
    match e {
        AdjustError::InvalidParameter(param) => CommandRefusal::new(e.code(), json!({"param": param})),
        AdjustError::Unsupported(feature) => CommandRefusal::new(e.code(), json!({"feature": feature})),
        AdjustError::OutOfDomain => CommandRefusal::new(e.code(), json!({"message": "a colour outside the range the adjustment layer's composite takes (unit, 0..=1)"})),
    }
}

/// The engine's per-pixel form of `a` at the working depth of `stage` (the one an adjustment layer renders with),
/// or its refusal.
fn prepared(a: &StudioAdjustment, stage: OutputStage) -> Result<PreparedAdjustment, CommandRefusal> {
    let op = hsk_studio_composite::lower_extensions::adjustment_op(a, hsk_studio_composite::BlendSpace::LinearLight)
        .map_err(|code| CommandRefusal::new(code, json!({"kind": adjustment_kind(a)})))?;
    op.prepare().map(|p| p.with_eight_bit(stage.is_eight_bit())).map_err(adjust_refusal)
}

/// A new layer's name: at most 128 bytes, no control character, not empty.
fn layer_name(name: Option<String>, fallback: &str) -> Result<String, CommandRefusal> {
    match name {
        None => Ok(fallback.to_owned()),
        Some(n) if !n.is_empty() && n.len() <= 128 && !n.chars().any(char::is_control) => Ok(n),
        Some(_) => Err(invalid("name", "1..=128 bytes without control characters")),
    }
}

// ---- commands -------------------------------------------------------------------------------------------------

/// Runs one `effect.*` or `adjustment.*` row (`Viewer::run_command` checked the declared keys already).
fn run(v: &mut Viewer, id: &str, params: &Map<String, Value>) -> Result<Value, CommandRefusal> {
    match id {
        "effect.parameters" => parameters(opt_text(params, "filter_kind")?.as_deref()),
        "effect.read" => v.effect_read(&text(params, "layer")?, opt_flag(params, "full")?.unwrap_or(false)),
        "effect.apply" => v.effect_apply(params),
        "effect.update" => v.effect_update(params),
        "effect.reorder" => {
            let (layer, instance) = (text(params, "layer")?, text(params, "instance")?);
            let to = opt_index(params, "to")?.ok_or_else(|| missing("to"))?;
            v.edit_raster_records(id, &layer, |records, _| {
                let (stack, at) = entry_mut(records, &layer, &instance)?;
                if to >= stack.filters.len() {
                    return Err(CommandRefusal::new("index_out_of_range", json!({"key": "to", "value": to, "count": stack.filters.len()})));
                }
                if to == at {
                    return Err(nothing());
                }
                let entry = stack.filters.remove(at);
                stack.filters.insert(to, entry);
                Ok(json!({"layer_id": layer, "instance_id": instance, "from": at, "to": to}))
            })
        }
        "effect.toggle" => {
            let (layer, instance) = (text(params, "layer")?, opt_text(params, "instance")?);
            let enabled = opt_flag(params, "enabled")?.ok_or_else(|| missing("enabled"))?;
            v.edit_raster_records(id, &layer, |records, _| {
                let target = match &instance {
                    Some(i) => {
                        let (stack, at) = entry_mut(records, &layer, i)?;
                        &mut stack.filters[at].enabled
                    }
                    None => {
                        let stack = records.layers.iter_mut().find(|r| r.layer_id == layer).and_then(|r| r.smart_filters.as_mut());
                        &mut stack.ok_or_else(|| CommandRefusal::new("effect_instance_unknown", json!({"layer": layer, "message": "the layer has no smart filter stack"})))?.enabled
                    }
                };
                if *target == enabled {
                    return Err(nothing());
                }
                *target = enabled;
                Ok(json!({"layer_id": layer, "instance_id": instance, "enabled": enabled}))
            })
        }
        "effect.remove" => {
            let (layer, instance) = (text(params, "layer")?, text(params, "instance")?);
            v.edit_raster_records(id, &layer, |records, _| {
                let (stack, at) = entry_mut(records, &layer, &instance)?;
                stack.filters.remove(at);
                tidy(records, &layer);
                Ok(json!({"layer_id": layer, "instance_id": instance}))
            })
        }
        "adjustment.create" => {
            let a = adjustment(params)?;
            prepared(&a, OutputStage::default())?;
            let kind = adjustment_kind(&a);
            let name = layer_name(opt_text(params, "name")?, &kind)?;
            let clipped = opt_flag(params, "clipped")?.unwrap_or(false);
            let mut out = v.create_layer_with_record(id, &name, |layer_id| {
                let payload = json!({"payload_kind": "primitive_reference", "schema_id": folio::ADJUSTMENT_SCHEMA, "object_id": layer_id});
                let appearance = clipped.then(|| folio::LayerAppearance { clipped: true, ..folio::LayerAppearance::identity(false) });
                let mut record = StudioRasterLayerRecord::new(layer_id);
                record.adjustment = Some(a);
                Ok(("adjustment", payload, appearance, record))
            })?;
            out["kind"] = json!(kind);
            out["clipped"] = json!(clipped);
            Ok(out)
        }
        "adjustment.edit" => {
            let layer = text(params, "layer")?;
            let a = adjustment(params)?;
            prepared(&a, OutputStage::default())?;
            let kind = adjustment_kind(&a);
            v.edit_raster_records(id, &layer, |records, _| {
                let current = records
                    .layers
                    .iter_mut()
                    .find(|r| r.layer_id == layer)
                    .and_then(|r| r.adjustment.as_mut())
                    .ok_or_else(|| CommandRefusal::new("layer_not_adjustment", json!({"layer": layer})))?;
                if *current == a {
                    return Err(nothing());
                }
                *current = a;
                Ok(json!({"layer_id": layer, "kind": kind}))
            })
        }
        "adjustment.apply" => {
            let layer = text(params, "layer")?;
            let a = adjustment(params)?;
            // Review F7: the composite's own rule: the records envelope's default output stage (range, working
            // depth), Normal blend, coverage 1, through pigment's composite_adjustment_in.
            let stage = OutputStage::default();
            let f = prepared(&a, stage)?;
            let mut out = v.map_layer_pixels(id, &layer, |rgb, alpha| {
                composite_adjustment_in(stage.sample_range, StudioBlendMode::Normal, Rgba { rgb, a: alpha }, &f, 1.0)
                    .map(|c| c.rgb)
                    .map_err(|e| match e {
                        hsk_studio_pigment::adjust_math::AdjustLayerError::Adjust(e) => adjust_refusal(e),
                        hsk_studio_pigment::adjust_math::AdjustLayerError::Blend(e) => CommandRefusal::new("adjust_blend", json!({"message": format!("{e:?}")})),
                    })
            })?;
            out["kind"] = json!(adjustment_kind(&a));
            Ok(out)
        }
        other => Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    }
}

impl Viewer {
    /// The `effect.*` rows (module docs; the method their rows name). Parameters as `Viewer::run_command` passes
    /// them after its declared-key check, which a direct caller does not get.
    pub fn effect_command(&mut self, command_id: &str, params: &Map<String, Value>) -> Result<Value, CommandRefusal> {
        if !command_id.starts_with("effect.") {
            return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": command_id})));
        }
        run(self, command_id, params)
    }

    /// The `adjustment.*` rows (module docs; the method their rows name), as [`Viewer::effect_command`].
    pub fn adjustment_command(&mut self, command_id: &str, params: &Map<String, Value>) -> Result<Value, CommandRefusal> {
        if !command_id.starts_with("adjustment.") {
            return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": command_id})));
        }
        run(self, command_id, params)
    }

    /// `effect.read` (module docs).
    pub fn effect_read(&self, layer_id: &str, full: bool) -> Result<Value, CommandRefusal> {
        let doc = self.run.snapshot().map(|s| s.document().clone()).ok_or_else(|| CommandRefusal::new("not_open", json!({})))?;
        if !doc.layers.iter().any(|l| l.layer_id == layer_id) {
            return Err(CommandRefusal::new("layer_unknown", json!({"layer": layer_id})));
        }
        let bounds = self.layer_content_bounds(layer_id).map(rect_json);
        let Some(stack) = self.editable_raster_record(layer_id)?.and_then(|r| r.smart_filters) else {
            return Ok(json!({"layer_id": layer_id, "content_bounds": bounds, "enabled": Value::Null, "has_stack_mask": false, "filters": []}));
        };
        let filters: Vec<Value> = stack
            .filters
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let (kind, params) = kind_params(&f.params, full);
                let soft = out_of_soft_range(&kind, &params);
                json!({"index": i, "instance_id": f.instance_id, "filter_kind": kind, "enabled": f.enabled, "opacity": f.opacity, "has_mask": f.mask.is_some(), "params": params, "out_of_soft_range": soft})
            })
            .collect();
        Ok(json!({"layer_id": layer_id, "content_bounds": bounds, "enabled": stack.enabled, "has_stack_mask": stack.stack_mask.is_some(), "filters": filters}))
    }

    fn effect_apply(&mut self, params: &Map<String, Value>) -> Result<Value, CommandRefusal> {
        const OP: &str = "effect.apply";
        let layer = text(params, "layer")?;
        let kind = text(params, "filter_kind")?;
        let given_params = opt_object(params, "params", "an object of the kind's parameters")?.unwrap_or_default();
        let opacity = opacity(opt_number(params, "opacity")?.unwrap_or(100.0))?;
        let enabled = opt_flag(params, "enabled")?.unwrap_or(true);
        let at = opt_index(params, "at")?;
        filter_spec(&kind).ok_or_else(|| unknown_kind(&kind))?;
        let content = self.layer_content(&layer)?;
        let (block, defaulted) = decode_kind(&kind, &given_params, content.as_ref().and_then(|c| c.bounds))?;
        admit(&layer, &engine_op(&block)?, &kind, content.as_ref())?;
        drop(content);
        self.edit_raster_records(OP, &layer, |records, doc| {
            raster_layer(doc, &layer)?;
            let record = record_mut(records, &layer);
            let stack = record.smart_filters.get_or_insert_with(|| StudioSmartFilterStack { enabled: true, stack_mask: None, filters: Vec::new() });
            if stack.filters.len() >= MAX_SMART_FILTERS {
                return Err(CommandRefusal::new("effect_stack_full", json!({"layer": layer, "at_most": MAX_SMART_FILTERS})));
            }
            let index = at.unwrap_or(stack.filters.len());
            if index > stack.filters.len() {
                return Err(CommandRefusal::new("index_out_of_range", json!({"key": "at", "value": index, "count": stack.filters.len()})));
            }
            let instance_id = mint_instance(stack);
            stack.filters.insert(index, StudioSmartFilter { instance_id: instance_id.clone(), enabled, opacity, mask: None, params: block });
            Ok(json!({"layer_id": layer, "instance_id": instance_id, "filter_kind": kind, "index": index, "defaulted": defaulted}))
        })
    }

    fn effect_update(&mut self, params: &Map<String, Value>) -> Result<Value, CommandRefusal> {
        const OP: &str = "effect.update";
        let layer = text(params, "layer")?;
        let instance = text(params, "instance")?;
        let changes = opt_object(params, "params", "an object of the kind's parameters")?.filter(|c| !c.is_empty());
        let opacity = opt_number(params, "opacity")?.map(opacity).transpose()?;
        if changes.is_none() && opacity.is_none() {
            return Err(nothing());
        }
        // The new kind block, decoded and admitted before the edit runs. Review F9: no decode (no mesh copy) for an
        // opacity-only update; a liquify update without a new mesh keeps the stored mesh as it is.
        let block = match &changes {
            None => None,
            Some(changes) => {
                let current = self
                    .editable_raster_record(&layer)?
                    .and_then(|r| r.smart_filters)
                    .and_then(|s| s.filters.into_iter().find(|f| f.instance_id == instance))
                    .ok_or_else(|| instance_unknown(&layer, &instance))?;
                let content = self.layer_content(&layer)?;
                let block = match current.params {
                    StudioSmartFilterKind::Liquify { mesh, resample } if !changes.contains_key("mesh") => {
                        let spec = filter_spec("liquify").ok_or_else(|| unknown_kind("liquify"))?;
                        if let Some(k) = changes.keys().find(|k| k.as_str() != "resample") {
                            return Err(CommandRefusal::new("parameter_unknown", json!({"key": format!("params.{k}"), "filter_kind": "liquify"})));
                        }
                        let resample = match (changes.get("resample"), spec.params.iter().find(|p| p.key == "resample")) {
                            (Some(v), Some(p)) => serde_json::from_value(check_param("liquify", p, v)?).map_err(|_| invalid("params.resample", "a resample token"))?,
                            _ => resample,
                        };
                        StudioSmartFilterKind::Liquify { mesh, resample }
                    }
                    other => {
                        let (kind, mut merged) = kind_params(&other, true);
                        for (k, value) in changes {
                            merged.insert(k.clone(), value.clone());
                        }
                        let (block, _) = decode_kind(&kind, &merged, content.as_ref().and_then(|c| c.bounds))?;
                        admit(&layer, &engine_op(&block)?, &kind, content.as_ref())?;
                        block
                    }
                };
                Some(block)
            }
        };
        let changed: Vec<String> = changes.iter().flat_map(|c| c.keys().cloned()).collect();
        self.edit_raster_records(OP, &layer, |records, _| {
            let (stack, at) = entry_mut(records, &layer, &instance)?;
            let entry = &mut stack.filters[at];
            let before_opacity = entry.opacity;
            let mut differs = false;
            if let Some(block) = block {
                differs |= entry.params != block;
                entry.params = block;
            }
            if let Some(o) = opacity {
                differs |= before_opacity != o;
                entry.opacity = o;
            }
            if !differs {
                return Err(nothing());
            }
            let (kind, _) = kind_params(&entry.params, false);
            Ok(json!({"layer_id": layer, "instance_id": instance, "filter_kind": kind, "changed": changed}))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A kind block with `key` set to `value` over the kind's declared defaults (or 1 where a default is unknown).
    fn probe(spec: &FilterSpec, key: &str, value: f64) -> Option<Map<String, Value>> {
        let mut m = Map::new();
        for p in spec.params {
            let v = if p.key == key {
                match p.ty {
                    ParamType::Float => json!(value),
                    ParamType::Int(_) => json!(value as i64),
                    _ => return None,
                }
            } else {
                match (p.ty, default_json(p, Some(Rect { x: 0, y: 0, width: 8, height: 8 }))) {
                    (_, Some(d)) => d,
                    (ParamType::Float | ParamType::Int(_), None) => json!(1),
                    (ParamType::Bool, None) => json!(false),
                    (ParamType::Enum(o), None) => json!(o[0].0),
                    (ParamType::Weights, None) => json!(vec![0; 25]),
                    (ParamType::Mesh, None) => return None,
                }
            };
            m.insert(p.key.into(), v);
        }
        Some(m)
    }

    /// Pigment accepts the kind block (the engine's own check, bypassing the table).
    fn engine_accepts(kind: &str, m: &Map<String, Value>) -> bool {
        let mut block = m.clone();
        block.insert("filter_kind".into(), json!(kind));
        let Ok(parsed) = serde_json::from_value::<StudioSmartFilterKind>(Value::Object(block)) else { return false };
        engine_op(&parsed).is_ok()
    }

    /// Every known hard bound of the table is the engine's: the bound itself (or, for an open bound, just inside
    /// it) passes pigment, and just outside it pigment refuses, so the table never accepts what the engine refuses
    /// nor refuses what it accepts at the edges (declared bounds included: pigment cites the same tables).
    #[test]
    fn table_hard_bounds_are_the_engines() {
        let mut checked: u32 = 0;
        for spec in FILTER_SPECS {
            for p in spec.params.iter().filter(|p| numeric(p.ty).is_some()) {
                for (side, b) in p.hard.iter().enumerate() {
                    let Some(edge) = b.value else { continue };
                    // One integer, or a float step above the f32 resolution at the edge.
                    let step = if matches!(p.ty, ParamType::Int(_)) { 1.0 } else { 1e-3_f64.max(edge.abs() * 1e-5) };
                    let (inside, outside) = match (side, b.open) {
                        (0, false) => (edge, edge - step),
                        (0, true) => (edge + step, edge),
                        _ => (edge, edge + step),
                    };
                    let (lo, hi) = match p.ty {
                        ParamType::Int(t) => int_domain(t),
                        _ => (i128::MIN, i128::MAX),
                    };
                    let representable = |v: f64| !matches!(p.ty, ParamType::Int(_)) || ((lo as f64)..=(hi as f64)).contains(&v);
                    let Some(m) = probe(spec, p.key, inside) else { continue };
                    assert!(engine_accepts(spec.kind, &m), "{}.{} = {inside} should pass pigment", spec.kind, p.key);
                    assert!(within_hard(p, inside), "{}.{} = {inside} should pass the table", spec.kind, p.key);
                    if representable(outside) {
                        let m = probe(spec, p.key, outside).unwrap();
                        assert!(!engine_accepts(spec.kind, &m), "{}.{} = {outside} should fail pigment", spec.kind, p.key);
                        assert!(!within_hard(p, outside), "{}.{} = {outside} should fail the table", spec.kind, p.key);
                    }
                    checked = checked.saturating_add(1);
                }
            }
        }
        assert!(checked >= 30, "{checked} bounds checked");
    }

    #[test]
    fn the_shipped_table_passes_the_contract_check_and_covers_every_folio_kind() {
        let all = parameters(None).unwrap();
        assert_eq!(all["findings"], json!([]), "{}", all["findings"]);
        assert_eq!(FILTER_SPECS.len(), 25);
        // Every Folio kind tag decodes (parameterless kinds with no params; the rest from probe values).
        for spec in FILTER_SPECS {
            if spec.params.iter().any(|p| p.ty == ParamType::Mesh) {
                continue;
            }
            let m = probe(spec, "", 0.0).unwrap_or_default();
            let mut block = m.clone();
            block.insert("filter_kind".into(), json!(spec.kind));
            assert!(serde_json::from_value::<StudioSmartFilterKind>(Value::Object(block)).is_ok(), "{} decodes", spec.kind);
        }
        for p in FILTER_SPECS.iter().flat_map(|s| s.params) {
            assert!(p.unit.is_none_or(|u| UNITS.contains(&u)), "{}", p.key);
        }
        // STU-RAS-109: every enumeration member has an integer discriminant.
        for p in FILTER_SPECS.iter().flat_map(|s| s.params) {
            if let ParamType::Enum(members) = p.ty {
                let mut d: Vec<u16> = members.iter().map(|m| m.1).collect();
                d.dedup();
                assert_eq!(d.len(), members.len(), "{}: distinct discriminants", p.key);
            }
        }
    }

    #[test]
    fn the_contract_check_finds_each_defect_class() {
        let good = param_spec_json(0, &FILTER_SPECS[6].params[0]);
        assert!(parameter_contract_findings(&good).is_empty(), "{good}");
        let codes = |spec: &Value| parameter_contract_findings(spec).iter().map(|f| f["code"].as_str().unwrap().to_owned()).collect::<Vec<_>>();
        let mut missing_field = good.clone();
        missing_field.as_object_mut().unwrap().remove("soft_max");
        assert!(codes(&missing_field).contains(&"field_missing:soft_max".to_owned()));
        let mut equated = good.clone();
        equated["soft_max"] = good["hard_max"].clone();
        equated["bound_classification"]["soft_max"] = json!("derived");
        assert!(codes(&equated).contains(&"hard_soft_equated".to_owned()), "{:?}", codes(&equated));
        let mut invented = good.clone();
        invented["bound_classification"]["hard_max"] = json!("unknown");
        assert!(codes(&invented).contains(&"classification_mismatch:hard_max".to_owned()));
        let mut observed = good.clone();
        observed["observed_max"] = json!(30_000.0);
        observed["bound_classification"]["hard_max"] = json!("observed");
        let found = codes(&observed);
        assert!(found.contains(&"hard_bound_observed:hard_max".to_owned()) && found.contains(&"observed_used_as_bound:hard_max".to_owned()), "{found:?}");
        let mut unit = good.clone();
        unit["unit"] = json!("px");
        assert!(codes(&unit).contains(&"unit_not_in_vocabulary".to_owned()));
        let mut step = good.clone();
        step["fine_step"] = json!(0.01);
        assert!(codes(&step).contains(&"step_disagrees".to_owned()));
        let mut default = good;
        default["default"] = json!(-1.0);
        assert!(codes(&default).contains(&"default_outside_hard".to_owned()));
        // STU-RAS-109: an enumeration member without an integer discriminant.
        let mut noise = param_spec_json(1, &FILTER_SPECS[4].params[1]);
        assert!(parameter_contract_findings(&noise).is_empty(), "{noise}");
        noise["options"][1]["discriminant"] = json!("unknown");
        assert_eq!(codes(&noise), vec!["discriminant_missing:gaussian".to_owned()]);
    }

    #[test]
    fn the_input_schema_carries_known_hard_bounds_only() {
        let box_blur = kind_input_schema(filter_spec("box_blur").unwrap());
        let radius = &box_blur["properties"]["radius_px"];
        assert_eq!((radius["minimum"].as_f64(), radius["maximum"].as_f64(), radius["x-soft-max"].as_f64()), (Some(0.0), Some(30_000.0), Some(127.0)), "{radius}");
        assert_eq!(box_blur["required"], json!([]), "every box blur parameter has a declared default");
        let angle = &kind_input_schema(filter_spec("motion_blur").unwrap())["properties"]["angle_deg"];
        assert!(angle.get("minimum").is_none() && angle.get("maximum").is_none(), "no fabricated bound for unknown: {angle}");
        let sigma = &kind_input_schema(filter_spec("bilateral_blur").unwrap())["properties"]["range_sigma"];
        assert_eq!(sigma["exclusiveMinimum"].as_f64(), Some(0.0), "{sigma}");
        let scale = &kind_input_schema(filter_spec("custom_convolution").unwrap())["properties"]["scale"];
        assert_eq!(scale["not"], json!({"const": 0}));
        let centre = &kind_input_schema(filter_spec("radial_blur").unwrap())["properties"]["centre_x_px"];
        assert_eq!(centre["x-default"]["fraction_of_layer_content_bounds"].as_f64(), Some(0.5), "{centre}");
    }

    /// Review F9: the vertex-list cap ([`mesh_cap`] in `check_param`, here a small cap; the harness test
    /// `a_large_liquify_mesh_is_summarised_and_kept_by_updates` runs the real one through the row).
    #[test]
    fn a_mesh_list_over_the_cap_is_refused_before_decoding() {
        let mesh = |n: usize| json!({"displacement": vec![[0.0, 0.0]; n], "freeze": vec![0.0; 3]});
        let m = mesh(3);
        assert!(mesh_lists_within("params.mesh", m.as_object().unwrap(), 3).is_ok());
        let r = mesh_lists_within("params.mesh", mesh(4).as_object().unwrap(), 3).unwrap_err();
        assert_eq!((r.code.as_str(), &r.detail), ("budget_exceeded", &json!({"key": "params.mesh.displacement", "at_most": 3})));
        let freeze = json!({"displacement": [], "freeze": vec![0.0; 4]});
        assert_eq!(mesh_lists_within("params.mesh", freeze.as_object().unwrap(), 3).unwrap_err().detail["key"], "params.mesh.freeze");
        // The real cap (`check_param` passes it): Folio's node budget of the records validation, below its vertex cap.
        assert_eq!(mesh_cap(), folio::Budget::default().nodes);
        assert!(mesh_cap() <= usize::try_from(MAX_LIQUIFY_VERTICES).unwrap());
    }
}
