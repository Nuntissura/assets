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
use hsk_studio_composite::{
    OwnedMask, RenderedScene, SceneComposition, SceneContent, SceneLayer, SceneMatte, SceneMatteMode,
    ScenePixels, SceneTile, StaticContent,
};
use hsk_studio_pigment::adjust_math::{AdjustmentOp, composite_adjustment};
use hsk_studio_pigment::render_plan::{
    Group, Layer, MaskFormat, MaskInput, OutputTarget, PlanExtensions, PlanNode, PlanNodeId, RenderPlan,
    SourceKey,
};
use std::collections::BTreeMap;
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
                TilePix::new(tile_id(9, "td"), 6, 6, 4),
                TilePix::new(tile_id(10, "te"), 10, 8, 5),
                TilePix::new(tile_id(11, "tf"), 20, 14, 6),
                TilePix::new(tile_id(12, "tg"), 8, 8, 7),
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
            matte: None,
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
            matte: None,
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
                matte: None,
            },
            SceneLayer {
                layer_id: identity("SLYR", 6),
                content: SceneContent::None,
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                matrix: parent,
                matte: None,
            },
        ],
    }
}

pub fn composition3_id() -> String {
    identity("SCOM", 3)
}
pub fn composition4_id() -> String {
    identity("SCOM", 4)
}
/// Frames of composition 3 the renderer tests play: before the nested composition starts (0),
/// fractional nested move (6), integer nested moves (12, 22), after the nested duration (30).
pub const FRAMES3: [u64; 5] = [0, 6, 12, 22, 30];

/// The nested composition 4 at its own frame `inner` (12x10 px, its red background is NOT drawn
/// when nested): layer 9, tile `td` 6x6, anchor (3, 3), position keys inner frame 0 -> 10:
/// (3, 3) -> (9, 7).
pub fn inner_scene4(inner: u64, pixels: &Pixels) -> SceneComposition {
    let t = (inner.min(10) as f64) / 10.0;
    let (px, py) = (lerp(3.0, 9.0, t), lerp(3.0, 7.0, t));
    let td = pixels.get(&tile_id(9, "td")).unwrap();
    SceneComposition {
        composition_id: composition4_id(),
        width: 12,
        height: 10,
        background: None,
        filter: ResampleFilter::Bilinear,
        layers: vec![SceneLayer {
            layer_id: identity("SLYR", 9),
            content: SceneContent::Tiles(vec![td.scene_tile(0, 0)]),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            matrix: Affine2::from_parts(3.0, 3.0, 1.0, 1.0, 0.0, px - 3.0, py - 3.0),
            matte: None,
        }],
    }
}

/// Composition 3 (slice 3: precomposition), 32x24 px, background (0.2, 0.2, 0.2, 1): layer 8
/// shows composition 4 with `start_time` = frame 2 and `stretch` 2 (inner frame =
/// (outer - 2) / 2), anchor = nested centre (6, 5), position (16, 12), rotation 15 deg. Composition
/// 4 lasts 12 frames, so outer frames 0 (inner -1) and 30 (inner 14) show nothing.
pub fn expected_scene3(frame: u64, pixels: &Pixels) -> SceneComposition {
    let inner = if frame >= 2 && (frame - 2) / 2 < 12 {
        inner_scene4((frame - 2) / 2, pixels)
    } else {
        SceneComposition { layers: Vec::new(), ..inner_scene4(0, pixels) }
    };
    SceneComposition {
        composition_id: composition3_id(),
        width: W,
        height: H,
        background: Some([0.2, 0.2, 0.2, 1.0]),
        filter: ResampleFilter::Bilinear,
        layers: vec![SceneLayer {
            layer_id: identity("SLYR", 8),
            content: SceneContent::Precomp(Box::new(inner)),
            mode: StudioBlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
            matrix: Affine2::from_parts(6.0, 5.0, 1.0, 1.0, 15.0, 10.0, 7.0),
            matte: None,
        }],
    }
}

/// Every rendered case: `(label, scene)` for the renderer tests.
pub fn cases(pixels: &Pixels) -> Vec<(String, SceneComposition)> {
    let mut out: Vec<(String, SceneComposition)> =
        FRAMES.iter().map(|f| (format!("comp1/frame{f}"), expected_scene(*f, pixels))).collect();
    out.extend(FRAMES2.iter().map(|f| (format!("comp2/frame{f}"), expected_scene2(*f, pixels))));
    out.extend(FRAMES3.iter().map(|f| (format!("comp3/frame{f}"), expected_scene3(*f, pixels))));
    out.extend(FRAMES5.iter().map(|f| (format!("comp5/frame{f}"), expected_scene5(*f, pixels))));
    out.extend(FRAMES6.iter().map(|f| (format!("comp6/frame{f}"), expected_scene6(*f, pixels))));
    out
}

pub fn composition5_id() -> String {
    identity("SCOM", 5)
}
/// Frames of composition 5: matte inside its window (0, 5) and after it (10).
pub const FRAMES5: [u64; 3] = [0, 5, 10];

/// Layer 10 of composition 5 (the matte source): tile `te` 10x8, window frames [0, 8), anchor
/// (5, 4), position keys frame 0 -> 10: (8, 12) -> (24.5, 12).
pub fn matte_layer10(frame: u64, pixels: &Pixels) -> Option<SceneLayer> {
    if frame >= 8 {
        return None;
    }
    let t = (frame.min(10) as f64) / 10.0;
    let px = lerp(8.0, 24.5, t);
    let te = pixels.get(&tile_id(10, "te")).unwrap();
    Some(SceneLayer {
        layer_id: identity("SLYR", 10),
        content: SceneContent::Tiles(vec![te.scene_tile(0, 0)]),
        mode: StudioBlendMode::Normal,
        opacity: 1.0,
        fill_opacity: 1.0,
        matrix: Affine2::from_parts(5.0, 4.0, 1.0, 1.0, 0.0, px - 5.0, 12.0 - 4.0),
        matte: None,
    })
}

/// Composition 5 (slice 4: track mattes), 32x24 px, background (0.1, 0.1, 0.1, 1). Stack
/// (bottom first): layer 11 (tile `tf` 20x14 at position (16, 12), anchor (10, 7) -> integer
/// move (6, 5)) with an ALPHA matte from layer 10; layer 12 (tile `tg` 8x8 at position (6, 6),
/// anchor (4, 4) -> move (2, 2)) with an INVERTED LUMA matte from the same layer 10. Layer 10
/// itself does not render (STU-CMP-030 default). At frame 10 layer 10 is outside its window:
/// layer 11 shows nothing and layer 12 shows fully.
pub fn expected_scene5(frame: u64, pixels: &Pixels) -> SceneComposition {
    let tf = pixels.get(&tile_id(11, "tf")).unwrap();
    let tg = pixels.get(&tile_id(12, "tg")).unwrap();
    let matte = |mode, inverted| SceneMatte {
        layer: matte_layer10(frame, pixels).map(Box::new),
        mode,
        inverted,
    };
    SceneComposition {
        composition_id: composition5_id(),
        width: W,
        height: H,
        background: Some([0.1, 0.1, 0.1, 1.0]),
        filter: ResampleFilter::Bilinear,
        layers: vec![
            SceneLayer {
                layer_id: identity("SLYR", 11),
                content: SceneContent::Tiles(vec![tf.scene_tile(0, 0)]),
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                matrix: Affine2::translate(6.0, 5.0),
                matte: Some(matte(SceneMatteMode::Alpha, false)),
            },
            SceneLayer {
                layer_id: identity("SLYR", 12),
                content: SceneContent::Tiles(vec![tg.scene_tile(0, 0)]),
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                matrix: Affine2::translate(2.0, 2.0),
                matte: Some(matte(SceneMatteMode::Luma, true)),
            },
        ],
    }
}

/// Effective coverage of a matte over the composition rect, row-major: the matte layer's oracle
/// frame alone (normal mode, no background), alpha or BT.709 luma times alpha, quantised to 16
/// bits like composite's `coverage_u16le` plane, inverted when the matte is inverted.
pub fn matte_coverage(scene: &SceneComposition, m: &SceneMatte, pixels: &Pixels) -> Vec<f32> {
    let n = (scene.width * scene.height) as usize;
    let raw: Vec<f32> = match &m.layer {
        None => vec![0.0; n],
        Some(l) => {
            let alone = SceneComposition {
                background: None,
                layers: vec![SceneLayer { mode: StudioBlendMode::Normal, ..(**l).clone() }],
                ..scene.clone()
            };
            oracle(&alone, pixels)
                .iter()
                .map(|p| match m.mode {
                    SceneMatteMode::Alpha => p[3],
                    SceneMatteMode::Luma => (0.2126f32 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]) * p[3],
                })
                .collect()
        }
    };
    raw.iter()
        .map(|v| {
            let q = (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
            let c = (f64::from(q) / 65535.0) as f32;
            if m.inverted { 1.0 - c } else { c }
        })
        .collect()
}

/// Does any layer of the scene (precompositions and mattes included) need resampling, i.e. has
/// a content matrix other than an exact integer translation?
pub fn scene_resamples(scene: &SceneComposition) -> bool {
    fn layer(l: &SceneLayer) -> bool {
        let [a, b, c, d, e, f] = l.matrix.m;
        let integer = a == 1.0 && b == 0.0 && d == 0.0 && e == 1.0 && c.fract() == 0.0 && f.fract() == 0.0;
        let has_content = !matches!(l.content, SceneContent::None);
        (has_content && !integer)
            || matches!(&l.content, SceneContent::Precomp(n) if scene_resamples(n))
            || l.matte.as_ref().and_then(|m| m.layer.as_deref()).is_some_and(layer)
    }
    scene.layers.iter().any(layer)
}

/// Frames of composition 6 (static subtree and adjustment layer, lane SMB-COMPOSITE-COMPOSITION-2).
pub const FRAMES6: [u64; 2] = [0, 7];
const STATIC_FILL: [f32; 4] = [0.3, 0.8, 0.6, 1.0];

/// The static content of layer 13: a 10x8 solid cut by an owned `coverage_u8` ramp (what a vector
/// or text layer lowers to), content rect (2, 3)-(12, 11).
pub fn static13() -> StaticContent {
    let rect = Rect { x: 2, y: 3, width: 10, height: 8 };
    let mut bytes = Vec::new();
    for y in 0..8u32 {
        for x in 0..10u32 {
            bytes.push(((x * 29 + y * 17) % 256) as u8);
        }
    }
    let sha = hsk_studio_prism::profile_hash(&bytes);
    let plan = RenderPlan {
        revision: 7,
        target: OutputTarget { extent: rect, format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, space: space() },
        nodes: vec![
            PlanNode::Solid { rect, rgba: STATIC_FILL },
            PlanNode::Group(Group {
                isolated: true,
                layers: vec![Layer {
                    input: PlanNodeId(0),
                    mode: StudioBlendMode::Normal,
                    opacity: 1.0,
                    fill_opacity: 1.0,
                    mask: Some(MaskInput { key: SourceKey(0), placement: rect, format: MaskFormat::CoverageU8, sha256: sha, inverted: false, density: 1.0 }),
                }],
            }),
        ],
        root: PlanNodeId(1),
    };
    let mut owned_masks = BTreeMap::new();
    owned_masks.insert(SourceKey(0), OwnedMask { bounds: rect, sample_format: "coverage_u8".into(), bytes, stride_bytes: 10, sha256: sha });
    StaticContent {
        plan,
        extensions: PlanExtensions::default(),
        rect,
        tiles: BTreeMap::new(),
        masks: BTreeMap::new(),
        owned_masks,
        owned_tiles: BTreeMap::new(),
    }
}

/// Composition 6, 32x24, background (0.15, 0.1, 0.05, 1): layer 2 tile `ta` moved (8, 6); layer
/// 13 the static content rotating about its centre (frame 0: 0 deg, frame 7: 21 deg) at position
/// (16, 12), 90 %; layer 14 an INVERT adjustment at 70 % over everything below.
pub fn expected_scene6(frame: u64, pixels: &Pixels) -> SceneComposition {
    let ta = pixels.get(&tile_id(2, "ta")).unwrap();
    let rot = 3.0 * frame as f64;
    SceneComposition {
        composition_id: identity("SCOM", 6),
        width: W,
        height: H,
        background: Some([0.15, 0.1, 0.05, 1.0]),
        filter: ResampleFilter::Bilinear,
        layers: vec![
            SceneLayer {
                layer_id: identity("SLYR", 2),
                content: SceneContent::Tiles(vec![ta.scene_tile(0, 0)]),
                mode: StudioBlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                matrix: Affine2::translate(8.0, 6.0),
                matte: None,
            },
            SceneLayer {
                layer_id: identity("SLYR", 13),
                content: SceneContent::Static(Box::new(static13())),
                mode: StudioBlendMode::Normal,
                opacity: 0.9,
                fill_opacity: 1.0,
                // Content origin (2, 3), anchor = centre (5, 4) of the content.
                matrix: Affine2::translate(-2.0, -3.0).then(Affine2::from_parts(5.0, 4.0, 1.0, 1.0, rot, 11.0, 8.0)),
                matte: None,
            },
            SceneLayer {
                layer_id: identity("SLYR", 14),
                content: SceneContent::Adjustment(AdjustmentOp::Invert),
                mode: StudioBlendMode::Normal,
                opacity: 0.7,
                fill_opacity: 1.0,
                matrix: Affine2::IDENTITY,
                matte: None,
            },
        ],
    }
}

/// The surface pixels of a static content (oracle of [`static13`]: fill times coverage).
fn static_frame(c: &StaticContent) -> Vec<[f32; 4]> {
    let m = c.owned_masks.get(&SourceKey(0)).unwrap();
    m.bytes
        .iter()
        .map(|b| {
            let cov = (f64::from(*b) / 255.0) as f32;
            let r = composite_over(StudioBlendMode::Normal, Rgba::TRANSPARENT,
                Rgba::new([STATIC_FILL[0], STATIC_FILL[1], STATIC_FILL[2]], STATIC_FILL[3]), cov, PixelSite::default()).unwrap();
            [r.rgb[0], r.rgb[1], r.rgb[2], r.a]
        })
        .collect()
}

/// One oracle step, bottom first.
enum Step<'a> {
    Draw(&'a SceneLayer, Texels<'a>, PreparedTransform, Option<Vec<f32>>),
    Adjust(&'a SceneLayer, Box<hsk_studio_pigment::adjust_math::PreparedAdjustment>),
}

/// Where an oracle layer reads its texels.
enum Texels<'a> {
    Tile(&'a TilePix),
    /// A nested oracle frame, row-major, `width` texels per row.
    Frame(Vec<[f32; 4]>, u32),
}
impl Texels<'_> {
    fn at(&self, i: u32, j: u32) -> [f32; 4] {
        match self {
            Self::Tile(t) => t.at(i, j),
            Self::Frame(f, w) => f[(j * w + i) as usize],
        }
    }
}

/// Straight RGBA of every composition pixel, row-major (module docs). A precomposition is the
/// oracle frame of the nested scene, sampled like a tile over the nested rect.
pub fn oracle(scene: &SceneComposition, pixels: &Pixels) -> Vec<[f32; 4]> {
    let mut prepared = Vec::new();
    for l in &scene.layers {
        if l.opacity == 0.0 || l.matte.as_ref().is_some_and(|m| m.layer.is_none() && !m.inverted) {
            continue;
        }
        let mut sources = Vec::new();
        match &l.content {
            SceneContent::Tiles(tiles) => {
                for t in tiles {
                    sources.push((Texels::Tile(pixels.get(&t.tile_id).expect("fixture tile")), t.placement));
                }
            }
            SceneContent::Precomp(nested) => {
                sources.push((Texels::Frame(oracle(nested, pixels), nested.width), nested.rect()));
            }
            SceneContent::Static(c) => {
                sources.push((Texels::Frame(static_frame(c), c.rect.width), c.rect));
            }
            SceneContent::Adjustment(op) => {
                prepared.push(Step::Adjust(l, Box::new(op.prepare().expect("adjustment"))));
                continue;
            }
            _ => {}
        }
        let coverage = l.matte.as_ref().map(|m| matte_coverage(scene, m, pixels));
        for (texels, rect) in sources {
            let p = PreparedTransform::new(&LayerTransform::new(l.matrix, scene.filter), rect)
                .expect("invertible");
            prepared.push(Step::Draw(l, texels, p, coverage.clone()));
        }
    }
    let mut out = Vec::with_capacity((scene.width * scene.height) as usize);
    for y in 0..i64::from(scene.height) {
        for x in 0..i64::from(scene.width) {
            let mut acc = Rgba::TRANSPARENT;
            if let Some(b) = scene.background {
                acc = composite_over(StudioBlendMode::Normal, acc, Rgba::new([b[0], b[1], b[2]], b[3]), 1.0, PixelSite::default()).unwrap();
            }
            for step in &prepared {
                let (l, texels, p, coverage) = match step {
                    Step::Draw(l, texels, p, coverage) => (l, texels, p, coverage),
                    Step::Adjust(l, prep) => {
                        let k = (l.opacity * l.fill_opacity) as f32;
                        acc = composite_adjustment(l.mode, acc, prep, k).unwrap();
                        continue;
                    }
                };
                let s = p.sample_rgba(x, y, |i, j| Ok::<_, ()>(texels.at(i, j))).unwrap();
                let src = Rgba::new([s[0], s[1], s[2]], s[3]);
                let mask = coverage.as_ref().map_or(1.0, |c| c[(y * i64::from(scene.width) + x) as usize]);
                let opacity = (l.opacity * l.fill_opacity) as f32 * mask;
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
