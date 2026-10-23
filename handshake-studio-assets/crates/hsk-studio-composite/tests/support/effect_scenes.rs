//! Shared fixture of lane SMB-AE-EFFECT-STACK-1 (folio-free, motion-free). Included with `#[path]`
//! by composite `tests/composition_effects.rs` (kernels, Folio records -> scenes -> pixels through
//! a reference executor) and by render-cpu `tests/composition_effect_stack.rs` (decision B1: the
//! renderer proof renders the same effected scene on the CPU backend).
//!
//! Source: a 16x12 straight-alpha tile with a horizontal and vertical colour ramp and an alpha that
//! steps through `{1, 0.75, 0.5, 0}` by column band, so premultiplication, transparent regions and
//! every distort's resampling are exercised. Every sample is a dyadic fraction (exact in `f32`).
//! [`golden_cases`] are the pinned stacks of the kernel golden test: one per implemented effect
//! plus two orders of one pair, run on [`premultiplied_source`].
#![allow(dead_code)]
use hsk_studio_composite::composition::{EffectedContent, GeneratorComposite, SceneEffect, SceneEffectEntry};
use hsk_studio_composite::{SceneComposition, SceneContent, SceneLayer, ScenePixels, SceneTile};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{BlendSpace, StudioBlendMode};
use hsk_studio_pigment::render_plan::{AlphaConvention, ColourTag, SampleFormat, TilePlanes};
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};

pub const W: u32 = 16;
pub const H: u32 = 12;

pub fn identity(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
pub fn tile_id() -> String {
    format!("{}/src", identity("SLYR", 2))
}
pub fn space() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded }
}

/// Straight RGBA of source pixel `(x, y)` (module docs).
pub fn source_texel(x: u32, y: u32) -> [f32; 4] {
    let a = match x / 4 {
        0 => 1.0,
        1 => 0.75,
        2 => 0.5,
        _ => if y < 6 { 1.0 } else { 0.0 },
    };
    [x as f32 / 16.0, y as f32 / 16.0, 0.25 + ((x + y) % 4) as f32 / 8.0, a]
}
/// The source as the premultiplied working buffer of the kernels (`f64` product, `f32` store, as
/// the scene path premultiplies).
pub fn premultiplied_source() -> Vec<[f32; 4]> {
    let mut v = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let p = source_texel(x, y);
            let a = f64::from(p[3]);
            v.push([(f64::from(p[0]) * a) as f32, (f64::from(p[1]) * a) as f32, (f64::from(p[2]) * a) as f32, a as f32]);
        }
    }
    v
}
/// A premultiplied buffer as the straight `(colour, alpha)` planes the scene path places.
pub fn straight(buf: &[[f32; 4]]) -> Vec<[f32; 4]> {
    buf.iter().map(|p| if p[3] > 0.0 { [p[0] / p[3], p[1] / p[3], p[2] / p[3], p[3]] } else { [0.0; 4] }).collect()
}
/// SHA-256 (hex) of the buffer's little-endian `f32` bytes, row-major, RGBA per pixel.
pub fn digest(buf: &[[f32; 4]]) -> String {
    let mut bytes = Vec::with_capacity(buf.len() * 16);
    for p in buf {
        for c in p {
            bytes.extend_from_slice(&c.to_le_bytes());
        }
    }
    hsk_studio_prism::profile_hash(&bytes).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn entry(id: &str, effect: SceneEffect) -> SceneEffectEntry {
    SceneEffectEntry { effect_id: id.to_owned(), effect, mix: 1.0 }
}
pub fn twirl() -> SceneEffect {
    SceneEffect::Twirl { angle_deg: 135.0, radius_px: 7.0, centre: [8.0, 6.0] }
}
pub fn spherize() -> SceneEffect {
    SceneEffect::Spherize { radius_px: 5.5, centre: [7.5, 6.0] }
}
pub fn offset() -> SceneEffect {
    SceneEffect::Offset { shift: [3.0, 2.0], blend: 0.0 }
}
pub fn polar(rect_to_polar: bool) -> SceneEffect {
    SceneEffect::PolarCoordinates { interpolation: 0.75, rect_to_polar }
}
pub fn ramp(radial: bool) -> SceneEffect {
    SceneEffect::GradientRamp {
        start: [2.0, 3.0],
        start_colour: [1.0, 0.25, 0.0, 1.0],
        end: [14.0, 9.0],
        end_colour: [0.0, 0.5, 1.0, 0.5],
        radial,
        blend: 0.25,
    }
}
pub fn circle(composite: GeneratorComposite) -> SceneEffect {
    SceneEffect::Circle { centre: [8.0, 6.0], radius_px: 4.25, feather_px: 0.0, colour: [1.0, 1.0, 0.5, 1.0], opacity: 0.75, composite }
}
pub fn checkerboard(composite: GeneratorComposite) -> SceneEffect {
    SceneEffect::Checkerboard { anchor: [1.0, 2.0], cell: [3.0, 2.0], colour: [0.0, 0.0, 1.0, 1.0], opacity: 1.0, composite }
}

/// The golden stacks (module docs).
pub fn golden_cases() -> Vec<(&'static str, Vec<SceneEffectEntry>)> {
    vec![
        ("twirl", vec![entry("a", twirl())]),
        ("spherize", vec![entry("a", spherize())]),
        ("offset", vec![entry("a", offset())]),
        ("polar_rect_to_polar", vec![entry("a", polar(true))]),
        ("polar_polar_to_rect", vec![entry("a", polar(false))]),
        ("ramp_linear", vec![entry("a", ramp(false))]),
        ("ramp_radial", vec![entry("a", ramp(true))]),
        ("circle_over_feathered", vec![entry("a", SceneEffect::Circle {
            centre: [5.0, 5.0], radius_px: 3.0, feather_px: 2.5, colour: [1.0, 0.0, 0.0, 1.0], opacity: 1.0, composite: GeneratorComposite::Over,
        })]),
        ("circle_replace", vec![entry("a", circle(GeneratorComposite::Replace))]),
        ("checkerboard_over", vec![entry("a", checkerboard(GeneratorComposite::Over))]),
        ("twirl_then_offset", vec![entry("a", twirl()), entry("b", offset())]),
        ("offset_then_twirl_mixed", vec![entry("b", offset()), SceneEffectEntry { mix: 0.5, ..entry("a", twirl()) }]),
    ]
}

pub struct Pixels {
    pub colour: Vec<u8>,
    pub alpha: Vec<u8>,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}
impl Pixels {
    pub fn fixture() -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..H {
            for x in 0..W {
                let p = source_texel(x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        let colour_sha256 = hsk_studio_prism::profile_hash(&colour);
        let alpha_sha256 = hsk_studio_prism::profile_hash(&alpha);
        Self { colour, alpha, colour_sha256, alpha_sha256 }
    }
    pub fn scene_tile(&self) -> SceneTile {
        SceneTile {
            tile_id: tile_id(),
            placement: Rect { x: 0, y: 0, width: W, height: H },
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: self.colour_sha256,
            alpha_sha256: self.alpha_sha256,
        }
    }
}
impl ScenePixels for Pixels {
    fn tile(&self, id: &str) -> Option<TilePlanes<'_>> {
        (id == tile_id()).then(|| TilePlanes {
            width: W,
            height: H,
            colour: &self.colour,
            colour_stride_bytes: u64::from(W) * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(W) * 4,
            colour_sha256: self.colour_sha256,
            alpha_sha256: self.alpha_sha256,
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: space(),
        })
    }
}

/// One frame: the source tile under `effects` (and an optional layer-space mask) at identity.
pub fn effected_scene(pixels: &Pixels, effects: Vec<SceneEffectEntry>, mask: Option<Vec<f32>>) -> SceneComposition {
    SceneComposition {
        composition_id: identity("SCOM", 1),
        width: W,
        height: H,
        background: None,
        filter: ResampleFilter::Bilinear,
        layers: vec![SceneLayer {
            layer_id: identity("SLYR", 2),
            content: SceneContent::Effected(Box::new(EffectedContent {
                content: SceneContent::Tiles(vec![pixels.scene_tile()]),
                rect: Rect { x: 0, y: 0, width: W, height: H },
                mask,
                effects,
            })),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            matrix: Affine2::IDENTITY,
            matte: None,
            clipped: false,
        }],
    }
}
