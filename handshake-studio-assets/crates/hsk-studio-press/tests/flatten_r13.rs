//! R13 pre-review of the D27 flatten design (validation/R13-prereview-press.json; lead decision
//! 2026-10-11T20:26:55Z): one regression test per finding. Every assertion reads the SAVED bytes; page
//! structure and pixels are read with hayro-syntax (not this crate's parser), markers with a raw scan
//! plus an independent zlib inflate of every stream body.
//!
//! Red proofs: lines marked `R13-new-api` use API added by this remediation; the red copy of d079292
//! drops them (the remaining assertions fail there).
use hayro_interpret::hayro_syntax::Pdf;
use hayro_interpret::hayro_syntax::object::{Array as HArray, Dict as HDict, Stream as HStream};
use hsk_studio_accord::CancellationToken;
use hsk_studio_press::{FlattenNote, Limits, PdfDoc, PressError, RedactionRegion, UnsupportedPolicy, redact_and_save};
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
const R: [f64; 4] = [90.0, 690.0, 200.0, 715.0];
const SECRET: &str = "BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET";

fn token() -> CancellationToken {
    CancellationToken::default()
}

#[allow(clippy::field_reassign_with_default)]
fn quick() -> Limits {
    let mut l = Limits::default();
    l.flatten_dpi = 36;
    l
}

fn region(rect: [f64; 4]) -> RedactionRegion {
    RedactionRegion { page_index: 0, rect }
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

/// `marker` survives nowhere in the saved file (raw bytes or inflated stream bodies).
fn gone(bytes: &[u8], marker: &str) -> bool {
    find(bytes, marker.as_bytes()).is_none() && independent_streams(bytes).iter().all(|s| find(s, marker.as_bytes()).is_none())
}

fn keys(d: &HDict<'_>) -> Vec<String> {
    d.keys().map(|k| String::from_utf8_lossy(k.as_ref()).into_owned()).collect()
}

struct PageFacts {
    ops: Vec<String>,
    keys: Vec<String>,
    raster: Option<(u32, u32, Vec<u8>)>,
    media: [f64; 4],
    /// Keys of every node of the `/Parent` chain (nearest first).
    chain: Vec<Vec<String>>,
    /// The `/Annots` a viewer that inherits it (pdf.js) reads: the first found on the page or a node.
    viewer_annots: usize,
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
    let raster = raw.get::<HDict<'_>>(b"Resources").and_then(|r| r.get::<HDict<'_>>(b"XObject")).and_then(|x| x.get::<HStream<'_>>(b"HskFl0")).and_then(|s| {
        let w = s.dict().get::<u32>(b"Width")?;
        let h = s.dict().get::<u32>(b"Height")?;
        Some((w, h, s.decoded().ok()?.to_vec()))
    });
    // The /Parent chain, followed through the cross-reference table as a viewer does.
    let mut chain = Vec::new();
    let mut viewer_annots: Option<usize> = raw.get::<HArray<'_>>(b"Annots").map(|a| a.raw_iter().count());
    let mut parent = raw.get_ref(b"Parent");
    assert!(parent.is_some(), "the page names its /Parent");
    while let Some(r) = parent {
        let n: HDict<'_> = pdf.xref().get(r.into()).expect("the /Parent node resolves");
        chain.push(keys(&n));
        if viewer_annots.is_none() {
            viewer_annots = n.get::<HArray<'_>>(b"Annots").map(|a| a.raw_iter().count());
        }
        parent = n.get_ref(b"Parent");
        if chain.len() > 64 {
            break;
        }
    }
    let m = page.media_box();
    PageFacts { ops, keys: keys(raw), raster, media: [m.x0, m.y0, m.x1, m.y1], chain, viewer_annots: viewer_annots.unwrap_or(0) }
}

fn pixel(f: &PageFacts, x: f64, y: f64) -> Option<[u8; 3]> {
    let (w, h, px) = f.raster.as_ref()?;
    let sx = f64::from(*w) / (f.media[2] - f.media[0]);
    let sy = f64::from(*h) / (f.media[3] - f.media[1]);
    let (cx, cy) = (((x - f.media[0]) * sx) as usize, ((f.media[3] - y) * sy) as usize);
    let i = (cy * *w as usize + cx) * 3;
    px.get(i..i + 3).map(|s| [s[0], s[1], s[2]])
}

const PAGE_KEYS: [&str; 9] = ["Type", "Parent", "MediaBox", "CropBox", "Rotate", "UserUnit", "Resources", "Contents", "Annots"];
const NODE_KEYS: [&str; 4] = ["Type", "Kids", "Count", "Parent"];

/// R13-F1 (and V1: the test the docs cite): a flattened page holds only the raster, `/Annots []`
/// explicitly, and nothing reaches it by inheritance: every node of its `/Parent` chain holds only
/// `/Type /Kids /Count /Parent`. The R13-F1 counterexample: an annotation listed on the `/Pages` node
/// (pdf.js inherits `/Annots`) drawing SECRET-ALPHA over the region; the page's `/Thumb`, `/AA` and
/// `/StructParents` go too; the other page keeps what it inherited, now explicit.
#[test]
fn flattened_pages_hold_only_the_raster() {
    let pdf = build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /Annots [20 0 R] /Resources << /Font << /F1 6 0 R >> >> /MediaBox [0 0 612 792] /Rotate 0 /AA << >> >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Thumb 22 0 R /AA << /O 23 0 R >> /StructParents 0 >>".to_vec()),
        (4, stream_body("", b"BT /F1 12 Tf 72 300 Td (Public) Tj ET")),
        (5, b"<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>".to_vec()),
        (6, HELVETICA.to_vec()),
        (7, stream_body("", b"BT /F1 12 Tf 72 700 Td (Page two) Tj ET")),
        (20, b"<< /Type /Annot /Subtype /FreeText /Rect [90 690 200 715] /AP << /N 21 0 R >> >>".to_vec()),
        (21, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 110 25] /Resources << /Font << /F1 6 0 R >> >>", b"BT /F1 12 Tf 2 8 Td (SECRET-ALPHA) Tj ET")),
        (22, stream_body("/Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8", b"THUMB-SECRET")),
        (23, b"<< /S /JavaScript /JS (AA-SECRET) >>".to_vec()),
    ]);
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    let f = page_facts(&out.bytes, 0);
    let mut red: Vec<String> = Vec::new();
    if f.ops != ["q", "cm", "Do", "Q"] || f.raster.is_none() {
        red.push(format!("page is not the raster: {:?}", f.ops));
    }
    if !f.keys.iter().all(|k| PAGE_KEYS.contains(&k.as_str())) || !f.keys.iter().any(|k| k == "Annots") {
        red.push(format!("page keys {:?}", f.keys));
    }
    for node in &f.chain {
        if !node.iter().all(|k| NODE_KEYS.contains(&k.as_str())) {
            red.push(format!("a /Parent node holds inheritable keys: {node:?}"));
        }
    }
    if f.viewer_annots != 0 {
        red.push(format!("a viewer that inherits /Annots draws {} annotation(s) on the flattened page", f.viewer_annots));
    }
    for marker in ["SECRET-ALPHA", "THUMB-SECRET", "AA-SECRET"] {
        if !gone(&out.bytes, marker) {
            red.push(format!("{marker} survives in the saved file"));
        }
    }
    let two = page_facts(&out.bytes, 1);
    if two.ops != ["BT", "Tf", "Td", "Tj", "ET"] || two.media != [0.0, 0.0, 612.0, 792.0] {
        red.push(format!("page two lost what it inherited: {:?} {:?} {:?}", two.ops, two.keys, two.media));
    }
    if !out.region_flattened() || out.residual_in_region != 0 {
        red.push(format!("region_flattened {} residual {}", out.region_flattened(), out.residual_in_region));
    }
    if out.flattened_annotations_removed != 1 { // R13-new-api
        red.push(format!("annotations removed {}", out.flattened_annotations_removed)); // R13-new-api
    } // R13-new-api
    assert!(red.is_empty(), "{red:#?}");
}

/// R13-F2: a pure-XFA document (catalog `/NeedsRendering true`, `/AcroForm /XFA`, empty `/Fields`) is
/// drawn by pdf.js from its XFA packets, not from the flattened page. Choice: strip XFA from every
/// redaction output (flatten_purge.rs docs).
#[test]
fn xfa_is_stripped_from_redaction_outputs() {
    let xfa = b"<xdp:xdp><template><subform><draw><value><text>SECRET-ALPHA</text></value></draw></subform></template></xdp:xdp>";
    let pdf = build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R /NeedsRendering true /AcroForm << /XFA 40 0 R /Fields [] >> >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> >> >>".to_vec()),
        (4, stream_body("", b"BT /F1 12 Tf 72 300 Td (Please wait) Tj ET")),
        (6, HELVETICA.to_vec()),
        (40, stream_body("", xfa)),
    ]);
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    let saved = Pdf::new(out.bytes.clone()).expect("open saved");
    let catalog: HDict<'_> = saved.xref().get(saved.xref().root_id()).expect("catalog");
    let mut red: Vec<String> = Vec::new();
    if catalog.contains_key(b"NeedsRendering") {
        red.push("catalog keeps /NeedsRendering".into());
    }
    if catalog.get::<HDict<'_>>(b"AcroForm").is_some_and(|f| f.contains_key(b"XFA")) {
        red.push("/AcroForm keeps /XFA".into());
    }
    if !gone(&out.bytes, "SECRET-ALPHA") {
        red.push("the XFA template text survives".into());
    }
    if !out.xfa_removed { // R13-new-api
        red.push("xfa_removed not reported".into()); // R13-new-api
    } // R13-new-api
    if !out.region_flattened() {
        red.push(format!("region_flattened false, residual {}", out.residual_in_region));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R13-F3: optional content the document's default configuration hides (`/OCProperties /D /OFF`) stays
/// hidden in the raster: a marked-content rectangle and a form with `/OC` in the OFF group are not
/// drawn, the ON group's rectangle is; region pixels stay opaque.
#[test]
fn optional_content_hidden_by_default_stays_hidden() {
    let content = format!(
        "/OC /L1 BDC 0 g 300 100 200 100 re f EMC /OC /L2 BDC 0 g 50 100 100 100 re f EMC q /Fm1 Do Q /OC /L1 BDC BT 0 g /F1 24 Tf 300 600 Td (SALARY 95000) Tj ET EMC {SECRET}"
    );
    let pdf = build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [50 0 R 51 0 R] /D << /OFF [50 0 R] >> >> >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> /Properties << /L1 50 0 R /L2 51 0 R >> /XObject << /Fm1 30 0 R >> >> >>".to_vec(),
        ),
        (4, stream_body("", content.as_bytes())),
        (6, HELVETICA.to_vec()),
        (30, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /OC 50 0 R", b"0 g 300 400 200 100 re f")),
        (50, b"<< /Type /OCG /Name (Hidden) >>".to_vec()),
        (51, b"<< /Type /OCG /Name (Shown) >>".to_vec()),
    ]);
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    let f = page_facts(&out.bytes, 0);
    let mut red: Vec<String> = Vec::new();
    for (what, x, y) in [("OFF marked content", 400.0, 150.0), ("OFF form", 400.0, 450.0)] {
        if pixel(&f, x, y) != Some([255, 255, 255]) {
            red.push(format!("{what} drawn into the raster: {:?}", pixel(&f, x, y)));
        }
    }
    // The OFF text: no dark pixel along its line.
    let dark = (0..150).filter_map(|dx| pixel(&f, 300.0 + f64::from(dx), 608.0)).filter(|p| p.iter().all(|c| *c < 128)).count();
    if dark != 0 {
        red.push(format!("OFF text drawn into the raster ({dark} dark pixels)"));
    }
    if pixel(&f, 100.0, 150.0) != Some([0, 0, 0]) {
        red.push(format!("ON marked content not drawn: {:?}", pixel(&f, 100.0, 150.0)));
    }
    if pixel(&f, 150.0, 702.0) != Some([0, 0, 0]) {
        red.push("region pixels not opaque".into());
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R13-F4: nesting the renderer would recurse through (hayro-syntax arrays in a form the selector never
/// reads, a Type 4 tint transform, a colour space that is its own base, a ToUnicode CMap read by
/// hayro-postscript) is refused typed BEFORE rendering, and the document is unchanged; a stack overflow
/// would abort the process instead. Nesting within the bound renders.
#[test]
fn renderer_input_is_bounded_before_rendering() {
    let deep_arrays = vec![b'['; 100_000];
    let mut deep_procs = vec![b'{'; 100_000];
    deep_procs.extend(vec![b'}'; 100_000]);
    let deep_dicts = b"<<".repeat(100_000);
    let page = |resources: &str, content: &str, extra: Vec<(u32, Vec<u8>)>| {
        let mut objs = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << {resources} >> >>").into_bytes()),
            (4, stream_body("", format!("{content} {SECRET}").as_bytes())),
            (6, HELVETICA.to_vec()),
        ];
        objs.extend(extra);
        build(&objs)
    };
    let cases = vec![
        ("form outside the region: 100000 nested arrays", page("/Font << /F1 6 0 R >> /XObject << /Fm1 30 0 R >>", "q /Fm1 Do Q", vec![(30, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 10 10]", &deep_arrays))])),
        (
            "Type 4 tint transform: 100000 nested procedures",
            page("/Font << /F1 6 0 R >> /ColorSpace << /CS0 [/Separation /Spot /DeviceGray 60 0 R] >>", "q /CS0 cs 1 scn 400 100 10 10 re f Q", vec![(60, stream_body("/FunctionType 4 /Domain [0 1] /Range [0 1]", &deep_procs))]),
        ),
        (
            "an /Indexed colour space whose base is itself (the renderer builds it recursively)",
            page("/Font << /F1 6 0 R >> /ColorSpace << /CS1 63 0 R >>", "q /CS1 cs 0 sc 400 200 10 10 re f Q", vec![(63, b"[/Indexed 63 0 R 0 <00>]".to_vec())]),
        ),
        (
            "ToUnicode CMap: a run of 100000 << tokens",
            page("/Font << /F1 6 0 R /F2 61 0 R >>", "BT /F2 12 Tf 72 300 Td (Public) Tj ET", vec![(61, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 62 0 R >>".to_vec()), (62, stream_body("", &deep_dicts))]),
        ),
    ];
    let mut red: Vec<String> = Vec::new();
    for (what, pdf) in &cases {
        let mut doc = PdfDoc::open(pdf, &quick(), &token()).expect("open");
        let before = doc.save_full(&token()).expect("save");
        let res = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token());
        if res.is_ok() {
            red.push(format!("{what}: rendered"));
        }
        if !matches!(res, Err(PressError::FlattenRefused(hsk_studio_press::FlattenCause::NestingDepth))) { // R13-new-api
            red.push(format!("{what}: not a NestingDepth refusal: {:?}", res.as_ref().err())); // R13-new-api
        } // R13-new-api
        if doc.save_full(&token()).expect("save") != before {
            red.push(format!("{what}: document not restored"));
        }
    }
    // Within the bound (30 nested arrays in a marked-content property list): rendered.
    let nested = format!("/Tag << /A {}{} >> BDC EMC", "[".repeat(30), "]".repeat(30));
    let pdf = page("/Font << /F1 6 0 R >>", &nested, vec![]);
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    if let Err(e) = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()) {
        red.push(format!("nesting within the bound refused: {e}"));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R13-F5: image sizes are bounded BEFORE rendering whatever the spelling: an inline image with
/// abbreviated keys whose Flate data holds 16 bytes for 2000 x 2000 CMYK samples, an image XObject sized
/// by `/W` `/H` with one data byte, and an image whose renderer memory exceeds
/// `Limits::max_render_image_bytes` are typed refusals; a small, complete image renders.
#[test]
fn renderer_input_images_are_bounded() {
    let zlib = miniz_oxide::deflate::compress_to_vec_zlib(&[0u8; 16], 6);
    let mut inline = b"q 100 0 0 100 400 100 cm BI /W 2000 /H 2000 /BPC 8 /CS /CMYK /F /Fl ID ".to_vec();
    inline.extend_from_slice(&zlib);
    inline.extend_from_slice(b" EI Q ");
    inline.extend_from_slice(SECRET.as_bytes());
    let page = |resources: &str, content: &[u8], extra: Vec<(u32, Vec<u8>)>| {
        let mut objs = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> {resources} >> >>").into_bytes()),
            (4, stream_body("", content)),
            (6, HELVETICA.to_vec()),
        ];
        objs.extend(extra);
        build(&objs)
    };
    let draw = format!("q 100 0 0 100 400 300 cm /Im1 Do Q {SECRET}");
    let cases = vec![
        ("inline image, abbreviated keys, 16 bytes for 2000 x 2000 CMYK", page("", &inline, vec![])),
        (
            "image XObject sized by /W /H, one data byte",
            page("/XObject << /Im1 35 0 R >>", draw.as_bytes(), vec![(35, stream_body("/Type /XObject /Subtype /Image /W 2000 /H 2000 /BPC 8 /CS /DeviceGray", &[0]))]),
        ),
        (
            "8000 x 8000 CMYK JPEG over the renderer memory budget",
            page("/XObject << /Im1 35 0 R >>", draw.as_bytes(), vec![(35, stream_body("/Type /XObject /Subtype /Image /Width 8000 /Height 8000 /BitsPerComponent 8 /ColorSpace /DeviceCMYK /Filter /DCTDecode", &[0xFF, 0xD8, 0xFF, 0xD9]))]),
        ),
    ];
    let mut red: Vec<String> = Vec::new();
    for (what, pdf) in &cases {
        let mut doc = PdfDoc::open(pdf, &quick(), &token()).expect("open");
        let before = doc.save_full(&token()).expect("save");
        let res = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token());
        if !matches!(res, Err(PressError::FlattenRefused(hsk_studio_press::FlattenCause::ImageBudget))) {
            red.push(format!("{what}: not an ImageBudget refusal: {:?}", res.as_ref().map(|o| o.flattened.clone()).err()));
        }
        if doc.save_full(&token()).expect("save") != before {
            red.push(format!("{what}: document not restored"));
        }
    }
    let small = page("/XObject << /Im1 35 0 R >>", draw.as_bytes(), vec![(35, stream_body("/Type /XObject /Subtype /Image /Width 8 /Height 8 /BitsPerComponent 8 /ColorSpace /DeviceGray", &[0u8; 64]))]);
    let mut doc = PdfDoc::open(&small, &quick(), &token()).expect("open");
    if let Err(e) = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()) {
        red.push(format!("a small complete image refused: {e}"));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// The pages of the R13-F6 cases: `/Im1` (an image whose bytes are `SCAN-PIXELS-SECRET`) and `/Fm9` (a
/// vector form marked `VECTOR-FORM-SECRET`) are drawn in the region of page one; page two draws
/// `page_two`. `layout` places the resources: on the `/Pages` node (inherited), in one dictionary both
/// pages share, or (one page) on the node.
fn painted_doc(layout: &str, page_two: &str) -> Vec<u8> {
    painted_doc_with(layout, page_two, "", "", Vec::new())
}

/// [`painted_doc`] with extra catalog entries (`catalog`), extra page-two entries (`page_two_keys`) and extra
/// objects (`more`): the doubly referenced sources of R14-P1.
fn painted_doc_with(layout: &str, page_two: &str, catalog: &str, page_two_keys: &str, more: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let res = "<< /XObject << /Im1 10 0 R /Fm9 11 0 R >> /Font << /F1 6 0 R >> >>";
    let page_one = format!("q 612 0 0 792 0 0 cm /Im1 Do Q q /Fm9 Do Q BT /F1 12 Tf 72 300 Td (Public) Tj ET {SECRET}");
    let (node, own) = match layout {
        "shared" => (String::new(), "/Resources 9 0 R".to_string()),
        _ => (format!("/Resources {res}"), String::new()),
    };
    let kids = if layout == "single" { "[3 0 R] /Count 1" } else { "[3 0 R 5 0 R] /Count 2" };
    let mut objs = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>").into_bytes()),
        (2, format!("<< /Type /Pages /Kids {kids} /MediaBox [0 0 612 792] {node} >>").into_bytes()),
        (3, format!("<< /Type /Page /Parent 2 0 R /Contents 4 0 R {own} >>").into_bytes()),
        (4, stream_body("", page_one.as_bytes())),
        (6, HELVETICA.to_vec()),
        (9, res.as_bytes().to_vec()),
        (10, stream_body("/Type /XObject /Subtype /Image /Width 18 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"SCAN-PIXELS-SECRET")),
        (11, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /HskMarker (VECTOR-FORM-SECRET)", b"0 g 100 690 50 20 re f")),
    ];
    if layout != "single" {
        objs.push((5, format!("<< /Type /Page /Parent 2 0 R /Contents 7 0 R {own} {page_two_keys} >>").into_bytes()));
        objs.push((7, stream_body("", page_two.as_bytes())));
    }
    objs.extend(more);
    build(&objs)
}

/// R13-F6: an image or vector form drawn in the region survives byte for byte when it is reachable
/// through an inherited or shared resource dictionary. After flattening it is gone from the file unless
/// a kept page DRAWS it (then it stays and is not reported).
#[test]
fn painted_sources_under_a_region_do_not_survive() {
    let text_only = "BT /F1 12 Tf 72 700 Td (Page two) Tj ET";
    let mut red: Vec<String> = Vec::new();
    for (what, layout) in [("one page, resources on the /Pages node", "single"), ("two pages, inherited resources", "inherited"), ("two pages, one shared resource dictionary", "shared")] {
        let pdf = painted_doc(layout, text_only);
        let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
        let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
        for marker in ["SCAN-PIXELS-SECRET", "VECTOR-FORM-SECRET"] {
            if !gone(&out.bytes, marker) {
                red.push(format!("{what}: {marker} survives"));
            }
        }
        if layout != "single" {
            let two = page_facts(&out.bytes, 1);
            if two.ops != ["BT", "Tf", "Td", "Tj", "ET"] {
                red.push(format!("{what}: page two changed: {:?}", two.ops));
            }
        }
        if !out.painted_sources_kept.is_empty() { // R13-new-api
            red.push(format!("{what}: reported kept {:?}", out.painted_sources_kept)); // R13-new-api
        } // R13-new-api
        if !out.region_flattened() {
            red.push(format!("{what}: region_flattened false (residual {})", out.residual_in_region));
        }
    }
    // Page two draws the image too: it stays (drawn content of a kept page), not reported.
    let pdf = painted_doc("inherited", "q 10 0 0 10 400 100 cm /Im1 Do Q BT /F1 12 Tf 72 700 Td (Page two) Tj ET");
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if gone(&out.bytes, "SCAN-PIXELS-SECRET") {
        red.push("an image a kept page draws was dropped".into());
    }
    if !gone(&out.bytes, "VECTOR-FORM-SECRET") {
        red.push("drawn-elsewhere case: the form only page one drew survives".into());
    }
    if !out.painted_sources_kept.is_empty() { // R13-new-api
        red.push(format!("drawn-elsewhere case reported kept {:?}", out.painted_sources_kept)); // R13-new-api
    } // R13-new-api
    assert!(red.is_empty(), "{red:#?}");
}

/// A file specification whose embedded file IS the painted image (object 10): the plain, non-drawing reference of
/// the R14-P1 cases.
const IMAGE_FILESPEC: &[u8] = b"<< /Type /Filespec /F (picture.bin) /AFRelationship /Data /EF << /F 10 0 R >> >>";

/// R14-P1 (PLR-076): a painted region source that a kept catalog structure also REFERENCES without drawing it
/// (`/AF`, a `/Names` `/EmbeddedFiles` tree, a `/Dests` name tree) is not silently kept: it stays in the file (the
/// kept structure is not edited), it is reported in `painted_sources_kept` / `NotProven::PaintedSourceKept`, and
/// `region_flattened` reads false. The form that nothing else references is still dropped.
#[test]
fn painted_source_referenced_by_a_kept_catalog_structure_is_reported() {
    let text_only = "BT /F1 12 Tf 72 700 Td (Page two) Tj ET";
    let mut red: Vec<String> = Vec::new();
    for (what, catalog, more) in [
        ("catalog /AF", "/AF [20 0 R]", vec![(20, IMAGE_FILESPEC.to_vec())]),
        (
            "/Names /EmbeddedFiles tree",
            "/Names << /EmbeddedFiles 21 0 R >>",
            vec![(20, IMAGE_FILESPEC.to_vec()), (21, b"<< /Names [(att) 20 0 R] >>".to_vec())],
        ),
        ("/Names /Dests tree", "/Names << /Dests 22 0 R >>", vec![(22, b"<< /Names [(d) [10 0 R /Fit]] >>".to_vec())]),
    ] {
        let pdf = painted_doc_with("inherited", text_only, catalog, "", more);
        let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
        let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
        if out.painted_sources_kept != [10] {
            red.push(format!("{what}: painted_sources_kept {:?}, expected [10] (the image)", out.painted_sources_kept));
        }
        if out.not_proven_reasons() != [hsk_studio_press::NotProven::PaintedSourceKept] {
            red.push(format!("{what}: not-proven reasons {:?}", out.not_proven_reasons()));
        }
        if out.region_flattened() || out.is_proven() {
            red.push(format!("{what}: region_flattened {} is_proven {} with a kept, undrawn source", out.region_flattened(), out.is_proven()));
        }
        if gone(&out.bytes, "SCAN-PIXELS-SECRET") {
            red.push(format!("{what}: the referenced image was dropped (the kept structure would dangle)"));
        }
        if !gone(&out.bytes, "VECTOR-FORM-SECRET") {
            red.push(format!("{what}: the form nothing references survives"));
        }
        let two = page_facts(&out.bytes, 1);
        if two.ops != ["BT", "Tf", "Td", "Tj", "ET"] {
            red.push(format!("{what}: page two changed: {:?}", two.ops));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R16 (P-1): the image (object 10, the painted source of the flattened page) is the one painted source reported
/// kept in `pdf`: `NotProven::PaintedSourceKept` is the only reason, `region_flattened` and `is_proven` read false,
/// the image bytes stay (the kept file's reference would dangle), the form nothing references is gone.
fn reported_kept_image(what: &str, pdf: &[u8], red: &mut Vec<String>) {
    let mut doc = PdfDoc::open(pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if out.painted_sources_kept != [10] {
        red.push(format!("{what}: painted_sources_kept {:?}, expected [10] (the image)", out.painted_sources_kept));
    }
    if out.not_proven_reasons() != [hsk_studio_press::NotProven::PaintedSourceKept] {
        red.push(format!("{what}: not-proven reasons {:?}", out.not_proven_reasons()));
    }
    if out.region_flattened() || out.is_proven() {
        red.push(format!("{what}: region_flattened {} is_proven {} with a kept, undrawn source", out.region_flattened(), out.is_proven()));
    }
    if gone(&out.bytes, "SCAN-PIXELS-SECRET") {
        red.push(format!("{what}: the referenced image was dropped (the kept reference would dangle)"));
    }
    if !gone(&out.bytes, "VECTOR-FORM-SECRET") {
        red.push(format!("{what}: the form nothing references survives"));
    }
}

/// R16 (P-1): a kept page that LISTS a painted region source in its own `/Resources` without drawing it. An entry of
/// `/XObject` (a gated category) nobody draws is dropped (the control); an entry of any other resource category
/// (`/Properties`, `/ColorSpace`, an unknown key) is a plain reference: the source stays and is reported.
#[test]
fn painted_source_referenced_but_not_drawn_by_a_kept_page_is_reported() {
    let text_only = "BT /F1 12 Tf 72 700 Td (Page two) Tj ET";
    let own = |extra: &str| format!("/Resources << /Font << /F1 6 0 R >> {extra} >>");
    let mut red: Vec<String> = Vec::new();
    for (what, extra) in [
        ("page two lists it in /Properties", "/Properties << /P0 10 0 R >>"),
        ("page two lists it in /ColorSpace", "/ColorSpace << /Cs0 10 0 R >>"),
        ("page two lists it under an unknown /Resources key", "/HskExtra [10 0 R]"),
    ] {
        reported_kept_image(what, &painted_doc_with("inherited", text_only, "", &own(extra), Vec::new()), &mut red);
    }
    // Control: listed in /XObject, not drawn, referenced by nothing else: dropped, nothing reported.
    let pdf = painted_doc_with("inherited", text_only, "", &own("/XObject << /Im1 10 0 R >>"), Vec::new());
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if !out.painted_sources_kept.is_empty() || !out.region_flattened() || !gone(&out.bytes, "SCAN-PIXELS-SECRET") {
        red.push(format!("/XObject entry nobody draws: kept {:?}, region_flattened {}", out.painted_sources_kept, out.region_flattened()));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R16 (P-1): a painted region source that a kept page's drawn object merely REFERENCES: a plain key of a form the page
/// draws and the `/Alternates` of an image it draws. None of them is a draw by a kept page: the source stays and is
/// reported. Changed by PLR-104 (`/Properties` is a gated category): an undrawn form listed in a kept page's `/Properties`
/// that no `BDC` or `DP` names is not reached, so the image only its content draws is dropped (its entry in the form's
/// `/XObject` pruned) and nothing is reported; before PLR-104 the `/Properties` entry was a plain reference and the image
/// stayed, reported.
#[test]
fn painted_source_referenced_by_a_key_of_a_drawn_object_is_reported() {
    let own = |extra: &str| format!("/Resources << /Font << /F1 6 0 R >> {extra} >>");
    let page = |draw: &str| format!("{draw} BT /F1 12 Tf 72 700 Td (Page two) Tj ET");
    let form_with_key = stream_body("/Type /XObject /Subtype /Form /BBox [0 0 10 10] /PieceInfo << /Hsk 10 0 R >>", b"0 g 0 0 10 10 re f");
    let image_with_alternate =
        stream_body("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Alternates [<< /Image 10 0 R >>]", &[0u8]);
    let undrawn_form_that_draws_it = stream_body("/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << /XObject << /Im1 10 0 R >> >>", b"q 10 0 0 10 0 0 cm /Im1 Do Q");
    let mut red: Vec<String> = Vec::new();
    for (what, content, resources, more) in [
        ("a plain key (/PieceInfo) of a form page two draws", page("q /Fx Do Q"), own("/XObject << /Fx 40 0 R >>"), vec![(40, form_with_key)]),
        (
            "the /Alternates of an image page two draws",
            page("q 10 0 0 10 400 100 cm /Ix Do Q"),
            own("/XObject << /Ix 41 0 R >>"),
            vec![(41, image_with_alternate)],
        ),
    ] {
        reported_kept_image(what, &painted_doc_with("inherited", &content, "", &resources, more), &mut red);
    }
    let pdf = painted_doc_with("inherited", &page(""), "", &own("/Properties << /P0 42 0 R >>"), vec![(42, undrawn_form_that_draws_it)]);
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if !out.painted_sources_kept.is_empty() || !gone(&out.bytes, "SCAN-PIXELS-SECRET") || !out.region_flattened() {
        red.push(format!(
            "an undrawn form (/Properties, no BDC names it) whose content draws it: kept {:?}, image dropped {}, region_flattened {}",
            out.painted_sources_kept,
            gone(&out.bytes, "SCAN-PIXELS-SECRET"),
            out.region_flattened()
        ));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R14-P1, R16, R18, the other side: a painted source a kept page DRAWS with `Do` directly in its own content (also
/// when a catalog structure references it too, when a form draws it as well, through the page's own `/Resources`, an
/// ancestor's inherited ones, a shared dictionary or an indirect `/XObject` dictionary) is drawn content of the kept
/// file: it stays and is not reported. The minimal draw set: nothing else is silent, so the same `Do` inside
/// `BDC /OC` marked content is reported.
#[test]
fn painted_source_drawn_directly_by_a_kept_page_is_not_reported() {
    let text = "BT /F1 12 Tf 72 700 Td (Page two) Tj ET";
    let draw = "q 10 0 0 10 400 100 cm /Im1 Do Q";
    let mut red: Vec<String> = Vec::new();
    // (a) page two draws the image and /AF lists it as well (resources inherited from the /Pages node).
    let af = painted_doc_with("inherited", &format!("{draw} {text}"), "/AF [20 0 R]", "", vec![(20, IMAGE_FILESPEC.to_vec())]);
    // (b) page two's own /Resources, direct dictionaries.
    let own = painted_doc_with("inherited", &format!("{draw} {text}"), "", "/Resources << /Font << /F1 6 0 R >> /XObject << /Im1 10 0 R >> >>", Vec::new());
    // (c) one resource dictionary object both pages share.
    let shared = painted_doc("shared", &format!("{draw} {text}"));
    // (d) the /XObject sub-dictionary is an indirect dictionary object.
    let indirect = painted_doc_with("inherited", &format!("{draw} {text}"), "", "/Resources << /Font << /F1 6 0 R >> /XObject 41 0 R >>", vec![(41, b"<< /Im1 10 0 R >>".to_vec())]);
    // (e) a form page two draws draws the image too: the direct `Do` of the page decides, the form changes nothing.
    let form = stream_body("/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /Im1 10 0 R >> >>", b"q 100 0 0 100 0 0 cm /Im1 Do Q");
    let and_a_form = painted_doc_with(
        "inherited",
        &format!("/Fx Do {draw} {text}"),
        "",
        "/Resources << /Font << /F1 6 0 R >> /XObject << /Im1 10 0 R /Fx 40 0 R >> >>",
        vec![(40, form)],
    );
    for (what, pdf) in [
        ("kept page draws it, /AF lists it", af),
        ("page two's own /Resources", own),
        ("one shared resource dictionary object", shared),
        ("an indirect /XObject dictionary", indirect),
        ("a form draws it too", and_a_form),
    ] {
        silent_drawn_image(what, &pdf, &mut red);
        let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
        let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
        if !gone(&out.bytes, "VECTOR-FORM-SECRET") {
            red.push(format!("{what}: the form nothing draws survives"));
        }
    }
    // The same `Do` inside BDC /OC marked content (optional content counts as possibly hidden) is reported.
    let hidden = painted_doc("inherited", &format!("/OC << /Type /OCG >> BDC {draw} EMC {text}"));
    reported_kept_image_too("the same Do inside BDC /OC", &hidden, &mut red);
    assert!(red.is_empty(), "{red:#?}");
}

/// R18 (replaces the R16 test of the same case, which expected nothing reported): drawing an image does not make what
/// is rendered along with it silent. Page two draws the image (object 10, silent), a shading, the shading pattern and
/// the graphics state; the same resource dictionary lists them on the flattened page one. Only the image is silent: its
/// `/SMask` image (12), the shading (13), the pattern (14), the pattern's `/Shading` (15) and the `/SMask` `/G` group of
/// the graphics state (17) are reported (a shared logo with an alpha mask is the usual case: its mask is reported, an
/// over-report that is accepted), and every one of them stays in the file.
#[test]
fn painted_sources_rendered_with_a_drawn_image_are_reported() {
    let resources = "<< /XObject << /Im1 10 0 R >> /Shading << /Sh1 13 0 R >> /Pattern << /P1 14 0 R >> /ExtGState << /G1 16 0 R >> /Font << /F1 6 0 R >> >>";
    let axial = |coords: &str| format!("<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [{coords}] /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> >>").into_bytes();
    let page_two = "q /G1 gs 10 0 0 10 400 100 cm /Im1 Do Q q /Sh1 sh Q /Pattern cs /P1 scn 100 100 50 50 re f BT /F1 12 Tf 72 700 Td (Page two) Tj ET";
    let pdf = build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /MediaBox [0 0 612 792] >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources 9 0 R >>".to_vec()),
        (4, stream_body("", format!("q 612 0 0 792 0 0 cm /Im1 Do Q BT /F1 12 Tf 72 300 Td (Public) Tj ET {SECRET}").as_bytes())),
        (5, b"<< /Type /Page /Parent 2 0 R /Contents 7 0 R /Resources 9 0 R >>".to_vec()),
        (6, HELVETICA.to_vec()),
        (7, stream_body("", page_two.as_bytes())),
        (9, resources.as_bytes().to_vec()),
        (10, stream_body("/Type /XObject /Subtype /Image /Width 18 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 12 0 R", b"SCAN-PIXELS-SECRET")),
        (12, stream_body("/Type /XObject /Subtype /Image /Width 18 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"MASK-PIXELS-SECRET")),
        (13, axial("0 0 100 0")),
        (14, b"<< /PatternType 2 /Shading 15 0 R >>".to_vec()),
        (15, axial("0 0 0 100")),
        (16, b"<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 17 0 R >> >>".to_vec()),
        (17, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Group << /S /Transparency /CS /DeviceGray >> /HskMarker (SMASK-GROUP-SECRET)", b"1 g 0 0 612 792 re f")),
    ]);
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    let mut red: Vec<String> = Vec::new();
    if out.painted_sources_kept != [12, 13, 14, 15, 17] {
        red.push(format!("painted_sources_kept {:?}, expected [12, 13, 14, 15, 17] (12 = the image's /SMask, 13 = the shading, 14 = the pattern, 15 = its /Shading, 17 = the soft-mask group; the image 10 is silent)", out.painted_sources_kept));
    }
    if out.not_proven_reasons() != [hsk_studio_press::NotProven::PaintedSourceKept] || out.region_flattened() {
        red.push(format!("reasons {:?}, region_flattened {}", out.not_proven_reasons(), out.region_flattened()));
    }
    for marker in ["SCAN-PIXELS-SECRET", "MASK-PIXELS-SECRET", "SMASK-GROUP-SECRET"] {
        if gone(&out.bytes, marker) {
            red.push(format!("{marker}: a kept page's source was dropped (the kept reference would dangle)"));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R17: [`reported_kept_image`] for the documents in which the misplaced image makes the kept file unprovable in another
/// way too (page two's text finds no font, an appearance or soft-mask group holds an image): the image is the one
/// painted source reported kept, `NotProven::PaintedSourceKept` is among the reasons, `region_flattened` reads false,
/// the image bytes stay, the form nothing references is gone.
fn reported_kept_image_too(what: &str, pdf: &[u8], red: &mut Vec<String>) {
    let mut doc = PdfDoc::open(pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if out.painted_sources_kept != [10] {
        red.push(format!("{what}: painted_sources_kept {:?}, expected [10] (the image)", out.painted_sources_kept));
    }
    if !out.not_proven_reasons().contains(&hsk_studio_press::NotProven::PaintedSourceKept) {
        red.push(format!("{what}: not-proven reasons {:?} lack PaintedSourceKept", out.not_proven_reasons()));
    }
    if out.region_flattened() {
        red.push(format!("{what}: region_flattened true with a kept, undrawn source"));
    }
    if gone(&out.bytes, "SCAN-PIXELS-SECRET") {
        red.push(format!("{what}: the image was dropped (the kept reference would dangle)"));
    }
    if !gone(&out.bytes, "VECTOR-FORM-SECRET") {
        red.push(format!("{what}: the form nothing references survives"));
    }
}

/// R17: the image (object 10) stays in the file and nothing is reported, because a kept page DRAWS it.
fn silent_drawn_image(what: &str, pdf: &[u8], red: &mut Vec<String>) {
    let mut doc = PdfDoc::open(pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if !out.painted_sources_kept.is_empty() {
        red.push(format!("{what}: reported kept {:?} although a kept page draws it", out.painted_sources_kept));
    }
    if !out.region_flattened() {
        red.push(format!("{what}: region_flattened false (residual {}, not proven {:?})", out.residual_in_region, out.not_proven_reasons()));
    }
    if gone(&out.bytes, "SCAN-PIXELS-SECRET") {
        red.push(format!("{what}: a drawn image was dropped"));
    }
}

/// R17, R18: kept page two takes the extra entries `keys` (no `/Annots`), the extra objects `more` and runs `content`
/// before its text; `annots` are the numbers of the annotations in `more` that it lists. The image (object 10) is placed
/// or named where a viewer does not draw it (or does not draw it as an image): it stays and is reported. R18 (the
/// minimal draw set): the same document in which an annotation appearance page two lists draws the image (object 34, one
/// more annotation, 33) is reported too, because a draw by an appearance is not a direct `Do` in a kept page's own
/// content. (Before R18 that control was silent; the controls that stay silent draw with a direct `Do`, tests
/// `painted_source_drawn_directly_by_a_kept_page_is_not_reported`, `painted_sources_under_a_region_do_not_survive`.)
fn reported_even_when_an_appearance_draws_it(what: &str, keys: &str, content: &str, more: Vec<(u32, Vec<u8>)>, annots: &[u32], red: &mut Vec<String>) {
    let text = format!("{content} BT /F1 12 Tf 72 700 Td (Page two) Tj ET");
    let list = |extra: &[u32]| {
        let all: Vec<String> = annots.iter().chain(extra).map(|n| format!("{n} 0 R")).collect();
        if all.is_empty() { String::new() } else { format!("/Annots [{}]", all.join(" ")) }
    };
    reported_kept_image_too(what, &painted_doc_with("inherited", &text, "", &format!("{keys} {}", list(&[])), more.clone()), red);
    let stamp = b"<< /Type /Annot /Subtype /Stamp /Rect [300 100 400 200] /P 5 0 R /AP << /N 34 0 R >> >>".to_vec();
    let appearance = stream_body(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /Im1 10 0 R >> >>",
        b"q 100 0 0 100 0 0 cm /Im1 Do Q",
    );
    let mut drawn = more;
    drawn.extend([(33, stamp), (34, appearance)]);
    reported_kept_image_too(
        &format!("{what}, and an annotation appearance page two lists draws it"),
        &painted_doc_with("inherited", &text, "", &format!("{keys} {}", list(&[33])), drawn),
        red,
    );
}

/// R17 (P1): the image is a kept page's `/Resources` value, or a drawn form's. A resource dictionary is reached by
/// position, not drawn: the image stays and is reported (a viewer draws nothing from it).
#[test]
fn painted_source_placed_as_a_resources_value_is_reported() {
    let mut red: Vec<String> = Vec::new();
    reported_even_when_an_appearance_draws_it("the image is page two's /Resources value", "/Resources 10 0 R", "", Vec::new(), &[], &mut red);
    let form = stream_body("/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources 10 0 R", b"0 g 0 0 10 10 re f");
    reported_even_when_an_appearance_draws_it(
        "the image is the /Resources value of a form page two draws",
        "/Resources << /Font << /F1 6 0 R >> /XObject << /Fx 40 0 R >> >>",
        "q /Fx Do Q",
        vec![(40, form)],
        &[],
        &mut red,
    );
    assert!(red.is_empty(), "{red:#?}");
}

/// R17 (P1): the image is the value of a gated key of a kept page's `/Resources` (`/XObject 10 0 R`, `/ExtGState 10 0 R`)
/// where a sub-dictionary of names belongs: reached by position, no name selects it, no draw.
#[test]
fn painted_source_placed_as_a_gated_sub_dictionary_is_reported() {
    let mut red: Vec<String> = Vec::new();
    for (what, keys) in [
        ("the image is page two's /XObject value", "/Resources << /Font << /F1 6 0 R >> /XObject 10 0 R >>"),
        ("the image is page two's /ExtGState value", "/Resources << /Font << /F1 6 0 R >> /ExtGState 10 0 R >>"),
    ] {
        reported_even_when_an_appearance_draws_it(what, keys, "", Vec::new(), &[], &mut red);
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R17 (P1): the image is an appearance state (or the appearance stream itself) of an annotation a kept page lists.
/// An appearance is drawn only as a form XObject: an image there is no draw, so it stays and is reported.
#[test]
fn painted_source_placed_as_an_appearance_state_is_reported() {
    // `/AS /x` selects the state `x`, so it is the kind of the object, not the selection, that keeps the state undrawn.
    let annot = |ap: &str| format!("<< /Type /Annot /Subtype /Stamp /Rect [300 100 400 200] /P 5 0 R /AS /x /AP << {ap} >> >>").into_bytes();
    let mut red: Vec<String> = Vec::new();
    for (what, ap) in [
        ("the image is an /AP /N state value", "/N << /x 10 0 R >>"),
        ("the image is the /AP /N stream", "/N 10 0 R"),
        ("the image is an /AP /D state value", "/D << /x 10 0 R >>"),
    ] {
        reported_even_when_an_appearance_draws_it(what, "", "", vec![(30, annot(ap))], &[30], &mut red);
    }
    // Not an image either, but no form: a shading (object 13, a painted source because page one draws it with `sh`) as the
    // appearance state of an annotation page two lists is not drawn as an appearance.
    let shading = b"<< /ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 100 0] /Function << /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 1 >> >>";
    let pdf = build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /MediaBox [0 0 612 792] >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /Shading << /Sh1 13 0 R >> /Font << /F1 6 0 R >> >> >>".to_vec()),
        (4, stream_body("", format!("q /Sh1 sh Q BT /F1 12 Tf 72 300 Td (Public) Tj ET {SECRET}").as_bytes())),
        (5, b"<< /Type /Page /Parent 2 0 R /Contents 7 0 R /Resources << /Font << /F1 6 0 R >> >> /Annots [30 0 R] >>".to_vec()),
        (6, HELVETICA.to_vec()),
        (7, stream_body("", b"BT /F1 12 Tf 72 700 Td (Page two) Tj ET")),
        (13, shading.to_vec()),
        (30, annot("/N << /x 13 0 R >>")),
    ]);
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if out.painted_sources_kept != [13] || out.region_flattened() {
        red.push(format!("a shading as an /AP /N state value: painted_sources_kept {:?}, region_flattened {}, expected [13] and false", out.painted_sources_kept, out.region_flattened()));
    }
    if !out.not_proven_reasons().contains(&hsk_studio_press::NotProven::PaintedSourceKept) {
        red.push(format!("a shading as an /AP /N state value: not-proven reasons {:?} lack PaintedSourceKept", out.not_proven_reasons()));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R18 (the minimal draw set; replaces the R17b test `painted_source_in_an_appearance_a_viewer_does_not_draw_is_reported`,
/// which expected the appearances a viewer draws to be silent): an annotation appearance is not a direct `Do` in a
/// kept page's own content, so the image an appearance form draws is reported whether or not a viewer draws that
/// appearance: the unselected state (`/AS` another state, missing, or naming no state), an unselected `/R` or `/D`
/// state, the Invisible (1), Hidden (2) and NoView (32) flags, and the appearances a viewer does draw (the selected
/// state, the stream itself, a flag that hides nothing, `/R` and `/D`, with `/AF` listing the image too). Form 31 draws
/// the image through its own `/Resources`, form 32 is a blank.
#[test]
fn painted_source_in_an_annotation_appearance_is_reported() {
    let text_only = "BT /F1 12 Tf 72 700 Td (Page two) Tj ET";
    let draws_image = stream_body(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /Im1 10 0 R >> >>",
        b"q 100 0 0 100 0 0 cm /Im1 Do Q",
    );
    let blank = stream_body("/Type /XObject /Subtype /Form /BBox [0 0 100 100]", b"0 g");
    let doc = |extra: &str, ap: &str| {
        let annot = format!("<< /Type /Annot /Subtype /Stamp /Rect [300 100 400 200] /P 5 0 R {extra} /AP << {ap} >> >>").into_bytes();
        painted_doc_with("inherited", text_only, "", "/Annots [30 0 R]", vec![(30, annot), (31, draws_image.clone()), (32, blank.clone())])
    };
    let states = "/N << /On 31 0 R /Off 32 0 R >>";
    let with_af = painted_doc_with(
        "inherited",
        text_only,
        "/AF [20 0 R]",
        "/Annots [30 0 R]",
        vec![
            (20, IMAGE_FILESPEC.to_vec()),
            (30, b"<< /Type /Annot /Subtype /Widget /FT /Btn /Rect [300 100 400 200] /P 5 0 R /AS /On /AP << /N 32 0 R >> >>".to_vec()),
            (31, draws_image.clone()),
            (32, b"<< /On 31 0 R >>".to_vec()),
        ],
    );
    let mut red: Vec<String> = Vec::new();
    for (what, pdf) in [
        ("the state /AS does not name (/AS /Off)", doc("/AS /Off", states)),
        ("a state dictionary and no /AS", doc("", states)),
        ("/AS names no state of the dictionary", doc("/AS /Gone", states)),
        ("an unselected rollover state (/AP /R)", doc("/AS /Off", "/R << /On 31 0 R >>")),
        ("an unselected down state (/AP /D)", doc("/AS /Off", "/D << /On 31 0 R >>")),
        ("the Invisible flag (/F 1)", doc("/F 1", "/N 31 0 R")),
        ("the Hidden flag (/F 2)", doc("/F 2", "/N 31 0 R")),
        ("the NoView flag (/F 32)", doc("/F 32", "/N 31 0 R")),
        ("Print and Hidden (/F 6)", doc("/F 6", "/N 31 0 R")),
        ("Hidden over a selected state", doc("/F 2 /AS /On", states)),
        ("the state /AS names (/AS /On)", doc("/AS /On", states)),
        ("the selected rollover state", doc("/AS /On", "/R << /On 31 0 R >>")),
        ("the stream itself, no flags", doc("", "/N 31 0 R")),
        ("the rollover appearance stream (/AP /R)", doc("", "/R 31 0 R")),
        ("the down appearance stream (/AP /D)", doc("", "/D 31 0 R")),
        ("a flag that hides nothing (/F 4, Print)", doc("/F 4", "/N 31 0 R")),
        ("Print and a selected state (/F 4 /AS /On)", doc("/F 4 /AS /On", states)),
        ("a selected state dictionary, /AF lists the image too", with_af),
    ] {
        reported_kept_image_too(what, &pdf, &mut red);
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R17c, R18 (lead decisions): optional content is not evaluated, it counts as possibly hidden, so a direct `Do` is
/// silent only when it is not optional content: an image with an `/OC`, and a draw inside marked content opened by `BDC`
/// with the tag `/OC` (from the `BDC` to its `EMC`, across the content streams of the page, the tag also written with a
/// name escape, other marked content nested inside included) leave the region image kept and reported, as does every draw
/// that is not a direct `Do` (an annotation, with or without an `/OC`, a form with or without an `/OC`, a form drawn
/// inside `BDC /OC`). A direct `Do` outside such marked content (none, before, after, a non-optional `BDC`) keeps it
/// silently. Page two draws the image `/Im1` (object 10) or the form `/Fx` (40, which draws it), or lists the annotation
/// 30 whose appearance form (31) draws it.
#[test]
fn painted_source_under_optional_content_is_reported() {
    let text = "BT /F1 12 Tf 72 700 Td (Page two) Tj ET";
    let draw = "q 10 0 0 10 400 100 cm /Im1 Do Q";
    let image = |keys: &str| {
        stream_body(&format!("/Type /XObject /Subtype /Image /Width 18 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 {keys}"), b"SCAN-PIXELS-SECRET")
    };
    let form = |keys: &str| {
        stream_body(&format!("/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /Im1 10 0 R >> >> {keys}"), b"q 100 0 0 100 0 0 cm /Im1 Do Q")
    };
    let doc = |image_keys: &str, form_keys: &str, contents: &[String], annot: Option<&str>| -> Vec<u8> {
        let numbered: Vec<(u32, &String)> = (70u32..).zip(contents.iter()).collect();
        let refs: Vec<String> = numbered.iter().map(|(n, _)| format!("{n} 0 R")).collect();
        let annots = if annot.is_some() { "/Annots [30 0 R]" } else { "" };
        let mut objs: Vec<(u32, Vec<u8>)> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /MediaBox [0 0 612 792] >>".to_vec()),
            (3, b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /XObject << /Im1 10 0 R >> /Font << /F1 6 0 R >> >> >>".to_vec()),
            (4, stream_body("", format!("q 612 0 0 792 0 0 cm /Im1 Do Q BT /F1 12 Tf 72 300 Td (Public) Tj ET {SECRET}").as_bytes())),
            (
                5,
                format!(
                    "<< /Type /Page /Parent 2 0 R /Contents [{}] /Resources << /XObject << /Im1 10 0 R /Fx 40 0 R >> /Properties << /MC0 45 0 R >> /Font << /F1 6 0 R >> >> {annots} >>",
                    refs.join(" ")
                )
                .into_bytes(),
            ),
            (6, HELVETICA.to_vec()),
            (10, image(image_keys)),
            (40, form(form_keys)),
            (45, b"<< /Type /OCG /Name (Layer) >>".to_vec()),
        ];
        objs.extend(numbered.iter().map(|(n, c)| (*n, stream_body("", c.as_bytes()))));
        if let Some(extra) = annot {
            objs.push((30, format!("<< /Type /Annot /Subtype /Stamp /Rect [300 100 400 200] /P 5 0 R {extra} /AP << /N 31 0 R >> >>").into_bytes()));
            objs.push((31, form("")));
        }
        build(&objs)
    };
    let hidden = |s: &str| format!("{s} {text}");
    let mut red: Vec<String> = Vec::new();
    for (what, pdf) in [
        ("an annotation with an /OC", doc("", "", &[text.to_string()], Some("/OC 45 0 R"))),
        ("an image with an /OC drawn by Do", doc("/OC 45 0 R", "", &[hidden(draw)], None)),
        ("a form with an /OC drawn by Do (it draws the image)", doc("", "/OC 45 0 R", &[hidden("/Fx Do")], None)),
        ("Do inside BDC /OC", doc("", "", &[hidden(&format!("/OC /MC0 BDC {draw} EMC"))], None)),
        ("Do inside BDC with the tag written /O#43", doc("", "", &[hidden(&format!("/O#43 /MC0 BDC {draw} EMC"))], None)),
        ("Do inside BDC /OC after other marked content nested in it was closed", doc("", "", &[hidden(&format!("/OC /MC0 BDC /Span BMC EMC {draw} EMC"))], None)),
        ("BDC /OC in the first content stream, Do in the second", doc("", "", &["/OC /MC0 BDC".to_string(), hidden(&format!("{draw} EMC"))], None)),
        ("a form drawn inside BDC /OC", doc("", "", &[hidden("/OC /MC0 BDC /Fx Do EMC")], None)),
        // R18: not a direct Do, so reported without any optional content (these two were silent controls before R18).
        ("a plain form drawn (a form is not a direct Do)", doc("", "", &[hidden("/Fx Do")], None)),
        ("an annotation without an /OC (an appearance is not a direct Do)", doc("", "", &[text.to_string()], Some(""))),
    ] {
        reported_kept_image_too(what, &pdf, &mut red);
    }
    for (what, pdf) in [
        ("a plain draw", doc("", "", &[hidden(draw)], None)),
        ("Do after the BDC /OC sequence is closed", doc("", "", &[hidden(&format!("/OC /MC0 BDC EMC {draw}"))], None)),
        ("Do before a BDC /OC sequence", doc("", "", &[hidden(&format!("{draw} /OC /MC0 BDC EMC"))], None)),
        ("Do inside BDC with another tag", doc("", "", &[hidden(&format!("/Span << /ActualText (x) >> BDC {draw} EMC"))], None)),
        ("BDC /OC closed by EMC in the next content stream, Do after it", doc("", "", &["/OC /MC0 BDC".to_string(), hidden(&format!("EMC {draw}"))], None)),
    ] {
        silent_drawn_image(what, &pdf, &mut red);
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R17 (P1, same class): a draw makes an object certain only when it is the kind that draw needs. An operator that
/// names the image (or an object of the wrong kind) through a resource category it does not belong to, or an
/// object a drawn object points at as its mask, shading or soft-mask group but that is not an image, shading or
/// form, draws nothing: the source stays and is reported.
#[test]
fn painted_source_named_where_it_is_not_drawn_is_reported() {
    let font = "/Font << /F1 6 0 R /F9 10 0 R >>";
    let mut red: Vec<String> = Vec::new();
    for (what, keys, content, more) in [
        ("Tf selects the image listed under /Font", format!("/Resources << {font} >>"), "BT /F9 12 Tf ET", Vec::new()),
        ("gs names the image listed under /ExtGState", "/Resources << /Font << /F1 6 0 R >> /ExtGState << /G9 10 0 R >> >>".to_string(), "/G9 gs", Vec::new()),
        ("sh names the image listed under /Shading", "/Resources << /Font << /F1 6 0 R >> /Shading << /S9 10 0 R >> >>".to_string(), "/S9 sh", Vec::new()),
        (
            "scn names the image listed under /Pattern",
            "/Resources << /Font << /F1 6 0 R >> /Pattern << /P9 10 0 R >> >>".to_string(),
            "/Pattern cs /P9 scn 100 100 50 50 re f",
            Vec::new(),
        ),
        (
            "the /SMask /G group of a drawn graphics state is the image",
            "/Resources << /Font << /F1 6 0 R >> /ExtGState << /G1 41 0 R >> >>".to_string(),
            "/G1 gs",
            vec![(41, b"<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 10 0 R >> >>".to_vec())],
        ),
        (
            "the /Shading of a drawn shading pattern is the image",
            "/Resources << /Font << /F1 6 0 R >> /Pattern << /P1 42 0 R >> >>".to_string(),
            "/Pattern cs /P1 scn 100 100 50 50 re f",
            vec![(42, b"<< /PatternType 2 /Shading 10 0 R >>".to_vec())],
        ),
    ] {
        reported_even_when_an_appearance_draws_it(what, &keys, content, more, &[], &mut red);
    }
    // The /SMask of a drawn image is the vector form (object 11): not an image, so not rendered with it.
    let drawn_image = stream_body("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 11 0 R", &[0u8]);
    let pdf = painted_doc_with(
        "inherited",
        "q 10 0 0 10 400 100 cm /Ix Do Q BT /F1 12 Tf 72 700 Td (Page two) Tj ET",
        "",
        "/Resources << /Font << /F1 6 0 R >> /XObject << /Ix 43 0 R >> >>",
        vec![(43, drawn_image)],
    );
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if out.painted_sources_kept != [11] {
        red.push(format!("the /SMask of a drawn image is a form: painted_sources_kept {:?}, expected [11]", out.painted_sources_kept));
    }
    if out.not_proven_reasons() != [hsk_studio_press::NotProven::PaintedSourceKept] || out.region_flattened() {
        red.push(format!("the /SMask of a drawn image is a form: reasons {:?}, region_flattened {}", out.not_proven_reasons(), out.region_flattened()));
    }
    if gone(&out.bytes, "VECTOR-FORM-SECRET") {
        red.push("the /SMask of a drawn image is a form: the form was dropped (the kept reference would dangle)".into());
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R17 (V-1): a name resolves to ONE resource entry, the nearest in the chain of the scope (own `/Resources` before an
/// ancestor node's, as a viewer that merges them reads it). The page tree has two branches: the flattened page one
/// under node 20, page two under node 21 whose `/Resources` (object 9, shared with page one) lists `/Im1` as the region
/// image (object 10); the flatten step strips the resources of page one's ancestors, not of node 21. Page two lists its
/// own `/Im1` (object 40, a different image) and draws `/Im1`: the draw resolves to 40, so the shadowed ancestor entry
/// (10) is reached, not drawn: it stays and is reported. The control lists the region image under a name nothing
/// shadows (`/Ib`) and draws that name: silent.
#[test]
fn painted_source_shadowed_by_a_nearer_resource_entry_is_reported() {
    let other_image = stream_body("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0u8]);
    let two_branches = |own_xobjects: &str, draw: &str| {
        build(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [20 0 R 21 0 R] /Count 2 /MediaBox [0 0 612 792] >>".to_vec()),
            (20, b"<< /Type /Pages /Parent 2 0 R /Kids [3 0 R] /Count 1 >>".to_vec()),
            (21, b"<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 /Resources 9 0 R >>".to_vec()),
            (3, b"<< /Type /Page /Parent 20 0 R /Contents 4 0 R /Resources 9 0 R >>".to_vec()),
            (4, stream_body("", format!("q 612 0 0 792 0 0 cm /Im1 Do Q q /Fm9 Do Q BT /F1 12 Tf 72 300 Td (Public) Tj ET {SECRET}").as_bytes())),
            (5, format!("<< /Type /Page /Parent 21 0 R /Contents 7 0 R /Resources << /Font << /F1 6 0 R >> /XObject << {own_xobjects} >> >> >>").into_bytes()),
            (6, HELVETICA.to_vec()),
            (7, stream_body("", format!("{draw} BT /F1 12 Tf 72 700 Td (Page two) Tj ET").as_bytes())),
            (9, b"<< /XObject << /Im1 10 0 R /Fm9 11 0 R >> /Font << /F1 6 0 R >> >>".to_vec()),
            (10, stream_body("/Type /XObject /Subtype /Image /Width 18 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"SCAN-PIXELS-SECRET")),
            (11, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /HskMarker (VECTOR-FORM-SECRET)", b"0 g 100 690 50 20 re f")),
            (40, other_image.clone()),
        ])
    };
    let mut red: Vec<String> = Vec::new();
    let shadowed = two_branches("/Im1 40 0 R", "q 10 0 0 10 400 100 cm /Im1 Do Q");
    reported_kept_image("node 21's /Im1 (the region image) is shadowed by page two's own /Im1", &shadowed, &mut red);
    let unshadowed = two_branches("/Im1 40 0 R /Ib 10 0 R", "q 10 0 0 10 400 100 cm /Ib Do Q");
    silent_drawn_image("page two draws the region image under a name nothing shadows", &unshadowed, &mut red);
    assert!(red.is_empty(), "{red:#?}");
}

/// R18 (N1): a stream is read as a dictionary wherever a dictionary is read, so a name can resolve THROUGH a stream placed
/// where a page's `/Resources` value, a `/XObject` category value or an inherited `/Resources` belongs (a viewer reads those
/// as dictionary objects). Page two draws `/Im1` directly (a draw that is silent when the path is dictionaries): through a
/// stream the image stays and is reported. Every case also runs with a dictionary on the path as the control: silent.
#[test]
fn painted_source_named_through_a_stream_used_as_a_dictionary_is_reported() {
    let text = "q 10 0 0 10 400 100 cm /Im1 Do Q BT /F1 12 Tf 72 700 Td (Page two) Tj ET";
    let names = "/XObject << /Im1 10 0 R >> /Font << /F1 6 0 R >>";
    // `image_keys`: more entries of the image dictionary (object 10); `page_two`: more entries of page two; `node`: more
    // entries of the /Pages node; `tree`: the /Kids of the /Pages node and the /Parent of page two; `more`: more objects.
    let doc_in = |image_keys: &str, page_two: &str, node: &str, tree: (&str, u32), page_is_stream: bool, more: Vec<(u32, Vec<u8>)>| -> Vec<u8> {
        let mut objs = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, format!("<< /Type /Pages /Kids [{}] /Count 2 /MediaBox [0 0 612 792] {node} >>", tree.0).into_bytes()),
            (3, b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources 9 0 R >>".to_vec()),
            (4, stream_body("", format!("q 612 0 0 792 0 0 cm /Im1 Do Q q /Fm9 Do Q BT /F1 12 Tf 72 300 Td (Public) Tj ET {SECRET}").as_bytes())),
            (
                5,
                if page_is_stream {
                    stream_body(&format!("/Type /Page /Parent {} 0 R /Contents 7 0 R {page_two}", tree.1), b"x")
                } else {
                    format!("<< /Type /Page /Parent {} 0 R /Contents 7 0 R {page_two} >>", tree.1).into_bytes()
                },
            ),
            (6, HELVETICA.to_vec()),
            (7, stream_body("", text.as_bytes())),
            (9, b"<< /XObject << /Im1 10 0 R /Fm9 11 0 R >> /Font << /F1 6 0 R >> >>".to_vec()),
            (10, stream_body(&format!("/Type /XObject /Subtype /Image /Width 18 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 {image_keys}"), b"SCAN-PIXELS-SECRET")),
            (11, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /HskMarker (VECTOR-FORM-SECRET)", b"0 g 100 690 50 20 re f")),
        ];
        objs.extend(more);
        build(&objs)
    };
    let doc = |image_keys: &str, page_two: &str, node: &str, more: Vec<(u32, Vec<u8>)>| doc_in(image_keys, page_two, node, ("3 0 R 5 0 R", 2), false, more);
    let stream = |dict: &str| stream_body(dict, b"x");
    let dict_names = format!("<< {names} >>").into_bytes();
    let mut red: Vec<String> = Vec::new();
    for (what, through_stream, through_dict) in [
        (
            "page two's /XObject value",
            doc("", "/Resources << /Font << /F1 6 0 R >> /XObject 20 0 R >>", "", vec![(20, stream("/Im1 10 0 R"))]),
            doc("", "/Resources << /Font << /F1 6 0 R >> /XObject 20 0 R >>", "", vec![(20, b"<< /Im1 10 0 R >>".to_vec())]),
        ),
        (
            "page two's /XObject value (the image itself, whose dictionary names it)",
            doc("/Im1 10 0 R", "/Resources << /Font << /F1 6 0 R >> /XObject 10 0 R >>", "", Vec::new()),
            doc("", "/Resources << /Font << /F1 6 0 R >> /XObject 20 0 R >>", "", vec![(20, b"<< /Im1 10 0 R >>".to_vec())]),
        ),
        (
            "page two's /Resources value",
            doc("", "/Resources 20 0 R", "", vec![(20, stream(names))]),
            doc("", "/Resources 20 0 R", "", vec![(20, dict_names.clone())]),
        ),
        (
            "page two's /Resources value (the image itself, whose dictionary holds the resources)",
            doc(names, "/Resources 10 0 R", "", Vec::new()),
            doc("", "/Resources 20 0 R", "", vec![(20, dict_names.clone())]),
        ),
        (
            "the /Resources value inherited from the /Pages node",
            doc("", "", "/Resources 20 0 R", vec![(20, stream(names))]),
            doc("", "", "/Resources 20 0 R", vec![(20, dict_names.clone())]),
        ),
        (
            "the /Resources holder, a page-tree node between page two and the root",
            doc_in(
                "",
                "",
                "",
                ("3 0 R 21 0 R", 21),
                false,
                vec![(21, stream_body("/Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 /Resources 22 0 R", b"x")), (22, dict_names.clone())],
            ),
            doc_in(
                "",
                "",
                "",
                ("3 0 R 21 0 R", 21),
                false,
                vec![(21, b"<< /Type /Pages /Parent 2 0 R /Kids [5 0 R] /Count 1 /Resources 22 0 R >>".to_vec()), (22, dict_names.clone())],
            ),
        ),
        (
            "a page-tree node between page two and the node that holds the /Resources",
            doc_in(
                "",
                "",
                "",
                ("3 0 R 23 0 R", 21),
                false,
                vec![
                    (23, b"<< /Type /Pages /Parent 2 0 R /Kids [21 0 R] /Count 1 /Resources 22 0 R >>".to_vec()),
                    (21, stream_body("/Type /Pages /Parent 23 0 R /Kids [5 0 R] /Count 1", b"x")),
                    (22, dict_names.clone()),
                ],
            ),
            doc_in(
                "",
                "",
                "",
                ("3 0 R 23 0 R", 21),
                false,
                vec![
                    (23, b"<< /Type /Pages /Parent 2 0 R /Kids [21 0 R] /Count 1 /Resources 22 0 R >>".to_vec()),
                    (21, b"<< /Type /Pages /Parent 23 0 R /Kids [5 0 R] /Count 1 >>".to_vec()),
                    (22, dict_names.clone()),
                ],
            ),
        ),
        (
            "the page object itself (with its own /Resources)",
            doc_in("", &format!("/Resources << {names} >>"), "", ("3 0 R 5 0 R", 2), true, Vec::new()),
            doc_in("", &format!("/Resources << {names} >>"), "", ("3 0 R 5 0 R", 2), false, Vec::new()),
        ),
    ] {
        reported_kept_image_too(&format!("{what} is a stream"), &through_stream, &mut red);
        silent_drawn_image(&format!("{what} is a dictionary"), &through_dict, &mut red);
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R18 (N2): any stream with `/Resources` is a content scope, whatever it is. The bytes of an image, a mesh shading or an
/// image mask are not content to any viewer, so what they would name is no draw: the image stays and is reported. The
/// image 20 (a 6 x 1 grey image to a viewer), the mask 22 of the image 21 and the mesh shading 23 each carry `/Resources`
/// naming the region image and hold the bytes `/A Do`.
#[test]
fn painted_source_drawn_by_an_image_or_shading_stream_read_as_content_is_reported() {
    let names = "/Resources << /XObject << /A 10 0 R >> >>";
    let image = |extra: &str| {
        stream_body(&format!("/Type /XObject /Subtype /Image /Width 6 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 {extra}"), b"/A Do ")
    };
    let mesh = stream_body(&format!("/ShadingType 4 /ColorSpace /DeviceGray /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 1 0 1 0 1] {names}"), b"/A Do ");
    let mut red: Vec<String> = Vec::new();
    for (what, keys, content, more) in [
        (
            "an image page two draws carries /Resources",
            "/Resources << /Font << /F1 6 0 R >> /XObject << /Im2 20 0 R >> >>",
            "q 10 0 0 10 400 100 cm /Im2 Do Q",
            vec![(20, image(names))],
        ),
        (
            "the /SMask image of an image page two draws carries /Resources",
            "/Resources << /Font << /F1 6 0 R >> /XObject << /Im2 21 0 R >> >>",
            "q 10 0 0 10 400 100 cm /Im2 Do Q",
            vec![(21, image("/SMask 22 0 R")), (22, image(names))],
        ),
        (
            "a mesh shading page two draws with sh carries /Resources",
            "/Resources << /Font << /F1 6 0 R >> /Shading << /Sh2 23 0 R >> >>",
            "/Sh2 sh",
            vec![(23, mesh)],
        ),
    ] {
        let text = format!("{content} BT /F1 12 Tf 72 700 Td (Page two) Tj ET");
        reported_kept_image_too(what, &painted_doc_with("inherited", &text, "", keys, more), &mut red);
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R18 (N3): `Tf`, `scn`/`SCN` and `gs` select, they do not paint. A Type 3 font selected by `Tf` (no text shown, or text
/// shown: a glyph procedure is not a direct `Do`), a dictionary a `gs` names under `/ExtGState` that is a Type 3 font, a
/// tiling pattern `scn` or `SCN` selects (painting or not) and a soft mask `gs` sets, each holding a draw of the region
/// image in a glyph procedure, a pattern cell or a mask group, leave the image kept and reported.
#[test]
fn painted_source_selected_by_a_type3_font_pattern_or_soft_mask_is_reported() {
    let type3 = |resources: &str| {
        format!(
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [1 0 0 1 0 0] /CharProcs << /a 51 0 R >> /Encoding << /Type /Encoding /Differences [97 /a] >> /FirstChar 97 /LastChar 97 /Widths [1] {resources} >>"
        )
        .into_bytes()
    };
    let glyph = stream_body("", b"1 0 d0 q 1 0 0 1 0 0 cm /Im1 Do Q");
    let own = "/Resources << /XObject << /Im1 10 0 R >> >>";
    let cell = |resources: &str| {
        stream_body(&format!("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 10 10] /XStep 10 /YStep 10 {resources}"), b"q 10 0 0 10 0 0 cm /Im1 Do Q")
    };
    let group = |resources: &str| {
        stream_body(&format!("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Group << /S /Transparency /CS /DeviceGray >> {resources}"), b"q 612 0 0 792 0 0 cm /Im1 Do Q")
    };
    let fonts = "/Font << /F1 6 0 R /F3 50 0 R >>";
    let mut red: Vec<String> = Vec::new();
    for (what, keys, content, more) in [
        ("Tf selects a Type 3 font with /Resources, no text is shown", format!("/Resources << {fonts} >>"), "BT /F3 12 Tf ET", vec![(50, type3(own)), (51, glyph.clone())]),
        (
            "Tf selects a Type 3 font with /Resources and text is shown",
            format!("/Resources << {fonts} >>"),
            "BT /F3 12 Tf 72 600 Td (a) Tj ET",
            vec![(50, type3(own)), (51, glyph.clone())],
        ),
        (
            "Tf selects a Type 3 font without /Resources, its glyph procedure draws with the page's resources",
            format!("/Resources << {fonts} /XObject << /Im1 10 0 R >> >>"),
            "BT /F3 12 Tf 72 600 Td (a) Tj ET",
            vec![(50, type3("")), (51, glyph.clone())],
        ),
        (
            "gs names a Type 3 font dictionary under /ExtGState",
            "/Resources << /Font << /F1 6 0 R >> /ExtGState << /G3 50 0 R >> >>".to_string(),
            "/G3 gs",
            vec![(50, type3(own)), (51, glyph.clone())],
        ),
        (
            "SCN selects a tiling pattern, nothing is painted",
            "/Resources << /Font << /F1 6 0 R >> /Pattern << /P1 52 0 R >> >>".to_string(),
            "/Pattern CS /P1 SCN",
            vec![(52, cell(own))],
        ),
        (
            "scn selects a tiling pattern and a rectangle is filled",
            "/Resources << /Font << /F1 6 0 R >> /Pattern << /P1 52 0 R >> >>".to_string(),
            "/Pattern cs /P1 scn 100 100 50 50 re f",
            vec![(52, cell(own))],
        ),
        (
            "gs sets a soft mask whose group draws it, nothing is painted",
            "/Resources << /Font << /F1 6 0 R >> /ExtGState << /G1 53 0 R >> >>".to_string(),
            "/G1 gs",
            vec![(53, b"<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 54 0 R >> >>".to_vec()), (54, group(own))],
        ),
    ] {
        let text = format!("{content} BT /F1 12 Tf 72 700 Td (Page two) Tj ET");
        reported_kept_image_too(what, &painted_doc_with("inherited", &text, "", &keys, more), &mut red);
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R18 (the minimal draw set): the region image drawn by anything but a `Do` in a kept page's own content is reported: a
/// form page two draws (with `/Resources` of its own, or without: it then names the page's `/Im1`), a tiling pattern it
/// paints with (likewise), a soft-mask group a `gs` sets (likewise). Each case also runs with a direct `Do` of the image
/// added to page two's content: silent, the direct `Do` alone decides.
#[test]
fn painted_source_drawn_by_anything_but_a_kept_pages_own_content_is_reported() {
    let draws = b"q 100 0 0 100 0 0 cm /Im1 Do Q";
    let own = "/Resources << /XObject << /Im1 10 0 R >> >>";
    let form = |resources: &str| stream_body(&format!("/Type /XObject /Subtype /Form /BBox [0 0 100 100] {resources}"), draws);
    let cell = |resources: &str| stream_body(&format!("/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 100 100] /XStep 100 /YStep 100 {resources}"), draws);
    let group = |resources: &str| {
        stream_body(&format!("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Group << /S /Transparency /CS /DeviceGray >> {resources}"), draws)
    };
    let resources = |extra: &str| format!("/Resources << /Font << /F1 6 0 R >> /XObject << /Im1 10 0 R >> {extra} >>");
    let gstate = b"<< /Type /ExtGState /SMask << /Type /Mask /S /Luminosity /G 45 0 R >> >>".to_vec();
    let direct = "q 10 0 0 10 400 100 cm /Im1 Do Q";
    let mut red: Vec<String> = Vec::new();
    let forms = "/Resources << /Font << /F1 6 0 R >> /XObject << /Im1 10 0 R /Fx 40 0 R >> >>".to_string();
    let patterns = resources("/Pattern << /P1 42 0 R >>");
    let gstates = resources("/ExtGState << /G1 44 0 R >>");
    for (what, keys, content, more) in [
        ("a form with /Resources of its own", &forms, "/Fx Do", vec![(40, form(own))]),
        ("a form without /Resources (it names the page's /Im1)", &forms, "/Fx Do", vec![(40, form(""))]),
        ("a tiling pattern with /Resources of its own, painted", &patterns, "/Pattern cs /P1 scn 100 100 50 50 re f", vec![(42, cell(own))]),
        ("a tiling pattern without /Resources, painted", &patterns, "/Pattern cs /P1 scn 100 100 50 50 re f", vec![(42, cell(""))]),
        ("a soft-mask group with /Resources of its own, set by gs and painted through", &gstates, "/G1 gs 100 100 50 50 re f", vec![(44, gstate.clone()), (45, group(own))]),
        ("a soft-mask group without /Resources", &gstates, "/G1 gs 100 100 50 50 re f", vec![(44, gstate.clone()), (45, group(""))]),
    ] {
        let text = format!("{content} BT /F1 12 Tf 72 700 Td (Page two) Tj ET");
        reported_kept_image_too(what, &painted_doc_with("inherited", &text, "", keys, more.clone()), &mut red);
        let with_direct = format!("{content} {direct} BT /F1 12 Tf 72 700 Td (Page two) Tj ET");
        silent_drawn_image(&format!("{what}, and page two also draws the image directly"), &painted_doc_with("inherited", &with_direct, "", keys, more), &mut red);
    }
    // Only an image is silent: the region FORM (object 11) that page two draws directly with `Do` stays and is reported
    // (the image 10 is listed under a name nobody draws, so it is dropped).
    let pdf = painted_doc("inherited", "q /Fm9 Do Q BT /F1 12 Tf 72 700 Td (Page two) Tj ET");
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if out.painted_sources_kept != [11] || out.region_flattened() {
        red.push(format!("a form page two draws directly: painted_sources_kept {:?}, region_flattened {}, expected [11] and false", out.painted_sources_kept, out.region_flattened()));
    }
    if !out.not_proven_reasons().contains(&hsk_studio_press::NotProven::PaintedSourceKept) {
        red.push(format!("a form page two draws directly: not-proven reasons {:?} lack PaintedSourceKept", out.not_proven_reasons()));
    }
    if gone(&out.bytes, "VECTOR-FORM-SECRET") {
        red.push("a form page two draws directly was dropped (the kept reference would dangle)".into());
    }
    if !gone(&out.bytes, "SCAN-PIXELS-SECRET") {
        red.push("the image nobody draws survives".into());
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R19 (N1): a kept page's own content is exclusive, the one place a `Do` can be silent, only when each of its content
/// streams is named by exactly one reference of the file. The marked-content state that guards the silence is carried
/// from stream to stream of the page, and a stream already read in the scope is skipped, so a stream that the page's
/// `/Contents` lists twice, or that a form read first also names, would not feed its `BDC /OC` to the page. Page two
/// (object 5) draws the image `/Im1` (object 10) after such a stream, and has a trailing content stream the strict
/// parser refuses (an unknown operator), so the canonical rewrite leaves its streams as written. Every document below
/// is reported (the image stays, `NotProven::PaintedSourceKept`); each control, with the streams named once, is silent.
/// The last case is the over-report accepted by the rule: a stream two kept pages share, and the balanced form.
#[test]
fn painted_source_is_reported_when_a_content_stream_of_the_page_is_named_elsewhere() {
    let draw = "q 10 0 0 10 400 100 cm /Im1 Do Q";
    let bad = "1 2 3 frob";
    let form = |data: &str| stream_body("/Type /XObject /Subtype /Form /BBox [0 0 10 10]", data.as_bytes());
    // Page two takes `contents` as its /Contents (a second kept page, object 12, takes the same when `third`), the extra
    // names `xobjects` in its /XObject, the content streams `streams` and the extra objects `more`.
    let doc = |contents: &str, third: bool, streams: &[(u32, String)], xobjects: &str, more: Vec<(u32, Vec<u8>)>| -> Vec<u8> {
        let res = format!("<< /XObject << /Im1 10 0 R {xobjects} >> /Properties << /MC0 45 0 R >> /Font << /F1 6 0 R >> >>");
        let kids = if third { "[3 0 R 5 0 R 12 0 R] /Count 3" } else { "[3 0 R 5 0 R] /Count 2" };
        let mut objs: Vec<(u32, Vec<u8>)> = vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, format!("<< /Type /Pages /Kids {kids} /MediaBox [0 0 612 792] >>").into_bytes()),
            (3, b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R /Resources << /XObject << /Im1 10 0 R >> /Font << /F1 6 0 R >> >> >>".to_vec()),
            (4, stream_body("", format!("q 612 0 0 792 0 0 cm /Im1 Do Q BT /F1 12 Tf 72 300 Td (Public) Tj ET {SECRET}").as_bytes())),
            (5, format!("<< /Type /Page /Parent 2 0 R /Contents {contents} /Resources {res} >>").into_bytes()),
            (6, HELVETICA.to_vec()),
            (10, stream_body("/Type /XObject /Subtype /Image /Width 18 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"SCAN-PIXELS-SECRET")),
            (45, b"<< /Type /OCG /Name (Layer) >>".to_vec()),
        ];
        if third {
            objs.push((12, format!("<< /Type /Page /Parent 2 0 R /Contents {contents} /Resources {res} >>").into_bytes()));
        }
        objs.extend(streams.iter().map(|(n, c)| (*n, stream_body("", c.as_bytes()))));
        objs.extend(more);
        build(&objs)
    };
    let s = |n: u32, c: &str| (n, c.to_string());
    let list = |nums: &[u32]| format!("[{}]", nums.iter().map(|n| format!("{n} 0 R")).collect::<Vec<_>>().join(" "));
    let mut red: Vec<String> = Vec::new();
    for (what, pdf) in [
        (
            "a stream the /Contents array lists twice (the first copy opens BDC /OC, the second is skipped as already read)",
            doc(&list(&[70, 70, 71, 72]), false, &[s(70, "/OC /MC0 BDC"), s(71, &format!("EMC {draw}")), s(72, bad)], "", Vec::new()),
        ),
        (
            "the same list as an indirect array object",
            doc("8 0 R", false, &[s(70, "/OC /MC0 BDC"), s(71, &format!("EMC {draw}")), s(72, bad)], "", vec![(8, list(&[70, 70, 71, 72]).into_bytes())]),
        ),
        (
            "a stream that is also a form the page draws before it (the form read first opens BDC /OC and does not close it)",
            doc(&list(&[70, 71, 72, 73]), false, &[s(70, "/Fx Do"), s(72, draw), s(73, bad)], "/Fx 71 0 R", vec![(71, form("/OC /MC0 BDC"))]),
        ),
        (
            "the same with a form that closes its sequence (over-reported: the stream is named twice)",
            doc(&list(&[70, 71, 72, 73]), false, &[s(70, "/Fx Do"), s(72, draw), s(73, bad)], "/Fx 71 0 R", vec![(71, form("/OC /MC0 BDC EMC"))]),
        ),
        ("a single content stream that two kept pages list (over-reported)", doc("70 0 R", true, &[s(70, draw)], "", Vec::new())),
    ] {
        reported_kept_image_too(what, &pdf, &mut red);
    }
    for (what, pdf) in [
        ("a plain page: one content stream, named once", doc("70 0 R", false, &[s(70, draw)], "", Vec::new())),
        (
            "BDC /OC closed by EMC in the next stream, no stream listed twice",
            doc(&list(&[70, 71, 72]), false, &[s(70, "/OC /MC0 BDC"), s(71, &format!("EMC {draw}")), s(72, bad)], "", Vec::new()),
        ),
        // PLR-083, R18 validator V-2: form 80 leaves BDC /OC open and this control expects the page's later Do silent (a nested form starts a fresh marked-content stack, flatten_purge.rs read_scope); pdf.js closes marked content left open when the form stream ends, other viewers UNVERIFIED.
        (
            "a form the page draws is a stream of its own, not a content stream",
            doc(&list(&[70, 71, 72, 73]), false, &[s(70, "/Fx Do"), s(71, "/OC /MC0 BDC EMC"), s(72, draw), s(73, bad)], "/Fx 80 0 R", vec![(80, form("/OC /MC0 BDC"))]),
        ),
    ] {
        silent_drawn_image(what, &pdf, &mut red);
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R19 (N2): the report lists painted sources only (image, form, pattern, shading). A font or a graphics state of the
/// flattened page that stays because a kept page names it (`Tf`, `gs`) is not one: nothing is reported and
/// `region_flattened` stays true. Control: when page two names neither, both are dropped from the file with the rest of
/// the old page's sources, so they are sources of the flattened page that are pruned when named by no kept page, and
/// unreported when one does.
#[test]
fn a_font_or_graphics_state_that_stays_is_not_a_painted_source() {
    let doc = |page_two: &str| -> Vec<u8> {
        build(&[
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /MediaBox [0 0 612 792] /Resources << /XObject << /Im1 10 0 R >> /Font << /F1 6 0 R >> /ExtGState << /G1 30 0 R >> >> >>".to_vec()),
            (3, b"<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>".to_vec()),
            (4, stream_body("", format!("q 612 0 0 792 0 0 cm /Im1 Do Q /G1 gs BT /F1 12 Tf 72 300 Td (Public) Tj ET {SECRET}").as_bytes())),
            (5, b"<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>".to_vec()),
            (6, HELVETICA.to_vec()),
            (7, stream_body("", page_two.as_bytes())),
            (10, stream_body("/Type /XObject /Subtype /Image /Width 18 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"SCAN-PIXELS-SECRET")),
            (30, b"<< /Type /ExtGState /LW 2 /HskMarker (GSTATE-MARKER) >>".to_vec()),
        ])
    };
    let mut red: Vec<String> = Vec::new();
    let pdf = doc("/G1 gs BT /F1 12 Tf 72 700 Td (Page two) Tj ET");
    let mut open = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut open, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if !out.painted_sources_kept.is_empty() || !out.region_flattened() {
        red.push(format!("a font and a graphics state page two names: painted_sources_kept {:?}, region_flattened {}", out.painted_sources_kept, out.region_flattened()));
    }
    if out.not_proven_reasons().contains(&hsk_studio_press::NotProven::PaintedSourceKept) {
        red.push("a font and a graphics state page two names: PaintedSourceKept".into());
    }
    if gone(&out.bytes, "GSTATE-MARKER") || gone(&out.bytes, "Helvetica") {
        red.push("the font or the graphics state page two names was dropped".into());
    }
    if !gone(&out.bytes, "SCAN-PIXELS-SECRET") {
        red.push("the image nobody draws survives".into());
    }
    let pdf = doc("0 g 10 10 20 20 re f");
    let mut open = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut open, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if !gone(&out.bytes, "GSTATE-MARKER") || !gone(&out.bytes, "Helvetica") {
        red.push("control: a font and a graphics state no kept page names survive".into());
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R14-P1, R16, R17, R18, R19 (V4-07): the DESCRIPTOR sentences about what happens to a painted source state the minimal draw
/// set and the reported cases exactly, and cite tests that exist. No sentence of the earlier rules (an annotation
/// appearance a viewer draws, the mask, shading or soft-mask group rendered with a drawn object, the draws of nested
/// content) is left in it.
#[test]
fn descriptor_names_the_painted_source_kept_by_reference_case() {
    let source = include_str!("flatten_r13.rs");
    let cited_tests = [
        "painted_sources_under_a_region_do_not_survive",
        "painted_source_drawn_directly_by_a_kept_page_is_not_reported",
        "painted_source_referenced_by_a_kept_catalog_structure_is_reported",
        "painted_source_referenced_but_not_drawn_by_a_kept_page_is_reported",
        "painted_source_referenced_by_a_key_of_a_drawn_object_is_reported",
        "painted_source_placed_as_a_resources_value_is_reported",
        "painted_source_placed_as_a_gated_sub_dictionary_is_reported",
        "painted_source_placed_as_an_appearance_state_is_reported",
        "painted_source_named_where_it_is_not_drawn_is_reported",
        "painted_source_shadowed_by_a_nearer_resource_entry_is_reported",
        "painted_source_in_an_annotation_appearance_is_reported",
        "painted_source_under_optional_content_is_reported",
        "painted_source_named_through_a_stream_used_as_a_dictionary_is_reported",
        "painted_source_drawn_by_an_image_or_shading_stream_read_as_content_is_reported",
        "painted_source_selected_by_a_type3_font_pattern_or_soft_mask_is_reported",
        "painted_source_drawn_by_anything_but_a_kept_pages_own_content_is_reported",
        "painted_sources_rendered_with_a_drawn_image_are_reported",
        "painted_source_is_reported_when_a_content_stream_of_the_page_is_named_elsewhere",
        "a_font_or_graphics_state_that_stays_is_not_a_painted_source",
    ];
    for needle in [
        "R14-P1",
        "R16",
        "R17",
        "R18",
        "R19",
        "the minimal draw set",
        "a painted source that stays is silent only when a kept page draws it with Do directly in its own content",
        "a painted source (image, form, pattern, shading) that stays is silent only under the minimal draw set",
        "each of them named by exactly one reference of the file (R19: a stream the /Contents lists twice, or that anything else refers to, such as a form or another page, makes the page's content non-exclusive and nothing it draws is silent)",
        "a font or graphics state that stays because a kept page names it is not a painted source and is not reported",
        "an image XObject stream without an /OC that a kept page draws with Do directly in its own content stream(s)",
        "outside BDC /OC marked content (from the BDC to its matching EMC, the nesting of BMC, BDC and EMC tracked per operator across the streams of the page; optional content is not evaluated and counts as possibly hidden)",
        "resolved through the page's own nearest resource entry (the nearest entry of the name in /Resources /XObject of the page and of its ancestors; an entry a nearer entry of the same name shadows is reached, not drawn)",
        "with a dictionary object, not a stream, as the page, every ancestor node, the /Resources value and the /XObject value on the path",
        "every other painted source that stays is not pruned and is reported",
        "every other painted source that stays is reported",
        "a kept object only references it or places it where a viewer draws nothing from it (a catalog /AF, a /Names or /Dests name tree, a kept page's /Resources entry outside /XObject /Pattern /Shading /ExtGState /Font, a plain key of an object a kept page draws, an object used as a /Resources value or as a gated sub-dictionary, a name an operator uses for an object of another kind, any plain reference)",
        "or a viewer draws it through a form, tiling pattern, Type 3 glyph procedure, soft-mask group, image mask, shading or annotation appearance",
        "or because a viewer draws it through anything but that direct Do (R18, R19: a page whose content streams are named elsewhere, a form, tiling pattern, Type 3 glyph procedure, soft-mask group, image mask, shading or annotation appearance, a Tf, scn, SCN or gs that selects without painting, a name an operator uses for an object of another kind)",
        "over-reporting is accepted",
    ] {
        assert!(hsk_studio_press::DESCRIPTOR.contains(needle), "DESCRIPTOR lacks {needle:?}");
    }
    for cited in cited_tests {
        assert!(hsk_studio_press::DESCRIPTOR.contains(cited), "DESCRIPTOR does not cite {cited}");
        assert!(source.contains(&format!("fn {cited}()")), "the cited test {cited} does not exist");
    }
    for stale in [
        "an annotation appearance a kept page lists, draws them",
        "the image /SMask and /Mask of a drawn image",
        "the /Shading of a drawn shading pattern",
        "none without a matching /AS",
        "Invisible, Hidden or NoView",
        "each of the kind its operator draws",
        "dropped from the file unless a kept page draws them (redaction.flatten.purge",
        "a source that stays is silent only",
        "every other source that stays",
    ] {
        assert!(!hsk_studio_press::DESCRIPTOR.contains(stale), "DESCRIPTOR still says {stale:?}");
    }
}

/// PLR-084 (lead decision 2026-10-12T05:10:24Z; D27 "narrow": no visibility analysis is added): the silence of the minimal
/// draw set is syntactic. Page two draws the region image (object 10) directly with `Do` in its own content, and the draw
/// cannot be seen: a zero-scale `cm`, an empty clip, an alpha of 0 set by `gs`, a later object painted over it. The image
/// stays and nothing is reported in each case (the residual the DESCRIPTOR states). Control: the same invisible draw
/// inside `BDC /OC` marked content is reported, so the harness tells a silent draw from a reported one.
#[test]
fn silent_painted_source_visibility_is_not_checked() {
    let text = "BT /F1 12 Tf 72 700 Td (Page two) Tj ET";
    let own = "/Resources << /Font << /F1 6 0 R >> /XObject << /Im1 10 0 R >> /ExtGState << /G0 50 0 R >> >>";
    let doc = |content: &str| painted_doc_with("inherited", &format!("{content} {text}"), "", own, vec![(50, b"<< /Type /ExtGState /ca 0 /CA 0 >>".to_vec())]);
    let mut red: Vec<String> = Vec::new();
    for (what, draw) in [
        ("a zero-scale cm", "q 0 0 0 0 0 0 cm /Im1 Do Q"),
        ("an empty clip", "q 0 0 0 0 re W n /Im1 Do Q"),
        ("an alpha of 0 set by gs", "q /G0 gs 10 0 0 10 400 100 cm /Im1 Do Q"),
        ("a later object painted over it", "q 10 0 0 10 400 100 cm /Im1 Do Q 1 g 390 90 30 30 re f"),
    ] {
        silent_drawn_image(what, &doc(draw), &mut red);
    }
    reported_kept_image_too("control: the zero-scale draw inside BDC /OC", &doc("/OC << /Type /OCG >> BDC q 0 0 0 0 0 0 cm /Im1 Do Q EMC"), &mut red);
    assert!(red.is_empty(), "{red:#?}");
}

/// PLR-084 (V4-07): the DESCRIPTOR states the residual of the minimal draw set (silence is syntactic, the visibility of the
/// direct draw is not checked) where it defines silence (`redaction.flatten.painted_over`, `redaction.flatten.purge`) and in
/// the list of limitations, cites the test that shows it, and the sentence holds none of the words V4-07 reserves for claims
/// with a proof (never, always, guarantees, proves).
#[test]
fn descriptor_states_that_painted_source_silence_is_syntactic() {
    let residual = "silence is syntactic: it means a kept page's own content stream draws the image directly with Do, and the visibility of that draw (a zero-scale cm, an empty clip, an alpha of 0, a later object painted over it) is not checked, so a silent source can be invisible or covered on the kept page (PLR-084, test silent_painted_source_visibility_is_not_checked)";
    let descriptor = hsk_studio_press::DESCRIPTOR;
    assert_eq!(descriptor.matches(residual).count(), 3, "the residual is stated once in each of three keys");
    for (key, from, to) in [
        ("redaction.flatten.painted_over", "\"painted_over\":", "\"renderer_differential\":"),
        ("redaction.flatten.purge", "\"purge\":", "\"removes\":"),
        ("proof.limitations", "\"limitations\":", "\"scope\":"),
    ] {
        let start = descriptor.find(from).unwrap_or_else(|| panic!("DESCRIPTOR lacks the key {key}"));
        let end = start + descriptor[start..].find(to).unwrap_or_else(|| panic!("DESCRIPTOR has no key after {key}"));
        assert!(descriptor[start..end].contains(residual), "{key} does not state the residual");
    }
    let cited = "silent_painted_source_visibility_is_not_checked";
    assert!(include_str!("flatten_r13.rs").contains(&format!("fn {cited}()")), "the cited test {cited} does not exist");
    for word in ["never", "always", "guarantee", "proves"] {
        assert!(!residual.contains(word), "the residual sentence says {word:?}");
    }
}

/// R13-P1 (lead decision: no structure content, no widgets): the `/Alt` of a structure element on the
/// flattened page and the value of a widget of that page (outside the region) are gone; the structure of
/// the other page stays. R23 FIX_FIRST -> STRUCTURE TREE = NOT PROVEN: the document is tagged, so the save reads not proven
/// for exactly that tree, counted once (`residual_in_region` 1) although the purge read it all and it holds two elements.
#[test]
fn structure_and_widgets_of_flattened_pages_are_removed() {
    let pdf = build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 80 0 R /MarkInfo << /Marked true >> /AcroForm << /Fields [70 0 R] /DR << /Font << /F1 6 0 R >> >> >> >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> >> /StructParents 0 /Annots [70 0 R] >>".to_vec()),
        (4, stream_body("", format!("/Figure << /MCID 0 >> BDC 0 g 300 300 100 100 re f EMC {SECRET}").as_bytes())),
        (5, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R /Resources << /Font << /F1 6 0 R >> >> /StructParents 1 >>".to_vec()),
        (6, HELVETICA.to_vec()),
        (7, stream_body("", b"/P << /MCID 0 >> BDC BT /F1 12 Tf 72 700 Td (Page two) Tj ET EMC")),
        (70, b"<< /Type /Annot /Subtype /Widget /Rect [400 100 500 120] /FT /Tx /T (name) /V (WIDGET-VALUE-SECRET) /P 3 0 R /AP << /N 71 0 R >> >>".to_vec()),
        (71, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 6 0 R >> >>", b"BT /F1 10 Tf 2 5 Td (WIDGET-VALUE-SECRET) Tj ET")),
        (80, b"<< /Type /StructTreeRoot /K [81 0 R 82 0 R] /ParentTree 83 0 R >>".to_vec()),
        (81, b"<< /Type /StructElem /S /Figure /P 80 0 R /Pg 3 0 R /Alt (ALT-SECRET-PHOTO) /K 0 >>".to_vec()),
        (82, b"<< /Type /StructElem /S /P /P 80 0 R /Pg 5 0 R /ActualText (KEPT-STRUCT-TEXT) /K 0 >>".to_vec()),
        (83, b"<< /Nums [0 [81 0 R] 1 [82 0 R]] >>".to_vec()),
    ]);
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    let mut red: Vec<String> = Vec::new();
    for marker in ["ALT-SECRET-PHOTO", "WIDGET-VALUE-SECRET"] {
        if !gone(&out.bytes, marker) {
            red.push(format!("{marker} survives"));
        }
    }
    if gone(&out.bytes, "KEPT-STRUCT-TEXT") {
        red.push("the kept page's structure text was removed".into());
    }
    if out.structure_elements_neutralized != 1 || out.flattened_annotations_removed != 1 { // R13-new-api
        red.push(format!("neutralized {} annotations {}", out.structure_elements_neutralized, out.flattened_annotations_removed)); // R13-new-api
    } // R13-new-api
    if out.region_flattened() || out.residual_in_region != 1 || out.not_proven_reasons() != [hsk_studio_press::NotProven::ResidualInRegion] {
        red.push(format!("region_flattened {} residual {} reasons {:?}, expected the tree reported once", out.region_flattened(), out.residual_in_region, out.not_proven_reasons()));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// PLR-087 (V4-07): the overview of `flatten.rs` says what happens to the annotations and the structure elements of a
/// flattened page; this test shows each part. Page one (flattened) lists annotations 70 and 71; the catalog names 70 by a
/// plain key and in an array (kept objects that reference it); page two lists 71 in its own `/Annots`. Result: 70 leaves the
/// file and both references to it are removed (key dropped, array element nulled); 71 stays with its appearance. The
/// structure element of page one is not dropped: it stays in the tree without its `/Alt`, the element of page two keeps its text.
/// R23 FIX_FIRST -> STRUCTURE TREE = NOT PROVEN: the tagged document reads not proven for the tree (`residual_in_region` 1).
#[test]
fn flattened_page_annotations_leave_and_structure_elements_stay() {
    let pdf = build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 80 0 R /MarkInfo << /Marked true >> /HskNote 70 0 R /HskList [70 0 R 6 0 R] >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> >> /StructParents 0 /Annots [70 0 R 71 0 R] >>".to_vec()),
        (4, stream_body("", format!("/Figure << /MCID 0 >> BDC 0 g 300 300 100 100 re f EMC {SECRET}").as_bytes())),
        (5, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R /Resources << /Font << /F1 6 0 R >> >> /StructParents 1 /Annots [71 0 R] >>".to_vec()),
        (6, HELVETICA.to_vec()),
        (7, stream_body("", b"/P << /MCID 0 >> BDC BT /F1 12 Tf 72 700 Td (Page two) Tj ET EMC")),
        (70, b"<< /Type /Annot /Subtype /FreeText /Rect [400 100 500 120] /P 3 0 R /AP << /N 72 0 R >> >>".to_vec()),
        (71, b"<< /Type /Annot /Subtype /FreeText /Rect [400 200 500 220] /P 5 0 R /AP << /N 73 0 R >> >>".to_vec()),
        (72, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 6 0 R >> >>", b"BT /F1 10 Tf 2 5 Td (ANNOT-GONE-SECRET) Tj ET")),
        (73, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 6 0 R >> >>", b"BT /F1 10 Tf 2 5 Td (ANNOT-KEPT-TEXT) Tj ET")),
        (80, b"<< /Type /StructTreeRoot /K [81 0 R 82 0 R] /ParentTree 83 0 R >>".to_vec()),
        (81, b"<< /Type /StructElem /S /Figure /P 80 0 R /Pg 3 0 R /Alt (ALT-SECRET-PHOTO) /K 0 >>".to_vec()),
        (82, b"<< /Type /StructElem /S /P /P 80 0 R /Pg 5 0 R /ActualText (KEPT-STRUCT-TEXT) /K 0 >>".to_vec()),
        (83, b"<< /Nums [0 [81 0 R] 1 [82 0 R]] >>".to_vec()),
    ]);
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    let bytes = &out.bytes;
    let mut red: Vec<String> = Vec::new();
    if !gone(bytes, "ANNOT-GONE-SECRET") {
        red.push("the annotation a kept catalog references survives".into());
    }
    if find(bytes, b"/HskNote").is_some() {
        red.push("the catalog key that named the removed annotation stays".into());
    }
    match find(bytes, b"/HskList") {
        Some(at) => {
            let tail = &bytes[at..bytes.len().min(at + 48)];
            if find(tail, b"70 0 R").is_some() {
                red.push(format!("the catalog array still references the removed annotation: {}", String::from_utf8_lossy(tail)));
            }
        }
        None => red.push("the catalog array disappeared".into()),
    }
    if gone(bytes, "ANNOT-KEPT-TEXT") {
        red.push("the annotation a kept page lists in its own /Annots was removed".into());
    }
    if out.flattened_annotations_removed != 1 {
        red.push(format!("annotations removed {}", out.flattened_annotations_removed));
    }
    if !gone(bytes, "ALT-SECRET-PHOTO") {
        red.push("the /Alt of the structure element of the flattened page survives".into());
    }
    if gone(bytes, "KEPT-STRUCT-TEXT") {
        red.push("the structure text of the kept page was removed".into());
    }
    let elements = bytes.windows(b"/StructElem".len()).filter(|w| *w == b"/StructElem").count();
    if elements != 2 {
        red.push(format!("{elements} structure element(s) in the saved file, expected both"));
    }
    if out.structure_elements_neutralized != 1 {
        red.push(format!("neutralized {}", out.structure_elements_neutralized));
    }
    if out.region_flattened() || out.residual_in_region != 1 || out.not_proven_reasons() != [hsk_studio_press::NotProven::ResidualInRegion] {
        red.push(format!("region_flattened {} residual {} reasons {:?}, expected the tree reported once", out.region_flattened(), out.residual_in_region, out.not_proven_reasons()));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// PLR-087, R20-O3 (V4-07): the DESCRIPTOR states the condition `region_cleared` checks for painted sources
/// (`painted_sources_kept` empty) without the looser clause "kept unread", and its `purge` key opens with the category
/// wording (annotations and widgets leave the file, structure elements stay neutralized), not with "dropped from the file".
#[test]
fn descriptor_states_the_painted_source_condition_and_the_purge_categories() {
    let descriptor = hsk_studio_press::DESCRIPTOR;
    for needle in [
        "redaction.flatten.region_clearing; painted_sources_kept empty, R13-F6)",
        "what described or drew the old page is purged by category (flatten_purge.rs): R13-F2",
    ] {
        assert_eq!(descriptor.matches(needle).count(), 1, "DESCRIPTOR lacks {needle:?}");
    }
    for stale in ["kept unread", "what described or drew the old page is dropped from the file"] {
        assert!(!descriptor.contains(stale), "DESCRIPTOR still says {stale:?}");
    }
}

/// PLR-094 fixture (lead decision 2026-10-12T07:26:27Z; since R23 FIX_FIRST every tagged document is reported): the catalog holds its
/// `/StructTreeRoot` as `tree`, here in a spelling the purge does not edit (flatten_purge.rs `edited_structure_root`: only an
/// indirect reference to a dictionary is edited). Page one (index 0, with the region) is flattened, page two is kept; `objects`
/// are the structure objects (and any extra), `nums` the `/Nums` array of parent tree 83 (page one is `/StructParents 0`, page
/// two `/StructParents 1`).
fn direct_tree_output(tree: &str, objects: &[(u32, String)], nums: &str) -> hsk_studio_press::RedactedOutput {
    tree_output_with(&quick(), tree, objects, nums)
}

/// `direct_tree_output` under the limits `limits` (the same fixture; the sibling tests below pass `80 0 R` as `tree` and the root
/// dictionary as object 80, which is the one spelling the purge edits).
fn tree_output_with(limits: &Limits, tree: &str, objects: &[(u32, String)], nums: &str) -> hsk_studio_press::RedactedOutput {
    let mut all = vec![(83, format!("<< /Nums {nums} >>"))];
    all.extend(objects.iter().cloned());
    pages_output_with(limits, &format!("/StructTreeRoot {tree} /MarkInfo << /Marked true >>"), "/StructParents 0", "/StructParents 1", &all)
}

/// The fixture family of the structure tests: page one (index 0, with the region `R` and `SECRET`) is flattened, page two is kept.
/// `catalog_extra`, `page_one_extra` and `page_two_extra` are appended to the catalog and the two page dictionaries (the structure
/// keys of a tagged document, or none) and `objects` are further objects of the file.
fn pages_output_with(limits: &Limits, catalog_extra: &str, page_one_extra: &str, page_two_extra: &str, objects: &[(u32, String)]) -> hsk_studio_press::RedactedOutput {
    let pdf = pages_pdf(catalog_extra, page_one_extra, page_two_extra, objects);
    let mut doc = PdfDoc::open(&pdf, limits, &token()).expect("open");
    redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact")
}

/// The input file of `pages_output_with`, for a save with a different set of regions (PLR-101: none).
fn pages_pdf(catalog_extra: &str, page_one_extra: &str, page_two_extra: &str, objects: &[(u32, String)]) -> Vec<u8> {
    let mut objs: Vec<(u32, Vec<u8>)> = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes()),
        (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec()),
        (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> >> {page_one_extra} >>").into_bytes()),
        (4, stream_body("", format!("/Figure << /MCID 0 >> BDC 0 g 300 300 100 100 re f EMC {SECRET}").as_bytes())),
        (5, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R /Resources << /Font << /F1 6 0 R >> >> {page_two_extra} >>").into_bytes()),
        (6, HELVETICA.to_vec()),
        (7, stream_body("", b"/P << /MCID 0 >> BDC BT /F1 12 Tf 72 700 Td (Page two) Tj ET EMC")),
    ];
    objs.extend(objects.iter().map(|(n, body)| (*n, body.clone().into_bytes())));
    build(&objs)
}

const DIRECT_TEXTS: [&str; 3] = ["ALT-DIRECT-SECRET", "ACTUAL-DIRECT-SECRET", "EXPANSION-DIRECT-SECRET"];
const DIRECT_TEXT_KEYS: &str = "/Alt (ALT-DIRECT-SECRET) /ActualText (ACTUAL-DIRECT-SECRET) /E (EXPANSION-DIRECT-SECRET)";

/// PLR-094 (FAIL CLOSED; probe and fix), kept under STRUCTURE TREE = NOT PROVEN: the catalog holds the structure tree root in a
/// spelling the purge does not edit (a direct dictionary, or a chain of references) and an element of it carries `/Alt`,
/// `/ActualText` and `/E` text but no kept page: the text stays in the file, so the save reports it (`residual_in_region`,
/// `NotProven::ResidualInRegion`, `region_flattened` false). These 12 inputs (a `/Pg` that names nothing, a decoy comment, a key
/// in lower case, a name escape, an element only the parent tree or id tree reaches, a root behind a chain of references) were
/// the adversarial cases of the per-element rule that R23 replaced; they stay as inputs of the tree rule.
#[test]
fn direct_structure_tree_elements_off_the_kept_pages_are_reported() {
    let tree = "<< /Type /StructTreeRoot /K [81 0 R] /ParentTree 83 0 R >>".to_string();
    let element = |pg: &str| format!("<< /Type /StructElem /S /Figure {pg} {DIRECT_TEXT_KEYS} /K 0 >>");
    let kept = "<< /Type /StructElem /S /P /Pg 5 0 R /ActualText (KEPT-DIRECT-TEXT) /K 1 >>".to_string();
    let cases = vec![
        ("an element without /Pg", tree.clone(), vec![(81, element(""))], "[0 [81 0 R]]"),
        ("a /Pg that names no object", tree.clone(), vec![(81, element("/Pg 99 0 R"))], "[0 [81 0 R]]"),
        ("a /Pg that names an object that is not a page", tree.clone(), vec![(81, element("/Pg 6 0 R"))], "[0 [81 0 R]]"),
        ("a /Pg that names the flattened page", tree.clone(), vec![(81, element("/Pg 3 0 R"))], "[0 [81 0 R]]"),
        // V4-08: the page is read from the parsed dictionary, not from raw text (comment decoy, key case, name escape).
        ("a /Pg that only a comment names", tree.clone(), vec![(81, format!("<< /Type /StructElem /S /Figure % /Pg 5 0 R\n {DIRECT_TEXT_KEYS} /K 0 >>"))], "[0 [81 0 R]]"),
        ("a /pg key in lower case", tree.clone(), vec![(81, element("/pg 5 0 R"))], "[0 [81 0 R]]"),
        ("an element whose /S is spelled with a name escape", tree.clone(), vec![(81, format!("<< /Type /StructElem /#53 /Figure {DIRECT_TEXT_KEYS} /K 0 >>"))], "[0 [81 0 R]]"),
        (
            "an element only the parent tree reaches",
            "<< /Type /StructTreeRoot /K [82 0 R] /ParentTree 83 0 R >>".to_string(),
            vec![(81, element("")), (82, kept.clone())],
            "[0 [81 0 R] 1 [82 0 R]]",
        ),
        (
            "an element only the id tree reaches",
            "<< /Type /StructTreeRoot /K [82 0 R] /IDTree << /Names [(a) 81 0 R] >> >>".to_string(),
            vec![(81, element("")), (82, kept.clone())],
            "[1 [82 0 R]]",
        ),
        (
            "a direct dictionary in /K",
            format!("<< /Type /StructTreeRoot /K [ << /Type /StructElem /S /Figure {DIRECT_TEXT_KEYS} /K 0 >> ] /ParentTree 83 0 R >>"),
            vec![],
            "[]",
        ),
        (
            "a child whose own /Pg overrides its parent's",
            "<< /Type /StructTreeRoot /K [82 0 R] /ParentTree 83 0 R >>".to_string(),
            vec![(82, "<< /Type /StructElem /S /Document /Pg 5 0 R /K [81 0 R] >>".to_string()), (81, element("/Pg 99 0 R"))],
            "[0 [81 0 R]]",
        ),
        (
            "a root reached through a chain of references",
            "80 0 R".to_string(),
            vec![(80, "84 0 R".to_string()), (84, tree.clone()), (81, element(""))],
            "[0 [81 0 R]]",
        ),
    ];
    let mut red: Vec<String> = Vec::new();
    for (name, tree, objects, nums) in cases {
        let out = direct_tree_output(&tree, &objects, nums);
        let remaining = DIRECT_TEXTS.iter().filter(|t| !gone(&out.bytes, t)).count();
        if remaining != DIRECT_TEXTS.len() {
            red.push(format!("{name}: only {remaining} of {} texts remain (the case needs the tree unedited)", DIRECT_TEXTS.len()));
        }
        if out.region_flattened() {
            red.push(format!("{name}: region_flattened true while the structure text survives (residual {})", out.residual_in_region));
        }
        let reasons = out.not_proven_reasons();
        if reasons != [hsk_studio_press::NotProven::ResidualInRegion] {
            red.push(format!("{name}: reasons {reasons:?}, expected only ResidualInRegion"));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// PLR-094, the other side of the per-element rule that R23 replaced: a catalog-direct structure tree whose every element sits on
/// the kept page (its own `/Pg`, or the nearest ancestor element's) is reported too, because the document is tagged
/// (`region_flattened` false, `NotProven::ResidualInRegion` only, `residual_in_region` 1); the unedited text stays.
#[test]
fn direct_structure_tree_on_kept_pages_is_reported_too() {
    let objects = vec![
        (82, "<< /Type /StructElem /S /Document /Pg 5 0 R /K [84 0 R 85 0 R 86 0 R] >>".to_string()),
        // V4-08: the key is spelled with a name escape and the reference is spread over lines and a tab.
        (86, "<< /Type /StructElem /S /P /P#67\n 5 \t 0\nR /ActualText (KEPT-ESCAPED-TEXT) /K 1 >>".to_string()),
        (84, "<< /Type /StructElem /S /P /ActualText (KEPT-INHERITED-TEXT) /K 0 >>".to_string()),
        (85, "<< /Type /StructElem /S /P /Pg 5 0 R /Alt (KEPT-OWN-TEXT) /K 1 >>".to_string()),
    ];
    let out = direct_tree_output("<< /Type /StructTreeRoot /K [82 0 R] /ParentTree 83 0 R >>", &objects, "[1 [84 0 R 85 0 R 86 0 R]]");
    let mut red: Vec<String> = Vec::new();
    for text in ["KEPT-INHERITED-TEXT", "KEPT-OWN-TEXT", "KEPT-ESCAPED-TEXT"] {
        if gone(&out.bytes, text) {
            red.push(format!("{text} was removed (the tree is not edited)"));
        }
    }
    if out.structure_elements_neutralized != 0 {
        red.push(format!("neutralized {}", out.structure_elements_neutralized));
    }
    if out.region_flattened() || out.residual_in_region != 1 || out.not_proven_reasons() != [hsk_studio_press::NotProven::ResidualInRegion] {
        red.push(format!("region_flattened {} reasons {:?} residual {}, expected the tree reported once", out.region_flattened(), out.not_proven_reasons(), out.residual_in_region));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// A structure element that holds `/Alt`, `/ActualText` and `/E` text (`DIRECT_TEXT_KEYS`); `pg` is its `/Pg` entry (or empty) and
/// `k` its `/K` value.
fn text_element(pg: &str, k: &str) -> String {
    format!("<< /Type /StructElem /S /Figure {pg} {DIRECT_TEXT_KEYS} /K {k} >>")
}

const INDIRECT_ROOT: &str = "<< /Type /StructTreeRoot /K [81 0 R] /ParentTree 83 0 R >>";
const INDIRECT_ROOT_ARRAY: &str = "<< /Type /StructTreeRoot /K 85 0 R /ParentTree 83 0 R >>";

/// The catalog names the root as `80 0 R` (an indirect reference to a dictionary: the one spelling `neutralize_structure` edits).
fn indirect_root_output(limits: &Limits, root: &str, objects: &[(u32, String)], nums: &str) -> hsk_studio_press::RedactedOutput {
    let mut all = vec![(80, root.to_string())];
    all.extend(objects.iter().cloned());
    tree_output_with(limits, "80 0 R", &all, nums)
}

/// PLR-094 sibling (lead decision 2026-10-12T07:54:54Z, FAIL CLOSED; V4-06 site flatten_purge.rs `element` and the root `/K` read),
/// kept under STRUCTURE TREE = NOT PROVEN: `neutralize_structure` reads a `/K` that is a direct array or one element, not a `/K`
/// that is a reference to an array, so the elements under such an array are not edited. Probe A (the first case) was fail-open
/// before the sibling fix: a parent whose `/Pg` is the flattened page is neutralized (its `/Pg` leaves the file) while the child
/// under its indirect `/K` array keeps `/Alt`, `/ActualText` and `/E`. The text stays in the file (the tree is not edited here)
/// and the save reports the tree: `region_flattened` false, `NotProven::ResidualInRegion` only. Each case also checks that the
/// text survived (the case needs it) and the number of elements the purge neutralized.
#[test]
fn indirect_array_k_elements_off_the_kept_pages_are_reported() {
    let parent = |k: &str| format!("<< /Type /StructElem /S /Document /Pg 3 0 R /K {k} >>");
    let array = |members: &str| format!("[{members}]");
    // (case, root dictionary, objects, parent-tree /Nums, elements the purge neutralized)
    type Case = (&'static str, &'static str, Vec<(u32, String)>, &'static str, u32);
    let cases: Vec<Case> = vec![
        (
            "probe A: a parent on the flattened page, its /K an indirect array holding an element with text",
            INDIRECT_ROOT,
            vec![(81, parent("85 0 R")), (85, array("86 0 R")), (86, text_element("", "0"))],
            "[]",
            1,
        ),
        ("an element without /Pg in the root's indirect /K array", INDIRECT_ROOT_ARRAY, vec![(85, array("81 0 R")), (81, text_element("", "0"))], "[]", 0),
        (
            "a /Pg that names no object, in an indirect /K array",
            INDIRECT_ROOT_ARRAY,
            vec![(85, array("81 0 R")), (81, text_element("/Pg 99 0 R", "0"))],
            "[]",
            0,
        ),
        (
            "an indirect array under an element that is itself under a direct array",
            INDIRECT_ROOT,
            vec![(81, parent("[86 0 R]")), (86, "<< /Type /StructElem /S /Sect /K 85 0 R >>".to_string()), (85, array("87 0 R")), (87, text_element("", "0"))],
            "[]",
            1,
        ),
        (
            "an indirect array behind a chain of references",
            INDIRECT_ROOT,
            vec![(81, parent("84 0 R")), (84, "85 0 R".to_string()), (85, array("86 0 R")), (86, text_element("", "0"))],
            "[]",
            1,
        ),
        (
            "an array reference listed inside a direct /K array",
            INDIRECT_ROOT,
            vec![(81, parent("[85 0 R]")), (85, array("86 0 R")), (86, text_element("", "0"))],
            "[]",
            1,
        ),
        (
            "an array written inside a direct /K array",
            INDIRECT_ROOT,
            vec![(81, parent("[[86 0 R]]")), (86, text_element("", "0"))],
            "[]",
            1,
        ),
        (
            "an element whose own /K is the indirect array that lists it (cycle)",
            INDIRECT_ROOT,
            vec![(81, parent("85 0 R")), (85, array("86 0 R")), (86, text_element("", "85 0 R"))],
            "[]",
            1,
        ),
    ];
    let mut red: Vec<String> = Vec::new();
    for (name, root, objects, nums, neutralized) in cases {
        let out = indirect_root_output(&quick(), root, &objects, nums);
        let remaining = DIRECT_TEXTS.iter().filter(|t| !gone(&out.bytes, t)).count();
        if remaining != DIRECT_TEXTS.len() {
            red.push(format!("{name}: only {remaining} of {} texts remain (the case needs the text to survive)", DIRECT_TEXTS.len()));
        }
        if out.structure_elements_neutralized != neutralized {
            red.push(format!("{name}: neutralized {}, expected {neutralized}", out.structure_elements_neutralized));
        }
        if out.region_flattened() {
            red.push(format!("{name}: region_flattened true while the structure text survives (residual {})", out.residual_in_region));
        }
        let reasons = out.not_proven_reasons();
        if reasons != [hsk_studio_press::NotProven::ResidualInRegion] {
            red.push(format!("{name}: reasons {reasons:?}, expected only ResidualInRegion"));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// PLR-094 sibling, the controls of probe A (the scratch probe's cases B, C and D, plus two for the kept page), which pin what the
/// best-effort purge does and does not remove; since R23 FIX_FIRST (STRUCTURE TREE = NOT PROVEN) all of them are tagged documents
/// and read not proven, with the same text facts as before: the element names the flattened page as `/Pg`, which stays because
/// the purge does not read the array (B); the parent tree maps the element to the flattened page, so the text is gone (C); the
/// same tree as probe A with direct arrays, text gone (D); an indirect array whose elements sit on the kept page keeps its text
/// (E); and an edited tree read through direct arrays only, with a `Document` element holding `/Lang` and no `/Pg`, keeps that text
/// (F). C, D, E and F read `flattened` before R23.
#[test]
fn indirect_array_k_neutralization_controls() {
    let parent = |k: &str| format!("<< /Type /StructElem /S /Document /Pg 3 0 R /K {k} >>");
    let kept_p = "<< /Type /StructElem /S /P /Pg 5 0 R /ActualText (KEPT-INDIRECT-TEXT) /K 86 0 R >>".to_string();
    let kept_span = "<< /Type /StructElem /S /Span /Alt (KEPT-INHERITED-INDIRECT) /K 1 >>".to_string();
    struct Control {
        name: &'static str,
        root: &'static str,
        objects: Vec<(u32, String)>,
        nums: &'static str,
        neutralized: u32,
        /// Whether the three texts of `text_element` are gone from the saved bytes; `None` when the case holds none.
        direct_gone: Option<bool>,
        /// Texts of the kept page that must stay.
        stay: &'static [&'static str],
    }
    let cases = vec![
        Control {
            name: "B: an element of the root's indirect array names the flattened page as /Pg",
            root: INDIRECT_ROOT_ARRAY,
            objects: vec![(85, "[81 0 R]".to_string()), (81, text_element("/Pg 3 0 R", "0"))],
            nums: "[]",
            neutralized: 0,
            direct_gone: Some(false),
            stay: &[],
        },
        Control {
            name: "C: the parent tree maps an element of the root's indirect array to the flattened page",
            root: INDIRECT_ROOT_ARRAY,
            objects: vec![(85, "[81 0 R]".to_string()), (81, text_element("", "0"))],
            nums: "[0 [81 0 R]]",
            neutralized: 1,
            direct_gone: Some(true),
            stay: &[],
        },
        Control {
            name: "D: probe A with direct arrays",
            root: INDIRECT_ROOT,
            objects: vec![(81, parent("[86 0 R]")), (86, text_element("", "0"))],
            nums: "[]",
            neutralized: 2,
            direct_gone: Some(true),
            stay: &[],
        },
        Control {
            name: "E: indirect arrays whose elements sit on the kept page (own /Pg, then inherited)",
            root: INDIRECT_ROOT_ARRAY,
            objects: vec![(85, "[82 0 R]".to_string()), (82, kept_p), (86, "[87 0 R]".to_string()), (87, kept_span)],
            nums: "[1 [82 0 R]]",
            neutralized: 0,
            direct_gone: None,
            stay: &["KEPT-INDIRECT-TEXT", "KEPT-INHERITED-INDIRECT"],
        },
        Control {
            name: "F: direct arrays only, a Document element with /Lang and no /Pg above an element of the kept page",
            root: INDIRECT_ROOT,
            objects: vec![
                (81, "<< /Type /StructElem /S /Document /Lang (KEPT-DOCUMENT-LANG) /K [82 0 R] >>".to_string()),
                (82, "<< /Type /StructElem /S /P /Pg 5 0 R /ActualText (KEPT-DIRECT-ARRAY-TEXT) /K 1 >>".to_string()),
            ],
            nums: "[1 [82 0 R]]",
            neutralized: 0,
            direct_gone: None,
            stay: &["KEPT-DOCUMENT-LANG", "KEPT-DIRECT-ARRAY-TEXT"],
        },
    ];
    let mut red: Vec<String> = Vec::new();
    for c in cases {
        let (name, out) = (c.name, indirect_root_output(&quick(), c.root, &c.objects, c.nums));
        if let Some(want_gone) = c.direct_gone {
            let gone_count = DIRECT_TEXTS.iter().filter(|t| gone(&out.bytes, t)).count();
            let want = if want_gone { DIRECT_TEXTS.len() } else { 0 };
            if gone_count != want {
                red.push(format!("{name}: {gone_count} of {} texts gone, expected {want}", DIRECT_TEXTS.len()));
            }
        }
        for text in c.stay {
            if gone(&out.bytes, text) {
                red.push(format!("{name}: {text} was removed (the kept page's text stays)"));
            }
        }
        if out.structure_elements_neutralized != c.neutralized {
            red.push(format!("{name}: neutralized {}, expected {}", out.structure_elements_neutralized, c.neutralized));
        }
        let reasons = out.not_proven_reasons();
        if out.region_flattened() || reasons != [hsk_studio_press::NotProven::ResidualInRegion] || out.residual_in_region == 0 {
            red.push(format!("{name}: region_flattened {} reasons {reasons:?} residual {}, expected reported", out.region_flattened(), out.residual_in_region));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// PLR-091 (pin of the current behaviour): an annotation that a kept page lists in its own `/Annots` stays in the file, and
/// when its `/P` names a flattened page the saved file is reported (`flatten_purge::check_document` counts it:
/// `residual_in_region` 1, `NotProven::ResidualInRegion`, `region_flattened` false). Fail closed: the annotation is not removed
/// and not read as cleared.
#[test]
fn kept_annotation_with_a_flattened_p_is_reported() {
    let pdf = build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> >> >>".to_vec()),
        (4, stream_body("", format!("0 g 300 300 100 100 re f {SECRET}").as_bytes())),
        (5, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R /Resources << /Font << /F1 6 0 R >> >> /Annots [71 0 R] >>".to_vec()),
        (6, HELVETICA.to_vec()),
        (7, stream_body("", b"BT /F1 12 Tf 72 700 Td (Page two) Tj ET")),
        (71, b"<< /Type /Annot /Subtype /FreeText /Rect [400 200 500 220] /P 3 0 R /AP << /N 73 0 R >> >>".to_vec()),
        (73, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 6 0 R >> >>", b"BT /F1 10 Tf 2 5 Td (ANNOT-KEPT-TEXT) Tj ET")),
    ]);
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    let mut red: Vec<String> = Vec::new();
    if gone(&out.bytes, "ANNOT-KEPT-TEXT") || out.flattened_annotations_removed != 0 {
        red.push(format!("the annotation a kept page lists was removed (removed {})", out.flattened_annotations_removed));
    }
    if out.residual_in_region != 1 {
        red.push(format!("residual_in_region {}, expected 1", out.residual_in_region));
    }
    if out.region_flattened() || out.not_proven_reasons() != [hsk_studio_press::NotProven::ResidualInRegion] {
        red.push(format!("region_flattened {} reasons {:?}", out.region_flattened(), out.not_proven_reasons()));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// The save reads not proven for the structure tree, for that reason alone: `region_flattened` and `region_cleared` false, a
/// non-zero `residual_in_region` and `NotProven::ResidualInRegion` as the only reason.
fn reported_for_the_tree(out: &hsk_studio_press::RedactedOutput) -> bool {
    !out.region_flattened()
        && !out.region_cleared()
        && out.residual_in_region >= 1
        && out.not_proven_reasons() == [hsk_studio_press::NotProven::ResidualInRegion]
}

/// The save reads the region as flattened: `region_flattened`, nothing counted, no reason at all.
fn read_as_flattened(out: &hsk_studio_press::RedactedOutput) -> bool {
    out.region_flattened() && out.residual_in_region == 0 && out.not_proven_reasons().is_empty()
}

/// R23 FIX_FIRST -> STRUCTURE TREE = NOT PROVEN (lead decision 2026-10-12T08:41:40Z): a saved document that has a structure tree
/// reads `region_flattened` false and `NotProven::ResidualInRegion`, whatever the tree looks like and whatever the purge removed
/// of it. The inputs are the R23 pre-review probes (validation/R23-prereview-narrow.json; probe file r23_probe.rs), every one of
/// which read `region_flattened` true at 54c60f9 while structure text survived: P1/P2/P3/P10 alias objects, P4/P5 an element only
/// the id tree or a foreign parent-tree key reaches, P9 an element deeper than the purge's 64 levels, P11 an element without
/// `/Pg`, and the well-formed files of R23-P4 (P6 `/IDTree` key text, P7 `/ClassMap` attribute text, P8 a `/Table` without
/// `/Pg`). The controls P3c, P5c and P9c show the best-effort removal still running (the text is gone) and the save still
/// reports the tree. Cases that need text to survive check that it does.
#[test]
fn tagged_documents_read_not_proven_whatever_the_tree_looks_like() {
    // `levels` element dictionaries written directly inside each other, the innermost naming the flattened page.
    let nested = |levels: usize| -> String {
        let mut s = format!("<< /S /Figure /Pg 3 0 R {DIRECT_TEXT_KEYS} /K 0 >>");
        for _ in 1..levels {
            s = format!("<< /S /Div /K {s} >>");
        }
        format!("<< /Type /StructTreeRoot /K {s} /ParentTree 83 0 R >>")
    };
    struct Case {
        name: &'static str,
        limits: Limits,
        root: String,
        objects: Vec<(u32, String)>,
        nums: &'static str,
        /// Texts that must survive in the saved file (the case needs them to).
        survive: &'static [&'static str],
        /// Texts the best-effort removal takes out.
        removed: &'static [&'static str],
    }
    let deep = Limits { max_depth: 200, ..quick() };
    let sect = "<< /Type /StructElem /S /Sect /Pg 3 0 R /K [84 0 R] >>".to_string();
    let document = "<< /Type /StructElem /S /Document /Pg 3 0 R /K 0 >>".to_string();
    let alias_root = "<< /Type /StructTreeRoot /K [84 0 R] /ParentTree 83 0 R >>".to_string();
    let case = |name: &'static str,
                limits: &Limits,
                root: &str,
                objects: Vec<(u32, String)>,
                nums: &'static str,
                survive: &'static [&'static str],
                removed: &'static [&'static str]| Case {
        name,
        limits: *limits,
        root: root.to_string(),
        objects,
        nums,
        survive,
        removed,
    };
    let all: &'static [&'static str] = &DIRECT_TEXTS;
    let cases = vec![
        case(
            "P1: a /K member is an alias object (84 = 86 0 R) under a parent on the flattened page",
            &quick(),
            INDIRECT_ROOT,
            vec![(81, sect.clone()), (84, "86 0 R".into()), (86, text_element("", "0"))],
            "[]",
            all,
            &[],
        ),
        case(
            "P2: the same, the parent tree maps page one to the alias",
            &quick(),
            INDIRECT_ROOT,
            vec![(81, sect), (84, "86 0 R".into()), (86, text_element("", "0"))],
            "[0 [84 0 R]]",
            all,
            &[],
        ),
        case(
            "P3: a root /K member is an alias object, the parent tree maps page one through the alias",
            &quick(),
            &alias_root,
            vec![(84, "86 0 R".into()), (86, text_element("", "0"))],
            "[0 [84 0 R]]",
            all,
            &[],
        ),
        case(
            "P3c control: the parent tree maps page one to the element itself, the removal runs",
            &quick(),
            &alias_root,
            vec![(84, "86 0 R".into()), (86, text_element("", "0"))],
            "[0 [86 0 R]]",
            &[],
            all,
        ),
        case(
            "P4: an element only the id tree reaches (its /P is the element on the flattened page)",
            &quick(),
            "<< /Type /StructTreeRoot /K [81 0 R] /ParentTree 83 0 R /IDTree << /Names [(e1) 86 0 R] >> >>",
            vec![(81, document.clone()), (86, text_element("/P 81 0 R /ID (e1)", "1"))],
            "[0 [81 0 R]]",
            all,
            &[],
        ),
        case(
            "P5: an element only a parent-tree key no page names reaches",
            &quick(),
            INDIRECT_ROOT,
            vec![(81, document.clone()), (86, text_element("/P 81 0 R", "1"))],
            "[0 [81 0 R] 7 [86 0 R]]",
            all,
            &[],
        ),
        case(
            "P5c control: the element is reached through the flattened page's parent-tree entry, the removal runs",
            &quick(),
            INDIRECT_ROOT,
            vec![(81, document), (86, text_element("/P 81 0 R", "1"))],
            "[0 [81 0 R 86 0 R]]",
            &[],
            all,
        ),
        case(
            "P6 (R23-P4, well-formed): the /IDTree key of a neutralized element keeps the id text",
            &quick(),
            "<< /Type /StructTreeRoot /K [81 0 R] /ParentTree 83 0 R /IDTree << /Names [(R23-ID-SECRET) 81 0 R] >> >>",
            vec![(81, text_element("/Pg 3 0 R /ID (R23-ID-SECRET)", "0"))],
            "[0 [81 0 R]]",
            &["R23-ID-SECRET"],
            all,
        ),
        case(
            "P7 (R23-P4, well-formed): the /ClassMap attribute text of a neutralized element stays",
            &quick(),
            "<< /Type /StructTreeRoot /K [81 0 R] /ParentTree 83 0 R /ClassMap << /Cls1 << /O /UserProperties /P [<< /N (note) /V (R23-CLASSMAP-SECRET) >>] >> >> >>",
            vec![(81, text_element("/Pg 3 0 R /C /Cls1", "0"))],
            "[0 [81 0 R]]",
            &["R23-CLASSMAP-SECRET"],
            all,
        ),
        case(
            "P8 (R23-P4, well-formed): a /Table without /Pg keeps its /Alt and /T, its only child is on the flattened page",
            &quick(),
            INDIRECT_ROOT,
            vec![
                (81, "<< /Type /StructElem /S /Table /Alt (R23-GROUP-ALT) /T (R23-GROUP-TITLE) /K [82 0 R] >>".into()),
                (82, "<< /Type /StructElem /S /TR /P 81 0 R /Pg 3 0 R /K 0 >>".into()),
            ],
            "[0 [82 0 R]]",
            &["R23-GROUP-ALT", "R23-GROUP-TITLE"],
            &[],
        ),
        case("P9: 70 element dictionaries nested directly, Limits::max_depth 200", &deep, &nested(70), vec![], "[]", all, &[]),
        case("P9c control: 20 levels, Limits::max_depth 200, the removal runs", &deep, &nested(20), vec![], "[]", &[], all),
        case(
            "P10: an element whose /Pg is an alias object of the flattened page (90 = 3 0 R)",
            &quick(),
            INDIRECT_ROOT,
            vec![(81, text_element("/Pg 90 0 R", "0")), (90, "3 0 R".into())],
            "[]",
            all,
            &[],
        ),
        case(
            "P11: an element without /Pg, marked-content content the parent tree does not map",
            &quick(),
            INDIRECT_ROOT,
            vec![(81, text_element("", "0"))],
            "[]",
            all,
            &[],
        ),
    ];
    let mut red: Vec<String> = Vec::new();
    for c in cases {
        let out = indirect_root_output(&c.limits, &c.root, &c.objects, c.nums);
        for marker in c.survive {
            if gone(&out.bytes, marker) {
                red.push(format!("{}: {marker} was removed (the case needs the text to survive)", c.name));
            }
        }
        for marker in c.removed {
            if !gone(&out.bytes, marker) {
                red.push(format!("{}: {marker} survives (the removal is expected to run here)", c.name));
            }
        }
        if !reported_for_the_tree(&out) {
            red.push(format!(
                "{}: region_flattened {} residual {} reasons {:?}, expected only ResidualInRegion",
                c.name,
                out.region_flattened(),
                out.residual_in_region,
                out.not_proven_reasons()
            ));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// STRUCTURE TREE = NOT PROVEN, the controls: a document without a structure tree reads flattened (nothing counted, no reason),
/// and so does a tagged document whose caller stripped the tree before the save (the catalog's `/StructTreeRoot` and `/MarkInfo`
/// and every `/StructParents` removed): the tree objects left in the file unreferenced are not written, so their texts are gone.
/// `/MarkInfo` alone, and a `/StructParents` that only the flattened page carried (the flatten drops it), are not a tree.
#[test]
fn untagged_documents_and_stripped_trees_still_read_flattened() {
    let tree = vec![
        (80, INDIRECT_ROOT.to_string()),
        (81, text_element("/Pg 3 0 R", "0")),
        (82, text_element("/Pg 5 0 R", "1")),
        (83, "<< /Nums [0 [81 0 R] 1 [82 0 R]] >>".to_string()),
    ];
    let cases = vec![
        ("an untagged document", pages_output_with(&quick(), "", "", "", &[])),
        ("a tagged document stripped by the caller, its tree objects left in the file unreferenced", pages_output_with(&quick(), "", "", "", &tree)),
        ("/MarkInfo alone", pages_output_with(&quick(), "/MarkInfo << /Marked true >>", "", "", &[])),
        ("a /StructParents only the flattened page carried, no tree", pages_output_with(&quick(), "", "/StructParents 0", "", &[])),
    ];
    let mut red: Vec<String> = Vec::new();
    for (name, out) in cases {
        if !read_as_flattened(&out) {
            red.push(format!("{name}: region_flattened {} residual {} reasons {:?}", out.region_flattened(), out.residual_in_region, out.not_proven_reasons()));
        }
        if !gone(&out.bytes, "SECRET-ALPHA") {
            red.push(format!("{name}: SECRET-ALPHA survives"));
        }
    }
    let stripped = pages_output_with(&quick(), "", "", "", &tree);
    for text in DIRECT_TEXTS {
        if !gone(&stripped.bytes, text) {
            red.push(format!("the unreferenced tree object text {text} was written"));
        }
    }
    // The same file with the tree wired in is the tagged document these controls differ from.
    let tagged = tree_output_with(&quick(), "80 0 R", &tree[..3], "[0 [81 0 R] 1 [82 0 R]]");
    if !reported_for_the_tree(&tagged) {
        red.push(format!("the tagged counterpart: region_flattened {} reasons {:?}", tagged.region_flattened(), tagged.not_proven_reasons()));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// STRUCTURE TREE = NOT PROVEN, V4-08 (hostile text format: the keys are read from the parsed dictionary, not matched in raw
/// text): a name escape spells the key (`/Struct#54reeRoot`, `/Struct#50arents`) and counts; whitespace between key and value
/// counts; a comment, a string that holds the key text, and a key in another case (`/structtreeroot`, `/STRUCTPARENTS`) do not.
/// The value is read through references with the crate's bound: a null value, a reference to no object, a reference to a null
/// object and a short chain that ends in either read as no tree (the null object); a chain of 20 aliases counts, whatever it ends
/// in (fail closed past `MAX_REF_HOPS`), so does an alias chain that ends in the root dictionary (R23-P1's class).
#[test]
fn structure_tree_keys_are_read_as_names_not_text() {
    let tree = vec![(80, "<< /Type /StructTreeRoot /K [81 0 R] >>".to_string()), (81, text_element("/Pg 5 0 R", "1"))];
    // `hops` aliases (80 -> 81 -> ...) and then `end` as the last object.
    let chain = |hops: u32, end: &str| -> Vec<(u32, String)> {
        (0..hops).map(|i| (80 + i, format!("{} 0 R", 81 + i))).chain([(80 + hops, end.to_string())]).collect()
    };
    let root = "<< /Type /StructTreeRoot /K [] >>";
    let limits = quick();
    // (case, catalog extra, page two extra, objects, expected: true = reported for the tree)
    type Case = (&'static str, &'static str, &'static str, Vec<(u32, String)>, bool);
    let cases: Vec<Case> = vec![
        ("control: the key spelled plainly", "/StructTreeRoot 80 0 R", "", tree.clone(), true),
        ("the catalog key spelled with a name escape", "/Struct#54reeRoot 80 0 R", "", tree.clone(), true),
        ("a /StructParents spelled with a name escape on a kept page", "", "/Struct#50arents 1", vec![], true),
        ("whitespace between the key and its value", "", "/StructParents\n\t 1", vec![], true),
        (
            "the key text only in comments and strings",
            "% /StructTreeRoot 80 0 R\n /Lang (/StructTreeRoot 80 0 R) /Title (/StructParents 1)",
            "% /StructParents 1\n /HskNote (/StructParents 1 /StructTreeRoot 80 0 R)",
            tree.clone(),
            false,
        ),
        (
            "the keys in another case name no tree, and the objects they reach are plain dictionaries",
            "/structtreeroot 80 0 R /STRUCTTREEROOT 80 0 R",
            "/structparents 1 /STRUCTPARENTS 1 /Structparents 1",
            vec![(80, "<< /HskPlain (x) >>".to_string())],
            false,
        ),
        // PLR-102 (changed assertion: was `false` with these objects): the keys name no tree, but the objects they reach are a
        // /StructTreeRoot and a /StructElem dictionary that the saved file still holds, which is the detached-element class.
        (
            "the keys in another case name no tree, but the objects they reach are a tree root and an element (PLR-102)",
            "/structtreeroot 80 0 R /STRUCTTREEROOT 80 0 R",
            "/structparents 1 /STRUCTPARENTS 1 /Structparents 1",
            tree.clone(),
            true,
        ),
        ("a null /StructTreeRoot and a null /StructParents", "/StructTreeRoot null", "/StructParents null", tree.clone(), false),
        ("a /StructTreeRoot that names no object", "/StructTreeRoot 99 0 R", "", vec![], false),
        ("a /StructTreeRoot that names a null object", "/StructTreeRoot 80 0 R", "", vec![(80, "null".to_string())], false),
        ("a chain of 3 aliases that ends in a null object", "/StructTreeRoot 80 0 R", "", chain(3, "null"), false),
        ("a chain of 3 aliases that ends in the root dictionary", "/StructTreeRoot 80 0 R", "", chain(3, root), true),
        ("a chain of 20 aliases that ends in a null object", "/StructTreeRoot 80 0 R", "", chain(20, "null"), true),
        ("a chain of 20 aliases that ends in the root dictionary", "/StructTreeRoot 80 0 R", "", chain(20, root), true),
        ("a /StructTreeRoot that is not a dictionary", "/StructTreeRoot 12", "", vec![], true),
    ];
    let mut red: Vec<String> = Vec::new();
    for (name, catalog_extra, page_extra, objects, reported) in cases {
        let out = pages_output_with(&limits, catalog_extra, "", page_extra, &objects);
        if reported && !reported_for_the_tree(&out) {
            red.push(format!("{name}: region_flattened {} reasons {:?}, expected the tree reported", out.region_flattened(), out.not_proven_reasons()));
        }
        if !reported && !read_as_flattened(&out) {
            red.push(format!("{name}: region_flattened {} residual {} reasons {:?}, expected flattened", out.region_flattened(), out.residual_in_region, out.not_proven_reasons()));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// STRUCTURE TREE = NOT PROVEN: a `/StructParents` or `/StructParent` entry left in the file counts although the catalog names no
/// tree: on a kept page, in a nested direct dictionary of a kept object, on an annotation a kept page lists. The same keys on the
/// flattened page and on its annotation do not (the flatten drops the page's key, the purge removes the annotation). A save with no
/// region applies none of these rules (PLR-101, test `structure_rules_apply_only_when_a_region_is_flattened`).
#[test]
fn structure_parents_left_in_the_file_are_reported() {
    let annot = |n: u32, key: &str| (n, format!("<< /Type /Annot /Subtype /Text /Rect [10 10 20 20] /Contents (note) {key} >>"));
    let cases = vec![
        ("a kept page keeps /StructParents", pages_output_with(&quick(), "", "", "/StructParents 1", &[]), true),
        ("a nested direct dictionary of a kept page holds /StructParents", pages_output_with(&quick(), "", "", "/HskNested << /Inner << /StructParents 3 >> >>", &[]), true),
        ("an annotation a kept page lists keeps /StructParent", pages_output_with(&quick(), "", "", "/Annots [71 0 R]", &[annot(71, "/StructParent 5")]), true),
        ("an annotation of the flattened page held /StructParent, it is removed", pages_output_with(&quick(), "", "/Annots [72 0 R]", "", &[annot(72, "/StructParent 6")]), false),
        ("an annotation a kept page lists holds no key", pages_output_with(&quick(), "", "", "/Annots [71 0 R]", &[annot(71, "")]), false),
    ];
    let mut red: Vec<String> = Vec::new();
    for (name, out, reported) in cases {
        if reported && !reported_for_the_tree(&out) {
            red.push(format!("{name}: region_flattened {} reasons {:?}, expected the key reported", out.region_flattened(), out.not_proven_reasons()));
        }
        if !reported && !read_as_flattened(&out) {
            red.push(format!("{name}: region_flattened {} residual {} reasons {:?}, expected flattened", out.region_flattened(), out.residual_in_region, out.not_proven_reasons()));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// An object stream (`/Type /ObjStm`) that holds `members`, written as the uncompressed text or, with `hex`, ASCIIHex-encoded. No
/// cross-reference entry names its members, so the input is an ordinary stream object that nothing expands.
fn object_stream(members: &[(u32, &str)], hex: bool) -> String {
    let (mut head, mut body) = (String::new(), String::new());
    for (n, s) in members {
        head.push_str(&format!("{n} {} ", body.len()));
        body.push_str(s);
        body.push(' ');
    }
    let first = head.len();
    let data = format!("{head}{body}");
    let (filter, data) = if hex { ("/Filter /ASCIIHexDecode ", data.bytes().map(|b| format!("{b:02X}")).collect::<String>() + ">") } else { ("", data) };
    String::from_utf8(stream_body(&format!("{filter}/Type /ObjStm /N {} /First {first}", members.len()), data.as_bytes())).expect("ascii")
}

/// An outline (object 70) with one item (object 71, `item`), the catalog key `/Outlines 70 0 R` to add.
fn outline_with(item: &str) -> Vec<(u32, String)> {
    vec![
        (70, "<< /Type /Outlines /First 71 0 R /Last 71 0 R /Count 1 >>".to_string()),
        (71, item.to_string()),
    ]
}

/// PLR-102 (lead decision R24-P2, FAIL CLOSED), the R24 pre-review inputs J1, J2a, J2b and K1: structure-element text survives in
/// the saved file although the catalog names no structure tree and no `/StructParents` is left, so the structure-tree rule alone
/// read them as flattened. A saved file that still holds a dictionary with the shape of a structure element (or of an outline item
/// that names one) reads `region_flattened` false and `NotProven::ResidualInRegion` only, wherever the dictionary sits: J1 a
/// `/StructTreeRoot` dictionary under a private catalog key, J2a and J2b a stripped tree whose element an outline item still names
/// with `/SE` (J2a with the element's `/Pg` the flattened page, which the `/Pg` scan already counted), K1 an object stream the
/// catalog references whose members no cross-reference entry names (plain and ASCIIHex-encoded). An object stream the file holds as
/// a stream that cannot be read (a refused filter, no `/N`, an offset past the data, a header longer than the data, members that
/// overlap past the decoded size) counts as well (fail closed). Cases that need the text to survive check that it does.
#[test]
fn detached_structure_elements_are_reported() {
    let root = "<< /Type /StructTreeRoot /K [81 0 R] >>";
    let element = |pg: &str| format!("<< /Type /StructElem /S /Figure /P 80 0 R {pg} {DIRECT_TEXT_KEYS} /K 0 >>");
    let members: [(u32, &str); 3] = [
        (60, "<< /Type /Catalog /Pages 2 0 R /StructTreeRoot 80 0 R >>"),
        (80, root),
        (81, "<< /Type /StructElem /S /Figure /P 80 0 R /Alt (ALT-DIRECT-SECRET) /ActualText (ACTUAL-DIRECT-SECRET) /E (EXPANSION-DIRECT-SECRET) /K 0 >>"),
    ];
    let item = |se: &str| format!("<< /Title (Figure) /Parent 70 0 R {se} /Dest [5 0 R /Fit] >>");
    let raw_stream = |dict: &str, data: &str| String::from_utf8(stream_body(dict, data.as_bytes())).expect("ascii");
    let with_outline = |item: String, more: Vec<(u32, String)>| -> Vec<(u32, String)> {
        let mut v = outline_with(&item);
        v.extend(more);
        v
    };
    // (case, catalog extra, objects, the texts that must survive in the saved file: the case needs them to)
    let all: &'static [&'static str] = &DIRECT_TEXTS;
    type Case = (&'static str, &'static str, Vec<(u32, String)>, &'static [&'static str]);
    let cases: Vec<Case> = vec![
        ("J1: a /StructTreeRoot dictionary under a private catalog key, its element on the kept page", "/HskTree 80 0 R", vec![(80, root.to_string()), (81, element("/Pg 5 0 R"))], all),
        (
            "J2a: stripped tree, an outline item names an element whose /Pg is the flattened page",
            "/Outlines 70 0 R",
            with_outline(item("/SE 81 0 R"), vec![(80, root.to_string()), (81, element("/Pg 3 0 R"))]),
            all,
        ),
        (
            "J2b: stripped tree, an outline item names an element without /Pg",
            "/Outlines 70 0 R",
            with_outline(item("/SE 81 0 R"), vec![(80, root.to_string()), (81, element(""))]),
            all,
        ),
        ("K1: an object stream the catalog references, its members (catalog, root, element) named by no xref entry", "/HskJunk 50 0 R", vec![(50, object_stream(&members, false))], all),
        ("K1 with the object stream ASCIIHex-encoded", "/HskJunk 50 0 R", vec![(50, object_stream(&members, true))], &[]),
        (
            "an element with no /Type, /S and /P and text keys, under a private key",
            "/HskNote 81 0 R",
            vec![(81, format!("<< /S /Figure /P 80 0 R {DIRECT_TEXT_KEYS} >>"))],
            all,
        ),
        (
            "a structure tree root with /ClassMap text and no element under a private key",
            "/HskTree 80 0 R",
            vec![(80, "<< /Type /StructTreeRoot /ClassMap << /Cls1 << /O /UserProperties /P [<< /N (note) /V (CLASSMAP-DETACHED-SECRET) >>] >> >> >>".to_string())],
            &["CLASSMAP-DETACHED-SECRET"],
        ),
        // Fail closed: an object stream the file holds as a stream that cannot be read counts, whatever it holds.
        (
            "an object stream with a filter this crate refuses (DCTDecode)",
            "/HskJunk 50 0 R",
            vec![(50, raw_stream("/Type /ObjStm /N 1 /First 5 /Filter /DCTDecode", "60 0 << >>"))],
            &[],
        ),
        ("an object stream without /N", "/HskJunk 50 0 R", vec![(50, raw_stream("/Type /ObjStm /First 5", "60 0 << >>"))], &[]),
        ("an object stream whose member offset is past its data", "/HskJunk 50 0 R", vec![(50, raw_stream("/Type /ObjStm /N 1 /First 5", "60 900 << >>"))], &[]),
        ("an object stream whose member does not parse", "/HskJunk 50 0 R", vec![(50, raw_stream("/Type /ObjStm /N 1 /First 5", "60 0 >>>>"))], &[]),
        // V4-02 dims_u32_max-style rows: the header sizes are read as claims and checked against the decoded data.
        (
            "an object stream with /N 4294967295",
            "/HskJunk 50 0 R",
            vec![(50, raw_stream("/Type /ObjStm /N 4294967295 /First 5", "60 0 << >>"))],
            &[],
        ),
        (
            "an object stream with /First 4294967295",
            "/HskJunk 50 0 R",
            vec![(50, raw_stream("/Type /ObjStm /N 1 /First 4294967295", "60 0 << >>"))],
            &[],
        ),
        (
            "an object stream whose header lists more members than it holds",
            "/HskJunk 50 0 R",
            vec![(50, raw_stream("/Type /ObjStm /N 3 /First 5", "60 0 << >>"))],
            &[],
        ),
        (
            "an object stream whose three members are one dictionary (parsed bytes over the decoded size)",
            "/HskJunk 50 0 R",
            vec![(50, raw_stream("/Type /ObjStm /N 3 /First 15", "60 0 61 0 62 0 << /Hsk (aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa) >>"))],
            &[],
        ),
    ];
    let mut red: Vec<String> = Vec::new();
    for (name, catalog_extra, objects, survive) in cases {
        let out = pages_output_with(&quick(), catalog_extra, "", "", &objects);
        for marker in survive {
            if gone(&out.bytes, marker) {
                red.push(format!("{name}: {marker} was removed (the case needs the text to survive)"));
            }
        }
        let counted = !out.region_flattened()
            && !out.region_cleared()
            && out.residual_in_region >= 1
            && out.not_proven_reasons().contains(&hsk_studio_press::NotProven::ResidualInRegion);
        // The unreadable object streams may add their own reasons (a refused filter); every other case reports for this rule alone.
        if !counted || (!name.starts_with("an object stream") && !reported_for_the_tree(&out)) {
            red.push(format!(
                "{name}: region_flattened {} cleared {} residual {} reasons {:?}, expected ResidualInRegion",
                out.region_flattened(),
                out.region_cleared(),
                out.residual_in_region,
                out.not_proven_reasons()
            ));
        }
        if !gone(&out.bytes, "SECRET-ALPHA") {
            red.push(format!("{name}: SECRET-ALPHA survives (the region text is removed whatever the structure)"));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// PLR-102, V4-08 (hostile text format): the shape is read from the PARSED dictionary, not matched in raw text. A name escape
/// spells the key or the value, white space and line breaks separate them, a value that is a reference to a name is followed:
/// these count. A comment, a string that holds the keys, the keys in another case, null values, and the dictionaries that only look
/// like an element (an action and a border style that have `/S`, a page label with `/S` and `/P`) do not, and read flattened with
/// the region text gone. The last non-reported row records the rule's boundary: `/S` with a text key but no `/P` and no
/// `/Type` is not read as an element.
#[test]
fn structure_shape_is_read_from_the_parsed_dictionary() {
    let e = DIRECT_TEXT_KEYS;
    // (case, catalog extra, page two extra, objects, expected: true = reported)
    type Case = (&'static str, String, &'static str, Vec<(u32, String)>, bool);
    let note = "/HskNote 81 0 R".to_string();
    let single = |body: String| vec![(81u32, body)];
    let cases: Vec<Case> = vec![
        ("control: /Type /StructElem", note.clone(), "", single(format!("<< /Type /StructElem /S /Figure {e} >>")), true),
        ("a name escape in the value, /Struct#45lem", note.clone(), "", single(format!("<< /Type /Struct#45lem {e} >>")), true),
        ("a name escape in the key, /#54ype", note.clone(), "", single("<< /#54ype /StructElem >>".to_string()), true),
        ("/Type through a reference to a name", note.clone(), "", vec![(81, "<< /Type 82 0 R >>".to_string()), (82, "/StructElem".to_string())], true),
        ("white space and line breaks between the key and the value", note.clone(), "", single("<<\t/Type\r\n\n/StructElem >>".to_string()), true),
        ("a structure tree root alone, with /ClassMap text", note.clone(), "", single("<< /Type /StructTreeRoot /ClassMap << /C1 << /O /UserProperties >> >> >>".to_string()), true),
        ("/S a name, /P and text keys, no /Type", note.clone(), "", single(format!("<< /S /Figure /P 80 0 R {e} >>")), true),
        ("the same with the keys spelled /#53 and /#50", note.clone(), "", single("<< /#53 /Figure /#50 80 0 R /Alt (ALT-DIRECT-SECRET) >>".to_string()), true),
        ("/S through a reference to a name", note.clone(), "", vec![(81, "<< /S 82 0 R /P 80 0 R /Lang (en) >>".to_string()), (82, "/Figure".to_string())], true),
        (
            "an outline item with /SE that names a plain dictionary",
            "/Outlines 70 0 R".to_string(),
            "",
            vec![
                (70, "<< /Type /Outlines /First 71 0 R /Last 71 0 R /Count 1 >>".to_string()),
                (71, "<< /Title (Figure) /Parent 70 0 R /SE 81 0 R >>".to_string()),
                (81, "<< /HskPlain (x) >>".to_string()),
            ],
            true,
        ),
        (
            "an outline item with /SE spelled /S#45 and a line break before the value",
            "/Outlines 70 0 R".to_string(),
            "",
            vec![
                (70, "<< /Type /Outlines /First 71 0 R /Last 71 0 R /Count 1 >>".to_string()),
                (71, "<< /Title (Figure) /Parent 70 0 R /S#45\n81 0 R >>".to_string()),
                (81, "<< /HskPlain (x) >>".to_string()),
            ],
            true,
        ),
        ("an element in a nested direct dictionary and array of a private key", "/HskNote << /A << /B [ << /Type /StructElem >> ] >> >>".to_string(), "", vec![], true),
        (
            "an element in a member of an object stream the file keeps as a stream",
            "/HskJunk 50 0 R".to_string(),
            "",
            vec![(50, object_stream(&[(60, "<< /Type /StructElem /S /Figure >>")], false))],
            true,
        ),
        ("a comment holds the keys", note.clone(), "", single("<< /Hsk (note) % /Type /StructElem /SE 81 0 R /S /Figure /P 80 0 R /Alt (X)\n >>".to_string()), false),
        (
            "a string holds the keys",
            "/HskNote (<< /Type /StructElem /S /Figure /P 80 0 R /Alt (X) >> /SE 81 0 R)".to_string(),
            "",
            vec![],
            false,
        ),
        (
            "the keys and the value in another case",
            note.clone(),
            "",
            single("<< /type /structelem /Type /Structelem /se 81 0 R /Se 81 0 R /s /Figure /p 80 0 R /alt (X) >>".to_string()),
            false,
        ),
        ("null values", note.clone(), "", single("<< /Type null /SE null /S /Figure /P 80 0 R /Alt null /Lang null >>".to_string()), false),
        (
            "an action and a border style that only have /S, on an annotation a kept page lists",
            String::new(),
            "/Annots [71 0 R]",
            vec![(71, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /BS << /W 1 /S /D /D [3 2] >> /A << /S /URI /URI (http://example.com) >> /T (note) /C [1 0 0] /P 5 0 R >>".to_string())],
            false,
        ),
        ("a page label dictionary, /S and /P (a prefix string)", "/PageLabels << /Nums [0 << /S /D /P (A-) /St 1 >>] >>".to_string(), "", vec![], false),
        ("boundary: /S and a text key, no /P and no /Type", note.clone(), "", single("<< /S /Figure /Alt (X) >>".to_string()), false),
        (
            "an object stream whose members hold no structure shape",
            "/HskJunk 50 0 R".to_string(),
            "",
            vec![(50, object_stream(&[(60, "<< /Type /Catalog /Pages 2 0 R >>"), (61, "<< /S /URI /URI (http://example.com) >>")], false))],
            false,
        ),
    ];
    let mut red: Vec<String> = Vec::new();
    for (name, catalog_extra, page_two_extra, objects, reported) in cases {
        let out = pages_output_with(&quick(), &catalog_extra, "", page_two_extra, &objects);
        if reported && !reported_for_the_tree(&out) {
            red.push(format!("{name}: region_flattened {} reasons {:?}, expected reported", out.region_flattened(), out.not_proven_reasons()));
        }
        if !reported && !read_as_flattened(&out) {
            red.push(format!("{name}: region_flattened {} residual {} reasons {:?}, expected flattened", out.region_flattened(), out.residual_in_region, out.not_proven_reasons()));
        }
        if !gone(&out.bytes, "SECRET-ALPHA") {
            red.push(format!("{name}: SECRET-ALPHA survives"));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// PLR-101 (scope): the structure rules (the tree, `/StructParents`, and the structure-element shape of PLR-102) apply when the
/// save flattens at least one region. The same input read with the region is reported and read with no region at all is not:
/// nothing is flattened, `residual_in_region` is 0 and no reason is reported. The inputs are a tagged document (catalog tree,
/// `/StructParents`, an element) and the R24 detached inputs J1, J2b and K1.
#[test]
fn structure_rules_apply_only_when_a_region_is_flattened() {
    let root = "<< /Type /StructTreeRoot /K [81 0 R] >>";
    let element = "<< /Type /StructElem /S /Figure /P 80 0 R /Pg 5 0 R /Alt (ALT-DIRECT-SECRET) /K 1 >>";
    let tagged = (
        "a tagged document: catalog tree, /StructParents, an element",
        "/StructTreeRoot 80 0 R /MarkInfo << /Marked true >>",
        "/StructParents 0",
        "/StructParents 1",
        vec![(80, "<< /Type /StructTreeRoot /K [81 0 R] /ParentTree 83 0 R >>".to_string()), (81, element.to_string()), (83, "<< /Nums [1 [81 0 R]] >>".to_string())],
    );
    let j1 = ("J1", "/HskTree 80 0 R", "", "", vec![(80, root.to_string()), (81, element.to_string())]);
    let mut j2b_objects = outline_with("<< /Title (Figure) /Parent 70 0 R /SE 81 0 R /Dest [5 0 R /Fit] >>");
    j2b_objects.extend([(80, root.to_string()), (81, "<< /Type /StructElem /S /Figure /P 80 0 R /Alt (ALT-DIRECT-SECRET) /K 0 >>".to_string())]);
    let j2b = ("J2b", "/Outlines 70 0 R", "", "", j2b_objects);
    let k1 = ("K1", "/HskJunk 50 0 R", "", "", vec![(50, object_stream(&[(80, root), (81, element)], false))]);
    let mut red: Vec<String> = Vec::new();
    for (name, catalog_extra, page_one_extra, page_two_extra, objects) in [tagged, j1, j2b, k1] {
        let with = pages_output_with(&quick(), catalog_extra, page_one_extra, page_two_extra, &objects);
        if !reported_for_the_tree(&with) {
            red.push(format!("{name}, with a region: region_flattened {} reasons {:?}, expected reported", with.region_flattened(), with.not_proven_reasons()));
        }
        let pdf = pages_pdf(catalog_extra, page_one_extra, page_two_extra, &objects);
        let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
        let none = redact_and_save(&mut doc, &[], UnsupportedPolicy::Refuse, &token()).expect("redact with no region");
        if !none.flattened.is_empty() || !read_as_flattened(&none) {
            red.push(format!(
                "{name}, no region: flattened pages {} region_flattened {} residual {} reasons {:?}, expected nothing counted",
                none.flattened.len(),
                none.region_flattened(),
                none.residual_in_region,
                none.not_proven_reasons()
            ));
        }
        if gone(&none.bytes, "Page two") {
            red.push(format!("{name}, no region: the kept page text is gone (a save with no region keeps the pages)"));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R23 FIX_FIRST -> STRUCTURE TREE = NOT PROVEN (V4-07): the DESCRIPTOR states the narrowed claim in `purge` (the residual) and in
/// `region_clearing` (what `check_document` counts), in place of the per-element sentences of 54c60f9 (the unedited-root rule and
/// the unread-`/K`-array rule); the residual cites tests that exist and uses none of the words that need a citation. PLR-101 and
/// PLR-102 (SMB-PRESS-STRUCT-ORPHANS): both rules are stated, the structure rules apply when a region is flattened, the shape that
/// counts is spelled out, and the stale wording (the tree alone, every save) is gone.
#[test]
fn descriptor_states_the_structure_tree_rule() {
    let descriptor = hsk_studio_press::DESCRIPTOR;
    let rule_start = "residuals: STRUCTURE TREE = NOT PROVEN (R23 FIX_FIRST, lead decision 2026-10-12T08:41:40Z): the redaction proof covers untagged documents;";
    let counted = "and, when the save flattened a region, no structure tree: a /StructTreeRoot in the catalog, any /StructParents or /StructParent entry, or any dictionary with the shape of a structure element or of an outline item that names one (PLR-102), adds 1 together, so region_flattened is true only for an untagged document, or one whose caller stripped the whole tree before the save";
    let scope = "when a save flattens at least one region and the saved document has a structure tree, that is a /StructTreeRoot that the catalog holds";
    let shape = "(PLR-102: /Type /StructElem or /StructTreeRoot, or /S as a name together with /P and any of /Alt /ActualText /E /T /Lang /A /C /ID /Ref /AF /Phoneme /PhoneticAlphabet, or an /SE entry as an outline item has), residual_in_region adds 1 for them together (one count, not one per element)";
    let no_region = "a save with no region does not apply these rules (PLR-101)";
    for needle in [rule_start, counted, scope, shape, no_region] {
        assert_eq!(descriptor.matches(needle).count(), 1, "DESCRIPTOR lacks {needle:?}");
    }
    for stale in [
        "a structure tree root that is not an indirect reference to a dictionary (written directly into the catalog",
        "in a structure tree the purge did not edit, no element that is off the kept pages",
        "below an unread /K array",
        "elements are reached from the root's /K, /ParentTree and /IDTree",
        "a structure tree root written directly into the catalog (not an indirect object) is not edited, so an element that still names",
        "residual_in_region counts 1 for it (one count for the tree, not one per element)",
        "and no structure tree: a /StructTreeRoot in the catalog or any /StructParents or /StructParent entry counts 1",
        "or one whose caller stripped the tree before the save",
    ] {
        assert!(!descriptor.contains(stale), "DESCRIPTOR still says {stale:?}");
    }
    let start = descriptor.find(rule_start).expect("rule sentence");
    let end = start + descriptor[start..].find("; an object reference (OBJR)").expect("the residual that follows the rule");
    let rule = &descriptor[start..end];
    for word in ["never", "always", "guarantee", "proves"] {
        assert!(!rule.contains(word), "the rule sentence says {word:?}");
    }
    for name in [
        "tagged_documents_read_not_proven_whatever_the_tree_looks_like",
        "untagged_documents_and_stripped_trees_still_read_flattened",
        "structure_tree_keys_are_read_as_names_not_text",
        "structure_parents_left_in_the_file_are_reported",
        "direct_structure_tree_elements_off_the_kept_pages_are_reported",
        "direct_structure_tree_on_kept_pages_is_reported_too",
        "indirect_array_k_elements_off_the_kept_pages_are_reported",
        "indirect_array_k_neutralization_controls",
        "detached_structure_elements_are_reported",
        "structure_shape_is_read_from_the_parsed_dictionary",
        "structure_rules_apply_only_when_a_region_is_flattened",
        "unreadable_structure_shapes_are_counted",
    ] {
        assert!(rule.contains(name), "the rule sentence does not cite {name}");
        assert!(include_str!("flatten_r13.rs").contains(&format!("fn {name}()")), "the cited test {name} does not exist");
    }
    // PLR-104 (V4-07): the property-list sentence states the purge and the fail-closed count and cites tests that exist.
    let props_start = descriptor.find("PLR-104 marked-content property lists").expect("PLR-104 sentence");
    let props = &descriptor[props_start..props_start + descriptor[props_start..].find("; residuals:").expect("end of the PLR-104 sentence")];
    for needle in ["the entries that no BDC or DP of the content read names are removed, inline dictionaries and references alike", "residual_in_region counts, fail closed"] {
        assert!(props.contains(needle), "PLR-104 sentence lacks {needle:?}");
    }
    for name in ["marked_content_property_lists_of_flattened_pages_are_purged", "property_lists_the_purge_cannot_clear_are_counted"] {
        assert!(props.contains(name), "the PLR-104 sentence does not cite {name}");
        assert!(include_str!("flatten_r13.rs").contains(&format!("fn {name}()")), "the cited test {name} does not exist");
    }
    for word in ["never", "always", "guarantee", "proves"] {
        assert!(!props.contains(word), "the PLR-104 sentence says {word:?}");
    }
}

/// V4-02 aggregates (R13-V1: the bounds the docs cite): the streams isolated for the renderer are
/// decoded against `Limits::max_decoded_total` (typed `InputLimit`), and image decoding inside the
/// renderer is charged to the page's decode budget before each image is decoded (`FlattenNote::WorkBudget`
/// once spent: one 300 x 300 image drawn 200 times at one pixel).
#[test]
fn flatten_aggregate_over_budget() {
    let mut red: Vec<String> = Vec::new();
    // (i) five 1 MiB streams against a 3 MiB total.
    let big = miniz_oxide::deflate::compress_to_vec_zlib(&vec![7u8; 1 << 20], 6);
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> /XObject << /I1 21 0 R /I2 22 0 R /I3 23 0 R /I4 24 0 R /I5 25 0 R >> >> >>".to_vec()),
        (4, stream_body("", format!("q 1 0 0 1 400 100 cm /I1 Do /I2 Do /I3 Do /I4 Do /I5 Do Q {SECRET}").as_bytes())),
        (6, HELVETICA.to_vec()),
    ];
    for n in 21..=25 {
        objs.push((n, stream_body("/Type /XObject /Subtype /Image /Width 1024 /Height 1024 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode", &big)));
    }
    let pdf = build(&objs);
    let tight = Limits { max_decoded_total: 3 << 20, ..quick() };
    let mut doc = PdfDoc::open(&pdf, &tight, &token()).expect("open");
    let res = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token());
    if !matches!(res, Err(PressError::InputLimit(_))) {
        red.push(format!("isolation over max_decoded_total not refused: {:?}", res.as_ref().err()));
    }
    // (ii) decode budget: 200 draws of a 300 x 300 image, 200000-pixel raster budget at 36 dpi.
    let draws: String = (0..200).map(|i| format!("q 1 0 0 1 {} 100 cm /Im1 Do Q ", 300 + i)).collect();
    let pdf = build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> /XObject << /Im1 35 0 R >> >> >>".to_vec()),
        (4, stream_body("", format!("{draws}{SECRET}").as_bytes())),
        (6, HELVETICA.to_vec()),
        (35, stream_body("/Type /XObject /Subtype /Image /Width 300 /Height 300 /ColorSpace /DeviceGray /BitsPerComponent 8", &vec![128u8; 90_000])),
    ]);
    let small = Limits { max_flatten_pixels: 200_000, ..quick() };
    let mut doc = PdfDoc::open(&pdf, &small, &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if !out.flattened[0].notes.contains(&FlattenNote::WorkBudget) {
        red.push(format!("200 x 90000 decoded pixels within a 12800000-pixel budget: notes {:?}", out.flattened[0].notes));
    }
    assert!(red.is_empty(), "{red:#?}");
}

// ------------------------------------------------------------------ SMB-PRESS-PROPERTIES (PLR-104, PLR-105)

/// Texts of the property lists only the flattened page names (PLR-104).
const PROP_TEXTS: [&str; 4] = ["PROP-ALT-SECRET", "PROP-ACTUAL-SECRET", "PROP-E-SECRET", "PROP-LANG-SECRET"];
/// The `/Properties` entries of the PLR-104 fixture: `/MC0` (inline) and `/MC2` (object 20) are named only by page one (flattened),
/// `/MC1` (inline, text `KEPT-PROP-TEXT`) and `/OC1` (the optional-content group 30) by page two (kept).
const PROPS: &str = "/MC0 << /Alt (PROP-ALT-SECRET) /ActualText (PROP-ACTUAL-SECRET) >> /MC1 << /ActualText (KEPT-PROP-TEXT) >> /MC2 20 0 R /OC1 30 0 R";

/// PLR-104 fixture (R25-P7): an untagged document; page one (flattened, index 0) draws `SECRET` inside `/Span /MC0 BDC .. EMC` and
/// has `/Note /MC2 DP`; page two (kept) draws its text inside `/Span /MC1 BDC .. EMC` and a square inside `/OC /OC1 BDC .. EMC`,
/// then `page_two_ops`. `layout` places the property lists: `shared` (one resources object 9 both pages name), `inherited` (an
/// inline `/Resources` on the `/Pages` node), `inherited-object` (the node names resources object 9), `properties-object` (each
/// page's own `/Resources` names one `/Properties` object 21), `lists-as-objects` (object 9, every property list an object),
/// `alias` (object 9's `/Properties` is alias object 24 of object 21: a spelling the purge does not edit).
fn properties_pdf(layout: &str, page_two_ops: &str, catalog_extra: &str, more: &[(u32, String)]) -> Vec<u8> {
    let inline_res = format!("<< /Font << /F1 6 0 R >> /Properties << {PROPS} >> >>");
    let (node, own) = match layout {
        "shared" => (String::new(), "/Resources 9 0 R".to_string()),
        "inherited" => (format!("/Resources {inline_res}"), String::new()),
        "inherited-object" => ("/Resources 9 0 R".to_string(), String::new()),
        "properties-object" | "alias-own" => (String::new(), "/Resources << /Font << /F1 6 0 R >> /Properties 21 0 R >>".to_string()),
        _ => (String::new(), "/Resources 9 0 R".to_string()),
    };
    let res9 = match layout {
        "lists-as-objects" => "<< /Font << /F1 6 0 R >> /Properties << /MC0 22 0 R /MC1 23 0 R /MC2 20 0 R /OC1 30 0 R >> >>".to_string(),
        "alias" => "<< /Font << /F1 6 0 R >> /Properties 24 0 R >>".to_string(),
        _ => inline_res.clone(),
    };
    let page_one = format!("/Span /MC0 BDC {SECRET} EMC /Note /MC2 DP");
    let page_two = format!("/Span /MC1 BDC BT /F1 12 Tf 72 700 Td (Page two) Tj ET EMC /OC /OC1 BDC 0 g 10 10 5 5 re f EMC {page_two_ops}");
    let mut objs: Vec<(u32, Vec<u8>)> = vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R /OCProperties << /OCGs [30 0 R] /D << /Order [30 0 R] >> >> {catalog_extra} >>").into_bytes()),
        (2, format!("<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 2 /MediaBox [0 0 612 792] {node} >>").into_bytes()),
        (3, format!("<< /Type /Page /Parent 2 0 R /Contents 4 0 R {own} >>").into_bytes()),
        (4, stream_body("", page_one.as_bytes())),
        (5, format!("<< /Type /Page /Parent 2 0 R /Contents 7 0 R {own} >>").into_bytes()),
        (6, HELVETICA.to_vec()),
        (7, stream_body("", page_two.as_bytes())),
        (9, res9.into_bytes()),
        (20, b"<< /E (PROP-E-SECRET) /Lang (PROP-LANG-SECRET) >>".to_vec()),
        (21, format!("<< {PROPS} >>").into_bytes()),
        (22, b"<< /Alt (PROP-ALT-SECRET) /ActualText (PROP-ACTUAL-SECRET) >>".to_vec()),
        (23, b"<< /ActualText (KEPT-PROP-TEXT) >>".to_vec()),
        (24, b"21 0 R".to_vec()),
        (30, b"<< /Type /OCG /Name (Layer one) >>".to_vec()),
    ];
    objs.extend(more.iter().map(|(n, b)| (*n, b.clone().into_bytes())));
    build(&objs)
}

fn properties_output(layout: &str, page_two_ops: &str, catalog_extra: &str, more: &[(u32, String)]) -> hsk_studio_press::RedactedOutput {
    let pdf = properties_pdf(layout, page_two_ops, catalog_extra, more);
    let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
    redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact")
}

/// The `/Properties` names a viewer reads for page `index` of the saved file (hayro-syntax, references followed).
fn property_names(bytes: &[u8], index: usize) -> Vec<String> {
    let pdf = Pdf::new(bytes.to_vec()).expect("hayro-syntax opens the saved file");
    let page = &pdf.pages()[index];
    page.raw().get::<HDict<'_>>(b"Resources").and_then(|r| r.get::<HDict<'_>>(b"Properties")).map(|p| keys(&p)).unwrap_or_default()
}

/// PLR-104 (R25-P7; Operator-confirmed plan 2026-10-14, lane SMB-PRESS-PROPERTIES): a marked-content property list of a flattened
/// page (`/Alt`, `/ActualText`, `/E`, `/Lang`) kept in a resources dictionary another page shares or inherits survived with the save
/// read proven (red at 1da2453: R25-P7 X2a shared, X2b inherited). `/Properties` is now a gated category: every entry the flattened
/// page reached that no kept `BDC` or `DP` names is purged (inline dictionaries and references), the entries the kept page names stay
/// (its own `/Span` text and its optional-content group), and the save reads flattened. Control: when the kept page names `/MC0`
/// too, `/MC0` stays (kept content invokes it) and the save still reads flattened.
#[test]
fn marked_content_property_lists_of_flattened_pages_are_purged() {
    let mut red: Vec<String> = Vec::new();
    for layout in ["shared", "inherited", "inherited-object", "properties-object", "lists-as-objects"] {
        let out = properties_output(layout, "", "", &[]);
        for text in PROP_TEXTS.iter().chain(&["SECRET-ALPHA"]) {
            if !gone(&out.bytes, text) {
                red.push(format!("{layout}: {text} survives"));
            }
        }
        if gone(&out.bytes, "KEPT-PROP-TEXT") {
            red.push(format!("{layout}: the property list the kept page names was purged"));
        }
        let names = property_names(&out.bytes, 1);
        if names.iter().any(|n| n == "MC0" || n == "MC2") || !names.iter().any(|n| n == "MC1") || !names.iter().any(|n| n == "OC1") {
            red.push(format!("{layout}: page two /Properties {names:?}, expected MC1 and OC1 only"));
        }
        if !read_as_flattened(&out) {
            red.push(format!("{layout}: region_flattened {} residual {} reasons {:?}", out.region_flattened(), out.residual_in_region, out.not_proven_reasons()));
        }
    }
    // V4-08: the names are read from the parsed operators. A name escape (`/MC#30`) names /MC0 (kept); a comment that holds the
    // operator and a name in another case (`/mc0`) do not (purged). The DP form names a property list as BDC does.
    for (what, ops, kept) in [
        ("/MC0 named by the kept page", "/Span /MC0 BDC EMC", true),
        ("/MC0 named through a name escape, /MC#30", "/Span /MC#30 BDC EMC", true),
        ("/MC0 named by DP", "/Note /MC0 DP", true),
        ("a comment holds /Span /MC0 BDC", "% /Span /MC0 BDC EMC\n", false),
        ("a name in another case, /mc0", "/Span /mc0 BDC EMC", false),
    ] {
        let out = properties_output("shared", ops, "", &[]);
        if gone(&out.bytes, "PROP-ALT-SECRET") == kept || !read_as_flattened(&out) {
            red.push(format!(
                "control, {what}: PROP-ALT-SECRET kept {} (expected {kept}) region_flattened {} reasons {:?}",
                !gone(&out.bytes, "PROP-ALT-SECRET"),
                out.region_flattened(),
                out.not_proven_reasons()
            ));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// PLR-104, fail closed: a property list of the flattened page that the purge cannot clear is counted (`residual_in_region`,
/// `NotProven::ResidualInRegion`, `region_flattened` false): (a) its object stays in the file because another object references it
/// and no kept content names it, while it holds `/E` and `/Lang` text; (b) the `/Properties` value is an alias object (a spelling
/// the purge does not edit) and an entry no kept content names holds text; (c) the kept page's content cannot be read, so which
/// entries it names is undetermined (every entry stays).
#[test]
fn property_lists_the_purge_cannot_clear_are_counted() {
    let unreadable = "BT /F1 12 Tf 72 650 Td (more) Tj ET 1 2 hskunknownop";
    let cases: Vec<(&str, hsk_studio_press::RedactedOutput, &[&str])> = vec![
        ("(a) the property list object is also referenced by the catalog", properties_output("shared", "", "/HskKeep 20 0 R", &[]), &["PROP-E-SECRET", "PROP-LANG-SECRET"]),
        ("(b) /Properties is an alias object", properties_output("alias", "", "", &[]), &PROP_TEXTS),
        ("(c) the kept page's content cannot be read", properties_output("shared", unreadable, "", &[]), &PROP_TEXTS),
    ];
    let mut red: Vec<String> = Vec::new();
    for (name, out, survive) in cases {
        for text in survive {
            if gone(&out.bytes, text) {
                red.push(format!("{name}: {text} was removed (the case needs it to survive)"));
            }
        }
        if out.region_flattened() || out.residual_in_region == 0 || !out.not_proven_reasons().contains(&hsk_studio_press::NotProven::ResidualInRegion) {
            red.push(format!("{name}: region_flattened {} residual {} reasons {:?}, expected counted", out.region_flattened(), out.residual_in_region, out.not_proven_reasons()));
        }
        if !gone(&out.bytes, "SECRET-ALPHA") {
            red.push(format!("{name}: SECRET-ALPHA survives"));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// PLR-105 (R25-P3, R25-P4, R25-P5; fail closed): the structure-shape scan counts what it cannot read instead of passing it.
/// `/Type /StructElem` or `/S` behind 15 alias objects (a chain the crate's reader reads as null after 16 hops), an object stream
/// without `/Type` that has `/N` and `/First`, an object stream whose bytes are not all members by its own header (`/N` too small,
/// bytes between the header and `/First`, `/N 0` with data), and a structure-element dictionary in the saved trailer's `/ID` each
/// read `region_flattened` false and `NotProven::ResidualInRegion`. Controls: 14 alias objects is read as before (counted through
/// the name), an object stream whose members cover its data with no shape reads flattened.
#[test]
fn unreadable_structure_shapes_are_counted() {
    let alias_chain = |first: u32, count: u32, end: &str| -> Vec<(u32, String)> {
        (0..count).map(|i| (first + i, format!("{} 0 R", first + i + 1))).chain([(first + count, end.to_string())]).collect()
    };
    let with = |head: (u32, String), chain: Vec<(u32, String)>| -> Vec<(u32, String)> {
        let mut v = vec![head];
        v.extend(chain);
        v
    };
    let raw_stream = |dict: &str, data: &str| String::from_utf8(stream_body(dict, data.as_bytes())).expect("ascii");
    let el = "<< /Type /StructElem /S /Figure /P 1 0 R /Alt (ALT-DIRECT-SECRET) >>";
    let note = "/HskNote 81 0 R";
    let junk = "/HskJunk 50 0 R";
    type Case = (&'static str, &'static str, Vec<(u32, String)>, bool);
    let cases: Vec<Case> = vec![
        ("R25-P3: /Type /StructElem behind 15 alias objects, no /P", note, with((81, "<< /Type 100 0 R /Alt (ALT-DIRECT-SECRET) >>".into()), alias_chain(100, 15, "/StructElem")), true),
        ("R25-P3: /S behind 15 alias objects, /P and text", note, with((81, "<< /S 100 0 R /P 1 0 R /Alt (ALT-DIRECT-SECRET) >>".into()), alias_chain(100, 15, "/Figure")), true),
        ("control: /Type /StructElem behind 14 alias objects", note, with((81, "<< /Type 100 0 R >>".into()), alias_chain(100, 14, "/StructElem")), true),
        ("R25-P4 K4: an object stream without /Type", junk, vec![(50, raw_stream("/N 1 /First 5", &format!("60 0 {el}")))], true),
        ("R25-P4 K5: /N 1 while the data holds a second member", junk, vec![(50, raw_stream("/Type /ObjStm /N 1 /First 5", &format!("60 0 << >> {el}")))], true),
        (
            "R25-P4 K6: the element in the gap between the header and /First",
            junk,
            vec![(50, raw_stream(&format!("/Type /ObjStm /N 1 /First {}", format!("60 0 {el} ").len()), &format!("60 0 {el} << >>")))],
            true,
        ),
        ("R25-P4 K11: /N 0 with members present", junk, vec![(50, raw_stream("/Type /ObjStm /N 0 /First 5", &format!("60 0 {el}")))], true),
        ("control: an object stream whose members cover its data, no shape", junk, vec![(50, raw_stream("/Type /ObjStm /N 2 /First 10", "60 0 61 6 << >> << /Hsk 1 >>"))], false),
    ];
    let mut red: Vec<String> = Vec::new();
    for (name, catalog_extra, objects, reported) in cases {
        let out = pages_output_with(&quick(), catalog_extra, "", "", &objects);
        if reported && !reported_for_the_tree(&out) {
            red.push(format!("{name}: region_flattened {} residual {} reasons {:?}, expected reported", out.region_flattened(), out.residual_in_region, out.not_proven_reasons()));
        }
        if !reported && !read_as_flattened(&out) {
            red.push(format!("{name}: region_flattened {} residual {} reasons {:?}, expected flattened", out.region_flattened(), out.residual_in_region, out.not_proven_reasons()));
        }
    }
    // R25-P5 X1: a structure-element dictionary in the trailer /ID is written into the saved trailer.
    let pdf = pages_pdf("", "", "", &[]);
    let at = find(&pdf, b"/Root 1 0 R >>").expect("trailer");
    let mut x1 = pdf[..at].to_vec();
    x1.extend_from_slice(format!("/Root 1 0 R /ID [<01> <02> {el}] >>").as_bytes());
    x1.extend_from_slice(&pdf[at + b"/Root 1 0 R >>".len()..]);
    let mut doc = PdfDoc::open(&x1, &quick(), &token()).expect("open");
    let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
    if !gone(&out.bytes, "ALT-DIRECT-SECRET") && !reported_for_the_tree(&out) {
        red.push(format!("R25-P5 X1: trailer /ID element kept, region_flattened {} reasons {:?}", out.region_flattened(), out.not_proven_reasons()));
    }
    assert!(red.is_empty(), "{red:#?}");
}
