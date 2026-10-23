//! Operator decision D27 ("Flatten + narrow"): every page carrying a redaction region is replaced by a
//! raster of itself with the regions painted opaque. Every assertion reads the SAVED bytes; the page
//! structure and pixels are read with hayro-syntax (not this crate's parser), needles with a raw scan
//! plus an independent zlib inflate of every stream body.
//!
//! Red proofs: lines marked `D27-new-api` use API added with the flattening; the red copy of b8ce934
//! drops them (the remaining assertions fail there because b8ce934 never flattens a page).
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::hayro_syntax::object::{Dict as HDict, Stream as HStream};
use hsk_studio_accord::CancellationToken;
use hsk_studio_press::{DESCRIPTOR, Limits, PdfDoc, PressError, RedactionRegion, UnsupportedPolicy, redact_and_save, verify_payload_absent};
use std::collections::BTreeMap;

fn stream_body(dict_extra: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict_extra} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream");
    v
}

fn build(objs: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut off: BTreeMap<u32, usize> = BTreeMap::new();
    for (n, body) in objs {
        off.insert(*n, out.len());
        out.extend(format!("{n} 0 obj\n").into_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let size = off.keys().max().unwrap() + 1;
    let xref = out.len();
    out.extend(format!("xref\n0 {size}\n0000000000 65535 f \n").into_bytes());
    for n in 1..size {
        match off.get(&n) {
            Some(o) => out.extend(format!("{o:010} 00000 n \n").into_bytes()),
            None => out.extend_from_slice(b"0000000000 00001 f \n"),
        }
    }
    out.extend(format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").into_bytes());
    out
}

const HELVETICA: &[u8] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>";

/// One page (object 3, 612 x 792) with `resources`, `content` (object 4) and `extra` objects; font 6 is
/// Helvetica.
fn page_doc(resources: &str, content: &[u8], extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << {resources} >> >>").into_bytes()),
        (4, stream_body("", content)),
        (6, HELVETICA.to_vec()),
    ];
    objs.extend(extra);
    build(&objs)
}

fn token() -> CancellationToken {
    CancellationToken::default()
}

/// Default limits at a 36 dpi raster: the structural checks need no detail, and a debug build draws and
/// proves a 150 dpi page in about two seconds.
#[allow(clippy::field_reassign_with_default)]
fn quick() -> Limits {
    #[allow(unused_mut)]
    let mut l = Limits::default();
    l.flatten_dpi = 36; // D27-new-api
    l
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Every stream body of `bytes`, zlib-inflated when it inflates (raw otherwise), found by markers.
fn independent_streams(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(p) = find(&bytes[i..], b"\nstream\n") {
        let s = i + p + 8;
        let Some(e) = find(&bytes[s..], b"\nendstream") else { break };
        let raw = &bytes[s..s + e];
        out.push(miniz_oxide::inflate::decompress_to_vec_zlib(raw).unwrap_or_else(|_| raw.to_vec()));
        i = s + e;
    }
    out
}

/// No spelling of `needle` (ASCII bytes) in the saved file: raw bytes, inflated streams, and the
/// crate's own payload proof.
fn needle_gone(bytes: &[u8], needle: &str) -> bool {
    find(bytes, needle.as_bytes()).is_none()
        && independent_streams(bytes).iter().all(|s| find(s, needle.as_bytes()).is_none())
        && verify_payload_absent(bytes, &[needle.as_bytes().to_vec()], &Limits::default(), &token()).is_ok_and(|p| p.hits.is_empty())
}

/// What hayro-syntax reads on page `index` of `bytes`: its content operators, its dictionary keys and the
/// `/HskFl0` raster (width, height, RGB pixels) when the page has one.
struct PageFacts {
    ops: Vec<String>,
    keys: Vec<String>,
    raster: Option<(u32, u32, Vec<u8>)>,
    media: [f64; 4],
}

fn page_facts(bytes: &[u8], index: usize) -> PageFacts {
    let pdf = Pdf::new(bytes.to_vec()).expect("hayro-syntax opens the saved file");
    let page = &pdf.pages()[index];
    let mut ops = Vec::new();
    let mut it = page.operations();
    while let Some(i) = it.next() {
        ops.push(String::from_utf8_lossy(i.operator).into_owned());
    }
    let raw = page.raw();
    let keys = raw.keys().map(|k| String::from_utf8_lossy(k.as_ref()).into_owned()).collect();
    let raster = raw
        .get::<HDict<'_>>(b"Resources")
        .and_then(|r| r.get::<HDict<'_>>(b"XObject"))
        .and_then(|x| x.get::<HStream<'_>>(b"HskFl0"))
        .and_then(|s| {
            let w = s.dict().get::<u32>(b"Width")?;
            let h = s.dict().get::<u32>(b"Height")?;
            Some((w, h, s.decoded().ok()?.to_vec()))
        });
    let m = page.media_box();
    PageFacts { ops, keys, raster, media: [m.x0, m.y0, m.x1, m.y1] }
}

fn pixel(f: &PageFacts, x: f64, y: f64) -> Option<[u8; 3]> {
    let (w, h, px) = f.raster.as_ref()?;
    let sx = f64::from(*w) / (f.media[2] - f.media[0]);
    let sy = f64::from(*h) / (f.media[3] - f.media[1]);
    let (cx, cy) = (((x - f.media[0]) * sx) as usize, ((f.media[3] - y) * sy) as usize);
    let i = (cy * *w as usize + cx) * 3;
    px.get(i..i + 3).map(|s| [s[0], s[1], s[2]])
}

/// Every raster pixel whose centre lies inside `rect` (user space) is black.
fn region_black(f: &PageFacts, rect: [f64; 4]) -> bool {
    let Some((w, h, px)) = &f.raster else { return false };
    let sx = f64::from(*w) / (f.media[2] - f.media[0]);
    let sy = f64::from(*h) / (f.media[3] - f.media[1]);
    let mut seen = 0usize;
    for cy in 0..*h as usize {
        for cx in 0..*w as usize {
            let (ux, uy) = (f.media[0] + (cx as f64 + 0.5) / sx, f.media[3] - (cy as f64 + 0.5) / sy);
            if ux > rect[0] && ux < rect[2] && uy > rect[1] && uy < rect[3] {
                seen += 1;
                let i = (cy * *w as usize + cx) * 3;
                if px[i..i + 3] != [0, 0, 0] {
                    return false;
                }
            }
        }
    }
    seen > 0
}

/// The saved page holds only the raster: `q cm Do Q`, no text operator, no annotations (R13-F1: an
/// explicit empty `/Annots`), nothing else.
fn raster_only(f: &PageFacts) -> bool {
    let allowed = ["Type", "Parent", "MediaBox", "CropBox", "Rotate", "UserUnit", "Resources", "Contents", "Annots"];
    f.ops == ["q", "cm", "Do", "Q"] && f.keys.iter().all(|k| allowed.contains(&k.as_str())) && f.raster.is_some()
}

fn region(rect: [f64; 4]) -> RedactionRegion {
    RedactionRegion { page_index: 0, rect }
}

const R: [f64; 4] = [90.0, 690.0, 200.0, 715.0];

/// R8-R11 pre-review cases whose redacted page a viewer drew differently from the crate's reading:
/// after D27 the saved page is the raster alone, no byte of the needle survives anywhere, and the
/// region's pixels are opaque. Sub-cases are named in the red run.
#[test]
fn prereview_region_pages_are_flattened() {
    let f2_data: &[u8] = b"BT\n/F1 12 Tf\n100 100 Td\n(SECRET-ALPHA) Tj\nET\n";
    assert_eq!(f2_data.len(), 45);
    let mut f2 = b"BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET BI /W 45 /H 1 /BPC 8 /CS /G /Subtype /Form /BBox [0 0 612 792] ID ".to_vec();
    f2.extend_from_slice(f2_data);
    f2.extend_from_slice(b" EI");
    let f5_data: &[u8] = b"x EI BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET";
    let mut f5 = format!("BI /W {} /H 1 /BPC 8 /CS /G ID ", f5_data.len()).into_bytes();
    f5.extend_from_slice(f5_data);
    f5.extend_from_slice(b" EI");
    let smask_res = "/Font << /F1 6 0 R >> /ExtGState << /GS1 << /SMask << /S /Luminosity /G 30 0 R >> >> >>";
    let diff_font = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding << /Type /Encoding /Differences [1 /S /E /C /R /T /hyphen /A /L /P /H] >> >>".to_vec();
    let type3 = b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [1 0 0 1 0 0] /CharProcs << /A 41 0 R >> /Encoding << /Type /Encoding /Differences [65 /A] >> /FirstChar 65 /LastChar 65 /Widths [0.6] /Resources << >> >>".to_vec();
    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, Vec<u8>, [f64; 4], UnsupportedPolicy, Vec<&str>)> = vec![
        (
            "R11-F1 (a) ExtGState /SMask /G form drawing the needle in the region",
            page_doc(
                smask_res,
                b"q 1 0 0 1 0 0 cm /GS1 gs Q BT /F1 12 Tf 72 300 Td (Public) Tj ET",
                vec![(30, stream_body("/Subtype /Form /BBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >>", b"BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET"))],
            ),
            R,
            UnsupportedPolicy::Refuse,
            vec!["SECRET-ALPHA"],
        ),
        (
            "R11-F1 (b) /SMask /G stream without /Subtype or /Resources, /Differences codes glued to ET",
            page_doc(
                "/Font << /F1 6 0 R /F2 31 0 R >> /ExtGState << /GS1 << /SMask << /S /Luminosity /G 30 0 R >> >> >>",
                b"q /GS1 gs Q BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET",
                vec![(30, stream_body("", b"BT /F2 24 Tf 100 100 Td <010203040205060708090A07>TjET")), (31, diff_font.clone())],
            ),
            R,
            UnsupportedPolicy::Refuse,
            vec!["SECRET-ALPHA", "010203040205060708090A07"],
        ),
        ("R11-F2 inline image whose dictionary says /Subtype /Form", page_doc("/Font << /F1 6 0 R >>", &f2, vec![]), R, UnsupportedPolicy::Refuse, vec!["SECRET-ALPHA"]),
        (
            "R11-F3 Courier without /FontDescriptor (viewer width 600, crate 0)",
            page_doc(
                "/Font << /F1 32 0 R >>",
                b"BT /F1 10 Tf 100 700 Td (SECRET-ALPHA) Tj ET",
                vec![(32, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /FirstChar 65 /LastChar 65 /Widths [600] >>".to_vec())],
            ),
            [130.0, 690.0, 180.0, 715.0],
            UnsupportedPolicy::Refuse,
            vec!["SECRET-ALPHA", "T-ALPHA"],
        ),
        // R11-F5 (ReportAndContinue: under Refuse the selector refuses, see below).
        ("R11-F5 inline data a viewer may end at its first EI", page_doc("/Font << /F1 6 0 R >>", &f5, vec![]), R, UnsupportedPolicy::ReportAndContinue, vec!["SECRET-ALPHA"]),
        (
            "R10-F3 form XObject whose /Subtype is an indirect reference",
            page_doc(
                "/Font << /F1 6 0 R >> /XObject << /Fm1 33 0 R >>",
                b"q /Fm1 Do Q",
                vec![
                    (33, stream_body("/Type /XObject /Subtype 34 0 R /BBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >>", b"BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET")),
                    (34, b"/Form".to_vec()),
                ],
            ),
            R,
            UnsupportedPolicy::Refuse,
            vec!["SECRET-ALPHA"],
        ),
        (
            "R9-F2 (a) font selected by an ExtGState /Font",
            page_doc("/Font << /F1 6 0 R >> /ExtGState << /GS1 << /Font [6 0 R 12] >> >>", b"BT /GS1 gs 100 700 Td (SECRET-ALPHA) Tj ET", vec![]),
            [120.0, 690.0, 180.0, 715.0],
            UnsupportedPolicy::Refuse,
            vec!["SECRET-ALPHA"],
        ),
        (
            "R8-F4 Type 3 text with a non-0.001 /FontMatrix (ReportAndContinue)",
            page_doc(
                "/Font << /F1 6 0 R /T3 40 0 R >>",
                b"BT /T3 10 Tf 100 700 Td (AAAAAAAAAA) Tj (SECRET) Tj ET",
                vec![(40, type3), (41, stream_body("", b"0.6 0 d0 0 0 0.5 0.7 re f"))],
            ),
            [150.0, 690.0, 200.0, 715.0],
            UnsupportedPolicy::ReportAndContinue,
            vec!["(SECRET)"],
        ),
    ];
    let mut red: Vec<String> = Vec::new();
    for (what, pdf, rect, policy, needles) in &cases {
        let mut doc = PdfDoc::open(pdf, &quick(), &token()).expect("open");
        let out = match redact_and_save(&mut doc, &[region(*rect)], *policy, &token()) {
            Ok(o) => o,
            Err(e) => {
                red.push(format!("{what}: refused {e}"));
                continue;
            }
        };
        let f = page_facts(&out.bytes, 0);
        if !raster_only(&f) {
            red.push(format!("{what}: page is not the raster alone: ops {:?} keys {:?}", f.ops, f.keys));
        }
        for n in needles {
            if !needle_gone(&out.bytes, n) {
                red.push(format!("{what}: needle {n} survives in the saved file"));
            }
        }
        if !region_black(&f, *rect) {
            red.push(format!("{what}: region pixels not opaque"));
        }
        if out.residual_in_region != 0 {
            red.push(format!("{what}: residual_in_region {}", out.residual_in_region));
        }
        if out.flattened.len() != 1 { // D27-new-api
            red.push(format!("{what}: flattened {:?}", out.flattened)); // D27-new-api
        } // D27-new-api
        // The words a viewer may draw from ambiguous inline data are unknown to the selector: the
        // region is flattened, the result is not proven (R11-F5).
        if what.starts_with("R11-F5") && out.is_proven() {
            red.push(format!("{what}: proven although the region's text is not known"));
        }
    }
    // R11-F5 under Refuse: typed refusal (fail closed), the document unchanged.
    let f5_doc = page_doc("/Font << /F1 6 0 R >>", &f5, vec![]);
    let mut doc = PdfDoc::open(&f5_doc, &quick(), &token()).expect("open");
    if !matches!(redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()), Err(PressError::UnsupportedRedactionClass(_))) {
        red.push("R11-F5 under Refuse: not refused".to_string());
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// Two pages: the region on page one, `page_two` (with `resources` and `extra` objects) on page two.
fn two_pages(page_two: &[u8], resources: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> >> >>".to_vec()),
        (4, stream_body("", b"BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET")),
        (5, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R /Resources << {resources} >> >>").into_bytes()),
        (6, HELVETICA.to_vec()),
        (7, stream_body("", page_two)),
    ];
    objs.extend(extra);
    build(&objs)
}

/// R11-F2 and R11-F5 on a page WITHOUT a region (canonical rewrite, not flattening): an inline image
/// whose dictionary names another /Subtype, or whose data a viewer may end at an earlier EI, is never
/// moved into an XObject; its stream stays as it was and the result is not proven.
#[test]
fn inline_image_keys_never_change_the_xobject() {
    let f2_data: &[u8] = b"BT\n/F1 12 Tf\n100 100 Td\n(SECRET-ALPHA) Tj\nET\n";
    let mut f2 = b"BI /W 45 /H 1 /BPC 8 /CS /G /Subtype /Form /BBox [0 0 612 792] ID ".to_vec();
    f2.extend_from_slice(f2_data);
    f2.extend_from_slice(b" EI");
    let f5_data: &[u8] = b"x EI BT /F1 12 Tf 100 100 Td (SECRET-ALPHA) Tj ET";
    let mut f5 = format!("BI /W {} /H 1 /BPC 8 /CS /G ID ", f5_data.len()).into_bytes();
    f5.extend_from_slice(f5_data);
    f5.extend_from_slice(b" EI");
    let mut red = Vec::new();
    for (what, content) in [("R11-F2 /Subtype /Form in an inline image", f2), ("R11-F5 ambiguous inline data", f5)] {
        let pdf = two_pages(&content, "/Font << /F1 6 0 R >>", vec![]);
        let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
        let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
        if find(&out.bytes, b"/HskIm0").is_some() {
            red.push(format!("{what}: moved into an XObject"));
        }
        if out.is_proven() {
            red.push(format!("{what}: proven"));
        }
        let ops = page_facts(&out.bytes, 1).ops;
        if ops.iter().any(|o| o == "Do") {
            red.push(format!("{what}: page two draws an XObject: {ops:?}"));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R11-F1 on a page WITHOUT a region: the /G stream of an ExtGState /SMask dictionary (no /Subtype, no
/// /Resources, `TjET` glued) is content a viewer draws; the crate must read or refuse it, never keep it
/// unread while proven.
#[test]
fn soft_mask_groups_are_content() {
    let pdf = two_pages(
        b"q /GS1 gs Q",
        "/Font << /F2 9 0 R >> /ExtGState << /GS1 << /SMask << /S /Luminosity /G 8 0 R >> >> >>",
        vec![
            (8, stream_body("", b"BT /F2 24 Tf 100 100 Td <010203040205060708090A07>TjET")),
            (9, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding << /Type /Encoding /Differences [1 /S /E /C /R /T /hyphen /A /L /P /H] >> >>".to_vec()),
        ],
    );
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    assert!(!out.is_proven(), "an unread soft-mask group read as proven: {:?}", out.proof);
}

/// R11-F4: a node typed /Page that has /Kids is a page for viewers (pdf.js catalog.js) and an
/// intermediate node for a reader that follows /Kids; a /Count other than the leaf count makes viewers
/// index pages differently. Both are refused when the file is opened.
#[test]
fn ambiguous_page_trees_are_refused() {
    let page = |n: u32, extra: &str| (n, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 9 0 R /Resources << /Font << /F1 6 0 R >> >> {extra} >>").into_bytes());
    let common = |pages: Vec<(u32, Vec<u8>)>, root: &[u8]| {
        let mut objs = vec![(1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()), (2, root.to_vec()), (6, HELVETICA.to_vec()), (9, stream_body("", b"BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET"))];
        objs.extend(pages);
        build(&objs)
    };
    let kids = common(vec![page(3, "/Kids [4 0 R]"), page(4, "")], b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
    let count = common(vec![page(3, ""), page(4, "")], b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 3 >>");
    let fine = common(vec![page(3, ""), page(4, "")], b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>");
    assert!(matches!(PdfDoc::open(&kids, &quick(), &token()), Err(PressError::Malformed("page_tree"))), "/Type /Page with /Kids");
    assert!(matches!(PdfDoc::open(&count, &quick(), &token()), Err(PressError::Malformed("page_tree"))), "/Count 3 over 2 leaves");
    assert_eq!(PdfDoc::open(&fine, &quick(), &token()).expect("open").inspect().page_count, 2);
}

/// R11-F3: a simple font with /Widths and no /FontDescriptor: pdf.js draws codes outside /Widths with the
/// base font's default width (600 for Courier), so the crate bounds them instead of reading 0: the
/// region over the advanced glyphs selects the string.
#[test]
fn widths_without_a_descriptor_are_bounded() {
    let pdf = page_doc(
        "/Font << /F1 32 0 R >>",
        b"BT /F1 10 Tf 100 700 Td (SECRET-ALPHA) Tj ET",
        vec![(32, b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /FirstChar 65 /LastChar 65 /Widths [600] >>".to_vec())],
    );
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let report = doc.redact_region(&region([130.0, 690.0, 180.0, 715.0]), UnsupportedPolicy::Refuse, &token()).expect("select");
    assert_eq!(report.removed_text, vec![b"SECRET-ALPHA".to_vec()], "the region over x 130..180 holds T-ALPHA for a viewer");
}

/// The raster shows the region opaque and the rest of the page rendered: a red rectangle partly under
/// the region stays red outside it, text outside the region is drawn, blank paper stays white.
#[test]
fn flattened_region_pixels_are_opaque_and_the_rest_is_rendered() {
    let pdf = page_doc(
        "/Font << /F1 6 0 R >>",
        b"1 0 0 rg 150 650 250 100 re f BT 0 g /F1 24 Tf 72 300 Td (Public) Tj ET BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET",
        vec![],
    );
    let mut doc = PdfDoc::open(&pdf, &Limits::default(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    let f = page_facts(&out.bytes, 0);
    assert!(raster_only(&f), "{:?} {:?}", f.ops, f.keys);
    let (w, h, _) = f.raster.as_ref().expect("raster");
    assert_eq!((*w, *h), (1275, 1650), "612 x 792 at the default 150 dpi");
    assert!(region_black(&f, R));
    assert_eq!(pixel(&f, 300.0, 700.0), Some([255, 0, 0]), "red rectangle outside the region");
    assert_eq!(pixel(&f, 195.0, 702.0), Some([0, 0, 0]), "red rectangle under the region is painted over");
    assert_eq!(pixel(&f, 550.0, 100.0), Some([255, 255, 255]), "blank paper");
    // "Public" at 24 pt: some pixels of its line are dark (the standard-14 substitute font is drawn).
    let dark = (0..80).flat_map(|dx| (0..17).map(move |dy| (72.0 + f64::from(dx), 300.0 + f64::from(dy)))).filter_map(|(x, y)| pixel(&f, x, y)).filter(|p| p.iter().all(|c| *c < 100)).count();
    assert!(dark > 40, "text outside the region is rendered ({dark} dark pixels)");
    assert!(needle_gone(&out.bytes, "SECRET-ALPHA"));
    assert!(out.region_cleared() && out.is_proven(), "{:?}", out.not_proven_reasons());
    assert_eq!(out.flattened[0].width_px, 1275); // D27-new-api
}

/// A page whose raster exceeds `Limits::max_flatten_pixels` (or an image declaring more pixels than
/// `Limits::max_decoded_bytes`) refuses with a typed reason; the document is left as it was.
#[test]
fn flatten_pixel_budget_is_a_typed_refusal() {
    let pdf = page_doc("/Font << /F1 6 0 R >>", b"BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET", vec![]);
    let limits = Limits::default();
    let limits = Limits { max_flatten_pixels: 1275 * 1650 - 1, ..limits }; // D27-new-api
    let mut doc = PdfDoc::open(&pdf, &limits, &token()).expect("open");
    let before = doc.save_full(&token()).expect("save");
    let res = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token());
    assert!(res.is_err(), "a raster over the budget must refuse");
    assert!(matches!(res, Err(PressError::FlattenRefused(hsk_studio_press::FlattenCause::PixelBudget)))); // D27-new-api
    assert_eq!(doc.save_full(&token()).expect("save"), before, "document restored");
    // dims_u32_max: an image declaring 100000 x 100000 pixels on the region's page.
    let huge = page_doc(
        "/Font << /F1 6 0 R >> /XObject << /Im1 35 0 R >>",
        b"BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET q 10 0 0 10 400 100 cm /Im1 Do Q",
        vec![(35, stream_body("/Type /XObject /Subtype /Image /Width 100000 /Height 100000 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0u8]))],
    );
    let mut doc = PdfDoc::open(&huge, &Limits::default(), &token()).expect("open");
    let res = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token());
    assert!(res.is_err(), "an image over the decode budget must refuse");
    assert!(matches!(res, Err(PressError::FlattenRefused(hsk_studio_press::FlattenCause::ImageBudget)))); // D27-new-api
}

/// Pages without a region keep their content, canonically rewritten (lead decision 2026-10-11T15:00:20Z);
/// only the region's page becomes a raster.
#[test]
fn pages_without_regions_stay_canonical_text() {
    let objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> >> >>".to_vec()),
        (4, stream_body("", b"BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET")),
        (5, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R /Resources << /Font << /F1 6 0 R >> >> >>".to_vec()),
        (6, HELVETICA.to_vec()),
        (7, stream_body("", b"BT   /F1 12 Tf   72 700 Td (Page two) Tj ET % a comment\n")),
    ];
    let pdf = build(&objs);
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    assert!(raster_only(&page_facts(&out.bytes, 0)), "region page is the raster");
    let second = page_facts(&out.bytes, 1);
    assert_eq!(second.ops, ["BT", "Tf", "Td", "Tj", "ET"], "page two keeps its text");
    assert!(second.raster.is_none());
    assert!(independent_streams(&out.bytes).iter().any(|s| s == b"BT\n/F1 12 Tf\n72 700 Td\n(Page two) Tj\nET\n"), "page two rewritten canonically");
    assert!(find(&out.bytes, b"a comment").is_none());
    assert!(out.proof.non_canonical_streams.is_empty());
    assert!(out.is_proven(), "{:?}", out.not_proven_reasons());
}

/// The narrowed claim (D27 (3)): DESCRIPTOR states what the proof is verified against and lists every
/// known viewer-differential limitation with its pre-review id or test (V4-07).
#[test]
fn descriptor_states_the_narrowed_claim_and_its_limitations() {
    assert!(DESCRIPTOR.contains("verified against Studio's own strict PDF reading"));
    assert!(DESCRIPTOR.contains("\"flatten\":"));
    for id in ["R11-F1", "R11-F3", "R11-F6", "R11-P1", "R11-P2", "R11-P3", "R11-P4", "R10-P3", "R10-P5"] {
        assert!(DESCRIPTOR.contains(id), "limitation {id} not listed");
    }
    // R13 (lead decision 2026-10-11T20:26:55Z): every finding is named where the DESCRIPTOR states it.
    for id in ["R13-F1", "R13-F2", "R13-F3", "R13-F4", "R13-F5", "R13-F6", "R13-F7", "R13-P1"] {
        assert!(DESCRIPTOR.contains(id), "R13 finding {id} not named");
    }
    // R13-F7 (CX-CAP-001 lead decision 2026-10-11T19:18:40Z): the colour limitation, verbatim.
    assert!(DESCRIPTOR.contains("flattened page colour is renderer-internal, not prism-managed"), "CX-CAP-001 colour limitation missing");
    // R13-F3: the corrected claim; the old one is gone.
    assert!(!DESCRIPTOR.contains("absent content only removes information"), "uncorrected fails_safe claim");
    assert!(DESCRIPTOR.contains("uncovers what it covered"));
    for test in [
        "prereview_region_pages_are_flattened",
        "flattened_region_pixels_are_opaque_and_the_rest_is_rendered",
        "flatten_pixel_budget_is_a_typed_refusal",
        "pages_without_regions_stay_canonical_text",
        "flattened_pages_hold_only_the_raster",
        "xfa_is_stripped_from_redaction_outputs",
        "optional_content_hidden_by_default_stays_hidden",
        "renderer_input_is_bounded_before_rendering",
        "renderer_input_images_are_bounded",
        "painted_sources_under_a_region_do_not_survive",
        "structure_and_widgets_of_flattened_pages_are_removed",
        "flatten_aggregate_over_budget",
        "renderer_refusal_is_typed",
    ] {
        assert!(DESCRIPTOR.contains(test), "DESCRIPTOR does not cite {test}");
    }
    // R13-V1: the names the DESCRIPTOR used to cite that no test carries.
    assert!(!DESCRIPTOR.contains("renderer_failure_is_a_typed_refusal"));
}
