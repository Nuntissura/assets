//! Magic wand proofs (lane SMB-PS-SELECT-LIQUIFY slice 1) on synthetic images with known regions.
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::select_tools::wand::*;
use hsk_studio_pigment::select_tools::{ToolError, ToolLimits};
use hsk_studio_pigment::selection::{ColourView, CoveragePlane, SelectionContext, SelectionLimits};

struct Img {
    bounds: Rect,
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Img {
    fn new(bounds: Rect, f: impl Fn(i64, i64) -> [f32; 4]) -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in bounds.y..bounds.y + i64::from(bounds.height) {
            for x in bounds.x..bounds.x + i64::from(bounds.width) {
                let p = f(x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
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

fn code(r: Result<CoveragePlane, ToolError>) -> &'static str {
    r.unwrap_err().code()
}

const CANVAS: Rect = Rect {
    x: 0,
    y: 0,
    width: 40,
    height: 20,
};
const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

fn ctx(aa: bool) -> SelectionContext {
    let mut c = SelectionContext::new(CANVAS);
    c.anti_alias = aa;
    c
}

/// Two red 6x6 squares (x 2..8 and 30..36, y 5..11) on blue.
fn two_squares(x: i64, y: i64) -> [f32; 4] {
    let in_y = (5..11).contains(&y);
    if in_y && ((2..8).contains(&x) || (30..36).contains(&x)) { RED } else { BLUE }
}

#[test]
fn contiguous_selects_one_region_global_selects_both() {
    let img = Img::new(CANVAS, two_squares);
    let layers = [img.view()];
    let lim = ToolLimits::default();
    let opts = WandOptions::default();
    let p = magic_wand(&layers, 0, (4, 7), &opts, &ctx(false), &lim).unwrap();
    assert_eq!(p.bounds, Rect { x: 2, y: 5, width: 6, height: 6 });
    assert!(p.data.iter().all(|v| *v == u16::MAX));
    let g = magic_wand(
        &layers,
        0,
        (4, 7),
        &WandOptions {
            contiguous: false,
            ..opts
        },
        &ctx(false),
        &lim,
    )
    .unwrap();
    assert_eq!(g.bounds, Rect { x: 2, y: 5, width: 34, height: 6 });
    assert_eq!(g.area_sum(), 72.0);
    assert_eq!(g.at(31, 6), u16::MAX);
    assert_eq!(g.at(20, 6), 0);
    // Seeding the blue background floods around both squares: 800 - 72 pixels.
    let bg = magic_wand(&layers, 0, (0, 0), &opts, &ctx(false), &lim).unwrap();
    assert_eq!(bg.area_sum(), 728.0);
    // Anti-alias on: boundary pixels get k/9 of full, interior pixels stay hard.
    let aa = magic_wand(&layers, 0, (4, 7), &opts, &ctx(true), &lim).unwrap();
    assert_eq!(aa.bounds, Rect { x: 1, y: 4, width: 8, height: 8 });
    let k9 = |k: u64| ((k * 65535 * 2 + 9) / 18) as u16;
    assert_eq!(aa.at(4, 7), u16::MAX); // interior
    assert_eq!(aa.at(2, 7), k9(6)); // inside edge: 6 of 9 selected
    assert_eq!(aa.at(1, 7), k9(3)); // outside edge
    assert_eq!(aa.at(1, 4), 0); // diagonal corner: no 4-neighbour boundary
    assert_eq!(aa.at(2, 5), k9(4)); // inside corner
}

#[test]
fn tolerance_is_chebyshev_in_8_bit_units() {
    // Red channel steps by 5/255 per column; green fixed.
    let img = Img::new(CANVAS, |x, _| [x as f32 * 5.0 / 255.0, 0.5, 0.0, 1.0]);
    let layers = [img.view()];
    let lim = ToolLimits::default();
    for (tol, lo, hi) in [(0.0, 20, 20), (10.0, 18, 22), (25.0, 15, 25), (4.9, 20, 20)] {
        let opts = WandOptions {
            tolerance: tol,
            ..WandOptions::default()
        };
        let p = magic_wand(&layers, 0, (20, 3), &opts, &ctx(false), &lim).unwrap();
        assert_eq!(p.bounds.x, lo, "tol {tol}");
        assert_eq!(p.bounds.x + i64::from(p.bounds.width) - 1, hi, "tol {tol}");
        assert_eq!(p.bounds.height, 20);
    }
    // 3x3 sample at the canvas corner is clipped to columns 0..1 (mean 2.5 units): tolerance 5
    // covers columns 0 and 1 (2.5 units away) but not 2 (7.5).
    let opts = WandOptions {
        tolerance: 5.0,
        sample_size: 3,
        ..WandOptions::default()
    };
    let p = magic_wand(&layers, 0, (0, 0), &opts, &ctx(false), &lim).unwrap();
    assert_eq!((p.bounds.x, p.bounds.width), (0, 2));
}

#[test]
fn sample_merged_reads_the_composite() {
    // Bottom: all red. Top: blue over x < 10, transparent elsewhere.
    let bottom = Img::new(CANVAS, |_, _| RED);
    let top = Img::new(CANVAS, |x, _| if x < 10 { BLUE } else { [0.0, 0.0, 0.0, 0.0] });
    let layers = [bottom.view(), top.view()];
    let lim = ToolLimits::default();
    let opts = WandOptions::default();
    // Active bottom layer only: the whole canvas is red.
    let p = magic_wand(&layers, 0, (5, 5), &opts, &ctx(false), &lim).unwrap();
    assert_eq!(p.area_sum(), 800.0);
    // Active top layer only: transparent pixels form their own region.
    let t = magic_wand(&layers, 1, (20, 5), &opts, &ctx(false), &lim).unwrap();
    assert_eq!(t.bounds, Rect { x: 10, y: 0, width: 30, height: 20 });
    // Merged: the blue strip only.
    let m = magic_wand(
        &layers,
        0,
        (5, 5),
        &WandOptions {
            sample_merged: true,
            ..opts
        },
        &ctx(false),
        &lim,
    )
    .unwrap();
    assert_eq!(m.bounds, Rect { x: 0, y: 0, width: 10, height: 20 });
    assert_eq!(m.area_sum(), 200.0);
}

#[test]
fn eight_connectivity_crosses_diagonals() {
    // Diagonal line of red pixels on blue: 4-connected stops at one pixel, 8-connected follows.
    let img = Img::new(CANVAS, |x, y| if x == y { RED } else { BLUE });
    let layers = [img.view()];
    let lim = ToolLimits::default();
    let four = magic_wand(&layers, 0, (3, 3), &WandOptions::default(), &ctx(false), &lim).unwrap();
    assert_eq!(four.area_sum(), 1.0);
    let eight = magic_wand(
        &layers,
        0,
        (3, 3),
        &WandOptions {
            connectivity: Connectivity::Eight,
            ..WandOptions::default()
        },
        &ctx(false),
        &lim,
    )
    .unwrap();
    assert_eq!(eight.area_sum(), 20.0);
}

#[test]
fn hostile_inputs_refuse_typed_before_allocation() {
    let img = Img::new(CANVAS, two_squares);
    let layers = [img.view()];
    let lim = ToolLimits::default();
    let opts = WandOptions::default();
    let bad = |o: WandOptions| code(magic_wand(&layers, 0, (1, 1), &o, &ctx(false), &lim));
    assert_eq!(bad(WandOptions { tolerance: f64::NAN, ..opts }), "nonfinite");
    assert_eq!(bad(WandOptions { tolerance: -1.0, ..opts }), "tool_out_of_range");
    assert_eq!(bad(WandOptions { sample_size: 4, ..opts }), "tool_out_of_range");
    assert_eq!(bad(WandOptions { sample_size: 0, ..opts }), "tool_out_of_range");
    // Seed outside the canvas, missing active layer.
    assert_eq!(code(magic_wand(&layers, 0, (-1, 0), &opts, &ctx(false), &lim)), "invalid_input");
    assert_eq!(code(magic_wand(&layers, 3, (1, 1), &opts, &ctx(false), &lim)), "invalid_input");
    // dims_u32_max: a u32::MAX x u32::MAX canvas refuses before any allocation.
    let mut huge = ctx(false);
    huge.canvas = Rect { x: 0, y: 0, width: u32::MAX, height: u32::MAX };
    let c = code(magic_wand(&layers, 0, (1, 1), &opts, &huge, &lim));
    assert!(c == "budget_exceeded" || c == "overflow", "{c}");
    // aggregate_over_budget: a canvas inside the plane budget but over the work budget.
    let mut big = ctx(false);
    big.canvas = Rect { x: 0, y: 0, width: 2000, height: 2000 };
    big.limits = SelectionLimits { max_bytes: 1 << 30, max_points: 10 };
    let small = ToolLimits { max_work_bytes: 1 << 20 };
    assert_eq!(code(magic_wand(&layers, 0, (1, 1), &opts, &big, &small)), "budget_exceeded");
    // NaN pixel and malformed stride.
    let nan = Img::new(CANVAS, |x, _| if x == 2 { [f32::NAN, 0.0, 0.0, 1.0] } else { BLUE });
    assert_eq!(code(magic_wand(&[nan.view()], 0, (1, 1), &opts, &ctx(false), &lim)), "nonfinite");
    let short = ColourView { bounds: CANVAS, colour: &img.colour[..12], alpha: &img.alpha };
    assert_eq!(code(magic_wand(&[short], 0, (1, 1), &opts, &ctx(false), &lim)), "malformed_stride");
    let alpha = Img::new(CANVAS, |_, _| [0.0, 0.0, 0.0, 2.0]);
    assert_eq!(code(magic_wand(&[alpha.view()], 0, (1, 1), &opts, &ctx(false), &lim)), "alpha_range");
}
