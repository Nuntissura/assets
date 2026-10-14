//! Layer transforms on the CPU backend (lane SMB-PIGMENT-TRANSFORM slice 2): `execute_extended`
//! resamples a transformed tile layer with pigment's binary64 reference sampler
//! (`transform_math::PreparedTransform`), so every chunking and thread count must give the bits the
//! pigment reference gives, an identity transform must give the untransformed plan's bytes, and a
//! linked mask must move with its content.
use hsk_studio_pigment::blend_math::{PixelSite, Rgba, composite_over};
use hsk_studio_pigment::effect_math::{ColourOverlayOp, EffectOp};
use hsk_studio_pigment::render_plan::*;
use hsk_studio_pigment::transform_math::*;
use hsk_studio_render_cpu::Renderer;

mod common;
use common::*;

const NORMAL: StudioBlendMode = StudioBlendMode::Normal;
const MULTIPLY: StudioBlendMode = StudioBlendMode::Multiply;
const FILTERS: [ResampleFilter; 3] = [
    ResampleFilter::NearestNeighbour,
    ResampleFilter::Bilinear,
    ResampleFilter::Bicubic,
];

/// Colour ramp with a non-binary alpha and transparent holes (colour kept where alpha is 0).
fn tile_px(x: u32, y: u32) -> [f32; 4] {
    let f = |n: u32, d: f32| f32::from(u16::try_from(n % 1000).unwrap()) / d;
    let hole = x.wrapping_mul(3).wrapping_add(y.wrapping_mul(5)).is_multiple_of(7);
    [
        f(x, 7.0).min(1.0),
        f(y, 5.0).min(1.0),
        f(x.wrapping_add(y) % 4, 4.0),
        if hole { 0.0 } else { f(x.wrapping_add(y) % 5 + 1, 5.0) },
    ]
}

/// 0: opaque backdrop solid; 1: the 7x5 tile at (3, 2); 2: root group [solid Normal, tile Multiply
/// 0.8 (optionally masked)].
fn plan_for(tile: &TileData, mask: Option<MaskInput>) -> RenderPlan {
    let extent = rect(0, 0, 16, 14);
    let mut top = layer(1, MULTIPLY, 0.8);
    top.mask = mask;
    plan(
        extent,
        vec![
            solid(extent, [0.9, 0.7, 0.5, 1.0]),
            PlanNode::Tile(tile.input_at(1, 3, 2)),
            group(vec![layer(0, NORMAL, 1.0), top]),
        ],
        2,
    )
}

fn transformed(t: LayerTransform) -> PlanExtensions {
    envelope(vec![one_layer(2, 1).with_transform(t)])
}

fn bits(grid: &Grid) -> Vec<[u32; 4]> {
    grid.px.iter().map(|p| p.map(f32::to_bits)).collect()
}

fn render(renderer: &Renderer, plan: &RenderPlan, ext: &PlanExtensions, sources: &Sources) -> Grid {
    let (result, grid) = run_ext(renderer, plan, ext, Exactness::Approximate, 1 << 26, sources);
    result.unwrap();
    grid
}

/// What render-cpu's compositor does with one straight sample (pigment's `composite_over`).
fn over(mode: StudioBlendMode, b: [f32; 4], s: [f32; 4], opacity: f32, x: i64, y: i64) -> [f32; 4] {
    if opacity <= 0.0 || s[3] <= 0.0 {
        return b;
    }
    let out = composite_over(
        mode,
        Rgba::new([b[0], b[1], b[2]], b[3]),
        Rgba::new([s[0], s[1], s[2]], s[3]),
        opacity,
        PixelSite { x, y, seed: 7 },
    )
    .unwrap();
    [out.rgb[0], out.rgb[1], out.rgb[2], out.a.min(1.0)]
}

#[test]
fn cpu_matches_pigment_reference_for_every_chunking_and_thread_count() {
    let tile = TileData::new(7, 5, tile_px);
    let sources = Sources {
        tiles: vec![(1, tile.planes())],
        masks: vec![],
    };
    let plan = plan_for(&tile, None);
    let src = rect(3, 2, 7, 5);
    // Scale 1.37 x 0.85, rotate 23 degrees about the tile centre, move by (0.3, 1.6).
    let affine = Affine2::from_parts(6.5, 4.5, 1.37, 0.85, 23.0, 0.3, 1.6);
    for filter in FILTERS {
        for edge in [EdgeMode::Transparent, EdgeMode::Clamp] {
            let t = LayerTransform::new(affine, filter).with_edge(edge);
            let prepared = PreparedTransform::new(&t, src).unwrap();
            let read = |i: u32, j: u32| Ok::<_, ()>(tile_px(i, j));
            let mut want = Vec::new();
            for y in 0..14i64 {
                for x in 0..16i64 {
                    let b = over(NORMAL, [0.0; 4], [0.9, 0.7, 0.5, 1.0], 1.0, x, y);
                    let s = prepared.sample_rgba(x, y, read).unwrap();
                    want.push(over(MULTIPLY, b, s, 0.8, x, y).map(f32::to_bits));
                }
            }
            let ext = transformed(t);
            for (chunk, threads) in [(1, 1), (3, 1), (5, 4), (64, 1), (4, 8)] {
                let r = Renderer::with_threads(chunk, threads).unwrap();
                let got = render(&r, &plan, &ext, &sources);
                assert_eq!(bits(&got), want, "{filter:?} {edge:?} chunk {chunk} x{threads}");
            }
        }
    }
}

#[test]
fn identity_and_integer_moves_equal_the_untransformed_plan() {
    let tile = TileData::new(7, 5, tile_px);
    let sources = Sources {
        tiles: vec![(1, tile.planes())],
        masks: vec![],
    };
    let renderer = Renderer::with_threads(4, 2).unwrap();
    let plan = plan_for(&tile, None);
    let plain = render(&renderer, &plan, &PlanExtensions::default(), &sources);
    // The same tile placed 2 right and 3 down, without a transform.
    let mut moved_plan = plan_for(&tile, None);
    moved_plan.nodes[1] = PlanNode::Tile(tile.input_at(1, 5, 5));
    let moved = render(&renderer, &moved_plan, &PlanExtensions::default(), &sources);
    for filter in FILTERS {
        for edge in [EdgeMode::Transparent, EdgeMode::Clamp] {
            let identity = LayerTransform::new(Affine2::IDENTITY, filter).with_edge(edge);
            let got = render(&renderer, &plan, &transformed(identity), &sources);
            assert_eq!(bits(&got), bits(&plain), "identity {filter:?} {edge:?}");
            let shift = LayerTransform::new(Affine2::translate(2.0, 3.0), filter).with_edge(edge);
            let got = render(&renderer, &plan, &transformed(shift), &sources);
            assert_eq!(bits(&got), bits(&moved), "translate {filter:?} {edge:?}");
        }
    }
}

#[test]
fn linked_mask_moves_with_its_content_and_unlinked_mask_stays() {
    // 4x4 tile at (3, 2) with a 4x4 u8 mask at the origin -> placed with the tile at (3, 2) below.
    let tile = TileData::new(4, 4, tile_px);
    let mask = MaskData::u8(16, 14, |x, y| {
        u8::try_from(x.wrapping_mul(16).wrapping_add(y.wrapping_mul(3)) % 256).unwrap()
    });
    let sources = Sources {
        tiles: vec![(1, tile.planes())],
        masks: vec![(9, &mask)],
    };
    let renderer = Renderer::with_threads(3, 2).unwrap();
    let mut plan = plan_for(&tile, Some(mask.input(9, false, 1.0)));
    plan.nodes[1] = PlanNode::Tile(tile.input_at(1, 3, 2));
    // A quarter turn about the tile centre (5, 4): an exact copy for every filter.
    let quarter = Affine2::from_parts(5.0, 4.0, 1.0, 1.0, 90.0, 0.0, 0.0);
    for filter in FILTERS {
        let t = LayerTransform::new(quarter, filter);
        let prepared = PreparedTransform::new(&t, rect(3, 2, 4, 4)).unwrap();
        let mask_prepared = PreparedTransform::new(&t, mask.bounds).unwrap();
        let read = |i: u32, j: u32| Ok::<_, ()>(tile_px(i, j));
        let coverage = |x: i64, y: i64, linked: bool| -> f32 {
            let raw = |mx: i64, my: i64| {
                f32::from(u8::try_from(mx.wrapping_mul(16).wrapping_add(my.wrapping_mul(3)) % 256).unwrap())
            };
            if linked {
                mask_prepared
                    .sample_coverage(x, y, |i, j| Ok::<_, ()>((f64::from(raw(i64::from(i), i64::from(j))) / 255.0) as f32))
                    .unwrap()
            } else {
                (f64::from(raw(x, y)) / 255.0) as f32
            }
        };
        for linked in [false, true] {
            let got = render(&renderer, &plan, &transformed(t.with_mask(linked)), &sources);
            for y in 0..14i64 {
                for x in 0..16i64 {
                    let b = over(NORMAL, [0.0; 4], [0.9, 0.7, 0.5, 1.0], 1.0, x, y);
                    let m = coverage(x, y, linked);
                    let s = prepared.sample_rgba(x, y, read).unwrap();
                    let want = if m <= 0.0 { b } else { over(MULTIPLY, b, s, 0.8 * m, x, y) };
                    assert_eq!(
                        got.at(x, y).map(f32::to_bits),
                        want.map(f32::to_bits),
                        "{filter:?} linked {linked} ({x},{y})"
                    );
                }
            }
        }
        // The rotated content lands on (3..7, 2..6) as texel (y-2, 3-(x-3)) rotated: spot check one.
        let s = prepared.sample_rgba(6, 2, read).unwrap();
        assert_eq!(s, tile_px(0, 0));
    }
}

#[test]
fn hostile_and_undefined_transforms_are_refused_before_any_sink_call() {
    let tile = TileData::new(7, 5, tile_px);
    let sources = Sources {
        tiles: vec![(1, tile.planes())],
        masks: vec![],
    };
    let plan = plan_for(&tile, None);
    let renderer = Renderer::new(8).unwrap();
    let refuse = |ext: PlanExtensions, want: BackendError| {
        let (result, grid) =
            run_ext(&renderer, &plan, &ext, Exactness::Approximate, 1 << 26, &sources);
        assert_eq!(result, Err(want));
        assert_eq!(grid.calls, 0);
    };
    let matrix = |m: [f64; 6], error| {
        refuse(
            transformed(LayerTransform::new(Affine2 { m }, ResampleFilter::Bicubic)),
            BackendError::InvalidPlan(PlanError::TransformMatrix { node: 2, error }),
        );
    };
    matrix([f64::NAN, 0.0, 0.0, 0.0, 1.0, 0.0], TransformError::NonFinite);
    matrix([1.0, 0.0, f64::INFINITY, 0.0, 1.0, 0.0], TransformError::NonFinite);
    matrix([2.0, 4.0, 0.0, 1.0, 2.0, 0.0], TransformError::Singular);
    matrix([5000.0, 0.0, 0.0, 0.0, 1.0, 0.0], TransformError::ScaleOutOfRange);
    matrix([1.0, 0.0, 0.0, 0.0, 1.0, -1.0e9], TransformError::TranslationOutOfRange);
    let ok = LayerTransform::new(Affine2::translate(0.5, 0.5), ResampleFilter::Bilinear);
    // Transform on the solid layer (not a tile) and transform together with effects.
    refuse(
        envelope(vec![one_layer(2, 0).with_transform(ok)]),
        BackendError::InvalidPlan(PlanError::TransformTarget { node: 2 }),
    );
    refuse(
        envelope(vec![one_layer(2, 1).with_transform(ok).with_effects(vec![
            EffectOp::ColourOverlay(ColourOverlayOp::new([1.0, 0.0, 0.0], NORMAL, 1.0)),
        ])]),
        BackendError::InvalidPlan(PlanError::TransformCombination { node: 2 }),
    );
    // The class is Approximate: an Exact floor refuses it under the filter key.
    let (result, grid) =
        run_ext(&renderer, &plan, &transformed(ok), Exactness::Exact, 1 << 26, &sources);
    assert_eq!(
        result,
        Err(BackendError::Unsupported {
            operator: "transform_bilinear",
            exactness: Exactness::Approximate
        })
    );
    assert_eq!(grid.calls, 0);
    // Supported and recorded in the receipt ledger.
    let (result, _) =
        run_ext(&renderer, &plan, &transformed(ok), Exactness::Approximate, 1 << 26, &sources);
    let receipt = result.unwrap();
    assert!(receipt
        .exactness
        .operators
        .iter()
        .any(|o| o.key == "transform_bilinear" && o.exactness == Exactness::Approximate));
}
