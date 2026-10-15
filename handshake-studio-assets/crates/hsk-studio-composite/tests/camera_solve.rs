//! Lane SMB-AE-3D slice 4: 3D camera solve (STU-CMP-065 "3D camera solve"; parameter set gap
//! STU-CMP-065b, Studio definition in `camera_solve` module docs). Synthetic footage: a known point
//! cloud seen by a known moving camera (the `three_d` camera model); exact tracks must give the
//! path and cloud back up to the solve's scale gauge, and the written camera layer must reproject
//! the cloud onto the tracks; noisy, partly occluded tracks stay within a stated error; hostile
//! input is refused before any work.
use hsk_studio_composite::camera_solve::*;
use hsk_studio_composite::three_d::{Vec3, camera, layer_rotation, mat_vec, transpose};
use hsk_studio_composite::track_apply::TrackTiming;
use hsk_studio_folio::{StudioLayerProperty, StudioLayerTiming, StudioRational};

const W: u32 = 320;
const H: u32 = 240;
const ZOOM: f64 = 400.0;
const FRAMES: usize = 12;

fn hash(i: u64) -> f64 {
    let mut x = i.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    x ^= x >> 31;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 29;
    (x >> 11) as f64 / (1u64 << 53) as f64
}
fn cloud() -> Vec<Vec3> {
    (0..64u64).map(|i| [60.0 + 200.0 * hash(3 * i), 40.0 + 160.0 * hash(3 * i + 1), -80.0 + 200.0 * hash(3 * i + 2)]).collect()
}
/// Truth camera at frame f: position, rx, ry, rz.
fn truth(f: usize) -> (Vec3, f64, f64, f64) {
    let t = f as f64;
    ([160.0 + 8.0 * t, 120.0 - 3.0 * t, -400.0 + 5.0 * t], 0.8 * t, -1.2 * t, 0.5 * t)
}
fn project(pos: Vec3, rx: f64, ry: f64, rz: f64, p: Vec3) -> [f64; 2] {
    let rc = layer_rotation([0.0; 3], rx, ry, rz);
    let q = mat_vec(&transpose(&rc), [p[0] - pos[0], p[1] - pos[1], p[2] - pos[2]]);
    [f64::from(W) / 2.0 + ZOOM * q[0] / q[2], f64::from(H) / 2.0 + ZOOM * q[1] / q[2]]
}
fn tracks(noise: f64, drop: f64) -> Vec<Vec<Option<[f64; 2]>>> {
    cloud()
        .iter()
        .enumerate()
        .map(|(i, p)| {
            (0..FRAMES)
                .map(|f| {
                    let (c, rx, ry, rz) = truth(f);
                    let k = (i * FRAMES + f) as u64;
                    if f > 0 && hash(1000 + k) < drop {
                        return None;
                    }
                    let s = project(c, rx, ry, rz, *p);
                    Some([s[0] + noise * (2.0 * hash(5000 + 2 * k) - 1.0), s[1] + noise * (2.0 * hash(5001 + 2 * k) - 1.0)])
                })
                .collect()
        })
        .collect()
}
fn dist(a: Vec3, b: Vec3) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}
/// Worst camera position error (relative to the path length) and worst rotation error (degrees)
/// after the solve's scale gauge is aligned to the truth (frame 0 is shared exactly).
fn path_error(s: &CameraSolve) -> (f64, f64) {
    let c0 = s.cameras[0].position;
    let t0 = truth(0).0;
    let last = FRAMES - 1;
    let k = dist(truth(last).0, t0) / dist(s.cameras[last].position, c0);
    let (mut pos, mut rot) = (0.0f64, 0.0f64);
    for (f, c) in s.cameras.iter().enumerate() {
        let (tp, rx, ry, rz) = truth(f);
        let aligned = [t0[0] + k * (c.position[0] - c0[0]), t0[1] + k * (c.position[1] - c0[1]), t0[2] + k * (c.position[2] - c0[2])];
        pos = pos.max(dist(aligned, tp) / dist(truth(last).0, t0));
        rot = rot.max((c.rx - rx).abs()).max((c.ry - ry).abs()).max((c.rz - rz).abs());
    }
    (pos, rot)
}

#[test]
fn exact_tracks_recover_the_path_and_the_written_camera_reprojects_the_cloud() {
    let tr = tracks(0.0, 0.0);
    let s = solve_camera(&tr, ZOOM, W, H).expect("solve");
    let (pos, rot) = path_error(&s);
    println!("camera solve exact: rms {:.3e} px, path {pos:.3e} (relative), rotation {rot:.3e} deg", s.rms_px);
    assert!(s.rms_px < 1e-6, "{}", s.rms_px);
    assert!(pos < 1e-6 && rot < 1e-6, "{pos} {rot}");
    assert_eq!(s.cameras[0].position, [160.0, 120.0, -400.0], "frame 0 sits at the default camera pose");
    // The written camera layer: keys per frame reproduce the solved camera, which reprojects the
    // solved cloud onto the tracks.
    let timing = StudioLayerTiming { in_point: 0, out_point: 1200, start_time: 0, stretch: StudioRational { numerator: 1, denominator: 1 } };
    let layer = camera_layer(&s, "SLYR-camera", timing, TrackTiming { first_tick: 0, ticks_per_frame: 100 }).unwrap();
    let d = layer.three_d.as_ref().unwrap();
    let stream = |p: StudioLayerProperty| &layer.streams.iter().find(|s| s.property == p).unwrap().keyframes;
    let (xy, rz) = (stream(StudioLayerProperty::Position), stream(StudioLayerProperty::Rotation));
    let (z, rx, ry) = (&d.position_z.as_ref().unwrap().keyframes, &d.rotation_x.as_ref().unwrap().keyframes, &d.rotation_y.as_ref().unwrap().keyframes);
    assert_eq!((xy.len(), xy[3].time), (FRAMES, 300));
    let mut worst = 0.0f64;
    for f in 0..FRAMES {
        let cam = camera([xy[f].value[0], xy[f].value[1], z[f].value[0]], [0.0; 3], rx[f].value[0], ry[f].value[0], rz[f].value[0], None, ZOOM, W, H).unwrap();
        for (i, p) in s.points.iter().enumerate() {
            let p = p.unwrap();
            let q = mat_vec(&transpose(&cam.rotation), [p[0] - cam.position[0], p[1] - cam.position[1], p[2] - cam.position[2]]);
            let o = tr[i][f].unwrap();
            worst = worst.max((cam.centre[0] + ZOOM * q[0] / q[2] - o[0]).hypot(cam.centre[1] + ZOOM * q[1] / q[2] - o[1]));
        }
    }
    println!("camera layer reprojection: worst {worst:.3e} px");
    assert!(worst < 1e-6, "{worst}");
    assert_eq!(layer.camera.as_ref().unwrap().zoom.static_value, vec![ZOOM]);
    let target = target_layer(&s, 5, "SLYR-target-5", timing).unwrap();
    let p5 = s.points[5].unwrap();
    assert_eq!((target.streams[0].static_value.clone(), target.three_d.unwrap().position_z.unwrap().static_value), (vec![p5[0], p5[1]], vec![p5[2]]));
}

#[test]
fn noisy_occluded_tracks_stay_within_bounds_and_hostile_input_is_refused() {
    let s = solve_camera(&tracks(0.3, 0.1), ZOOM, W, H).expect("solve");
    let (pos, rot) = path_error(&s);
    println!("camera solve noisy (+-0.3 px, 10% dropped): rms {:.3} px, path {pos:.3e} (relative), rotation {rot:.3e} deg", s.rms_px);
    assert!(s.rms_px < 0.3, "{}", s.rms_px);
    assert!(pos < 0.02 && rot < 0.2, "{pos} {rot}");
    // Refusals before any work.
    let good = tracks(0.0, 0.0);
    let mut short = good.clone();
    short[3].pop();
    assert_eq!(solve_camera(&short, ZOOM, W, H).unwrap_err(), CameraSolveError::Parameter);
    let mut nan = good.clone();
    nan[0][2] = Some([f64::NAN, 1.0]);
    assert_eq!(solve_camera(&nan, ZOOM, W, H).unwrap_err(), CameraSolveError::Parameter);
    assert_eq!(solve_camera(&good, 0.0, W, H).unwrap_err(), CameraSolveError::Parameter);
    assert_eq!(solve_camera(&vec![vec![None; 2]; MAX_SOLVE_POINTS + 1], ZOOM, W, H).unwrap_err(), CameraSolveError::Parameter);
    assert_eq!(solve_camera(&good[..5], ZOOM, W, H).unwrap_err(), CameraSolveError::TooFewPoints);
    let still: Vec<Vec<Option<[f64; 2]>>> = good.iter().map(|t| vec![t[0]; FRAMES]).collect();
    assert_eq!(solve_camera(&still, ZOOM, W, H).unwrap_err(), CameraSolveError::NoParallax);
    let mut lonely = good.clone();
    for t in lonely.iter_mut().skip(4) {
        t[7] = None;
    }
    assert_eq!(solve_camera(&lonely, ZOOM, W, H).unwrap_err(), CameraSolveError::FrameUnderconstrained(7));
}
