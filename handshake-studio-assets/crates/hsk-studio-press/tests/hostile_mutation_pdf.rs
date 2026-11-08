//! SMB-PARSER-HOSTILE-1 item 6, PDF target: a deterministic mutation sweep
//! (`hsk-studio-interop-tiff/tests/support/hostile_mutator.rs`) over a file `PdfExport` writes (the
//! `tests/vector_read.rs` `fixture()` shape: fills, strokes, a clipped axial shading, a group, an
//! embedded TrueType font) and a hand-built uncompressed file (a form, a Type 1 font, an image, a
//! Separation space with a Type 2 function, an ExtGState) so stream contents are reachable. Each
//! mutant runs `PdfDoc::open` and `proposal`, `read_vector_document` and `render_page` at 0.25 px
//! per pt, with `Limits::default()`. A caught hayro panic (`vector_reader_panicked`) is a typed
//! answer with a third-party cause: counted (`caught_panics`), not a failure.
#[path = "../../hsk-studio-interop-tiff/tests/support/hostile_mutator.rs"]
mod hostile_mutator;

use hostile_mutator::*;
use hsk_studio_accord::CancellationToken;
use hsk_studio_press::*;
use std::collections::BTreeMap;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const CAP: usize = 256 << 20;

const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");

fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<PathCommand> {
    vec![PathCommand::MoveTo(x0, y0), PathCommand::LineTo(x1, y0), PathCommand::LineTo(x1, y1), PathCommand::LineTo(x0, y1), PathCommand::Close]
}

/// `tests/vector_read.rs` `fixture()`, copied.
fn exported() -> Vec<u8> {
    let mut ex = PdfExport::new(ExportOptions::default(), &Limits::default()).unwrap();
    let p = ex.add_page(PageSpec { media_box_pt: [0.0, 0.0, 200.0, 100.0], trim_box_pt: None, bleed_box_pt: None }).unwrap();
    ex.fill_path(p, &rect(10.0, 10.0, 60.0, 40.0), FillRule::EvenOdd, Colour::DeviceRgb([1.0, 0.0, 0.0])).unwrap();
    ex.stroke_path(p, &[PathCommand::MoveTo(10.0, 80.0), PathCommand::LineTo(190.0, 80.0)], Colour::DeviceRgb([0.0, 0.0, 1.0]), StrokeStyle { width_pt: 4.0, cap: LineCap::Round, join: LineJoin::Bevel }).unwrap();
    ex.push_clip(p, &rect(100.0, 10.0, 150.0, 40.0), FillRule::NonZero).unwrap();
    let stops = [GradientStop::new(0.0, [1.0, 1.0, 0.0]), GradientStop::new(1.0, [0.0, 1.0, 1.0])];
    let g = Gradient { geometry: GradientGeometry::Axial { from: [100.0, 0.0], to: [150.0, 0.0] }, stops: &stops, extend: [true, true], spread: GradientSpread::Pad, matrix: [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], profile: None };
    ex.fill_path_gradient(p, &rect(90.0, 0.0, 160.0, 50.0), FillRule::NonZero, &g).unwrap();
    ex.pop_clip(p).unwrap();
    ex.begin_group(p).unwrap();
    ex.fill_path(p, &rect(160.0, 10.0, 190.0, 40.0), FillRule::NonZero, Colour::DeviceRgb([0.0, 1.0, 0.0])).unwrap();
    ex.end_group(p, GroupStyle { opacity: 0.5, blend: BlendMode::Multiply, isolated: true, knockout: false, soft_mask: None }).unwrap();
    let font = ex.embed_truetype(INTER, 0, &[(161, 740)], &[(161, "H")]).unwrap();
    ex.show_glyphs(p, font, 24.0, [20.0, 50.0], Colour::DeviceGray(0.0), &[(161, 0.0, 0.0)]).unwrap();
    ex.finish(&CancellationToken::default()).unwrap().0
}

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

/// Uncompressed: every stream body is plain bytes the mutator can reach.
fn hand_built() -> Vec<u8> {
    build(&[
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> /XObject << /Fm1 7 0 R /Im1 8 0 R >> /ColorSpace << /CS0 [/Separation /Spot /DeviceCMYK 9 0 R] >> /ExtGState << /G0 << /CA 0.5 /ca 0.5 /BM /Multiply >> >> >> >>".to_vec(),
        ),
        (
            4,
            stream_body(
                "",
                b"q /G0 gs 1 0 0 rg 10 10 100 50 re f Q q /CS0 cs 0.7 scn 120 10 50 50 re f Q BT /F1 12 Tf 20 150 Td (Hello [world]) Tj [(A) -120 (B)] TJ ET q 2 w 0 0 1 RG 10 100 m 200 180 l S Q q /Fm1 Do Q q 40 0 0 40 200 100 cm /Im1 Do Q",
            ),
        ),
        (6, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".to_vec()),
        (7, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 50 50] /Matrix [1 0 0 1 250 20] /Resources << >>", b"0 g 0 0 m 50 0 l 25 40 l h f")),
        (8, stream_body("/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0, 85, 170, 255])),
        (9, b"<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.5 1 0] /N 1 >>".to_vec()),
    ])
}

const PROFILE: Profile<'static> = Profile {
    dictionary: &[
        b"/Length ", b"/Filter /FlateDecode", b"/Type /Page", b"/Kids [", b"/Count 999999", b"/Contents ", b"/Resources ", b"0 R", b"obj", b"endobj",
        b"stream\n", b"endstream", b"xref", b"trailer", b"startxref", b"<<", b">>", b"[", b"]", b"{", b"(", b"\\", b"/Width 65535",
        b"/Indexed 8 0 R", b"/FunctionType 4", b"/Prev 0", b"/Encrypt << >>", b" Do ", b" Tf ", b" cm ", b" BI /W 9 /H 9 ID ", b" EI ",
    ],
    text: true,
};

fn press_code(e: PressError) -> &'static str {
    match e {
        PressError::InputLimit(w) | PressError::Malformed(w) => w,
        other => other.code(),
    }
}

fn answer(b: &[u8]) -> Answer {
    let cancel = CancellationToken::default();
    let limits = Limits::default();
    let doc = PdfDoc::open(b, &limits, &cancel).map_err(press_code)?;
    doc.proposal(&cancel).map_err(press_code)?;
    read_vector_document(b, 64 << 20, &cancel).map_err(press_code)?;
    render_page(b, 0, 0.25, &limits, &cancel).map(|_| ()).map_err(|e| match e {
        ExportError::Press(p) => press_code(p),
        other => other.code(),
    })
}

#[test]
fn pdf_mutation_sweep_exported() {
    let seeds = vec![exported()];
    assert_eq!(answer(&seeds[0]), Ok(()), "seed reads");
    let report = sweep("pdf_exported", 0x9DF0_0000_0000_0001, &seeds, 300, PROFILE, CAP, answer);
    report.assert_reach(12);
}

#[test]
fn pdf_mutation_sweep_hand_built() {
    let seeds = vec![hand_built()];
    assert_eq!(answer(&seeds[0]), Ok(()), "seed reads");
    let report = sweep("pdf_hand_built", 0x9DF0_0000_0000_0002, &seeds, 300, PROFILE, CAP, answer);
    report.assert_reach(8);
}
