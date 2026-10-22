//! Mask paths (lane SMB-AE-KEY-TRACK-3): a planar track written as an animated Folio mask path, and
//! the ordered mask combination of `mask_paths` on exact integer rectangles. The adapter wiring is in
//! `composition_adapter.rs` (refusals test), the render proofs in render-cpu / render-gpu
//! `composition_mask_paths.rs`.
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::mask_paths::{MaskAtTick, mask_coverage};
use hsk_studio_composite::track_apply::{TrackApplyError, TrackTiming, apply_planar_mask};
use hsk_studio_folio::{StudioCompositionLayer, StudioCompositionMaskMode, StudioLayerTiming, StudioRational};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::track_math::*;
use hsk_studio_pigment::transform_math::{Affine2, Projective2};

fn tex(x: f64, y: f64) -> f64 {
    0.5 + 0.18 * (0.31 * x + 0.17 * y).sin() + 0.14 * (0.13 * x - 0.29 * y + 1.0).sin() + 0.1 * (0.53 * x + 0.41 * y + 2.0).sin()
}

fn layer() -> StudioCompositionLayer {
    StudioCompositionLayer {
        layer_id: "SLYR-footage".to_owned(),
        timing: StudioLayerTiming { in_point: 0, out_point: 10_000, start_time: 0, stretch: StudioRational { numerator: 1, denominator: 1 } },
        parent: None,
        precomp: None,
        track_matte: None,
        streams: vec![],
        corner_pin: None,
        masks: vec![],
        three_d: None,
        camera: None,
        light: None,
    }
}

#[test]
fn a_planar_track_becomes_an_animated_mask_path() {
    let h = |t: f64| -> [f64; 9] {
        let target = [1.05, 0.04, -3.0, 0.02, 0.97, 2.0, 0.0004, -0.0003, 1.0];
        let id = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        std::array::from_fn(|k| id[k] + t / 5.0 * (target[k] - id[k]))
    };
    let frames: Vec<LumaFrame> = (0..6)
        .map(|t| {
            let inv = Projective2 { m: h(f64::from(t)) }.inverse().unwrap();
            let mut data = Vec::new();
            for j in 0..96u32 {
                for i in 0..96u32 {
                    let (x, y) = inv.apply(f64::from(i) + 0.5, f64::from(j) + 0.5).unwrap();
                    data.push(tex(x, y) as f32);
                }
            }
            LumaFrame::new(96, 96, data).unwrap()
        })
        .collect();
    let spec = PlanarSpec {
        quad: [[20.0, 20.0], [76.0, 20.0], [76.0, 76.0], [20.0, 76.0]],
        max_features: 48,
        feature_radius: 6,
        search_radius: 8,
        min_spacing_px: 8,
        ransac_iterations: 200,
        inlier_px: 0.25,
        seed: 3,
    };
    let planar = track_planar(&frames, 0, TrackDirection::Forward, &spec, &TrackParams::DEFAULT, &TrackLimits::DEFAULT, &CancellationToken::default(), &mut |_, _| {}).unwrap();
    // A pentagon with one smooth vertex (handles) on the tracked surface.
    let path = [30.0, 30.0, 0.0, 0.0, 0.0, 0.0, 60.0, 28.0, 0.0, 0.0, 0.0, 0.0, 66.0, 50.0, -3.0, -4.0, 3.0, 4.0, 48.0, 64.0, 0.0, 0.0, 0.0, 0.0, 28.0, 52.0, 0.0, 0.0, 0.0, 0.0];
    let timing = TrackTiming { first_tick: 0, ticks_per_frame: 100 };
    let target = layer();
    let out = apply_planar_mask(&target, "planar", StudioCompositionMaskMode::Add, &path, &planar, timing).unwrap();
    assert!(target.masks.is_empty(), "the input record is untouched");
    let mask = &out.masks[0];
    assert_eq!((mask.mask_id.as_str(), mask.closed, mask.keyframes.len()), ("planar", true, 6));
    let mut worst = 0.0f64;
    for (f, k) in mask.keyframes.iter().enumerate() {
        assert_eq!(k.time, 100 * f as u64);
        let truth = Projective2 { m: h(f as f64) };
        for (v, got) in path.chunks_exact(6).zip(k.value.chunks_exact(6)) {
            let (x, y) = truth.apply(v[0], v[1]).unwrap();
            let (hx, hy) = truth.apply(v[0] + v[4], v[1] + v[5]).unwrap();
            worst = worst.max((got[0] - x).hypot(got[1] - y)).max((got[4] - (hx - x)).hypot(got[5] - (hy - y)));
        }
    }
    println!("planar mask: worst vertex / handle error {worst:.5} px");
    assert!(worst < 0.1, "mask vertices follow the surface within 0.1 px, measured {worst}");
    // Writing again replaces the mask of the same id; hostile paths are refused.
    let again = apply_planar_mask(&out, "planar", StudioCompositionMaskMode::Subtract, &path, &planar, timing).unwrap();
    assert_eq!((again.masks.len(), again.masks[0].mode), (1, StudioCompositionMaskMode::Subtract));
    for bad in [vec![1.0; 5], vec![], vec![f64::NAN; 6]] {
        assert_eq!(apply_planar_mask(&target, "m", StudioCompositionMaskMode::Add, &bad, &planar, timing).unwrap_err(), TrackApplyError::Parameter);
    }
    assert_eq!(apply_planar_mask(&target, "", StudioCompositionMaskMode::Add, &path, &planar, timing).unwrap_err(), TrackApplyError::Parameter);
}

fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<f64> {
    vec![x0, y0, 0.0, 0.0, 0.0, 0.0, x1, y0, 0.0, 0.0, 0.0, 0.0, x1, y1, 0.0, 0.0, 0.0, 0.0, x0, y1, 0.0, 0.0, 0.0, 0.0]
}

#[test]
fn masks_combine_in_order_on_exact_rectangles() {
    let rect = Rect { x: 0, y: 0, width: 12, height: 10 };
    let mask = |mode, inverted, path: Vec<f64>| MaskAtTick { mode, inverted, closed: true, path };
    let a = rect_path(2.0, 2.0, 10.0, 8.0);
    let b = rect_path(4.0, 4.0, 6.0, 6.0);
    let inside = |r: [i64; 4], x: i64, y: i64| x >= r[0] && x < r[2] && y >= r[1] && y < r[3];
    let check = |masks: Vec<MaskAtTick>, want: &dyn Fn(i64, i64) -> f32| {
        let cov = mask_coverage(&masks, &Affine2::IDENTITY, rect, 120).unwrap().unwrap();
        for y in 0..10 {
            for x in 0..12 {
                assert_eq!(cov[(y * 12 + x) as usize], want(x, y), "({x},{y})");
            }
        }
    };
    let ia = |x, y| if inside([2, 2, 10, 8], x, y) { 1.0 } else { 0.0 };
    let ib = |x, y| if inside([4, 4, 6, 6], x, y) { 1.0 } else { 0.0 };
    use StudioCompositionMaskMode as M;
    check(vec![mask(M::Add, false, a.clone())], &ia);
    check(vec![mask(M::Add, false, a.clone()), mask(M::Subtract, false, b.clone())], &|x, y| ia(x, y) * (1.0 - ib(x, y)));
    check(vec![mask(M::Add, false, a.clone()), mask(M::Intersect, false, b.clone())], &ib);
    check(vec![mask(M::Add, false, a.clone()), mask(M::Difference, false, b.clone())], &|x, y| (ia(x, y) - ib(x, y)).abs());
    check(vec![mask(M::Add, true, a.clone())], &|x, y| 1.0 - ia(x, y));
    // A first subtracting mask cuts from the whole layer; `none` contributes nothing.
    check(vec![mask(M::None, false, a.clone()), mask(M::Subtract, false, b.clone())], &|x, y| 1.0 - ib(x, y));
    assert!(mask_coverage(&[mask(M::None, false, a)], &Affine2::IDENTITY, rect, 120).unwrap().is_none());
    // The plane bound is refused, not truncated.
    assert!(mask_coverage(&[mask(M::Add, false, b)], &Affine2::IDENTITY, rect, 119).is_err());
}
