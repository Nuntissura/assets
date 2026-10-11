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
    let res = "<< /XObject << /Im1 10 0 R /Fm9 11 0 R >> /Font << /F1 6 0 R >> >>";
    let page_one = format!("q 612 0 0 792 0 0 cm /Im1 Do Q q /Fm9 Do Q BT /F1 12 Tf 72 300 Td (Public) Tj ET {SECRET}");
    let (node, own) = match layout {
        "shared" => (String::new(), "/Resources 9 0 R".to_string()),
        _ => (format!("/Resources {res}"), String::new()),
    };
    let kids = if layout == "single" { "[3 0 R] /Count 1" } else { "[3 0 R 5 0 R] /Count 2" };
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, format!("<< /Type /Pages /Kids {kids} /MediaBox [0 0 612 792] {node} >>").into_bytes()),
        (3, format!("<< /Type /Page /Parent 2 0 R /Contents 4 0 R {own} >>").into_bytes()),
        (4, stream_body("", page_one.as_bytes())),
        (6, HELVETICA.to_vec()),
        (9, res.as_bytes().to_vec()),
        (10, stream_body("/Type /XObject /Subtype /Image /Width 18 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"SCAN-PIXELS-SECRET")),
        (11, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /HskMarker (VECTOR-FORM-SECRET)", b"0 g 100 690 50 20 re f")),
    ];
    if layout != "single" {
        objs.push((5, format!("<< /Type /Page /Parent 2 0 R /Contents 7 0 R {own} >>").into_bytes()));
        objs.push((7, stream_body("", page_two.as_bytes())));
    }
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

/// R13-P1 (lead decision: no structure content, no widgets): the `/Alt` of a structure element on the
/// flattened page and the value of a widget of that page (outside the region) are gone; the structure of
/// the other page stays.
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
    if !out.region_flattened() {
        red.push(format!("region_flattened false (residual {})", out.residual_in_region));
    }
    assert!(red.is_empty(), "{red:#?}");
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
