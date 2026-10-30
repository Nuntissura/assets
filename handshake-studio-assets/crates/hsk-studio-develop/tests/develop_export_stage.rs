//! Lane SMB-LR-EXPORT-SIZING-1: the develop export stage (image sizing, output sharpening, graphic watermark) that
//! runs on the linear-light developed image before the output transfer (module 14: STU-IO-134..138). Every expected
//! size, value and count here is computed by hand or from the stated Studio reading, never snapshotted. Lightroom
//! Classic sizing, sharpening and watermark behaviour is UNVERIFIED (no oracle, D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_develop::*;

fn image(rgb: Vec<f32>, width: u32, height: u32) -> DevelopedImage {
    DevelopedImage { width, height, rgb }
}

fn flat(width: u32, height: u32, v: f32) -> DevelopedImage {
    image(vec![v; (width * height * 3) as usize], width, height)
}

fn sizing(resize_type: ResizeType) -> OutputSizing {
    OutputSizing { constrain_size: true, resize_type, ..OutputSizing::default() }
}

fn fit(s: &OutputSizing, w: u32, h: u32) -> (u32, u32) {
    let f = fit_output_size(s, w, h).unwrap_or_else(|e| panic!("{:?} {w}x{h}: {e:?}", s.resize_type));
    (f.width, f.height)
}

fn stage(sizing: OutputSizing) -> ExportStage {
    ExportStage { sizing, ..ExportStage::default() }
}

/// Every resize type on a landscape 3000x2000 and a portrait 2000x3000 source, against sizes computed by hand from
/// the stated rules (round half up; Megapixels floors both edges so the area never exceeds the target).
#[test]
fn sizing_every_resize_type_fits_portrait_and_landscape_by_hand() {
    // Defaults: STU-IO-137 (1000, 1000, 100 %, 240) and the studio_declared ones.
    let d = OutputSizing::default();
    assert_eq!((d.max_width, d.max_height, d.percentage, d.resolution), (1000.0, 1000.0, 100.0, 240.0));
    assert_eq!((d.size_unit, d.resolution_unit), (SizeUnit::Pixels, ResolutionUnit::PixelsPerInch));
    assert!(d.constrain_size && !d.do_not_enlarge);
    let mut s = sizing(ResizeType::WidthHeight);
    (s.max_width, s.max_height, s.percentage, s.megapixels) = (1000.0, 800.0, 25.0, 1.5);
    let cases = [
        // min(1000/3000, 800/2000) = 1/3 -> 1000 x 666.67; portrait min(1000/2000, 800/3000) = 4/15 -> 533.33 x 800.
        (ResizeType::WidthHeight, (1000, 667), (533, 800)),
        // long box 1000, short box 800 in either orientation: 1/3 both ways.
        (ResizeType::Dimensions, (1000, 667), (667, 1000)),
        // long edge = max_width (1000).
        (ResizeType::LongEdge, (1000, 667), (667, 1000)),
        // short edge = max_width (1000): scale 1/2.
        (ResizeType::ShortEdge, (1500, 1000), (1000, 1500)),
        // 1.5 MP of 6 MP: scale sqrt(1/4) = 1/2.
        (ResizeType::Megapixels, (1500, 1000), (1000, 1500)),
        // 25 %.
        (ResizeType::Percentage, (750, 500), (500, 750)),
    ];
    for (t, landscape, portrait) in cases {
        s.resize_type = t;
        assert_eq!(fit(&s, 3000, 2000), landscape, "{t:?} landscape");
        assert_eq!(fit(&s, 2000, 3000), portrait, "{t:?} portrait");
        assert_eq!(fit_output_size(&s, 3000, 2000).unwrap().rule, t.fit_rule());
    }
    // Megapixels rounding: 2 MP of 4000x3000 is scale sqrt(1/6) -> 1632.99 x 1224.74; round half up would give
    // 1633 x 1225 = 2 000 425 px (over the target), the floor gives 1632 x 1224 = 1 997 568 px.
    let mut mp = sizing(ResizeType::Megapixels);
    mp.megapixels = 2.0;
    assert_eq!(fit(&mp, 4000, 3000), (1632, 1224));
    assert_eq!(fit(&mp, 3000, 4000), (1224, 1632));
    // Physical units: 10 cm at 240 ppi = 944.88 -> 945 px box; 4 in at 100 ppcm (254 ppi) = 1016 px.
    let mut cm = sizing(ResizeType::LongEdge);
    (cm.max_width, cm.size_unit) = (10.0, SizeUnit::Centimetres);
    assert_eq!(fit(&cm, 3000, 2000), (945, 630));
    let mut inch = sizing(ResizeType::LongEdge);
    (inch.max_width, inch.size_unit, inch.resolution, inch.resolution_unit) = (4.0, SizeUnit::Inches, 100.0, ResolutionUnit::PixelsPerCm);
    assert_eq!(fit(&inch, 2000, 3000), (677, 1016));
    // constrain_size off: the source size, whatever the type says.
    let mut off = sizing(ResizeType::Percentage);
    (off.constrain_size, off.percentage) = (false, 10.0);
    assert_eq!(fit(&off, 3000, 2000), (3000, 2000));
    assert_eq!(fit_output_size(&off, 3000, 2000).unwrap().rule, "unconstrained");
}

/// do_not_enlarge never upsamples: a 40x30 source asked for a 100 px long edge stays 40x30 and its pixels are copied
/// exactly; without the flag it is enlarged to 100x75.
#[test]
fn do_not_enlarge_never_upsamples_a_small_source() {
    let rgb: Vec<f32> = (0..40 * 30 * 3).map(|i| (i % 97) as f32 / 97.0).collect();
    let src = image(rgb, 40, 30);
    let mut s = sizing(ResizeType::LongEdge);
    (s.max_width, s.do_not_enlarge) = (100.0, true);
    let (out, receipt) = apply_export_stage(&src, &stage(s.clone()), &CancellationToken::default()).unwrap();
    assert_eq!((out.width, out.height), (40, 30));
    assert_eq!(out.rgb, src.rgb, "no resample when the output is the source size");
    assert_eq!((receipt.source_size, receipt.output_size, receipt.resized), ((40, 30), (40, 30), false));
    s.do_not_enlarge = false;
    let (big, receipt) = apply_export_stage(&src, &stage(s), &CancellationToken::default()).unwrap();
    assert_eq!((big.width, big.height, big.rgb.len()), (100, 75, 100 * 75 * 3));
    assert_eq!((receipt.output_size, receipt.resized, receipt.filter), ((100, 75), true, RESAMPLE_FILTER));
    // A flat field resizes to itself exactly, up and down.
    for (w, h) in [(100, 75), (13, 10)] {
        let mut s = sizing(ResizeType::WidthHeight);
        (s.max_width, s.max_height) = (f64::from(w), f64::from(h));
        let (o, _) = apply_export_stage(&flat(40, 30, 0.375), &stage(s), &CancellationToken::default()).unwrap();
        assert_eq!((o.width, o.height), (w, h));
        assert!(o.rgb.iter().all(|v| *v == 0.375), "flat {w}x{h}");
    }
}

/// A 1-pixel checkerboard (0.2 / 0.8 linear) reduced 300x200 -> 128x85 (factor 2.34) keeps its mean, and the
/// stretched-kernel reduction leaves no aliasing beat. Expectation, computed: the pattern is (-1)^(x+y), separable,
/// so an output pixel is mean + 0.3 * A(x) * A(y) with A the filter's response to (-1)^i. A point sampler has
/// |A| = 1 (swings of +-0.3); the Keys cubic stretched by the factor has |A| <= 0.02 away from the edges (worked
/// for these sizes: interior |A| <= 0.0065) and <= 0.2 at the replicated edges, so every pixel lies within
/// 0.3 * 0.2 * 0.2 = 0.012 of the mean and every pixel 3 or more from the edges within 0.3 * 0.02^2 = 1.2e-4.
#[test]
fn checkerboard_downscale_keeps_the_mean_without_an_aliasing_beat() {
    let (w, h) = (300u32, 200u32);
    let rgb: Vec<f32> = (0..h)
        .flat_map(|y| (0..w).map(move |x| if (x + y) % 2 == 0 { 0.8f32 } else { 0.2 }))
        .flat_map(|v| [v; 3])
        .collect();
    let input_mean = rgb.iter().map(|v| f64::from(*v)).sum::<f64>() / rgb.len() as f64;
    let mut s = sizing(ResizeType::LongEdge);
    s.max_width = 128.0;
    let (out, receipt) = apply_export_stage(&image(rgb, w, h), &stage(s), &CancellationToken::default()).unwrap();
    // 200 * 128 / 300 = 85.33 -> 85.
    assert_eq!((out.width, out.height), (128, 85));
    assert_eq!(receipt.fit_rule, "long_edge");
    let mean = out.rgb.iter().map(|v| f64::from(*v)).sum::<f64>() / out.rgb.len() as f64;
    assert!((mean - input_mean).abs() <= 1e-4, "mean {mean} vs input {input_mean}");
    let mut worst = 0.0f64;
    let mut worst_inside = 0.0f64;
    for (i, px) in out.rgb.chunks_exact(3).enumerate() {
        let (x, y) = (i as u32 % out.width, i as u32 / out.width);
        let dev = (f64::from(px[0]) - input_mean).abs();
        worst = worst.max(dev);
        if (3..out.width - 3).contains(&x) && (3..out.height - 3).contains(&y) {
            worst_inside = worst_inside.max(dev);
        }
        assert_eq!(px[0], px[1]);
        assert_eq!(px[1], px[2]);
    }
    assert!(worst <= 0.012, "edge residual {worst}");
    assert!(worst_inside <= 1.2e-4, "aliasing beat {worst_inside}");
}

/// Hostile sizing settings are typed refusals naming the key, checked before any pixel work; an output that would
/// overflow the stage's pixel budget is refused, never allocated.
#[test]
fn hostile_sizing_settings_get_typed_refusals() {
    let src = flat(30, 20, 0.5);
    let refuse = |s: OutputSizing| {
        let e = apply_export_stage(&src, &stage(s.clone()), &CancellationToken::default()).unwrap_err();
        assert_eq!(fit_output_size(&s, 3000, 2000).unwrap_err().code(), e.code());
        (e.code(), e.detail())
    };
    for v in [0.0, -5.0, 0.5] {
        let mut s = sizing(ResizeType::WidthHeight);
        s.max_width = v;
        assert_eq!(refuse(s), ("export_setting_out_of_range", "max_width"), "max_width {v}");
        let mut s = sizing(ResizeType::WidthHeight);
        s.max_height = v;
        assert_eq!(refuse(s), ("export_setting_out_of_range", "max_height"), "max_height {v}");
    }
    let mut s = sizing(ResizeType::Percentage);
    s.percentage = 0.0;
    assert_eq!(refuse(s), ("export_setting_out_of_range", "resize_percentage"));
    for v in [f64::NAN, f64::INFINITY] {
        let mut s = sizing(ResizeType::LongEdge);
        s.resolution = v;
        assert_eq!(refuse(s), ("export_setting_not_finite", "resolution"));
    }
    let mut s = sizing(ResizeType::LongEdge);
    s.resolution = 0.0;
    assert_eq!(refuse(s), ("export_setting_out_of_range", "resolution"));
    let mut s = sizing(ResizeType::Megapixels);
    s.megapixels = -1.0;
    assert_eq!(refuse(s), ("export_setting_out_of_range", "megapixels"));
    // 1e12 MP of a 30x20 source: the edges overflow every bound; refused before allocation.
    let mut s = sizing(ResizeType::Megapixels);
    s.megapixels = 1e12;
    assert_eq!(refuse(s).0, "export_output_too_large");
    let mut s = sizing(ResizeType::Percentage);
    s.percentage = 1e9;
    assert_eq!(refuse(s).0, "export_output_too_large");
    // An image whose buffer does not match its size is the develop code.
    let bad = image(vec![0.5; 10], 30, 20);
    let e = apply_export_stage(&bad, &ExportStage::default(), &CancellationToken::default()).unwrap_err();
    assert_eq!(e.code(), DevelopError::InvalidDimensions.code());
    // A cancelled token stops the stage.
    let cancel = CancellationToken::default();
    cancel.cancel();
    let mut s = sizing(ResizeType::Percentage);
    s.percentage = 50.0;
    let e = apply_export_stage(&src, &stage(s), &cancel).unwrap_err();
    assert_eq!(e.code(), DevelopError::Canceled.code());
}

// ---------------------------------------------------------------------------------------------------------------
// Slice 2: output sharpening (unsharp mask after sizing; Lightroom's algorithm is not public, UNVERIFIED, D5).

const SRGB_V4: &[u8] = include_bytes!("profiles/sRGB-v4.icc");

/// The stated table (Studio reading): Gaussian sigma in output pixels (Screen 0.5 px; Glossy 1/240 in and Matte
/// 1/160 in, times the output ppi) and strength per level 1..3.
fn stated(media: SharpenMedia, level: u8, ppi: f64) -> (f64, f64) {
    let sigma = match media {
        SharpenMedia::Screen => 0.5,
        SharpenMedia::Glossy => ppi / 240.0,
        SharpenMedia::Matte => ppi / 160.0,
    };
    let per_level = match media {
        SharpenMedia::Screen => [0.4, 0.8, 1.2],
        SharpenMedia::Glossy => [0.5, 1.0, 1.5],
        SharpenMedia::Matte => [0.6, 1.2, 1.8],
    };
    (sigma, per_level[usize::from(level - 1)])
}

/// Centre weight of the normalised discrete Gaussian over -R..=R, R = max(1, ceil(3 sigma)).
fn centre_weight(sigma: f64) -> (f64, u32) {
    let r = (3.0 * sigma).ceil().max(1.0) as i32;
    let sum: f64 = (-r..=r).map(|d| (-f64::from(d * d) / (2.0 * sigma * sigma)).exp()).sum();
    (1.0 / sum, r as u32)
}

fn sharpen_stage(media: SharpenMedia, level: u8, resolution: f64) -> ExportStage {
    let mut s = ExportStage::default();
    s.sizing.constrain_size = false;
    s.sizing.resolution = resolution;
    s.sharpening = OutputSharpening { enabled: true, media, level };
    s
}

fn step(width: u32, height: u32, at: u32, a: f32, b: f32) -> DevelopedImage {
    let rgb = (0..height).flat_map(|_| (0..width).map(move |x| if x < at { a } else { b })).flat_map(|v| [v; 3]).collect();
    image(rgb, width, height)
}

/// A vertical step a -> b gains, on each side of the edge, the overshoot of an unsharp mask with the stated row:
/// out(edge) = b + s (b - a)(1 - w0) / 2 and out(edge - 1) = a - s (b - a)(1 - w0) / 2, where w0 is the Gaussian's
/// normalised centre weight (half the kernel, plus half the centre tap, lies on the pixel's own side).
#[test]
fn step_edge_gains_the_stated_overshoot_per_media_and_level() {
    let (a, b) = (0.3f32, 0.7f32);
    let src = step(40, 4, 20, a, b);
    for media in [SharpenMedia::Screen, SharpenMedia::Glossy, SharpenMedia::Matte] {
        for level in 1..=3u8 {
            let (out, receipt) = apply_export_stage(&src, &sharpen_stage(media, level, 240.0), &CancellationToken::default()).unwrap();
            let (sigma, strength) = stated(media, level, 240.0);
            let (w0, radius) = centre_weight(sigma);
            let row = receipt.sharpening.expect("sharpening row");
            assert_eq!((row.media, row.level, row.kernel_radius), (media, level, radius), "{media:?} {level}");
            assert!((row.sigma_px - sigma).abs() < 1e-12 && (row.strength - strength).abs() < 1e-12, "{media:?} {level}: {row:?}");
            let over = strength * f64::from(b - a) * (1.0 - w0) / 2.0;
            for y in 0..4usize {
                let at = |x: usize| f64::from(out.rgb[(y * 40 + x) * 3]);
                assert!((at(20) - (f64::from(b) + over)).abs() < 1e-6, "{media:?} {level} bright side {} want {}", at(20), f64::from(b) + over);
                assert!((at(19) - (f64::from(a) - over)).abs() < 1e-6, "{media:?} {level} dark side {}", at(19));
                assert_eq!(at(0), f64::from(a), "far from the edge nothing changes");
                assert_eq!(at(39), f64::from(b));
            }
        }
    }
    // The kernel scales with the output resolution for print media (Matte at 480 ppi: sigma 3 px) and not for Screen.
    let (_, r) = apply_export_stage(&src, &sharpen_stage(SharpenMedia::Matte, 2, 480.0), &CancellationToken::default()).unwrap();
    assert_eq!(r.sharpening.unwrap().sigma_px, 3.0);
    let (_, r) = apply_export_stage(&src, &sharpen_stage(SharpenMedia::Screen, 2, 480.0), &CancellationToken::default()).unwrap();
    assert_eq!(r.sharpening.unwrap().sigma_px, 0.5);
}

/// A flat field is unchanged, bit for bit, by every (media, level).
#[test]
fn flat_field_is_unchanged_by_output_sharpening() {
    let src = flat(17, 9, 0.4);
    for media in [SharpenMedia::Screen, SharpenMedia::Glossy, SharpenMedia::Matte] {
        for level in 1..=3u8 {
            let (out, receipt) = apply_export_stage(&src, &sharpen_stage(media, level, 300.0), &CancellationToken::default()).unwrap();
            assert_eq!(out, src, "{media:?} {level}");
            assert_eq!(receipt.out_of_range, ClipReport::default());
        }
    }
}

/// Sharpening a 0 -> 1 step pushes values out of 0..1; the stage counts them and the output transfer clamps and
/// counts exactly those (Matte High at 240 ppi: sigma 1.5, R 5, so 5 columns each side, 4 rows, 3 channels = 60).
/// Level 0 and 4 are typed refusals (STU-IO-137 hard 1..3).
#[test]
fn sharpening_clip_is_counted_in_the_clip_report() {
    let src = step(40, 4, 20, 0.0, 1.0);
    let (out, receipt) = apply_export_stage(&src, &sharpen_stage(SharpenMedia::Matte, 3, 240.0), &CancellationToken::default()).unwrap();
    assert_eq!(receipt.out_of_range, ClipReport { below_zero: 60, above_one: 60 });
    assert_eq!(receipt.sharpen_out_of_range, receipt.out_of_range, "sizing was off, so sharpening made all of them");
    let rendered = render_output(&out, OutputDepth::Eight, SRGB_V4, None).unwrap();
    assert_eq!(rendered.clip, receipt.out_of_range);
    for level in [0u8, 4, u8::MAX] {
        let e = apply_export_stage(&src, &sharpen_stage(SharpenMedia::Glossy, level, 240.0), &CancellationToken::default()).unwrap_err();
        assert_eq!((e.code(), e.detail()), ("export_setting_out_of_range", "output_sharpening_level"), "level {level}");
    }
    // A print kernel wider than the stage bound (sigma above 64 px) is refused, never truncated.
    let e = apply_export_stage(&src, &sharpen_stage(SharpenMedia::Matte, 2, 1e6), &CancellationToken::default()).unwrap_err();
    assert_eq!((e.code(), e.detail()), ("export_setting_out_of_range", "output_sharpening_radius"));
    // Disabled sharpening records no row and changes nothing.
    let mut off = sharpen_stage(SharpenMedia::Matte, 3, 240.0);
    off.sharpening.enabled = false;
    let (o, r) = apply_export_stage(&src, &off, &CancellationToken::default()).unwrap();
    assert_eq!((o, r.sharpening), (src, None));
}
