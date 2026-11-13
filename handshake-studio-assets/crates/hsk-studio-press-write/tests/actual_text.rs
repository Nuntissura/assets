//! Lane SMB-TYPE-CHARACTER-ATTRIBUTES-1, review F2 + lead ruling: a glyph run inside an `/ActualText` span
//! (press-write `begin_actual_text` / `end_actual_text`) reads back with that text on every glyph of the span
//! (press-read `VectorGlyph::actual_text`), its /ToUnicode untouched; a PDF without ActualText reads exactly
//! as before.
use hsk_studio_accord::CancellationToken;
use hsk_studio_press_read::{Limits, VectorItem, read_vector_document};
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
