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

/// R16 (P-1): a painted region source that a kept page's drawn object, or a form that only a plain reference
/// reaches, merely REFERENCES: a plain key of a form the page draws, the `/Alternates` of an image it draws, and
/// an undrawn form (listed in `/Properties`) whose own content draws the source. None of them is a draw by a
/// kept page: the source stays and is reported.
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
        ("an undrawn form (/Properties) whose content draws it", page(""), own("/Properties << /P0 42 0 R >>"), vec![(42, undrawn_form_that_draws_it)]),
    ] {
        reported_kept_image(what, &painted_doc_with("inherited", &content, "", &resources, more), &mut red);
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R14-P1, R16, the other side: a painted source a kept page DRAWS (also when a catalog structure references it too), or
/// that the appearance stream of an annotation a kept page lists draws (`/AP` `/N`, `/R` or `/D`), is drawn content of
/// the kept file: it stays and is not reported.
#[test]
fn painted_sources_drawn_by_kept_pages_or_annotation_appearances_are_not_reported() {
    let text_only = "BT /F1 12 Tf 72 700 Td (Page two) Tj ET";
    let mut red: Vec<String> = Vec::new();
    // (a) page two draws the image and /AF lists it as well.
    let drawn = painted_doc_with(
        "inherited",
        "q 10 0 0 10 400 100 cm /Im1 Do Q BT /F1 12 Tf 72 700 Td (Page two) Tj ET",
        "/AF [20 0 R]",
        "",
        vec![(20, IMAGE_FILESPEC.to_vec())],
    );
    // (b) only the appearance stream of an annotation page two lists draws the image.
    let appearance = stream_body(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Resources << /XObject << /Im1 10 0 R >> >>",
        b"q 100 0 0 100 0 0 cm /Im1 Do Q",
    );
    let stamp = b"<< /Type /Annot /Subtype /Stamp /Rect [300 100 400 200] /P 5 0 R /AP << /N 31 0 R >> >>".to_vec();
    let annotated = painted_doc_with("inherited", text_only, "", "/Annots [30 0 R]", vec![(30, stamp), (31, appearance.clone())]);
    // (d) the same, the appearance stream listed as the rollover (/R) or the down (/D) appearance only.
    let [rollover, down] = ["R", "D"].map(|key| {
        let annot = format!("<< /Type /Annot /Subtype /Stamp /Rect [300 100 400 200] /P 5 0 R /AP << /{key} 31 0 R >> >>").into_bytes();
        painted_doc_with("inherited", text_only, "", "/Annots [30 0 R]", vec![(30, annot), (31, appearance.clone())])
    });
    // (c) the same, with the appearance stream behind a state dictionary and /AF listing the image too.
    let states = painted_doc_with(
        "inherited",
        text_only,
        "/AF [20 0 R]",
        "/Annots [30 0 R]",
        vec![
            (20, IMAGE_FILESPEC.to_vec()),
            (30, b"<< /Type /Annot /Subtype /Widget /FT /Btn /Rect [300 100 400 200] /P 5 0 R /AS /On /AP << /N 32 0 R >> >>".to_vec()),
            (31, appearance),
            (32, b"<< /On 31 0 R >>".to_vec()),
        ],
    );
    for (what, pdf) in [
        ("kept page draws it, /AF lists it", drawn),
        ("annotation appearance draws it", annotated),
        ("rollover (/AP /R) appearance draws it", rollover),
        ("down (/AP /D) appearance draws it", down),
        ("state-dictionary appearance draws it, /AF lists it", states),
    ] {
        let mut doc = PdfDoc::open(&pdf, &quick(), &token()).expect("open");
        let out = redact_and_save(&mut doc, &[region(R)], UnsupportedPolicy::Refuse, &token()).expect("redact");
        if !out.painted_sources_kept.is_empty() {
            red.push(format!("{what}: reported kept {:?}", out.painted_sources_kept));
        }
        if !out.region_flattened() {
            red.push(format!("{what}: region_flattened false (residual {}, not proven {:?})", out.residual_in_region, out.not_proven_reasons()));
        }
        if gone(&out.bytes, "SCAN-PIXELS-SECRET") {
            red.push(format!("{what}: a drawn image was dropped"));
        }
        if !gone(&out.bytes, "VECTOR-FORM-SECRET") {
            red.push(format!("{what}: the form nothing draws survives"));
        }
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R16: what a kept page draws includes what drawing renders along with the named object: the `/SMask` and `/Mask`
/// of an image (object 12), the `/Shading` of a shading pattern (15) and the `/SMask` `/G` group of a graphics state
/// (17). Page two draws the image, a shading, the shading pattern and the graphics state; the same resource
/// dictionary lists them on the flattened page one. Every one of them stays, nothing is reported (a shared logo with
/// an alpha mask is the usual case).
#[test]
fn painted_sources_rendered_with_a_drawn_object_are_not_reported() {
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
    if !out.painted_sources_kept.is_empty() {
        red.push(format!("reported kept {:?} (12 = the image's /SMask, 15 = the pattern's /Shading, 17 = the soft-mask group)", out.painted_sources_kept));
    }
    if !out.region_flattened() {
        red.push(format!("region_flattened false (residual {}, not proven {:?})", out.residual_in_region, out.not_proven_reasons()));
    }
    for marker in ["SCAN-PIXELS-SECRET", "MASK-PIXELS-SECRET", "SMASK-GROUP-SECRET"] {
        if gone(&out.bytes, marker) {
            red.push(format!("{marker}: a source page two draws was dropped"));
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

/// R17: kept page two takes the extra entries `keys` (no `/Annots`), the extra objects `more` and runs `content` before
/// its text; `annots` are the numbers of the annotations in `more` that it lists. The image (object 10) is placed or
/// named where a viewer does not draw it (or does not draw it as an image): it stays and is reported. The same
/// document in which a kept page also DRAWS the image keeps it silently: a draw, not a position, is what makes a
/// source certain. The draw is the appearance form (object 34) of one more annotation (33) page two lists: it names
/// the image in its own `/Resources`, so it does not depend on the page's resources the case replaces.
fn reported_unless_drawn(what: &str, keys: &str, content: &str, more: Vec<(u32, Vec<u8>)>, annots: &[u32], red: &mut Vec<String>) {
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
    silent_drawn_image(
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
    reported_unless_drawn("the image is page two's /Resources value", "/Resources 10 0 R", "", Vec::new(), &[], &mut red);
    let form = stream_body("/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources 10 0 R", b"0 g 0 0 10 10 re f");
    reported_unless_drawn(
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
        reported_unless_drawn(what, keys, "", Vec::new(), &[], &mut red);
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
        reported_unless_drawn(what, "", "", vec![(30, annot(ap))], &[30], &mut red);
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

/// R17 (lead decision, press rules): an appearance form that draws the region image counts as drawn only when a viewer
/// draws that appearance. A stream under `/AP` `/N` `/R` `/D` is the drawn one; in a state dictionary only the state the
/// annotation's `/AS` names is (no `/AS`, or one that names no state: none); an annotation whose `/F` has the Invisible
/// (1), Hidden (2) or NoView (32) bit draws nothing. Form 31 draws the image through its own `/Resources`, form 32 is a
/// blank. Each undrawn case keeps the image and reports it; the drawn cases (selected state, a flag that hides nothing)
/// keep it silently.
#[test]
fn painted_source_in_an_appearance_a_viewer_does_not_draw_is_reported() {
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
    ] {
        reported_kept_image_too(what, &pdf, &mut red);
    }
    for (what, pdf) in [
        ("the state /AS names (/AS /On)", doc("/AS /On", states)),
        ("the selected rollover state", doc("/AS /On", "/R << /On 31 0 R >>")),
        ("the stream itself, no flags", doc("", "/N 31 0 R")),
        ("a flag that hides nothing (/F 4, Print)", doc("/F 4", "/N 31 0 R")),
        ("Print and a selected state (/F 4 /AS /On)", doc("/F 4 /AS /On", states)),
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
        reported_unless_drawn(what, &keys, content, more, &[], &mut red);
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

/// R14-P1, R16 (V4-07): the DESCRIPTOR sentence about what happens to a painted source names the kept-by-reference
/// cases and cites tests that exist.
#[test]
fn descriptor_names_the_painted_source_kept_by_reference_case() {
    let source = include_str!("flatten_r13.rs");
    for needle in [
        "R14-P1",
        "R16",
        "R17",
        "unless a kept page, or an annotation appearance a kept page lists, draws them",
        "a kept object only references it or places it where a viewer draws nothing from it (a catalog /AF, a /Names or /Dests name tree, a kept page's /Resources entry outside /XObject /Pattern /Shading /ExtGState /Font that it does not draw, a plain key of an object a kept page draws, an object used as a /Resources value, as a gated sub-dictionary or as an appearance state, an appearance state the annotation's /AS does not select, the appearance of an Invisible, Hidden or NoView annotation, a name an operator uses for an object of another kind, any plain reference)",
        "the Do, sh, scn/SCN, gs and Tf targets of its content and of the content of what it draws in turn (forms, patterns, soft-mask groups, Type 3 glyph procedures), each of the kind its operator draws (Do an image or form stream, sh a shading, scn/SCN a pattern, gs and Tf a plain dictionary)",
        "the form XObject appearance streams of the annotations it lists that a viewer draws (/AP /N /R /D: the stream itself, or in a state dictionary only the state the annotation's /AS names, none without a matching /AS; none for an annotation whose /F has the Invisible, Hidden or NoView bit)",
        "painted_source_referenced_by_a_kept_catalog_structure_is_reported",
        "painted_source_referenced_but_not_drawn_by_a_kept_page_is_reported",
        "painted_source_referenced_by_a_key_of_a_drawn_object_is_reported",
        "painted_sources_rendered_with_a_drawn_object_are_not_reported",
        "painted_sources_drawn_by_kept_pages_or_annotation_appearances_are_not_reported",
        "painted_source_placed_as_a_resources_value_is_reported",
        "painted_source_placed_as_a_gated_sub_dictionary_is_reported",
        "painted_source_placed_as_an_appearance_state_is_reported",
        "painted_source_named_where_it_is_not_drawn_is_reported",
        "painted_source_shadowed_by_a_nearer_resource_entry_is_reported",
        "painted_source_in_an_appearance_a_viewer_does_not_draw_is_reported",
        "a name resolves to the nearest entry of its category in the resource chain, an entry a nearer entry of the same name shadows is reached, not drawn",
    ] {
        assert!(hsk_studio_press::DESCRIPTOR.contains(needle), "DESCRIPTOR lacks {needle:?}");
    }
    for cited in [
        "painted_source_referenced_by_a_kept_catalog_structure_is_reported",
        "painted_source_referenced_but_not_drawn_by_a_kept_page_is_reported",
        "painted_source_referenced_by_a_key_of_a_drawn_object_is_reported",
        "painted_sources_rendered_with_a_drawn_object_are_not_reported",
        "painted_sources_drawn_by_kept_pages_or_annotation_appearances_are_not_reported",
        "painted_source_placed_as_a_resources_value_is_reported",
        "painted_source_placed_as_a_gated_sub_dictionary_is_reported",
        "painted_source_placed_as_an_appearance_state_is_reported",
        "painted_source_named_where_it_is_not_drawn_is_reported",
        "painted_source_shadowed_by_a_nearer_resource_entry_is_reported",
        "painted_source_in_an_appearance_a_viewer_does_not_draw_is_reported",
    ] {
        assert!(source.contains(&format!("fn {cited}()")), "the cited test {cited} does not exist");
    }
    assert!(
        !hsk_studio_press::DESCRIPTOR.contains("dropped from the file unless a kept page draws them (redaction.flatten.purge"),
        "the uncorrected sentence"
    );
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
