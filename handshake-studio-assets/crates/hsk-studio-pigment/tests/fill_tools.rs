//! Raster gradient, paint bucket and Edit > Fill (lane SMB-PS-GRADIENT-BUCKET-1): hand-computed
//! pixels on small layers, selection clipping and typed refusals of hostile input.
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::blend_math::StudioBlendMode;
use hsk_studio_pigment::fill_tools::*;
use hsk_studio_pigment::select_tools::ToolLimits;
use hsk_studio_pigment::selection::{ColourView, CoveragePlane};

const CANVAS: Rect = Rect { x: 0, y: 0, width: 8, height: 8 };
const EPS: f32 = 1e-5;

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
        ColourView { bounds: self.bounds, colour: &self.colour, alpha: &self.alpha }
    }
}

fn clear() -> Img {
    Img::new(CANVAS, |_, _| [0.0; 4])
}

/// Replacement pixel `(x, y)` of a result: straight RGBA.
fn px(r: &FillResult, x: i64, y: i64) -> [f32; 4] {
    let b = r.bounds;
    assert!(x >= b.x && y >= b.y && x < b.x + i64::from(b.width) && y < b.y + i64::from(b.height), "({x}, {y}) outside {b:?}");
    let i = ((y - b.y) as usize) * b.width as usize + (x - b.x) as usize;
    let f = |bytes: &[u8], k: usize| f32::from_le_bytes(bytes[k * 4..k * 4 + 4].try_into().unwrap());
    [f(&r.colour, i * 3), f(&r.colour, i * 3 + 1), f(&r.colour, i * 3 + 2), f(&r.alpha, i)]
}

fn near(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(p, q)| (p - q).abs() <= EPS)
}

fn grey(v: f32) -> [f32; 4] {
    [v, v, v, 1.0]
}

const BW: [ColourStop; 2] = [
    ColourStop { location: 0, midpoint: 50.0, colour: [0.0; 3] },
    ColourStop { location: 4096, midpoint: 50.0, colour: [1.0; 3] },
];
const OPAQUE: [TransparencyStop; 2] = [
    TransparencyStop { location: 0, midpoint: 50.0, opacity: 100.0 },
    TransparencyStop { location: 4096, midpoint: 50.0, opacity: 100.0 },
];

fn grad<'a>(geometry: GradientGeometry, colour: &'a [ColourStop], alpha: &'a [TransparencyStop]) -> RasterGradient<'a> {
    RasterGradient { form: GradientForm::Stops, geometry, colour_stops: colour, transparency_stops: alpha, smoothness: SMOOTHNESS_FULL, reverse: false, dither: false, dither_seed: 0 }
}

fn run(target: &Img, sel: Option<&CoveragePlane>, from: [f64; 2], to: [f64; 2], g: &RasterGradient<'_>, paint: &FillPaint) -> Result<FillResult, FillError> {
    gradient_fill(&target.view(), sel, CANVAS, DragLine { from, to }, g, paint, &ToolLimits::default())
}

fn ok(r: Result<FillResult, FillError>) -> FillResult {
    r.unwrap_or_else(|e| panic!("{}", e.code()))
}

#[test]
fn gradient_geometries_from_the_drag_line_hand_computed() {
    let img = clear();
    let p = FillPaint::default();
    // Linear along a 45-degree drag (0,0)->(8,8): t = (v.d) / |d|^2 at the pixel centre.
    let r = ok(run(&img, None, [0.0, 0.0], [8.0, 8.0], &grad(GradientGeometry::Linear, &BW, &OPAQUE), &p));
    assert_eq!(r.bounds, CANVAS);
    assert!(near(px(&r, 3, 3), grey(0.4375)), "{:?}", px(&r, 3, 3)); // (3.5*8 + 3.5*8) / 128
    assert!(near(px(&r, 1, 5), grey(0.4375))); // same line perpendicular to the drag
    assert!(near(px(&r, 7, 7), grey(0.9375)));
    assert!(r.coverage.data.iter().all(|v| *v == u16::MAX));
    // Reflected along (4,4)->(8,8): t = |v.d| / 32; linear clamps the far side to 0.
    let r = ok(run(&img, None, [4.0, 4.0], [8.0, 8.0], &grad(GradientGeometry::Reflected, &BW, &OPAQUE), &p));
    assert!(near(px(&r, 1, 1), grey(0.625)), "{:?}", px(&r, 1, 1)); // |-20| / 32
    assert!(near(px(&r, 5, 5), grey(0.375))); // 12 / 32
    let r = ok(run(&img, None, [4.0, 4.0], [8.0, 8.0], &grad(GradientGeometry::Linear, &BW, &OPAQUE), &p));
    assert!(near(px(&r, 1, 1), grey(0.0)));
    // Radial (0.5,0.5)->(4.5,0.5): t = |v| / 4; the pixel centred at the radius is the end colour.
    let r = ok(run(&img, None, [0.5, 0.5], [4.5, 0.5], &grad(GradientGeometry::Radial, &BW, &OPAQUE), &p));
    assert!(near(px(&r, 4, 0), grey(1.0)));
    assert!(near(px(&r, 0, 4), grey(1.0)));
    assert!(near(px(&r, 2, 0), grey(0.5)));
    assert!(near(px(&r, 2, 2), grey((8.0f32).sqrt() / 4.0)));
    // Angle (4.5,4.5)->(8.5,4.5): counter-clockwise on screen, one turn = 1; straight up is 90 deg.
    let r = ok(run(&img, None, [4.5, 4.5], [8.5, 4.5], &grad(GradientGeometry::Angle, &BW, &OPAQUE), &p));
    assert!(near(px(&r, 4, 1), grey(0.25)), "{:?}", px(&r, 4, 1));
    assert!(near(px(&r, 1, 4), grey(0.5)));
    assert!(near(px(&r, 4, 7), grey(0.75)));
    assert!(near(px(&r, 7, 4), grey(0.0)));
    // Diamond on an axis, same drag: t = (|v.u| + |v.n|) / 4.
    let r = ok(run(&img, None, [4.5, 4.5], [8.5, 4.5], &grad(GradientGeometry::Diamond, &BW, &OPAQUE), &p));
    assert!(near(px(&r, 6, 4), grey(0.375)));
    assert!(near(px(&r, 4, 6), grey(0.375)));
    assert!(near(px(&r, 5, 5), grey(0.25)));
}

#[test]
fn gradient_midpoint_transparency_list_reverse_and_blend() {
    let img = clear();
    let p = FillPaint::default();
    // Midpoint 25: f < 0.25 -> 0.5 f / 0.25; else 0.5 + 0.5 (f - 0.25) / 0.75.
    let mid = [ColourStop { location: 0, midpoint: 25.0, colour: [0.0; 3] }, ColourStop { location: 4096, midpoint: 50.0, colour: [1.0; 3] }];
    let r = ok(run(&img, None, [0.0, 0.0], [8.0, 0.0], &grad(GradientGeometry::Linear, &mid, &OPAQUE), &p));
    assert!(near(px(&r, 1, 0), grey(0.375)), "{:?}", px(&r, 1, 0)); // t = 0.1875
    assert!(near(px(&r, 3, 0), grey(0.625)), "{:?}", px(&r, 3, 0)); // t = 0.4375
    // Three colour stops against two transparency stops (independent lists, STU-RAS-160).
    let rgb = [
        ColourStop { location: 0, midpoint: 50.0, colour: [1.0, 0.0, 0.0] },
        ColourStop { location: 2048, midpoint: 50.0, colour: [0.0, 1.0, 0.0] },
        ColourStop { location: 4096, midpoint: 50.0, colour: [0.0, 0.0, 1.0] },
    ];
    let fade = [TransparencyStop { location: 0, midpoint: 50.0, opacity: 100.0 }, TransparencyStop { location: 4096, midpoint: 50.0, opacity: 0.0 }];
    let g = grad(GradientGeometry::Linear, &rgb, &fade);
    let r = ok(run(&img, None, [0.0, 0.0], [8.0, 0.0], &g, &p));
    assert!(near(px(&r, 1, 0), [0.625, 0.375, 0.0, 0.8125]), "{:?}", px(&r, 1, 0)); // t = 0.1875
    assert!(near(px(&r, 5, 0), [0.0, 0.625, 0.375, 0.3125]), "{:?}", px(&r, 5, 0)); // t = 0.6875
    // Reverse: t -> 1 - t.
    let r = ok(run(&img, None, [0.0, 0.0], [8.0, 0.0], &RasterGradient { reverse: true, ..g }, &p));
    assert!(near(px(&r, 1, 0), [0.0, 0.375, 0.625, 0.1875]), "{:?}", px(&r, 1, 0)); // t = 0.8125
    // Blend: multiply a solid 0.5 over opaque grey 0.5 = 0.25; at opacity 0.5: 0.5 * 0.25 + 0.5 * 0.5.
    let backdrop = Img::new(CANVAS, |_, _| grey(0.5));
    let half = [ColourStop { location: 0, midpoint: 50.0, colour: [0.5; 3] }];
    let solid = [TransparencyStop { location: 0, midpoint: 50.0, opacity: 100.0 }];
    let g = grad(GradientGeometry::Linear, &half, &solid);
    let multiply = FillPaint { mode: StudioBlendMode::Multiply, ..FillPaint::default() };
    let r = ok(run(&backdrop, None, [0.0, 0.0], [8.0, 0.0], &g, &multiply));
    assert!(near(px(&r, 4, 4), grey(0.25)));
    let r = ok(run(&backdrop, None, [0.0, 0.0], [8.0, 0.0], &g, &FillPaint { opacity: 0.5, ..multiply }));
    assert!(near(px(&r, 4, 4), grey(0.375)), "{:?}", px(&r, 4, 4));
    // Dither: seeded, deterministic, within half a level of the undithered value, not a no-op.
    let g = grad(GradientGeometry::Linear, &BW, &OPAQUE);
    let plain = ok(run(&img, None, [0.0, 0.0], [8.0, 8.0], &g, &p));
    let d = RasterGradient { dither: true, dither_seed: 7, ..g };
    let a = ok(run(&img, None, [0.0, 0.0], [8.0, 8.0], &d, &p));
    let b = ok(run(&img, None, [0.0, 0.0], [8.0, 8.0], &d, &p));
    assert_eq!(a, b);
    let mut moved = 0;
    for y in 0..8 {
        for x in 0..8 {
            let (u, v) = (px(&plain, x, y), px(&a, x, y));
            for c in 0..3 {
                assert!((u[c] - v[c]).abs() <= 0.5 / 255.0 + EPS, "({x}, {y}) {u:?} {v:?}");
                moved += usize::from(u[c] != v[c]);
            }
        }
    }
    assert!(moved > 0, "dither changed nothing");
}

#[test]
fn gradient_is_clipped_to_a_feathered_selection() {
    let img = Img::new(CANVAS, |x, y| [x as f32 / 8.0, y as f32 / 8.0, 0.25, 1.0]);
    // A feathered selection: full in the middle, partial at its edge, zero in a corner.
    let bounds = Rect { x: 2, y: 2, width: 4, height: 3 };
    let data: Vec<u16> = (0..12).map(|i| match i % 4 { 0 => 16384, 3 => 0, _ => u16::MAX }).collect();
    let sel = CoveragePlane { bounds, data };
    let r = ok(run(&img, Some(&sel), [0.0, 0.0], [8.0, 0.0], &grad(GradientGeometry::Linear, &BW, &OPAQUE), &FillPaint::default()));
    assert_eq!(r.coverage.bounds, r.bounds);
    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(r.coverage.at(x, y), sel.at(x, y), "coverage at ({x}, {y})");
        }
    }
    // Where the coverage is 0 the replacement is the layer's own bytes.
    let b = r.bounds;
    for y in b.y..b.y + i64::from(b.height) {
        for x in b.x..b.x + i64::from(b.width) {
            if sel.at(x, y) == 0 {
                assert_eq!(px(&r, x, y), [x as f32 / 8.0, y as f32 / 8.0, 0.25, 1.0]);
            } else {
                assert!(near(px(&r, x, y), grey((x as f32 + 0.5) / 8.0)), "({x}, {y})");
            }
        }
    }
    // A selection that misses the canvas fills nothing.
    let away = CoveragePlane { bounds: Rect { x: 20, y: 20, width: 2, height: 2 }, data: vec![u16::MAX; 4] };
    assert_eq!(run(&img, Some(&away), [0.0, 0.0], [8.0, 0.0], &grad(GradientGeometry::Linear, &BW, &OPAQUE), &FillPaint::default()).unwrap_err().code(), "nothing_to_fill");
}

#[test]
fn gradient_hostile_input_refuses_typed() {
    let img = clear();
    let p = FillPaint::default();
    let g = grad(GradientGeometry::Linear, &BW, &OPAQUE);
    let code = |r: Result<FillResult, FillError>| r.unwrap_err().code();
    assert_eq!(code(run(&img, None, [f64::NAN, 0.0], [8.0, 0.0], &g, &p)), "nonfinite");
    assert_eq!(code(run(&img, None, [0.0, 0.0], [f64::INFINITY, 0.0], &g, &p)), "nonfinite");
    assert_eq!(code(run(&img, None, [0.0, 0.0], [1e300, 0.0], &g, &p)), "coordinate_out_of_range");
    assert_eq!(code(run(&img, None, [3.0, 3.0], [3.0, 3.0], &g, &p)), "zero_length_drag");
    let many: Vec<ColourStop> = (0..=MAX_FILL_STOPS).map(|i| ColourStop { location: (i % 4097) as u16, midpoint: 50.0, colour: [0.0; 3] }).collect();
    assert_eq!(code(run(&img, None, [0.0, 0.0], [8.0, 0.0], &grad(GradientGeometry::Linear, &many, &OPAQUE), &p)), "too_many_stops");
    assert_eq!(code(run(&img, None, [0.0, 0.0], [8.0, 0.0], &grad(GradientGeometry::Linear, &[], &OPAQUE), &p)), "too_few_stops");
    let backwards = [BW[1], BW[0]];
    assert_eq!(code(run(&img, None, [0.0, 0.0], [8.0, 0.0], &grad(GradientGeometry::Linear, &backwards, &OPAQUE), &p)), "stops_out_of_order");
    let late = [ColourStop { location: 4097, ..BW[0] }];
    assert_eq!(code(run(&img, None, [0.0, 0.0], [8.0, 0.0], &grad(GradientGeometry::Linear, &late, &OPAQUE), &p)), "stop_out_of_range");
    let mid = [TransparencyStop { midpoint: 101.0, ..OPAQUE[0] }];
    assert_eq!(code(run(&img, None, [0.0, 0.0], [8.0, 0.0], &grad(GradientGeometry::Linear, &BW, &mid), &p)), "stop_out_of_range");
    let nan = [ColourStop { colour: [f32::NAN, 0.0, 0.0], ..BW[0] }];
    assert_eq!(code(run(&img, None, [0.0, 0.0], [8.0, 0.0], &grad(GradientGeometry::Linear, &nan, &OPAQUE), &p)), "stop_out_of_range");
    for opacity in [1.5, -0.1, f32::NAN] {
        assert_eq!(code(run(&img, None, [0.0, 0.0], [8.0, 0.0], &g, &FillPaint { opacity, ..p })), "opacity_out_of_range");
    }
    assert_eq!(code(run(&img, None, [0.0, 0.0], [8.0, 0.0], &RasterGradient { form: GradientForm::Noise, ..g }, &p)), "gradient_noise_unsupported");
    assert_eq!(code(run(&img, None, [0.0, 0.0], [8.0, 0.0], &RasterGradient { geometry: GradientGeometry::ShapeBurst, ..g }, &p)), "gradient_shape_burst_unsupported");
    assert_eq!(code(run(&img, None, [0.0, 0.0], [8.0, 0.0], &RasterGradient { smoothness: 2048, ..g }, &p)), "gradient_smoothness_unsupported");
    assert_eq!(code(run(&img, None, [0.0, 0.0], [8.0, 0.0], &g, &FillPaint { mode: StudioBlendMode::PassThrough, ..p })), "blend_group_only");
    assert_eq!(code(run(&img, None, [0.0, 0.0], [8.0, 0.0], &g, &FillPaint { mode: StudioBlendMode::Average, ..p })), "blend_unsupported");
    // Malformed view and an over-budget plane.
    let short = ColourView { bounds: CANVAS, colour: &img.colour[..12], alpha: &img.alpha };
    assert_eq!(gradient_fill(&short, None, CANVAS, DragLine { from: [0.0, 0.0], to: [8.0, 0.0] }, &g, &p, &ToolLimits::default()).unwrap_err().code(), "malformed_stride");
    let tiny = ToolLimits { max_work_bytes: 64 };
    assert_eq!(gradient_fill(&img.view(), None, CANVAS, DragLine { from: [0.0, 0.0], to: [8.0, 0.0] }, &g, &p, &tiny).unwrap_err().code(), "budget_exceeded");
}
