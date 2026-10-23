//! Lane SMB-PIGMENT-CONTENT-AWARE-2 slice 4: photo-size proof. A 4096 x 4096 plane with a 256 x 256
//! hole is filled within an explicit byte and work budget; the hole content is checked against the
//! known periodic texture; the wall time is printed (`PERF:` line, a measurement of this run, not a
//! bound). The default limits (16 MiB) refuse the same request before allocating the pyramid.
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::content_aware::*;
use hsk_studio_pigment::retouch::{RetouchLimits, RgbaPlane};
use hsk_studio_pigment::selection::CoveragePlane;
use std::time::Instant;

const SIDE: u32 = 4096;
const HOLE: u32 = 256;

/// Periodic texture: red = 8 x 8 checker, green = vertical stripes of period 6, blue constant.
fn texture(x: i64, y: i64) -> [f32; 4] {
    let r = if (x.div_euclid(8) + y.div_euclid(8)).rem_euclid(2) == 0 { 0.2 } else { 0.8 };
    let g = if x.rem_euclid(6) < 3 { 0.3 } else { 0.7 };
    [r, g, 0.5, 1.0]
}

#[test]
fn photo_size_fill_within_the_budget() {
    let t0 = Instant::now();
    let canvas = Rect { x: 0, y: 0, width: SIDE, height: SIDE };
    let h0 = i64::from((SIDE - HOLE) / 2);
    let in_hole = |x: i64, y: i64| (h0..h0 + i64::from(HOLE)).contains(&x) && (h0..h0 + i64::from(HOLE)).contains(&y);
    let mut data = Vec::with_capacity((SIDE as usize) * (SIDE as usize));
    for y in 0..i64::from(SIDE) {
        for x in 0..i64::from(SIDE) {
            // The object to remove: solid magenta (must not leak into the fill).
            data.push(if in_hole(x, y) { [1.0, 0.0, 1.0, 1.0] } else { texture(x, y) });
        }
    }
    let target = RgbaPlane { bounds: canvas, data };
    let hole_bounds = Rect { x: h0, y: h0, width: HOLE, height: HOLE };
    let hole = CoveragePlane { bounds: hole_bounds, data: vec![u16::MAX; (HOLE * HOLE) as usize] };
    let built = t0.elapsed();
    let options = ContentAwareOptions { threads: 16, seed: 7, ..ContentAwareOptions::default() };
    // The default limits (16 MiB) refuse before any pyramid allocation.
    let refused = fill(&target, &hole, SamplingArea::Auto, &options, &RetouchLimits::default());
    assert_eq!(refused.unwrap_err().code(), "budget_exceeded");
    // Explicit photo budget: 3 GiB of charged working bytes, 2^40 patch-pixel visits.
    let limits = RetouchLimits { max_bytes: 3 << 30, max_work: 1 << 40 };
    let t1 = Instant::now();
    let r = fill(&target, &hole, SamplingArea::Auto, &options, &limits).unwrap();
    let wall = t1.elapsed();
    let rc = &r.receipt;
    // Quality: the hole continues the texture (exemplar copies), the rest is untouched.
    let (mut sum, mut worst, mut n) = (0.0f64, 0.0f64, 0.0f64);
    for y in h0..h0 + i64::from(HOLE) {
        for x in h0..h0 + i64::from(HOLE) {
            let (a, e) = (r.result.plane.at(x, y), texture(x, y));
            for k in 0..3 {
                let d = (f64::from(a[k]) - f64::from(e[k])).abs();
                sum += d;
                worst = worst.max(d);
                n += 1.0;
            }
        }
    }
    let mean = sum / n;
    println!(
        "PERF: content_aware::fill {SIDE}x{SIDE} plane, {HOLE}x{HOLE} hole, threads 16, colour_adaptation default: \
         fill wall {:.2} s (plane build {:.2} s); levels {}, level-0 domain {}, work {} of {}, blend {:?}; hole mean |err| {mean:.3e} worst {worst:.3e}",
        wall.as_secs_f64(),
        built.as_secs_f64(),
        rc.levels.len(),
        rc.levels.last().map_or(0, |l| l.domain_centres),
        rc.work,
        limits.max_work,
        rc.blend
    );
    assert!(rc.work <= limits.max_work);
    assert_eq!(rc.levels.last().map(|l| (l.width, l.height)), Some((SIDE, SIDE)));
    assert_eq!(r.result.damage, Some(hole_bounds));
    assert!(mean <= 0.02 && worst <= 0.1, "photo-size fill: mean {mean} worst {worst}");
    assert_eq!(r.result.plane.at(5, 5), target.at(5, 5));
    assert_eq!(r.result.plane.at(h0 - 1, h0), target.at(h0 - 1, h0));
}
