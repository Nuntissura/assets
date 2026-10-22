//! Shared fixture of lane SMB-AE-ROTO-WIRING (folio-free, motion-free). Included with `#[path]` by
//! composite `tests/composition_roto.rs` (Folio records -> scenes -> pixels through a small
//! reference executor) and by the renderer tests render-cpu / render-gpu
//! `tests/composition_roto_frames.rs` (decision B1: cross-crate render proofs live in renderer
//! test files), which render the hand-built frame-blend and roto scenes below on each backend.
//!
//! Source ("nested") composition `SRC`: 16x12 px, 24 fps, 8 frames, no background. Layer 2: an
//! opaque 16x12 background tile (`bg`, cool colours); layer 3: an opaque 4x4 tile (`fg`, warm
//! colours) whose top-left corner is at `(3 + k, 4)` on frame `k` (one pixel per frame). Every
//! sample is in `[0, 1]` and opaque, so a frame's pixel is the fg texel inside the square and the
//! bg texel elsewhere ([`nested_pixel`]).
#![allow(dead_code)]
use hsk_studio_composite::composition::{FrameBlendMethod, FrameBlendedContent, RotoMatte, RotoscopedContent};
use hsk_studio_composite::{RenderedScene, SceneComposition, SceneContent, SceneLayer, ScenePixels, SceneTile};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{BlendSpace, StudioBlendMode};
use hsk_studio_pigment::frame_blend::FlowParams;
use hsk_studio_pigment::render_plan::{AlphaConvention, ColourTag, SampleFormat, TilePlanes};
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::selection::CoveragePlane;
use hsk_studio_pigment::transform_math::{Affine2, ResampleFilter};
use std::sync::Arc;

pub const W: u32 = 16;
pub const H: u32 = 12;
pub const FRAMES: u64 = 8;
/// 24 fps.
pub const TICKS_PER_FRAME: u64 = 10_584_000_000;
pub const FG: u32 = 4;

pub fn identity(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
pub fn bg_id() -> String {
    format!("{}/bg", identity("SLYR", 2))
}
pub fn fg_id() -> String {
    format!("{}/fg", identity("SLYR", 3))
}
pub fn space() -> ColourTag {
    ColourTag { profile_sha256: [7; 32], blend_space: BlendSpace::Encoded }
}

pub fn bg_texel(x: u32, y: u32) -> [f32; 4] {
    [0.10 + 0.02 * (x % 4) as f32, 0.30 + 0.02 * (y % 3) as f32, 0.70 + 0.01 * ((x + y) % 5) as f32, 1.0]
}
pub fn fg_texel(x: u32, y: u32) -> [f32; 4] {
    [0.85 + 0.03 * (x % 3) as f32, 0.20 + 0.04 * (y % 2) as f32, 0.10 + 0.02 * ((x * y) % 3) as f32, 1.0]
}
/// Left edge of the fg square on frame `k`.
pub fn fg_x(k: u64) -> i64 {
    3 + k as i64
}
pub const FG_Y: i64 = 4;
pub fn inside_fg(k: u64, x: i64, y: i64) -> bool {
    x >= fg_x(k) && x < fg_x(k) + i64::from(FG) && y >= FG_Y && y < FG_Y + i64::from(FG)
}
/// Pixel `(x, y)` of source frame `k` (module docs).
pub fn nested_pixel(k: u64, x: u32, y: u32) -> [f32; 4] {
    let (xi, yi) = (i64::from(x), i64::from(y));
    if inside_fg(k, xi, yi) {
        fg_texel((xi - fg_x(k)) as u32, (yi - FG_Y) as u32)
    } else {
        bg_texel(x, y)
    }
}
/// Source frame `k` as Pigment's straight RGBA plane.
pub fn nested_plane(k: u64) -> RgbaPlane {
    let mut data = Vec::new();
    for y in 0..H {
        for x in 0..W {
            data.push(nested_pixel(k, x, y));
        }
    }
    RgbaPlane { bounds: Rect { x: 0, y: 0, width: W, height: H }, data }
}

pub struct TilePix {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub colour: Vec<u8>,
    pub alpha: Vec<u8>,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}
impl TilePix {
    fn new(id: String, width: u32, height: u32, texel: fn(u32, u32) -> [f32; 4]) -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..height {
            for x in 0..width {
                let p = texel(x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        let colour_sha256 = hsk_studio_prism::profile_hash(&colour);
        let alpha_sha256 = hsk_studio_prism::profile_hash(&alpha);
        Self { id, width, height, colour, alpha, colour_sha256, alpha_sha256 }
    }
    pub fn scene_tile(&self) -> SceneTile {
        SceneTile {
            tile_id: self.id.clone(),
            placement: Rect { x: 0, y: 0, width: self.width, height: self.height },
            format: SampleFormat::RgbF32Le,
            alpha: AlphaConvention::Straight,
            colour_sha256: self.colour_sha256,
            alpha_sha256: self.alpha_sha256,
        }
    }
    pub fn planes(&self) -> TilePlanes<'_> {
        TilePlanes {
            width: self.width,
            height: self.height,
            colour: &self.colour,
            colour_stride_bytes: u64::from(self.width) * 12,
            alpha: &self.alpha,
            alpha_stride_bytes: u64::from(self.width) * 4,
            colour_sha256: self.colour_sha256,
            alpha_sha256: self.alpha_sha256,
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: space(),
        }
    }
}

pub struct Pixels {
    pub tiles: Vec<TilePix>,
}
impl Pixels {
    pub fn fixture() -> Self {
        Self { tiles: vec![TilePix::new(bg_id(), W, H, bg_texel), TilePix::new(fg_id(), FG, FG, fg_texel)] }
    }
    pub fn get(&self, id: &str) -> Option<&TilePix> {
        self.tiles.iter().find(|t| t.id == id)
    }
}
impl ScenePixels for Pixels {
    fn tile(&self, tile_id: &str) -> Option<TilePlanes<'_>> {
        self.get(tile_id).map(TilePix::planes)
    }
}

fn layer(id: String, content: SceneContent, matrix: Affine2) -> SceneLayer {
    SceneLayer {
        layer_id: id,
        content,
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        matrix,
        matte: None,
        clipped: false,
    }
}

/// Source frame `k` as a scene (hand-built; the bottom layer first).
pub fn nested_scene(k: u64, pixels: &Pixels) -> SceneComposition {
    let bg = pixels.get(&bg_id()).expect("bg tile");
    let fg = pixels.get(&fg_id()).expect("fg tile");
    SceneComposition {
        composition_id: identity("SCOM", 2),
        width: W,
        height: H,
        background: None,
        filter: ResampleFilter::Bilinear,
        layers: vec![
            layer(identity("SLYR", 2), SceneContent::Tiles(vec![bg.scene_tile()]), Affine2::IDENTITY),
            layer(identity("SLYR", 3), SceneContent::Tiles(vec![fg.scene_tile()]), Affine2::translate(fg_x(k) as f64, FG_Y as f64)),
        ],
    }
}

fn outer(content: SceneContent) -> SceneComposition {
    SceneComposition {
        composition_id: identity("SCOM", 1),
        width: W,
        height: H,
        background: None,
        filter: ResampleFilter::Bilinear,
        layers: vec![layer(identity("SLYR", 1), content, Affine2::IDENTITY)],
    }
}

/// An outer frame blending source frames `a` and `b` at `weight_b`.
pub fn blended_scene(a: u64, b: u64, weight_b: f64, method: FrameBlendMethod, pixels: &Pixels) -> SceneComposition {
    outer(SceneContent::FrameBlended(Box::new(FrameBlendedContent {
        a: nested_scene(a, pixels),
        b: nested_scene(b, pixels),
        weight_b,
        method,
        flow: FlowParams::DEFAULT,
    })))
}

/// A disc matte (radius 3.5 around the fg square centre of frame `k`, coverage from the pixel
/// centre distance, soft over one pixel) with decontaminated colours (pure fg red) where covered.
pub fn disc_matte(k: u64) -> RotoMatte {
    let (cx, cy) = (fg_x(k) as f64 + 2.0, FG_Y as f64 + 2.0);
    let mut data = Vec::new();
    let mut colours = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let d = ((x as f64 + 0.5 - cx).powi(2) + (y as f64 + 0.5 - cy).powi(2)).sqrt();
            let c = (4.0 - d).clamp(0.0, 1.0);
            data.push((c * 65535.0).round() as u16);
            colours.push([0.9, 0.2, 0.1]);
        }
    }
    RotoMatte { coverage: CoveragePlane { bounds: Rect { x: 0, y: 0, width: W, height: H }, data }, colours: Some(colours) }
}

/// An outer frame showing source frame `k` under `matte`.
pub fn roto_scene(k: u64, matte: RotoMatte, pixels: &Pixels) -> SceneComposition {
    outer(SceneContent::Rotoscoped(Box::new(RotoscopedContent {
        content: SceneContent::Precomp(Box::new(nested_scene(k, pixels))),
        rect: Rect { x: 0, y: 0, width: W, height: H },
        matte: Arc::new(matte),
    })))
}

/// The roto oracle: source pixel with alpha times coverage and the matte colour where covered.
pub fn roto_oracle(k: u64, m: &RotoMatte) -> RgbaPlane {
    let mut data = Vec::new();
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) as usize;
            let p = nested_pixel(k, x, y);
            let c = f64::from(m.coverage.data[i]) / 65535.0;
            let a = (f64::from(p[3]) * c) as f32;
            let rgb = match &m.colours {
                Some(v) if c > 0.0 => v[i],
                _ => [p[0], p[1], p[2]],
            };
            data.push([rgb[0], rgb[1], rgb[2], a]);
        }
    }
    RgbaPlane { bounds: Rect { x: 0, y: 0, width: W, height: H }, data }
}

/// Largest |difference| of premultiplied colour and alpha between a frame and an oracle plane.
pub fn max_delta(got: &RenderedScene, want: &RgbaPlane) -> f32 {
    let mut worst = 0.0f32;
    for y in 0..H {
        for x in 0..W {
            let g = got.pixel(x, y).expect("pixel");
            let w = want.data[(y * W + x) as usize];
            worst = worst.max((g[3] - w[3]).abs());
            for c in 0..3 {
                worst = worst.max((g[c] * g[3] - w[c] * w[3]).abs());
            }
        }
    }
    worst
}

pub fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}
