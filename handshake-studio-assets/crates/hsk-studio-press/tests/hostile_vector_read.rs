//! SMB-PARSER-HOSTILE-1 item 3 (red) and item 4 (guard): the PDF reader routes that hand the
//! ORIGINAL file bytes to hayro (`read_vector_pages` / `read_vector_document`, press-read
//! `vector_read.rs`, and `render_page`, press-write `export/render.rs`) ran hayro unguarded: the
//! nesting bound of `render_bounds.rs` had one caller, the flatten. hayro-syntax 0.7.2 reads arrays
//! and dictionaries recursively without a depth limit, hayro-interpret 0.7.0 parses Type 4
//! procedures recursively and builds colour spaces and functions recursively without a cycle
//! guard, and hayro-postscript recurses per `<<` token of a run. A stack overflow aborts the process;
//! the 256 MiB reader stack (`RENDER_STACK_BYTES`) is margin, not the guard. Own test binary: an
//! abort takes it down, which counts as the test failing.
//!
//! Shapes after `tests/flatten_r13.rs` `renderer_input_is_bounded_before_rendering`; 4,000,000
//! levels so the depth passes a 256 MiB stack at any frame of 67 bytes or more per level. Every
//! case runs on the three routes (`render_page` at 0.25 px per pt). After item 4: a nesting or
//! cycle violation is `PressError::InputLimit("depth")` on every route, a file `PdfDoc::open`
//! refuses is refused with that error, and an image the renderer would allocate past its budget is
//! `InputLimit("render_image")` on the render route only (the vector recorder never decodes an
//! image: hayro-interpret `ImageXObject::new` reads no sample, `Recorder::draw_image` notes it).
use hsk_studio_accord::CancellationToken;
use hsk_studio_press::{ExportError, Limits, PressError, read_vector_document, read_vector_pages, render_page};
use std::collections::BTreeMap;

const LEVELS: usize = 4_000_000;

fn stream_body(dict_extra: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict_extra} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream");
    v
}

fn flate_body(dict_extra: &str, data: &[u8]) -> Vec<u8> {
    stream_body(&format!("{dict_extra} /Filter /FlateDecode"), &miniz_oxide::deflate::compress_to_vec_zlib(data, 6))
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

/// One page with `resources` and a FlateDecode content stream `content`, plus `extra` objects.
fn page(resources: &str, content: &[u8], extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> {resources} >> >>").into_bytes()),
        (4, flate_body("", content)),
        (6, HELVETICA.to_vec()),
    ];
    objs.extend(extra);
    build(&objs)
}

const BOX: &[u8] = b"q 0 0 1 rg 100 100 50 50 re f Q";

fn content_arrays() -> Vec<u8> {
    page("", &vec![b'['; LEVELS], vec![])
}
fn form_arrays() -> Vec<u8> {
    page("/XObject << /Fm1 30 0 R >>", b"q /Fm1 Do Q", vec![(30, flate_body("/Type /XObject /Subtype /Form /BBox [0 0 10 10]", &vec![b'['; LEVELS]))])
}
fn type4_procs() -> Vec<u8> {
    let mut procs = vec![b'{'; LEVELS];
    procs.extend(vec![b'}'; LEVELS]);
    page("/ColorSpace << /CS0 [/Separation /Spot /DeviceGray 60 0 R] >>", b"q /CS0 cs 1 scn 400 100 10 10 re f Q", vec![(60, flate_body("/FunctionType 4 /Domain [0 1] /Range [0 1]", &procs))])
}
fn cmap_dicts() -> Vec<u8> {
    page(
        "/Font << /F1 6 0 R /F2 61 0 R >>",
        b"BT /F2 12 Tf 72 300 Td (Public) Tj ET",
        vec![(61, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /ToUnicode 62 0 R >>".to_vec()), (62, flate_body("", &b"<<".repeat(LEVELS)))],
    )
}
fn indexed_self() -> Vec<u8> {
    page("/ColorSpace << /CS1 63 0 R >>", b"q /CS1 cs 0 sc 400 200 10 10 re f Q", vec![(63, b"[/Indexed 63 0 R 0 <00>]".to_vec())])
}
fn indexed_cycle() -> Vec<u8> {
    page("/ColorSpace << /CS1 63 0 R >>", b"q /CS1 cs 0 sc 400 200 10 10 re f Q", vec![(63, b"[/Indexed 64 0 R 0 <00>]".to_vec()), (64, b"[/Indexed 63 0 R 0 <00>]".to_vec())])
}
fn stitching_self() -> Vec<u8> {
    page(
        "/ColorSpace << /CS0 [/Separation /Spot /DeviceGray 60 0 R] >>",
        b"q /CS0 cs 1 scn 400 100 10 10 re f Q",
        vec![(60, b"<< /FunctionType 3 /Domain [0 1] /Functions [60 0 R] /Bounds [] /Encode [0 1] >>".to_vec())],
    )
}
/// Press's reader refuses it (the page's resources nest past `Limits::max_depth`); hayro reads
/// the same object recursively.
fn unopenable_deep() -> Vec<u8> {
    let mut objs = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources 5 0 R >>".to_vec()),
        (4, flate_body("", BOX)),
    ];
    objs.push((5, vec![b'['; LEVELS]));
    build(&objs)
}
fn big_image() -> Vec<u8> {
    page(
        "/XObject << /Im1 40 0 R >>",
        b"q 100 0 0 100 0 0 cm /Im1 Do Q",
        vec![(40, stream_body("/Type /XObject /Subtype /Image /Width 60000 /Height 60000 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0; 16]))],
    )
}
fn within_bound() -> Vec<u8> {
    let content = format!("/Tag << /A {}{} >> BDC {} EMC", "[".repeat(30), "]".repeat(30), std::str::from_utf8(BOX).unwrap());
    page("", content.as_bytes(), vec![])
}

fn token() -> CancellationToken {
    CancellationToken::default()
}
fn pages(pdf: &[u8]) -> Result<usize, PressError> {
    read_vector_pages(pdf, 64 << 20, &token()).map(|p| p.len())
}
fn document(pdf: &[u8]) -> Result<usize, PressError> {
    read_vector_document(pdf, 64 << 20, &token()).map(|d| d.pages.len())
}
fn render(pdf: &[u8]) -> Result<(u32, u32), ExportError> {
    render_page(pdf, 0, 0.25, &Limits::default(), &token()).map(|r| (r.width, r.height))
}

const DEPTH: PressError = PressError::InputLimit("depth");

macro_rules! routes {
    ($case:ident, $vector:expr, $render:expr) => {
        mod $case {
            use super::*;
            #[test]
            fn read_vector_pages_route() {
                assert_eq!(pages(&$case()), $vector);
            }
            #[test]
            fn read_vector_document_route() {
                assert_eq!(document(&$case()), $vector);
            }
            #[test]
            fn render_page_route() {
                assert_eq!(render(&$case()), $render);
            }
        }
    };
}

routes!(content_arrays, Err(DEPTH), Err(ExportError::Press(DEPTH)));
routes!(form_arrays, Err(DEPTH), Err(ExportError::Press(DEPTH)));
routes!(type4_procs, Err(DEPTH), Err(ExportError::Press(DEPTH)));
routes!(cmap_dicts, Err(DEPTH), Err(ExportError::Press(DEPTH)));
routes!(indexed_self, Err(DEPTH), Err(ExportError::Press(DEPTH)));
routes!(indexed_cycle, Err(DEPTH), Err(ExportError::Press(DEPTH)));
routes!(stitching_self, Err(DEPTH), Err(ExportError::Press(DEPTH)));
routes!(unopenable_deep, Err(DEPTH), Err(ExportError::Press(DEPTH)));
routes!(big_image, Ok(1), Err(ExportError::Press(PressError::InputLimit("render_image"))));
routes!(within_bound, Ok(1), Ok((153, 198)));

/// Measurement probe (item 3: the smallest aborting count of content arrays). Not a gate:
/// `HOSTILE_PROBE_DEPTH=<n>` reads one page whose content is `n` nested arrays.
#[test]
#[ignore = "measurement probe, run by hand with HOSTILE_PROBE_DEPTH"]
fn content_arrays_probe() {
    let Some(n) = std::env::var("HOSTILE_PROBE_DEPTH").ok().and_then(|v| v.parse::<usize>().ok()) else {
        println!("NOT_PROVEN: HOSTILE_PROBE_DEPTH not set");
        return;
    };
    println!("PROBE pdf content arrays n={n} answer={:?}", pages(&page("", &vec![b'['; n], vec![])));
}
