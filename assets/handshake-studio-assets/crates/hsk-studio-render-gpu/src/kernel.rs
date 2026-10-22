//! The WGSL compute pipeline (private: no wgpu type leaves the crate). One pipeline composites
//! one layer (solid fill, uploaded region, or a finished group buffer, optionally through a
//! coverage mask) into a chunk destination buffer.
//!
//! The list of blend modes is NOT written here: `gpu_blend_qualifies` derives it from pigment's
//! `MODES` facts (layer-applicable, class not Unsupported, separable or non-separable family,
//! plus Normal) and the WGSL switch is generated from that list, so pigment is the only owner of
//! "which modes exist" and of each mode's exactness class (an Approximate mode stays Approximate
//! in every receipt: the backend only ever lowers a class). This module owns only the per-mode
//! WGSL formula bodies (`FORMULAS` per channel, `FORMULAS_RGB` over the whole colour, transcribed
//! from `blend_math`); a qualifying mode without a body is simply unsupported (admission refuses
//! it) and the drift test in `tests/gpu_plan.rs` fails until a body with a parity case exists.
//!
//! The plan-extension envelope follows the same rule: `gpu_adjust_supported` is "pigment's
//! `AdjustmentKind::ALL` has this kind and `ADJ_FORMULAS` has a body", the adjustment switch is
//! generated from `ALL` (the opcode of a kind is its position in `ALL`), and the module holds three
//! entry points: `composite` (v1, bindings 0-3), `composite_ext` (adds the Blend-If weight,
//! bindings 0-4) and `adjust` (adjustment layers, bindings 0-5). The layer-effect matte passes are
//! a second module (`fx.wgsl`, entry point `fx`, its own bind group layout of one uniform block and
//! four storage buffers); `fx.rs` lists the passes of each effect.
#![deny(clippy::arithmetic_side_effects)]
use crate::pipeline::{PipelineFault, Stage, guarded};
use hsk_studio_pigment::adjust_math::AdjustmentKind;
use hsk_studio_pigment::blend_math::{
    Applicability, Exactness, Family, MODES, StudioBlendMode, fill_inside_blend,
};

pub(crate) const WORKGROUP_SIDE: u32 = 8;
/// Size of the uniform block in `kernel.wgsl` (`Params`).
pub(crate) const PARAMS_BYTES: u64 = 80;
/// Bytes per destination/source texel: RGBA f32.
pub(crate) const TEXEL_BYTES: u64 = 16;
/// Bytes per mask texel: coverage f32.
pub(crate) const MASK_TEXEL_BYTES: u64 = 4;
/// Bytes of the resample parameter block (`resample.wgsl` `ResampleParams`).
pub(crate) const RESAMPLE_PARAMS_BYTES: u64 = 48;
/// Bytes of one per-pixel sample site of the resample pass.
pub(crate) const SITE_BYTES: u64 = 16;

/// WGSL bodies of `fn blend_<key>(b: f32, s: f32) -> f32` (b = backdrop, s = source), copied
/// operation-for-operation from `hsk_studio_pigment::blend_math` (W3C Compositing 1 s10.1 and the
/// community-documented separable functions).
const FORMULAS: &[(StudioBlendMode, &str)] = &[
    (StudioBlendMode::Darken, "return min(b, s);"),
    (StudioBlendMode::Multiply, "return b * s;"),
    (
        StudioBlendMode::ColourBurn,
        "if (b == 1.0) { return 1.0; } if (s == 0.0) { return 0.0; } return 1.0 - min((1.0 - b) / s, 1.0);",
    ),
    (StudioBlendMode::Lighten, "return max(b, s);"),
    (StudioBlendMode::Screen, "return b + s - b * s;"),
    (
        StudioBlendMode::ColourDodge,
        "if (b == 0.0) { return 0.0; } if (s == 1.0) { return 1.0; } return min(b / (1.0 - s), 1.0);",
    ),
    // overlay(b, s) = hard_light(s, b)
    (
        StudioBlendMode::Overlay,
        "let hb = s; let hs = b; if (hs <= 0.5) { return hb * (2.0 * hs); } let t = 2.0 * hs - 1.0; return hb + t - hb * t;",
    ),
    (
        StudioBlendMode::SoftLight,
        "if (s <= 0.5) { return b - (1.0 - 2.0 * s) * b * (1.0 - b); } var d: f32; if (b <= 0.25) { d = ((16.0 * b - 12.0) * b + 4.0) * b; } else { d = sqrt(b); } return b + (2.0 * s - 1.0) * (d - b);",
    ),
    (
        StudioBlendMode::HardLight,
        "if (s <= 0.5) { return b * (2.0 * s); } let t = 2.0 * s - 1.0; return b + t - b * t;",
    ),
    (StudioBlendMode::Difference, "return abs(b - s);"),
    (StudioBlendMode::Exclusion, "return b + s - 2.0 * b * s;"),
    // ---- community-documented separable modes (pigment class Approximate); pigment names the
    // arguments (a = backdrop, b = source), here b = backdrop and s = source ----
    (StudioBlendMode::LinearBurn, "return max(b + s - 1.0, 0.0);"),
    (StudioBlendMode::LinearDodgeAdd, "return min(b + s, 1.0);"),
    (
        StudioBlendMode::VividLight,
        "if (s < 0.5) { if (s == 0.0) { return 0.0; } return clamp(1.0 - (1.0 - b) / (2.0 * s), 0.0, 1.0); } if (s >= 1.0) { return 1.0; } return clamp(b / (2.0 * (1.0 - s)), 0.0, 1.0);",
    ),
    (
        StudioBlendMode::LinearLight,
        "return clamp(b + 2.0 * s - 1.0, 0.0, 1.0);",
    ),
    (
        StudioBlendMode::PinLight,
        "if (s < 0.5) { return min(b, 2.0 * s); } return max(b, 2.0 * s - 1.0);",
    ),
    (
        StudioBlendMode::HardMix,
        "let t = b + s - 1.0; if (abs(t) <= 1.0e-6) { if (s <= 0.5) { return 1.0; } return 0.0; } if (t > 0.0) { return 1.0; } return 0.0;",
    ),
    (StudioBlendMode::Subtract, "return max(b - s, 0.0);"),
    (
        StudioBlendMode::Divide,
        "if (s == 0.0) { if (b == 0.0) { return 0.0; } return 1.0; } return min(b / s, 1.0);",
    ),
];

/// WGSL bodies of `fn blend_<key>(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32>` for the modes whose
/// result mixes channels (W3C Compositing 1 s10.2 non-separable family, and the whole-pixel
/// darker/lighter colour). The non-separable bodies call the helpers
/// `hsk_lum`, `hsk_set_lum`, `hsk_sat`, `hsk_set_sat` of `kernel.wgsl`, which follow
/// `blend_math::{lum, set_lum, sat, set_sat}` operation for operation.
const FORMULAS_RGB: &[(StudioBlendMode, &str)] = &[
    (
        StudioBlendMode::Hue,
        "return hsk_set_lum(hsk_set_sat(cs, hsk_sat(cb)), hsk_lum(cb));",
    ),
    (
        StudioBlendMode::Saturation,
        "return hsk_set_lum(hsk_set_sat(cb, hsk_sat(cs)), hsk_lum(cb));",
    ),
    (
        StudioBlendMode::Colour,
        "return hsk_set_lum(cs, hsk_lum(cb));",
    ),
    (
        StudioBlendMode::Luminosity,
        "return hsk_set_lum(cb, hsk_lum(cs));",
    ),
    // whole-pixel modes (pigment class Approximate): channel sums compared left to right as
    // `Iterator::sum` does; a tie keeps the backdrop
    (
        StudioBlendMode::DarkerColour,
        "if (cs.x + cs.y + cs.z < cb.x + cb.y + cb.z) { return cs; } return cb;",
    ),
    (
        StudioBlendMode::LighterColour,
        "if (cs.x + cs.y + cs.z > cb.x + cb.y + cb.z) { return cs; } return cb;",
    ),
];

fn formula(mode: StudioBlendMode) -> Option<&'static str> {
    FORMULAS
        .iter()
        .find(|(m, _)| *m == mode)
        .map(|(_, body)| *body)
}

fn formula_rgb(mode: StudioBlendMode) -> Option<&'static str> {
    FORMULAS_RGB
        .iter()
        .find(|(m, _)| *m == mode)
        .map(|(_, body)| *body)
}

/// Does pigment's fact table say the GPU backend should implement `mode`? Layer-applicable, a
/// usable formula (class Exact or Approximate, not Unsupported), and `Normal`, the separable,
/// the non-separable or the whole-pixel family. Stochastic (dissolve: the plan carries no seed),
/// structural, tool-only and unspecified modes are not in the list. This is the single source of
/// the supported list; the class itself stays pigment's (`facts().exactness`).
pub fn gpu_blend_qualifies(mode: StudioBlendMode) -> bool {
    let facts = mode.facts();
    facts.applicability == Applicability::Layer
        && facts.exactness != Exactness::Unsupported
        && (mode == StudioBlendMode::Normal
            || matches!(
                facts.family,
                Family::Separable | Family::NonSeparable | Family::WholePixel
            ))
}

/// Qualifies by pigment's facts AND has a WGSL body here (`Normal` needs none). Only these modes
/// pass `exactness()` and are compiled into the shader.
pub fn gpu_blend_supported(mode: StudioBlendMode) -> bool {
    gpu_blend_qualifies(mode)
        && (mode == StudioBlendMode::Normal
            || formula(mode).is_some()
            || formula_rgb(mode).is_some())
}

/// WGSL bodies of `fn adj_<key>(rgb: vec3<f32>) -> vec3<f32>`: one-line calls into the helper
/// functions of `kernel.wgsl`, which transcribe `hsk_studio_pigment::adjust_math` and
/// `hsk_studio_pigment::adjust_colour`. `Invert` is inline (`1 - x`). `BrightnessContrast` has the
/// legacy transfer only (`compile_adjustment` refuses the modern form, as pigment's `prepare` does).
const ADJ_FORMULAS: &[(AdjustmentKind, &str)] = &[
    (AdjustmentKind::Levels, "return levels_apply(rgb);"),
    (AdjustmentKind::Curves, "return curves_apply(rgb);"),
    (
        AdjustmentKind::HueSaturation,
        "return hue_saturation_apply(rgb);",
    ),
    (AdjustmentKind::Exposure, "return exposure_apply(rgb);"),
    (AdjustmentKind::Invert, "return vec3<f32>(1.0) - rgb;"),
    (AdjustmentKind::Threshold, "return threshold_apply(rgb);"),
    (AdjustmentKind::Posterize, "return posterize_apply(rgb);"),
    (
        AdjustmentKind::BrightnessContrast,
        "return brightness_contrast_apply(rgb);",
    ),
    (AdjustmentKind::Vibrance, "return vibrance_apply(rgb);"),
    (AdjustmentKind::ColourBalance, "return colour_balance_apply(rgb);"),
    (AdjustmentKind::PhotoFilter, "return photo_filter_apply(rgb);"),
    (AdjustmentKind::BlackAndWhite, "return black_and_white_apply(rgb);"),
    (AdjustmentKind::ChannelMixer, "return channel_mixer_apply(rgb);"),
    (
        AdjustmentKind::SelectiveColour,
        "return selective_colour_apply(rgb);",
    ),
    (AdjustmentKind::GradientMap, "return gradient_map_apply(rgb);"),
    (AdjustmentKind::ColourLookup, "return colour_lookup_apply(rgb);"),
    (
        AdjustmentKind::ShadowsHighlights,
        "return shadows_highlights_apply(rgb);",
    ),
    (
        AdjustmentKind::ToneCompression,
        "return tone_compression_apply(rgb);",
    ),
    (AdjustmentKind::MatchColour, "return match_colour_apply(rgb);"),
    (AdjustmentKind::ReplaceColour, "return replace_colour_apply(rgb);"),
];

fn adjust_formula(kind: AdjustmentKind) -> Option<&'static str> {
    ADJ_FORMULAS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, body)| *body)
}

/// Does the shader carry an adjustment body for `kind`? Only these kinds pass
/// `extension_exactness`; a kind without a body is refused at admission.
pub fn gpu_adjust_supported(kind: AdjustmentKind) -> bool {
    adjust_formula(kind).is_some()
}

/// The shader with its blend functions and mode switch generated from `MODES`, its adjustment
/// switch generated from `AdjustmentKind::ALL`, and the two composite entry points instantiated.
pub(crate) fn shader_source() -> String {
    let mut adjust_functions = String::new();
    let mut adjust_cases = String::new();
    for (opcode, kind) in AdjustmentKind::ALL.iter().enumerate() {
        let Some(body) = adjust_formula(*kind) else {
            continue;
        };
        let key = kind.key();
        adjust_functions.push_str(&format!(
            "fn adj_{key}(rgb: vec3<f32>) -> vec3<f32> {{\n    {body}\n}}\n"
        ));
        adjust_cases.push_str(&format!(
            "        case {opcode}u: {{\n            out = adj_{key}(rgb);\n        }}\n"
        ));
    }
    let pixel = include_str!("kernel_pixel.wgsl");
    let entries = format!(
        "{}\n{}",
        pixel
            .replace("@ENTRY@", "composite")
            .replace("//@BLEND_IF@", ""),
        pixel.replace("@ENTRY@", "composite_ext").replace(
            "//@BLEND_IF@",
            "let bw = blend_if_weight(s.rgb, b.rgb, b.a); opacity = opacity * bw; unfilled = unfilled * bw;"
        )
    );
    let mut functions = String::new();
    let mut cases = String::new();
    for facts in &MODES {
        let mode = facts.mode;
        if !gpu_blend_supported(mode) {
            continue;
        }
        let d = mode.discriminant();
        if mode == StudioBlendMode::Normal {
            cases.push_str(&format!(
                "        case {d}u: {{\n            raw = cs;\n        }}\n"
            ));
            continue;
        }
        let key = facts.key;
        if let Some(body) = formula(mode) {
            functions.push_str(&format!(
                "fn blend_{key}(b: f32, s: f32) -> f32 {{\n    {body}\n}}\n"
            ));
            cases.push_str(&format!(
                "        case {d}u: {{\n            raw = vec3<f32>(blend_{key}(cb.x, cs.x), blend_{key}(cb.y, cs.y), blend_{key}(cb.z, cs.z));\n        }}\n"
            ));
        } else if let Some(body) = formula_rgb(mode) {
            functions.push_str(&format!(
                "fn blend_{key}(cb: vec3<f32>, cs: vec3<f32>) -> vec3<f32> {{\n    {body}\n}}\n"
            ));
            cases.push_str(&format!(
                "        case {d}u: {{\n            raw = blend_{key}(cb, cs);\n        }}\n"
            ));
        }
    }
    // Photoshop's special-eight modes (pigment `fill_inside_blend`, `composite_over_fill`).
    let special: Vec<String> = MODES
        .iter()
        .filter(|f| fill_inside_blend(f.mode))
        .map(|f| format!("mode == {}u", f.mode.discriminant()))
        .collect();
    let d = |m: StudioBlendMode| m.discriminant();
    functions.push_str(&format!(
        "fn fill_inside(mode: u32) -> bool {{
    return {};
}}
         fn fill_source(mode: u32, s: f32, f: f32, eight_bit: bool) -> f32 {{
             if (mode == {}u || mode == {}u) {{ return 1.0 - (1.0 - s) * f; }}
             if (eight_bit && mode == {}u) {{ return floor((0.5 + (s - 0.5) * f) * 255.0) / 255.0; }}
             if (mode == {}u || mode == {}u) {{ return 0.5 + (s - 0.5) * f; }}
             return s * f;
}}
         fn is_hard_mix(mode: u32) -> bool {{
    return mode == {}u;
}}
",
        if special.is_empty() { "false".to_string() } else { special.join(" || ") },
        d(StudioBlendMode::ColourBurn),
        d(StudioBlendMode::LinearBurn),
        d(StudioBlendMode::LinearLight),
        d(StudioBlendMode::VividLight),
        d(StudioBlendMode::LinearLight),
        d(StudioBlendMode::HardMix),
    ));
    let mut wgsl = include_str!("kernel.wgsl")
        .replace("//@PIXEL_ENTRIES@", &entries)
        .replace("//@BLEND_FUNCTIONS@", &functions)
        .replace("//@BLEND_CASES@", &cases)
        .replace(
            "@ADD_MODE@",
            &StudioBlendMode::LinearDodgeAdd.discriminant().to_string(),
        )
        .replace(
            "@SCREEN_MODE@",
            &StudioBlendMode::Screen.discriminant().to_string(),
        )
        .replace("//@ADJUST_FUNCTIONS@", &adjust_functions)
        .replace("//@ADJUST_CASES@", &adjust_cases);
    // Knockout passes (lane SMB-PS-ADJUST-STYLES-3): appended so they share `blend` and the
    // overflow helpers; their bindings 6-9 are in no other entry point's layout.
    wgsl.push_str(include_str!("kernel_knockout.wgsl"));
    wgsl
}

/// Size of the knockout uniform block `Ko` in `kernel_knockout.wgsl`.
pub(crate) const KO_PARAMS_BYTES: u64 = 16;
/// `Ko.flags` bits.
pub(crate) const KO_RECORD: u32 = 1;
pub(crate) const KO_OUTER: u32 = 2;
pub(crate) const KO_EXTENDED: u32 = 4;

/// The `Ko` block: the layer opacity (the knockout shape's factor) and the flags.
pub(crate) fn ko_params_bytes(layer_opacity: f32, flags: u32) -> [u8; KO_PARAMS_BYTES as usize] {
    let mut out = [0u8; KO_PARAMS_BYTES as usize];
    out[..4].copy_from_slice(&layer_opacity.to_ne_bytes());
    out[4..8].copy_from_slice(&flags.to_ne_bytes());
    out
}

/// Which knockout entry point a dispatch runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KoPass {
    /// `knockout_s` (bind group from [`Kernel::bind_ko_s`]).
    S,
    /// `knockout_mix` (bind group from [`Kernel::bind_ko_mix`]).
    Mix,
    /// `knockout_clear` (bind group from [`Kernel::bind_ko_mix`]).
    Clear,
}

/// Where a layer's source texels come from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SourceKind {
    /// Straight RGBA fill.
    Solid([f32; 4]),
    /// Uploaded region, `region.width * region.height` texels, row-major.
    Region,
    /// A finished group buffer of chunk size, indexed like the destination.
    Chunk,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct LayerParams {
    pub chunk_w: u32,
    pub chunk_h: u32,
    pub region: [u32; 4],
    pub mode: StudioBlendMode,
    pub source: SourceKind,
    pub has_mask: bool,
    pub opacity: f32,
    /// The opacity without the fill and the fill (lane SMB-PS-ORACLE-PARITY-1): a unit-range
    /// special-eight mode with `fill < 1` composites with pigment `composite_over_fill`.
    pub unfilled: f32,
    pub fill: f32,
    /// Clip-run base initialisation: replace the (cleared) destination with the source texel
    /// scaled by the mask, ignoring opacity and mode.
    pub replace: bool,
    /// Clipped layer inside a run: source alpha times the destination's alpha, alpha held.
    pub clip: bool,
    /// `flags` bit 3: the source counts as fully covering (alpha 1), the content of an interior
    /// unit (lane SMB-PS-ORACLE-PARITY-1).
    pub opaque_source: bool,
    /// `flags` bit 4: pigment `interior_mix` of the destination (below) and the source (the
    /// interior unit) by the mask (the shape alpha).
    pub mix: bool,
    /// Mask density (`1.0` = plain mask: the shader leaves the coverage bit-for-bit).
    pub density: f32,
    /// Extended-range output stage: `flags` bit 2, no unit clamp in the kernel.
    pub extended: bool,
    /// `flags` bit 6 (lane SMB-PS-ORACLE-PARITY-2): no composite, the destination texel is stored
    /// in 8-bit levels (pigment `render_plan::quantise_8bit`), the 8-bit working depth.
    pub quantise: bool,
    /// `flags` bit 7 (lane SMB-PS-ORACLE-PARITY-2): an 8-bit document, the linear-light fill
    /// source is a whole level (pigment `blend_math::composite_over_fill_at`).
    pub eight_bit: bool,
}
impl LayerParams {
    pub(crate) fn to_bytes(self) -> [u8; PARAMS_BYTES as usize] {
        let (kind, solid) = match self.source {
            SourceKind::Solid(rgba) => (0u32, rgba),
            SourceKind::Region => (1, [0.0; 4]),
            SourceKind::Chunk => (2, [0.0; 4]),
        };
        let flags = u32::from(self.replace)
            | if self.clip { 2 } else { 0 }
            | if self.opaque_source { 8 } else { 0 }
            | if self.mix { 16 } else { 0 }
            | if self.quantise { 64 } else { 0 }
            | if self.eight_bit { 128 } else { 0 }
            | if self.extended {
                crate::plan_sink::EXTENDED_FLAG
            } else {
                0
            };
        // The 20 words of `Params` in declaration order (the solid colour starts at byte 48, the
        // fill split at byte 64).
        let words: [[u8; 4]; 20] = [
            self.chunk_w.to_ne_bytes(),
            self.chunk_h.to_ne_bytes(),
            self.region[0].to_ne_bytes(),
            self.region[1].to_ne_bytes(),
            self.region[2].to_ne_bytes(),
            self.region[3].to_ne_bytes(),
            u32::from(self.mode.discriminant()).to_ne_bytes(),
            kind.to_ne_bytes(),
            self.opacity.to_ne_bytes(),
            u32::from(self.has_mask).to_ne_bytes(),
            flags.to_ne_bytes(),
            self.density.to_ne_bytes(),
            solid[0].to_ne_bytes(),
            solid[1].to_ne_bytes(),
            solid[2].to_ne_bytes(),
            solid[3].to_ne_bytes(),
            self.fill.to_ne_bytes(),
            self.unfilled.to_ne_bytes(),
            [0; 4],
            [0; 4],
        ];
        let mut out = [0u8; PARAMS_BYTES as usize];
        for (slot, word) in out.chunks_exact_mut(4).zip(words) {
            slot.copy_from_slice(&word);
        }
        out
    }
}

pub(crate) struct Kernel {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    /// The envelope pipelines read the extra bindings 4 (`ext`) and 5 (`lut`).
    pipeline_ext: wgpu::ComputePipeline,
    pipeline_adjust: wgpu::ComputePipeline,
    layout_ext: wgpu::BindGroupLayout,
    /// Layer-effect matte passes (`fx.wgsl`, its own module and bind group layout).
    pipeline_fx: wgpu::ComputePipeline,
    layout_fx: wgpu::BindGroupLayout,
    /// Layer-transform resampling (`resample.wgsl`, lane SMB-PIGMENT-TRANSFORM).
    pipeline_resample: wgpu::ComputePipeline,
    layout_resample: wgpu::BindGroupLayout,
    /// Layer and backdrop blur passes (`blur.wgsl`, lane SMB-COMPOSITE-NEW-KINDS).
    pipeline_blur: wgpu::ComputePipeline,
    layout_blur: wgpu::BindGroupLayout,
    /// Layer filter passes (`filter.wgsl`, lane SMB-RENDER-FILTERS), over the blur bind layout.
    pipeline_filter: wgpu::ComputePipeline,
    /// Knockout passes (`kernel_knockout.wgsl` in the composite module, lane
    /// SMB-PS-ADJUST-STYLES-3): `knockout_s` over its layout, `knockout_mix` and `knockout_clear`
    /// over the mix layout.
    pipeline_ko_s: wgpu::ComputePipeline,
    layout_ko_s: wgpu::BindGroupLayout,
    pipeline_ko_mix: wgpu::ComputePipeline,
    pipeline_ko_clear: wgpu::ComputePipeline,
    layout_ko_mix: wgpu::BindGroupLayout,
    /// Masked smart-filter mix (`filter.wgsl` entry `filter_mix_masked`, lane
    /// SMB-PS-ADJUST-STYLES-3): the blur layout plus the weight plane (binding 4).
    pipeline_filter_w: wgpu::ComputePipeline,
    layout_filter_w: wgpu::BindGroupLayout,
}

/// Bytes of the uniform block `FilterP` in `filter.wgsl`: 12 words. It is written into the blur
/// parameter block (`BLURP_BYTES`, at least this size).
pub(crate) const FILTERP_BYTES: u64 = 48;

/// One pass of `filter.wgsl` (the op codes of the shader header).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum FilterOp {
    Unsharp { amount: f32, threshold: f32 },
    HighPass,
    Copy,
    Median { radius: u32 },
    Noise { amount: f32, gaussian: bool, mono: bool, seed: u64, origin: (i32, i32) },
    /// Premultiplied lerp of `src` toward `aux` by `t` (smart-filter entry mix), transparent
    /// outside the frame-relative rect `[x0, x1) x [y0, y1)`.
    Mix { t: f32, rect: (i32, i32, u32, u32) },
}

/// The `FilterP` block of `op` over a `frame_w x frame_h` frame.
/// `filter.wgsl` op flag of an extended-range pass (bit 8 of `FilterP.op`).
pub(crate) const FILTER_EXTENDED_BIT: u32 = 0x100;

pub(crate) fn filter_params_bytes(op: FilterOp, frame_w: u32, frame_h: u32, extended: bool) -> Vec<u8> {
    // fw, fh, op, radius, origin_x, origin_y, seed_lo, seed_hi, amount, threshold, dist, mono
    let (code, radius, origin, seed, amount, threshold, dist, mono) = match op {
        FilterOp::Unsharp { amount, threshold } => (0u32, 0u32, (0i32, 0i32), 0u64, amount, threshold, 0u32, 0u32),
        FilterOp::HighPass => (1, 0, (0, 0), 0, 0.0, 0.0, 0, 0),
        FilterOp::Copy => (2, 0, (0, 0), 0, 0.0, 0.0, 0, 0),
        FilterOp::Median { radius } => (3, radius, (0, 0), 0, 0.0, 0.0, 0, 0),
        FilterOp::Noise { amount, gaussian, mono, seed, origin } => {
            (4, 0, origin, seed, amount, 0.0, u32::from(gaussian), u32::from(mono))
        }
        FilterOp::Mix { t, rect } => (5, 0, (rect.0, rect.1), u64::from(rect.2) | (u64::from(rect.3) << 32), t, 0.0, 0, 0),
    };
    let code = if extended { code | FILTER_EXTENDED_BIT } else { code };
    let words = [
        frame_w,
        frame_h,
        code,
        radius,
        origin.0 as u32,
        origin.1 as u32,
        seed as u32,
        (seed >> 32) as u32,
        amount.to_bits(),
        threshold.to_bits(),
        dist,
        mono,
    ];
    let mut out = Vec::with_capacity(FILTERP_BYTES as usize);
    for w in words {
        out.extend_from_slice(&w.to_ne_bytes());
    }
    out
}

/// Bytes of the uniform block `BlurP` in `blur.wgsl`: 8 header words and 751 vec4 of taps.
pub(crate) const BLURP_BYTES: u64 = 12_048;
/// Taps the block can carry (pigment's largest blur kernel is 2 * 1500 + 1 = 3001).
pub(crate) const BLUR_MAX_TAPS: usize = 3_004;

/// One blur pass of `blur.wgsl` over a `fw x fh` frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BlurOp {
    Horizontal,
    Vertical,
    Mix,
}

/// The `BlurP` block: header and taps; `None` when there are more taps than the block holds.
pub(crate) fn blur_params_bytes(
    op: BlurOp,
    frame_w: u32,
    frame_h: u32,
    taps: &[f32],
    opacity: f32,
    extended: bool,
) -> Option<Vec<u8>> {
    if taps.len() > BLUR_MAX_TAPS || taps.is_empty() {
        return None;
    }
    let half = u32::try_from(taps.len().checked_div(2)?).ok()?;
    let n = u32::try_from(taps.len()).ok()?;
    let op = match op {
        BlurOp::Horizontal => 0u32,
        BlurOp::Vertical => 1,
        BlurOp::Mix => 2,
    };
    let mut out = Vec::with_capacity(BLURP_BYTES as usize);
    for w in [frame_w, frame_h, half, n, op, u32::from(extended), opacity.to_bits(), 0] {
        out.extend_from_slice(&w.to_ne_bytes());
    }
    for t in taps {
        out.extend_from_slice(&t.to_ne_bytes());
    }
    out.resize(BLURP_BYTES as usize, 0);
    Some(out)
}

impl Kernel {
    /// Build the pipelines from `source` (`None` = the generated [`shader_source`]; `Some` is the
    /// fault-injection route for a broken shader). Every module, layout and pipeline is created
    /// under [`guarded`] (out-of-memory, internal and validation scopes plus a panic guard), so a
    /// naga rejection is `shader_validation` and a backend compiler rejection (DX12 FXC, class
    /// internal) is `pipeline_create`, both with the object's label and the backend message, before
    /// any dispatch and never through the uncaptured-error handler (tests
    /// pipeline_failure_is_typed_and_fails_the_gpu_gate, tests/gpu_pipeline_fault.rs). The first
    /// failure stops the build: no later object is created.
    pub(crate) fn new(device: &wgpu::Device, source: Option<&str>) -> Result<Self, PipelineFault> {
        let module = |label: &'static str, wgsl: std::borrow::Cow<'static, str>| {
            guarded(device, label, Stage::Module, || {
                device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some(label),
                    source: wgpu::ShaderSource::Wgsl(wgsl),
                })
            })
        };
        let bind_layout = |label: &'static str, entries: &[wgpu::BindGroupLayoutEntry]| {
            guarded(device, label, Stage::Pipeline, || {
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some(label),
                    entries,
                })
            })
        };
        let pipeline_layout = |label: &'static str, layout: &wgpu::BindGroupLayout| {
            guarded(device, label, Stage::Pipeline, || {
                device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some(label),
                    bind_group_layouts: &[layout],
                    push_constant_ranges: &[],
                })
            })
        };
        let make = |label: &'static str,
                    layout: &wgpu::PipelineLayout,
                    module: &wgpu::ShaderModule,
                    entry_point: &str| {
            guarded(device, label, Stage::Pipeline, || {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(label),
                    layout: Some(layout),
                    module,
                    entry_point: Some(entry_point),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    cache: None,
                })
            })
        };
        let entry = |binding: u32, ty: wgpu::BufferBindingType| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let uniform = wgpu::BufferBindingType::Uniform;
        let read = wgpu::BufferBindingType::Storage { read_only: true };
        let write = wgpu::BufferBindingType::Storage { read_only: false };

        let wgsl = source.map_or_else(shader_source, str::to_owned);
        let composite_module = module("hsk-studio-render-gpu composite", std::borrow::Cow::Owned(wgsl))?;
        let layout = bind_layout(
            "hsk-studio-render-gpu layout",
            &[entry(0, uniform), entry(1, read), entry(2, write), entry(3, read)],
        )?;
        let layout_main = pipeline_layout("hsk-studio-render-gpu pipeline layout", &layout)?;
        let layout_ext = bind_layout(
            "hsk-studio-render-gpu envelope layout",
            &[
                entry(0, uniform),
                entry(1, read),
                entry(2, write),
                entry(3, read),
                entry(4, uniform),
                entry(5, read),
            ],
        )?;
        let layout_ext_main =
            pipeline_layout("hsk-studio-render-gpu envelope pipeline layout", &layout_ext)?;
        let pipeline = make(
            "hsk-studio-render-gpu composite",
            &layout_main,
            &composite_module,
            "composite",
        )?;
        let pipeline_ext = make(
            "hsk-studio-render-gpu composite envelope",
            &layout_ext_main,
            &composite_module,
            "composite_ext",
        )?;
        let pipeline_adjust = make(
            "hsk-studio-render-gpu adjust",
            &layout_ext_main,
            &composite_module,
            "adjust",
        )?;
        // Knockout passes (lane SMB-PS-ADJUST-STYLES-3), entry points of the composite module.
        // knockout_s: 0 params, 1 source, 3 mask, 6 Ko, 7 S (prefilled with the initial
        // backdrop), 8 the shape plane; knockout_mix / knockout_clear: 0 params, 2 destination, 6
        // Ko, 7 S, 8 the shape plane, 9 the accumulated deep shape. Four storage buffers each.
        let layout_ko_s = bind_layout(
            "hsk-studio-render-gpu knockout layout",
            &[
                entry(0, uniform),
                entry(1, read),
                entry(3, read),
                entry(6, uniform),
                entry(7, write),
                entry(8, write),
            ],
        )?;
        let layout_ko_s_main =
            pipeline_layout("hsk-studio-render-gpu knockout pipeline layout", &layout_ko_s)?;
        let pipeline_ko_s = make(
            "hsk-studio-render-gpu knockout s",
            &layout_ko_s_main,
            &composite_module,
            "knockout_s",
        )?;
        let layout_ko_mix = bind_layout(
            "hsk-studio-render-gpu knockout mix layout",
            &[
                entry(0, uniform),
                entry(2, write),
                entry(6, uniform),
                entry(7, write),
                entry(8, write),
                entry(9, write),
            ],
        )?;
        let layout_ko_mix_main =
            pipeline_layout("hsk-studio-render-gpu knockout mix pipeline layout", &layout_ko_mix)?;
        let pipeline_ko_mix = make(
            "hsk-studio-render-gpu knockout mix",
            &layout_ko_mix_main,
            &composite_module,
            "knockout_mix",
        )?;
        let pipeline_ko_clear = make(
            "hsk-studio-render-gpu knockout clear",
            &layout_ko_mix_main,
            &composite_module,
            "knockout_clear",
        )?;
        // Layer-effect passes: a separate module so the layouts above stay as they were. Bindings:
        // 0 uniform parameter block (with the blur taps), 1-2 read-only planes, 3 the output plane,
        // 4 the content frame; four storage buffers is the device's downlevel limit.
        let layout_fx = bind_layout(
            "hsk-studio-render-gpu effect layout",
            &[entry(0, uniform), entry(1, read), entry(2, read), entry(3, write), entry(4, read)],
        )?;
        let layout_fx_main = pipeline_layout("hsk-studio-render-gpu effect pipeline layout", &layout_fx)?;
        let fx_module = module(
            "hsk-studio-render-gpu effects",
            std::borrow::Cow::Borrowed(include_str!("fx.wgsl")),
        )?;
        let pipeline_fx = make("hsk-studio-render-gpu effects", &layout_fx_main, &fx_module, "fx")?;
        // Layer-transform resampling: 0 uniform parameters, 1 per-pixel sample sites, 2 the source
        // window, 3 the region-sized output (the composite pass's source buffer).
        let layout_resample = bind_layout(
            "hsk-studio-render-gpu resample layout",
            &[entry(0, uniform), entry(1, read), entry(2, read), entry(3, write)],
        )?;
        let layout_resample_main =
            pipeline_layout("hsk-studio-render-gpu resample pipeline layout", &layout_resample)?;
        let resample_module = module(
            "hsk-studio-render-gpu resample",
            std::borrow::Cow::Borrowed(include_str!("resample.wgsl")),
        )?;
        let pipeline_resample = make(
            "hsk-studio-render-gpu resample",
            &layout_resample_main,
            &resample_module,
            "resample",
        )?;
        // Blur passes: 0 uniform parameters (with the taps), 1 read-only source, 2 the output, 3 a
        // read-only auxiliary frame (the layer shape of the backdrop mix).
        let layout_blur = bind_layout(
            "hsk-studio-render-gpu blur layout",
            &[entry(0, uniform), entry(1, read), entry(2, write), entry(3, read)],
        )?;
        let layout_blur_main =
            pipeline_layout("hsk-studio-render-gpu blur pipeline layout", &layout_blur)?;
        let blur_module = module(
            "hsk-studio-render-gpu blur",
            std::borrow::Cow::Borrowed(include_str!("blur.wgsl")),
        )?;
        let pipeline_blur = make("hsk-studio-render-gpu blur", &layout_blur_main, &blur_module, "blur")?;
        let filter_module = module(
            "hsk-studio-render-gpu filter",
            std::borrow::Cow::Borrowed(include_str!("filter.wgsl")),
        )?;
        let pipeline_filter = make(
            "hsk-studio-render-gpu filter",
            &layout_blur_main,
            &filter_module,
            "filter_pass",
        )?;
        // Masked smart-filter mix (lane SMB-PS-ADJUST-STYLES-3): the blur layout plus binding 4,
        // the per-texel weight (four storage buffers).
        let layout_filter_w = bind_layout(
            "hsk-studio-render-gpu masked mix layout",
            &[entry(0, uniform), entry(1, read), entry(2, write), entry(3, read), entry(4, read)],
        )?;
        let layout_filter_w_main =
            pipeline_layout("hsk-studio-render-gpu masked mix pipeline layout", &layout_filter_w)?;
        let pipeline_filter_w = make(
            "hsk-studio-render-gpu masked mix",
            &layout_filter_w_main,
            &filter_module,
            "filter_mix_masked",
        )?;
        Ok(Self {
            pipeline_filter_w,
            layout_filter_w,
            pipeline,
            layout,
            pipeline_ext,
            pipeline_adjust,
            layout_ext,
            pipeline_fx,
            layout_fx,
            pipeline_resample,
            layout_resample,
            pipeline_blur,
            layout_blur,
            pipeline_filter,
            pipeline_ko_s,
            layout_ko_s,
            pipeline_ko_mix,
            pipeline_ko_clear,
            layout_ko_mix,
        })
    }

    /// Bind group of the envelope entry points: the v1 bindings plus `ext` (4) and `lut` (5).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn bind_ext(
        &self,
        device: &wgpu::Device,
        params: &wgpu::Buffer,
        src: &wgpu::Buffer,
        mask: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        ext: &wgpu::Buffer,
        lut: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        fn at(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            }
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hsk-studio-render-gpu envelope bind"),
            layout: &self.layout_ext,
            entries: &[
                at(0, params),
                at(1, src),
                at(2, dst),
                at(3, mask),
                at(4, ext),
                at(5, lut),
            ],
        })
    }

    /// Bind group of `knockout_s`: params, the source, the mask, `Ko`, S and the shape plane.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn bind_ko_s(
        &self,
        device: &wgpu::Device,
        params: &wgpu::Buffer,
        src: &wgpu::Buffer,
        mask: &wgpu::Buffer,
        ko: &wgpu::Buffer,
        s: &wgpu::Buffer,
        k: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        fn at(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            }
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hsk-studio-render-gpu knockout bind"),
            layout: &self.layout_ko_s,
            entries: &[at(0, params), at(1, src), at(3, mask), at(6, ko), at(7, s), at(8, k)],
        })
    }

    /// Bind group of `knockout_mix` / `knockout_clear`: params, the destination, `Ko`, S, the
    /// shape plane and the accumulated deep shape (four distinct storage buffers).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn bind_ko_mix(
        &self,
        device: &wgpu::Device,
        params: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        ko: &wgpu::Buffer,
        s: &wgpu::Buffer,
        k: &wgpu::Buffer,
        acc: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        fn at(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            }
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hsk-studio-render-gpu knockout mix bind"),
            layout: &self.layout_ko_mix,
            entries: &[at(0, params), at(2, dst), at(6, ko), at(7, s), at(8, k), at(9, acc)],
        })
    }

    /// One knockout pass over `width` x `height` texels.
    pub(crate) fn dispatch_ko(
        &self,
        pass: KoPass,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        let pipeline = match pass {
            KoPass::S => &self.pipeline_ko_s,
            KoPass::Mix => &self.pipeline_ko_mix,
            KoPass::Clear => &self.pipeline_ko_clear,
        };
        Self::dispatch_with(pipeline, encoder, bind, width, height);
    }

    /// Bind group of one effect pass: parameter block, two read-only planes, the output plane and
    /// the content frame. The output must be a different buffer from both inputs.
    pub(crate) fn bind_fx(
        &self,
        device: &wgpu::Device,
        params: &wgpu::Buffer,
        planes: [&wgpu::Buffer; 3],
        content: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let [fin, fin2, fout] = planes;
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hsk-studio-render-gpu effect bind"),
            layout: &self.layout_fx,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: fin.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: fin2.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: fout.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: content.as_entire_binding(),
                },
            ],
        })
    }

    /// Bind group of the resample pass: parameters, sample sites, source window, output.
    pub(crate) fn bind_resample(
        &self,
        device: &wgpu::Device,
        params: &wgpu::Buffer,
        sites: &wgpu::Buffer,
        window: &wgpu::Buffer,
        out: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let entries: Vec<wgpu::BindGroupEntry<'_>> = [params, sites, window, out]
            .into_iter()
            .zip(0u32..)
            .map(|(buffer, binding)| wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            })
            .collect();
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hsk-studio-render-gpu resample bind"),
            layout: &self.layout_resample,
            entries: &entries,
        })
    }

    /// Bind group of one blur pass: parameters, source, output, auxiliary frame. The output must
    /// differ from both read-only buffers.
    pub(crate) fn bind_blur(
        &self,
        device: &wgpu::Device,
        params: &wgpu::Buffer,
        src: &wgpu::Buffer,
        dst: &wgpu::Buffer,
        aux: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        let entries: Vec<wgpu::BindGroupEntry<'_>> = [params, src, dst, aux]
            .into_iter()
            .zip(0u32..)
            .map(|(buffer, binding)| wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            })
            .collect();
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hsk-studio-render-gpu blur bind"),
            layout: &self.layout_blur,
            entries: &entries,
        })
    }

    /// One blur pass over `width` x `height` texels (bind group from [`Kernel::bind_blur`]).
    pub(crate) fn dispatch_blur(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        Self::dispatch_with(&self.pipeline_blur, encoder, bind, width, height);
    }

    /// One filter pass over `width` x `height` texels (bind group from [`Kernel::bind_blur`]: the
    /// filter module shares the blur bind group layout).
    pub(crate) fn dispatch_filter(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        Self::dispatch_with(&self.pipeline_filter, encoder, bind, width, height);
    }

    /// Bind group of the masked mix: the blur bindings plus the weight plane.
    pub(crate) fn bind_filter_w(
        &self,
        device: &wgpu::Device,
        params: &wgpu::Buffer,
        buffers: [&wgpu::Buffer; 4],
    ) -> wgpu::BindGroup {
        let [src, dst, aux, weight] = buffers;
        fn at(binding: u32, buffer: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry {
                binding,
                resource: buffer.as_entire_binding(),
            }
        }
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hsk-studio-render-gpu masked mix bind"),
            layout: &self.layout_filter_w,
            entries: &[at(0, params), at(1, src), at(2, dst), at(3, aux), at(4, weight)],
        })
    }

    /// One masked mix pass over `width` x `height` texels.
    pub(crate) fn dispatch_filter_w(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        Self::dispatch_with(&self.pipeline_filter_w, encoder, bind, width, height);
    }

    /// One resample pass over `width` x `height` texels (bind group from [`Kernel::bind_resample`]).
    pub(crate) fn dispatch_resample(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        Self::dispatch_with(&self.pipeline_resample, encoder, bind, width, height);
    }

    pub(crate) fn bind(
        &self,
        device: &wgpu::Device,
        params: &wgpu::Buffer,
        src: &wgpu::Buffer,
        mask: &wgpu::Buffer,
        dst: &wgpu::Buffer,
    ) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("hsk-studio-render-gpu bind"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: src.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: dst.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: mask.as_entire_binding(),
                },
            ],
        })
    }

    /// Record one v1 composite dispatch covering `width` x `height` texels.
    pub(crate) fn dispatch(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        Self::dispatch_with(&self.pipeline, encoder, bind, width, height);
    }

    /// Composite with the Blend-If weight (bind groups from [`Kernel::bind_ext`]).
    pub(crate) fn dispatch_ext(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        Self::dispatch_with(&self.pipeline_ext, encoder, bind, width, height);
    }

    /// Adjustment layer over the destination in place (bind groups from [`Kernel::bind_ext`]).
    pub(crate) fn dispatch_adjust(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        Self::dispatch_with(&self.pipeline_adjust, encoder, bind, width, height);
    }

    /// One effect pass over `width` x `height` texels (bind group from [`Kernel::bind_fx`]).
    pub(crate) fn dispatch_fx(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        Self::dispatch_with(&self.pipeline_fx, encoder, bind, width, height);
    }

    fn dispatch_with(
        pipeline: &wgpu::ComputePipeline,
        encoder: &mut wgpu::CommandEncoder,
        bind: &wgpu::BindGroup,
        width: u32,
        height: u32,
    ) {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("hsk-studio-render-gpu composite"),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, bind, &[]);
        pass.dispatch_workgroups(
            width.div_ceil(WORKGROUP_SIDE),
            height.div_ceil(WORKGROUP_SIDE),
            1,
        );
    }
}
