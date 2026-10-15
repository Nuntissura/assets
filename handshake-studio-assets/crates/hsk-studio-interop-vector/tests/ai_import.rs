//! Lane SMB-ILLUSTRATOR-INTEROP: `.ai` import through the PDF-compatible stream. The drawing comes
//! from a hand-built provider (Press reads real PDFs; this crate has no Press edge), so every
//! expected Folio value is derived from the drawn items: y flips from the crop box top, artboards
//! sit left to right 20 pt apart, a stroke repeating the fill's path joins its layer.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{Payload, StudioAppearanceRow, StudioPaint};
use hsk_studio_interop_vector::*;

const AI: &[u8] = b"%PDF-1.6\n%AI12_FileFormatLevel: 3\n<< /AIPrivateData1 1 0 R >>\n";

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<PdfEl> {
    vec![PdfEl::MoveTo([x0, y0]), PdfEl::LineTo([x1, y0]), PdfEl::LineTo([x1, y1]), PdfEl::LineTo([x0, y1]), PdfEl::Close]
}

struct Drawn(Vec<PdfDrawingPage>);
impl PdfDrawingProvider for Drawn {
    fn drawing(&self, _: &[u8], _: &CancellationToken) -> Result<PdfDrawing> {
        Ok(PdfDrawing { pages: self.0.clone(), ..PdfDrawing::default() })
    }
}

fn drawing() -> Drawn {
    let stroke = PdfStroke { width: 4.0, cap: 1, join: 2, miter_limit: 10.0, dash: vec![3.0], dash_offset: 0.0 };
    let page1 = PdfDrawingPage {
        media_box: [0.0, 0.0, 200.0, 100.0],
        crop_box: [0.0, 0.0, 200.0, 100.0],
        items: vec![
            PdfDrawItem::Fill { path: rect(10.0, 10.0, 60.0, 40.0), even_odd: true, paint: PdfPaint::Solid([1.0, 0.0, 0.0, 1.0]), blend: 0, glyph: None, source: None },
            PdfDrawItem::Stroke { path: rect(10.0, 10.0, 60.0, 40.0), stroke, paint: PdfPaint::Solid([0.0, 0.0, 1.0, 0.5]), blend: 0, glyph: None, source: None },
            PdfDrawItem::PushClip { path: rect(100.0, 10.0, 150.0, 40.0), even_odd: false },
            PdfDrawItem::Fill {
                path: rect(90.0, 0.0, 160.0, 50.0),
                even_odd: false,
                paint: PdfPaint::Gradient { axial: true, coords: [0.0, 0.0, 1.0, 0.0, 0.0, 0.0], matrix: [50.0, 0.0, 0.0, 1.0, 100.0, 0.0], stops: vec![(0.0, [1.0, 1.0, 0.0, 1.0]), (1.0, [0.0, 1.0, 1.0, 1.0])], extend: [true, true] },
                blend: 0,
                glyph: None,
                source: None,
            },
            PdfDrawItem::PopClip,
            PdfDrawItem::PushGroup { opacity: 0.5, blend: 1 },
            PdfDrawItem::Fill { path: rect(160.0, 10.0, 190.0, 40.0), even_odd: false, paint: PdfPaint::Solid([0.0, 1.0, 0.0, 1.0]), blend: 0, glyph: None, source: None },
            PdfDrawItem::PopGroup,
            PdfDrawItem::Fill { path: vec![PdfEl::MoveTo([20.0, 50.0]), PdfEl::CubicTo([25.0, 70.0], [35.0, 70.0], [40.0, 50.0]), PdfEl::Close], even_odd: false, paint: PdfPaint::Solid([0.0, 0.0, 0.0, 1.0]), blend: 0, glyph: None, source: None },
        ],
        notes: vec!["pdf_text_as_outlines", "pdf_image_not_read"],
    };
    let page2 = PdfDrawingPage { media_box: [0.0, 0.0, 100.0, 50.0], crop_box: [0.0, 0.0, 100.0, 50.0], items: vec![PdfDrawItem::Fill { path: rect(5.0, 5.0, 15.0, 15.0), even_odd: false, paint: PdfPaint::Solid([0.0, 0.0, 0.0, 1.0]), blend: 2, glyph: None, source: None }], notes: Vec::new() };
    Drawn(vec![page1, page2])
}

fn import(bytes: &[u8], policy: &ImportPolicy) -> Result<VectorProposal> {
    import_ai_with(bytes, policy, &drawing(), &mut SequentialIdMint::new(7), &CancellationToken::default())
}

#[test]
fn pdf_compatible_ai_becomes_folio_layers_artboards_clips_and_losses() {
    let p = import(AI, &ImportPolicy::default()).unwrap();
    let doc = p.snapshot.document();
    // Two artboards, left to right, 20 pt apart.
    assert_eq!(doc.artboards.len(), 2);
    assert_eq!((doc.artboards[1].bounds.x.value, doc.artboards[1].bounds.width.value, doc.artboards[1].bounds.height.value), (220.0, 100.0, 50.0));
    // The red even-odd fill with the joined dashed stroke: one layer, two rows; y flipped from the top (100).
    let red = p.paths.iter().find(|v| v.anchors.first().is_some_and(|a| (a.position.x.value(), a.position.y.value()) == (10.0, 90.0))).expect("red path");
    assert_eq!(red.winding, hsk_studio_nib::Winding::EvenOdd);
    let style_of = |layer: &str| {
        let id = p.design.node_design(layer).and_then(|n| n.style_refs.get(LAYER_STYLE_REF)).expect("style").clone();
        p.design.paint_styles.iter().find(|s| s.style_id == id).expect("style record").clone()
    };
    let rows = style_of(&red.layer_id).rows;
    assert_eq!(rows.len(), 2);
    match &rows[1] {
        StudioAppearanceRow::Stroke { stroke, opacity, .. } => {
            assert_eq!((stroke.weight.value, stroke.dash.len(), *opacity), (4.0, 2, 50.0), "odd dash repeated, alpha as opacity");
        }
        other => panic!("{other:?}"),
    }
    // The shading: a linear gradient whose transform is flip . matrix.
    let gradient = p.design.paint_styles.iter().find_map(|s| match s.rows.first() {
        Some(StudioAppearanceRow::Fill { paint: StudioPaint::Gradient { gradient }, .. }) => Some(gradient.clone()),
        _ => None,
    }).expect("gradient");
    assert_eq!(gradient.transform, [50.0, 0.0, 0.0, -1.0, 100.0, 100.0]);
    // The clip: a group with a Folio vector clip whose hidden source holds the clip rectangle.
    assert_eq!(p.vector_records.clips.len(), 1);
    let source = doc.layers.iter().find(|l| l.layer_id == p.vector_records.clips[0].source_layer_id).unwrap();
    assert!(!source.visible && matches!(source.payload, Payload::Vector { .. }));
    assert_eq!(p.vector_records.attachments.len(), 1);
    // The transparency group: opacity 50 %, multiply (Folio 5); page 2's screen fill keeps blend 9.
    assert!(doc.layers.iter().any(|l| l.appearance.as_ref().is_some_and(|a| a.opacity == 50.0 && a.blend_mode == 5)));
    assert!(p.design.paint_styles.iter().any(|s| matches!(s.rows.first(), Some(StudioAppearanceRow::Fill { blend_mode: 9, .. }))));
    // Typed losses: native editability, private data, text as outlines, the reader's image note.
    let codes: Vec<&str> = p.loss.iter().map(|l| l.code).collect();
    for code in ["ai_native_editability_not_imported", "ai_private_data_not_imported", "ai_text_as_outlines", "pdf_image_not_read"] {
        assert!(codes.contains(&code), "{code} in {codes:?}");
    }
    // The proposal exports as SVG with its clip.
    let svg = export_proposal(&p, &ExportOptions::default()).unwrap();
    assert!(svg.contains("<clipPath") && svg.contains("linearGradient"));
    // Hostile and other inputs.
    assert!(matches!(import(b"%!PS-Adobe-3.0 EPSF", &ImportPolicy::default()), Err(VectorError::NativeEditabilityBlocked { .. })));
    assert!(matches!(import(b"<svg/>", &ImportPolicy::default()), Err(VectorError::Unsupported(_))));
    let tight = ImportPolicy { limits: Limits { max_nodes: 3, ..Limits::default() }, ..ImportPolicy::default() };
    assert!(matches!(import(AI, &tight), Err(VectorError::BudgetExceeded("nodes"))));
}

/// Keeps a run as text only when it reads "Hi" (the host's layout check stands in).
struct Layered(PdfDrawing);
impl PdfDrawingProvider for Layered {
    fn drawing(&self, _: &[u8], _: &CancellationToken) -> Result<PdfDrawing> {
        Ok(self.0.clone())
    }
    fn text_as_text(&self, run: &PdfTextRun<'_>) -> bool {
        run.text == "Hi" && (run.size_pt - 24.0).abs() < 1e-9 && run.glyphs.len() == 2 && run.glyphs[0] == (43, 0.0) && run.glyphs[1].0 == 76 && (run.glyphs[1].1 - 17.0).abs() < 1e-9
    }
}

fn glyph(id: u32, text: &str, x: f64, y: f64) -> PdfDrawItem {
    let g = PdfGlyph { font: 0, glyph_id: id, unicode: Some(text.into()), matrix: [0.024, 0.0, 0.0, 0.024, x, y] };
    PdfDrawItem::Fill { path: rect(x, y, x + 10.0, y + 15.0), even_odd: false, paint: PdfPaint::Solid([0.0, 0.0, 0.0, 1.0]), blend: 0, glyph: Some(g), source: None }
}

#[test]
fn ai_layers_document_colours_and_text_runs_are_kept() {
    let cmyk = PdfColour::Process { space: PdfSpace::Icc(0), components: vec![0.0, 1.0, 1.0, 0.0] };
    let lab = PdfColour::Process { space: PdfSpace::Lab { white: [0.9642, 1.0, 0.8249] }, components: vec![50.0, 20.0, -30.0] };
    let spot = PdfColour::Spot { name: "PANTONE 185 C".into(), tint: 0.5, alternate: Some((PdfSpace::Cmyk, vec![0.0, 0.91, 0.76, 0.0])) };
    let fill = |x: f64, c: PdfColour| PdfDrawItem::Fill { path: rect(x, 10.0, x + 20.0, 30.0), even_odd: false, paint: PdfPaint::Solid([1.0, 0.0, 0.0, 1.0]), blend: 0, glyph: None, source: Some(c) };
    let page = PdfDrawingPage {
        media_box: [0.0, 0.0, 200.0, 100.0],
        crop_box: [0.0, 0.0, 200.0, 100.0],
        items: vec![
            PdfDrawItem::BeginLayer { layer: 0 },
            fill(10.0, cmyk),
            fill(40.0, lab),
            glyph(43, "H", 10.0, 60.0),
            glyph(76, "i", 27.0, 60.0),
            glyph(49, "N", 100.0, 60.0),
            glyph(82, "o", 117.0, 60.0),
            PdfDrawItem::EndLayer,
            PdfDrawItem::BeginLayer { layer: 1 },
            fill(70.0, spot),
            PdfDrawItem::PushGroup { opacity: 1.0, blend: 0 },
            PdfDrawItem::BeginLayer { layer: 0 },
            PdfDrawItem::EndLayer,
            PdfDrawItem::PopGroup,
            PdfDrawItem::EndLayer,
        ],
        notes: Vec::new(),
    };
    let drawing = PdfDrawing {
        pages: vec![page],
        fonts: vec![PdfFont { postscript_name: Some("Inter-Regular".into()), bytes: Some(std::sync::Arc::from(&b"font program"[..])) }],
        layers: vec![PdfLayer { name: "Artwork".into(), visible: true }, PdfLayer { name: "Hidden".into(), visible: false }],
        profiles: vec![PdfIcc { components: 4, bytes: vec![7; 200], sha256: "ab".repeat(32) }],
        output_intent: Some(0),
    };
    let ai = import_ai_document(AI, &ImportPolicy::default(), &Layered(drawing), &mut SequentialIdMint::new(9), &CancellationToken::default()).unwrap();
    let p = &ai.proposal;
    let doc = p.snapshot.document();
    // The two layers are named groups under the artboard group, the second hidden; the nested
    // bracket inside a group is flattened.
    let artboard = &doc.layers[0].layer_id;
    let groups: Vec<(&str, bool)> = doc.layers.iter().filter(|l| matches!(&l.parent, hsk_studio_folio::Parent::Node(n) if &n.node_id == artboard) && l.kind == "group").map(|l| (l.name.as_str(), l.visible)).collect();
    assert_eq!(groups, [("Artwork", true), ("Hidden", false)]);
    assert!(p.loss.iter().any(|l| l.code == "ai_layer_nested_flattened"));
    // Document colours: the output intent is the CMYK working profile; ICC CMYK, Lab and the spot
    // ink are swatches bound to their rows (the spot at 50 %).
    let r = &p.vector_records;
    assert_eq!(r.icc_profiles.len(), 1);
    assert!(r.icc_profiles[0].working_space && r.icc_profiles[0].content_digest.digest == "ab".repeat(32));
    assert_eq!(ai.icc.len(), 1);
    let models: Vec<(Option<hsk_studio_folio::StudioColorModel>, Vec<f64>, bool)> = r.swatches.iter().map(|s| (s.color_model, s.components.clone(), s.profile_ref.is_some())).collect();
    assert_eq!(models[0], (Some(hsk_studio_folio::StudioColorModel::Cmyk), vec![0.0, 1.0, 1.0, 0.0], true));
    assert_eq!(models[1], (Some(hsk_studio_folio::StudioColorModel::Lab), vec![50.0, 20.0, -30.0], false));
    let ink = r.swatches[2].spot.as_ref().expect("spot ink");
    assert_eq!((ink.ink_name.as_str(), ink.alternate_color_value.clone()), ("PANTONE 185 C", vec![0.0, 0.91, 0.76, 0.0]));
    assert_eq!(r.swatch_paints.iter().map(|b| b.tint).collect::<Vec<_>>(), [None, None, Some(50.0)]);
    // "Hi" is a text layer (24 pt, the font named by its family); "No" stays outlines.
    assert_eq!(ai.text_layers.len(), 1);
    let t = &ai.text_layers[0];
    assert_eq!(t.origin_pt, [10.0, 40.0]);
    assert_eq!(ai.stories[0].inline_text(), Some("Hi"));
    let ch = ai.type_styles.styles.iter().find(|s| s.style_id == t.character_style).unwrap();
    assert!(ch.font_size.as_ref().is_some_and(|l| (l.value - 24.0).abs() < 1e-9) && ch.font_family.as_deref() == Some(ai.fonts[t.font].family.as_str()));
    assert!(doc.layers.iter().any(|l| l.layer_id == t.layer_id && l.kind == "text"));
    assert!(doc.layers.iter().any(|l| l.name == "text outline: No"));
    assert!(p.loss.iter().any(|l| l.code == "ai_text_as_outlines"));
    // The plain proposal path still works (no text records needed for outlines).
    assert!(import_ai_with(AI, &ImportPolicy::default(), &drawing_of_one(), &mut SequentialIdMint::new(3), &CancellationToken::default()).is_ok());
}

fn drawing_of_one() -> Drawn {
    Drawn(vec![PdfDrawingPage { media_box: [0.0, 0.0, 10.0, 10.0], crop_box: [0.0, 0.0, 10.0, 10.0], items: vec![glyph(1, "x", 1.0, 1.0)], notes: Vec::new() }])
}
