//! EPS stage 1 (lane SMB-DXF-EPS-IMPORT): DOS binary header, DSC comments and the EPSI preview,
//! read without running PostScript. Fixtures are generated here.
use hsk_studio_accord::CancellationToken;
use hsk_studio_interop_vector_core::{DOS_EPS_MAGIC, EPS_ROUTE, EpsPreview, ImportPolicy, LossKind, VectorError, import_eps, import_eps_preview, read_eps};

fn policy() -> ImportPolicy {
    ImportPolicy::default()
}

/// A DOS binary EPS: 30-byte header, then the PostScript, WMF and TIFF sections.
fn dos_eps(ps: &[u8], wmf: &[u8], tiff: &[u8]) -> Vec<u8> {
    let mut out = DOS_EPS_MAGIC.to_vec();
    let ps_at = 30u32;
    let wmf_at = ps_at + ps.len() as u32;
    let tiff_at = wmf_at + wmf.len() as u32;
    for v in [ps_at, ps.len() as u32, if wmf.is_empty() { 0 } else { wmf_at }, wmf.len() as u32, if tiff.is_empty() { 0 } else { tiff_at }, tiff.len() as u32] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.extend_from_slice(&[0xFF, 0xFF]);
    out.extend_from_slice(ps);
    out.extend_from_slice(wmf);
    out.extend_from_slice(tiff);
    out
}

const BODY: &str = "%%EndComments\nnewpath 10 10 moveto 90 10 lineto 90 40 lineto closepath fill\nshowpage\n%%EOF\n";

/// DOS header sections and DSC values: the TIFF preview is returned byte-exact, the high-resolution
/// box wins, fonts come from `%%DocumentNeededResources` and its `%%+` continuation; the vector
/// import refuses naming the route and preserving the bytes.
#[test]
fn dos_header_and_dsc_comments_are_read() {
    let ps = format!(
        "%!PS-Adobe-3.0 EPSF-3.0\r\n%%BoundingBox: 0 0 100 50\r\n%%HiResBoundingBox: 0 0 99.5 49.25\r\n%%LanguageLevel: 2\r\n%%DocumentNeededResources: font Helvetica\r\n%%+ font Times-Roman procset Foo\r\n{}",
        BODY
    );
    let tiff = b"II*\0fake-tiff-bytes".to_vec();
    let wmf = b"\xD7\xCD\xC6\x9Afake-wmf".to_vec();
    let file = dos_eps(ps.as_bytes(), &wmf, &tiff);
    let info = read_eps(&file, &policy()).expect("read eps");
    assert!(info.dos_header);
    assert_eq!(info.epsf_version.as_deref(), Some("EPSF-3.0"));
    assert_eq!(info.bounding_box, Some([0, 0, 100, 50]));
    assert_eq!(info.bounds(), Some([0.0, 0.0, 99.5, 49.25]));
    assert_eq!(info.language_level, Some(2));
    assert_eq!(info.fonts, vec!["Helvetica".to_owned(), "Times-Roman".to_owned()]);
    assert_eq!(&file[info.postscript.clone()], ps.as_bytes());
    let import = import_eps_preview(&file, &policy(), &CancellationToken::default()).expect("preview import");
    assert_eq!(import.preview, Some(EpsPreview::Tiff(tiff)));
    assert!(import.loss.iter().any(|l| l.code == "eps_vector_not_decoded" && l.kind == LossKind::Unsupported));
    assert!(import.loss.iter().any(|l| l.code == "eps_preview_raster"));
    assert_eq!(import.preserved.bytes, file);
    match import_eps(&file, &policy(), &CancellationToken::default()) {
        Err(VectorError::EpsVectorNotDecoded { route, preserved }) => {
            assert_eq!(route, EPS_ROUTE);
            assert_eq!(preserved.bytes, file);
        }
        other => panic!("expected eps_vector_not_decoded, got {other:?}"),
    }
    // WMF only: the metafile is the preview.
    let wmf_only = dos_eps(ps.as_bytes(), &wmf, &[]);
    let import = import_eps_preview(&wmf_only, &policy(), &CancellationToken::default()).expect("wmf preview");
    assert_eq!(import.preview, Some(EpsPreview::Wmf(wmf)));
}

/// EPS content conversion is a typed refusal (remediation 2026-10-21): the route names that the
/// PostScript interpreter route was removed for licensing and that DSC data and the preview stay
/// available; the error is `eps_vector_not_decoded` (Unavailable) with the bytes preserved, a plain
/// PostScript file is refused the same way, and the DSC data and the EPSI preview of the same file
/// are still read.
#[test]
fn eps_conversion_refusal_names_the_removed_interpreter_route() {
    assert!(EPS_ROUTE.contains("postscript_interpreter_route_removed_for_licensing"), "{EPS_ROUTE}");
    assert!(EPS_ROUTE.contains("read_eps") && EPS_ROUTE.contains("import_eps_preview"), "{EPS_ROUTE}");
    let epsi = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 2 1\n%%EndComments\n%%BeginPreview: 2 1 8 1\n% 00 ff\n%%EndPreview\nnewpath 0 0 moveto 2 1 lineto stroke\nshowpage\n%%EOF\n";
    for bytes in [epsi.as_bytes(), b"%!PS-Adobe-3.0\n%%BoundingBox: 0 0 10 10\nshowpage\n".as_slice()] {
        match import_eps(bytes, &policy(), &CancellationToken::default()) {
            Err(e @ VectorError::EpsVectorNotDecoded { .. }) => {
                assert_eq!(e.code(), "eps_vector_not_decoded");
                assert_eq!(e.failure_code(), Some(hsk_studio_observe::FailureCode::Unavailable));
                let VectorError::EpsVectorNotDecoded { route, preserved } = e else { unreachable!() };
                assert_eq!(route, EPS_ROUTE);
                assert_eq!(preserved.bytes, bytes);
                assert_eq!(preserved.kind, "eps");
            }
            other => panic!("expected eps_vector_not_decoded, got {other:?}"),
        }
    }
    let info = read_eps(epsi.as_bytes(), &policy()).expect("dsc data");
    assert_eq!(info.bounding_box, Some([0, 0, 2, 1]));
    let preview = import_eps_preview(epsi.as_bytes(), &policy(), &CancellationToken::default()).expect("preview");
    assert_eq!(preview.preview, Some(EpsPreview::Epsi { width: 2, height: 1, depth: 8, gray: vec![0, 255] }));
    // Damaged input is still a malformed-file error, not the conversion refusal.
    assert_eq!(import_eps(b"<svg/>", &policy(), &CancellationToken::default()).err(), Some(VectorError::Unsupported("not_an_eps_file")));
}

/// EPSI previews decode to 8-bit gray (depth 1 and 8, rows padded to bytes); `(atend)` boxes come
/// from the trailer; a file with no preview reports `eps_preview_missing`.
#[test]
fn epsi_preview_and_atend_boxes() {
    let epsi = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: (atend)\n%%EndComments\n%%BeginPreview: 10 2 1 2\n% A5C0\n% FF00\n%%EndPreview\nshowpage\n%%Trailer\n%%BoundingBox: 1 2 11 4\n%%EOF\n";
    let import = import_eps_preview(epsi.as_bytes(), &policy(), &CancellationToken::default()).expect("epsi");
    assert_eq!(import.info.bounding_box, Some([1, 2, 11, 4]));
    let Some(EpsPreview::Epsi { width, height, depth, gray }) = import.preview else { panic!("epsi preview") };
    assert_eq!((width, height, depth), (10, 2, 1));
    // A5 C0 = 1010 0101 11 (10 pixels), FF 00 = 1111 1111 00.
    assert_eq!(&gray[..10], &[255, 0, 255, 0, 0, 255, 0, 255, 255, 255]);
    assert_eq!(&gray[10..], &[255, 255, 255, 255, 255, 255, 255, 255, 0, 0]);
    let gray8 = "%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 2 1\n%%EndComments\n%%BeginPreview: 2 1 8 1\n% 00 7f\n%%EndPreview\n";
    let import = import_eps_preview(gray8.as_bytes(), &policy(), &CancellationToken::default()).expect("epsi 8");
    assert_eq!(import.preview, Some(EpsPreview::Epsi { width: 2, height: 1, depth: 8, gray: vec![0, 127] }));
    let plain = format!("%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 50\n{BODY}");
    let import = import_eps_preview(plain.as_bytes(), &policy(), &CancellationToken::default()).expect("no preview");
    assert_eq!(import.preview, None);
    assert!(import.loss.iter().any(|l| l.code == "eps_preview_missing"));
}

/// DSC is tokenised by line, at the start of a line, case-sensitive: a decoy inside a comment, a
/// lowercase key, a value after `%%EndComments` and one inside a PostScript string are ignored;
/// tabs and CR / CRLF line ends are accepted.
#[test]
fn dsc_is_line_tokenised_against_decoys() {
    let decoys = "%!PS-Adobe-3.0 EPSF-3.0\r% %%BoundingBox: 0 0 999 999\r%%boundingbox: 0 0 888 888\r%%BoundingBox:\t5 6\t 7  8\r%%EndComments\r(%%HiResBoundingBox: 0 0 1 1) pop\r%%HiResBoundingBox: 0 0 2 2\rshowpage\r";
    let info = read_eps(decoys.as_bytes(), &policy()).expect("decoys");
    assert_eq!(info.bounding_box, Some([5, 6, 7, 8]));
    assert_eq!(info.hires_bounding_box, None, "values after %%EndComments are not header values");
    // A box without four numbers, an empty box and a fractional integer box are typed errors.
    for bad in ["%%BoundingBox: 0 0 10", "%%BoundingBox: 10 10 0 0", "%%BoundingBox: 0 0 1.5 2"] {
        let text = format!("%!PS-Adobe-3.0 EPSF-3.0\n{bad}\n%%EndComments\n");
        assert!(matches!(read_eps(text.as_bytes(), &policy()), Err(VectorError::MalformedEps(_))), "{bad}");
    }
    assert_eq!(read_eps(b"<svg/>", &policy()).err(), Some(VectorError::Unsupported("not_an_eps_file")));
}

/// Hostile EPS: preview sizes are checked before allocation (u32::MAX side, area over budget,
/// declared data larger than the file), DOS sections out of bounds or overflowing are typed errors,
/// the input size is capped.
#[test]
fn hostile_eps_is_bounded() {
    let preview = |decl: &str| format!("%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 1 1\n%%EndComments\n%%BeginPreview: {decl}\n% FF\n%%EndPreview\n");
    let run = |text: String| import_eps_preview(text.as_bytes(), &policy(), &CancellationToken::default()).err();
    assert_eq!(run(preview("4294967295 1 8 1")), Some(VectorError::BudgetExceeded("epsi_preview_side")));
    assert_eq!(run(preview("16384 16384 8 1")), Some(VectorError::BudgetExceeded("epsi_preview_pixels")));
    assert_eq!(run(preview("4096 4096 8 1")), Some(VectorError::MalformedEps("epsi_data_truncated")));
    assert_eq!(run(preview("1 1 3 1")), Some(VectorError::MalformedEps("epsi_depth")));
    let overrun = "%!PS-Adobe-3.0 EPSF-3.0\n%%EndComments\n%%BeginPreview: 1 1 8 1\n% FFFF\n%%EndPreview\n".to_owned();
    assert_eq!(run(overrun), Some(VectorError::MalformedEps("epsi_data_overrun")));
    // DOS sections: beyond the file, and offset + length overflowing u32 arithmetic.
    let mut beyond = dos_eps(b"%!PS-Adobe-3.0 EPSF-3.0\n", &[], &[]);
    beyond[20..24].copy_from_slice(&40u32.to_le_bytes());
    beyond[24..28].copy_from_slice(&1000u32.to_le_bytes());
    assert_eq!(read_eps(&beyond, &policy()).err(), Some(VectorError::MalformedEps("dos_tiff_section")));
    let mut overflow = dos_eps(b"%!PS-Adobe-3.0 EPSF-3.0\n", &[], &[]);
    overflow[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    overflow[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
    assert_eq!(read_eps(&overflow, &policy()).err(), Some(VectorError::MalformedEps("dos_postscript_section")));
    assert_eq!(read_eps(&DOS_EPS_MAGIC, &policy()).err(), Some(VectorError::MalformedEps("dos_header")));
    let mut small = policy();
    small.limits.max_bytes = 16;
    assert_eq!(read_eps(b"%!PS-Adobe-3.0 EPSF-3.0\n%%EOF\n", &small).err(), Some(VectorError::BudgetExceeded("bytes")));
}
