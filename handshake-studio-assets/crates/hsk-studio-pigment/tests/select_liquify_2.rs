//! Lane SMB-PS-SELECT-LIQUIFY-2 slice A proofs: magnetic lasso (live wire), refinement brushes,
//! liquify turbulence and mesh clone, on synthetic images with known edges and mattes.
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::brush::StrokeSample;
use hsk_studio_pigment::liquify::{LiquifyBrush, LiquifyLimits, LiquifyMesh, LiquifyTool};
use hsk_studio_pigment::select_tools::magnetic::{LiveWire, MagneticOptions, magnetic_lasso};
use hsk_studio_pigment::select_tools::refine::{MatteMethod, RefineOptions};
use hsk_studio_pigment::select_tools::refine_brush::{RefineBrush, RefineBrushMode, RefineState};
use hsk_studio_pigment::select_tools::{Composite, ToolLimits};
use hsk_studio_pigment::selection::{ColourView, CoveragePlane, SelectionContext};

struct Img {
    bounds: Rect,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Img {
    fn new(bounds: Rect, f: impl Fn(i64, i64) -> [f64; 3]) -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in bounds.y..bounds.y + i64::from(bounds.height) {
            for x in bounds.x..bounds.x + i64::from(bounds.width) {
                for c in f(x, y) {
                    colour.extend_from_slice(&(c as f32).to_le_bytes());
                }
                alpha.extend_from_slice(&1.0f32.to_le_bytes());
            }
        }
        Self { bounds, colour, alpha }
    }
    fn view(&self) -> ColourView<'_> {
        ColourView { bounds: self.bounds, colour: &self.colour, alpha: &self.alpha }
    }
}

const C64: Rect = Rect { x: 0, y: 0, width: 64, height: 64 };

fn noise(x: i64, y: i64, c: u64) -> f64 {
    let mut h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ c;
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    ((h % 1000) as f64 / 1000.0 - 0.5) * 0.06
}

fn noisy(base: [f64; 3], x: i64, y: i64) -> [f64; 3] {
    [base[0] + noise(x, y, 0), base[1] + noise(x, y, 1), base[2] + noise(x, y, 2)]
}

fn ctx(canvas: Rect) -> SelectionContext {
    let mut c = SelectionContext::new(canvas);
    c.anti_alias = false;
    c
}

fn s(x: f64, y: f64) -> StrokeSample {
    StrokeSample { x, y, pressure: None, time_ms: None }
}

#[test]
fn live_wire_follows_an_edge_and_magnetic_lasso_snaps_to_the_object() {
    // Vertical edge at x = 32: the wire between two points on it stays on the edge columns.
    let img = Img::new(C64, |x, y| noisy(if x < 32 { [0.85, 0.2, 0.2] } else { [0.15, 0.3, 0.85] }, x, y));
    let layers = [img.view()];
    let wire = LiveWire::new(&Composite::new(&layers).unwrap(), C64, &ToolLimits::default()).unwrap();
    let path = wire.path((31, 10), (31, 50)).unwrap();
    assert_eq!(path.first(), Some(&(31, 10)));
    assert_eq!(path.last(), Some(&(31, 50)));
    assert!(path.iter().all(|(x, _)| (31..=32).contains(x)), "{path:?}");
    // Snap: a point 5 px left of the edge moves onto it within width 8.
    let (sx, _) = wire.snap((26, 30), 8.0, 10.0);
    assert!((31..=32).contains(&sx), "{sx}");
    // Square 20..44 red on blue; a rough trace 3 px outside it.
    let sq = Img::new(C64, |x, y| noisy(if (20..44).contains(&x) && (20..44).contains(&y) { [0.85, 0.2, 0.2] } else { [0.15, 0.3, 0.85] }, x, y));
    let layers = [sq.view()];
    let mut trace = Vec::new();
    let (a, b) = (17.0, 47.0);
    let mut t = a;
    while t < b {
        trace.push([t, a]);
        t += 2.0;
    }
    t = a;
    while t < b {
        trace.push([b, t]);
        t += 2.0;
    }
    t = b;
    while t > a {
        trace.push([t, b]);
        t -= 2.0;
    }
    t = b;
    while t > a {
        trace.push([a, t]);
        t -= 2.0;
    }
    let plane = magnetic_lasso(&layers, 0, &trace, &MagneticOptions::default(), &ctx(C64), &ToolLimits::default()).unwrap();
    let (mut inter, mut uni) = (0u32, 0u32);
    for y in 0..64 {
        for x in 0..64 {
            let (p, q) = (plane.at(x, y) >= 32768, (20..44).contains(&x) && (20..44).contains(&y));
            inter += u32::from(p && q);
            uni += u32::from(p || q);
        }
    }
    let iou = f64::from(inter) / f64::from(uni);
    println!("magnetic lasso IoU vs square = {iou:.4}");
    assert!(iou >= 0.9, "{iou}");
}

/// Ramp matte image of the refine tests: alpha 1 left of x = 28, 0 from x = 36.
fn ramp() -> Img {
    let f = [0.9, 0.2, 0.1];
    let b = [0.1, 0.3, 0.8];
    Img::new(Rect { x: 0, y: 0, width: 64, height: 32 }, |x, _| {
        let a = ((36.0 - (x as f64 + 0.5)) / 8.0).clamp(0.0, 1.0);
        [0, 1, 2].map(|k| a * f[k] + (1.0 - a) * b[k])
    })
}

#[test]
fn refinement_brushes_edit_the_trimap_and_selection() {
    let img = ramp();
    let canvas = img.bounds;
    let c = ctx(canvas);
    let lim = ToolLimits::default();
    let layers = [img.view()];
    let data = (0..32).flat_map(|_| (0..64).map(|x| if x < 32 { u16::MAX } else { 0 })).collect();
    let mut st = RefineState::new(CoveragePlane { bounds: canvas, data });
    let opts = RefineOptions { radius: 0.0, method: MatteMethod::ClosedForm, ..RefineOptions::default() };
    let hard = st.render(&layers, 0, &opts, &c, &lim).unwrap();
    assert_eq!(hard.band_pixels, 0);
    let brush = |samples: &'static [StrokeSample], d: f64, mode| RefineBrush { samples, diameter_px: d, hardness_pct: 100.0, mode, feather_px: 0.0 };
    static EDGE: [StrokeSample; 2] = [StrokeSample { x: 32.0, y: 2.0, pressure: None, time_ms: None }, StrokeSample { x: 32.0, y: 14.0, pressure: None, time_ms: None }];
    st.apply_brush(&brush(&EDGE, 16.0, RefineBrushMode::RefineEdge), &c).unwrap();
    let out = st.render(&layers, 0, &opts, &c, &lim).unwrap();
    assert!(out.band_pixels > 0);
    let a = |o: &hsk_studio_pigment::select_tools::refine::RefineOutput, x: i64, y: i64| f64::from(o.coverage.at(x, y)) / 65535.0;
    let mut worst = 0.0f64;
    for x in 26..38 {
        let truth = ((36.0 - (x as f64 + 0.5)) / 8.0).clamp(0.0, 1.0);
        worst = worst.max((a(&out, x, 8) - truth).abs());
    }
    println!("refine-edge brush row 8 max |alpha - truth| = {worst:.4}");
    assert!(worst < 0.05, "{worst}");
    assert_eq!(a(&out, 33, 28), 0.0, "rows outside the stroke stay hard");
    assert_eq!(a(&out, 30, 28), 1.0);
    // Foreground pins 1, background pins 0 (and clears the band there).
    static FG: [StrokeSample; 1] = [StrokeSample { x: 34.5, y: 8.5, pressure: None, time_ms: None }];
    static BG: [StrokeSample; 1] = [StrokeSample { x: 29.5, y: 8.5, pressure: None, time_ms: None }];
    st.apply_brush(&brush(&FG, 2.0, RefineBrushMode::Foreground), &c).unwrap();
    st.apply_brush(&brush(&BG, 2.0, RefineBrushMode::Background), &c).unwrap();
    let pinned = st.render(&layers, 0, &opts, &c, &lim).unwrap();
    assert_eq!(a(&pinned, 34, 8), 1.0);
    assert_eq!(a(&pinned, 29, 8), 0.0);
    // Add / subtract change the selection itself.
    static ADD: [StrokeSample; 1] = [StrokeSample { x: 50.5, y: 24.5, pressure: None, time_ms: None }];
    static SUB: [StrokeSample; 1] = [StrokeSample { x: 10.5, y: 24.5, pressure: None, time_ms: None }];
    st.apply_brush(&brush(&ADD, 4.0, RefineBrushMode::AddToSelection), &c).unwrap();
    st.apply_brush(&brush(&SUB, 4.0, RefineBrushMode::SubtractFromSelection), &c).unwrap();
    let edited = st.render(&layers, 0, &opts, &c, &lim).unwrap();
    assert_eq!(a(&edited, 50, 24), 1.0);
    assert_eq!(a(&edited, 10, 24), 0.0);
    // Feather mode softens only under its stroke.
    static FE: [StrokeSample; 2] = [StrokeSample { x: 32.0, y: 22.0, pressure: None, time_ms: None }, StrokeSample { x: 32.0, y: 30.0, pressure: None, time_ms: None }];
    let mut fb = brush(&FE, 8.0, RefineBrushMode::Feather);
    fb.feather_px = 3.0;
    st.apply_brush(&fb, &c).unwrap();
    let soft = st.render(&layers, 0, &opts, &c, &lim).unwrap();
    assert!(a(&soft, 31, 26) < 1.0 && a(&soft, 32, 26) > 0.0);
    assert_eq!(a(&soft, 20, 26), 1.0, "outside the feather stroke");
    fb.feather_px = 0.0;
    assert_eq!(st.apply_brush(&fb, &c).unwrap_err().code(), "tool_out_of_range");
}

#[test]
fn turbulence_is_seeded_and_local() {
    let lim = LiquifyLimits::default();
    let brush = LiquifyBrush { size: 24.0, density: 50.0, pressure: 100.0, rate: 100.0 };
    let samples = [s(32.0, 32.0); 4];
    let mut a = LiquifyMesh::new(C64, 4, &lim).unwrap();
    let mut b = a.clone();
    let mut c = a.clone();
    a.apply_stroke(LiquifyTool::Turbulence { seed: 7 }, &samples, &brush, &lim).unwrap();
    b.apply_stroke(LiquifyTool::Turbulence { seed: 7 }, &samples, &brush, &lim).unwrap();
    c.apply_stroke(LiquifyTool::Turbulence { seed: 8 }, &samples, &brush, &lim).unwrap();
    assert_eq!(a, b);
    assert_ne!(a, c);
    assert_ne!(a.displacement_at(36.5, 32.5), [0.0, 0.0]);
    assert_eq!(a.displacement_at(4.5, 4.5), [0.0, 0.0], "outside the brush");
    let max = a.disp.iter().map(|d| d[0].hypot(d[1])).fold(0.0, f64::max);
    println!("turbulence max displacement = {max:.4} px (bound 4 dabs x 0.05 x 24 x sqrt 2 = {:.4})", 4.0 * 0.05 * 24.0 * 2f64.sqrt());
    assert!(max > 0.0 && max <= 4.0 * 0.05 * 24.0 * 2f64.sqrt());
    a.reconstruct_all();
    assert!(a.is_identity());
}

#[test]
fn mesh_clone_copies_the_distortion() {
    let lim = LiquifyLimits::default();
    let flat = LiquifyBrush { size: 16.0, density: 100.0, pressure: 100.0, rate: 100.0 };
    let mut m = LiquifyMesh::new(C64, 4, &lim).unwrap();
    m.apply_stroke(LiquifyTool::Twirl { clockwise: true }, &[s(16.5, 32.5); 3], &flat, &lim).unwrap();
    // Clone at x + 32 (8 vertices): every vertex within the brush copies its source exactly.
    m.apply_stroke(LiquifyTool::MeshClone { dx: -32.0, dy: 0.0 }, &[s(48.5, 32.5)], &flat, &lim).unwrap();
    for (vx, vy) in [(48.5, 32.5), (52.5, 32.5), (44.5, 28.5), (48.5, 36.5)] {
        assert_eq!(m.displacement_at(vx, vy), m.displacement_at(vx - 32.0, vy), "{vx},{vy}");
    }
    assert_ne!(m.displacement_at(52.5, 32.5), [0.0, 0.0]);
    let bad = m.apply_stroke(LiquifyTool::MeshClone { dx: f64::NAN, dy: 0.0 }, &[s(1.0, 1.0)], &flat, &lim);
    assert_eq!(bad.unwrap_err().code(), "nonfinite");
}

#[test]
fn magnetic_lasso_hostile_inputs_refuse_typed() {
    let img = Img::new(C64, |x, y| noisy([0.5, 0.5, 0.5], x, y));
    let layers = [img.view()];
    let lim = ToolLimits::default();
    let tri = [[10.0, 10.0], [40.0, 10.0], [20.0, 40.0]];
    let code = |t: &[[f64; 2]], o: &MagneticOptions, l: &ToolLimits| magnetic_lasso(&layers, 0, t, o, &ctx(C64), l).unwrap_err().code();
    let d = MagneticOptions::default();
    assert_eq!(code(&[[f64::NAN, 1.0], [2.0, 2.0], [3.0, 1.0]], &d, &lim), "nonfinite");
    assert_eq!(code(&tri, &MagneticOptions { frequency: 0.0, ..d }, &lim), "tool_out_of_range");
    assert_eq!(code(&tri, &MagneticOptions { width: f64::INFINITY, ..d }, &lim), "nonfinite");
    assert_eq!(code(&tri[..2], &d, &lim), "invalid_input");
    // aggregate_over_budget: the live-wire maps exceed the work budget before allocation.
    assert_eq!(code(&tri, &d, &ToolLimits { max_work_bytes: 4096 }), "budget_exceeded");
    // dims: a trace spanning 1e12 pixels overflows the plan space before any allocation.
    let far = [[0.0, 0.0], [1.0e12, 0.0], [0.0, 1.0e12]];
    let mut huge = ctx(C64);
    huge.canvas = Rect { x: 0, y: 0, width: u32::MAX, height: u32::MAX };
    let c = magnetic_lasso(&layers, 0, &far, &d, &huge, &lim).unwrap_err().code();
    assert!(c == "overflow" || c == "budget_exceeded", "{c}");
}
