//! Select and Mask / refine edge proofs (lane SMB-PS-SELECT-LIQUIFY slice 3) on synthetic images
//! with known mattes (I = alpha F + (1 - alpha) B). Bounds are measured here and printed; they are
//! not Photoshop parity (D5).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::select_tools::refine::*;
use hsk_studio_pigment::select_tools::{ToolError, ToolLimits};
use hsk_studio_pigment::selection::{ColourView, CoveragePlane, SelectionContext, feather};

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

const CANVAS: Rect = Rect { x: 0, y: 0, width: 64, height: 32 };
const F: [f64; 3] = [0.9, 0.2, 0.1];
const B: [f64; 3] = [0.1, 0.3, 0.8];

/// Ground-truth matte: 1 left of x = 28, linear ramp to 0 at x = 36 (pixel centres).
fn alpha_true(x: i64) -> f64 {
    ((36.0 - (x as f64 + 0.5)) / 8.0).clamp(0.0, 1.0)
}

fn ramp_image() -> Img {
    Img::new(CANVAS, |x, _| {
        let a = alpha_true(x);
        [0, 1, 2].map(|k| a * F[k] + (1.0 - a) * B[k])
    })
}

/// Hard selection of columns `x < split` over the canvas.
fn hard_selection(split: i64) -> CoveragePlane {
    let data = (0..32).flat_map(|_| (0..64).map(move |x| if x < split { u16::MAX } else { 0 })).collect();
    CoveragePlane { bounds: CANVAS, data }
}

fn ctx() -> SelectionContext {
    let mut c = SelectionContext::new(CANVAS);
    c.anti_alias = false;
    c
}

fn a(out: &RefineOutput, x: i64, y: i64) -> f64 {
    f64::from(out.coverage.at(x, y)) / 65535.0
}

#[test]
fn closed_form_matting_recovers_a_linear_ramp_matte() {
    let img = ramp_image();
    let opts = RefineOptions { radius: 6.0, method: MatteMethod::ClosedForm, ..RefineOptions::default() };
    let out = refine_edge(&[img.view()], 0, &hard_selection(32), &opts, &ctx(), &ToolLimits::default()).unwrap();
    // Band: |distance - 0.5| < 6 around the x = 32 boundary -> columns 26..=37, every row.
    assert_eq!(out.band_pixels, 12 * 32);
    let mut worst = 0.0f64;
    for y in 0..32 {
        for x in 0..64 {
            worst = worst.max((a(&out, x, y) - alpha_true(x)).abs());
        }
    }
    println!("closed-form max |alpha - truth| = {worst:.5}");
    assert!(worst < 0.02, "{worst}");
    assert_eq!(a(&out, 10, 5), 1.0);
    assert_eq!(a(&out, 50, 5), 0.0);
}

#[test]
fn guided_filter_softens_the_band_toward_the_matte() {
    let img = ramp_image();
    let opts = RefineOptions { radius: 6.0, window_radius: 4, ..RefineOptions::default() };
    let out = refine_edge(&[img.view()], 0, &hard_selection(32), &opts, &ctx(), &ToolLimits::default()).unwrap();
    let (mut err_refined, mut err_hard) = (0.0f64, 0.0f64);
    for x in 26..38 {
        err_refined += (a(&out, x, 16) - alpha_true(x)).abs();
        err_hard += ((if x < 32 { 1.0 } else { 0.0 }) - alpha_true(x)).abs();
    }
    let (mr, mh) = (err_refined / 12.0, err_hard / 12.0);
    println!("guided filter band mean |alpha - truth| = {mr:.5} (hard selection {mh:.5})");
    // Measured 0.0638 vs 0.1667 for the hard selection (one guided pass is an approximation;
    // the closed-form test holds the tight bound).
    assert!(mr < 0.08, "{mr}");
    assert!(mr < mh * 0.5);
    // Monotone across the ramp.
    for x in 26..37 {
        assert!(a(&out, x, 16) >= a(&out, x + 1, 16) - 1e-4, "{x}");
    }
}

#[test]
fn smooth_feather_contrast_and_shift_edge_hand_values() {
    // Flat image: no band (radius 0); a rect with a one-pixel notch and a one-pixel speck.
    let img = Img::new(CANVAS, |_, _| [0.5, 0.5, 0.5]);
    let mut sel = hard_selection(0);
    for y in 8..24 {
        for x in 10..30 {
            sel.data[(y * 64 + x) as usize] = u16::MAX;
        }
    }
    sel.data[(8 * 64 + 20) as usize] = 0; // notch
    sel.data[(2 * 64 + 50) as usize] = u16::MAX; // speck
    let base = RefineOptions { radius: 0.0, ..RefineOptions::default() };
    let lim = ToolLimits::default();
    let plain = refine_edge(&[img.view()], 0, &sel, &base, &ctx(), &lim).unwrap();
    assert_eq!(plain.band_pixels, 0);
    assert_eq!(plain.coverage.at(20, 8), 0);
    let smooth = refine_edge(&[img.view()], 0, &sel, &RefineOptions { smooth: 1.0, ..base }, &ctx(), &lim).unwrap();
    assert_eq!(smooth.coverage.at(20, 8), u16::MAX); // notch filled by the 3x3 majority
    assert_eq!(smooth.coverage.at(50, 2), 0); // speck removed
    assert_eq!(smooth.coverage.at(10, 8), 0); // convex corner: 4 of 9 votes
    // Feather equals selection::feather of the binary selection.
    let fe = refine_edge(&[img.view()], 0, &sel, &RefineOptions { feather: 3.0, ..base }, &ctx(), &lim).unwrap();
    let expect = feather(&plain.coverage, 3.0, &ctx()).unwrap();
    for (x, y) in [(10, 8), (9, 15), (29, 23), (5, 15), (20, 7)] {
        assert!((i32::from(fe.coverage.at(x, y)) - i32::from(expect.at(x, y))).abs() <= 1, "{x},{y}");
    }
    // Contrast 50 doubles the slope around 0.5; shift +50 raises alpha to the power 0.5.
    let cs = refine_edge(&[img.view()], 0, &sel, &RefineOptions { feather: 3.0, contrast: 50.0, shift_edge: 50.0, ..base }, &ctx(), &lim).unwrap();
    for (x, y) in [(9, 15), (11, 15), (8, 15), (12, 15)] {
        let f = f64::from(fe.coverage.at(x, y)) / 65535.0;
        let want = (0.5 + (f - 0.5) * 2.0).clamp(0.0, 1.0).powf(0.5);
        assert!((a(&cs, x, y) - want).abs() < 2e-4, "{x},{y} {} {want}", a(&cs, x, y));
    }
    let hard = refine_edge(&[img.view()], 0, &sel, &RefineOptions { feather: 3.0, contrast: 100.0, ..base }, &ctx(), &lim).unwrap();
    assert!(hard.coverage.data.iter().all(|v| *v == 0 || *v == u16::MAX));
}

#[test]
fn decontamination_and_smart_radius() {
    let img = ramp_image();
    let opts = RefineOptions {
        radius: 6.0,
        method: MatteMethod::ClosedForm,
        decontaminate_colours: true,
        decontamination_amount: 100.0,
        ..RefineOptions::default()
    };
    let out = refine_edge(&[img.view()], 0, &hard_selection(32), &opts, &ctx(), &ToolLimits::default()).unwrap();
    let colours = out.colours.as_ref().unwrap();
    let cb = out.coverage.bounds;
    let mut worst = 0.0f64;
    for x in 28..34 {
        let i = ((16 - cb.y) * i64::from(cb.width) + (x - cb.x)) as usize;
        let c = colours[i];
        let dist = (0..3).map(|k| (f64::from(c[k]) - F[k]).abs()).fold(0.0, f64::max);
        let before = (0..3)
            .map(|k| ((alpha_true(x) * F[k] + (1.0 - alpha_true(x)) * B[k]) - F[k]).abs())
            .fold(0.0, f64::max);
        assert!(dist < before || before < 1e-6, "{x}: {dist} vs {before}");
        worst = worst.max(dist);
    }
    println!("decontaminated band colour max |c - F| = {worst:.5}");
    assert!(worst < 0.15, "{worst}");
    // Smart radius: a hard step narrows the band to a quarter, a soft ramp keeps most of it.
    let step = Img::new(CANVAS, |x, _| if x < 32 { F } else { B });
    let wide = RefineOptions { radius: 8.0, ..RefineOptions::default() };
    let smart = RefineOptions { smart_radius: true, ..wide };
    let lim = ToolLimits::default();
    let s_off = refine_edge(&[step.view()], 0, &hard_selection(32), &wide, &ctx(), &lim).unwrap().band_pixels;
    let s_on = refine_edge(&[step.view()], 0, &hard_selection(32), &smart, &ctx(), &lim).unwrap().band_pixels;
    let r_on = refine_edge(&[img.view()], 0, &hard_selection(32), &smart, &ctx(), &lim).unwrap().band_pixels;
    println!("band pixels: hard step {s_off} -> smart {s_on}; ramp smart {r_on}");
    assert_eq!(s_off, 16 * 32);
    assert_eq!(s_on, 4 * 32);
    assert!(r_on > s_on);
}

#[test]
fn hostile_options_refuse_typed_before_work() {
    let img = ramp_image();
    let layers = [img.view()];
    let sel = hard_selection(32);
    let lim = ToolLimits::default();
    let code = |o: RefineOptions, l: &ToolLimits| -> &'static str {
        let r: Result<RefineOutput, ToolError> = refine_edge(&layers, 0, &sel, &o, &ctx(), l);
        r.unwrap_err().code()
    };
    let d = RefineOptions::default();
    assert_eq!(code(RefineOptions { radius: f64::NAN, ..d }, &lim), "nonfinite");
    assert_eq!(code(RefineOptions { shift_edge: f64::INFINITY, ..d }, &lim), "nonfinite");
    assert_eq!(code(RefineOptions { radius: -1.0, ..d }, &lim), "tool_out_of_range");
    assert_eq!(code(RefineOptions { epsilon: 0.0, ..d }, &lim), "tool_out_of_range");
    assert_eq!(code(RefineOptions { window_radius: 0, ..d }, &lim), "tool_out_of_range");
    assert_eq!(code(RefineOptions { decontamination_amount: -5.0, ..d }, &lim), "tool_out_of_range");
    // Huge radius: the reach overflows the plan space before any allocation.
    assert_eq!(code(RefineOptions { radius: 1.0e12, ..d }, &lim), "overflow");
    // Operation budget and work budget (aggregate_over_budget).
    assert_eq!(code(RefineOptions { method: MatteMethod::ClosedForm, max_ops: 1000, ..d }, &lim), "budget_exceeded");
    assert_eq!(code(d, &ToolLimits { max_work_bytes: 4096 }), "budget_exceeded");
    assert_eq!(code(RefineOptions { feather: 1.0e6, ..d }, &lim), "coverage_feather_out_of_range");
    // dims_u32_max: a selection plane claiming u32::MAX x u32::MAX on a huge canvas.
    let mut huge = ctx();
    huge.canvas = Rect { x: 0, y: 0, width: u32::MAX, height: u32::MAX };
    let bomb = CoveragePlane { bounds: huge.canvas, data: Vec::new() };
    let c = refine_edge(&layers, 0, &bomb, &d, &huge, &lim).unwrap_err().code();
    assert!(c == "overflow" || c == "budget_exceeded", "{c}");
}
