//! Smart filter effects and adjustment layers as typed commands (lane SMB-PS-FILTER-ADJUST-COMMANDS-1; [STU-FX-007],
//! [STU-FX-012c], [STU-FX-103]..[STU-FX-106], [STU-FX-123], [STU-RAS-035], [STU-RAS-037], [STU-RAS-103]..[STU-RAS-112]).
//! egui-free and drawing nothing: the rows `effect.*` and `adjustment.*` of `action::commands::photo` reach [`run`]
//! through `Viewer::run_command`.
//!
//! # Manual (no-context operator or model)
//!
//! A raster layer's smart filter stack is the Folio `StudioSmartFilterStack` of its raster record in the session
//! entry `raster_records` (module `place`); composite renders it live (pigment `SmartFilterStack`), the layer's
//! pixels stay as they are, and save, reopen and export carry it. Every mutating command is ONE undo step named by
//! its command id and attributed to the viewer's run context (the caller's); a refusal is a typed code and changes
//! nothing.
//!
//! - `effect.parameters {filter_kind?}` (no document needed): the parameter specs of every kind, or one, without
//!   running anything ([STU-RAS-110]). A numeric parameter carries the [STU-RAS-103] fields as separate values:
//!   `hard_min`, `hard_max` (what the engine accepts; outside is a refusal, never a clamp), `soft_min`, `soft_max`
//!   (what a control presents; a value beyond them inside the hard range is legal, [STU-FX-105]), `default`, `unit`
//!   (a [STU-RAS-105] token), `precision`, `step`, `coarse_step`, `fine_step` (the [STU-RAS-108] derivation),
//!   `observed_min`, `observed_max` (evidence only, never a bound, [STU-RAS-107]; `absent` for every kind here) and
//!   `bound_classification` (`declared`, `observed`, `derived`, `unknown` per value, [STU-RAS-112]). A value no
//!   source declared is the token `unknown` ([STU-RAS-106]): nothing is invented, clamped or narrowed to an
//!   observation. `declared` values come from the 14.9.7 effect parameter tables the kernels cite (Fast Box Blur,
//!   Directional Blur, Radial Blur, Solarize, Mosaic, Minimax, Dust & Scratches); `derived` hard bounds are the
//!   engine's own (pigment `filter_math` Studio readings, a type's domain, a mathematical domain such as
//!   `> 0`, flagged `hard_min_exclusive`). The result also carries `findings` of the `STUDIO_EFFECT_PARAM_CONTRACT`
//!   check ([STU-FX-123], [STU-RAS-111]; [`parameter_contract_findings`]) run over every spec it lists: empty for
//!   the shipped table.
//! - `effect.apply {layer, filter_kind, params?, opacity?, enabled?, at?}` (`STUDIO_EFFECT_APPLY`): adds an entry
//!   to the stack of raster layer `layer` (the stack is created when absent) at render position `at` (default:
//!   last, applied last). Each parameter of the kind is checked here against its spec: an unknown key is
//!   `parameter_unknown`, a wrong type `parameter_invalid`, a value outside the hard bounds
//!   `effect_param_out_of_range` naming the parameter, the value and both hard bounds ([STU-FX-104]); an omitted
//!   parameter takes its declared default, and one whose default is `unknown` is `parameter_missing`. `opacity`
//!   is percent `0..=100` (default 100). The entry then passes Folio's record validation and pigment's own
//!   (`SmartFilterOp::check`). Returns `{layer_id, instance_id, filter_kind, index, defaulted, revision}`.
//! - `effect.update {layer, instance, params?, opacity?}` (`STUDIO_EFFECT_UPDATE`): the given parameters over the
//!   entry's current ones, checked as above; the kind stays.
//! - `effect.reorder {layer, instance, to}` (`STUDIO_EFFECT_REORDER`), `effect.toggle {layer, instance?, enabled}`
//!   (`STUDIO_EFFECT_TOGGLE`; without `instance`: the stack's master switch), `effect.remove {layer, instance}`
//!   (`STUDIO_EFFECT_REMOVE`; the last entry removes the stack when it has no stack mask).
//! - `effect.read {layer}`: the stack: each entry's kind, parameters, opacity, enable, whether it holds a mask, and
//!   `out_of_soft_range` (the parameters whose value lies outside a known soft range, [STU-FX-105]).
//! - `adjustment.create {adjustment, name?, clipped?}`: a new `adjustment` layer above the selected layer holding
//!   the Folio `StudioAdjustment` record (tag `kind`, [STU-RAS-151]; payload `hsk.studio.adjustment@1` naming its
//!   own id), clipped to the layer below when `clipped`; layer and record are one step.
//! - `adjustment.edit {layer, adjustment}`: the adjustment layer's record replaced (any kind: Layer > Change Layer
//!   Content); re-editable parameters ([STU-RAS-035]).
//! - `adjustment.apply {layer, adjustment}`: the explicit destructive apply-in-place form ([STU-RAS-035]): the pixel
//!   layer's colour becomes `f(colour)` per texel with alpha > 0 (alpha kept), the same per-pixel function an
//!   adjustment layer applies to its backdrop (pigment `PreparedAdjustment::apply`, float working depth), so an
//!   adjustment layer over a lone pixel layer and the in-place form give the same frame. A layer holding values
//!   outside 0..=1 takes only the kinds pigment defines there (exposure, `apply_in` extended); the others refuse
//!   `adjust_unsupported`. A placed asset layer refuses `layer_not_pixel` (rasterize it first).
//!
//! An adjustment record is checked by Folio (structure, declared bounds) and by the engine before anything is
//! written: composite's conversion (`lower_extensions::adjustment_op`, its codes such as `colour_lookup_unresolved`)
//! and pigment's `prepare` (`adjust_invalid_parameter` naming the parameter, `adjust_unsupported`).
//!
//! Refusal codes: `not_open`, `canvas_unavailable`, `layer_unknown`, `layer_not_pixel`, `layer_not_adjustment`,
//! `layer_empty`, `tile_format_unsupported`, `effect_instance_unknown`, `effect_stack_full` (Folio's
//! `MAX_SMART_FILTERS`), `index_out_of_range`, `nothing_to_change`, `parameter_unknown`, `parameter_missing`,
//! `parameter_invalid`, `parameter_out_of_range` (opacity), `effect_param_out_of_range`, pigment's
//! `filter_parameter`, composite's and pigment's adjustment codes, `folio_<code>`, `budget_exceeded`, `canceled`
//! and the run's own codes.
//!
//! Cuts (named, not silent): per-filter and stack masks (`STUDIO_EFFECT_MASK_EDIT`), selection-scoped apply,
//! destructive filter forms and bake (`STUDIO_EFFECT_BAKE`) are the next slice; the sandbox ->
//! `StudioValidationDescriptor` -> `PromotionGate` route of [STU-FX-007] is not built (an edit commits through the
//! session's chronicle provider after the checks here); adjustment presets and default-valued one-click creation
//! (most adjustment defaults are `unknown`, [STU-RAS-153]); the `ocio`, `normals`, `tone_stretch`, `desaturate`,
//! `equalize` and auto kinds have no Folio `StudioAdjustment` member yet; adjustment parameter specs (the
//! [STU-RAS-151] table) are not emitted, and pigment's adjustment ranges are narrower than the spec's `unknown`
//! hard bounds on several kinds (vibrance, colour balance, photo filter density, ...): a pigment defect, named.
//! Photoshop parity of every kernel and default is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::commands::CommandRefusal;
use hsk_studio_folio::{
    self as folio, MAX_LIQUIFY_VERTICES, MAX_SMART_FILTERS, StudioAdjustment, StudioRasterLayerRecord, StudioRasterRecords, StudioSmartFilter,
    StudioSmartFilterKind, StudioSmartFilterStack,
};
use hsk_studio_pigment::adjust_math::{AdjustError, PreparedAdjustment};
use hsk_studio_pigment::render_plan::SampleRange;
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

/// A parameter's factory value ([STU-FX-103] `default`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DefaultValue {
    Number(f64, Classification),
    Token(&'static str, Classification),
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

/// What a parameter holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamType {
    Float,
    Int(Int),
    Bool,
    /// Tokens with their [STU-RAS-109] integer discriminants where a source declares them.
    Enum(&'static [(&'static str, Option<u16>)]),
    /// The 25 `i32` weights of a 5x5 convolution, row-major.
    Weights,
    /// A liquify mesh (Folio `StudioLiquifyMesh`).
    Mesh,
}

/// One parameter of a smart filter kind ([STU-FX-103] / [STU-RAS-103]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EffectParam {
    pub key: &'static str,
    pub ty: ParamType,
    pub hard: [Bound; 2],
    pub soft: [Bound; 2],
    pub default: DefaultValue,
    /// A [STU-RAS-105] token; `None` = `unknown`.
    pub unit: Option<&'static str>,
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
    EffectParam { key, ty, hard, soft, default, unit, precision, observed: None, nonzero: false }
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
const PX: Option<&str> = Some("pixels");
const PCT: Option<&str> = Some("percent");
const DEG: Option<&str> = Some("degrees");
const LEVELS: Option<&str> = Some("levels_0_255");
const COUNT: Option<&str> = Some("count");
const NO_UNIT: Option<&str> = None;

const DIMENSIONS: &[(&str, Option<u16>)] = &[("horizontal_and_vertical", Some(1)), ("horizontal", Some(2)), ("vertical", Some(3))];
const RADIAL: &[(&str, Option<u16>)] = &[("spin", Some(1)), ("zoom", Some(2))];
const NOISE: &[(&str, Option<u16>)] = &[("uniform", None), ("gaussian", None)];
const PRESERVE: &[(&str, Option<u16>)] = &[("squareness", None), ("roundness", None)];
/// [STU-RAS-141] resample members 2..=4.
const RESAMPLE: &[(&str, Option<u16>)] = &[("nearest_neighbour", Some(2)), ("bilinear", Some(3)), ("bicubic", Some(4))];

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
        source: "Folio StudioLiquifyMesh and pigment LiquifyMesh::validate (structure, not a range)",
        params: &[other("mesh", ParamType::Mesh, DefaultValue::Unknown), other("resample", ParamType::Enum(RESAMPLE), DefaultValue::Unknown)],
    },
    FilterSpec {
        kind: "box_blur",
        source: "14.9.7 Fast Box Blur table (ADBE Box Blur2)",
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
        source: "14.9.7 Radial Blur table (ADBE Radial Blur); its centre default (50 % of the layer) has no pixel value here",
        params: &[
            engine("centre_x_px", [UNK, UNK], PX),
            engine("centre_y_px", [UNK, UNK], PX),
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
        source: "14.9.7 Dust & Scratches table (soft equal to hard as the table declares; threshold unit `pixel` as declared)",
        params: &[
            p("radius_px", ParamType::Int(Int::U32), [dec(0.0), dec(255.0)], [dec(0.0), dec(255.0)], DefaultValue::Number(1.0, D), PX, Some(0)),
            p("threshold", ParamType::Float, [dec(0.0), dec(1.0)], [dec(0.0), dec(1.0)], DefaultValue::Number(0.0, D), PX, Some(1)),
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

/// One parameter's spec record as the command surface shows it.
pub fn param_spec_json(index: usize, p: &EffectParam) -> Value {
    let default = match p.default {
        DefaultValue::Number(v, _) => number_json(p.ty, v),
        DefaultValue::Token(t, _) => json!(t),
        DefaultValue::Unknown => json!(UNKNOWN),
    };
    let default_class = match p.default {
        DefaultValue::Number(_, c) | DefaultValue::Token(_, c) => c,
        DefaultValue::Unknown => Classification::Unknown,
    };
    let Some(kind) = numeric(p.ty) else {
        let mut spec = json!({"param_key": p.key, "param_index": index, "default": default, "default_classification": default_class.token()});
        match p.ty {
            ParamType::Bool => spec["kind"] = json!("boolean"),
            ParamType::Enum(options) => {
                spec["kind"] = json!("enumeration");
                spec["options"] = Value::Array(options.iter().map(|(t, d)| json!({"token": t, "discriminant": d.map_or_else(|| json!(UNKNOWN), |d| json!(d))})).collect());
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
    spec
}

/// The `STUDIO_EFFECT_PARAM_CONTRACT` check ([STU-FX-123], [STU-RAS-111]) over one numeric spec record (a
/// non-numeric record passes when an enumeration lists its options). Findings, each `{param_key, code}`:
/// `field_missing:<field>` (a [STU-RAS-103] field absent), `classification_missing:<bound>`,
/// `classification_mismatch:<bound>` (a number classified `unknown`, or `unknown` classified otherwise),
/// `hard_soft_equated` (soft equal to hard without `declared` on all four, [STU-RAS-104]),
/// `hard_bound_observed:<bound>` (a hard bound classified `observed`: a clamp to an observation, [STU-RAS-107]),
/// `observed_used_as_bound:<bound>` (a bound equal to its observed value without `declared`), `unit_not_in_vocabulary`,
/// `step_disagrees` (not the [STU-RAS-108] derivation), `default_outside_hard`, `options_missing`.
pub fn parameter_contract_findings(spec: &Value) -> Vec<Value> {
    let key = spec["param_key"].as_str().unwrap_or("");
    let mut out = Vec::new();
    let mut find = |code: String| out.push(json!({"param_key": key, "code": code}));
    match spec["kind"].as_str() {
        Some("number" | "integer") => {}
        Some("enumeration") => {
            if spec["options"].as_array().is_none_or(Vec::is_empty) {
                find("options_missing".into());
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

/// `effect.parameters`: the specs of `kind` (or every kind) and the contract findings over them.
pub fn parameters(kind: Option<&str>) -> Result<Value, CommandRefusal> {
    let specs: Vec<&FilterSpec> = match kind {
        Some(k) => vec![filter_spec(k).ok_or_else(|| unknown_kind(k))?],
        None => FILTER_SPECS.iter().collect(),
    };
    let mut findings = Vec::new();
    let mut kinds = Vec::with_capacity(specs.len());
    for s in specs {
        let params: Vec<Value> = s.params.iter().enumerate().map(|(i, p)| param_spec_json(i, p)).collect();
        for spec in &params {
            for f in parameter_contract_findings(spec) {
                findings.push(json!({"filter_kind": s.kind, "finding": f}));
            }
        }
        kinds.push(json!({"filter_kind": s.kind, "source": s.source, "parameters": params}));
    }
    Ok(json!({"schema_id": PARAMETER_SPEC_SCHEMA, "check": PARAM_CONTRACT_CHECK, "findings": findings, "kinds": kinds}))
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
            if !within_hard(p, n) {
                return Err(out_of_range(kind, p, v));
            }
            if n.abs() > f64::from(f32::MAX) {
                return Err(invalid(&key, "a number a 32-bit float holds"));
            }
            Ok(json!(n))
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
            let tokens: Vec<&str> = options.iter().map(|(t, _)| *t).collect();
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
            // Hostile size: the vertex lists are refused before Folio decodes them (V4-02).
            let cap = usize::try_from(MAX_LIQUIFY_VERTICES).unwrap_or(usize::MAX);
            for list in ["displacement", "freeze"] {
                if m.get(list).and_then(Value::as_array).is_some_and(|a| a.len() > cap) {
                    return Err(CommandRefusal::new("budget_exceeded", json!({"key": format!("{key}.{list}"), "at_most": MAX_LIQUIFY_VERTICES})));
                }
            }
            Ok(v.clone())
        }
    }
}

/// A declared default as the parameter's JSON value (an integer parameter's as an integer).
fn default_json(p: &EffectParam) -> Option<Value> {
    match p.default {
        DefaultValue::Number(n, _) => Some(number_json(p.ty, n)),
        DefaultValue::Token(t, _) => Some(json!(t)),
        DefaultValue::Unknown => None,
    }
}

/// The Folio kind block of `kind` from `given` parameters (each checked), and the keys that took their declared
/// default. Pigment then checks the entry as it will render it (`SmartFilterOp::check`).
fn decode_kind(kind: &str, given_params: &Map<String, Value>) -> Result<(StudioSmartFilterKind, Vec<&'static str>), CommandRefusal> {
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
                let d = default_json(p).ok_or_else(|| CommandRefusal::new("parameter_missing", json!({"key": format!("params.{}", p.key), "filter_kind": kind, "default": UNKNOWN})))?;
                defaulted.push(p.key);
                d
            }
        };
        block.insert(p.key.into(), check_param(kind, p, &value)?);
    }
    let parsed: StudioSmartFilterKind =
        serde_json::from_value(Value::Object(block)).map_err(|e| CommandRefusal::new("parameter_invalid", json!({"key": "params", "filter_kind": kind, "message": e.to_string()})))?;
    let probe = StudioSmartFilterStack {
        enabled: true,
        stack_mask: None,
        filters: vec![StudioSmartFilter { instance_id: "probe".into(), enabled: true, opacity: 100.0, mask: None, params: parsed.clone() }],
    };
    let accepted = hsk_studio_pigment::smart_filter::SmartFilterOp::from_record(&probe, |_| None).is_ok_and(|op| op.check());
    if !accepted {
        return Err(CommandRefusal::new("filter_parameter", json!({"filter_kind": kind, "message": "the engine refused the entry (pigment SmartFilterOp::check)"})));
    }
    Ok((parsed, defaulted))
}

/// The kind tag and the parameters (without the tag) of a Folio kind block.
fn kind_params(kind: &StudioSmartFilterKind) -> (String, Map<String, Value>) {
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

fn percent(key: &str, v: f64) -> Result<f64, CommandRefusal> {
    if (0.0..=100.0).contains(&v) { Ok(v) } else { Err(CommandRefusal::new("parameter_out_of_range", json!({"key": key, "value": v, "min": 0, "max": 100}))) }
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
        AdjustError::OutOfDomain => CommandRefusal::new(e.code(), json!({})),
    }
}

/// The engine's per-pixel form of `a` (the one an adjustment layer renders with), or its refusal.
fn prepared(a: &StudioAdjustment) -> Result<PreparedAdjustment, CommandRefusal> {
    let op = hsk_studio_composite::lower_extensions::adjustment_op(a, hsk_studio_composite::BlendSpace::LinearLight)
        .map_err(|code| CommandRefusal::new(code, json!({"kind": adjustment_kind(a)})))?;
    op.prepare().map_err(adjust_refusal)
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
        "effect.read" => v.effect_read(&text(params, "layer")?),
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
            prepared(&a)?;
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
            prepared(&a)?;
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
            let f = prepared(&a)?;
            let mut out = v.map_layer_pixels(id, &layer, |extended, rgb| {
                let range = if extended { SampleRange::Extended } else { SampleRange::Unit };
                f.apply_in(range, rgb).map_err(adjust_refusal)
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
    pub fn effect_read(&self, layer_id: &str) -> Result<Value, CommandRefusal> {
        let doc = self.run.snapshot().map(|s| s.document().clone()).ok_or_else(|| CommandRefusal::new("not_open", json!({})))?;
        if !doc.layers.iter().any(|l| l.layer_id == layer_id) {
            return Err(CommandRefusal::new("layer_unknown", json!({"layer": layer_id})));
        }
        let Some(stack) = self.editable_raster_record(layer_id)?.and_then(|r| r.smart_filters) else {
            return Ok(json!({"layer_id": layer_id, "enabled": Value::Null, "has_stack_mask": false, "filters": []}));
        };
        let filters: Vec<Value> = stack
            .filters
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let (kind, params) = kind_params(&f.params);
                let soft = out_of_soft_range(&kind, &params);
                json!({"index": i, "instance_id": f.instance_id, "filter_kind": kind, "enabled": f.enabled, "opacity": f.opacity, "has_mask": f.mask.is_some(), "params": params, "out_of_soft_range": soft})
            })
            .collect();
        Ok(json!({"layer_id": layer_id, "enabled": stack.enabled, "has_stack_mask": stack.stack_mask.is_some(), "filters": filters}))
    }

    fn effect_apply(&mut self, params: &Map<String, Value>) -> Result<Value, CommandRefusal> {
        const OP: &str = "effect.apply";
        let layer = text(params, "layer")?;
        let kind = text(params, "filter_kind")?;
        let given_params = opt_object(params, "params", "an object of the kind's parameters")?.unwrap_or_default();
        let opacity = percent("opacity", opt_number(params, "opacity")?.unwrap_or(100.0))?;
        let enabled = opt_flag(params, "enabled")?.unwrap_or(true);
        let at = opt_index(params, "at")?;
        let (block, defaulted) = decode_kind(&kind, &given_params)?;
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
        let changes = opt_object(params, "params", "an object of the kind's parameters")?;
        let opacity = opt_number(params, "opacity")?.map(|o| percent("opacity", o)).transpose()?;
        if changes.as_ref().is_none_or(Map::is_empty) && opacity.is_none() {
            return Err(nothing());
        }
        // The current entry (read first, so the merged parameters are checked before the edit runs).
        let current = self
            .editable_raster_record(&layer)?
            .and_then(|r| r.smart_filters)
            .and_then(|s| s.filters.into_iter().find(|f| f.instance_id == instance))
            .ok_or_else(|| instance_unknown(&layer, &instance))?;
        let (kind, mut merged) = kind_params(&current.params);
        let changed: Vec<String> = changes.iter().flat_map(|c| c.keys().cloned()).collect();
        for (k, value) in changes.unwrap_or_default() {
            merged.insert(k, value);
        }
        let (block, _) = decode_kind(&kind, &merged)?;
        self.edit_raster_records(OP, &layer, |records, _| {
            let (stack, at) = entry_mut(records, &layer, &instance)?;
            let entry = &mut stack.filters[at];
            let before = entry.clone();
            entry.params = block;
            if let Some(o) = opacity {
                entry.opacity = o;
            }
            if *entry == before {
                return Err(nothing());
            }
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
                match (p.ty, default_json(p)) {
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
        let probe = StudioSmartFilterStack { enabled: true, stack_mask: None, filters: vec![StudioSmartFilter { instance_id: "p".into(), enabled: true, opacity: 100.0, mask: None, params: parsed }] };
        hsk_studio_pigment::smart_filter::SmartFilterOp::from_record(&probe, |_| None).is_ok_and(|op| op.check())
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
    }
}
