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
    fn pages(&self, _: &[u8], _: &CancellationToken) -> Result<Vec<PdfDrawingPage>> {
        Ok(self.0.clone())
    }
}

fn drawing() -> Drawn {
    let stroke = PdfStroke { width: 4.0, cap: 1, join: 2, miter_limit: 10.0, dash: vec![3.0], dash_offset: 0.0 };
    let page1 = PdfDrawingPage {
        media_box: [0.0, 0.0, 200.0, 100.0],
        crop_box: [0.0, 0.0, 200.0, 100.0],
        items: vec![
            PdfDrawItem::Fill { path: rect(10.0, 10.0, 60.0, 40.0), even_odd: true, paint: PdfPaint::Solid([1.0, 0.0, 0.0, 1.0]), blend: 0, glyph: false },
            PdfDrawItem::Stroke { path: rect(10.0, 10.0, 60.0, 40.0), stroke, paint: PdfPaint::Solid([0.0, 0.0, 1.0, 0.5]), blend: 0, glyph: false },
            PdfDrawItem::PushClip { path: rect(100.0, 10.0, 150.0, 40.0), even_odd: false },
            PdfDrawItem::Fill {
                path: rect(90.0, 0.0, 160.0, 50.0),
                even_odd: false,
                paint: PdfPaint::Gradient { axial: true, coords: [0.0, 0.0, 1.0, 0.0, 0.0, 0.0], matrix: [50.0, 0.0, 0.0, 1.0, 100.0, 0.0], stops: vec![(0.0, [1.0, 1.0, 0.0, 1.0]), (1.0, [0.0, 1.0, 1.0, 1.0])], extend: [true, true] },
                blend: 0,
                glyph: false,
            },
            PdfDrawItem::PopClip,
            PdfDrawItem::PushGroup { opacity: 0.5, blend: 1 },
            PdfDrawItem::Fill { path: rect(160.0, 10.0, 190.0, 40.0), even_odd: false, paint: PdfPaint::Solid([0.0, 1.0, 0.0, 1.0]), blend: 0, glyph: false },
            PdfDrawItem::PopGroup,
            PdfDrawItem::Fill { path: vec![PdfEl::MoveTo([20.0, 50.0]), PdfEl::CubicTo([25.0, 70.0], [35.0, 70.0], [40.0, 50.0]), PdfEl::Close], even_odd: false, paint: PdfPaint::Solid([0.0, 0.0, 0.0, 1.0]), blend: 0, glyph: true },
        ],
        notes: vec!["pdf_text_as_outlines", "pdf_image_not_read"],
    };
    let page2 = PdfDrawingPage { media_box: [0.0, 0.0, 100.0, 50.0], crop_box: [0.0, 0.0, 100.0, 50.0], items: vec![PdfDrawItem::Fill { path: rect(5.0, 5.0, 15.0, 15.0), even_odd: false, paint: PdfPaint::Solid([0.0, 0.0, 0.0, 1.0]), blend: 2, glyph: false }], notes: Vec::new() };
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
