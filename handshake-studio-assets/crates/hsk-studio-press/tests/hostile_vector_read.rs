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

// ----- lead rulings 2026-11-08 (F4-F7): the reader-bounds pass never refuses a big legitimate print
// file, but still stops hostile amplification; every refusal is typed. -------------------------------

/// A page (media box 200 x 200) that draws one image XObject `/Im1` with the given dictionary keys
/// and data. `filter` empty for an uncompressed (raw) image.
fn image_page(w: u32, h: u32, cs: &str, bpc: u32, filter: &str, data: &[u8]) -> Vec<u8> {
    let f = if filter.is_empty() { String::new() } else { format!(" /Filter /{filter}") };
    let dict = format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /{cs} /BitsPerComponent {bpc}{f}");
    build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /XObject << /Im1 40 0 R >> >> >>".to_vec()),
        (4, flate_body("", b"q 200 0 0 200 0 0 cm /Im1 Do Q")),
        (40, stream_body(&dict, data)),
    ])
}

/// F5: print-resolution images are not refused. The reader render route checks the pixel count
/// (<= 150 Mpx) from the dictionary, at any colour space and bit depth, and never decodes the image
/// whole, so a 300 dpi A3 photo as Flate CMYK 8-bit, the same at 16-bit, and A2 RGB at 300 dpi all
/// render. The data is deliberately tiny: the backing-length check is off on this route.
#[test]
fn print_resolution_images_render() {
    // 300 dpi A3 = 3508 x 4961 = 17.4 Mpx; A2 RGB = 4961 x 7016 = 34.8 Mpx.
    for (w, h, cs, bpc) in [(3508u32, 4961u32, "DeviceCMYK", 8u32), (3508, 4961, "DeviceCMYK", 16), (4961, 7016, "DeviceRGB", 8)] {
        let pdf = image_page(w, h, cs, bpc, "FlateDecode", &miniz_oxide::deflate::compress_to_vec_zlib(&[0u8; 64], 1));
        assert!(render(&pdf).is_ok(), "{w}x{h} {cs} {bpc}-bit Flate render: {:?}", render(&pdf));
        // The vector routes never decode images: they read the page.
        assert_eq!(pages(&pdf), Ok(1));
    }
}

/// F5: an image past 150 Mpx is a typed refusal on the render route, and the vector routes (which
/// never decode an image) still read.
fn huge_image() -> Vec<u8> {
    // 20000 x 10000 = 200 Mpx.
    image_page(20_000, 10_000, "DeviceGray", 8, "FlateDecode", &miniz_oxide::deflate::compress_to_vec_zlib(&[0u8; 64], 1))
}
routes!(huge_image, Ok(1), Err(ExportError::Press(PressError::InputLimit("render_image"))));

/// F6: a CIDFont's `/CIDToGIDMap` stream and an `/ICCBased` profile stream are data, never scanned as
/// PDF syntax; bytes that look like deep nesting in them do not refuse the file.
fn cid_to_gid_map() -> Vec<u8> {
    // The CIDToGIDMap data is `[` repeated far past the depth cap: a syntax scan would refuse it.
    let deep = vec![b'['; 100_000];
    build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F0 6 0 R >> >> >>".to_vec()),
        (4, flate_body("", b"BT /F0 12 Tf 20 100 Td <0001> Tj ET")),
        (6, b"<< /Type /Font /Subtype /Type0 /BaseFont /Sub /Encoding /Identity-H /DescendantFonts [7 0 R] >>".to_vec()),
        (7, b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Sub /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /CIDToGIDMap 8 0 R >>".to_vec()),
        (8, stream_body("", &deep)),
    ])
}

fn inline_icc_profile() -> Vec<u8> {
    // The ICC profile stream holds `<<` runs past the cap; it is referenced by an [/ICCBased N 0 R]
    // array directly in the page's (inline) Resources, which `binary_streams` alone would miss.
    let deep = b"<<".repeat(100_000);
    build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /ColorSpace << /CS0 [/ICCBased 8 0 R] >> >> >>".to_vec()),
        (4, flate_body("", b"q /CS0 cs 0 0 0 0 scn 10 10 50 50 re f Q")),
        (8, stream_body("/N 4", &deep)),
    ])
}

#[test]
fn binary_data_streams_are_not_scanned_as_syntax() {
    assert_eq!(pages(&cid_to_gid_map()), Ok(1), "CIDToGIDMap");
    assert_eq!(pages(&inline_icc_profile()), Ok(1), "inline ICCBased");
    assert!(render(&cid_to_gid_map()).is_ok());
    assert!(render(&inline_icc_profile()).is_ok());
}

/// Lead ruling 2026-11-08 (SMB-PARSER-HOSTILE-1 entry): a CIDFont's `/CIDSet` stream and a
/// halftone stream (`/HT` of an `/ExtGState`, HalftoneType 6) are data, never scanned as syntax.
fn cid_set() -> Vec<u8> {
    let deep = vec![b'['; 100_000];
    build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /Font << /F0 6 0 R >> >> >>".to_vec()),
        (4, flate_body("", b"BT /F0 12 Tf 20 100 Td <0001> Tj ET")),
        (6, b"<< /Type /Font /Subtype /Type0 /BaseFont /Sub /Encoding /Identity-H /DescendantFonts [7 0 R] >>".to_vec()),
        (7, b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Sub /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor 9 0 R >>".to_vec()),
        (8, stream_body("", &deep)),
        (9, b"<< /Type /FontDescriptor /FontName /Sub /Flags 4 /FontBBox [0 0 1000 1000] /ItalicAngle 0 /Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 /CIDSet 8 0 R >>".to_vec()),
    ])
}

fn halftone() -> Vec<u8> {
    let deep = b"<<".repeat(100_000);
    build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << /ExtGState << /GS0 << /Type /ExtGState /HT 8 0 R >> >> >> >>".to_vec()),
        (4, flate_body("", b"q /GS0 gs 0 0 1 rg 100 100 50 50 re f Q")),
        (8, stream_body("/Type /Halftone /HalftoneType 6 /Width 4 /Height 4", &deep)),
    ])
}

#[test]
fn cid_set_and_halftone_streams_are_data() {
    for (name, pdf) in [("CIDSet", cid_set()), ("halftone", halftone())] {
        assert_eq!(pages(&pdf), Ok(1), "{name} read_vector_pages");
        assert_eq!(document(&pdf), Ok(1), "{name} read_vector_document");
        assert!(render(&pdf).is_ok(), "{name} render_page: {:?}", render(&pdf));
    }
}

/// Lead ruling required pass: a 300 dpi A2 RGB photo as JPEG (4961 x 7016 = 34.8 Mpx) renders. The
/// codec header declares the full size; the bound reads it and never decodes the image whole.
#[test]
fn a2_rgb_jpeg_renders() {
    let (w, h) = (4961u16, 7016u16);
    // SOI, SOF0 (8-bit, height, width, 3 components), EOI: the header the bound sizes the image from.
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
    jpeg.extend_from_slice(&h.to_be_bytes());
    jpeg.extend_from_slice(&w.to_be_bytes());
    jpeg.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 0, 3, 0x11, 0, 0xFF, 0xD9]);
    let pdf = image_page(u32::from(w), u32::from(h), "DeviceRGB", 8, "DCTDecode", &jpeg);
    assert!(render(&pdf).is_ok(), "4961x7016 DeviceRGB JPEG render: {:?}", render(&pdf));
    assert_eq!(pages(&pdf), Ok(1));
}

/// Lead ruling required refusal: a stream of at most 1 KiB that inflates past 256 MiB is a typed
/// decompression-bomb refusal on every route (chained Flate stages: one deflate pass cannot exceed
/// a ratio of about 1032, so the deflated zeros are deflated again until the stream is at most 1 KiB).
fn flate_bomb_1k() -> Vec<u8> {
    let zeros = vec![0u8; 260 << 20];
    let mut stream = miniz_oxide::deflate::compress_to_vec_zlib(&zeros, 1);
    drop(zeros);
    let mut stages = 1;
    while stream.len() > 1024 && stages < 5 {
        stream = miniz_oxide::deflate::compress_to_vec_zlib(&stream, 10);
        stages += 1;
    }
    assert!(stream.len() <= 1024, "the bomb stream is {} bytes after {stages} stages, not at most 1 KiB", stream.len());
    let filters = vec!["/FlateDecode"; stages].join(" ");
    build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << >> >>".to_vec()),
        (4, stream_body(&format!("/Filter [{filters}]"), &stream)),
    ])
}
routes!(flate_bomb_1k, Err(RATIO), Err(ExportError::Press(RATIO)));

/// Lead ruling: a bad Flate tail keeps base behaviour WITH A WARNING. The page still reads and
/// renders (the `bad_flate_tail` rows); every route also says so: the vector routes note
/// `pdf_stream_tail_damaged` on the pages, `render_page` notes `stream_tail_damaged`. An undamaged
/// file carries neither.
#[test]
fn bad_flate_tail_reads_with_a_warning() {
    let damaged = bad_flate_tail();
    let has = |notes: &[hsk_studio_press::VectorNote]| notes.iter().any(|n| n.code() == "pdf_stream_tail_damaged");
    let p = read_vector_pages(&damaged, 64 << 20, &token()).expect("pages");
    assert!(p.iter().all(|pg| has(&pg.notes)), "read_vector_pages notes: {:?}", p.iter().map(|pg| &pg.notes).collect::<Vec<_>>());
    let d = read_vector_document(&damaged, 64 << 20, &token()).expect("document");
    assert!(d.pages.iter().all(|pg| has(&pg.notes)), "read_vector_document notes: {:?}", d.pages.iter().map(|pg| &pg.notes).collect::<Vec<_>>());
    let r = render_page(&damaged, 0, 0.25, &Limits::default(), &token()).expect("render");
    assert!(r.notes.contains(&"stream_tail_damaged"), "render_page notes: {:?}", r.notes);
    let clean = within_bound();
    assert!(!read_vector_pages(&clean, 64 << 20, &token()).expect("clean pages").iter().any(|pg| has(&pg.notes)));
    assert!(!render_page(&clean, 0, 0.25, &Limits::default(), &token()).expect("clean render").notes.contains(&"stream_tail_damaged"));
}

/// F7: a damaged Flate tail keeps the prefix decoded so far and is not a document refusal.
fn bad_flate_tail() -> Vec<u8> {
    let mut deflated = miniz_oxide::deflate::compress_to_vec_zlib(BOX, 6);
    // Corrupt the Adler-32 trailer (last 4 bytes): the prefix still decodes to BOX.
    let n = deflated.len();
    deflated[n - 1] ^= 0xFF;
    build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << >> >>".to_vec()),
        (4, stream_body("/Filter /FlateDecode", &deflated)),
    ])
}
routes!(bad_flate_tail, Ok(1), Ok((50, 50)));

/// F7: a stream that inflates past 256 MiB at a ratio above 1024 is a typed decompression-bomb
/// refusal; the decode is stopped at the ratio cap, so it never allocates the whole bomb.
fn flate_bomb() -> Vec<u8> {
    // 260 MiB of a single byte compresses to ~257 KiB (ratio ~1035): past the 256 MiB floor and the
    // 1024 ratio.
    let zeros = vec![0u8; 260 << 20];
    build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R /Resources << >> >>".to_vec()),
        (4, stream_body("/Filter /FlateDecode", &miniz_oxide::deflate::compress_to_vec_zlib(&zeros, 1))),
    ])
}
const RATIO: PressError = PressError::InputLimit("decoded_ratio");
routes!(flate_bomb, Err(RATIO), Err(ExportError::Press(RATIO)));

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

/// The oracle corpus root (`.config/oracle-corpus.toml`, the workspace convention; no env vars).
fn corpus_root() -> Option<std::path::PathBuf> {
    let ws = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let cfg = std::fs::read_to_string(ws.join(".config/oracle-corpus.toml")).ok()?;
    let value = |key: &str| cfg.lines().find_map(|l| l.split_once('=').filter(|(k, _)| k.trim() == key).map(|(_, v)| v.trim().trim_matches('"').to_owned()));
    let (search, max) = (value("search")?, value("max-ancestors")?.parse::<usize>().ok()?);
    let ws = ws.canonicalize().ok()?;
    ws.ancestors().skip(1).take(max).map(|a| a.join(&search)).find(|p| p.is_dir())
}

/// Item 4 stop rule (lead ruling OQ1): every `.ai` and `.pdf` file of the oracle corpus through the
/// two guarded routes, one line per file, run before (base source) and after the guard and compared
/// by hand. Not a gate.
#[test]
#[ignore = "measurement probe, run by hand before and after the guard"]
fn corpus_pdfs_before_after_probe() {
    let Some(root) = corpus_root() else {
        println!("NOT_PROVEN: corpus_pdfs_before_after_probe reason=oracle-corpus root not found");
        return;
    };
    let mut stack = vec![root.clone()];
    let mut files = Vec::new();
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("ai") || x.eq_ignore_ascii_case("pdf")) {
                files.push(p);
            }
        }
    }
    files.sort();
    for p in files {
        let bytes = std::fs::read(&p).expect("corpus file");
        let started = std::time::Instant::now();
        let doc = document(&bytes);
        let read_ms = started.elapsed().as_millis();
        println!("CORPUS file={} bytes={} read_vector_document={doc:?} ms={read_ms} render_page={:?}", p.strip_prefix(&root).unwrap_or(&p).display(), bytes.len(), render(&bytes));
    }
}
