//! Lane SMB-ID-PREPRESS slice 6: data merge (STU-LAY-063, STU-LAY-163) and PDF place
//! (STU-LAY-028, STU-LAY-130). Byte offsets, boxes and coordinates are hand-computed. InDesign
//! parity UNVERIFIED (D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_interop_layout::press::{
    ExportOptions, Limits, PageSpec, PdfExport, VectorColour, VectorEl, VectorItem, VectorSpace, read_vector_document,
};
use hsk_studio_interop_layout::*;

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}

fn story(text: &str, paras: &[(u64, u64)]) -> StudioTextStory {
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", 1),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: paras.iter().map(|(s, e)| StoryParagraph { start: *s, end: *e, style_ref: None, paragraph_direction: None }).collect(),
        character_ranges: vec![StoryRange { start: 0, end: text.len() as u64, style_ref: None, applied_language: None }],
    }
}

const CSV: &str = "\u{FEFF}Name,@Photo,#Link,City\r\n\"Doe, Jane\",jane.jpg,https://x/1,Ghent\r\n\"Said \"\"hi\"\"\",,,\n\"multi\nline\",p.png,q,Paris";

#[test]
fn data_sources_parse_with_typed_errors() {
    let src = parse_delimited(CSV.as_bytes(), ',').unwrap();
    let kinds: Vec<(&str, FieldKind)> = src.fields.iter().map(|f| (f.name.as_str(), f.kind)).collect();
    assert_eq!(kinds, vec![("Name", FieldKind::Text), ("Photo", FieldKind::Image), ("Link", FieldKind::QrCode), ("City", FieldKind::Text)]);
    assert_eq!(src.records.len(), 3);
    assert_eq!(src.records[0], vec!["Doe, Jane", "jane.jpg", "https://x/1", "Ghent"]);
    assert_eq!(src.records[1], vec!["Said \"hi\"", "", "", ""]);
    assert_eq!(src.records[2][0], "multi\nline");
    // Tab-delimited, short rows padded.
    let tsv = parse_delimited(b"a\tb\n1\n", '\t').unwrap();
    assert_eq!(tsv.records, vec![vec!["1".to_string(), String::new()]]);
    // Malformed CSV and other refusals.
    let code = |r: Result<DataSource, MergeError>| r.unwrap_err().code();
    assert_eq!(code(parse_delimited(&[0xff, 0xfe, 0x00], ',')), "data_file_not_text");
    assert_eq!(code(parse_delimited(b"a,b\0\n1,2", ',')), "data_file_not_text");
    assert_eq!(code(parse_delimited(b"a,b\r\n", ',')), "data_source_empty");
    assert_eq!(code(parse_delimited(b"", ',')), "data_source_empty");
    assert_eq!(code(parse_delimited(b"a\n1,2", ',')), "data_row_too_wide");
    assert_eq!(code(parse_delimited(b"a\n\"open", ',')), "data_source_unsupported_format");
    let wide = format!("{}\n1", vec!["f"; MAX_MERGE_FIELDS + 1].join(","));
    assert_eq!(code(parse_delimited(wide.as_bytes(), ',')), "budget_exceeded");
    // JSON array of objects: fields in first-appearance order, null and missing -> empty.
    let j = parse_json_records(br#"[{"a":1,"b":"x"},{"b":null,"c":true}]"#).unwrap();
    assert_eq!(j.fields.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(), vec!["a", "b", "c"]);
    assert_eq!(j.records, vec![vec!["1", "x", ""], vec!["", "", "true"]]);
    assert_eq!(code(parse_json_records(b"{}")), "data_source_unsupported_format");
    assert_eq!(code(parse_json_records(b"[1]")), "data_source_unsupported_format");
    assert_eq!(code(parse_json_records(b"[]")), "data_source_empty");
}

#[test]
fn merge_records_into_story_placeholders_and_arrange() {
    let src = parse_delimited(CSV.as_bytes(), ',').unwrap();
    // "Dear <<Name>>,\n" 0..15, "<<City>>\n" 15..24, "End" 24..27.
    let text = "Dear <<Name>>,\n<<City>>\nEnd";
    let st = story(text, &[(0, 15), (15, 24), (24, 27)]);
    assert_eq!(find_placeholders(text), vec![Placeholder { start: 5, end: 13, field: "Name".into() }, Placeholder { start: 15, end: 23, field: "City".into() }]);
    let m0 = merge_story(&st, text, &src, 0, true).unwrap();
    assert_eq!(m0.text, "Dear Doe, Jane,\nGhent\nEnd");
    assert_eq!(m0.story.content, StoryContent::Inline { text_utf8: m0.text.clone() });
    // An empty field: with remove_blank_lines its paragraph goes, without it stays empty.
    let m1 = merge_story(&st, text, &src, 1, true).unwrap();
    assert_eq!(m1.text, "Dear Said \"hi\",\nEnd");
    assert_eq!(m1.story.paragraphs.iter().map(|p| (p.start, p.end)).collect::<Vec<_>>(), vec![(0, 16), (16, 19)]);
    assert_eq!(merge_story(&st, text, &src, 1, false).unwrap().text, "Dear Said \"hi\",\n\nEnd");
    // Image and QR placeholders are typed graphic fields, removed from the text.
    let g = "<<Photo>> by <<Link>>";
    let gm = merge_story(&story(g, &[(0, g.len() as u64)]), g, &src, 0, false).unwrap();
    assert_eq!(gm.text, " by ");
    assert_eq!(gm.graphic_fields, vec![("Photo".into(), FieldKind::Image, "jane.jpg".into()), ("Link".into(), FieldKind::QrCode, "https://x/1".into())]);
    // Captured errors.
    assert_eq!(merge_story(&story("Hello", &[(0, 5)]), "Hello", &src, 0, false).unwrap_err().code(), "missing_placeholders");
    assert_eq!(merge_story(&story("<<Nope>>", &[(0, 8)]), "<<Nope>>", &src, 0, false).unwrap_err().code(), "placeholder_field_missing");
    assert_eq!(merge_story(&st, text, &src, 3, false).unwrap_err().code(), "invalid_record_range");
    // Record selection.
    assert_eq!(select_records(&RecordSelection::All, 3).unwrap(), vec![0, 1, 2]);
    assert_eq!(select_records(&RecordSelection::One(2), 3).unwrap(), vec![1]);
    assert_eq!(select_records(&RecordSelection::Range("3, 1-2,2".into()), 3).unwrap(), vec![2, 0, 1]);
    for bad in ["3-1", "0", "4", "x", "1-", ""] {
        assert_eq!(select_records(&RecordSelection::Range(bad.into()), 3).unwrap_err().code(), "invalid_record_range", "{bad}");
    }
    // Multiple records per page: 180 x 180 margin box, 80 x 50 cells, 10 pt spacing -> 2 x 3 = 6.
    let (per, cells) = arrange_records(7, 200.0, 200.0, [10.0; 4], 80.0, 50.0, [10.0, 10.0], ArrangeBy::RowsFirst).unwrap();
    assert_eq!(per, 6);
    assert_eq!(cells[3], RecordCell { page: 0, x_pt: 100.0, y_pt: 70.0 });
    assert_eq!(cells[6], RecordCell { page: 1, x_pt: 10.0, y_pt: 10.0 });
    let (_, cols) = arrange_records(4, 200.0, 200.0, [10.0; 4], 80.0, 50.0, [10.0, 10.0], ArrangeBy::ColumnsFirst).unwrap();
    assert_eq!(cols[3], RecordCell { page: 0, x_pt: 100.0, y_pt: 10.0 });
    assert!(arrange_records(1, 200.0, 200.0, [10.0; 4], 190.0, 50.0, [0.0, 0.0], ArrangeBy::RowsFirst).is_err());
    assert!(arrange_records(1, 200.0, 200.0, [10.0; 4], 80.0, f64::NAN, [0.0, 0.0], ArrangeBy::RowsFirst).is_err());
}

fn assemble(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
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

/// 200 x 100 page with all five boxes; a CMYK rectangle on a visible layer, a black one on a hidden layer.
fn placed_source() -> Vec<u8> {
    let content = b"/OC /L1 BDC\n0 0 1 0 k 30 30 40 20 re f\nEMC\n/OC /L2 BDC\n0 g 150 10 20 20 re f\nEMC\n";
    let mut stream = format!("<< /Length {} >>\nstream\n", content.len()).into_bytes();
    stream.extend(content);
    stream.extend(b"\nendstream");
    assemble(&[
        b"<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [5 0 R 6 0 R] /D << /Order [5 0 R 6 0 R] /OFF [6 0 R] >> >> >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /CropBox [10 10 190 90] /TrimBox [20 20 180 80] /BleedBox [15 15 185 85] /ArtBox [0 0 300 300] /Resources << /Properties << /L1 5 0 R /L2 6 0 R >> >> /Contents 4 0 R >>".to_vec(),
        stream,
        b"<< /Type /OCG /Name (Shown) >>".to_vec(),
        b"<< /Type /OCG /Name (Hidden) >>".to_vec(),
    ])
}

#[test]
fn pdf_place_resolves_the_seven_crops() {
    let pdf = placed_source();
    let tok = CancellationToken::default();
    let rect = |c: PdfCrop| place_pdf_page(&pdf, 0, c, 1 << 20, &tok).unwrap().crop_rect;
    assert_eq!(rect(PdfCrop::Media), [0.0, 0.0, 200.0, 100.0]);
    assert_eq!(rect(PdfCrop::Crop), [10.0, 10.0, 190.0, 90.0]);
    assert_eq!(rect(PdfCrop::Trim), [20.0, 20.0, 180.0, 80.0]);
    assert_eq!(rect(PdfCrop::Bleed), [15.0, 15.0, 185.0, 85.0]);
    // The art box is clipped to the media box.
    assert_eq!(rect(PdfCrop::Art), [0.0, 0.0, 200.0, 100.0]);
    // Content bounds: visible layer only, and all layers.
    assert_eq!(rect(PdfCrop::ContentVisibleLayers), [30.0, 30.0, 70.0, 50.0]);
    assert_eq!(rect(PdfCrop::ContentAllLayers), [30.0, 10.0, 170.0, 50.0]);
    let p = place_pdf_page(&pdf, 0, PdfCrop::Trim, 1 << 20, &tok).unwrap();
    assert_eq!((p.content_w, p.content_h), (160.0, 60.0));
    assert_eq!(place_pdf_page(&pdf, 1, PdfCrop::Trim, 1 << 20, &tok).unwrap_err().code(), "pdf_page_missing");
    assert!(place_pdf_page(&pdf, 0, PdfCrop::Trim, 16, &tok).is_err());
    assert!(place_pdf_page(b"%PDF-1.7 garbage", 0, PdfCrop::Trim, 1 << 20, &tok).is_err());
}

#[test]
fn placed_pdf_draws_into_an_export_page() {
    let pdf = placed_source();
    let tok = CancellationToken::default();
    let placed = place_pdf_page(&pdf, 0, PdfCrop::Trim, 1 << 20, &tok).unwrap();
    let mut ex = PdfExport::new(ExportOptions::default(), &Limits::default()).unwrap();
    let page = ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 400.0, 200.0], trim_box_pt: None, bleed_box_pt: None }).unwrap();
    // Scale 2: the visible CMYK rectangle (30, 30)-(70, 50) lands at (60, 60)-(140, 100).
    let losses = draw_placed_pdf(&mut ex, page, &placed, [2.0, 0.0, 0.0, 2.0, 0.0, 0.0], None, &tok).unwrap();
    assert!(losses.is_empty(), "{losses:?}");
    let bytes = ex.finish(&tok).unwrap().0;
    let doc = read_vector_document(&bytes, 1 << 22, &tok).unwrap();
    let items = &doc.pages[0].items;
    let draws: Vec<(Vec<[f64; 2]>, Option<VectorColour>)> = items
        .iter()
        .filter_map(|i| match i {
            VectorItem::Draw { path, source, .. } => Some((
                path.iter().filter_map(|e| if let VectorEl::MoveTo(p) | VectorEl::LineTo(p) = e { Some(*p) } else { None }).collect(),
                source.clone(),
            )),
            _ => None,
        })
        .collect();
    // Only the visible layer is drawn, in its source CMYK.
    assert_eq!(draws.len(), 1, "{draws:?}");
    assert!(draws[0].0.contains(&[60.0, 60.0]) && draws[0].0.contains(&[140.0, 100.0]), "{:?}", draws[0].0);
    assert_eq!(draws[0].1, Some(VectorColour::Process { space: VectorSpace::Cmyk, components: vec![0.0, 0.0, 1.0, 0.0] }));
    // The trim box clips (scaled: 40, 40 .. 360, 160).
    assert!(items.iter().any(|i| matches!(i, VectorItem::PushClip { path, .. } if path.iter().any(|e| matches!(e, VectorEl::MoveTo(p) | VectorEl::LineTo(p) if *p == [40.0, 40.0])))));
    // A degenerate transform is refused.
    let mut ex = PdfExport::new(ExportOptions::default(), &Limits::default()).unwrap();
    let page = ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 400.0, 200.0], trim_box_pt: None, bleed_box_pt: None }).unwrap();
    assert_eq!(draw_placed_pdf(&mut ex, page, &placed, [0.0; 6], None, &tok).unwrap_err().code(), "invalid_transform");
}
