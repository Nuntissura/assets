//! Lane SMB-INTEROP-FIG-PAINTS-2: Figma image fills rendered from the `.fig` file's own image part
//! (decoded by hsk-studio-interop-raster, dev edge approved 2026-10-15) and the basic-shapes
//! artboard re-measured against the Operator PNG at tolerances from the run.
mod common;
use common::*;
use hsk_studio_folio::{Budget, StudioImageScale};
use hsk_studio_interop_fig::{FigFontGrant, resolve_text_outputs};

/// Largest channel difference between our pixel (over the PNG background) and the PNG.
fn diff(r: &Render, png: &(usize, usize, Vec<u8>), at: (usize, usize), origin: (usize, usize)) -> f64 {
    let bg = [png.2[0], png.2[1], png.2[2]];
    let ours = over(r, at.0, at.1, bg);
    let o = ((at.1 - origin.1) * png.0 + (at.0 - origin.0)) * 4;
    (0..3).map(|c| (ours[c] - f64::from(png.2[o + c])).abs()).fold(0.0, f64::max)
}

#[test]
fn image_fill_renders_from_the_fig_image_part() {
    let fig = fixture("fig-basic-shapes.fig");
    let p = map(&document("fig-basic-shapes.fig"), Budget::default());
    assert_eq!(p.vector_records.image_paints.len(), 1);
    assert_eq!(p.vector_records.image_paints[0].scale_mode, StudioImageScale::Fill);
    let images = Images::from_fig(&fig, &p);
    assert_eq!(images.0.len(), 1);
    let r = render_with(&p, None, Some(&images));
    // The image paint is drawn now: only the text layer (no fonts in this test) is refused.
    eprintln!("refused {:?}", r.refused);
    assert!(!r.refused.contains_key("image_paint_unresolved"));
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    // Probes inside the image ellipse (artboard 40..210 x 430..550 -> canvas 140..310 x 530..650).
    let mut d: Vec<f64> = Vec::new();
    for y in (560..630).step_by(10) {
        for x in (170..290).step_by(10) {
            d.push(diff(&r, &png, (x, y), (100, 100)));
        }
    }
    d.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = d.iter().sum::<f64>() / d.len() as f64;
    let median = d[d.len() / 2];
    eprintln!("image probes {} mean {mean:.6} median {median:.6} max {:.6}", d.len(), d[d.len() - 1]);
    // Measured over 84 probes: mean 1.926/255, median 1.364, max 12.683 (bilinear placement of the
    // JPEG vs Figma's resampling; Figma's FILL maths UNVERIFIED, composite class Approximate).
    assert!(mean <= 1.93, "image mean {mean:.3}");
    assert!(median <= 1.37, "image median {median:.3}");
    assert!(d[d.len() - 1] <= 12.7, "image max {:.3}", d[d.len() - 1]);
}

#[test]
fn basic_shapes_artboard_remeasured_with_image_fill() {
    let fig = fixture("fig-basic-shapes.fig");
    let p = map(&document("fig-basic-shapes.fig"), Budget::default());
    let images = Images::from_fig(&fig, &p);
    let r = render_with(&p, None, Some(&images));
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    let d = diffs(&r, &png, (100, 100));
    let n = d.len() as f64;
    let within = |t: f64| d.iter().filter(|v| **v <= t).count() as f64 / n;
    let mean = d.iter().sum::<f64>() / n;
    eprintln!(
        "basic-shapes+image: refused {:?} mean {mean:.6} <=2: {:.6} <=8: {:.6} <=32: {:.6} p99 {:.4}",
        r.refused,
        within(2.0),
        within(8.0),
        within(32.0),
        d[(n * 0.99) as usize]
    );
    // Measured: mean 2.478479/255, 95.8682 % within 2/255, 98.3156 % within 8/255 (5.0056 / 96.41 %
    // before the image fill rendered). Residue: the text layer, whose bold run needs Inter Bold
    // (not in the repository); Figma parity of this fixture stays UNVERIFIED.
    assert_eq!(r.refused.get("text_to_image_conversion"), Some(&1));
    assert!(within(8.0) >= 0.98315, "within 8/255 {:.4}", within(8.0));
    assert!(mean <= 2.479, "mean {mean:.4}");
}

#[test]
fn bold_text_renders_with_the_inter_bold_fixture() {
    let fig = fixture("fig-basic-shapes.fig");
    let p = map(&document("fig-basic-shapes.fig"), Budget::default());
    let files = [FontFile::inter_regular(), FontFile::inter_bold()];
    let grants = [
        FigFontGrant { family: "Inter", style: "Regular", resource: files[0].resource() },
        FigFontGrant { family: "Inter", style: "Bold", resource: files[1].resource() },
    ];
    let out = resolve_text_outputs(&p, &grants);
    // "Regular text + bold text": both runs resolve; nothing is missing now.
    assert!(out.missing_fonts.is_empty(), "{:?}", out.missing_fonts);
    assert_eq!(out.outputs.len(), 1);
    let ids: Vec<&str> = out.outputs[0].runs.iter().map(|r| r.font.identity).collect();
    assert_eq!(ids, ["Inter-Regular", "Inter-Bold"]);
    let text = prepare_outputs(&p, &out.outputs, &files);
    let images = Images::from_fig(&fig, &p);
    let r = render_with(&p, Some(&text), Some(&images));
    eprintln!("refused {:?}", r.refused);
    let png = decode_png(&fixture("fig-basic-shapes.png"));
    // The text line region (canvas x 370..690, y 560..600).
    let mut d: Vec<f64> = Vec::new();
    for y in 560..600 {
        for x in 370..690 {
            d.push(diff(&r, &png, (x, y), (100, 100)));
        }
    }
    d.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = d.len() as f64;
    let mean = d.iter().sum::<f64>() / n;
    let within8 = d.iter().filter(|v| **v <= 8.0).count() as f64 / n;
    eprintln!("text region mean {mean:.6} <=8 {within8:.6} p99 {:.6} max {:.6}", d[(n * 0.99) as usize], d[d.len() - 1]);
    // Whole artboard with image fill and text.
    let all = diffs(&r, &png, (100, 100));
    let m = all.len() as f64;
    let amean = all.iter().sum::<f64>() / m;
    let a8 = all.iter().filter(|v| **v <= 8.0).count() as f64 / m;
    let a2 = all.iter().filter(|v| **v <= 2.0).count() as f64 / m;
    let ap99 = all[(m * 0.99) as usize];
    eprintln!("basic-shapes+image+text: mean {amean:.6} <=2 {a2:.6} <=8 {a8:.6} p99 {ap99:.6}");
    // Measured (fonts: Inter Regular 3.019 from the type fixtures, Inter Bold 4.1 from these
    // fixtures; Figma uses its own Inter build): nothing refused; text region mean 3.862939/255,
    // 91.4453 % within 8/255; whole artboard mean 0.920768/255, 96.7568 % within 2/255, 99.3241 %
    // within 8/255, p99 5.697709 (2.478479 / 98.3156 % before the text rendered). Residue: glyph
    // antialiasing and font versions, image resampling, the drop shadow and blur edges.
    assert!(r.refused.is_empty(), "{:?}", r.refused);
    assert!(mean <= 3.863, "text region mean {mean:.6}");
    assert!(within8 >= 0.91445, "text region within 8/255 {within8:.6}");
    assert!(amean <= 0.9208, "artboard mean {amean:.6}");
    assert!(a8 >= 0.99324, "artboard within 8/255 {a8:.6}");
    assert!(ap99 <= 5.698, "artboard p99 {ap99:.6}");
}
