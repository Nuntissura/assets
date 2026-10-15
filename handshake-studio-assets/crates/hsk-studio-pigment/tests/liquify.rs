//! Liquify proofs (lane SMB-PS-SELECT-LIQUIFY slice 4) on synthetic images with known warps.
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::brush::StrokeSample;
use hsk_studio_pigment::liquify::*;
use hsk_studio_pigment::selection::{ColourView, CoveragePlane};
use hsk_studio_pigment::transform_math::ResampleFilter;

const B: Rect = Rect { x: 0, y: 0, width: 64, height: 64 };

struct Img {
    colour: Vec<u8>,
    alpha: Vec<u8>,
}
impl Img {
    fn new(f: impl Fn(i64, i64) -> [f32; 4]) -> Self {
        let (mut colour, mut alpha) = (Vec::new(), Vec::new());
        for y in 0..64 {
            for x in 0..64 {
                let p = f(x, y);
                for c in &p[..3] {
                    colour.extend_from_slice(&c.to_le_bytes());
                }
                alpha.extend_from_slice(&p[3].to_le_bytes());
            }
        }
        Self { colour, alpha }
    }
    fn view(&self) -> ColourView<'_> {
        ColourView { bounds: B, colour: &self.colour, alpha: &self.alpha }
    }
}

fn red(bytes: &[u8], x: i64, y: i64) -> f32 {
    let i = ((y * 64 + x) * 12) as usize;
    f32::from_le_bytes(bytes[i..i + 4].try_into().unwrap())
}

fn s(x: f64, y: f64) -> StrokeSample {
    StrokeSample { x, y, pressure: None, time_ms: None }
}

/// A vertical red line at x = 20 (3 px wide: 19..=21) and a red dot grid, on grey.
fn line_image() -> Img {
    Img::new(|x, y| {
        if (19..=21).contains(&x) || (x % 16 == 8 && y % 16 == 8) {
            [1.0, 0.0, 0.0, 1.0]
        } else {
            [0.25, 0.5, 0.75, 1.0]
        }
    })
}

#[test]
fn identity_and_forward_warp_through_the_resampler() {
    let img = line_image();
    let lim = LiquifyLimits::default();
    let mut mesh = LiquifyMesh::new(B, 4, &lim).unwrap();
    assert_eq!((mesh.cols, mesh.rows), (17, 17));
    // Identity mesh: byte-identical for every filter (rule 1 exact centre).
    for f in [ResampleFilter::NearestNeighbour, ResampleFilter::Bilinear, ResampleFilter::Bicubic] {
        let (c, a) = mesh.render(&img.view(), f, &lim).unwrap();
        assert_eq!((c, a), (img.colour.clone(), img.alpha.clone()));
    }
    // Drag right by 4.8 px (dabs every 2.4 px = 10 % of 24) across the line at y = 32 with a
    // 24 px brush, density 100 (flat weight).
    let brush = LiquifyBrush { size: 24.0, density: 100.0, pressure: 100.0, rate: 80.0 };
    mesh.apply_stroke(LiquifyTool::ForwardWarp, &[s(20.5, 32.5), s(25.3, 32.5)], &brush, &lim).unwrap();
    assert!(!mesh.is_identity());
    let (c, _) = mesh.render(&img.view(), ResampleFilter::Bilinear, &lim).unwrap();
    // Row 32: the line (19..=21) moved right by the 4.8 px drag: pixels 24 and 25 sample it.
    let row: Vec<f32> = (0..64).map(|x| red(&c, x, 32)).collect();
    let peak = (0..64).max_by(|a, b| row[*a].total_cmp(&row[*b]).then(b.cmp(a))).unwrap();
    println!("row 32 red peak after a 4.8 px forward warp: x = {peak}");
    assert!((24..=25).contains(&peak), "{peak}");
    assert!((red(&c, 24, 32) - 1.0).abs() < 1e-6 && (red(&c, 25, 32) - 1.0).abs() < 1e-6);
    assert!(row[20] < 0.5, "{}", row[20]);
    // Rows far from the brush are untouched bytes.
    for y in [0, 2, 60, 63] {
        for x in 0..64 {
            assert_eq!(red(&c, x, y), red(&img.colour, x, y));
        }
    }
}

#[test]
fn reconstruct_returns_exactly_to_identity() {
    let img = line_image();
    let lim = LiquifyLimits::default();
    let mut mesh = LiquifyMesh::new(B, 4, &lim).unwrap();
    let brush = LiquifyBrush::default();
    mesh.apply_stroke(LiquifyTool::ForwardWarp, &[s(10.0, 10.0), s(40.0, 50.0)], &LiquifyBrush { size: 30.0, ..brush }, &lim).unwrap();
    mesh.apply_stroke(LiquifyTool::Twirl { clockwise: true }, &[s(32.0, 32.0); 5], &LiquifyBrush { size: 40.0, ..brush }, &lim).unwrap();
    mesh.apply_stroke(LiquifyTool::Bloat, &[s(48.0, 16.0); 3], &LiquifyBrush { size: 20.0, ..brush }, &lim).unwrap();
    assert!(!mesh.is_identity());
    let (warped, _) = mesh.render(&img.view(), ResampleFilter::Bicubic, &lim).unwrap();
    assert_ne!(warped, img.colour);
    // Reconstruct brush at full strength (rate 100 %, pressure 100 %, density 100 %) over the
    // whole layer: k w = 1 -> every vertex exactly 0.
    let full = LiquifyBrush { size: 200.0, density: 100.0, pressure: 100.0, rate: 100.0 };
    let mut brushed = mesh.clone();
    brushed.apply_stroke(LiquifyTool::Reconstruct, &[s(32.0, 32.0)], &full, &lim).unwrap();
    assert!(brushed.is_identity());
    let (c, a) = brushed.render(&img.view(), ResampleFilter::Bicubic, &lim).unwrap();
    assert_eq!((c, a), (img.colour.clone(), img.alpha.clone()));
    // Partial reconstruct shrinks every touched displacement; reconstruct_all is exact.
    let before: f64 = mesh.disp.iter().map(|d| d[0].hypot(d[1])).sum();
    let mut half = mesh.clone();
    half.apply_stroke(LiquifyTool::Reconstruct, &[s(32.0, 32.0)], &LiquifyBrush { rate: 50.0, ..full }, &lim).unwrap();
    let after: f64 = half.disp.iter().map(|d| d[0].hypot(d[1])).sum();
    assert!((after - before * 0.5).abs() < 1e-9 * before.max(1.0), "{after} vs {before}");
    mesh.reconstruct_all();
    assert!(mesh.is_identity());
    let (c, _) = mesh.render(&img.view(), ResampleFilter::Bilinear, &lim).unwrap();
    assert_eq!(c, img.colour);
}

#[test]
fn twirl_pucker_bloat_push_left_hand_geometry() {
    let lim = LiquifyLimits::default();
    let flat = LiquifyBrush { size: 40.0, density: 100.0, pressure: 100.0, rate: 100.0 };
    let c = (32.5, 32.5); // a vertex (spacing 4: 0.5 + 8 * 4)
    // Bloat: q = c + (1 - k)(p - c), k = 0.05 -> D(p) = -k (p - c): sampling pulls inward
    // (content magnified).
    let mut m = LiquifyMesh::new(B, 4, &lim).unwrap();
    m.apply_stroke(LiquifyTool::Bloat, &[s(c.0, c.1)], &flat, &lim).unwrap();
    let d = m.displacement_at(40.5, 32.5);
    assert!((d[0] + 0.05 * 8.0).abs() < 1e-12 && d[1].abs() < 1e-12, "{d:?}");
    assert_eq!(m.displacement_at(c.0, c.1), [0.0, 0.0]);
    // Pucker: the opposite sign.
    let mut m = LiquifyMesh::new(B, 4, &lim).unwrap();
    m.apply_stroke(LiquifyTool::Pucker, &[s(c.0, c.1)], &flat, &lim).unwrap();
    let d = m.displacement_at(40.5, 32.5);
    assert!((d[0] - 0.05 * 8.0).abs() < 1e-12, "{d:?}");
    // Twirl clockwise by 0.1 rad: q = c + R(-0.1)(p - c), so a point 8 px right of the centre
    // samples from (8 cos 0.1, -8 sin 0.1): content turns clockwise on screen (y down).
    let mut m = LiquifyMesh::new(B, 4, &lim).unwrap();
    m.apply_stroke(LiquifyTool::Twirl { clockwise: true }, &[s(c.0, c.1)], &flat, &lim).unwrap();
    let d = m.displacement_at(40.5, 32.5);
    let (sn, cs) = 0.1f64.sin_cos();
    assert!((d[0] - (8.0 * cs - 8.0)).abs() < 1e-12 && (d[1] + 8.0 * sn).abs() < 1e-12, "{d:?}");
    // Push left: dragging up 8 px (one dab step at 10 % of 80) moves content left: D = +8 in x.
    let mut m = LiquifyMesh::new(B, 4, &lim).unwrap();
    m.apply_stroke(LiquifyTool::PushLeft, &[s(c.0, c.1 + 8.0), s(c.0, c.1)], &LiquifyBrush { size: 80.0, ..flat }, &lim).unwrap();
    let d = m.displacement_at(c.0, c.1);
    assert!((d[0] - 8.0).abs() < 1e-9 && d[1].abs() < 1e-9, "{d:?}");
    // Smooth relaxes toward the neighbour mean: a single spike halves at k = 0.5.
    let mut m = LiquifyMesh::new(B, 4, &lim).unwrap();
    let k = 8 * m.cols as usize + 8;
    m.disp[k] = [4.0, 0.0];
    m.apply_stroke(LiquifyTool::Smooth, &[s(c.0, c.1)], &LiquifyBrush { size: 2.0, rate: 50.0, ..flat }, &lim).unwrap();
    assert_eq!(m.disp[k], [2.0, 0.0]);
}

#[test]
fn freeze_mask_protects_and_thaw_releases() {
    let img = line_image();
    let lim = LiquifyLimits::default();
    let mut mesh = LiquifyMesh::new(B, 4, &lim).unwrap();
    // Freeze the top half (a StudioMask-style coverage plane).
    let data = (0..64).flat_map(|y| (0..64).map(move |_| if y < 32 { u16::MAX } else { 0 })).collect();
    mesh.set_freeze(&CoveragePlane { bounds: B, data });
    let brush = LiquifyBrush { size: 60.0, density: 100.0, pressure: 100.0, rate: 80.0 };
    mesh.apply_stroke(LiquifyTool::ForwardWarp, &[s(20.0, 32.0), s(28.0, 32.0)], &brush, &lim).unwrap();
    let (c, _) = mesh.render(&img.view(), ResampleFilter::Bilinear, &lim).unwrap();
    // Every frozen vertex row (y <= 28.5) kept zero displacement; pixel rows above 28 untouched.
    for y in 0..29 {
        for x in 0..64 {
            assert_eq!(red(&c, x, y), red(&img.colour, x, y), "{x},{y}");
        }
    }
    assert!(red(&c, 20, 40) < 0.5);
    mesh.thaw_all();
    mesh.apply_stroke(LiquifyTool::ForwardWarp, &[s(20.0, 20.0), s(28.0, 20.0)], &brush, &lim).unwrap();
    let (c, _) = mesh.render(&img.view(), ResampleFilter::Bilinear, &lim).unwrap();
    assert!(red(&c, 20, 20) < 0.5);
    // The mesh is re-editable data: serde round trip, then validate.
    let json = serde_json::to_string(&mesh).unwrap();
    let back: LiquifyMesh = serde_json::from_str(&json).unwrap();
    back.validate(&lim).unwrap();
    assert_eq!(back, mesh);
}

#[test]
fn hostile_meshes_brushes_and_pixels_refuse_typed_before_allocation() {
    let img = line_image();
    let lim = LiquifyLimits::default();
    let code = |r: Result<(), LiquifyError>| r.unwrap_err().code();
    // Mesh bomb: spacing 1 over 1e6 x 1e6 refuses before allocating; spacing 0 out of range.
    let big = Rect { x: 0, y: 0, width: 1_000_000, height: 1_000_000 };
    assert_eq!(LiquifyMesh::new(big, 1, &lim).unwrap_err().code(), "budget_exceeded");
    let max = Rect { x: 0, y: 0, width: u32::MAX, height: u32::MAX };
    assert_eq!(LiquifyMesh::new(max, 1, &lim).unwrap_err().code(), "budget_exceeded");
    assert_eq!(LiquifyMesh::new(B, 0, &lim).unwrap_err().code(), "liquify_out_of_range");
    let mut mesh = LiquifyMesh::new(B, 4, &lim).unwrap();
    let b = LiquifyBrush::default();
    let st = [s(10.0, 10.0), s(20.0, 20.0)];
    // Huge brush: the brush engine's tip range refuses; NaN and negative parameters typed.
    assert_eq!(code(mesh.apply_stroke(LiquifyTool::ForwardWarp, &st, &LiquifyBrush { size: 1.0e9, ..b }, &lim)), "brush_out_of_range");
    assert_eq!(code(mesh.apply_stroke(LiquifyTool::ForwardWarp, &st, &LiquifyBrush { pressure: f64::NAN, ..b }, &lim)), "nonfinite");
    assert_eq!(code(mesh.apply_stroke(LiquifyTool::ForwardWarp, &st, &LiquifyBrush { rate: -1.0, ..b }, &lim)), "liquify_out_of_range");
    assert_eq!(code(mesh.apply_stroke(LiquifyTool::ForwardWarp, &[s(f64::NAN, 0.0)], &b, &lim)), "nonfinite");
    // aggregate_over_budget: dab x vertex work over the cap refuses before the mesh changes.
    let tight = LiquifyLimits { max_work: 100, ..lim };
    assert_eq!(code(mesh.apply_stroke(LiquifyTool::ForwardWarp, &st, &b, &tight)), "budget_exceeded");
    let long = [s(0.0, 0.0), s(1.0e12, 0.0)];
    assert_eq!(code(mesh.apply_stroke(LiquifyTool::ForwardWarp, &long, &b, &lim)), "budget_exceeded");
    assert!(mesh.is_identity());
    // Render: mismatched bounds, NaN pixel, alpha out of range, output budget.
    let other = Img::new(|_, _| [0.0, 0.0, 0.0, 1.0]);
    let shifted = ColourView { bounds: Rect { x: 1, ..B }, colour: &other.colour, alpha: &other.alpha };
    assert_eq!(mesh.render(&shifted, ResampleFilter::Bilinear, &lim).unwrap_err().code(), "invalid_input");
    let nan = Img::new(|x, _| if x == 3 { [f32::NAN, 0.0, 0.0, 1.0] } else { [0.0, 0.0, 0.0, 1.0] });
    assert_eq!(mesh.render(&nan.view(), ResampleFilter::Bilinear, &lim).unwrap_err().code(), "nonfinite");
    let a2 = Img::new(|_, _| [0.0, 0.0, 0.0, 2.0]);
    assert_eq!(mesh.render(&a2.view(), ResampleFilter::Bilinear, &lim).unwrap_err().code(), "alpha_range");
    let small = LiquifyLimits { max_output_bytes: 1024, ..lim };
    assert_eq!(mesh.render(&img.view(), ResampleFilter::Bilinear, &small).unwrap_err().code(), "budget_exceeded");
    // A deserialized mesh with a wrong vertex count or NaN displacement is refused.
    let mut bad = mesh.clone();
    bad.disp.pop();
    assert_eq!(bad.validate(&lim).unwrap_err().code(), "invalid_input");
    let mut bad = mesh.clone();
    bad.disp[3] = [f64::NAN, 0.0];
    assert_eq!(bad.validate(&lim).unwrap_err().code(), "nonfinite");
}
