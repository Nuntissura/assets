//! SMB-ILLUSTRATOR-INTEROP-2 (feature `viewer`): clipping masks and vector masks are made in the
//! viewer, drawn, and exported: as PDF clip paths and soft masks, and as SVG clipPath / mask.
#![cfg(feature = "viewer")]

mod common;

use common::{Scratch, solid_psd};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::StudioMaskMode;
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness::view::vector::{PaintSpec, ShapeKind, VectorTool};
use hsk_studio_press::{VectorEl, VectorItem, VectorNote, read_vector_document, render_page};

const W: usize = 96;
const H: usize = 64;

fn px(f: &[u8], x: usize, y: usize) -> [u8; 3] {
    let i = (y * W + x) * 4;
    [f[i], f[i + 1], f[i + 2]]
}

#[test]
fn clips_and_masks_draw_and_export_to_pdf_and_svg() {
    let scratch = Scratch::new("vector-masks");
    let source = scratch.0.join("paper.psd");
    std::fs::write(&source, solid_psd(W as u32, H as u32, "paper", [255, 255, 255])).unwrap();
    let mut v = Viewer::new(Prefer::Cpu);
    v.open(&source).unwrap();
    v.set_vector_tool(VectorTool { fill_on: true, fill: [255, 0, 0], stroke_on: false, ..VectorTool::default() });
    // A red rectangle clipped by a smaller rectangle (20..40 x 15..35 px).
    let red = v.draw_shape(ShapeKind::Rectangle, [10.0, 10.0], [60.0, 40.0], false).unwrap();
    let clipper = v.draw_shape(ShapeKind::Rectangle, [20.0, 15.0], [40.0, 35.0], false).unwrap();
    v.make_clip(&red, &clipper, false).unwrap();
    // A blue rectangle (66..90 x 10..40) under a luminance mask: white over its left part only.
    let blue = v.draw_shape(ShapeKind::Rectangle, [66.0, 10.0], [90.0, 40.0], false).unwrap();
    v.set_fill(&blue, Some(&PaintSpec::Solid([0, 0, 255]))).unwrap();
    let white = v.draw_shape(ShapeKind::Rectangle, [60.0, 5.0], [78.0, 45.0], false).unwrap();
    v.set_fill(&white, Some(&PaintSpec::Solid([255, 255, 255]))).unwrap();
    v.make_vector_mask(&blue, &white, StudioMaskMode::Luminance, false).unwrap();
    // A green rectangle under an inverted VECTOR mask: a hole where the mask shape is.
    v.set_vector_tool(VectorTool { fill_on: true, fill: [0, 255, 0], stroke_on: false, ..VectorTool::default() });
    let green = v.draw_shape(ShapeKind::Rectangle, [10.0, 46.0], [60.0, 60.0], false).unwrap();
    let hole = v.draw_shape(ShapeKind::Rectangle, [30.0, 40.0], [40.0, 64.0], false).unwrap();
    v.make_vector_mask(&green, &hole, StudioMaskMode::Vector, true).unwrap();
    v.render_if_dirty();
    let f = v.frame().unwrap_or_else(|| panic!("no frame: {}", v.state_json())).rgba8.clone();
    assert_eq!((px(&f, 30, 25), px(&f, 12, 12)), ([255, 0, 0], [255, 255, 255]), "clipped: red inside, paper outside");
    assert_eq!((px(&f, 70, 25), px(&f, 85, 25)), ([0, 0, 255], [255, 255, 255]), "masked: blue under white only");
    assert_eq!((px(&f, 20, 53), px(&f, 35, 53)), ([0, 255, 0], [255, 255, 255]), "inverted vector mask: a hole");

    // PDF: the clip and the vector mask are clip paths (the page renders them), the luminance mask
    // a soft mask; no clip or mask is reported lost.
    let (pdf, losses) = v.vector_pdf_bytes().unwrap();
    assert!(losses.iter().all(|l| l.contains("raster_layer_not_exported") || l.contains("rasterized_at_document_resolution")), "{losses:?}");
    let doc = read_vector_document(&pdf, 1 << 24, &CancellationToken::default()).unwrap();
    // The clip rectangle (20, 15) px is (15, 36.75) pt; the inverted vector mask is even-odd
    // (transparency groups add their own bounding-box clips).
    let clips: Vec<(Vec<[f64; 2]>, bool)> = doc.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            VectorItem::PushClip { path, even_odd } => Some((path.iter().filter_map(|e| if let VectorEl::MoveTo(q) | VectorEl::LineTo(q) = e { Some(*q) } else { None }).collect(), *even_odd)),
            _ => None,
        })
        .collect();
    assert!(clips.iter().any(|(p, eo)| !eo && p.iter().any(|q| (q[0] - 15.0).abs() < 1e-6 && (q[1] - 36.75).abs() < 1e-6)), "{clips:?}");
    assert!(clips.iter().any(|(_, eo)| *eo), "{clips:?}");
    assert!(doc.pages[0].notes.contains(&VectorNote::SoftMaskNotRead), "the soft mask is written");
    let page = render_page(&pdf, 0, 4.0 / 3.0, &hsk_studio_press::Limits::default(), &CancellationToken::default()).unwrap();
    let rgb = |x: usize, y: usize| {
        let i = (y * page.width as usize + x) * 3;
        [page.rgb[i], page.rgb[i + 1], page.rgb[i + 2]]
    };
    assert_eq!((rgb(30, 25), rgb(12, 12), rgb(20, 53), rgb(35, 53)), ([255, 0, 0], [255, 255, 255], [0, 255, 0], [255, 255, 255]));

    // SVG: a clipPath for the clip and the vector mask, a mask for the luminance mask.
    let (svg, losses) = v.vector_svg().unwrap();
    assert_eq!(svg.matches("<clipPath").count(), 1, "{svg}");
    assert!(svg.contains("<mask") && svg.contains("clip-path=") && svg.contains("mask="), "{svg}");
    assert!(losses.iter().any(|l| l.contains("vector_mask_not_exported")), "the inverted vector mask is reported: {losses:?}");

    // Hostile: a layer cannot clip itself; releasing shows the source again.
    assert!(v.make_clip(&red, &red, false).is_err());
    v.release_masks(&red).unwrap();
    assert!(v.layers().iter().any(|r| r.layer_id == clipper && r.visible));
    assert!(v.release_masks(&red).is_err());
}
