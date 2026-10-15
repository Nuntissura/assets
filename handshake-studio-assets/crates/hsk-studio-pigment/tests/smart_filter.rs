//! Smart filter stack proofs (lane SMB-PS-SELECT-LIQUIFY slice 5): non-destructive re-render,
//! per-entry masks and opacity, liquify as a stack entry, hostile entries.
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::brush::StrokeSample;
use hsk_studio_pigment::filter_math::{LayerFilter, NoiseDistribution, filter_plane};
use hsk_studio_pigment::liquify::{LiquifyBrush, LiquifyLimits, LiquifyMesh, LiquifyTool};
use hsk_studio_pigment::retouch::RgbaPlane;
use hsk_studio_pigment::selection::{ColourView, CoveragePlane};
use hsk_studio_pigment::smart_filter::*;
use hsk_studio_pigment::transform_math::ResampleFilter;

const B: Rect = Rect { x: 0, y: 0, width: 32, height: 24 };

fn source() -> RgbaPlane {
    let data = (0..24)
        .flat_map(|y| (0..32).map(move |x| if (x / 4 + y / 4) % 2 == 0 { [0.9, 0.2, 0.1, 1.0] } else { [0.1, 0.3, 0.8, 0.5] }))
        .collect();
    RgbaPlane::new(B, data).unwrap()
}

fn blur() -> StackFilter {
    StackFilter::Filter(LayerFilter::GaussianBlur { radius_px: 3.0 })
}
fn noise() -> StackFilter {
    StackFilter::Filter(LayerFilter::AddNoise { amount_pct: 20.0, distribution: NoiseDistribution::Uniform, monochromatic: false, seed: 7 })
}

#[test]
fn stack_rerenders_in_order_from_the_untouched_source() {
    let src = source();
    let lim = StackLimits::default();
    // Empty and all-disabled stacks are byte copies.
    assert_eq!(SmartFilterStack::default().render(&src, &lim).unwrap(), src);
    let mut off = StackEntry::new(blur());
    off.enabled = false;
    assert_eq!(SmartFilterStack { entries: vec![off], stack_mask: None }.render(&src, &lim).unwrap(), src);
    // Full-strength entries equal sequential destructive application, in list order.
    let stack = SmartFilterStack { entries: vec![StackEntry::new(blur()), StackEntry::new(noise())], stack_mask: None };
    let out = stack.render(&src, &lim).unwrap();
    let StackFilter::Filter(fb) = blur() else { unreachable!() };
    let StackFilter::Filter(fnz) = noise() else { unreachable!() };
    let seq = filter_plane(&filter_plane(&src, &fb, lim.filter).unwrap(), &fnz, lim.filter).unwrap();
    assert_eq!(out, seq);
    let swapped = SmartFilterStack { entries: vec![StackEntry::new(noise()), StackEntry::new(blur())], stack_mask: None };
    assert_ne!(swapped.render(&src, &lim).unwrap(), out);
    // Re-editing a parameter re-renders from the source (the source is never modified).
    let before = src.clone();
    let mut edited = stack.clone();
    edited.entries[0].filter = StackFilter::Filter(LayerFilter::GaussianBlur { radius_px: 1.0 });
    let again = edited.render(&src, &lim).unwrap();
    assert_eq!(src, before);
    edited.entries[0].filter = blur();
    assert_eq!(edited.render(&src, &lim).unwrap(), out);
    assert_ne!(again, out);
}

#[test]
fn entry_masks_opacity_and_stack_mask() {
    let src = source();
    let lim = StackLimits::default();
    let StackFilter::Filter(fb) = blur() else { unreachable!() };
    let full = filter_plane(&src, &fb, lim.filter).unwrap();
    // Mask covering the left half only.
    let left = CoveragePlane { bounds: Rect { x: 0, y: 0, width: 16, height: 24 }, data: vec![u16::MAX; 16 * 24] };
    let mut e = StackEntry::new(blur());
    e.mask = Some(left.clone());
    let out = SmartFilterStack { entries: vec![e.clone()], stack_mask: None }.render(&src, &lim).unwrap();
    for y in 0..24 {
        for x in 0..32 {
            let i = (y * 32 + x) as usize;
            let want = if x < 16 { full.data[i] } else { src.data[i] };
            assert_eq!(out.data[i], want, "{x},{y}");
        }
    }
    // The same mask as the stack mask gives the same result.
    let via_stack = SmartFilterStack { entries: vec![StackEntry::new(blur())], stack_mask: Some(left) }.render(&src, &lim).unwrap();
    assert_eq!(via_stack, out);
    // Opacity 0.5: premultiplied midpoint.
    let mut half = StackEntry::new(blur());
    half.opacity = 0.5;
    let h = SmartFilterStack { entries: vec![half], stack_mask: None }.render(&src, &lim).unwrap();
    for i in [0usize, 37, 400, 767] {
        let (a, b) = (src.data[i], full.data[i]);
        let alpha = 0.5 * (f64::from(a[3]) + f64::from(b[3]));
        assert!((f64::from(h.data[i][3]) - alpha).abs() < 1e-6);
        for k in 0..3 {
            let p = 0.5 * (f64::from(a[k]) * f64::from(a[3]) + f64::from(b[k]) * f64::from(b[3]));
            assert!((f64::from(h.data[i][k]) - p / alpha).abs() < 1e-6, "{i} {k}");
        }
    }
}

#[test]
fn liquify_mesh_is_a_stack_entry() {
    let src = source();
    let lim = StackLimits::default();
    let mut mesh = LiquifyMesh::new(B, 4, &LiquifyLimits::default()).unwrap();
    let identity = StackEntry::new(StackFilter::Liquify { mesh: mesh.clone(), resample: ResampleFilter::Bicubic });
    assert_eq!(SmartFilterStack { entries: vec![identity], stack_mask: None }.render(&src, &lim).unwrap(), src);
    let samples = [
        StrokeSample { x: 8.0, y: 12.0, pressure: None, time_ms: None },
        StrokeSample { x: 16.0, y: 12.0, pressure: None, time_ms: None },
    ];
    mesh.apply_stroke(LiquifyTool::ForwardWarp, &samples, &LiquifyBrush { size: 16.0, ..LiquifyBrush::default() }, &LiquifyLimits::default()).unwrap();
    let entry = StackEntry::new(StackFilter::Liquify { mesh: mesh.clone(), resample: ResampleFilter::Bilinear });
    let out = SmartFilterStack { entries: vec![entry], stack_mask: None }.render(&src, &lim).unwrap();
    // Equals rendering the mesh directly.
    let colour: Vec<u8> = src.data.iter().flat_map(|p| p[..3].iter().flat_map(|c| c.to_le_bytes())).collect();
    let alpha: Vec<u8> = src.data.iter().flat_map(|p| p[3].to_le_bytes()).collect();
    let (c, a) = mesh.render(&ColourView { bounds: B, colour: &colour, alpha: &alpha }, ResampleFilter::Bilinear, &LiquifyLimits::default()).unwrap();
    let oc: Vec<u8> = out.data.iter().flat_map(|p| p[..3].iter().flat_map(|c| c.to_le_bytes())).collect();
    let oa: Vec<u8> = out.data.iter().flat_map(|p| p[3].to_le_bytes()).collect();
    assert_eq!((oc, oa), (c, a));
    assert_ne!(out, src);
}

#[test]
fn hostile_stacks_refuse_typed_before_any_filter_runs() {
    let src = source();
    let lim = StackLimits::default();
    let code = |s: &SmartFilterStack| s.render(&src, &lim).unwrap_err().code();
    let many = SmartFilterStack { entries: vec![StackEntry::new(blur()); 17], stack_mask: None };
    assert_eq!(code(&many), "smart_filter_too_many_entries");
    let mut e = StackEntry::new(blur());
    e.opacity = f32::NAN;
    assert_eq!(code(&SmartFilterStack { entries: vec![e.clone()], stack_mask: None }), "smart_filter_opacity");
    e.opacity = 1.5;
    assert_eq!(code(&SmartFilterStack { entries: vec![e], stack_mask: None }), "smart_filter_opacity");
    // A bad parameter in the LAST entry refuses before the first entry runs.
    let bad = StackEntry::new(StackFilter::Filter(LayerFilter::GaussianBlur { radius_px: f32::NAN }));
    assert_eq!(code(&SmartFilterStack { entries: vec![StackEntry::new(blur()), bad], stack_mask: None }), "filter_parameter");
    let huge = StackEntry::new(StackFilter::Filter(LayerFilter::Median { radius_px: 100_000 }));
    assert_eq!(code(&SmartFilterStack { entries: vec![huge], stack_mask: None }), "filter_parameter");
    // Liquify mesh over other bounds, or a corrupted mesh.
    let other = LiquifyMesh::new(Rect { x: 0, y: 0, width: 8, height: 8 }, 4, &LiquifyLimits::default()).unwrap();
    let wrong = StackEntry::new(StackFilter::Liquify { mesh: other, resample: ResampleFilter::Bilinear });
    assert_eq!(code(&SmartFilterStack { entries: vec![wrong], stack_mask: None }), "invalid_input");
    let mut broken = LiquifyMesh::new(B, 4, &LiquifyLimits::default()).unwrap();
    broken.disp[0] = [f64::INFINITY, 0.0];
    let broken = StackEntry::new(StackFilter::Liquify { mesh: broken, resample: ResampleFilter::Bilinear });
    assert_eq!(code(&SmartFilterStack { entries: vec![broken], stack_mask: None }), "nonfinite");
    // Over-budget filter window.
    let tight = StackLimits { filter: hsk_studio_pigment::filter_math::FilterLimits { max_bytes: 64, max_work: 64 }, ..lim };
    let r = SmartFilterStack { entries: vec![StackEntry::new(blur())], stack_mask: None }.render(&src, &tight);
    assert_eq!(r.unwrap_err().code(), "filter_over_budget");
}
