//! Quick selection / GrabCut proofs (lane SMB-PS-SELECT-LIQUIFY slice 2) on synthetic images with
//! known regions. Bounds are measured on these images (printed), not Photoshop parity (D5).
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::brush::StrokeSample;
use hsk_studio_pigment::select_tools::maxflow::FlowGraph;
use hsk_studio_pigment::select_tools::quick::*;
use hsk_studio_pigment::select_tools::{ToolError, ToolLimits};
use hsk_studio_pigment::selection::{ColourView, CoveragePlane, RectF, SelectionContext, rect};

struct Img {
    bounds: Rect,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Img {
    fn new(bounds: Rect, f: impl Fn(i64, i64) -> [f32; 3]) -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in bounds.y..bounds.y + i64::from(bounds.height) {
            for x in bounds.x..bounds.x + i64::from(bounds.width) {
                for c in f(x, y) {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&1.0f32.to_le_bytes());
            }
        }
        Self { bounds, colour, alpha }
    }
    fn view(&self) -> ColourView<'_> {
        ColourView {
            bounds: self.bounds,
            colour: &self.colour,
            alpha: &self.alpha,
        }
    }
}

const CANVAS: Rect = Rect {
    x: 0,
    y: 0,
    width: 64,
    height: 64,
};

/// Deterministic noise in [-0.04, 0.04].
fn noise(x: i64, y: i64, c: u32) -> f32 {
    let mut h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F) ^ u64::from(c);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    ((h % 1000) as f32 / 1000.0 - 0.5) * 0.08
}

fn in_disk(x: i64, y: i64) -> bool {
    let (dx, dy) = (x as f64 + 0.5 - 32.0, y as f64 + 0.5 - 32.0);
    dx * dx + dy * dy <= 14.0 * 14.0
}

fn disk_image(x: i64, y: i64) -> [f32; 3] {
    let base = if in_disk(x, y) {
        [0.85, 0.25, 0.2]
    } else {
        [0.2 + 0.3 * x as f32 / 64.0, 0.5, 0.7]
    };
    [base[0] + noise(x, y, 0), base[1] + noise(x, y, 1), base[2] + noise(x, y, 2)]
}

fn ctx() -> SelectionContext {
    let mut c = SelectionContext::new(CANVAS);
    c.anti_alias = false;
    c
}

fn sample(x: f64, y: f64) -> StrokeSample {
    StrokeSample { x, y, pressure: None, time_ms: None }
}

/// Intersection over union of `plane >= 0.5` with `truth`.
fn iou(plane: &CoveragePlane, truth: impl Fn(i64, i64) -> bool) -> f64 {
    let (mut inter, mut uni) = (0u32, 0u32);
    for y in 0..64 {
        for x in 0..64 {
            let (a, b) = (plane.at(x, y) >= 32768, truth(x, y));
            inter += u32::from(a && b);
            uni += u32::from(a || b);
        }
    }
    f64::from(inter) / f64::from(uni)
}

#[test]
fn dinic_max_flow_and_min_cut_hand_graph() {
    // CLRS 3rd ed. figure 26.1 network: max flow 23, min cut {s, v1, v2, v4} | {v3, t}.
    let mut g = FlowGraph::with_capacity(6, 9).unwrap();
    for (u, v, c) in [(0, 1, 16), (0, 2, 13), (1, 3, 12), (2, 1, 4), (2, 4, 14), (3, 2, 9), (3, 5, 20), (4, 3, 7), (4, 5, 4)] {
        g.add_edge(u, v, c, 0).unwrap();
    }
    let mut copy = g.clone();
    assert_eq!(g.max_flow(0, 5, u64::MAX).unwrap(), 23);
    assert_eq!(g.source_side(0), vec![true, true, true, false, true, false]);
    assert_eq!(copy.max_flow(0, 5, 3).unwrap_err().code(), "budget_exceeded");
    assert_eq!(g.add_edge(0, 1, -1, 0).unwrap_err().code(), "invalid_input");
    assert_eq!(g.add_edge(0, 9, 1, 0).unwrap_err().code(), "invalid_input");
}

#[test]
fn add_stroke_grows_to_the_region_and_snaps_to_its_edge() {
    let img = Img::new(CANVAS, disk_image);
    let layers = [img.view()];
    let stroke = [sample(30.0, 32.0), sample(34.0, 32.0)];
    let q = QuickStroke { samples: &stroke, diameter_px: 6.0, mode: StrokeMode::Add };
    let out = quick_select(&layers, 0, None, &q, &GraphCutOptions::default(), &ctx(), &ToolLimits::default()).unwrap();
    let score = iou(&out, in_disk);
    println!("quick add IoU vs disk = {score:.4}");
    assert!(score >= 0.95, "{score}");
    // A second stroke on an existing selection elsewhere keeps it (add never removes).
    let base = rect(RectF { x: 0.0, y: 0.0, width: 4.0, height: 4.0 }, &ctx()).unwrap();
    let out2 = quick_select(&layers, 0, Some(&base), &q, &GraphCutOptions::default(), &ctx(), &ToolLimits::default()).unwrap();
    assert_eq!(out2.at(1, 1), u16::MAX);
    assert!(iou(&out2, |x, y| in_disk(x, y) || (x < 4 && y < 4)) >= 0.95);
}

#[test]
fn subtract_stroke_removes_only_the_indicated_region() {
    // Left half red, right half blue, all selected; subtract a stroke down the blue half.
    let img = Img::new(CANVAS, |x, y| {
        let b = if x < 32 { [0.9, 0.2, 0.2] } else { [0.15, 0.3, 0.9] };
        [b[0] + noise(x, y, 0), b[1] + noise(x, y, 1), b[2] + noise(x, y, 2)]
    });
    let layers = [img.view()];
    let base = rect(RectF { x: 0.0, y: 0.0, width: 64.0, height: 64.0 }, &ctx()).unwrap();
    let stroke = [sample(48.0, 20.0), sample(48.0, 44.0)];
    let q = QuickStroke { samples: &stroke, diameter_px: 6.0, mode: StrokeMode::Subtract };
    let out = quick_select(&layers, 0, Some(&base), &q, &GraphCutOptions::default(), &ctx(), &ToolLimits::default()).unwrap();
    // ROI = stroke footprint (x 45..51) grown by 3 * 6 + 8 = 26: x 19..64, every row; the whole
    // blue half is inside it.
    let score = iou(&out, |x, _| x < 32);
    println!("quick subtract IoU vs left half = {score:.4}");
    assert!(score >= 0.98, "{score}");
    assert_eq!(out.at(10, 10), u16::MAX);
    assert_eq!(out.at(48, 32), 0);
    // Subtracting from nothing stays empty.
    let none = quick_select(&layers, 0, None, &q, &GraphCutOptions::default(), &ctx(), &ToolLimits::default()).unwrap();
    assert_eq!(none.data.len(), 0);
}

#[test]
fn grabcut_rect_finds_the_object_and_ml_select_refuses_model_unavailable() {
    let img = Img::new(CANVAS, disk_image);
    let layers = [img.view()];
    let r = Rect { x: 14, y: 14, width: 36, height: 36 };
    let out = grabcut_rect(&layers, 0, r, &GraphCutOptions::default(), &ctx(), &ToolLimits::default()).unwrap();
    let score = iou(&out, in_disk);
    println!("grabcut rect IoU vs disk = {score:.4}");
    assert!(score >= 0.95, "{score}");
    let refuse = |r: Result<CoveragePlane, ToolError>| r.unwrap_err().code();
    assert_eq!(refuse(object_select(&layers, &ObjectRegion::Rect(r), &ctx())), "model_unavailable");
    assert_eq!(refuse(object_select(&layers, &ObjectRegion::Hover { x: 3, y: 3 }, &ctx())), "model_unavailable");
    assert_eq!(refuse(subject_select(&layers, &ctx())), "model_unavailable");
    assert_eq!(refuse(sky_select(&layers, &ctx())), "model_unavailable");
}

#[test]
fn hostile_strokes_and_budgets_refuse_typed_before_allocation() {
    let img = Img::new(CANVAS, disk_image);
    let layers = [img.view()];
    let lim = ToolLimits::default();
    let opts = GraphCutOptions::default();
    let code = |r: Result<CoveragePlane, ToolError>| r.unwrap_err().code();
    let good = [sample(30.0, 32.0), sample(34.0, 32.0)];
    let run = |samples: &[StrokeSample], d: f64, o: &GraphCutOptions, l: &ToolLimits| {
        quick_select(&layers, 0, None, &QuickStroke { samples, diameter_px: d, mode: StrokeMode::Add }, o, &ctx(), l)
    };
    assert_eq!(code(run(&[sample(f64::NAN, 1.0)], 6.0, &opts, &lim)), "nonfinite");
    assert_eq!(code(run(&good, f64::NAN, &opts, &lim)), "brush_out_of_range");
    // Huge brush: refused by the brush engine's tip range before any dab or plane.
    assert_eq!(code(run(&good, 1.0e9, &opts, &lim)), "brush_out_of_range");
    // Very long stroke at 25 % spacing: the dab cap refuses before placement.
    assert_eq!(code(run(&[sample(0.0, 0.0), sample(1.0e12, 0.0)], 6.0, &opts, &lim)), "budget_exceeded");
    assert_eq!(code(run(&good, 6.0, &GraphCutOptions { gamma: f64::NAN, ..opts }, &lim)), "nonfinite");
    assert_eq!(code(run(&good, 6.0, &GraphCutOptions { gamma: -1.0, ..opts }, &lim)), "tool_out_of_range");
    assert_eq!(code(run(&good, 6.0, &GraphCutOptions { roi_margin_px: Some(f64::INFINITY), ..opts }, &lim)), "nonfinite");
    // aggregate_over_budget: the ROI's graph exceeds the work budget before the image copy.
    assert_eq!(code(run(&good, 6.0, &opts, &ToolLimits { max_work_bytes: 4096 })), "budget_exceeded");
    // Solver step budget.
    assert_eq!(code(run(&good, 6.0, &GraphCutOptions { max_flow_steps: 10, ..opts }, &lim)), "budget_exceeded");
    // dims_u32_max canvas: the ROI is clipped to the stroke + margin, so the work stays bounded
    // (the stroke plane itself is admitted by the brush engine).
    let mut huge = ctx();
    huge.canvas = Rect { x: 0, y: 0, width: u32::MAX, height: u32::MAX };
    let big = quick_select(&layers, 0, None, &QuickStroke { samples: &good, diameter_px: 6.0, mode: StrokeMode::Add }, &opts, &huge, &lim);
    assert!(big.is_ok(), "{big:?}");
    let c = code(grabcut_rect(&layers, 0, Rect { x: 0, y: 0, width: u32::MAX, height: u32::MAX }, &opts, &huge, &lim));
    assert!(c == "budget_exceeded" || c == "overflow", "{c}");
}
