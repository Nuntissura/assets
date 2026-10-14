//! Shared composition fixture of lane SMB-COMPOSITE-COMPOSITION-ADAPTER (folio-free, motion-free).
//! Included with `#[path]` by composite `tests/composition_adapter.rs` (which proves that the
//! stored Folio composition evaluates to exactly these scenes) and by the renderer tests
//! render-cpu / render-gpu `tests/composition_frames.rs` (decision B1: cross-crate render proofs
//! live in renderer test files), which render the scenes and compare every pixel with [`oracle`].
//!
//! Composition `COMP` 32x24 px at 24 fps, background (0.1, 0.2, 0.3, 1), filter bilinear:
//! - layer 2 (bottom): tile `ta` 8x6 at (0, 0); window frames [0, 20); position static (12, 9),
//!   anchor = content centre (4, 3) -> integer translation (8, 6); opacity static 80 %.
//! - layer 3 (top): tile `tb` 6x6 at (0, 0); window frames [2, 40); linear keys at frames 0 and
//!   10: position (5, 5) -> (15.5, 12.25), scale 100 -> 150 %, rotation 0 -> 30 deg, opacity
//!   100 -> 50 %; anchor = (3, 3).
//!
//! The oracle is independent of composite's lowering: per pixel it samples each layer with
//! pigment's binary64 reference sampler (`PreparedTransform::sample_rgba`) and composites with
//! pigment's single-pixel `composite_over`, bottom to top over the background.
#![allow(dead_code)]
use hsk_studio_composite::{RenderedScene, SceneComposition, SceneContent, SceneLayer, ScenePixels, SceneTile};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::{BlendSpace, PixelSite, Rgba, StudioBlendMode, composite_over};
use hsk_studio_pigment::render_plan::{AlphaConvention, ColourTag, SampleFormat, TilePlanes};
use hsk_studio_pigment::transform_math::{Affine2, LayerTransform, PreparedTransform, ResampleFilter};

pub const W: u32 = 32;
pub const H: u32 = 24;
pub const FPS: u64 = 24;
/// Accord ticks per frame at 24 fps (254016000000 / 24).
pub const TICKS_PER_FRAME: u64 = 10_584_000_000;
pub const BACKGROUND: [f32; 4] = [0.1, 0.2, 0.3, 1.0];
/// Frames the renderer tests play (layer 3 absent at 0, animated at 5 and 10, held at 25 where
/// layer 2 has left its window).
pub const FRAMES: [u64; 4] = [0, 5, 10, 25];

pub fn identity(prefix: &str, n: u8) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
pub fn composition_id() -> String {
    identity("SCOM", 1)
}
pub fn tile_id(layer: u8, key: &str) -> String {
    format!("{}/{key}", identity("SLYR", layer))
}
pub fn space() -> ColourTag {
    ColourTag {
        profile_sha256: [7; 32],
        blend_space: BlendSpace::Encoded,
    }
}

/// Deterministic straight RGBA with soft alpha and transparent holes.
pub fn texel(seed: u32, x: u32, y: u32) -> [f32; 4] {
    let hole = (x * 3 + y * 5 + seed).is_multiple_of(11);
    [
        ((x * 7 + seed * 3) % 16) as f32 / 15.0,
        ((y * 5 + seed) % 12) as f32 / 11.0,
        ((x + y + seed) % 8) as f32 / 7.0,
        if hole { 0.0 } else { ((x + 2 * y + seed) % 4 + 1) as f32 / 4.0 },
    ]
}

/// One resolved tile: planes, hashes and its scene description.
pub struct TilePix {
    pub id: String,
    pub width: u32,
    pub height: u32,
    pub seed: u32,
    pub colour: Vec<u8>,
    pub alpha: Vec<u8>,
    pub colour_sha256: [u8; 32],
    pub alpha_sha256: [u8; 32],
}
impl TilePix {
    pub fn new(id: String, width: u32, height: u32, seed: u32) -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..height {
            for x in 0..width {
                let p = texel(seed, x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        let colour_sha256 = hsk_studio_prism::profile_hash(&colour);
        let alpha_sha256 = hsk_studio_prism::profile_hash(&alpha);
        Self { id, width, height, seed, colour, alpha, colour_sha256, alpha_sha256 }
    }
    pub fn at(&self, x: u32, y: u32) -> [f32; 4] {
        texel(self.seed, x, y)
    }
    pub fn scene_tile(&self, x: i64, y: i64) -> SceneTile {
        SceneTile {
            tile_id: self.id.clone(),
            placement: Rect { x, y, width: self.width, height: self.height },
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

/// Every tile of the fixture.
pub struct Pixels {
    pub tiles: Vec<TilePix>,
}
impl Pixels {
    pub fn fixture() -> Self {
        Self {
            tiles: vec![
                TilePix::new(tile_id(2, "ta"), 8, 6, 1),
                TilePix::new(tile_id(3, "tb"), 6, 6, 2),
                TilePix::new(tile_id(7, "tc"), 6, 4, 3),
            ],
        }
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

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// The scene of `frame` written out by hand from the records described in the module docs.
pub fn expected_scene(frame: u64, pixels: &Pixels) -> SceneComposition {
    let mut layers = Vec::new();
    if frame < 20 {
        let ta = pixels.get(&tile_id(2, "ta")).unwrap();
        layers.push(SceneLayer {
            layer_id: identity("SLYR", 2),
            content: SceneContent::Tiles(vec![ta.scene_tile(0, 0)]),
            mode: StudioBlendMode::Normal,
            opacity: 0.8,
            fill_opacity: 1.0,
            matrix: Affine2::translate(8.0, 6.0),
        });
    }
    if (2..40).contains(&frame) {
        let t = (frame.min(10) as f64) / 10.0;
        let (px, py) = (lerp(5.0, 15.5, t), lerp(5.0, 12.25, t));
        let s = lerp(100.0, 150.0, t) / 100.0;
        let rot = lerp(0.0, 30.0, t);
        let tb = pixels.get(&tile_id(3, "tb")).unwrap();
        layers.push(SceneLayer {
            layer_id: identity("SLYR", 3),
            content: SceneContent::Tiles(vec![tb.scene_tile(0, 0)]),
            mode: StudioBlendMode::Normal,
            opacity: lerp(100.0, 50.0, t) / 100.0,
            fill_opacity: 1.0,
            // anchor (3, 3): x' = position + R S (x - anchor).
            matrix: Affine2::from_parts(3.0, 3.0, s, s, rot, px - 3.0, py - 3.0),
        });
    }
    SceneComposition {
        composition_id: composition_id(),
        width: W,
        height: H,
        background: Some(BACKGROUND),
        filter: ResampleFilter::Bilinear,
        layers,
    }
}

pub fn composition2_id() -> String {
    identity("SCOM", 2)
}
/// Frames of composition 2 the renderer tests play.
pub const FRAMES2: [u64; 3] = [0, 5, 10];

/// Composition 2 (slice 2: anchor stream and parent chain), 32x24 px at 24 fps, no background:
/// - layer 6: null layer (no content) at position (16, 12), scale (150, 75) %, rotation keys
///   frame 0 -> 10: 0 -> 90 deg; anchor (0, 0) (content-less default).
/// - layer 7: tile `tc` 6x4 at (0, 0), parent layer 6; anchor keys frame 0 -> 10: (0, 0) ->
///   (6, 4); position (2, 1) in the parent's layer space; rotation 20 deg.
///   Content matrix = local(7) then local(6): a sheared product (child rotation under the
///   parent's non-uniform scale).
pub fn expected_scene2(frame: u64, pixels: &Pixels) -> SceneComposition {
    let t = (frame.min(10) as f64) / 10.0;
    let parent = Affine2::from_parts(0.0, 0.0, 1.5, 0.75, lerp(0.0, 90.0, t), 16.0, 12.0);
    let (ax, ay) = (lerp(0.0, 6.0, t), lerp(0.0, 4.0, t));
    let child = Affine2::from_parts(ax, ay, 1.0, 1.0, 20.0, 2.0 - ax, 1.0 - ay);
    let tc = pixels.get(&tile_id(7, "tc")).unwrap();
    SceneComposition {
        composition_id: composition2_id(),
        width: W,
        height: H,
        background: None,
        filter: ResampleFilter::Bilinear,
        layers: vec![
            SceneLayer {
                layer_id: identity("SLYR", 7),
                content: SceneContent::Tiles(vec![tc.scene_tile(0, 0)]),
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                matrix: child.then(parent),
            },
            SceneLayer {
                layer_id: identity("SLYR", 6),
                content: SceneContent::None,
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                matrix: parent,
            },
        ],
    }
}

/// Every rendered case: `(label, scene)` for the renderer tests.
pub fn cases(pixels: &Pixels) -> Vec<(String, SceneComposition)> {
    let mut out: Vec<(String, SceneComposition)> =
        FRAMES.iter().map(|f| (format!("comp1/frame{f}"), expected_scene(*f, pixels))).collect();
    out.extend(FRAMES2.iter().map(|f| (format!("comp2/frame{f}"), expected_scene2(*f, pixels))));
    out
}

/// Straight RGBA of every composition pixel, row-major (module docs).
pub fn oracle(scene: &SceneComposition, pixels: &Pixels) -> Vec<[f32; 4]> {
    let mut prepared = Vec::new();
    for l in &scene.layers {
        let SceneContent::Tiles(tiles) = &l.content else { continue };
        for t in tiles {
            let tile = pixels.get(&t.tile_id).expect("fixture tile");
            let p = PreparedTransform::new(&LayerTransform::new(l.matrix, scene.filter), t.placement)
                .expect("invertible");
            prepared.push((l, tile, p));
        }
    }
    let mut out = Vec::with_capacity((W * H) as usize);
    for y in 0..i64::from(scene.height) {
        for x in 0..i64::from(scene.width) {
            let mut acc = Rgba::TRANSPARENT;
            if let Some(b) = scene.background {
                acc = composite_over(StudioBlendMode::Normal, acc, Rgba::new([b[0], b[1], b[2]], b[3]), 1.0, PixelSite::default()).unwrap();
            }
            for (l, tile, p) in &prepared {
                let s = p
                    .sample_rgba(x, y, |i, j| Ok::<_, ()>(tile.at(i, j)))
                    .unwrap();
                let src = Rgba::new([s[0], s[1], s[2]], s[3]);
                let opacity = (l.opacity * l.fill_opacity) as f32;
                acc = composite_over(l.mode, acc, src, opacity, PixelSite { x, y, seed: 0 }).unwrap();
            }
            out.push([acc.rgb[0], acc.rgb[1], acc.rgb[2], acc.a]);
        }
    }
    out
}

/// Largest |delta| of alpha and premultiplied colour between a rendered frame and the oracle.
pub fn max_delta(frame: &RenderedScene, want: &[[f32; 4]]) -> f32 {
    let mut worst = 0.0f32;
    for y in 0..frame.height {
        for x in 0..frame.width {
            let got = frame.pixel(x, y).unwrap();
            let w = want[(y * frame.width + x) as usize];
            worst = worst.max((got[3] - w[3]).abs());
            for c in 0..3 {
                worst = worst.max((got[c] * got[3] - w[c] * w[3]).abs());
            }
        }
    }
    worst
}

pub fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}
