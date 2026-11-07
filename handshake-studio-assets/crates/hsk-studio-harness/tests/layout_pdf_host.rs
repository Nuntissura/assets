//! Lane SMB-LAYOUT-PDF-HOST-1 (feature `viewer`): the live Layout Editor document reaches PDF and
//! IDML from the host. Proof is read back independently of the export code: press's vector reader
//! gives the pages' media boxes and the glyphs' Unicode text, press's renderer the ink, the PDF/X
//! identification and output intent are read from the receipt and the self-check, the IDML package
//! by its zip magic and mimetype entry. The process test drives the same commands through the
//! `studio-harness` JSONL session with no Rust call (scenarios print-publication 92, 95, 97, 99,
//! 105; board-slides-web 55, 57; vector-brand 99, 104).
#![cfg(feature = "viewer")]
mod common;
use common::Scratch;
use hsk_studio_accord::CancellationToken;
use hsk_studio_harness::layout_ui::{LayoutEdit, TableEdit};
use hsk_studio_harness::view::Viewer;
use hsk_studio_harness::view::layout_export::{LayoutPdfSettings, PdfXStandard};
use hsk_studio_harness::view::render::Prefer;
use hsk_studio_harness_core::action;
use hsk_studio_harness_core::manual::DRIVE_VIEWER;
use hsk_studio_press::{VectorItem, read_vector_document};
use serde_json::{Value, json};

const CMYK_ICC: &[u8] = include_bytes!("../../hsk-studio-prism/tests/oracle/profiles/hsk-synth-cmyk-v2.icc");

/// A 3-page 1920 x 1080 pt deck with one frame of text on page 1.
fn deck(v: &mut Viewer, text: &str) {
    let opened = v.layout_open(Some((1920.0, 1080.0)), Some(3)).unwrap();
    assert_eq!(opened["pages"].as_array().unwrap().len(), 3, "{opened}");
    let ed = v.layout_editor().unwrap();
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 100.0, y: 100.0, w: 800.0, h: 200.0 }).unwrap();
    let frame = ed.selected.clone().unwrap();
    let story = ed.story_of_frame(&frame).unwrap().story_id.clone();
    ed.apply(LayoutEdit::ReplaceText { story, start: 0, end: 0, text: text.into() }).unwrap();
}

/// Unicode of every glyph drawn on each page, in order.
fn page_texts(pdf: &[u8]) -> Vec<String> {
    let doc = read_vector_document(pdf, 1 << 26, &CancellationToken::default()).unwrap();
    doc.pages
        .iter()
        .map(|p| {
            p.items
                .iter()
                .filter_map(|i| match i {
                    VectorItem::Draw { glyph: Some(g), .. } => g.unicode.clone(),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

#[test]
fn layout_pdf_export_lowers_the_live_story_with_type_glyphs_on_sized_pages() {
    let dir = Scratch::new("layout-pdf");
    let mut v = Viewer::new(Prefer::Cpu);
    deck(&mut v, "Hello deck");
    let out = dir.0.join("deck.pdf");
    let r = v.export_layout_pdf(&out, false, &LayoutPdfSettings::default()).unwrap();
    assert_eq!(r["pages"].as_array().unwrap().len(), 3, "{r}");
    assert_eq!(r["fonts"], json!(["Ubuntu-Light"]), "{r}");
    assert_eq!(r["receipt"]["fonts_embedded"], 1, "{r}");
    assert_eq!(r["receipt"]["conformance"], "not_proven");
    assert_eq!(r["job"]["outcome"], "completed", "{r}");
    assert_eq!(r["page_labels"], 1, "one arabic range from page 1: {r}");
    assert!(r["glyphs_drawn"].as_u64().unwrap() >= 9, "{r}");
    let pdf = std::fs::read(&out).unwrap();
    let doc = read_vector_document(&pdf, 1 << 26, &CancellationToken::default()).unwrap();
    assert_eq!(doc.pages.len(), 3);
    for p in &doc.pages {
        assert_eq!(p.media_box, [0.0, 0.0, 1920.0, 1080.0], "the deck left A4");
    }
    let texts = page_texts(&pdf);
    assert_eq!(texts[0].replace(' ', ""), "Hellodeck", "{texts:?}");
    assert!(texts[1].is_empty() && texts[2].is_empty());
    // Read back as pixels: ink inside the frame of page 1, page 2 blank.
    let png = dir.0.join("p1.png");
    let page = v.render_pdf_page(&out, 0, 0.5, &png, false).unwrap();
    assert_eq!((page["width"].as_u64(), page["height"].as_u64()), (Some(960), Some(540)), "{page}");
    assert!(page["non_white_pixels"].as_u64().unwrap() > 0, "{page}");
    assert_eq!(&std::fs::read(&png).unwrap()[..8], b"\x89PNG\r\n\x1a\n");
    let blank = v.render_pdf_page(&out, 1, 0.5, &dir.0.join("p2.png"), false).unwrap();
    assert_eq!(blank["non_white_pixels"], 0, "{blank}");
    // An existing destination needs overwrite; nothing is replaced unasked.
    let again = v.export_layout_pdf(&out, false, &LayoutPdfSettings::default()).unwrap_err();
    assert_eq!(again.code, "overwrite_required");
    assert!(v.export_layout_pdf(&out, true, &LayoutPdfSettings::default()).is_ok());
}

#[test]
fn preflight_runs_before_export_and_an_error_refuses_typed() {
    let dir = Scratch::new("layout-preflight");
    let mut v = Viewer::new(Prefer::Cpu);
    v.layout_open(Some((200.0, 200.0)), Some(1)).unwrap();
    let ed = v.layout_editor().unwrap();
    // One 15 pt line (12 pt, 1.25 em) fits the 20 pt frame; the rest is overset.
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 10.0, y: 10.0, w: 60.0, h: 20.0 }).unwrap();
    let frame = ed.selected.clone().unwrap();
    let story = ed.story_of_frame(&frame).unwrap().story_id.clone();
    ed.apply(LayoutEdit::ReplaceText { story, start: 0, end: 0, text: "far too much text for one short frame to hold\nand a second paragraph".into() }).unwrap();
    let report = v.layout_preflight().unwrap();
    assert_eq!(report["ok"], false, "{report}");
    assert!(report["findings"].as_array().unwrap().iter().any(|f| f["rule_id"] == "text.overset"), "{report}");
    let out = dir.0.join("overset.pdf");
    let refused = v.export_layout_pdf(&out, false, &LayoutPdfSettings::default()).unwrap_err();
    assert_eq!(refused.code, "preflight_errors", "{refused}");
    assert!(refused.detail.contains("text.overset"), "{refused}");
    assert!(!out.exists(), "a refused export writes nothing");
    let r = v.export_layout_pdf(&out, false, &LayoutPdfSettings { ignore_preflight: true, ..LayoutPdfSettings::default() }).unwrap();
    assert_eq!(r["preflight"]["ok"], false);
    assert!(!r["overset_stories"].as_array().unwrap().is_empty(), "{r}");
}

/// The editor keeps a text inserted into an empty story as one paragraph: the terminator inside it
/// is set as a line break (both sides drawn, on two lines), and the lost paragraph boundary is
/// reported.
#[test]
fn a_break_inside_one_paragraph_is_set_as_a_line_break_with_both_sides_drawn() {
    let dir = Scratch::new("layout-inner-break");
    let mut v = Viewer::new(Prefer::Cpu);
    v.layout_open(Some((400.0, 400.0)), Some(1)).unwrap();
    let ed = v.layout_editor().unwrap();
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 20.0, y: 20.0, w: 300.0, h: 300.0 }).unwrap();
    let frame = ed.selected.clone().unwrap();
    let story = ed.story_of_frame(&frame).unwrap().story_id.clone();
    ed.apply(LayoutEdit::ReplaceText { story, start: 0, end: 0, text: "Above\nBelow".into() }).unwrap();
    let out = dir.0.join("break.pdf");
    let r = v.export_layout_pdf(&out, false, &LayoutPdfSettings::default()).unwrap();
    let lost: Vec<&Value> = r["host_loss"].as_array().unwrap().iter().filter(|l| l["code"] == "paragraph_break_inside_paragraph_set_as_line_break").collect();
    assert_eq!(lost.len(), 1, "{r}");
    assert_eq!(lost[0]["count"], 1, "{r}");
    assert_eq!(r["lines"], 2, "the break ends the first line: {r}");
    assert_eq!(page_texts(&std::fs::read(&out).unwrap())[0], "AboveBelow", "both sides are drawn");
}

#[test]
fn pdfx4_with_a_cmyk_output_intent_is_identified_and_self_checked() {
    let dir = Scratch::new("layout-pdfx");
    let mut v = Viewer::new(Prefer::Cpu);
    deck(&mut v, "Press");
    let settings = LayoutPdfSettings {
        pdfx: PdfXStandard::X4_2010,
        created: Some("D:20261107000000Z"),
        output_profile: Some(CMYK_ICC),
        output_condition_identifier: Some("synthetic test"),
        ..LayoutPdfSettings::default()
    };
    let r = v.export_layout_pdf(&dir.0.join("press.pdf"), false, &settings).unwrap();
    assert_eq!(r["pdfx"]["standard"], "pdfx.x4_2010", "{r}");
    assert_eq!(r["receipt"]["effective_output_condition"], "synthetic test", "{r}");
    assert!(r["receipt"]["effective_pdfx_profile"].is_string(), "{r}");
    assert!(!r["pdfx"]["self_check"]["rules"].as_array().unwrap().is_empty(), "{r}");
    assert_eq!(r["pdfx"]["self_check"]["conformance"], "not_proven", "a self-check is not certification: {r}");
    assert_eq!(r["pdfx"]["self_check"]["all_passed"], true, "a written X-4 file passed every self-check rule: {r}");
    // Unstyled text is painted in the intent's CMYK, not device RGB. NOT_PROVEN by readback: press's
    // vector reader keeps no source colour for glyph draws, so this is the export's own statement.
    assert_eq!(r["default_fill"], "device_cmyk", "{r}");
    println!("NOT_PROVEN: the glyph fill's colour space read back from the PDF (press reader keeps no source colour for glyphs)");
    // X-4 without its date, a profile that is not ICC: typed, before anything is written.
    let no_date = LayoutPdfSettings { created: None, ..settings };
    assert_eq!(v.export_layout_pdf(&dir.0.join("a.pdf"), false, &no_date).unwrap_err().code, "pdfx_created_required");
    let junk = LayoutPdfSettings { output_profile: Some(b"not a profile"), ..settings };
    assert_eq!(v.export_layout_pdf(&dir.0.join("b.pdf"), false, &junk).unwrap_err().code, "output_profile_invalid");
    assert!(!dir.0.join("a.pdf").exists() && !dir.0.join("b.pdf").exists());
}

#[test]
fn idml_export_writes_the_live_layout_as_a_package() {
    let dir = Scratch::new("layout-idml");
    let mut v = Viewer::new(Prefer::Cpu);
    deck(&mut v, "Exchange");
    let out = dir.0.join("deck.idml");
    let r = v.export_layout_idml(&out, false).unwrap();
    let bytes = std::fs::read(&out).unwrap();
    assert_eq!(&bytes[..2], b"PK", "{r}");
    let mimetype: &[u8] = b"application/vnd.adobe.indesign-idml-package";
    assert!(bytes.windows(mimetype.len()).any(|w| w == mimetype), "mimetype entry");
    assert_eq!(r["pages"].as_array().unwrap().len(), 3, "{r}");
    assert_eq!(r["job"]["outcome"], "completed");
    assert_eq!(v.export_layout_idml(&out, false).unwrap_err().code, "overwrite_required");
    // A filled table cell: its cell story is the table record's, not a host story (was dangling_story).
    let ed = v.layout_editor().unwrap();
    let host = ed.doc().stories[0].story_id.clone();
    ed.apply(LayoutEdit::Table(TableEdit::InsertTable { story: host, paragraph: 0, rows: 2, columns: 2, header_rows: 1 })).unwrap();
    let table = ed.records().longdoc.tables[0].table_id.clone();
    ed.apply(LayoutEdit::Table(TableEdit::SetCellText { table, row: 1, column: 1, text: "cell".into() })).unwrap();
    let with_table = v.export_layout_idml(&dir.0.join("table.idml"), false).unwrap();
    assert_eq!(with_table["note_stories"], 1, "{with_table}");
}

#[test]
fn hostile_sizes_paths_and_sources_refuse_typed() {
    let dir = Scratch::new("layout-hostile");
    let mut v = Viewer::new(Prefer::Cpu);
    assert_eq!(v.export_layout_pdf(&dir.0.join("x.pdf"), false, &LayoutPdfSettings::default()).unwrap_err().code, "layout_not_open");
    assert_eq!(v.layout_preflight().unwrap_err().code, "layout_not_open");
    for (size, pages, code) in [
        (Some((f64::NAN, 10.0)), None, "page_size_out_of_range"),
        (Some((0.5, 10.0)), None, "page_size_out_of_range"),
        (Some((10.0, 1e9)), None, "page_size_out_of_range"),
        (Some((f64::INFINITY, 10.0)), None, "page_size_out_of_range"),
        (None, Some(0), "page_count_out_of_range"),
        (None, Some(u32::MAX), "page_count_out_of_range"),
    ] {
        assert_eq!(v.layout_open(size, pages).unwrap_err().code, code, "{size:?} {pages:?}");
    }
    // Without a size the open document's pages (here none: the blank A4 layout) are edited.
    let opened = v.layout_open(None, None).unwrap();
    assert_eq!(opened["pages"][0]["width_pt"], 595.0, "{opened}");
    // A sized layout never drops edits.
    let ed = v.layout_editor().unwrap();
    ed.apply(LayoutEdit::DrawFrame { page_id: "page-1".into(), x: 10.0, y: 10.0, w: 60.0, h: 14.0 }).unwrap();
    assert_eq!(v.layout_open(Some((100.0, 100.0)), None).unwrap_err().code, "layout_unsaved_edits");
    // Render: a missing source, garbage bytes, a scale out of range, a page past the end.
    let missing = dir.0.join("none.pdf");
    assert_eq!(v.render_pdf_page(&missing, 0, 1.0, &dir.0.join("m.png"), false).unwrap_err().code, "source_unreadable");
    let junk = dir.0.join("junk.pdf");
    std::fs::write(&junk, b"%PDF-1.7\nnot really").unwrap();
    assert!(v.render_pdf_page(&junk, 0, 1.0, &dir.0.join("j.png"), false).is_err());
    v.layout_editor().unwrap().undo();
    let pdf = dir.0.join("one.pdf");
    v.export_layout_pdf(&pdf, false, &LayoutPdfSettings::default()).unwrap();
    for (page, scale) in [(0usize, f64::NAN), (0, 0.0), (0, 1e6), (9, 1.0)] {
        let e = v.render_pdf_page(&pdf, page, scale, &dir.0.join(format!("r{page}.png")), false).unwrap_err();
        assert!(!e.code.is_empty(), "{e}");
    }
    assert!(!dir.0.join("r9.png").exists());
}

/// The catalog rows run their viewer rows: same keys, every viewer.<method> is a drive row.
#[test]
fn layout_catalog_rows_name_their_drive_rows_with_the_same_keys() {
    for id in [action::CMD_LAYOUT_OPEN, action::CMD_LAYOUT_PREFLIGHT, action::CMD_LAYOUT_EXPORT_PDF, action::CMD_LAYOUT_EXPORT_IDML, action::CMD_LAYOUT_RENDER_PDF_PAGE] {
        let entry = action::command(id).unwrap_or_else(|| panic!("{id} missing"));
        assert!(!entry.recordable && !entry.mutates_authority, "{id}");
        let method = entry.run_method.strip_prefix("viewer.").unwrap();
        let row = DRIVE_VIEWER.iter().find(|r| r.method == method).unwrap_or_else(|| panic!("{method} not a drive row"));
        let mut a: Vec<&str> = entry.parameters.iter().map(|p| p.key).collect();
        let mut b: Vec<&str> = row.params.iter().map(|p| p.key).collect();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b, "{id}");
    }
}

// ---- the process: no Rust call ------------------------------------------------------------------

#[cfg(not(feature = "desktop"))]
fn binary() -> std::path::PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().and_then(std::path::Path::parent).expect("target directory").to_path_buf();
    let example = dir.join("examples").join(format!("studio-harness{}", std::env::consts::EXE_SUFFIX));
    assert!(example.is_file(), "the studio-harness example was not built at {}", example.display());
    example
}
#[cfg(feature = "desktop")]
fn binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_studio-harness"))
}

/// One quiet `command PACKAGE --stdin` session; one JSON value per stdout line.
fn session(dir: &std::path::Path, package: &std::path::Path, input: &[Value]) -> (i32, Vec<Value>) {
    use std::io::Write as _;
    let mut command = std::process::Command::new(binary());
    command
        .args(["command", package.to_str().unwrap(), "--actor", "layout-model", "--session", "s-1", "--stdin"])
        .current_dir(dir)
        .env("STUDIO_DIAG_DIR", dir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let mut child = command.spawn().expect("spawn studio-harness");
    let text: String = input.iter().map(|l| format!("{l}\n")).collect();
    child.stdin.take().unwrap().write_all(text.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let lines = String::from_utf8(out.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    (out.status.code().expect("an exit code"), lines)
}

#[test]
fn a_model_exports_a_sized_layout_through_the_process() {
    let dir = Scratch::new("layout-process");
    let package = dir.0.join("demo.handshake");
    std::fs::write(&package, include_bytes!("fixtures/demo.handshake")).unwrap();
    let (pdf, png, idml) = (dir.0.join("slides.pdf"), dir.0.join("slide1.png"), dir.0.join("slides.idml"));
    let call = |id: &str, parameters: Value| json!({"command":"command","command_id":id,"parameters":parameters});
    let input = [
        call("layout.open", json!({"page_width_pt": 841.89, "page_height_pt": 595.28, "pages": 12})),
        call("viewer.layout_preflight", json!({})),
        call("layout.export_pdf", json!({"path": pdf.to_str().unwrap(), "title": "guidelines"})),
        call("layout.render_pdf_page", json!({"source": pdf.to_str().unwrap(), "page": 11, "px_per_pt": 0.25, "path": png.to_str().unwrap()})),
        call("viewer.export_layout_idml", json!({"path": idml.to_str().unwrap()})),
        call("layout.export_pdf", json!({"path": pdf.to_str().unwrap()})),
        call("layout.export_pdf", json!({"path": package.to_str().unwrap(), "overwrite": true})),
        call("layout.open", json!({"page_width_pt": 100})),
    ];
    let (exit, r) = session(&dir.0, &package, &input);
    assert_eq!(r.len(), input.len() + 2, "open, one reply per line, end: {r:?}");
    assert_eq!(r[1]["ok"], true, "{}", r[1]);
    assert_eq!(r[1]["detail"]["result"]["pages"].as_array().unwrap().len(), 12);
    assert_eq!(r[2]["detail"]["result"]["warnings"], 12, "every page is blank: {}", r[2]);
    let export = &r[3]["detail"]["result"];
    assert_eq!(r[3]["ok"], true, "{}", r[3]);
    assert_eq!(export["pages"].as_array().unwrap().len(), 12, "{export}");
    assert_eq!(export["page_sizes"][0]["width_pt"], 841.89, "A4 landscape, not portrait");
    assert!((210..=211).contains(&r[4]["detail"]["result"]["width"].as_u64().unwrap_or(0)), "{}", r[4]);
    assert!(png.is_file() && idml.is_file() && pdf.is_file());
    assert_eq!(r[6]["error"], "overwrite_required", "{}", r[6]);
    assert_eq!(r[7]["error"], "export_over_source", "{}", r[7]);
    assert_eq!(r[8]["error"], "page_size_incomplete", "{}", r[8]);
    assert_eq!(exit, 1, "refused lines make the exit 1");
    let reread = read_vector_document(&std::fs::read(&pdf).unwrap(), 1 << 26, &CancellationToken::default()).unwrap();
    assert_eq!(reread.pages.len(), 12);
    assert!((reread.pages[0].media_box[2] - 841.89).abs() < 0.01 && (reread.pages[0].media_box[3] - 595.28).abs() < 0.01, "{:?}", reread.pages[0].media_box);
}
