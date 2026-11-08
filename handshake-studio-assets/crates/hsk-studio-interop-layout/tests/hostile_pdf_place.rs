//! SMB-PARSER-HOSTILE-1 item 3 (red): `place_pdf_page` opens with `PdfDoc::open` (which does not read
//! stream contents) and then reads the page through Press `read_vector_document`, whose hayro reader
//! was unguarded. A page content of 4,000,000 nested arrays (FlateDecode, a few KiB on disk) must be
//! a typed refusal after item 4, not an abort. Own test binary: an abort takes it down.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_layout::{PdfCrop, PlaceError, place_pdf_page};

fn deep_page() -> Vec<u8> {
    let data = miniz_oxide::deflate::compress_to_vec_zlib(&vec![b'['; 4_000_000], 6);
    let bodies: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> >>".to_vec(),
        {
            let mut v = format!("<< /Filter /FlateDecode /Length {} >>\nstream\n", data.len()).into_bytes();
            v.extend_from_slice(&data);
            v.extend_from_slice(b"\nendstream");
            v
        },
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in bodies.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).into_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", bodies.len() + 1).into_bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n \n").into_bytes());
    }
    out.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", bodies.len() + 1).into_bytes());
    out
}

#[test]
fn place_deep_content_is_a_typed_refusal() {
    let r = place_pdf_page(&deep_page(), 0, PdfCrop::Media, 64 << 20, &CancellationToken::default()).map(|_| ());
    assert_eq!(r, Err(PlaceError::Read("input_limit")));
}
