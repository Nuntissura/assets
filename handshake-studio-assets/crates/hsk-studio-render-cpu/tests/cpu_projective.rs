//! Projective (perspective) layer transforms on the CPU backend (lane SMB-AE-KEY-TRACK-2): a corner
//! pin lowered to `LayerTransform::projective` is resampled with pigment's binary64 reference
//! sampler, so every chunking and thread count must give the bits the pigment reference gives, and
//! an identity perspective must give the untransformed plan's bytes.
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::transform_math::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const MULTIPLY: StudioBlendMode = StudioBlendMode::Multiply;
const FILTERS: [ResampleFilter; 3] = [ResampleFilter::NearestNeighbour, ResampleFilter::Bilinear, ResampleFilter::Bicubic];
const BACK: [f32; 4] = [0.9, 0.7, 0.5, 1.0];

fn tile_px(x: u32, y: u32) -> [f32; 4] {
    let hole = (x * 3 + y * 5).is_multiple_of(7);
    [(x as f32 / 7.0).min(1.0), (y as f32 / 5.0).min(1.0), ((x + y) % 4) as f32 / 4.0, if hole { 0.0 } else { ((x + y) % 5 + 1) as f32 / 5.0 }]
}

/// 0: backdrop; 1: the 7x5 tile at (3, 2); 2: root [backdrop, tile Multiply 0.8].
fn plan_for(tile: &TileData) -> RenderPlan {
    let extent = rect(0, 0, 18, 15);
    plan(
        extent,
        vec![solid(extent, BACK), PlanNode::Tile(tile.input_at(1, 3, 2)), group(vec![layer(0, NORMAL, 1.0), layer(1, MULTIPLY, 0.8)])],
        2,
    )
}

fn over(mode: StudioBlendMode, b: [f32; 4], s: [f32; 4], opacity: f32, x: i64, y: i64) -> [f32; 4] {
    if opacity <= 0.0 || s[3] <= 0.0 {
        return b;
    }
    let out = composite_over(mode, Rgba::new([b[0], b[1], b[2]], b[3]), Rgba::new([s[0], s[1], s[2]], s[3]), opacity, PixelSite { x, y, seed: 7 })
        .unwrap();
    [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)]
}

fn bits(grid: &Grid) -> Vec<[u32; 4]> {
    grid.px.iter().map(|p| p.map(f32::to_bits)).collect()
}

/// The tile rectangle (3, 2)-(10, 7) pinned onto a perspective quad.
const FROM: [[f64; 2]; 4] = [[3.0, 2.0], [10.0, 2.0], [10.0, 7.0], [3.0, 7.0]];
const TO: [[f64; 2]; 4] = [[2.3, 1.6], [15.2, 3.1], [13.4, 12.7], [1.1, 9.8]];

#[test]
fn corner_pin_matches_the_pigment_reference_for_every_chunking() {
    let tile = TileData::new(7, 5, tile_px);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![] };
    let plan = plan_for(&tile);
    let p = Projective2::from_quads(FROM, TO).unwrap();
    for (f, t) in FROM.iter().zip(TO) {
        let (x, y) = p.apply(f[0], f[1]).unwrap();
        assert!((x - t[0]).abs() < 1e-9 && (y - t[1]).abs() < 1e-9, "corner maps exactly");
    }
    for filter in FILTERS {
        for edge in [EdgeMode::Transparent, EdgeMode::Clamp] {
            let t = LayerTransform::projective(p, filter).with_edge(edge);
            let prepared = PreparedTransform::new(&t, rect(3, 2, 7, 5)).unwrap();
            assert!(prepared.projective_inverse().is_some());
            let mut want = Vec::new();
            let mut covered = 0;
            for y in 0..15i64 {
                for x in 0..18i64 {
                    let b = over(NORMAL, [0.0; 4], BACK, 1.0, x, y);
                    let s = prepared.sample_rgba(x, y, |i, j| Ok::<_, ()>(tile_px(i, j))).unwrap();
                    covered += usize::from(s[3] > 0.0);
                    want.push(over(MULTIPLY, b, s, 0.8, x, y).map(f32::to_bits));
                }
            }
            assert!(covered > 60, "the pinned quad covers more than the 35-pixel tile: {covered}");
            let ext = envelope(vec![one_layer(2, 1).with_transform(t)]);
            for (chunk, threads) in [(1, 1), (3, 1), (5, 4), (64, 1), (4, 8)] {
                let (result, grid) = run_ext(&Renderer::with_threads(chunk, threads).unwrap(), &plan, &ext, Exactness::Approximate, 1 << 26, &sources);
                result.unwrap();
                assert_eq!(bits(&grid), want, "{filter:?} {edge:?} chunk {chunk} x{threads}");
            }
        }
    }
    // The identity perspective is the untransformed plan bit for bit; without a perspective the
    // record serialises as before (no `perspective` key, digests unchanged).
    let (plain, plain_grid) = run_ext(&Renderer::new(5).unwrap(), &plan, &envelope(vec![]), Exactness::Approximate, 1 << 26, &sources);
    plain.unwrap();
    let ident = envelope(vec![one_layer(2, 1).with_transform(LayerTransform::projective(Projective2::IDENTITY, ResampleFilter::Bicubic))]);
    let (r, grid) = run_ext(&Renderer::new(5).unwrap(), &plan, &ident, Exactness::Approximate, 1 << 26, &sources);
    r.unwrap();
    assert_eq!(bits(&grid), bits(&plain_grid));
    let json = serde_json::to_string(&LayerTransform::new(Affine2::IDENTITY, ResampleFilter::Bilinear)).unwrap();
    assert!(!json.contains("perspective"), "{json}");
}

#[test]
fn hostile_projective_maps_are_refused_typed_before_any_sink_call() {
    let tile = TileData::new(7, 5, tile_px);
    let sources = Sources { tiles: vec![(1, tile.planes())], masks: vec![] };
    let plan = plan_for(&tile);
    let refused = |m: [f64; 9], error: TransformError| {
        let t = LayerTransform::projective(Projective2 { m }, ResampleFilter::Bilinear);
        let (result, grid) = run_ext(&Renderer::new(4).unwrap(), &plan, &envelope(vec![one_layer(2, 1).with_transform(t)]), Exactness::Approximate, 1 << 26, &sources);
        assert_eq!(result.map(|_| ()), Err(BackendError::InvalidPlan(PlanError::TransformMatrix { node: 2, error })), "{m:?}");
        assert_eq!(grid.calls, 0);
    };
    // The plane crosses the horizon inside the tile footprint (weight 1 - 0.2 x <= 0 at x = 5).
    refused([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, -0.2, 0.0, 1.0], TransformError::Singular);
    refused([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0], TransformError::Singular);
    refused([f64::NAN, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0], TransformError::NonFinite);
    // A near-singular squash: the local linear part leaves [1/1024, 1024].
    refused([1.0, 0.0, 0.0, 0.0, 1e-6, 0.0, 0.0, 0.0, 1.0], TransformError::ScaleOutOfRange);
    // Collinear target corners are no homography.
    assert_eq!(Projective2::from_quads(FROM, [[0.0, 0.0], [1.0, 1.0], [2.0, 2.0], [3.0, 3.0]]), Err(TransformError::Singular));
}
