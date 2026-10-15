//! Corner pin from a tracker (lane SMB-AE-KEY-TRACK-2): a four-point or planar track becomes a
//! Folio corner pin on a layer, motion samples it at any tick, and the pinned quad lowers to a
//! pigment projective `LayerTransform` (rendered by render-cpu and render-gpu, see their
//! `cpu_projective.rs` / `gpu_projective.rs`). Exact synthetic tracks, plus one planar track over
//! rendered frames.
#![cfg(feature = "motion")]
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::track_apply::*;
use hsk_studio_folio::{StudioCompositionLayer, StudioLayerTiming, StudioRational};
use hsk_studio_pigment::track_math::*;
use hsk_studio_pigment::transform_math::{Projective2, ResampleFilter};

const TIMING: TrackTiming = TrackTiming { first_tick: 0, ticks_per_frame: 100 };
const FROM: Quad = [[0.0, 0.0], [40.0, 0.0], [40.0, 30.0], [0.0, 30.0]];

fn layer() -> StudioCompositionLayer {
    StudioCompositionLayer {
        layer_id: "SLYR-insert".to_owned(),
        timing: StudioLayerTiming { in_point: 0, out_point: 10_000, start_time: 0, stretch: StudioRational { numerator: 1, denominator: 1 } },
        parent: None,
        precomp: None,
        track_matte: None,
        streams: vec![],
        corner_pin: None,
    }
}

/// The surface quad at frame `t` (a drifting perspective quad).
fn quad(t: f64) -> Quad {
    [[20.0 + 1.5 * t, 10.0 + 0.5 * t], [70.0 + 1.0 * t, 12.0 - 0.25 * t], [74.0 + 0.75 * t, 52.0 + 0.5 * t], [18.0 + 1.25 * t, 48.0 + 0.75 * t]]
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

#[test]
fn four_point_track_pins_a_layer_and_lowers_to_a_projective_transform() {
    let tracks: Vec<Vec<TrackSample>> = (0..4)
        .map(|c| {
            (0..6)
                .map(|f| TrackSample { frame: f, position: quad(f as f64)[c], confidence: 1.0, status: TrackStatus::Tracked })
                .collect()
        })
        .collect();
    let mut lost = tracks.clone();
    lost[2][5].status = TrackStatus::Lost;
    assert_eq!(corner_frames_from_tracks([&lost[0], &lost[1], &lost[2], &lost[3]]).unwrap().len(), 5, "frames with a lost corner are not keyed");
    let frames = corner_frames_from_tracks([&tracks[0], &tracks[1], &tracks[2], &tracks[3]]).unwrap();
    let target = layer();
    let pinned = apply_corner_pin(&target, FROM, &frames, TIMING, [0.0, 0.0]).unwrap();
    assert!(target.corner_pin.is_none(), "the input record is untouched");
    let pin = pinned.corner_pin.as_ref().unwrap();
    assert_eq!(pin.keyframes.len(), 6);
    for f in 0..6 {
        let q = corner_pin_quad_at(pin, 100 * f as i64).unwrap();
        assert_eq!(q, quad(f as f64), "key {f}");
        let t = corner_pin_transform(FROM, q, ResampleFilter::Bilinear).unwrap();
        let p = t.perspective.unwrap();
        for (a, b) in FROM.iter().zip(q) {
            let (x, y) = p.apply(a[0], a[1]).unwrap();
            assert!(dist([x, y], b) < 1e-9, "frame {f}: {a:?} -> {:?} want {b:?}", (x, y));
        }
    }
    // Between keys the corners are linear (linear keyframes, sampled by motion).
    let mid = corner_pin_quad_at(pin, 250).unwrap();
    for c in 0..4 {
        assert!(dist(mid[c], quad(2.5)[c]) < 1e-9, "{mid:?}");
    }
    // Refusals: a bow-tie quad, unsorted frames, an empty track, a non-finite offset.
    let bowtie = [[0.0, 0.0], [40.0, 30.0], [40.0, 0.0], [0.0, 30.0]];
    assert_eq!(apply_corner_pin(&target, bowtie, &frames, TIMING, [0.0; 2]).unwrap_err(), TrackApplyError::Parameter);
    let swapped = vec![frames[1], frames[0]];
    assert_eq!(apply_corner_pin(&target, FROM, &swapped, TIMING, [0.0; 2]).unwrap_err(), TrackApplyError::Parameter);
    assert_eq!(apply_corner_pin(&target, FROM, &[], TIMING, [0.0; 2]).unwrap_err(), TrackApplyError::Parameter);
    assert_eq!(apply_corner_pin(&target, FROM, &frames, TIMING, [f64::NAN, 0.0]).unwrap_err(), TrackApplyError::Parameter);
    assert!(corner_pin_transform(FROM, bowtie, ResampleFilter::Bilinear).is_err());
}

fn tex(x: f64, y: f64) -> f64 {
    0.5 + 0.18 * (0.31 * x + 0.17 * y).sin() + 0.14 * (0.13 * x - 0.29 * y + 1.0).sin() + 0.1 * (0.53 * x + 0.41 * y + 2.0).sin()
}

#[test]
fn planar_track_pins_a_layer_onto_the_tracked_surface() {
    // A textured plane under a perspective ramp; the planar track's quads pin the insert layer.
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
    let surface: Quad = [[20.0, 20.0], [76.0, 20.0], [76.0, 76.0], [20.0, 76.0]];
    let spec = PlanarSpec {
        quad: surface,
        max_features: 48,
        feature_radius: 6,
        search_radius: 8,
        min_spacing_px: 8,
        ransac_iterations: 200,
        inlier_px: 0.25,
        seed: 3,
    };
    let planar = track_planar(&frames, 0, TrackDirection::Forward, &spec, &TrackParams::DEFAULT, &TrackLimits::DEFAULT, &CancellationToken::default(), &mut |_, _| {}).unwrap();
    let pinned = apply_corner_pin(&layer(), FROM, &corner_frames_from_planar(&planar), TIMING, [0.0, 0.0]).unwrap();
    let pin = pinned.corner_pin.unwrap();
    let mut worst = 0.0f64;
    for f in 0..6 {
        let q = corner_pin_quad_at(&pin, 100 * f).unwrap();
        let truth = Projective2 { m: h(f as f64) };
        for (c, got) in surface.iter().zip(q) {
            let (x, y) = truth.apply(c[0], c[1]).unwrap();
            worst = worst.max(dist(got, [x, y]));
        }
    }
    println!("planar corner pin: worst corner error {worst:.5} px");
    assert!(worst < 0.1, "pinned corners follow the surface within 0.1 px, measured {worst}");
}
