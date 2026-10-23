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
    // Text arrives as glyph outlines carrying the glyph (Type 3 glyphs alone are noted as outlines).
    assert!(draws.iter().any(|i| matches!(i, VectorItem::Draw { glyph: Some(_), .. })));
    assert!(!page.notes.contains(&VectorNote::TextAsOutlines));
    // Hostile: not a PDF, over the byte cap, canceled.
    assert!(read_vector_pages(b"not a pdf", 1 << 24, &CancellationToken::default()).is_err());
    assert_eq!(read_vector_pages(&fixture(), 16, &CancellationToken::default()), Err(PressError::InputLimit("vector_read")));
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(read_vector_pages(&fixture(), 1 << 24, &canceled), Err(PressError::Canceled));
}

const CMYK_ICC: &[u8] = include_bytes!("../../hsk-studio-prism/tests/oracle/profiles/hsk-synth-cmyk-v2.icc");

/// A hand-assembled PDF (objects numbered from 1, exact xref offsets).
fn assemble(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend(body);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
    out
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend(data);
    v.extend(b"\nendstream");
    v
}

/// Two layers (the second hidden), CMYK, Lab, ICC-based CMYK, a spot ink and an RGB stroke, and
/// the ICC profile as the output intent: what an Illustrator PDF-compatible stream carries.
fn layered() -> Vec<u8> {
    let content = b"/OC /L1 BDC\n0 1 1 0 k 10 10 40 30 re f\n/Lab0 cs 50 20 -30 sc 60 10 20 20 re f\n/Icc cs 0.1 0.2 0.3 0.4 sc 90 10 20 20 re f\nEMC\n/OC /L2 BDC\n/Spot cs 0.5 scn 120 10 30 30 re f\nEMC\n0 0 1 RG 2 w 10 90 m 190 90 l S\n";
    assemble(&[
        b"<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R 6 0 R] /D << /Order [5 0 R 6 0 R] /OFF [6 0 R] >> >> /OutputIntents [<< /Type /OutputIntent /S /GTS_PDFX /OutputConditionIdentifier (CMYK) /DestOutputProfile 7 0 R >>] >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Properties << /L1 5 0 R /L2 6 0 R >> /ColorSpace << /Spot [/Separation /PANTONE#20185#20C /DeviceCMYK << /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.91 0.76 0] /N 1 >>] /Lab0 [/Lab << /WhitePoint [0.9642 1 0.8249] /Range [-128 127 -128 127] >>] /Icc [/ICCBased 7 0 R] >> >> /Contents 4 0 R >>".to_vec(),
        stream("", content),
        b"<< /Type /OCG /Name (Artwork) >>".to_vec(),
        b"<< /Type /OCG /Name (Hidden) >>".to_vec(),
        stream("/N 4", CMYK_ICC),
    ])
}

#[test]
fn documents_keep_source_colours_layers_and_glyph_text() {
    let cancel = CancellationToken::default();
    let doc = read_vector_document(&layered(), 1 << 24, &cancel).unwrap();
    assert_eq!(doc.layers, vec![VectorLayer { name: "Artwork".into(), visible: true }, VectorLayer { name: "Hidden".into(), visible: false }]);
    assert_eq!(doc.profiles.len(), 1);
    assert_eq!(doc.profiles[0].bytes, CMYK_ICC);
    assert_eq!((doc.profiles[0].components, doc.output_intent), (4, Some(0)));
    let page = &doc.pages[0];
    assert!(!page.notes.contains(&VectorNote::SourceColourUnmatched) && !page.notes.contains(&VectorNote::LayersUnmatched), "{:?}", page.notes);
    // Layer brackets in content order; the hidden layer's spot fill is read too.
    let order: Vec<String> = page
        .items
        .iter()
        .filter_map(|i| match i {
            VectorItem::BeginLayer { layer } => Some(format!("L{layer}")),
            VectorItem::EndLayer => Some("/L".into()),
            VectorItem::Draw { source: Some(VectorColour::Process { space, .. }), .. } => Some(format!("{space:?}")),
            VectorItem::Draw { source: Some(VectorColour::Spot { name, .. }), .. } => Some(name.clone()),
            _ => None,
        })
        .collect();
    let lab = "Lab { white: [0.9642, 1.0, 0.8249] }".to_string();
    assert_eq!(order, ["L0", "Cmyk", lab.as_str(), "Icc(0)", "/L", "L1", "PANTONE 185 C", "/L", "Rgb"]);
    let sources: Vec<&VectorColour> = page.items.iter().filter_map(|i| if let VectorItem::Draw { source: Some(c), .. } = i { Some(c) } else { None }).collect();
    assert_eq!(sources[0], &VectorColour::Process { space: VectorSpace::Cmyk, components: vec![0.0, 1.0, 1.0, 0.0] });
    assert_eq!(sources[1], &VectorColour::Process { space: VectorSpace::Lab { white: [0.9642, 1.0, 0.8249] }, components: vec![50.0, 20.0, -30.0] });
    assert_eq!(sources[2], &VectorColour::Process { space: VectorSpace::Icc(0), components: vec![0.1, 0.2, 0.3, 0.4] });
    assert_eq!(sources[3], &VectorColour::Spot { name: "PANTONE 185 C".into(), tint: 0.5, alternate: Some((VectorSpace::Cmyk, vec![0.0, 0.91, 0.76, 0.0])) });
    // Glyphs carry their font, id and text.
    let text = read_vector_document(&fixture(), 1 << 24, &cancel).unwrap();
    let glyph = text.pages[0].items.iter().find_map(|i| if let VectorItem::Draw { glyph: Some(g), .. } = i { Some(g.clone()) } else { None }).expect("glyph");
    assert_eq!((glyph.glyph_id, glyph.unicode.as_deref()), (161, Some("H")));
    assert!((glyph.matrix[3] * 1000.0 - 24.0).abs() < 1e-6 && (glyph.matrix[4] - 20.0).abs() < 1e-6, "{:?}", glyph.matrix);
    assert!(text.fonts[glyph.font].bytes.as_ref().is_some_and(|b| b.len() > 1000));
    // Hostile: over the byte cap before anything is read, canceled.
    assert_eq!(read_vector_document(&layered(), 16, &cancel), Err(PressError::InputLimit("vector_read")));
    let canceled = CancellationToken::default();
    canceled.cancel();
    assert_eq!(read_vector_document(&layered(), 1 << 24, &canceled), Err(PressError::Canceled));
}
