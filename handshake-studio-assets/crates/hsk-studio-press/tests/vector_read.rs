//! Lane SMB-ILLUSTRATOR-INTEROP: `read_vector_pages` records what a page draws (fills, strokes,
//! shadings, clips, transparency groups, text as outlines) in PDF user space. The fixture is
//! written by this crate's own `PdfExport`, so every expected value is the one drawn.
use hsk_studio_accord::CancellationToken;
use hsk_studio_press::*;

const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<PathCommand> {
    vec![PathCommand::MoveTo(x0, y0), PathCommand::LineTo(x1, y0), PathCommand::LineTo(x1, y1), PathCommand::LineTo(x0, y1), PathCommand::Close]
}

fn fixture() -> Vec<u8> {
    let mut ex = PdfExport::new(ExportOptions::default(), &Limits::default()).unwrap();
    let p = ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 200.0, 100.0], trim_box_pt: None, bleed_box_pt: None }).unwrap();
    ex.fill_path(p, &rect(10.0, 10.0, 60.0, 40.0), FillRule::EvenOdd, Colour::DeviceRgb([1.0, 0.0, 0.0])).unwrap();
    ex.stroke_path(p, &[PathCommand::MoveTo(10.0, 80.0), PathCommand::LineTo(190.0, 80.0)], Colour::DeviceRgb([0.0, 0.0, 1.0]), StrokeStyle { width_pt: 4.0, cap: LineCap::Round, join: LineJoin::Bevel }).unwrap();
    ex.push_clip(p, &rect(100.0, 10.0, 150.0, 40.0), FillRule::NonZero).unwrap();
    let stops = [GradientStop::new(0.0, [1.0, 1.0, 0.0]), GradientStop::new(1.0, [0.0, 1.0, 1.0])];
    let g = Gradient { geometry: GradientGeometry::Axial { from: [100.0, 0.0], to: [150.0, 0.0] }, stops: &stops, extend: [true, true], spread: GradientSpread::Pad, matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], profile: None };
    ex.fill_path_gradient(p, &rect(90.0, 0.0, 160.0, 50.0), FillRule::NonZero, &g).unwrap();
    ex.pop_clip(p).unwrap();
    ex.begin_group(p).unwrap();
    ex.fill_path(p, &rect(160.0, 10.0, 190.0, 40.0), FillRule::NonZero, Colour::DeviceRgb([0.0, 1.0, 0.0])).unwrap();
    ex.end_group(p, GroupStyle { opacity: 0.5, blend: BlendMode::Multiply, isolated: true, knockout: false, soft_mask: None }).unwrap();
    let font = ex.embed_truetype(INTER, 0, &[(161, 740)], &[(161, "H")]).unwrap();
    ex.show_glyphs(p, font, 24.0, [20.0, 50.0], Colour::DeviceGray(0.0), &[(161, 0.0, 0.0)]).unwrap();
    ex.finish(&CancellationToken::default()).unwrap().0
}

fn points(path: &[VectorEl]) -> Vec<[f64; 2]> {
    path.iter().filter_map(|e| match e {
        VectorEl::MoveTo(a) | VectorEl::LineTo(a) => Some(*a),
        VectorEl::CubicTo(_, _, a) => Some(*a),
        VectorEl::Close => None,
    }).collect()
}

#[test]
fn pages_record_fills_strokes_shadings_clips_groups_and_glyphs() {
    let pages = read_vector_pages(&fixture(), 1 << 24, &CancellationToken::default()).unwrap();
    assert_eq!(pages.len(), 1);
    let page = &pages[0];
    assert_eq!(page.media_box, [0.0, 0.0, 200.0, 100.0]);
    let draws: Vec<&VectorItem> = page.items.iter().filter(|i| matches!(i, VectorItem::Draw { .. })).collect();
    // The red rectangle, even-odd, in user space.
    let red = draws.iter().find_map(|i| match i {
        VectorItem::Draw { path, mode: VectorMode::Fill { even_odd }, paint: VectorPaint::Solid(c), .. } if c[0] > 0.99 && c[1] < 0.01 && c[2] < 0.01 => Some((points(path), *even_odd)),
        _ => None,
    });
    let (red, even_odd) = red.expect("red fill");
    assert!(even_odd);
    assert!(red.contains(&[10.0, 10.0]) && red.contains(&[60.0, 40.0]), "{red:?}");
    // The blue stroke: width 4, round cap, bevel join.
    let stroke = draws.iter().find_map(|i| match i {
        VectorItem::Draw { mode: VectorMode::Stroke(s), paint: VectorPaint::Solid(c), .. } if c[2] > 0.99 => Some(s.clone()),
        _ => None,
    }).expect("stroke");
    assert_eq!((stroke.width, stroke.cap, stroke.join), (4.0, 1, 2));
    // The axial shading inside the clip: yellow to cyan.
    let shading = draws.iter().find_map(|i| match i {
        VectorItem::Draw { paint: VectorPaint::Gradient { axial: true, stops, .. }, .. } => Some(stops.clone()),
        _ => None,
    }).expect("gradient");
    assert_eq!(shading.len(), GRADIENT_SAMPLES);
    let (first, last) = (shading[0].1, shading[GRADIENT_SAMPLES - 1].1);
    assert!(first[0] > 0.99 && first[2] < 0.01 && last[0] < 0.01 && last[2] > 0.99, "{first:?} {last:?}");
    assert!(page.items.iter().any(|i| matches!(i, VectorItem::PushClip { .. })) && page.items.iter().any(|i| matches!(i, VectorItem::PopClip)));
    // The transparency group: opacity 0.5, multiply, the green rectangle inside.
    let open = page.items.iter().position(|i| matches!(i, VectorItem::PushGroup { .. })).expect("group");
    let close = page.items.iter().rposition(|i| matches!(i, VectorItem::PopGroup)).expect("group end");
    assert!(page.items[open..close].iter().any(|i| matches!(i, VectorItem::Draw { paint: VectorPaint::Solid(c), .. } if c[1] > 0.99 && c[0] < 0.01)));
    let groups: Vec<(f32, u8)> = page.items.iter().filter_map(|i| match i {
        VectorItem::PushGroup { opacity, blend } => Some((*opacity, *blend)),
        _ => None,
    }).collect();
    assert!(groups.iter().any(|(o, b)| (*o - 0.5).abs() < 1e-6 && *b == 1) || page.items.iter().any(|i| matches!(i, VectorItem::Draw { blend: 1, .. })), "{groups:?}");
    // Text arrives as glyph outlines, noted.
    assert!(draws.iter().any(|i| matches!(i, VectorItem::Draw { glyph: true, .. })));
    assert!(page.notes.contains(&VectorNote::TextAsOutlines));
    // Hostile: not a PDF, over the byte cap, canceled.
    assert!(read_vector_pages(b"not a pdf", 1 << 24, &CancellationToken::default()).is_err());
    assert_eq!(read_vector_pages(&fixture(), 16, &CancellationToken::default()), Err(PressError::InputLimit("vector_read")));
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(read_vector_pages(&fixture(), 1 << 24, &canceled), Err(PressError::Canceled));
}
