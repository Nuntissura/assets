//! Lane SMB-TYPE-CHARACTER-ATTRIBUTES-1, review F2 + lead ruling: a glyph run inside an `/ActualText` span
//! (press-write `begin_actual_text` / `end_actual_text`) reads back with that text on every glyph of the span
//! (press-read `VectorGlyph::actual_text`), its /ToUnicode untouched; a PDF without ActualText reads exactly
//! as before.
use hsk_studio_accord::CancellationToken;
use hsk_studio_press_read::{Limits, VectorItem, VectorNote, read_vector_document};
use hsk_studio_press_write::{Colour, ExportOptions, PageSpec, PdfExport};

/// Inter Regular (OFL). `H` = gid 161 (740/1000 em), `i` = gid 679 (237).
const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");

fn pdf(actual: Option<&str>) -> Vec<u8> {
    let mut ex = PdfExport::new(ExportOptions::default(), &Limits::default()).unwrap();
    let p = ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 200.0, 200.0], trim_box_pt: None, bleed_box_pt: None }).unwrap();
    let font = ex.embed_truetype(INTER, 0, &[(161, 740), (679, 237)], &[(161, "H"), (679, "i")]).unwrap();
    if let Some(t) = actual {
        ex.begin_actual_text(p, t).unwrap();
    }
    ex.show_glyph_run(p, font, 12.0, [10.0, 100.0], Colour::DeviceGray(0.0), &[(161, 0.0, 0.0), (679, 8.88, 0.0)]).unwrap();
    if actual.is_some() {
        ex.end_actual_text(p).unwrap();
    }
    ex.show_glyph_run(p, font, 12.0, [10.0, 50.0], Colour::DeviceGray(0.0), &[(161, 0.0, 0.0)]).unwrap();
    ex.finish(&CancellationToken::default()).unwrap().0
}

/// (unicode, actual text, x, y) of every drawn glyph.
fn read(bytes: &[u8]) -> Vec<(Option<String>, Option<String>, f64, f64)> {
    let doc = read_vector_document(bytes, 1 << 24, &CancellationToken::default()).unwrap();
    doc.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            VectorItem::Draw { glyph: Some(g), .. } => Some((g.unicode.clone(), g.actual_text.clone(), g.matrix[4], g.matrix[5])),
            _ => None,
        })
        .collect()
}

#[test]
fn glyphs_inside_an_actual_text_span_carry_its_text() {
    let g = read(&pdf(Some("Stra\u{df}e")));
    assert_eq!(g.iter().map(|x| (x.0.as_deref(), x.1.as_deref())).collect::<Vec<_>>(), [(Some("H"), Some("Stra\u{df}e")), (Some("i"), Some("Stra\u{df}e")), (Some("H"), None)]);
}

#[test]
fn a_pdf_without_actual_text_reads_as_before() {
    let plain = read(&pdf(None));
    assert_eq!(plain.iter().map(|x| (x.0.as_deref(), x.1.as_deref())).collect::<Vec<_>>(), [(Some("H"), None), (Some("i"), None), (Some("H"), None)]);
    // The span changes no position: the same glyphs at the same places.
    let spanned = read(&pdf(Some("x")));
    for (a, b) in plain.iter().zip(&spanned) {
        assert!((a.2 - b.2).abs() < 1e-9 && (a.3 - b.3).abs() < 1e-9, "{plain:?} {spanned:?}");
    }
}

/// press-read's bound on one `/ActualText` (`vector_read::MAX_ACTUAL_TEXT_CHARS`, crate-private).
const MAX_ACTUAL_TEXT_CHARS: usize = 4096;

/// A classic-xref one-page PDF (Helvetica as /F1) whose page content is `content`, uncompressed.
fn raw(content: &str) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_string(),
    ];
    let size = objects.len().saturating_add(1);
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{body}\nendobj\n", i.saturating_add(1)).into_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {size}\n0000000000 65535 f \n").into_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").into_bytes());
    }
    out.extend(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").into_bytes());
    out
}

#[test]
fn a_hostile_oversized_actual_text_is_cut_and_flagged_and_a_missing_one_reads_none() {
    // 5000 chars of UTF-16BE `A`, a span without /ActualText, and a non-string /ActualText.
    let hostile = format!("FEFF{}", "0041".repeat(5000));
    let content = format!("BT /F1 12 Tf 10 100 Td /Span <</ActualText <{hostile}>>> BDC (H) Tj EMC /Span <</Lang (en-US)>> BDC (i) Tj EMC /Span <</ActualText 42>> BDC (H) Tj EMC (i) Tj ET");
    let doc = read_vector_document(&raw(&content), 1 << 24, &CancellationToken::default()).unwrap();
    let texts: Vec<Option<String>> = doc.pages[0]
        .items
        .iter()
        .filter_map(|i| match i {
            VectorItem::Draw { glyph: Some(g), .. } => Some(g.actual_text.clone()),
            _ => None,
        })
        .collect();
    let cut: String = "A".repeat(MAX_ACTUAL_TEXT_CHARS) + "\u{fffd}";
    assert_eq!(texts, [Some(cut), None, None, None]);
    assert!(doc.pages[0].notes.contains(&VectorNote::ActualTextTruncated), "{:?}", doc.pages[0].notes);
    // Without an oversized value there is no truncation note.
    let plain = read_vector_document(&raw("BT /F1 12 Tf 10 100 Td /Span <</ActualText (Hi)>> BDC (H) Tj EMC ET"), 1 << 24, &CancellationToken::default()).unwrap();
    assert!(!plain.pages[0].notes.contains(&VectorNote::ActualTextTruncated));
}
