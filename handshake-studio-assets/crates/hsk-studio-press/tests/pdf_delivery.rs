//! Focused press tests (<= 5). Every PDF is built in-test from literal bytes; the byte-level
//! oracles (verbatim object bytes, raw scan, independent zlib inflate of every `stream` body)
//! do not use the crate's parser.
use hsk_studio_accord::CancellationToken;
use hsk_studio_press::{
    ArtifactSink, Limits, ObjRef, PdfDoc, PdfScalar, PressError, RedactionClass, RedactionRegion, SinkError, UnsupportedPolicy,
    redact_and_save, sha256_hex, verify_payload_absent,
};
use std::collections::BTreeMap;

// ---------------------------------------------------------------- builders

fn stream_body(dict_extra: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("<< {dict_extra} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream");
    v
}

fn obj_bytes(n: u32, body: &[u8]) -> Vec<u8> {
    let mut v = format!("{n} 0 obj\n").into_bytes();
    v.extend_from_slice(body);
    v.extend_from_slice(b"\nendobj\n");
    v
}

fn build(objs: &[(u32, Vec<u8>)], trailer_extra: &str, junk: &[u8]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut off: BTreeMap<u32, usize> = BTreeMap::new();
    for (n, body) in objs {
        off.insert(*n, out.len());
        out.extend(obj_bytes(*n, body));
    }
    out.extend_from_slice(junk);
    let size = off.keys().max().unwrap() + 1;
    let xref = out.len();
    out.extend(format!("xref\n0 {size}\n0000000000 65535 f \n").into_bytes());
    for n in 1..size {
        match off.get(&n) {
            Some(o) => out.extend(format!("{o:010} 00000 n \n").into_bytes()),
            None => out.extend_from_slice(b"0000000000 00001 f \n"),
        }
    }
    out.extend(format!("trailer\n<< /Size {size} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n").into_bytes());
    out
}

fn last_startxref(bytes: &[u8]) -> usize {
    let at = bytes.windows(9).rposition(|w| w == b"startxref").unwrap();
    std::str::from_utf8(&bytes[at + 9..]).unwrap().split_whitespace().next().unwrap().parse().unwrap()
}

fn incremental(base: &[u8], objs: &[(u32, Vec<u8>)], size: u32, trailer_extra: &str) -> Vec<u8> {
    let prev = last_startxref(base);
    let mut out = base.to_vec();
    let mut offs = Vec::new();
    for (n, body) in objs {
        offs.push((*n, out.len()));
        out.extend(obj_bytes(*n, body));
    }
    let xref = out.len();
    out.extend_from_slice(b"xref\n");
    for (n, o) in offs {
        out.extend(format!("{n} 1\n{o:010} 00000 n \n").into_bytes());
    }
    out.extend(format!("trailer\n<< /Size {size} /Root 1 0 R /Prev {prev} {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n").into_bytes());
    out
}

const CONTENT: &str = "BT /F1 12 Tf 72 700 Td (SECRET-ALPHA) Tj ET\nBT /F1 12 Tf 72 400 Td [(Public ) -250 (text)] TJ ET";

fn base_objs(content: &[u8], info: &str) -> Vec<(u32, Vec<u8>)> {
    vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Annots [5 0 R] /Resources << /Font << /F1 6 0 R >> /XObject << /Im1 8 0 R >> >> >>"
                .to_vec(),
        ),
        (4, stream_body("", content)),
        (5, b"<< /Type /Annot /Subtype /Text /Rect [72 695 120 715] /Contents (note-SECRET-NOTE) /P 3 0 R >>".to_vec()),
        (6, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()),
        (7, if info.is_empty() { b"<< >>".to_vec() } else { info.as_bytes().to_vec() }),
        (8, stream_body("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0u8])),
    ]
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Independent oracle: raw marker scan for stream bodies, zlib inflate (raw bytes if not zlib).
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

fn token() -> CancellationToken {
    CancellationToken::default()
}

/// Default limits, with a 36 dpi flattening raster (Operator decision D27): these tests read structure
/// and text, not pixels, and a debug build draws and proves a 150 dpi page in about two seconds.
fn limits() -> Limits {
    #[allow(unused_mut)]
    let mut l = Limits::default();
    l.flatten_dpi = 36; // D27-new-api
    l
}

fn open(bytes: &[u8]) -> PdfDoc {
    PdfDoc::open(bytes, &limits(), &token()).expect("open")
}

// ------------------------------------------------------------------- tests

#[test]
fn pdf_object_edit_save_independent_reopen() {
    let bytes = build(&base_objs(CONTENT.as_bytes(), "<< /Title (Old Title) /Author (Test Author) >>"), "/Info 7 0 R", b"");
    let mut doc = open(&bytes);
    let insp = doc.inspect();
    assert_eq!((insp.page_count, insp.encrypted, insp.signed, insp.xref_sections), (1, false, false, 1));
    assert_eq!(insp.source_sha256, sha256_hex(&bytes));

    let info = ObjRef { num: 7, generation: 0 };
    let receipt = doc.edit_entry(info, "Title", PdfScalar::Text("New Title".into()), &insp.source_sha256, &token()).unwrap();
    assert_eq!(receipt.key, "Title");
    let out = doc.save_full(&token()).unwrap();

    // Independent byte-level evidence (no crate parser).
    assert!(find(&out, b"/Title (New Title)").is_some());
    assert!(find(&out, b"Old Title").is_none());
    for n in 1..=6u32 {
        let body = base_objs(CONTENT.as_bytes(), "")[n as usize - 1].1.clone();
        assert!(find(&out, &obj_bytes(n, &body)).is_some(), "object {n} must be byte-identical");
    }
    assert_eq!(out.windows(9).filter(|w| *w == b"startxref").count(), 1, "one fresh xref, no incremental tail");

    // Reopen with the crate: edit visible, structure unchanged.
    let again = open(&out);
    let proposal = again.proposal(&token()).unwrap();
    assert_eq!(proposal.pages.len(), 1);
    assert_eq!(again.inspect().page_count, 1);
    assert_eq!(proposal.pages[0].annotations.len(), 1);
    assert_eq!(proposal.pages[0].annotations[0].subtype, "Text");
    assert_eq!(proposal.pages[0].media_box, Some([0.0, 0.0, 612.0, 792.0]));
}

#[test]
fn redacted_text_absent_all_reachable_streams() {
    let objs = {
        let mut o = base_objs(CONTENT.as_bytes(), "<< /Title (SECRET-OLD-TITLE) >>");
        o.push((9, b"<< /Old (SECRET-ORPHAN) >>".to_vec())); // listed in xref, unreachable
        o
    };
    let junk = obj_bytes(10, b"<< /Old (SECRET-UNLISTED) >>"); // present in bytes, not in xref
    let v1 = build(&objs, "/Info 7 0 R", &junk);
    // Incremental update replaces the Info object; the old revision stays in the file bytes.
    let v2 = incremental(&v1, &[(7, b"<< /Title (Clean Title) >>".to_vec())], 11, "/Info 7 0 R");
    assert!(find(&v2, b"SECRET-OLD-TITLE").is_some() && find(&v2, b"SECRET-UNLISTED").is_some());

    let mut doc = open(&v2);
    assert_eq!(doc.inspect().xref_sections, 2);
    let region = RedactionRegion { page_index: 0, rect: [60.0, 690.0, 200.0, 720.0] };
    let out = redact_and_save(&mut doc, &[region], UnsupportedPolicy::Refuse, &token()).unwrap();

    let report = &out.reports[0];
    assert_eq!(report.removed_text, vec![b"SECRET-ALPHA".to_vec()]);
    assert_eq!(report.annotations_removed, vec![ObjRef { num: 5, generation: 0 }]);
    // R10-F6 (lead decision 2026-10-11T15:00:20Z): a standard font without /Widths advances by its AFM
    // widths, exactly, so the removed string is replaced by its exact advance and nothing shifts.
    assert!(report.complete && !report.layout_shift_possible, "standard font without /Widths: exact AFM advance");
    assert!(out.proof.is_clean(), "{:?}", out.proof);
    assert!(out.is_proven());
    assert_eq!((out.short_needles, out.residual_in_region), (0, 0));
    assert_eq!(out.proof.xref_sections, 1);

    // Independent oracle over ALL bytes: raw + every inflated stream body.
    let needles: [&[u8]; 5] = [b"SECRET-ALPHA", b"SECRET-NOTE", b"SECRET-OLD-TITLE", b"SECRET-ORPHAN", b"SECRET-UNLISTED"];
    let streams = independent_streams(&out.bytes);
    for n in needles {
        assert!(find(&out.bytes, n).is_none(), "raw bytes still hold {:?}", String::from_utf8_lossy(n));
        assert!(streams.iter().all(|s| find(s, n).is_none()), "a decoded stream still holds {:?}", String::from_utf8_lossy(n));
    }
    // Text outside the region survived, in the rewritten (deflated) content stream.
    assert!(streams.iter().any(|s| find(s, b"[(Public ) -250 (text)] TJ").is_some()));
    // The crate's own verifier agrees on a wider needle set (previous revision + orphans).
    let wide: Vec<Vec<u8>> = needles.iter().map(|n| n.to_vec()).collect();
    let proof = verify_payload_absent(&out.bytes, &wide, &Limits::default(), &token()).unwrap();
    assert!(proof.is_clean() && proof.hits.is_empty() && proof.streams_decoded >= 2);
    // The verifier is not vacuous: it finds the strings in the un-redacted input revision.
    let before = verify_payload_absent(&v2, &wide, &Limits::default(), &token()).unwrap();
    assert!(!before.is_clean() && !before.hits.is_empty());
    assert!(before.unreachable_objects.contains(&9) && before.unlisted_object_markers >= 1 && before.xref_sections == 2);

    // Reopen the redacted file: page and public text intact, annotation gone.
    let again = open(&out.bytes);
    let proposal = again.proposal(&token()).unwrap();
    assert_eq!(proposal.pages[0].annotations.len(), 0);

    // Unsupported classes fail closed: an image under the region cannot be proven removed.
    let img_page = build(&base_objs(b"q 100 0 0 100 50 650 cm /Im1 Do Q", ""), "", b"");
    let mut doc = open(&img_page);
    let err = doc.redact_region(&region, UnsupportedPolicy::Refuse, &token()).unwrap_err();
    assert_eq!(err, PressError::UnsupportedRedactionClass(RedactionClass::Image));
    let partial = doc.redact_region(&region, UnsupportedPolicy::ReportAndContinue, &token()).unwrap();
    assert!(!partial.complete && partial.unredacted == vec![RedactionClass::Image]);
}

struct MemSink {
    last_good: Vec<u8>,
    staging: Vec<u8>,
    fail_commit: bool,
    aborted: u32,
}
impl ArtifactSink for MemSink {
    fn begin(&mut self) -> Result<(), SinkError> {
        self.staging.clear();
        Ok(())
    }
    fn write(&mut self, bytes: &[u8]) -> Result<(), SinkError> {
        self.staging.extend_from_slice(bytes);
        Ok(())
    }
    fn commit(&mut self) -> Result<(), SinkError> {
        if self.fail_commit {
            return Err(SinkError);
        }
        self.last_good = std::mem::take(&mut self.staging);
        Ok(())
    }
    fn abort(&mut self) {
        self.staging.clear();
        self.aborted += 1;
    }
}

#[test]
fn signed_readonly_cancel_stale_save_fault() {
    let plain = build(&base_objs(CONTENT.as_bytes(), "<< /Title (T) >>"), "/Info 7 0 R", b"");
    let sha = sha256_hex(&plain);
    let info = ObjRef { num: 7, generation: 0 };
    let region = RedactionRegion { page_index: 0, rect: [60.0, 690.0, 200.0, 720.0] };

    // Signed: AcroForm signature field with a /ByteRange value dictionary.
    let mut signed_objs = base_objs(CONTENT.as_bytes(), "<< /Title (T) >>");
    signed_objs[0].1 = b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /SigFlags 3 /Fields [9 0 R] >> >>".to_vec();
    signed_objs.push((9, b"<< /FT /Sig /T (Sig1) /V << /Type /Sig /ByteRange [0 10 20 30] /Contents <00112233> >> >>".to_vec()));
    let signed = build(&signed_objs, "/Info 7 0 R", b"");
    let mut doc = open(&signed);
    assert!(doc.inspect().signed && doc.inspect().has_byte_range_signature);
    let signed_sha = doc.inspect().source_sha256;
    assert_eq!(doc.edit_entry(info, "Title", PdfScalar::Text("X".into()), &signed_sha, &token()), Err(PressError::SignedDocument));
    assert_eq!(doc.redact_region(&region, UnsupportedPolicy::Refuse, &token()).unwrap_err(), PressError::SignedDocument);
    assert_eq!(doc.save_full(&token()), Err(PressError::SignedDocument));

    // Encrypted: modify bit clear -> PermissionDenied; set -> Encrypted (no decryption in this slice).
    for (p, expect) in [(-64i64, PressError::PermissionDenied), (-4, PressError::Encrypted)] {
        let mut objs = base_objs(CONTENT.as_bytes(), "<< /Title (T) >>");
        objs.push((9, format!("<< /Filter /Standard /V 1 /R 2 /P {p} >>").into_bytes()));
        let mut d = open(&build(&objs, "/Info 7 0 R /Encrypt 9 0 R", b""));
        assert!(d.inspect().encrypted);
        assert_eq!(d.inspect().permissions_allow_modify, Some(p == -4));
        assert_eq!(d.save_full(&token()), Err(expect.clone()));
        assert_eq!(d.edit_entry(info, "Title", PdfScalar::Text("X".into()), &d.inspect().source_sha256, &token()), Err(expect));
    }

    // Stale source hash, disallowed key, missing object, stream target, cancellation.
    let mut doc = open(&plain);
    let bad = "0".repeat(64);
    assert_eq!(doc.edit_entry(info, "Title", PdfScalar::Text("X".into()), &bad, &token()), Err(PressError::StaleSource));
    assert_eq!(doc.edit_entry(info, "Producer", PdfScalar::Text("X".into()), &sha, &token()), Err(PressError::KeyNotAllowed));
    assert_eq!(
        doc.edit_entry(ObjRef { num: 99, generation: 0 }, "Title", PdfScalar::Text("X".into()), &sha, &token()),
        Err(PressError::NoSuchObject)
    );
    assert_eq!(
        doc.edit_entry(ObjRef { num: 4, generation: 0 }, "Title", PdfScalar::Text("X".into()), &sha, &token()),
        Err(PressError::UnsupportedObject)
    );
    let cancelled = token();
    cancelled.cancel();
    assert_eq!(doc.edit_entry(info, "Title", PdfScalar::Text("X".into()), &sha, &cancelled), Err(PressError::Canceled));
    assert_eq!(PdfDoc::open(&plain, &Limits::default(), &cancelled).err(), Some(PressError::Canceled));

    // Unsupported stream filter on the page content is a typed refusal.
    let lzw = base_objs(b"\x80\x0b\x60\x50\x22", "");
    let mut lzw_objs = lzw;
    lzw_objs[3].1 = stream_body("/Filter /DCTDecode", b"\x80\x0b\x60\x50\x22");
    let mut doc = open(&build(&lzw_objs, "", b""));
    assert_eq!(
        doc.redact_region(&region, UnsupportedPolicy::Refuse, &token()).unwrap_err(),
        PressError::UnsupportedFilter("DCTDecode".into())
    );

    // Save fault: commit fails -> abort called, last-good artifact untouched.
    let mut doc = open(&plain);
    doc.edit_entry(info, "Title", PdfScalar::Text("Edited".into()), &sha, &token()).unwrap();
    let mut sink = MemSink { last_good: b"LAST-GOOD".to_vec(), staging: Vec::new(), fail_commit: true, aborted: 0 };
    assert_eq!(doc.save_to(&mut sink, &token()), Err(PressError::Sink(SinkError)));
    assert_eq!((sink.last_good.as_slice(), sink.aborted), (b"LAST-GOOD".as_slice(), 1));
    sink.fail_commit = false;
    let receipt = doc.save_to(&mut sink, &token()).unwrap();
    assert_eq!(receipt.sha256_hex, sha256_hex(&sink.last_good));
    assert!(find(&sink.last_good, b"/Title (Edited)").is_some());
}

#[test]
fn hostile_pdf_bounds() {
    let objs = base_objs(CONTENT.as_bytes(), "<< /Title (T) >>");
    let good = build(&objs, "/Info 7 0 R", b"");
    let open_with = |b: &[u8], l: &Limits| PdfDoc::open(b, l, &token()).err();

    // Oversized input and object-count bound.
    let tiny = Limits { max_bytes: 100, ..Limits::default() };
    assert_eq!(open_with(&good, &tiny), Some(PressError::InputLimit("max_bytes")));
    let few = Limits { max_objects: 3, ..Limits::default() };
    assert_eq!(open_with(&good, &few), Some(PressError::InputLimit("max_objects")));

    // Truncated xref (cut inside the table), missing startxref, garbage.
    let cut = good.windows(4).position(|w| w == b"xref").unwrap() + 30;
    assert!(matches!(open_with(&good[..cut], &Limits::default()), Some(PressError::Malformed(_))));
    assert!(matches!(open_with(b"%PDF-1.7\nnot a pdf at all", &Limits::default()), Some(PressError::Malformed(_))));
    assert!(matches!(open_with(b"", &Limits::default()), Some(PressError::Malformed(_))));

    // Cyclic page tree and a /Prev loop.
    let mut cyc = objs.clone();
    cyc[1].1 = b"<< /Type /Pages /Kids [2 0 R] /Count 1 >>".to_vec();
    assert_eq!(open_with(&build(&cyc, "", b""), &Limits::default()), Some(PressError::Malformed("page_tree_cycle")));
    let looped = {
        let mut b = good.clone();
        let sx = last_startxref(&b);
        let at = b.windows(7).rposition(|w| w == b"trailer").unwrap();
        let tail = format!("trailer\n<< /Size 9 /Root 1 0 R /Prev {sx} >>\nstartxref\n{sx}\n%%EOF\n");
        b.truncate(at);
        b.extend_from_slice(tail.as_bytes());
        b
    };
    assert_eq!(open_with(&looped, &Limits::default()), Some(PressError::Malformed("xref_loop")));

    // Depth bomb: deeply nested arrays in one object.
    let mut deep = objs.clone();
    deep[5].1 = format!("{}{}", "[".repeat(5000), "]".repeat(5000)).into_bytes();
    assert_eq!(open_with(&build(&deep, "", b""), &Limits::default()), Some(PressError::InputLimit("depth")));

    // Decompression bomb in page content: 8 MiB of zeros deflated to a few KB, 1 MiB cap.
    let bomb = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 8 << 20], 6);
    assert!(bomb.len() < 64 * 1024);
    let mut bomb_objs = base_objs(b"", "");
    bomb_objs[3].1 = stream_body("/Filter /FlateDecode", &bomb);
    let limits = Limits { max_decoded_bytes: 1 << 20, ..Limits::default() };
    let mut doc = PdfDoc::open(&build(&bomb_objs, "", b""), &limits, &token()).unwrap();
    let region = RedactionRegion { page_index: 0, rect: [0.0, 0.0, 10.0, 10.0] };
    assert_eq!(doc.redact_region(&region, UnsupportedPolicy::Refuse, &token()).unwrap_err(), PressError::InputLimit("decoded_bytes"));
    assert_eq!(doc.redact_region(&RedactionRegion { page_index: 5, ..region }, UnsupportedPolicy::Refuse, &token()).unwrap_err(), PressError::NoSuchPage);
}

#[test]
fn xref_stream_object_stream_predictor_and_object_model() {
    // Object stream (obj 5) holding catalog 1, pages 2, page 3, field 6 and an unsigned sig field 7;
    // xref stream (obj 8) with PNG "Up" predictor rows (W [1 2 1]).
    let members: Vec<(u32, &str)> = vec![
        (1, "<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [6 0 R 7 0 R] >> >>"),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R /Annots [6 0 R] /Rotate 90 >>"),
        (6, "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Ff 1 /V (Ada) /Rect [10 10 50 30] >>"),
        (7, "<< /FT /Sig /T (sign) /Rect [0 0 0 0] >>"),
    ];
    let mut header = String::new();
    let mut body = String::new();
    for (n, text) in &members {
        header.push_str(&format!("{n} {} ", body.len()));
        body.push_str(text);
        body.push('\n');
    }
    let first = header.len();
    let objstm_plain = format!("{header}{body}").into_bytes();
    let objstm_data = miniz_oxide::deflate::compress_to_vec_zlib(&objstm_plain, 6);

    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let off4 = out.len();
    out.extend(obj_bytes(4, &stream_body("", b"BT /F1 9 Tf 10 50 Td (Hi) Tj ET")));
    let off5 = out.len();
    out.extend(obj_bytes(
        5,
        &stream_body(&format!("/Type /ObjStm /N {} /First {first} /Filter /FlateDecode", members.len()), &objstm_data),
    ));
    let off8 = out.len();
    // Rows: type, field2 (2 bytes), field3.
    let rows: Vec<[u8; 4]> = vec![
        [0, 0, 0, 255],                                   // 0 free
        [2, 0, 5, 0],                                     // 1 in objstm 5 index 0
        [2, 0, 5, 1],                                     // 2
        [2, 0, 5, 2],                                     // 3
        [1, (off4 >> 8) as u8, off4 as u8, 0],            // 4
        [1, (off5 >> 8) as u8, off5 as u8, 0],            // 5
        [2, 0, 5, 3],                                     // 6
        [2, 0, 5, 4],                                     // 7
        [1, (off8 >> 8) as u8, off8 as u8, 0],            // 8 (the xref stream itself)
    ];
    let mut pred = Vec::new();
    let mut prev = [0u8; 4];
    for r in &rows {
        pred.push(2u8);
        for i in 0..4 {
            pred.push(r[i].wrapping_sub(prev[i]));
        }
        prev = *r;
    }
    let xdata = miniz_oxide::deflate::compress_to_vec_zlib(&pred, 6);
    out.extend(obj_bytes(
        8,
        &stream_body(
            "/Type /XRef /Size 9 /W [1 2 1] /Root 1 0 R /Filter /FlateDecode /DecodeParms << /Predictor 12 /Columns 4 >>",
            &xdata,
        ),
    ));
    out.extend(format!("startxref\n{off8}\n%%EOF\n").into_bytes());

    let doc = open(&out);
    let insp = doc.inspect();
    assert_eq!((insp.page_count, insp.object_count, insp.signed, insp.encrypted), (1, 8, false, false));
    let model = doc.proposal(&token()).unwrap();
    assert_eq!(model.pages[0].rotate, 90);
    assert_eq!(model.pages[0].media_box, Some([0.0, 0.0, 100.0, 100.0]));
    assert_eq!(model.pages[0].annotations[0].subtype, "Widget");
    assert_eq!(model.form_fields.len(), 2);
    let name = model.form_fields.iter().find(|f| f.name == "name").unwrap();
    assert_eq!((name.field_type.as_str(), name.read_only, name.has_value), ("Tx", true, true));
    assert_eq!(model.signatures.len(), 1);
    assert!(!model.signatures[0].signed);

    // Full save expands object-stream members into a classic, fresh, reachable-only file.
    let saved = doc.save_full(&token()).unwrap();
    assert!(find(&saved, b"/ObjStm").is_none());
    let again = open(&saved);
    assert_eq!(again.inspect().page_count, 1);
    assert_eq!(again.proposal(&token()).unwrap().form_fields.len(), 2);
    assert!(find(&saved, b"(Ada)").is_some());
}

// ----------------------------------------------------------------- slice 2

use hsk_studio_press::{PayloadLocation, SanitizePolicy, sanitize_full_rewrite};

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

fn form_doc(page_content: &str, extra: Vec<(u32, Vec<u8>)>, xobjects: &str) -> Vec<u8> {
    let mut objs = base_objs(page_content.as_bytes(), "<< /Title (T) >>");
    objs[2].1 = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> /XObject << {xobjects} >> >> >>"
    )
    .into_bytes();
    objs.extend(extra);
    build(&objs, "/Info 7 0 R", b"")
}

#[test]
fn form_xobject_text_redaction_copy_on_write() {
    let form = |content: &[u8], extra_res: &str| {
        stream_body(&format!("/Type /XObject /Subtype /Form /BBox [0 0 300 40] /Resources << /Font << /F1 6 0 R >> {extra_res} >>"), content)
    };
    let region = RedactionRegion { page_index: 0, rect: [40.0, 640.0, 400.0, 700.0] };

    // Two uses of one form: only the use under the region is rewritten (copy-on-write).
    let two = form_doc(
        "q 1 0 0 1 50 650 cm /Fm1 Do Q\nq 1 0 0 1 50 300 cm /Fm1 Do Q\nBT /F1 12 Tf 72 100 Td (page text) Tj ET",
        vec![(9, form(b"BT /F1 12 Tf 10 10 Td (SECRET-FORM) Tj ET", ""))],
        "/Fm1 9 0 R",
    );
    let mut doc = open(&two);
    let out = redact_and_save(&mut doc, &[region], UnsupportedPolicy::Refuse, &token()).unwrap();
    assert_eq!(out.reports[0].removed_text, vec![b"SECRET-FORM".to_vec()]);
    let streams = independent_streams(&out.bytes);
    assert_eq!(
        streams.iter().map(|s| count(s, b"SECRET-FORM")).sum::<usize>(),
        1,
        "only the unredacted second use keeps the original form"
    );
    let page = streams.iter().find(|s| find(s, b"(page text)").is_some()).expect("page content");
    assert_eq!((count(page, b"/Fm1 Do"), count(page, b"/HskRd0 Do")), (1, 1));
    assert!(out.proof.hits.is_empty() && out.proof.kept_text_collisions.len() == 1 && out.residual_in_region == 0, "{:?}", out.proof);
    assert!(out.is_proven());

    // One use with a nested form: both strings vanish from every byte, the old forms are dropped.
    let one = form_doc(
        "q 1 0 0 1 50 650 cm /Fm1 Do Q\nBT /F1 12 Tf 72 100 Td (page text) Tj ET",
        vec![
            (9, form(b"BT /F1 12 Tf 10 10 Td (SECRET-FORM) Tj ET\n/Fm3 Do", "/XObject << /Fm3 10 0 R >>")),
            (
                10,
                stream_body(
                    "/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 6 0 R >> >>",
                    b"BT /F1 12 Tf 0 0 Td (SECRET-NESTED) Tj ET",
                ),
            ),
        ],
        "/Fm1 9 0 R",
    );
    let mut doc = open(&one);
    let out = redact_and_save(&mut doc, &[region], UnsupportedPolicy::Refuse, &token()).unwrap();
    let mut removed = out.reports[0].removed_text.clone();
    removed.sort();
    assert_eq!(removed, vec![b"SECRET-FORM".to_vec(), b"SECRET-NESTED".to_vec()]);
    let streams = independent_streams(&out.bytes);
    for n in [&b"SECRET-FORM"[..], b"SECRET-NESTED"] {
        assert!(find(&out.bytes, n).is_none());
        assert!(streams.iter().all(|s| find(s, n).is_none()));
    }
    assert!(streams.iter().any(|s| find(s, b"(page text)").is_some()));
    assert!(out.proof.hits.is_empty() && out.proof.kept_text_collisions.is_empty());
    assert!(out.is_proven());
}

#[test]
fn sanitize_removes_thumb_pieceinfo_metadata_from_all_bytes() {
    let deflate = |d: &[u8]| miniz_oxide::deflate::compress_to_vec_zlib(d, 6);
    let mut objs = base_objs(CONTENT.as_bytes(), "<< /Title (SECRET-INFO) >>");
    objs[0].1 = b"<< /Type /Catalog /Pages 2 0 R /Metadata 20 0 R /PieceInfo << /App << /Data 21 0 R >> >> /Names << /EmbeddedFiles << /Names [(a.txt) 22 0 R] >> /JavaScript << /Names [(js) 23 0 R] >> >> /OpenAction 23 0 R >>".to_vec();
    objs[2].1 = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Thumb 24 0 R /PieceInfo 25 0 R /Resources << /Font << /F1 6 0 R >> >> >>".to_vec();
    objs.push((20, stream_body("/Type /Metadata /Subtype /XML", b"<x:xmpmeta>SECRET-XMP</x:xmpmeta>")));
    objs.push((21, stream_body("/Filter /FlateDecode", &deflate(b"private SECRET-PIECE-DATA"))));
    objs.push((22, b"<< /Type /Filespec /F (a.txt) /EF << /F 26 0 R >> >>".to_vec()));
    objs.push((23, b"<< /S /JavaScript /JS (app.alert\\('SECRET-JS'\\)) >>".to_vec()));
    objs.push((24, stream_body("/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", b"SECRET-THUMB")));
    objs.push((25, b"<< /App << /Private (SECRET-PAGE-PIECE) >> >>".to_vec()));
    objs.push((26, stream_body("/Type /EmbeddedFile /Filter /FlateDecode", &deflate(b"SECRET-EMBEDDED payload"))));
    let needles: [&[u8]; 7] =
        [b"SECRET-INFO", b"SECRET-XMP", b"SECRET-PIECE-DATA", b"SECRET-PAGE-PIECE", b"SECRET-JS", b"SECRET-THUMB", b"SECRET-EMBEDDED"];
    let input = build(&objs, "/Info 7 0 R", b"");

    // Non-vacuous: every payload is in the input (raw, or after an independent inflate).
    let in_streams = independent_streams(&input);
    for n in needles {
        assert!(
            find(&input, n).is_some() || in_streams.iter().any(|s| find(s, n).is_some()),
            "input lacks {}",
            String::from_utf8_lossy(n)
        );
    }

    let doc = open(&input);
    let (out, report) = sanitize_full_rewrite(&doc, &SanitizePolicy::ALL, &token()).unwrap();
    let out_streams = independent_streams(&out);
    for n in needles {
        assert!(find(&out, n).is_none(), "raw bytes still hold {}", String::from_utf8_lossy(n));
        assert!(out_streams.iter().all(|s| find(s, n).is_none()), "a decoded stream still holds {}", String::from_utf8_lossy(n));
    }
    let wide: Vec<Vec<u8>> = needles.iter().map(|n| n.to_vec()).collect();
    let proof = verify_payload_absent(&out, &wide, &Limits::default(), &token()).unwrap();
    assert!(proof.is_clean() && proof.hits.is_empty() && proof.kept_text_collisions.is_empty(), "{proof:?}");
    let classes: Vec<&str> = report.removed.iter().map(|(c, _)| c.as_str()).collect();
    for c in ["info_metadata", "xmp", "embedded_files", "javascript", "thumbnails", "private_data"] {
        assert!(classes.contains(&c), "report lacks {c}: {classes:?}");
    }
    assert_eq!(open(&out).inspect().page_count, 1);
    assert!(out_streams.iter().any(|s| find(s, b"[(Public ) -250 (text)] TJ").is_some()), "public content intact");

    // The policy is selective: keeping XMP keeps exactly that payload.
    let (kept, _) = sanitize_full_rewrite(&doc, &SanitizePolicy { drop_xmp: false, ..SanitizePolicy::ALL }, &token()).unwrap();
    assert!(find(&kept, b"SECRET-XMP").is_some() && find(&kept, b"SECRET-THUMB").is_none());
}

/// PDF LZW (EarlyChange 1) encoder; the decoder's code-width rule is mirrored exactly.
fn lzw_encode(data: &[u8]) -> Vec<u8> {
    struct Bits {
        out: Vec<u8>,
        acc: u32,
        n: u32,
    }
    impl Bits {
        fn put(&mut self, code: u16, width: u32) {
            self.acc = (self.acc << width) | u32::from(code);
            self.n += width;
            while self.n >= 8 {
                self.out.push((self.acc >> (self.n - 8)) as u8);
                self.n -= 8;
                self.acc &= (1 << self.n) - 1;
            }
        }
    }
    // The decoder lags the encoder by one table entry, and adds nothing for the first code.
    let width = |emitted: u32| {
        let d = 258 + emitted.saturating_sub(1);
        match d + 1 {
            ..512 => 9,
            512..1024 => 10,
            1024..2048 => 11,
            _ => 12,
        }
    };
    let mut bits = Bits { out: Vec::new(), acc: 0, n: 0 };
    let mut dict: std::collections::HashMap<(u16, u8), u16> = std::collections::HashMap::new();
    let (mut next, mut emitted) = (258u16, 0u32);
    bits.put(256, 9);
    let mut w: Option<u16> = None;
    for &b in data {
        match w {
            None => w = Some(u16::from(b)),
            Some(c) => {
                if let Some(&code) = dict.get(&(c, b)) {
                    w = Some(code);
                } else {
                    bits.put(c, width(emitted));
                    emitted += 1;
                    dict.insert((c, b), next);
                    next += 1;
                    w = Some(u16::from(b));
                    if next >= 4000 {
                        bits.put(256, width(emitted));
                        emitted = 0;
                        dict.clear();
                        next = 258;
                    }
                }
            }
        }
    }
    if let Some(c) = w {
        bits.put(c, width(emitted));
        emitted += 1;
    }
    bits.put(257, width(emitted));
    if bits.n > 0 {
        bits.out.push((bits.acc << (8 - bits.n)) as u8);
    }
    bits.out
}

#[test]
fn lzw_filter_decodes_and_is_bomb_bounded() {
    let region = RedactionRegion { page_index: 0, rect: [60.0, 690.0, 200.0, 720.0] };
    let mut content = String::new();
    for i in 0..400 {
        content.push_str(&format!("q 1 0 0 1 {} {} cm Q\n", i, i * 3));
    }
    content.push_str("BT /F1 12 Tf 72 700 Td (SECRET-LZW) Tj ET");
    let mut objs = base_objs(b"", "");
    objs[3].1 = stream_body("/Filter /LZWDecode", &lzw_encode(content.as_bytes()));
    let mut doc = open(&build(&objs, "", b""));
    let out = redact_and_save(&mut doc, &[region], UnsupportedPolicy::Refuse, &token()).unwrap();
    assert_eq!(out.reports[0].removed_text, vec![b"SECRET-LZW".to_vec()]);
    let streams = independent_streams(&out.bytes);
    assert!(
        streams.iter().any(|s| find(s, b"1 0 0 1 399 1197 cm").is_some()),
        "whole LZW stream decoded across code-width changes"
    );
    assert!(streams.iter().all(|s| find(s, b"SECRET-LZW").is_none()));

    // Decompression bomb: 4 MiB of zeros in a few KB, 64 KiB cap.
    let bomb = lzw_encode(&vec![0u8; 4 << 20]);
    assert!(bomb.len() < 16 * 1024, "bomb is {} bytes", bomb.len());
    let mut objs = base_objs(b"", "");
    objs[3].1 = stream_body("/Filter /LZWDecode", &bomb);
    let limits = Limits { max_decoded_bytes: 64 * 1024, ..Limits::default() };
    let mut doc = PdfDoc::open(&build(&objs, "", b""), &limits, &token()).unwrap();
    assert_eq!(doc.redact_region(&region, UnsupportedPolicy::Refuse, &token()).unwrap_err(), PressError::InputLimit("decoded_bytes"));
}

#[test]
fn proof_context_rule_kept_text_collision_is_not_a_leak() {
    let region = RedactionRegion { page_index: 0, rect: [60.0, 690.0, 200.0, 720.0] };
    let run = |content: &str, info: &str| {
        let mut doc = open(&build(&base_objs(content.as_bytes(), info), "/Info 7 0 R", b""));
        redact_and_save(&mut doc, &[region], UnsupportedPolicy::Refuse, &token()).unwrap()
    };
    let natural = "BT /F1 12 Tf 72 700 Td (budget plan) Tj ET\nBT /F1 12 Tf 72 400 Td (the budget plan is on page two) Tj ET";

    // The same words occur in kept text: a collision, reported, not a leak; the document is proven.
    let out = run(natural, "<< /Title (T) >>");
    assert_eq!(out.reports[0].removed_text, vec![b"budget plan".to_vec()]);
    assert!(out.proof.hits.is_empty());
    assert_eq!(out.proof.kept_text_collisions.len(), 1);
    assert_eq!(out.proof.kept_text_collisions[0].count, 1);
    assert_eq!(out.residual_in_region, 0);
    assert!(out.is_proven());

    // The same words in metadata are a leak, whatever the kept text says.
    let leak = run(natural, "<< /Title (T) /Subject (the budget plan memo) >>");
    assert_eq!(leak.proof.kept_text_collisions.len(), 1);
    assert!(leak.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::StringObject { object: 7 })));
    assert!(!leak.is_proven() && leak.region_cleared(), "the region is clear, the word survives in metadata");

    // ... and so are non-show content operands (marked-content properties).
    let actual = "BT /F1 12 Tf 72 700 Td (budget plan) Tj ET\n/Span << /ActualText (budget plan) >> BDC\nBT /F1 12 Tf 72 400 Td (page two) Tj ET\nEMC";
    let leak = run(actual, "<< /Title (T) >>");
    assert!(leak.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::ContentString { .. })));
    assert!(leak.proof.kept_text_collisions.is_empty() && !leak.is_proven());
}

// ----------------------------------------------------------------- slice 3
// Outline titles and annotation text. Oracles do not use the crate parser: a test-side string
// extractor (literal + hex strings, escapes, UTF-16BE) over the raw bytes and over every
// independently inflated stream body.

use hsk_studio_press::{TextFieldPolicy, TextMode, TextOutcomeKind, TextRole, redact_and_save_with, redact_text_fields_and_save};

/// UTF-16BE hex string `<FEFF....>`.
fn u16hex(s: &str) -> String {
    let mut h = String::from("<FEFF");
    for u in s.encode_utf16() {
        h.push_str(&format!("{u:04X}"));
    }
    h.push('>');
    h
}

fn decode_pdf_text(raw: &[u8]) -> String {
    let s = if raw.starts_with(&[0xFE, 0xFF]) {
        String::from_utf16_lossy(&raw[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect::<Vec<_>>())
    } else {
        raw.iter().map(|b| char::from(*b)).collect()
    };
    s.to_lowercase()
}

/// Decoded, lowercased text of every `( )` and `< >` string in `bytes`.
fn strings_of(bytes: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => {
                let (mut raw, mut depth) = (Vec::new(), 1);
                i += 1;
                while i < bytes.len() && depth > 0 {
                    match bytes[i] {
                        b'\\' => {
                            i += 1;
                            match bytes.get(i) {
                                Some(b'n') => raw.push(b'\n'),
                                Some(b'r') => raw.push(b'\r'),
                                Some(b't') => raw.push(b'\t'),
                                Some(d @ b'0'..=b'7') => {
                                    let mut v = u32::from(d - b'0');
                                    for _ in 0..2 {
                                        if let Some(n @ b'0'..=b'7') = bytes.get(i + 1) {
                                            v = v * 8 + u32::from(n - b'0');
                                            i += 1;
                                        }
                                    }
                                    raw.push(v as u8);
                                }
                                Some(b'\n' | b'\r') | None => {}
                                Some(o) => raw.push(*o),
                            }
                        }
                        b'(' => {
                            depth += 1;
                            raw.push(b'(');
                        }
                        b')' => {
                            depth -= 1;
                            if depth > 0 {
                                raw.push(b')');
                            }
                        }
                        b => raw.push(b),
                    }
                    i += 1;
                }
                out.push(decode_pdf_text(&raw));
            }
            b'<' if bytes.get(i + 1) == Some(&b'<') => i += 2,
            b'<' => {
                let (mut j, mut hex) = (i + 1, Vec::new());
                while j < bytes.len() && (bytes[j].is_ascii_hexdigit() || bytes[j].is_ascii_whitespace()) {
                    if bytes[j].is_ascii_hexdigit() {
                        hex.push(bytes[j]);
                    }
                    j += 1;
                }
                if bytes.get(j) == Some(&b'>') && !hex.is_empty() {
                    if hex.len() % 2 == 1 {
                        hex.push(b'0');
                    }
                    let raw: Vec<u8> = hex.chunks(2).map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap()).collect();
                    out.push(decode_pdf_text(&raw));
                    i = j + 1;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    out
}

/// Strings of the file and of every inflated stream body.
fn all_strings(bytes: &[u8]) -> Vec<String> {
    let mut v = strings_of(bytes);
    for s in independent_streams(bytes) {
        v.extend(strings_of(&s));
    }
    v
}

/// `needle` (case-insensitive) present in any spelling: raw bytes or decoded strings, in the file
/// or in any inflated stream.
fn leaks(bytes: &[u8], needle: &str) -> bool {
    let n = needle.to_lowercase();
    std::iter::once(bytes.to_vec())
        .chain(independent_streams(bytes))
        .any(|v| {
            let text = String::from_utf8_lossy(&v).to_lowercase();
            // `#20` is how a name object spells a blank.
            text.contains(&n) || text.replace("#20", " ").contains(&n) || strings_of(&v).iter().any(|s| s.contains(&n))
        })
}

fn outline_doc() -> Vec<u8> {
    let objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> >>".to_vec()),
        (4, stream_body("", b"q Q")),
        (10, b"<< /Type /Outlines /First 11 0 R /Last 14 0 R /Count 5 >>".to_vec()),
        (11, b"<< /Title (Budget Plan overview) /Parent 10 0 R /Next 12 0 R /Dest [3 0 R /Fit] >>".to_vec()),
        (
            12,
            format!(
                "<< /Title {} /Parent 10 0 R /Prev 11 0 R /Next 13 0 R /First 15 0 R /Last 15 0 R /Count 1 >>",
                u16hex("Online Retrieval of BUDGET PLAN")
            )
            .into_bytes(),
        ),
        (13, b"<< /Title 20 0 R /Parent 10 0 R /Prev 12 0 R /Next 14 0 R >>".to_vec()),
        (14, b"<< /Title (Glossary) /Parent 10 0 R /Prev 13 0 R >>".to_vec()),
        (15, format!("<< /Title {} /Parent 12 0 R >>", u16hex("Plan Caf\u{e9} Budget Plan \u{3a9}mega")).into_bytes()),
        (20, b"(Appendix: budget plan)".to_vec()),
    ];
    build(&objs, "", b"")
}

type Seen = (u32, TextRole, TextOutcomeKind, u32);

#[test]
fn outline_titles_redacted_in_every_encoding() {
    let input = outline_doc();
    let needle = ["Budget Plan".to_string()];
    assert!(leaks(&input, "budget plan"));
    assert!(strings_of(&input).iter().any(|s| s == "online retrieval of budget plan"), "the oracle decodes UTF-16BE hex strings");
    let before = verify_payload_absent(&input, &[b"Budget Plan".to_vec()], &Limits::default(), &token()).unwrap();
    assert!(!before.hits.is_empty(), "non-vacuous: the input holds the text in outline titles");

    let mut doc = open(&input);
    let out = redact_text_fields_and_save(&mut doc, &needle, &TextFieldPolicy::default(), &token()).unwrap();
    let seen: Vec<Seen> = out.report.outcomes.iter().map(|o| (o.object.num, o.role, o.kind, o.matches)).collect();
    let item = |n| (n, TextRole::OutlineTitle, TextOutcomeKind::Replaced, 1);
    assert_eq!(seen, vec![item(11), item(12), item(13), item(15)]);
    assert!(out.report.complete && out.report.kept_by_policy == 0 && out.report.matches_redacted == 4);
    assert_eq!((out.report.outline_items_scanned, out.report.annotations_scanned, out.report.fields_scanned), (6, 0, 0));
    assert!(out.is_proven(), "{:?}", out.proof);

    // Independent oracle: no spelling of the text survives anywhere; replaced text is as expected.
    assert!(!leaks(&out.bytes, "budget plan"));
    let strings = all_strings(&out.bytes);
    for expect in ["[redacted] overview", "online retrieval of [redacted]", "appendix: [redacted]", "plan caf\u{e9} [redacted] \u{3c9}mega", "glossary"] {
        assert!(strings.iter().any(|s| s == expect), "missing {expect:?} in {strings:?}");
    }
    // Structure intact, untouched item verbatim, UTF-16BE stays UTF-16BE, shared string dropped.
    assert!(find(&out.bytes, b"/Next 12 0 R").is_some() && find(&out.bytes, b"/First 15 0 R").is_some());
    assert!(find(&out.bytes, &obj_bytes(14, b"<< /Title (Glossary) /Parent 10 0 R /Prev 13 0 R >>")).is_some());
    assert!(find(&out.bytes, b"\\376\\377\\000O\\000n\\000l\\000i\\000n\\000e").is_some());
    assert_eq!(count(&out.bytes, b"Appendix"), 1, "object 20 is unreachable once /Title holds a direct string");
    assert_eq!(open(&out.bytes).inspect().page_count, 1);

    // Remove mode deletes the spans.
    let mut doc = open(&input);
    let removing = TextFieldPolicy { mode: TextMode::Remove, ..TextFieldPolicy::default() };
    let out = redact_text_fields_and_save(&mut doc, &needle, &removing, &token()).unwrap();
    assert!(out.report.outcomes.iter().all(|o| o.kind == TextOutcomeKind::Removed) && out.is_proven());
    let strings = all_strings(&out.bytes);
    assert!(strings.iter().any(|s| s == " overview") && strings.iter().any(|s| s == "appendix: "));
    assert!(!leaks(&out.bytes, "budget plan") && !strings.iter().any(|s| s.contains("redacted")));

    // Proof-aligned options: exact case + whole token leaves other-case titles (the proof is
    // case-sensitive, so it agrees), and rewrites only the exact-case ones.
    let mut doc = open(&input);
    let exact = TextFieldPolicy { case_insensitive: false, whole_token: true, ..TextFieldPolicy::default() };
    let out = redact_text_fields_and_save(&mut doc, &needle, &exact, &token()).unwrap();
    assert_eq!(out.report.outcomes.iter().map(|o| o.object.num).collect::<Vec<_>>(), vec![11, 15]);
    assert!(out.is_proven() && leaks(&out.bytes, "budget plan"), "BUDGET PLAN / budget plan titles are other spellings");
}

#[test]
fn annotation_text_slots_and_appearance_redacted() {
    let deflate = |d: &[u8]| miniz_oxide::deflate::compress_to_vec_zlib(d, 6);
    let rc = "<?xml version=\"1.0\"?><body xmlns=\"http://www.w3.org/1999/xhtml\"><p>Meeting about the Bud<b>get</b> <i>Plan</i></p></body>";
    let clean = b"<< /Type /Annot /Subtype /Text /Rect [10 100 30 120] /Contents (Nothing sensitive) /T (Alice) /AP << /N 14 0 R >> >>".to_vec();
    // The appearances name /Helv: since R9 (positive determination) a font name that no resource
    // dictionary holds is unresolved (opaque), so the document lists it in /AcroForm /DR (fixture only).
    let objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [] /DR << /Font << /Helv 15 0 R >> >> >> >>".to_vec()),
        (15, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> /Annots [5 0 R 6 0 R 7 0 R << /Subtype /Text /Rect [1 1 2 2] /Contents (direct Budget Plan note) >>] >>"
                .to_vec(),
        ),
        (4, stream_body("", b"q Q")),
        (
            5,
            format!(
                "<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (Meeting about the Budget Plan) /T {} /Subj (re: budget plan) /RC 9 0 R /AP << /N 12 0 R >> /P 3 0 R >>",
                u16hex("Budget Plan Owner")
            )
            .into_bytes(),
        ),
        (6, b"<< /Type /Annot /Subtype /FreeText /Rect [100 300 300 340] /Contents 20 0 R /DA (0 g /Helv 12 Tf) /AP << /N 13 0 R >> >>".to_vec()),
        (7, clean.clone()),
        (9, stream_body("", rc.as_bytes())),
        (12, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 20 20]", b"0 0 1 rg 0 0 20 20 re f")),
        (
            13,
            stream_body(
                "/Type /XObject /Subtype /Form /BBox [0 0 200 40] /Filter /FlateDecode",
                &deflate(b"BT /Helv 12 Tf 2 10 Td (Budget Plan: draft) Tj ET"),
            ),
        ),
        (14, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 200 40]", b"BT /Helv 12 Tf 2 10 Td (Nothing sensitive) Tj ET")),
        (20, b"(Budget Plan: draft)".to_vec()),
    ];
    let input = build(&objs, "", b"");
    let needle = ["Budget Plan".to_string()];

    // The proof treats annotation appearance text as annotation text: a hit, not a kept-text collision.
    let before = verify_payload_absent(&input, &[b"Budget Plan".to_vec()], &Limits::default(), &token()).unwrap();
    assert!(before.hits.iter().any(|h| matches!(h.location, PayloadLocation::ContentString { object: 13 })));
    assert!(before.kept_text_collisions.is_empty());

    let mut doc = open(&input);
    let out = redact_text_fields_and_save(&mut doc, &needle, &TextFieldPolicy::default(), &token()).unwrap();
    use TextOutcomeKind::{Dropped, Replaced};
    use TextRole::{AnnotContents, AnnotRichText, AnnotSubject, AnnotTitle, Appearance};
    let seen: Vec<(u32, bool, TextRole, TextOutcomeKind, u32)> =
        out.report.outcomes.iter().map(|o| (o.object.num, o.direct, o.role, o.kind, o.matches)).collect();
    assert_eq!(
        seen,
        vec![
            (5, false, AnnotContents, Replaced, 1),
            (5, false, AnnotSubject, Replaced, 1),
            (5, false, AnnotRichText, Dropped, 1),
            (5, false, AnnotTitle, Replaced, 1),
            (6, false, AnnotContents, Replaced, 1),
            (6, false, Appearance, Dropped, 1),
            (3, true, AnnotContents, Replaced, 1),
        ]
    );
    assert_eq!((out.report.annotations_scanned, out.report.matches_redacted), (4, 7));
    assert!(out.is_proven() && out.proof.kept_text_collisions.is_empty(), "{:?}", out.proof);

    // Independent oracle over raw bytes, strings and inflated streams.
    assert!(!leaks(&out.bytes, "budget plan"));
    assert!(find(&out.bytes, b"<?xml").is_none(), "the dropped rich text stream is unreachable");
    let strings = all_strings(&out.bytes);
    for expect in ["meeting about the [redacted]", "[redacted] owner", "re: [redacted]", "[redacted]: draft", "direct [redacted] note", "nothing sensitive", "alice"] {
        assert!(strings.iter().any(|s| s == expect), "missing {expect:?} in {strings:?}");
    }
    // The clean annotation and its appearance are byte-identical; the icon appearance stays; the
    // FreeText appearance is gone.
    assert!(find(&out.bytes, &obj_bytes(7, &clean)).is_some());
    assert_eq!(independent_streams(&out.bytes).iter().filter(|s| find(s, b"(Nothing sensitive) Tj").is_some()).count(), 1);
    assert!(find(&out.bytes, b"/AP << /N 12 0 R >>").is_some() && find(&out.bytes, b"/N 13 0 R").is_none());
    assert_eq!(open(&out.bytes).inspect().page_count, 1);
}

#[test]
fn widget_values_are_an_explicit_option_and_kept_text_is_reported() {
    let objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /Fields [30 0 R 31 0 R] >> >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> /Annots [30 0 R 31 0 R] >>".to_vec()),
        (4, stream_body("", b"q Q")),
        (
            30,
            b"<< /Type /Annot /Subtype /Widget /FT /Tx /T (Notes Budget Plan) /V (Budget Plan Q3) /DV (Budget Plan default) /Rect [10 10 200 30] /AP << /N 32 0 R >> /P 3 0 R >>"
                .to_vec(),
        ),
        (31, b"<< /Type /Annot /Subtype /Widget /FT /Ch /T (choice) /V [(Budget Plan A) (other)] /Rect [10 40 200 60] /P 3 0 R >>".to_vec()),
        (32, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 190 20]", b"BT /Helv 12 Tf 2 5 Td (Budget Plan Q3) Tj ET")),
    ];
    let input = build(&objs, "", b"");
    let needle = ["Budget Plan".to_string()];
    use TextOutcomeKind::{Dropped, KeptByPolicy, Replaced};
    use TextRole::{Appearance, FieldDefaultValue, FieldName, FieldValue};
    let tuples = |r: &hsk_studio_press::TextFieldReport| -> Vec<Seen> { r.outcomes.iter().map(|o| (o.object.num, o.role, o.kind, o.matches)).collect() };

    // Default: nothing in widgets is rewritten, everything that matches is reported as kept, and
    // the independent proof agrees (hits in the field strings and the widget appearance).
    let mut doc = open(&input);
    let out = redact_text_fields_and_save(&mut doc, &needle, &TextFieldPolicy::default(), &token()).unwrap();
    assert_eq!(
        tuples(&out.report),
        vec![
            (30, FieldName, KeptByPolicy, 1),
            (30, FieldValue, KeptByPolicy, 1),
            (30, FieldDefaultValue, KeptByPolicy, 1),
            (30, Appearance, KeptByPolicy, 1),
            (31, FieldValue, KeptByPolicy, 1),
        ]
    );
    assert!(!out.report.complete && out.report.kept_by_policy == 5 && out.report.matches_redacted == 0 && !out.is_proven());
    assert!(out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::StringObject { object: 30 })));
    assert!(out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::StringObject { object: 31 })));
    assert!(out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::ContentString { object: 32 })));
    assert!(leaks(&out.bytes, "budget plan q3"));

    // Explicit option: values, defaults and the widget appearance are redacted; the field NAME is
    // never rewritten, stays reported, and stays a proof hit.
    let mut doc = open(&input);
    let on = TextFieldPolicy { widget_values: true, ..TextFieldPolicy::default() };
    let out = redact_text_fields_and_save(&mut doc, &needle, &on, &token()).unwrap();
    assert_eq!(
        tuples(&out.report),
        vec![
            (30, FieldName, KeptByPolicy, 1),
            (30, FieldValue, Replaced, 1),
            (30, FieldDefaultValue, Replaced, 1),
            (30, Appearance, Dropped, 1),
            (31, FieldValue, Replaced, 1),
        ]
    );
    assert!(!out.report.complete && out.report.kept_by_policy == 1 && out.report.matches_redacted == 4 && !out.is_proven());
    assert!(out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::StringObject { object: 30 })));
    assert!(out.proof.hits.iter().all(|h| matches!(h.location, PayloadLocation::StringObject { object: 30 } | PayloadLocation::RawBytes { .. })));
    let strings = all_strings(&out.bytes);
    assert_eq!(strings.iter().filter(|s| s.contains("budget plan")).collect::<Vec<_>>(), vec!["notes budget plan"]);
    for expect in ["[redacted] q3", "[redacted] default", "[redacted] a", "other"] {
        assert!(strings.iter().any(|s| s == expect), "missing {expect:?} in {strings:?}");
    }
    assert!(find(&out.bytes, b"/N 32 0 R").is_none());
    assert_eq!(open(&out.bytes).proposal(&token()).unwrap().form_fields.len(), 2);
}

#[test]
fn region_redaction_with_text_fields_closes_outline_and_annotation_leaks() {
    // The appearance names /Helv, listed in /AcroForm /DR (since R9 an unresolved font name is opaque).
    let objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R /AcroForm << /Fields [] /DR << /Font << /Helv 6 0 R >> >> >> >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (
            3,
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R >> >> /Annots [5 0 R] >>".to_vec(),
        ),
        (
            4,
            stream_body("", b"BT /F1 12 Tf 72 700 Td (Budget Plan) Tj ET\nBT /F1 12 Tf 72 400 Td (the Budget Plan is on page two) Tj ET"),
        ),
        (5, b"<< /Type /Annot /Subtype /FreeText /Rect [100 300 300 340] /Contents (Budget Plan note) /AP << /N 13 0 R >> >>".to_vec()),
        (6, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()),
        (10, b"<< /Type /Outlines /First 11 0 R /Last 11 0 R /Count 1 >>".to_vec()),
        (11, format!("<< /Title {} /Parent 10 0 R >>", u16hex("Online Retrieval Budget Plan")).into_bytes()),
        (13, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 200 40]", b"BT /Helv 12 Tf 2 10 Td (Budget Plan note) Tj ET")),
    ];
    let pdf = build(&objs, "", b"");
    let region = RedactionRegion { page_index: 0, rect: [60.0, 690.0, 200.0, 720.0] };
    let run = |text: Option<&TextFieldPolicy>| {
        let mut doc = open(&pdf);
        redact_and_save_with(&mut doc, &[region], UnsupportedPolicy::Refuse, text, &token()).unwrap()
    };

    // Slice 2 behaviour: the region is clear, but the words survive in the bookmark and in the
    // annotation (its appearance is annotation text, so a hit, not a collision).
    let plain = run(None);
    assert!(plain.region_cleared() && !plain.is_proven() && plain.text_fields.is_none());
    assert!(plain.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::StringObject { object: 11 })));
    assert!(plain.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::ContentString { object: 13 })));
    assert_eq!(plain.proof.kept_text_collisions.len(), 1, "only the kept page text collides");

    // Slice 3: the same call with a text policy redacts both; the kept page text is still a collision.
    let fixed = run(Some(&TextFieldPolicy::default()));
    assert!(fixed.region_cleared() && fixed.is_proven(), "{:?}", fixed.proof);
    assert_eq!(fixed.proof.kept_text_collisions.len(), 1);
    let tf = fixed.text_fields.as_ref().expect("text fields requested");
    let seen: Vec<Seen> = tf.outcomes.iter().map(|o| (o.object.num, o.role, o.kind, o.matches)).collect();
    assert_eq!(
        seen,
        vec![
            (5, TextRole::AnnotContents, TextOutcomeKind::Replaced, 1),
            (5, TextRole::Appearance, TextOutcomeKind::Dropped, 1),
            (11, TextRole::OutlineTitle, TextOutcomeKind::Replaced, 1),
        ]
    );
    let strings = all_strings(&fixed.bytes);
    assert_eq!(strings.iter().filter(|s| s.contains("budget plan")).collect::<Vec<_>>(), vec!["the budget plan is on page two"]);
    assert!(strings.iter().any(|s| s == "online retrieval [redacted]") && strings.iter().any(|s| s == "[redacted] note"));
}

#[test]
fn text_fields_refusals_bounds_and_atomicity() {
    let needle = vec!["Budget Plan".to_string()];
    let policy = TextFieldPolicy::default();
    let input = outline_doc();
    let mut doc = open(&input);
    let before = doc.save_full(&token()).unwrap();

    // Nothing to do: empty report, complete, bytes unchanged.
    let none = doc.redact_text_fields(&["no such words".to_string()], &policy, &token()).unwrap();
    assert!(none.outcomes.is_empty() && none.complete && none.matches_redacted == 0);
    assert_eq!(doc.save_full(&token()).unwrap(), before);

    // Typed refusals; the document is untouched after each.
    let bad_marker = TextFieldPolicy { mode: TextMode::Replace("xbudget planx".into()), ..policy.clone() };
    assert_eq!(doc.redact_text_fields(&needle, &bad_marker, &token()), Err(PressError::Malformed("text_marker_matches_needle")));
    let many: Vec<String> = (0..4097).map(|i| format!("n{i}")).collect();
    assert_eq!(doc.redact_text_fields(&many, &policy, &token()), Err(PressError::InputLimit("needles")));
    let cancelled = token();
    cancelled.cancel();
    assert_eq!(doc.redact_text_fields(&needle, &policy, &cancelled), Err(PressError::Canceled));
    assert_eq!(doc.save_full(&token()).unwrap(), before);
    let mut signed_objs = base_objs(CONTENT.as_bytes(), "<< /Title (T) >>");
    signed_objs[0].1 = b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /SigFlags 3 /Fields [9 0 R] >> >>".to_vec();
    signed_objs.push((9, b"<< /FT /Sig /T (Sig1) /V << /Type /Sig /ByteRange [0 10 20 30] /Contents <00112233> >> >>".to_vec()));
    let mut signed = open(&build(&signed_objs, "/Info 7 0 R", b""));
    assert_eq!(signed.redact_text_fields(&needle, &policy, &token()), Err(PressError::SignedDocument));

    // Atomic: annotation 5 would be rewritten, annotation 6 hits the decoded-size bound -> error,
    // and nothing of 5's rewrite is visible.
    let bomb = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 8 << 20], 6);
    let objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> /Annots [5 0 R 6 0 R] >>".to_vec()),
        (4, stream_body("", b"q Q")),
        (5, b"<< /Type /Annot /Subtype /Text /Rect [1 1 2 2] /Contents (Budget Plan x) >>".to_vec()),
        (6, b"<< /Type /Annot /Subtype /FreeText /Rect [5 5 9 9] /Contents (fine) /AP << /N 7 0 R >> >>".to_vec()),
        (7, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 1 1] /Filter /FlateDecode", &bomb)),
    ];
    let limits = Limits { max_decoded_bytes: 1 << 20, ..Limits::default() };
    let mut doc = PdfDoc::open(&build(&objs, "", b""), &limits, &token()).unwrap();
    let saved = doc.save_full(&token()).unwrap();
    assert_eq!(doc.redact_text_fields(&needle, &policy, &token()), Err(PressError::InputLimit("decoded_bytes")));
    assert_eq!(doc.save_full(&token()).unwrap(), saved);

    // Unreadable rich text (filter not implemented) is dropped fail-closed and reported as such.
    let objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> /Annots [5 0 R] >>".to_vec()),
        (4, stream_body("", b"q Q")),
        (5, b"<< /Type /Annot /Subtype /Text /Rect [1 1 2 2] /Contents (fine) /RC 9 0 R >>".to_vec()),
        (9, stream_body("/Filter /DCTDecode", b"junk")),
    ];
    let mut doc = open(&build(&objs, "", b""));
    let r = doc.redact_text_fields(&needle, &policy, &token()).unwrap();
    assert_eq!(r.outcomes.len(), 1);
    let o = r.outcomes[0];
    assert_eq!((o.object.num, o.role, o.kind, o.matches, o.unreadable), (5, TextRole::AnnotRichText, TextOutcomeKind::Dropped, 0, true));
    assert!(find(&doc.save_full(&token()).unwrap(), b"/RC").is_none());

    // A cyclic outline (Next loop) terminates and every linked item is judged once.
    let objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> >>".to_vec()),
        (4, stream_body("", b"q Q")),
        (10, b"<< /Type /Outlines /First 11 0 R /Last 12 0 R >>".to_vec()),
        (11, b"<< /Title (a Budget Plan) /Parent 10 0 R /Next 12 0 R >>".to_vec()),
        (12, b"<< /Title (b budget plan) /Parent 10 0 R /Prev 11 0 R /Next 11 0 R >>".to_vec()),
    ];
    let mut doc = open(&build(&objs, "", b""));
    let r = doc.redact_text_fields(&needle, &policy, &token()).unwrap();
    assert_eq!(r.outcomes.iter().map(|o| o.object.num).collect::<Vec<_>>(), vec![11, 12]);
}

// ------------------------------------------------------------ remediation R5

/// A removed string stored in a stream the verifier cannot decode (unimplemented filter) is
/// invisible to the byte scan; `is_proven` and `region_cleared` must not claim it is absent.
#[test]
fn undecodable_stream_never_reads_as_proven() {
    let deflate = |d: &[u8]| miniz_oxide::deflate::compress_to_vec_zlib(d, 6);
    let region = RedactionRegion { page_index: 0, rect: [60.0, 690.0, 200.0, 720.0] };
    let run = |hidden: bool| {
        let mut objs = base_objs(CONTENT.as_bytes(), "<< /Title (T) >>");
        if hidden {
            // Reachable from the catalog; zlib bytes under a filter this crate does not implement.
            objs[0].1 = b"<< /Type /Catalog /Pages 2 0 R /Hidden 20 0 R >>".to_vec();
            objs.push((20, stream_body("/Filter /JBIG2Decode", &deflate(b"stash SECRET-ALPHA stash"))));
        }
        let mut doc = open(&build(&objs, "/Info 7 0 R", b""));
        redact_and_save(&mut doc, &[region], UnsupportedPolicy::Refuse, &token()).unwrap()
    };

    // Control: the same document without the stream is proven (the only blocker below is the stream).
    let clean = run(false);
    assert!(clean.proof.fully_decoded() && clean.proof.is_clean() && clean.region_cleared() && clean.is_proven());

    let out = run(true);
    assert_eq!(out.reports[0].removed_text, vec![b"SECRET-ALPHA".to_vec()]);
    assert_eq!(out.residual_in_region, 0);
    // The byte scan finds nothing, because the needle is only present in encoded form ...
    assert!(out.proof.hits.is_empty());
    // ... but an independent inflate of the saved file shows it is still there.
    assert!(independent_streams(&out.bytes).iter().any(|s| find(s, b"SECRET-ALPHA").is_some()), "the needle survives in the saved bytes");
    // So nothing may read as proven, and the blocker is named.
    assert_eq!(out.proof.streams_not_decodable, vec![(20u32, "JBIG2Decode".to_string())]);
    assert!(!out.proof.fully_decoded() && !out.proof.is_clean());
    assert!(!out.region_cleared() && !out.is_proven());
    let again = verify_payload_absent(&out.bytes, &[b"SECRET-ALPHA".to_vec()], &Limits::default(), &token()).unwrap();
    assert!(again.hits.is_empty() && !again.is_clean());
}

// ----------------------------------------------------------------- slice 4
// Destinations, URI actions, form labels, structure text, document information, popup/IRT
// annotations and composite-font appearances.

type Row = (u32, bool, TextOutcomeKind, u32);

fn rows(r: &hsk_studio_press::TextFieldReport, role: TextRole) -> Vec<Row> {
    let mut v: Vec<Row> = r.outcomes.iter().filter(|o| o.role == role).map(|o| (o.object.num, o.direct, o.kind, o.matches)).collect();
    v.sort_by_key(|r| (r.0, r.1));
    v
}

fn minimal_objs(catalog_extra: &str, annots: &str) -> Vec<(u32, Vec<u8>)> {
    vec![
        (1, format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra} >>").into_bytes()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> /Annots [{annots}] >>").into_bytes()),
        (4, stream_body("", b"q Q")),
    ]
}

#[test]
fn destinations_and_action_uris_redacted_with_consistent_references() {
    let mut objs = minimal_objs(
        "/Outlines 10 0 R /Names << /Dests 30 0 R >> /Dests 33 0 R /OpenAction << /S /URI /URI (http://y.example/budget plan) >> /URI << /Base (http://base.example/Budget Plan/) >>",
        "5 0 R 6 0 R 7 0 R 8 0 R",
    );
    let more: Vec<(u32, &str)> = vec![
        (5, "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /GoTo /D (Budget Plan A) >> >>"),
        (
            6,
            "<< /Type /Annot /Subtype /Link /Rect [0 20 10 30] /A << /S /URI /URI (http://corp.example/?q=Budget Plan&x=1) >> /AA << /E << /S /URI /URI (http://a.example/budget plan) >> >> >>",
        ),
        (7, "<< /Type /Annot /Subtype /Link /Rect [0 40 10 50] /Dest /Budget#20Plan#20Legacy >>"),
        (8, "<< /Type /Annot /Subtype /Link /Rect [0 60 10 70] /Dest (Budget Plan Missing) >>"),
        (10, "<< /Type /Outlines /First 11 0 R /Last 12 0 R /Count 2 >>"),
        (11, "<< /Title (Section one) /Parent 10 0 R /Next 12 0 R /Dest (Budget Plan A) >>"),
        (12, "<< /Title (Section two) /Parent 10 0 R /Prev 11 0 R /A << /S /URI /URI (http://x.example/Budget Plan) >> >>"),
        (30, "<< /Kids [31 0 R 32 0 R] >>"),
        (31, "<< /Limits [(Appendix) (Budget Plan A)] /Names [(Appendix) [3 0 R /Fit] (Budget Plan A) [3 0 R /XYZ 0 0 0]] >>"),
        (32, "<< /Limits [(Glossary) (zeta)] /Names [(Glossary) [3 0 R /Fit] (zeta) [3 0 R /Fit]] >>"),
        (33, "<< /Budget#20Plan#20Legacy [3 0 R /Fit] /Other [3 0 R /Fit] >>"),
    ];
    objs.extend(more.into_iter().map(|(n, b)| (n, b.as_bytes().to_vec())));
    let input = build(&objs, "", b"");
    let needle = ["Budget Plan".to_string()];
    assert!(leaks(&input, "budget plan"));

    let mut doc = open(&input);
    let out = redact_text_fields_and_save(&mut doc, &needle, &TextFieldPolicy::default(), &token()).unwrap();
    use TextOutcomeKind::Replaced;
    // Tree and legacy dictionary keys (identifiers) and every reference, with their holders.
    assert_eq!(
        rows(&out.report, TextRole::DestName),
        vec![(5, true, Replaced, 1), (7, false, Replaced, 1), (8, false, Replaced, 1), (11, false, Replaced, 1), (30, false, Replaced, 1), (33, false, Replaced, 1)]
    );
    assert_eq!(
        rows(&out.report, TextRole::ActionUri),
        vec![(1, false, Replaced, 1), (1, true, Replaced, 1), (6, true, Replaced, 1), (6, true, Replaced, 1), (12, true, Replaced, 1)]
    );
    assert!(out.report.complete && out.is_proven(), "{:?}", out.proof);

    // Independent oracle: no spelling survives (names with #20 included); URIs keep their shape.
    assert!(!leaks(&out.bytes, "budget plan"));
    let strings = all_strings(&out.bytes);
    for expect in ["http://corp.example/?q=[redacted]&x=1", "http://a.example/[redacted]", "http://x.example/[redacted]", "http://y.example/[redacted]", "http://base.example/[redacted]/"] {
        assert!(strings.iter().any(|s| s == expect), "missing {expect:?} in {strings:?}");
    }
    // One opaque name per old name, used by the tree/dictionary key and by every reference.
    let hsk = |n: &str| strings.iter().filter(|s| s.as_str() == n).count();
    assert_eq!((hsk("hskrd0"), hsk("hskrd2")), (3, 1), "tree key + GoTo /D + outline /Dest; dangling reference");
    assert_eq!(count(&out.bytes, b"/HskRd1"), 2, "legacy dictionary key + /Dest name reference");
    // The name tree is rebuilt as one sorted leaf with the original values; kids are gone.
    let root = {
        let at = find(&out.bytes, b"30 0 obj").expect("root object");
        let end = find(&out.bytes[at..], b"endobj").unwrap();
        out.bytes[at..at + end].to_vec()
    };
    assert!(find(&root, b"/Kids").is_none() && find(&out.bytes, b"/Limits").is_none());
    assert_eq!(strings_of(&root), vec!["appendix", "glossary", "hskrd0", "zeta"]);
    assert!(find(&root, b"[3 0 R /XYZ 0 0 0]").is_some());
    assert!(find(&out.bytes, b"/Other [3 0 R /Fit]").is_some());
    assert_eq!(open(&out.bytes).inspect().page_count, 1);

    // Disabled classes are reported kept, never silently, and the proof agrees.
    let mut doc = open(&input);
    let off = TextFieldPolicy { destinations: false, action_uris: false, ..TextFieldPolicy::default() };
    let kept = redact_text_fields_and_save(&mut doc, &needle, &off, &token()).unwrap();
    assert_eq!((kept.report.kept_by_policy, kept.report.matches_redacted, kept.report.complete), (11, 0, false));
    assert!(rows(&kept.report, TextRole::DestName).iter().all(|r| r.2 == TextOutcomeKind::KeptByPolicy));
    assert!(!kept.is_proven() && !kept.proof.hits.is_empty() && leaks(&kept.bytes, "budget plan"));
}

#[test]
fn form_labels_structure_text_info_and_xmp_redacted() {
    let xmp = b"<?xpacket begin='x'?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF><rdf:Description><dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">Budget Plan 2026</rdf:li></rdf:Alt></dc:title></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end='w'?>";
    let mut objs = minimal_objs("/AcroForm << /Fields [30 0 R] >> /StructTreeRoot 50 0 R /Metadata 40 0 R", "30 0 R");
    objs.extend([
        (7, b"<< /Title (Budget Plan 2026) /Author (A. Planner) /Keywords (budget plan, finance) /Custom (x Budget Plan y) >>".to_vec()),
        (
            30,
            b"<< /Type /Annot /Subtype /Widget /FT /Ch /T (choice) /TU (Pick the Budget Plan) /Opt [(Budget Plan A) [(exp1) (Budget Plan display)] (zzz)] /V (zzz) /Rect [10 40 200 60] /P 3 0 R >>"
                .to_vec(),
        ),
        (40, stream_body("/Type /Metadata /Subtype /XML", xmp)),
        (50, b"<< /Type /StructTreeRoot /K 51 0 R >>".to_vec()),
        (51, b"<< /Type /StructElem /S /Document /P 50 0 R /K [52 0 R] /T (Budget Plan T) >>".to_vec()),
        (52, b"<< /Type /StructElem /S /Figure /P 51 0 R /Alt (Chart of the Budget Plan) /ActualText (Budget Plan) /E (Budget Plan Expanded) >>".to_vec()),
    ]);
    let input = build(&objs, "/Info 7 0 R", b"");
    let needle = ["Budget Plan".to_string()];
    use TextOutcomeKind::{Dropped, Replaced};

    let mut doc = open(&input);
    let out = redact_text_fields_and_save(&mut doc, &needle, &TextFieldPolicy::default(), &token()).unwrap();
    assert_eq!(rows(&out.report, TextRole::InfoField), vec![(7, false, Replaced, 1); 3]);
    assert_eq!(rows(&out.report, TextRole::Xmp), vec![(1, false, Dropped, 1)]);
    assert_eq!(rows(&out.report, TextRole::FieldTooltip), vec![(30, false, Replaced, 1)]);
    assert_eq!(rows(&out.report, TextRole::FieldOption), vec![(30, false, Replaced, 2)]);
    assert_eq!(rows(&out.report, TextRole::StructTitle), vec![(51, false, Replaced, 1)]);
    assert_eq!(rows(&out.report, TextRole::StructAlt), vec![(52, false, Replaced, 1)]);
    assert_eq!(rows(&out.report, TextRole::StructActualText), vec![(52, false, Replaced, 1)]);
    assert_eq!(rows(&out.report, TextRole::StructExpansion), vec![(52, false, Replaced, 1)]);
    assert!(out.report.complete && out.is_proven(), "{:?}", out.proof);

    assert!(!leaks(&out.bytes, "budget plan") && find(&out.bytes, b"xmpmeta").is_none());
    let strings = all_strings(&out.bytes);
    for expect in [
        "[redacted] 2026",
        "a. planner",
        "[redacted], finance",
        "x [redacted] y",
        "pick the [redacted]",
        "[redacted] a",
        "exp1",
        "[redacted] display",
        "zzz",
        "chart of the [redacted]",
        "[redacted] expanded",
        "[redacted] t",
    ] {
        assert!(strings.iter().any(|s| s == expect), "missing {expect:?} in {strings:?}");
    }
    assert!(find(&out.bytes, b"/Opt [([REDACTED] A) [(exp1) ([REDACTED] display)] (zzz)]").is_some(), "option pairs keep their shape");
    assert!(find(&out.bytes, b"/Metadata").is_none());

    // The three classes switched off are all reported kept and remain proof hits.
    let mut doc = open(&input);
    let off = TextFieldPolicy { form_labels: false, structure_text: false, info_fields: false, ..TextFieldPolicy::default() };
    let kept = redact_text_fields_and_save(&mut doc, &needle, &off, &token()).unwrap();
    assert_eq!((kept.report.kept_by_policy, kept.report.matches_redacted, kept.report.complete), (10, 0, false));
    assert!(!kept.is_proven() && leaks(&kept.bytes, "budget plan") && find(&kept.bytes, b"xmpmeta").is_some());
    assert!(kept.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::StringObject { object: 7 })));
}

fn to_unicode_cmap() -> Vec<u8> {
    b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /Test-UCS def /CMapType 2 def\n\
1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
2 beginbfchar <0001> <0042> <0002> <0075> endbfchar\n\
2 beginbfrange <0003> <0003> <0064> <0004> <0006> [<0067> <0065> <0074>] endbfrange\n\
4 beginbfchar <0007> <0020> <0008> <0050> <0009> <006C> <000B> <006E> endbfchar\n\
1 beginbfrange <000A> <000A> <0061> endbfrange\n\
endcmap CMapName currentdict /CMap defineresource pop end end"
        .to_vec()
}

#[test]
fn popup_irt_annotations_and_composite_font_appearances() {
    let codes: String = (1..=11).map(|c| format!("{c:04X}")).collect();
    let ap = |font: &str| stream_body(&format!("/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 {font} 0 R >> >>"), format!("BT /F1 12 Tf 2 5 Td <{codes}> Tj ET").as_bytes());
    let font = |extra: &str| format!("<< /Type /Font /Subtype /Type0 /BaseFont /Test /Encoding /Identity-H /DescendantFonts [21 0 R] {extra} >>").into_bytes();
    let mut objs = minimal_objs("", "5 0 R 9 0 R 10 0 R 11 0 R 16 0 R");
    objs.extend([
        (5, b"<< /Type /Annot /Subtype /Text /Rect [10 10 30 30] /Contents (plain note) /Popup 6 0 R /IRT 8 0 R /P 3 0 R >>".to_vec()),
        (6, b"<< /Type /Annot /Subtype /Popup /Rect [40 40 100 100] /Parent 5 0 R /Contents (popup says Budget Plan) /Open false >>".to_vec()),
        (8, b"<< /Type /Annot /Subtype /Text /Rect [1 1 5 5] /Contents (replied-to: Budget Plan) /T (Owner) >>".to_vec()),
        (9, b"<< /Type /Annot /Subtype /FreeText /Rect [100 100 200 140] /Contents (label only) /AP << /N 13 0 R >> >>".to_vec()),
        (10, b"<< /Type /Annot /Subtype /FreeText /Rect [100 150 200 190] /Contents (Budget Plan note) /AP << /N 14 0 R >> >>".to_vec()),
        (11, b"<< /Type /Annot /Subtype /FreeText /Rect [100 200 200 240] /Contents (fine) /AP << /N 15 0 R >> >>".to_vec()),
        (16, b"<< /Type /Annot /Subtype /FreeText /Rect [100 250 200 290] /Contents (hostile map) /AP << /N 17 0 R >> >>".to_vec()),
        (13, ap("20")),
        (14, ap("23")),
        (15, ap("23")),
        (17, ap("24")),
        (20, font("/ToUnicode 22 0 R")),
        (21, b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Test /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>".to_vec()),
        (22, stream_body("", &to_unicode_cmap())),
        (23, font("")),
        // A hostile map: stray delimiters and a four-billion-code range are refused, never followed.
        (24, font("/ToUnicode 25 0 R")),
        (25, stream_body("", b")))) ] > << 1 beginbfrange <00000000> <FFFFFFFF> <0041> endbfrange")),
    ]);
    let input = build(&objs, "", b"");
    let needle = ["Budget Plan".to_string()];
    use TextOutcomeKind::{Dropped, Replaced, Unverifiable};

    let mut doc = open(&input);
    let out = redact_text_fields_and_save(&mut doc, &needle, &TextFieldPolicy::default(), &token()).unwrap();
    let seen: Vec<(u32, TextRole, TextOutcomeKind, u32, bool)> =
        out.report.outcomes.iter().map(|o| (o.object.num, o.role, o.kind, o.matches, o.unreadable)).collect();
    assert_eq!(
        seen,
        vec![
            (6, TextRole::AnnotContents, Replaced, 1, false),
            (8, TextRole::AnnotContents, Replaced, 1, false),
            // Decoded through /ToUnicode: the appearance shows the text although /Contents does not.
            (9, TextRole::Appearance, Dropped, 1, false),
            // Edited annotation, composite font without /ToUnicode: dropped fail-closed.
            (10, TextRole::AnnotContents, Replaced, 1, false),
            (10, TextRole::Appearance, Dropped, 0, true),
            // Not edited and unreadable: kept, reported, and the report is not complete.
            (11, TextRole::Appearance, Unverifiable, 0, true),
            (16, TextRole::Appearance, Unverifiable, 0, true),
        ]
    );
    assert_eq!((out.report.annotations_scanned, out.report.unverifiable, out.report.kept_by_policy), (7, 2, 0));
    assert!(!out.report.complete && !out.is_proven());
    assert!(out.proof.hits.is_empty(), "the byte proof sees nothing readable left: {:?}", out.proof);
    assert_eq!(out.proof.not_proven_reasons(), vec![NotProven::OpaqueText], "appearances 15 and 17 draw unreadable composite text");

    let strings = all_strings(&out.bytes);
    assert!(strings.iter().any(|s| s == "popup says [redacted]") && strings.iter().any(|s| s == "replied-to: [redacted]"));
    assert!(!leaks(&out.bytes, "budget plan"));
    assert!(find(&out.bytes, b"/N 13 0 R").is_none() && find(&out.bytes, b"/N 14 0 R").is_none());
    assert!(find(&out.bytes, b"/N 15 0 R").is_some() && find(&out.bytes, b"/N 17 0 R").is_some());
    assert_eq!(open(&out.bytes).inspect().page_count, 1);
}

// ----------------------------------------------------------------- slice 5
// Raster class, image removal, file targets, embedded-file names, XMP everywhere, ToUnicode in the
// proof, hostile decoder bounds and PDF string tokenization.

use hsk_studio_press::{RASTER_CLASS, RasterStream};

/// A minimal JPEG-shaped byte string: SOI, a comment segment, EOI.
fn jpeg(comment: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8, 0xFF, 0xFE];
    v.extend_from_slice(&((comment.len() + 2) as u16).to_be_bytes());
    v.extend_from_slice(comment);
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

fn hex_stream(bytes: &[u8]) -> Vec<u8> {
    let mut s: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    s.push('>');
    s.into_bytes()
}

const IMG: &str = "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8";
const TEXT_IN_REGION: &str = "BT /F1 12 Tf 72 700 Td (SECRET-ALPHA) Tj ET";

/// base_objs page with the image object 8 replaced and the given content.
fn raster_doc(img_dict_extra: &str, img_data: &[u8], content: &str) -> Vec<u8> {
    let mut objs = base_objs(content.as_bytes(), "<< /Title (T) >>");
    objs[7].1 = stream_body(&format!("{IMG} {img_dict_extra}"), img_data);
    build(&objs, "/Info 7 0 R", b"")
}

#[test]
fn raster_class_is_listed_and_region_images_are_removed() {
    let region = RedactionRegion { page_index: 0, rect: [60.0, 690.0, 200.0, 720.0] };
    let run = |pdf: &[u8], policy: UnsupportedPolicy| {
        let mut doc = open(pdf);
        redact_and_save(&mut doc, &[region], policy, &token())
    };
    let outside = format!("{TEXT_IN_REGION}\nq 100 0 0 100 300 300 cm /Im1 Do Q");
    let both = format!("{TEXT_IN_REGION}\nq 100 0 0 100 50 650 cm /Im1 Do Q\nq 100 0 0 100 300 300 cm /Im1 Do Q");
    let only_in = format!("{TEXT_IN_REGION}\nq 100 0 0 100 50 650 cm /Im1 Do Q");

    // The class: image XObject, chain ends in a raster codec after implemented filters. Listed,
    // not blocking: the document is proven.
    let out = run(&raster_doc("/Filter /DCTDecode", &jpeg(b"harmless"), &outside), UnsupportedPolicy::Refuse).unwrap();
    assert_eq!(
        out.proof.raster_streams,
        vec![RasterStream { object: 8, class: RASTER_CLASS, filter: "DCTDecode".into(), inline: false }]
    );
    assert!(out.proof.fully_decoded() && out.region_cleared() && out.is_proven(), "{:?}", out.proof);
    assert_eq!(RASTER_CLASS, "raster_unsearchable");

    // Still scanned: the decodable prefix (ASCIIHex) is searched, so a needle in the JPEG comment
    // segment is a hit even though it is invisible in the raw file.
    let hidden = hex_stream(&jpeg(b"scan SECRET-ALPHA here"));
    let out = run(&raster_doc("/Filter [/ASCIIHexDecode /DCTDecode]", &hidden, &outside), UnsupportedPolicy::Refuse).unwrap();
    assert!(find(&out.bytes, b"SECRET-ALPHA").is_none(), "hex-coded: not visible as raw bytes");
    assert_eq!(out.proof.raster_streams.len(), 1);
    assert!(out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedStream { object: 8, .. })));
    assert!(!out.is_proven());

    // Every other codec of the class is listed under its own name and does not block, when its data
    // has that codec's structure (lead decision PLR-040; test raster_names_need_codec_structure).
    for (codec, data, parms) in r7_codec_samples() {
        let pdf = raster_doc(&format!("/Filter /{codec} {parms}"), &data, &outside);
        let proof = verify_payload_absent(&pdf, &[b"zzzzzz".to_vec()], &Limits::default(), &token()).unwrap();
        assert_eq!(proof.raster_streams.iter().map(|r| r.filter.as_str()).collect::<Vec<_>>(), vec![codec]);
        assert!(proof.fully_decoded() && proof.is_clean(), "{codec}: {proof:?}");
    }

    // Outside the class: raster codec not last, Crypt anywhere, a non-image stream.
    let blocked = |dict: &str, subtype_image: bool| {
        let pdf = if subtype_image {
            raster_doc(dict, b"x", &outside)
        } else {
            let mut objs = base_objs(outside.as_bytes(), "<< /Title (T) >>");
            objs[0].1 = b"<< /Type /Catalog /Pages 2 0 R /Hidden 20 0 R >>".to_vec();
            objs.push((20, stream_body(dict, b"x")));
            build(&objs, "/Info 7 0 R", b"")
        };
        verify_payload_absent(&pdf, &[b"zzzzzz".to_vec()], &Limits::default(), &token()).unwrap()
    };
    for (dict, image) in [
        ("/Filter [/DCTDecode /FlateDecode]", true),
        ("/Filter /Crypt", true),
        ("/Filter [/Crypt /DCTDecode]", true),
        ("/Filter /DCTDecode", false),
        ("/Filter /JBIG2Decode", false),
    ] {
        let proof = blocked(dict, image);
        assert!(!proof.fully_decoded() && !proof.is_clean() && proof.raster_streams.is_empty(), "{dict} image={image}: {proof:?}");
    }

    // A region over an image is not proven unless the image is removed.
    let pdf = raster_doc("/Filter /DCTDecode", &jpeg(b"x"), &both);
    assert_eq!(run(&pdf, UnsupportedPolicy::Refuse).unwrap_err(), PressError::UnsupportedRedactionClass(RedactionClass::Image));
    let partial = run(&pdf, UnsupportedPolicy::ReportAndContinue).unwrap();
    assert_eq!(partial.reports[0].unredacted, vec![RedactionClass::Image]);
    assert!(!partial.region_cleared() && !partial.is_proven());
    // RemoveImages: the placement inside the region goes, the one outside stays, the image object
    // stays because it is still used.
    let removed = run(&pdf, UnsupportedPolicy::RemoveImages).unwrap();
    assert_eq!(removed.reports[0].images_removed, vec![ObjRef { num: 8, generation: 0 }]);
    assert_eq!(removed.reports[0].inline_images_removed, 0);
    assert!(removed.reports[0].complete && removed.region_cleared() && removed.is_proven(), "{:?}", removed.proof);
    let streams = independent_streams(&removed.bytes);
    assert_eq!(streams.iter().map(|s| count(s, b"/Im1 Do")).sum::<usize>(), 1);
    assert!(find(&removed.bytes, b"/Subtype /Image").is_some() && removed.proof.raster_streams.len() == 1);
    // Only placement in the region: the image and its resource entry leave the file.
    let removed = run(&raster_doc("/Filter /DCTDecode", &jpeg(b"x"), &only_in), UnsupportedPolicy::RemoveImages).unwrap();
    assert!(removed.is_proven());
    assert!(find(&removed.bytes, b"/Subtype /Image").is_none() && find(&removed.bytes, b"/Im1").is_none());
    assert!(removed.proof.raster_streams.is_empty());
    assert_eq!(independent_streams(&removed.bytes).iter().map(|s| count(s, b"/Im1 Do")).sum::<usize>(), 0);

    // Inline images: same class and same rules (the data is a JPEG: PLR-040).
    let inline = |at: &str, filter: &str| {
        let mut c = format!("{TEXT_IN_REGION}\nq 50 0 0 50 {at} cm BI /W 1 /H 1 /CS /G /BPC 8 /F /{filter} ID ").into_bytes();
        c.extend_from_slice(&jpeg(b"XXJPEGDATAXX"));
        c.extend_from_slice(b"\nEI Q");
        let mut objs = base_objs(&c, "<< /Title (T) >>");
        objs[7].1 = stream_body(&format!("{IMG} "), b"x");
        build(&objs, "/Info 7 0 R", b"")
    };
    let out = run(&inline("300 300", "DCT"), UnsupportedPolicy::Refuse).unwrap();
    // The proof over the source lists the inline image; the redaction's output holds it as an image
    // XObject (canonical rewrite, R10-F1).
    let p = verify_payload_absent(&inline("300 300", "DCT"), &[b"SECRET-ALPHA".to_vec()], &Limits::default(), &token()).unwrap();
    assert!(p.raster_streams.iter().any(|r| r.inline && r.filter == "DCT" && r.class == RASTER_CLASS));
    assert!(out.proof.raster_streams.iter().any(|r| !r.inline && r.filter == "DCTDecode" && r.class == RASTER_CLASS), "{:?}", out.proof.raster_streams);
    assert!(out.is_proven(), "{:?}", out.proof);
    let near = inline("80 660", "DCT");
    assert_eq!(run(&near, UnsupportedPolicy::Refuse).unwrap_err(), PressError::UnsupportedRedactionClass(RedactionClass::InlineImage));
    let removed = run(&near, UnsupportedPolicy::RemoveImages).unwrap();
    assert_eq!(removed.reports[0].inline_images_removed, 1);
    assert!(removed.is_proven() && independent_streams(&removed.bytes).iter().all(|s| find(s, b"XXJPEGDATAXX").is_none()));
    // An inline image with an unknown filter blocks, like a stream would: the proof over the source
    // lists it, and the redaction refuses, since where its data ends is not known (R10-F1).
    let p = verify_payload_absent(&inline("300 300", "Foo"), &[b"SECRET-ALPHA".to_vec()], &Limits::default(), &token()).unwrap();
    assert!(!p.is_clean() && p.unreadable_content_streams.iter().any(|(n, _)| *n == 4), "{:?}", p);
    assert_eq!(run(&inline("300 300", "Foo"), UnsupportedPolicy::Refuse).unwrap_err(), PressError::UnsupportedFilter("Foo".into()));
}

#[test]
fn file_targets_embedded_names_and_xmp_redacted() {
    let xmp = |title: &str| {
        format!("<?xpacket begin='x'?><x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:li>{title}</rdf:li></x:xmpmeta><?xpacket end='w'?>").into_bytes()
    };
    let mut objs = minimal_objs("/Names << /EmbeddedFiles 40 0 R >> /AF [53 0 R]", "20 0 R 21 0 R 22 0 R 23 0 R 24 0 R");
    objs[2].1 = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Metadata 60 0 R /Resources << /XObject << /Im1 61 0 R >> >> /Annots [20 0 R 21 0 R 22 0 R 23 0 R 24 0 R] >>".to_vec();
    let link = |action: &str| format!("<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << {action} >> >>").into_bytes();
    objs.extend([
        (20, link("/S /GoToR /F (Budget Plan.pdf) /D (Budget Plan A) /NewWindow true")),
        (21, link(r"/S /Launch /F (C:\\Budget Plan\\run.exe) /Win << /F (budget plan.exe) /D (C:\\budget plan) /O (open) /P (-open budget plan) >>")),
        (
            22,
            link("/S /ImportData /F << /Type /Filespec /F (budget plan.fdf) /UF (Budget Plan.fdf) /DOS (BUDGET PLAN.FDF) /Mac (Budget Plan) /Unix (budget plan.fdf) /Desc (the Budget Plan import) >>"),
        ),
        (23, link("/S /SubmitForm /F << /FS /URL /F (http://x.example/budget plan) >>")),
        (24, b"<< /Type /Annot /Subtype /FileAttachment /Rect [0 0 5 5] /Contents (attachment) /FS 50 0 R >>".to_vec()),
        (40, b"<< /Names [(Budget Plan.txt) 50 0 R (other.txt) 52 0 R] >>".to_vec()),
        (50, b"<< /Type /Filespec /F (Budget Plan.txt) /UF (Budget Plan.txt) /Desc (Budget Plan notes) /EF << /F 51 0 R >> >>".to_vec()),
        (51, stream_body("/Type /EmbeddedFile", b"unrelated payload")),
        (52, b"<< /Type /Filespec /F (other.txt) /EF << /F 51 0 R >> >>".to_vec()),
        (53, b"<< /F (af-Budget Plan.xml) /Desc (assoc Budget Plan) >>".to_vec()),
        (60, stream_body("/Type /Metadata /Subtype /XML", &xmp("page Budget Plan"))),
        (61, stream_body(&format!("{IMG} /Metadata 62 0 R"), b"\x00")),
        (62, stream_body("/Type /Metadata /Subtype /XML", &xmp("image Budget Plan"))),
    ]);
    let input = build(&objs, "", b"");
    let needle = ["Budget Plan".to_string()];
    use TextOutcomeKind::{Dropped, KeptByPolicy, Replaced};
    let r = |n: u32, direct: bool| (n, direct, Replaced, 1);

    let mut doc = open(&input);
    let out = redact_text_fields_and_save(&mut doc, &needle, &TextFieldPolicy::default(), &token()).unwrap();
    assert_eq!(
        rows(&out.report, TextRole::FileSpecName),
        vec![r(20, true), r(21, true), r(22, true), r(22, true), r(22, true), r(22, true), r(22, true), r(23, true), r(50, false), r(50, false), r(53, false)]
    );
    assert_eq!(rows(&out.report, TextRole::FileSpecDesc), vec![r(22, true), r(50, false), r(53, false)]);
    assert_eq!(rows(&out.report, TextRole::LaunchWin), vec![r(21, true), r(21, true), r(21, true)]);
    assert_eq!(rows(&out.report, TextRole::RemoteDest), vec![r(20, true)]);
    assert_eq!(rows(&out.report, TextRole::EmbeddedFileName), vec![r(40, false)]);
    assert_eq!(rows(&out.report, TextRole::Xmp), vec![(3, false, Dropped, 1), (61, false, Dropped, 1)]);
    assert!(out.report.complete && out.is_proven(), "{:?}", out.proof);

    assert!(!leaks(&out.bytes, "budget plan") && find(&out.bytes, b"xmpmeta").is_none());
    let strings = all_strings(&out.bytes);
    for expect in [
        "[redacted].pdf",
        "[redacted] a",
        "c:\\[redacted]\\run.exe",
        "[redacted].exe",
        "-open [redacted]",
        "[redacted].fdf",
        "http://x.example/[redacted]",
        "[redacted].txt",
        "[redacted] notes",
        "af-[redacted].xml",
        "other.txt",
    ] {
        assert!(strings.iter().any(|s| s == expect), "missing {expect:?} in {strings:?}");
    }
    // The embedded-file tree key became one opaque name; the tree is one sorted leaf.
    let root = {
        let at = find(&out.bytes, b"40 0 obj").expect("tree object");
        out.bytes[at..at + find(&out.bytes[at..], b"endobj").unwrap()].to_vec()
    };
    assert_eq!(strings_of(&root), vec!["hskrd0", "other.txt"]);
    assert!(find(&root, b"50 0 R").is_some() && find(&out.bytes, b"unrelated payload").is_some());
    assert_eq!(open(&out.bytes).inspect().page_count, 1);

    // file_targets off: every match is reported kept, nothing is silent (XMP is its own class).
    let mut doc = open(&input);
    let off = TextFieldPolicy { file_targets: false, ..TextFieldPolicy::default() };
    let kept = redact_text_fields_and_save(&mut doc, &needle, &off, &token()).unwrap();
    assert_eq!((kept.report.kept_by_policy, kept.report.complete), (19, false));
    assert!(rows(&kept.report, TextRole::FileSpecName).iter().all(|row| row.2 == KeptByPolicy));
    assert_eq!(rows(&kept.report, TextRole::Xmp).len(), 2);
    assert!(!kept.is_proven() && leaks(&kept.bytes, "budget plan"));
}

fn cmap_for(base: u32) -> Vec<u8> {
    let mut s = String::from("/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n1 begincodespacerange <0000> <FFFF> endcodespacerange\n11 beginbfchar\n");
    for (i, ch) in "Budget Plan".chars().enumerate() {
        s.push_str(&format!("<{:04X}> <{:04X}>\n", base + i as u32, ch as u32));
    }
    s.push_str("endbfchar\nendcmap\nend end");
    s.into_bytes()
}

fn codes(base: u32) -> String {
    (0..11).map(|i| format!("{:04X}", base + i)).collect()
}

#[test]
fn proof_reads_tounicode_text_and_region_needles() {
    let font = |tounicode: Option<u32>| {
        let extra = tounicode.map(|n| format!("/ToUnicode {n} 0 R")).unwrap_or_default();
        format!("<< /Type /Font /Subtype /Type0 /BaseFont /Test /Encoding /Identity-H /DescendantFonts [21 0 R] {extra} >>").into_bytes()
    };
    let (a, b) = (codes(1), codes(0x1001));
    let mut objs = minimal_objs("", "5 0 R");
    objs[2].1 = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 20 0 R /F2 23 0 R /F3 26 0 R >> >> /Annots [5 0 R] >>".to_vec();
    objs[3].1 = stream_body(
        "",
        format!("BT /F1 12 Tf 72 700 Td <{a}> Tj ET\nBT /F2 12 Tf 72 400 Td <{b}> Tj ET\nBT /F3 12 Tf 72 300 Td <00010002> Tj ET").as_bytes(),
    );
    objs.extend([
        (5, b"<< /Type /Annot /Subtype /FreeText /Rect [100 100 300 140] /Contents (label) /AP << /N 13 0 R >> >>".to_vec()),
        (
            13,
            stream_body(
                "/Type /XObject /Subtype /Form /BBox [0 0 200 40] /Resources << /Font << /F2 23 0 R >> >>",
                format!("BT /F2 12 Tf 2 5 Td <{b}> Tj ET").as_bytes(),
            ),
        ),
        (20, font(Some(22))),
        (21, b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Test /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> >>".to_vec()),
        (22, stream_body("", &cmap_for(1))),
        (23, font(Some(24))),
        (24, stream_body("", &cmap_for(0x1001))),
        (26, font(None)),
    ]);
    let pdf = build(&objs, "", b"");
    let region = RedactionRegion { page_index: 0, rect: [60.0, 690.0, 200.0, 720.0] };
    let code_bytes: Vec<u8> = (1u16..=11).flat_map(u16::to_be_bytes).collect();

    // The removed string is composite-font codes; read through its /ToUnicode it is text.
    let mut doc = open(&pdf);
    let plain = redact_and_save(&mut doc, &[region], UnsupportedPolicy::Refuse, &token()).unwrap();
    assert_eq!(plain.reports[0].removed_text, vec![code_bytes.clone()]);
    assert_eq!(plain.reports[0].removed_unicode, vec![Some("Budget Plan".to_string())]);
    // The page also draws F3 text that cannot be read: the region is not reported clear.
    assert!(!plain.region_cleared());
    assert_eq!(plain.not_proven_reasons(), vec![NotProven::OpaqueText, NotProven::Leak]);
    // The same words in other font codes are found: kept page text is a collision, the same text
    // in an annotation appearance is a hit (it is annotation text), and the page's composite text
    // that cannot be read at all is listed.
    assert_eq!(plain.proof.kept_text_collisions.len(), 1);
    let page_content = plain.proof.kept_text_collisions[0].object;
    assert_eq!(plain.proof.opaque_text_streams, vec![page_content]);
    assert!(plain.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedText { object: 13 })));
    assert!(!plain.is_proven());
    // A byte-only needle (no ToUnicode reading) is blind to both.
    let blind = verify_payload_absent(&plain.bytes, &[code_bytes], &Limits::default(), &token()).unwrap();
    assert!(blind.kept_text_collisions.is_empty() && blind.hits.is_empty());

    // With a text policy the appearance (found through its ToUnicode) is dropped and the document is proven.
    let mut doc = open(&pdf);
    let fixed = redact_and_save_with(&mut doc, &[region], UnsupportedPolicy::Refuse, Some(&TextFieldPolicy::default()), &token()).unwrap();
    assert!(fixed.proof.hits.is_empty());
    assert_eq!(fixed.proof.kept_text_collisions.len(), 1);
    assert_eq!(rows(fixed.text_fields.as_ref().unwrap(), TextRole::Appearance), vec![(5, false, TextOutcomeKind::Dropped, 1)]);
    assert!(find(&fixed.bytes, b"/N 13 0 R").is_none());
    // The page still draws F3 text that cannot be read at all: not proven, with the typed reason.
    assert!(!fixed.is_proven() && !fixed.region_cleared());
    assert_eq!(fixed.not_proven_reasons(), vec![NotProven::OpaqueText]);
    // Without that text the same document is proven.
    let mut clear = objs.clone();
    clear[3].1 = stream_body("", format!("BT /F1 12 Tf 72 700 Td <{a}> Tj ET\nBT /F2 12 Tf 72 400 Td <{b}> Tj ET").as_bytes());
    let mut doc = open(&build(&clear, "", b""));
    let clean = redact_and_save_with(&mut doc, &[region], UnsupportedPolicy::Refuse, Some(&TextFieldPolicy::default()), &token()).unwrap();
    assert!(clean.is_proven() && clean.proof.opaque_text_streams.is_empty(), "{:?}", clean.not_proven_reasons());
}

#[test]
fn hostile_decoder_bounds_dims_and_aggregate() {
    let needle = vec!["Budget Plan".to_string()];
    let policy = TextFieldPolicy::default();
    // An annotation whose appearance draws code 0001 with a composite font whose /ToUnicode is `map`.
    let ap_doc = |map: &[u8], second_map: Option<&[u8]>| {
        let mut objs = minimal_objs("", if second_map.is_some() { "5 0 R 6 0 R" } else { "5 0 R" });
        let annot = |ap: u32| format!("<< /Type /Annot /Subtype /FreeText /Rect [0 0 9 9] /Contents (fine) /AP << /N {ap} 0 R >> >>").into_bytes();
        let ap = |font: u32| {
            stream_body(
                &format!("/Type /XObject /Subtype /Form /BBox [0 0 9 9] /Resources << /Font << /F1 {font} 0 R >> >>"),
                b"BT /F1 12 Tf <0001> Tj ET",
            )
        };
        let f = |tu: u32| format!("<< /Type /Font /Subtype /Type0 /BaseFont /T /Encoding /Identity-H /DescendantFonts [21 0 R] /ToUnicode {tu} 0 R >>").into_bytes();
        objs.extend([
            (5, annot(13)),
            (13, ap(20)),
            (20, f(22)),
            (21, b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /T >>".to_vec()),
            (22, stream_body("", map)),
        ]);
        if let Some(m2) = second_map {
            objs.extend([(6, annot(14)), (14, ap(23)), (23, f(24)), (24, stream_body("", m2))]);
        }
        build(&objs, "", b"")
    };

    // dims_u32_max: code widths and ranges at the u32 edge are refused (the font becomes opaque and
    // the appearance Unverifiable), never followed; a valid edge-of-range map does not wrap or panic.
    let five_ranges: String = (0..5).map(|i| format!("1 beginbfrange <{i:02X}0000> <{i:02X}FFFF> <0041> endbfrange\n")).collect();
    let maps: [(&str, Vec<u8>, usize); 4] = [
        ("range 0..u32::MAX", b"1 beginbfrange <00000000> <FFFFFFFF> <0041> endbfrange".to_vec(), 1),
        ("five-byte code", b"1 beginbfchar <0000000001> <0041> endbfchar".to_vec(), 1),
        ("entry cap", five_ranges.into_bytes(), 1),
        // The map also names the drawn code 0001: since R8 a code the map does not name is opaque
        // text (Cmap::decode_checked), so the valid edge range sits beside a mapping for what is shown.
        ("u32 edge, valid", b"1 beginbfchar <0001> <0041> endbfchar 1 beginbfrange <FFFFFFFE> <FFFFFFFF> <FFFF> endbfrange".to_vec(), 0),
    ];
    for (what, map, unverifiable) in maps {
        let mut doc = open(&ap_doc(&map, None));
        let report = doc.redact_text_fields(&needle, &policy, &token()).unwrap();
        assert_eq!(report.unverifiable as usize, unverifiable, "{what}: {report:?}");
    }

    // aggregate_over_budget: streams that are each within the per-stream cap (512 KiB of 1 MiB) but
    // sum past the total (2 MiB).
    let zeros = vec![0u8; 512 << 10];
    let mut objs = minimal_objs("", "");
    objs[2].1 = format!(
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [{}] /Resources << >> >>",
        (10..18).map(|n| format!("{n} 0 R")).collect::<Vec<_>>().join(" ")
    )
    .into_bytes();
    objs.extend((10..18).map(|n| (n, stream_body("", &zeros))));
    let tight = Limits { max_decoded_bytes: 1 << 20, max_decoded_total: 2 << 20, ..Limits::default() };
    let pdf = build(&objs, "", b"");
    assert_eq!(verify_payload_absent(&pdf, &[b"zzzzzz".to_vec()], &tight, &token()).unwrap_err(), PressError::InputLimit("decoded_total"));
    let mut doc = PdfDoc::open(&pdf, &tight, &token()).unwrap();
    let region = RedactionRegion { page_index: 0, rect: [0.0, 0.0, 10.0, 10.0] };
    assert_eq!(doc.redact_region(&region, UnsupportedPolicy::Refuse, &token()).unwrap_err(), PressError::InputLimit("decoded_total"));
    // The same budget covers /ToUnicode streams read by the text-field pass: two fonts of 1 MiB each
    // against a 1.5 MiB total, and the document is unchanged afterwards.
    let big_map = |tail: &str| {
        let mut m = vec![b' '; 1 << 20];
        m.extend_from_slice(format!("1 beginbfchar <0001> <0041> endbfchar {tail}").as_bytes());
        m
    };
    let budget = Limits { max_decoded_bytes: 2 << 20, max_decoded_total: 3 << 19, ..Limits::default() };
    let pdf = ap_doc(&big_map(""), Some(&big_map("")));
    let mut doc = PdfDoc::open(&pdf, &budget, &token()).unwrap();
    let before = doc.save_full(&token()).unwrap();
    assert!(matches!(doc.redact_text_fields(&needle, &policy, &token()), Err(PressError::InputLimit(_))));
    assert_eq!(doc.save_full(&token()).unwrap(), before);

    // An inline image dictionary is bounded like any operand list.
    let keys: String = (0..300).map(|i| format!("/K{i} 1 ")).collect();
    let mut bad = base_objs(format!("BT ET\nBI {keys}ID x\nEI").as_bytes(), "<< /Title (T) >>");
    bad[7].1 = stream_body(IMG, b"\x00");
    let mut doc = open(&build(&bad, "", b""));
    let region = RedactionRegion { page_index: 0, rect: [0.0, 0.0, 10.0, 10.0] };
    assert_eq!(doc.redact_region(&region, UnsupportedPolicy::Refuse, &token()).unwrap_err(), PressError::InputLimit("operands"));

    // The decodable prefix of a raster stream is bounded like any stream.
    let bomb = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 8 << 20], 6);
    let pdf = raster_doc("/Filter [/FlateDecode /DCTDecode]", &bomb, "q Q");
    let small = Limits { max_decoded_bytes: 1 << 20, ..Limits::default() };
    assert_eq!(verify_payload_absent(&pdf, &[b"zzzzzz".to_vec()], &small, &token()).unwrap_err(), PressError::InputLimit("decoded_bytes"));
}

#[test]
fn pdf_string_tokenization_adversarial() {
    let mut objs = minimal_objs("/Outlines 10 0 R", "20 0 R 21 0 R 22 0 R 23 0 R 24 0 R");
    let link = |action: &str| format!("<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << {action} >> >>").into_bytes();
    let u16hex_raw = |s: &str| u16hex(s);
    objs.extend([
        // Escapes: a name spelled with #xx, octal escapes, a hex string with white space.
        (20, link(r"/S /U#52I /URI (http://x.example/\102udget\040Plan)")),
        (21, link("/S /URI /URI <6874 7470 3A2F 2F78 2E65 7861 6D70 6C65 2F42 7564 6765 7420 506C 616E>")),
        (22, link(r"/S /GoT#6FR /F (\102udget Plan.pdf)")),
        // Nested parentheses and PDF syntax inside text are text, not structure.
        (23, b"<< /Type /Annot /Subtype /Text /Rect [0 0 5 5] /Contents (a (Budget Plan) b /S /URI /URI (http://evil/Budget Plan)) >>".to_vec()),
        // Comment decoy and a wrongly cased action name: /uri is no URI action.
        (
            24,
            b"<< /Type /Annot /Subtype /Link /Rect [0 0 5 5] % /S /URI /URI (Budget Plan)\n /A << /S /uri /URI (http://x.example/Budget Plan) >> >>".to_vec(),
        ),
        (10, b"<< /Type /Outlines /First 11 0 R /Last 13 0 R /Count 3 >>".to_vec()),
        // Case, white space, no-break space, zero-width space.
        (11, b"<< /Title (BUDGET \\t\\n  plan) /Parent 10 0 R /Next 12 0 R >>".to_vec()),
        (12, format!("<< /Title {} /Parent 10 0 R /Prev 11 0 R /Next 13 0 R >>", u16hex_raw("Budget\u{a0}Plan")).into_bytes()),
        (13, format!("<< /Title {} /Parent 10 0 R /Prev 12 0 R >>", u16hex_raw("Bud\u{200b}get   Plan")).into_bytes()),
    ]);
    let input = build(&objs, "", b"");
    let mut doc = open(&input);
    let out = redact_text_fields_and_save(&mut doc, &["Budget Plan".to_string()], &TextFieldPolicy::default(), &token()).unwrap();
    use TextOutcomeKind::Replaced;
    let r = |n: u32, direct: bool| (n, direct, Replaced, 1);

    // Escapes are decoded before matching, names are decoded before dispatch.
    assert_eq!(rows(&out.report, TextRole::ActionUri), vec![r(20, true), r(21, true)]);
    assert_eq!(rows(&out.report, TextRole::FileSpecName), vec![r(22, true)]);
    let strings = all_strings(&out.bytes);
    assert_eq!(strings.iter().filter(|s| *s == "http://x.example/[redacted]").count(), 2);
    assert!(strings.iter().any(|s| s == "[redacted].pdf"));
    // Nested parentheses balance; the URI-looking text inside /Contents is text, redacted as text,
    // and does not become an action.
    assert_eq!(rows(&out.report, TextRole::AnnotContents), vec![(23, false, Replaced, 2)]);
    assert!(strings.iter().any(|s| s == "a ([redacted]) b /s /uri /uri (http://evil/[redacted])"), "{strings:?}");
    // Case, white-space runs, NBSP and zero-width characters do not hide a title.
    assert_eq!(rows(&out.report, TextRole::OutlineTitle), vec![r(11, false), r(12, false), r(13, false)]);
    // The comment decoy and the wrongly cased /uri are not actions: no outcome for object 24, and the
    // proof keeps reporting what it still sees there (nothing is silently kept).
    assert!(out.report.outcomes.iter().all(|o| o.object.num != 24));
    assert!(!out.is_proven());
    let at = find(&out.bytes, b"24 0 obj").expect("object 24");
    let end = at + find(&out.bytes[at..], b"endobj").unwrap();
    assert!(!out.proof.hits.is_empty());
    assert!(out.proof.hits.iter().all(|h| match h.location {
        PayloadLocation::StringObject { object } => object == 24,
        PayloadLocation::RawBytes { offset } => (at..end).contains(&offset),
        _ => false,
    }));
    assert!(out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::StringObject { object: 24 })));
    assert!(out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::RawBytes { .. })));
}

// ----------------------------------------------------------------- opaque text blocks the proof

use hsk_studio_press::NotProven;

/// Text drawn with a composite font that has no /ToUnicode map is text the proof cannot check. The
/// byte scan finds nothing, so without this rule the result would read as proven while the needle
/// is visibly on the page (the R5 class "proven while some text was never checked").
#[test]
fn opaque_composite_text_blocks_the_proof() {
    let region = RedactionRegion { page_index: 0, rect: [60.0, 690.0, 200.0, 720.0] };
    let doc_with = |kept: &str, removed: &str| {
        let mut objs = minimal_objs("", "");
        objs[2].1 = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R /F2 23 0 R >> >> >>".to_vec();
        objs[3].1 = stream_body("", format!("BT /F1 12 Tf 72 700 Td {removed} Tj ET\n{kept}").as_bytes());
        objs.extend([
            (6, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()),
            // Identity-H, no /ToUnicode: the codes below are glyph ids, not text.
            (23, b"<< /Type /Font /Subtype /Type0 /BaseFont /T /Encoding /Identity-H /DescendantFonts [21 0 R] >>".to_vec()),
            (21, b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /T >>".to_vec()),
        ]);
        build(&objs, "", b"")
    };
    let cid_text = "BT /F2 12 Tf 72 400 Td <010101020103010401050106> Tj ET";
    let run = |pdf: &[u8]| {
        let mut doc = open(pdf);
        redact_and_save(&mut doc, &[region], UnsupportedPolicy::Refuse, &token()).unwrap()
    };

    // A needle search ran (SECRET-ALPHA was removed) and the page draws unreadable CID text.
    let out = run(&doc_with(cid_text, "(SECRET-ALPHA)"));
    assert_eq!(out.reports[0].removed_text, vec![b"SECRET-ALPHA".to_vec()]);
    assert!(out.proof.hits.is_empty(), "the byte scan sees nothing: this is the false green of 272909b");
    assert_eq!(out.proof.needles_searched, 1);
    assert_eq!(out.proof.opaque_text_streams.len(), 1);
    assert_eq!(out.not_proven_reasons(), vec![NotProven::OpaqueText]);
    assert_eq!(NotProven::OpaqueText.code(), "opaque_text");
    assert!(!out.is_proven() && !out.region_cleared() && !out.proof.is_clean());

    // Control: the same document without the CID text is proven.
    let out = run(&doc_with("", "(SECRET-ALPHA)"));
    assert!(out.is_proven() && out.proof.opaque_text_streams.is_empty(), "{:?}", out.not_proven_reasons());

    // No text-needle search ran (the only removed string is shorter than the distinctive minimum).
    // CHANGED by R9 (lead decision 2026-10-11T13:10:26Z, positive determination): a redaction is
    // proven / cleared only if every text-showing operator of the saved file is determined, whatever
    // was removed, so the unreadable CID text now blocks (before R9 it was listed but read proven:
    // the R9-F3 (iii) gap). A plain payload check without needles still has nothing to look for.
    let out = run(&doc_with(cid_text, "(ab)"));
    assert_eq!((out.proof.needles_searched, out.short_needles), (0, 1));
    assert_eq!(out.proof.opaque_text_streams.len(), 1);
    assert_eq!(out.not_proven_reasons(), vec![NotProven::OpaqueText]);
    assert!(!out.is_proven() && !out.region_cleared() && out.proof.is_clean());

    // The text-field flow gives the same answer.
    let mut doc = open(&doc_with(cid_text, "(SECRET-ALPHA)"));
    let fields = redact_text_fields_and_save(&mut doc, &["SECRET-ALPHA".to_string()], &TextFieldPolicy::default(), &token()).unwrap();
    assert_eq!(fields.not_proven_reasons(), vec![NotProven::OpaqueText]);
    assert!(!fields.is_proven());
}

// ------------------------------------------------- workflow v4 hostile parser hardening

/// Appends a classic cross-reference table with one subsection per run of consecutive object numbers.
fn xref_tail(out: &mut Vec<u8>, off: &BTreeMap<u32, usize>, trailer_extra: &str) {
    let xref = out.len();
    let size = off.keys().max().map_or(1, |m| m + 1);
    out.extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
    let nums: Vec<u32> = off.keys().copied().collect();
    let mut i = 0;
    while i < nums.len() {
        let mut j = i;
        while j + 1 < nums.len() && nums[j + 1] == nums[j] + 1 {
            j += 1;
        }
        out.extend(format!("{} {}\n", nums[i], j - i + 1).into_bytes());
        for n in &nums[i..=j] {
            out.extend(format!("{:010} 00000 n \n", off[n]).into_bytes());
        }
        i = j + 1;
    }
    out.extend(format!("trailer\n<< /Size {size} /Root 1 0 R {trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n").into_bytes());
}

/// Like `build`, but the table lists only the objects given (no free entries for gaps).
fn build_sparse(objs: &[(u32, Vec<u8>)], trailer_extra: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut off: BTreeMap<u32, usize> = BTreeMap::new();
    for (n, body) in objs {
        off.insert(*n, out.len());
        out.extend(obj_bytes(*n, body));
    }
    xref_tail(&mut out, &off, trailer_extra);
    out
}

/// A PDF whose cross-reference is an uncompressed xref stream (object `xs`, rows /W [1 4 1]) covering
/// the object numbers 0..=max(xs, members). `in_stream` lists objects held by an object stream as
/// (number, stream, index). `dict_extra` is appended to the xref stream dictionary; a later duplicate
/// key wins, so it can override /Size, /W and /Index.
fn xref_stream_pdf(objs: &[(u32, Vec<u8>)], xs: u32, dict_extra: &str, in_stream: &[(u32, u32, u8)]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut off: BTreeMap<u32, usize> = BTreeMap::new();
    for (n, body) in objs {
        off.insert(*n, out.len());
        out.extend(obj_bytes(*n, body));
    }
    let xs_off = out.len();
    let hi = in_stream.iter().map(|(n, _, _)| *n).max().map_or(xs, |m| m.max(xs));
    let mut rows = Vec::new();
    for n in 0..=hi {
        let row: [u8; 6] = if let Some((_, stm, idx)) = in_stream.iter().find(|(m, _, _)| *m == n) {
            let b = stm.to_be_bytes();
            [2, b[0], b[1], b[2], b[3], *idx]
        } else if n == xs {
            let b = (xs_off as u32).to_be_bytes();
            [1, b[0], b[1], b[2], b[3], 0]
        } else if let Some(o) = off.get(&n) {
            let b = (*o as u32).to_be_bytes();
            [1, b[0], b[1], b[2], b[3], 0]
        } else {
            [0, 0, 0, 0, 0, 0]
        };
        rows.extend_from_slice(&row);
    }
    let dict = format!("/Type /XRef /Size {} /W [1 4 1] /Root 1 0 R {dict_extra}", hi + 1);
    out.extend(obj_bytes(xs, &stream_body(&dict, &rows)));
    out.extend(format!("startxref\n{xs_off}\n%%EOF\n").into_bytes());
    out
}

#[test]
fn hostile_parser_dims_u32_max() {
    let open_err = |b: &[u8], l: &Limits| PdfDoc::open(b, l, &token()).err();
    let limits = Limits::default();
    let base = minimal_objs("", "");
    let with_xref = |extra: &str| xref_stream_pdf(&base, 5, extra, &[]);

    // The control: the helper's own file opens.
    assert_eq!(open(&with_xref("")).inspect().page_count, 1);

    // /W (ISO 32000-2 7.5.8.2): a field wider than 8 bytes, a wrong count, all-zero or negative widths.
    for w in ["[9 1 1]", "[4294967295 1 1]", "[1 4]", "[1 4 1 1]", "[0 0 0]", "[-1 4 1]"] {
        assert_eq!(open_err(&with_xref(&format!("/W {w}")), &limits), Some(PressError::Malformed("xref_w")), "/W {w}");
    }

    // /Size and /Index counts the stream data cannot hold, at the u32 and i64 edge.
    for extra in ["/Size 4294967295", "/Size 9223372036854775807", "/Index [0 4294967295]", "/Index [0 6 6 4294967295]"] {
        assert_eq!(open_err(&with_xref(extra), &limits), Some(PressError::Malformed("xref_stream_short")), "{extra}");
    }

    // Object numbers past Limits::max_object_number (8,388,607), past u32, or negative.
    for (extra, want) in [
        ("/Index [4294967295 1]", PressError::InputLimit("object_number")),
        ("/Index [8388608 1]", PressError::InputLimit("object_number")),
        ("/Index [4294967296 1]", PressError::Malformed("xref_number")),
        ("/Index [-1 1]", PressError::Malformed("xref_index")),
    ] {
        assert_eq!(open_err(&with_xref(extra), &limits), Some(want), "{extra}");
    }
    let low = Limits { max_object_number: 4, ..Limits::default() };
    assert_eq!(open_err(&with_xref(""), &low), Some(PressError::InputLimit("object_number")));

    // The same dims in a classic table (7.5.4): subsection counts and first numbers at the edges.
    let classic = |sub: &str| {
        let good = build(&base, "", b"");
        let at = good.windows(4).position(|w| w == b"xref").unwrap();
        let mut b = good[..at].to_vec();
        let xref = b.len();
        b.extend(format!("xref\n{sub}\n0000000000 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").into_bytes());
        b
    };
    for (sub, want) in [
        ("4294967295 1", PressError::InputLimit("object_number")),
        ("0 4294967295", PressError::Malformed("xref_count")),
        ("4294967295 4294967295", PressError::Malformed("xref_count")),
        ("18446744073709551615 1", PressError::Malformed("xref_number")),
        ("18446744073709551616 1", PressError::Malformed("integer_overflow")),
    ] {
        assert_eq!(open_err(&classic(sub), &limits), Some(want), "subsection {sub}");
    }

    // /Length at the u32/i64 edge, negative, overflowing, or an indirect non-number: the stream is
    // recovered by its endstream, nothing is allocated from the declared size, and no huge object is parsed.
    for len in ["4294967295", "9223372036854775807", "-1", "18446744073709551616", "5 0 R"] {
        let mut objs = base.clone();
        let mut body = format!("<< /Length {len} >>\nstream\n").into_bytes();
        body.extend_from_slice(b"q 1 0 0 1 0 0 cm Q kept");
        body.extend_from_slice(b"\nendstream");
        objs[3].1 = body;
        objs.push((5, format!("<< /NotANumber [{}] >>", "1 ".repeat(1000)).into_bytes()));
        let doc = PdfDoc::open(&build(&objs, "", b""), &limits, &token()).unwrap_or_else(|e| panic!("/Length {len}: {e}"));
        assert!(find(&doc.save_full(&token()).unwrap(), b"Q kept").is_some(), "/Length {len}");
    }

    // Predictor dims (7.4.4.4) outside their ranges are a typed decode failure, never a row allocation.
    let pred_doc = |parms: &str, data: &[u8]| {
        let mut objs = base.clone();
        objs[3].1 = stream_body(&format!("/Filter /FlateDecode /DecodeParms << {parms} >>"), &miniz_oxide::deflate::compress_to_vec_zlib(data, 6));
        build(&objs, "", b"")
    };
    let needle = vec![b"zzzzzz".to_vec()];
    for parms in [
        "/Predictor 12 /Columns 4294967295",
        "/Predictor 12 /Columns 0",
        "/Predictor 12 /Columns 1048577",
        "/Predictor 12 /Columns -1",
        "/Predictor 12 /Colors 33",
        "/Predictor 12 /BitsPerComponent 3",
        "/Predictor 99",
    ] {
        let proof = verify_payload_absent(&pred_doc(parms, b"\x00abcd"), &needle, &limits, &token()).unwrap();
        assert!(!proof.fully_decoded(), "{parms}");
    }
    // The widest legal row (2^20 columns x 32 colours x 16 bits = 64 MiB per row) over five bytes of
    // data decodes to those bytes under a 1 KiB cap: rows are never padded to the declared width.
    let tight = Limits { max_decoded_bytes: 1024, ..Limits::default() };
    let wide = "/Predictor 12 /Columns 1048576 /Colors 32 /BitsPerComponent 16";
    let proof = verify_payload_absent(&pred_doc(wide, b"\x00abcd"), &needle, &tight, &token()).unwrap();
    assert!(proof.fully_decoded(), "{proof:?}");

    // A filter chain longer than Limits::max_filters is refused; eight filters are not.
    let chain = |n: usize| {
        let mut objs = base.clone();
        objs[3].1 = stream_body(&format!("/Filter [{}]", "/ASCIIHexDecode ".repeat(n)), b"41>");
        build(&objs, "", b"")
    };
    assert_eq!(verify_payload_absent(&chain(9), &needle, &limits, &token()).unwrap_err(), PressError::InputLimit("filter_chain"));
    assert!(verify_payload_absent(&chain(8), &needle, &limits, &token()).is_ok());

    // The highest live object number sizes the table of a save: 8,388,607 opens, but its 168 MB
    // table is refused before a line is written; a lower limit refuses the file at open.
    let mut far = base.clone();
    far[0].1 = b"<< /Type /Catalog /Pages 2 0 R /Far 8388607 0 R >>".to_vec();
    far.push((8_388_607, b"<< /Note (end) >>".to_vec()));
    let pdf = build_sparse(&far, "");
    assert_eq!(open(&pdf).save_full(&token()).unwrap_err(), PressError::InputLimit("max_output_bytes"));
    assert_eq!(open_err(&pdf, &Limits { max_object_number: 100, ..limits }), Some(PressError::InputLimit("object_number")));
    // Numbers at the edge that do fit save and reopen.
    let mut near = base.clone();
    near[0].1 = b"<< /Type /Catalog /Pages 2 0 R /Near 300 0 R >>".to_vec();
    near.push((300, b"<< /Note (end) >>".to_vec()));
    let saved = open(&build_sparse(&near, "")).save_full(&token()).unwrap();
    assert_eq!(open(&saved).inspect().object_count, 5);
}

#[test]
fn aggregate_over_budget_many_streams() {
    // 64 streams of 512 KiB of zeros (about 600 bytes of file each): every one is inside the 1 MiB
    // per-stream cap, together they pass the 8 MiB total of the operation.
    let limits = Limits { max_decoded_bytes: 1 << 20, max_decoded_total: 8 << 20, ..Limits::default() };
    let page_with = |filter: &str, body: &[u8]| {
        let mut objs = minimal_objs("", "");
        objs[2].1 = format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents [{}] /Resources << >> >>",
            (10..74).map(|n| format!("{n} 0 R")).collect::<Vec<_>>().join(" ")
        )
        .into_bytes();
        objs.extend((10..74).map(|n| (n, stream_body(filter, body))));
        build(&objs, "", b"")
    };
    let zeros = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 512 << 10], 6);
    let pdf = page_with("/Filter /FlateDecode", &zeros);
    let needle = vec![b"zzzzzz".to_vec()];
    assert_eq!(verify_payload_absent(&pdf, &needle, &limits, &token()).unwrap_err(), PressError::InputLimit("decoded_total"));
    let mut doc = PdfDoc::open(&pdf, &limits, &token()).unwrap();
    let region = RedactionRegion { page_index: 0, rect: [0.0, 0.0, 10.0, 10.0] };
    assert_eq!(doc.redact_region(&region, UnsupportedPolicy::Refuse, &token()).unwrap_err(), PressError::InputLimit("decoded_total"));
    // The control: the same document with a 64 MiB total decodes (64 x 512 KiB = 32 MiB).
    let roomy = Limits { max_decoded_total: 64 << 20, ..limits };
    assert!(verify_payload_absent(&pdf, &needle, &roomy, &token()).unwrap().fully_decoded());

    // A chain pays for its intermediate results, not only for the last one: Flate expands to 512 KiB
    // of '>' and ASCIIHex stops at the first byte, so every final output is empty (zero bytes each),
    // yet the 64 inflations are 32 MiB of work.
    let gt = miniz_oxide::deflate::compress_to_vec_zlib(&vec![b'>'; 512 << 10], 6);
    let chained = page_with("/Filter [/FlateDecode /ASCIIHexDecode]", &gt);
    assert_eq!(verify_payload_absent(&chained, &needle, &limits, &token()).unwrap_err(), PressError::InputLimit("decoded_total"));
    assert!(verify_payload_absent(&chained, &needle, &roomy, &token()).unwrap().fully_decoded());

    // Object streams count too: 64 object streams of 512 KiB against a 4 MiB total are refused at open.
    let mut objs = minimal_objs("", "");
    let mut members = Vec::new();
    for i in 0..64u32 {
        let mut text = b"<< /Pad [".to_vec();
        text.extend(std::iter::repeat_n(b' ', 512 << 10));
        text.extend_from_slice(b"] >>");
        let data = miniz_oxide::deflate::compress_to_vec_zlib(&[format!("{} 0 ", 100 + i).into_bytes(), text].concat(), 6);
        let first = format!("{} 0 ", 100 + i).len();
        objs.push((10 + i, stream_body(&format!("/Type /ObjStm /N 1 /First {first} /Filter /FlateDecode"), &data)));
        members.push((100 + i, 10 + i, 0u8));
    }
    let pdf = xref_stream_pdf(&objs, 200, "", &members);
    let tight = Limits { max_decoded_bytes: 1 << 20, max_decoded_total: 4 << 20, ..Limits::default() };
    assert_eq!(PdfDoc::open(&pdf, &tight, &token()).err(), Some(PressError::InputLimit("decoded_total")));
    assert!(PdfDoc::open(&pdf, &Limits { max_decoded_total: 128 << 20, ..tight }, &token()).is_ok());
}

#[test]
fn hostile_nesting_cycles_and_overlap() {
    let open_err = |b: &[u8], l: &Limits| PdfDoc::open(b, l, &token()).err();
    let limits = Limits::default();
    let base = minimal_objs("", "");

    // Nesting: whatever Limits::max_depth says, the tokenizer stops at its hard ceiling (no stack overflow).
    let huge_depth = Limits { max_depth: usize::MAX, ..limits };
    for opener in ["[", "<< /A "] {
        let mut deep = base_objs(CONTENT.as_bytes(), "");
        deep[5].1 = opener.repeat(100_000).into_bytes();
        assert_eq!(open_err(&build(&deep, "", b""), &huge_depth), Some(PressError::InputLimit("depth")), "{opener}");
    }

    // Object and numeric-token budgets.
    let mut wide = base_objs(CONTENT.as_bytes(), "");
    wide[5].1 = format!("[{}]", "0 ".repeat(5000)).into_bytes();
    let wide_pdf = build(&wide, "", b"");
    assert!(PdfDoc::open(&wide_pdf, &limits, &token()).is_ok());
    assert_eq!(open_err(&wide_pdf, &Limits { max_nodes: 1000, ..limits }), Some(PressError::InputLimit("nodes")));
    wide[5].1 = format!("[{}]", "7".repeat(70)).into_bytes();
    assert_eq!(open_err(&build(&wide, "", b""), &limits), Some(PressError::Malformed("number_length")));

    // Reference chains and cycles end as null after MAX_REF_HOPS hops: 15 objects resolve, 16 do not.
    let media = |bodies: Vec<Vec<u8>>| {
        let mut objs = minimal_objs("", "");
        objs[2].1 = b"<< /Type /Page /Parent 2 0 R /MediaBox 10 0 R /Contents 4 0 R /Resources << >> >>".to_vec();
        objs.extend(bodies.into_iter().enumerate().map(|(i, b)| (10 + i as u32, b)));
        open(&build(&objs, "", b"")).proposal(&token()).unwrap().pages[0].media_box
    };
    let chain = |k: usize| (0..k).map(|i| if i + 1 == k { b"[0 0 100 100]".to_vec() } else { format!("{} 0 R", 11 + i).into_bytes() }).collect::<Vec<_>>();
    assert_eq!(media(chain(1)), Some([0.0, 0.0, 100.0, 100.0]));
    assert_eq!(media(chain(15)), Some([0.0, 0.0, 100.0, 100.0]));
    assert_eq!(media(chain(16)), None);
    assert_eq!(media(chain(500)), None);
    assert_eq!(media(vec![b"10 0 R".to_vec()]), None, "a self reference");
    assert_eq!(media(vec![b"11 0 R".to_vec(), b"10 0 R".to_vec()]), None, "a two-object cycle");
    // A /Parent chain that loops (the page is its own parent) is bounded too.
    let mut self_parent = base.clone();
    self_parent[2].1 = b"<< /Type /Page /Parent 3 0 R /Contents 4 0 R /Resources << >> >>".to_vec();
    assert_eq!(open(&build(&self_parent, "", b"")).proposal(&token()).unwrap().pages[0].media_box, None);

    // An AcroForm /Kids cycle is refused; a huge /T shared by many fields, or inherited by many kids,
    // cannot be copied once per field past Limits::max_bytes.
    let mut form = minimal_objs("/AcroForm << /Fields [10 0 R] >>", "");
    form.push((10, b"<< /FT /Tx /T (a) /Kids [10 0 R] >>".to_vec()));
    assert_eq!(open(&build(&form, "", b"")).proposal(&token()).unwrap_err(), PressError::Malformed("field_cycle"));
    let big = "A".repeat(100_000);
    let small = Limits { max_bytes: 512 << 10, ..limits };
    let mut inherit = minimal_objs("/AcroForm << /Fields [10 0 R] >>", "");
    let kids: String = (11..61).map(|n| format!("{n} 0 R ")).collect();
    inherit.push((10, format!("<< /FT /Tx /T ({big}) /Kids [{kids}] >>").into_bytes()));
    inherit.extend((11..61).map(|n| (n, b"<< /T (k) /Parent 10 0 R >>".to_vec())));
    let pdf = build(&inherit, "", b"");
    assert_eq!(open(&pdf).proposal(&token()).unwrap().form_fields.len(), 51);
    assert_eq!(PdfDoc::open(&pdf, &small, &token()).unwrap().proposal(&token()).unwrap_err(), PressError::InputLimit("field_names"));
    let roots: String = (11..61).map(|n| format!("{n} 0 R ")).collect();
    let mut shared = minimal_objs(&format!("/AcroForm << /Fields [{roots}] >>"), "");
    shared.push((9, format!("({big})").into_bytes()));
    shared.extend((11..61).map(|n| (n, b"<< /FT /Tx /T 9 0 R >>".to_vec())));
    let pdf = build(&shared, "", b"");
    assert_eq!(open(&pdf).proposal(&token()).unwrap().form_fields.len(), 50);
    assert_eq!(PdfDoc::open(&pdf, &small, &token()).unwrap().proposal(&token()).unwrap_err(), PressError::InputLimit("field_names"));

    // Overlapping streams: 300 objects whose /Length all end at one endstream would copy the same
    // 256 KiB 300 times. The bytes parsed over all objects are bounded by twice the input.
    let overlap = |k: usize, payload: usize| {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut off: BTreeMap<u32, usize> = BTreeMap::new();
        for (n, body) in minimal_objs("", "") {
            off.insert(n, out.len());
            out.extend(obj_bytes(n, &body));
        }
        let hdr = |n: usize, len: usize| format!("{n} 0 obj\n<< /Length {len:09} >>\nstream\n").into_bytes();
        let hdr_len = hdr(100, 0).len();
        let start = out.len();
        let end = start + k * hdr_len + payload;
        for i in 0..k {
            off.insert(100 + i as u32, out.len());
            out.extend(hdr(100 + i, end - (start + (i + 1) * hdr_len)));
        }
        out.extend(vec![b'x'; payload]);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        xref_tail(&mut out, &off, "");
        out
    };
    assert!(PdfDoc::open(&overlap(1, 1000), &limits, &token()).is_ok());
    assert_eq!(open_err(&overlap(300, 256 << 10), &limits), Some(PressError::InputLimit("stream_bytes")));

    // Object-stream members that all point at one big array are parsed once per member without a bound.
    let big_array = format!("[{}]", "0 ".repeat(5000));
    let header: String = (0..50).map(|i| format!("{} 0 ", 20 + i)).collect();
    let mut objs = minimal_objs("", "");
    objs.push((5, stream_body(&format!("/Type /ObjStm /N 50 /First {}", header.len()), format!("{header}{big_array}").as_bytes())));
    let members: Vec<(u32, u32, u8)> = (0..50).map(|i| (20 + i, 5, i as u8)).collect();
    assert_eq!(open_err(&xref_stream_pdf(&objs, 70, "", &members), &limits), Some(PressError::InputLimit("objstm_overlap")));
    assert!(PdfDoc::open(&xref_stream_pdf(&objs, 70, "", &members[..1]), &limits, &token()).is_ok());
}

// ------------------------------------------------- remediation R6 (review B, SMB-PRESS)

#[test]
fn stray_streams_and_hidden_payloads_are_not_kept_text() {
    let region = RedactionRegion { page_index: 0, rect: [60.0, 690.0, 200.0, 720.0] };
    let page = format!("{TEXT_IN_REGION}\nBT /F1 12 Tf 72 400 Td (Public) Tj ET");
    let run = |pdf: &[u8]| {
        let mut doc = open(pdf);
        redact_and_save(&mut doc, &[region], UnsupportedPolicy::Refuse, &token()).unwrap()
    };

    // The control: the page alone is proven.
    let control = run(&build(&base_objs(page.as_bytes(), "<< /Title (T) >>"), "/Info 7 0 R", b""));
    assert!(control.is_proven(), "{:?}", control.not_proven_reasons());

    // Class 1 (finding 1): a catalog-reachable stream that is neither a page content stream nor a
    // form drawn from page resources, Flate-encoded and holding the removed string as a Tj operand.
    // Its text is not kept page text: the needle is a leak, not a collision.
    let stray = |stream: Vec<u8>| {
        let mut objs = base_objs(page.as_bytes(), "<< /Title (T) >>");
        objs[0].1 = b"<< /Type /Catalog /Pages 2 0 R /Hidden 20 0 R >>".to_vec();
        objs.push((20, stream));
        build(&objs, "/Info 7 0 R", b"")
    };
    let body = miniz_oxide::deflate::compress_to_vec_zlib(b"BT /F1 12 Tf 72 700 Td (SECRET-ALPHA) Tj ET", 6);
    for dict in ["/Filter /FlateDecode", "/Filter /FlateDecode /Subtype /Form /BBox [0 0 9 9]", "/Filter /FlateDecode /Type /Pattern /PatternType 1"] {
        let out = run(&stray(stream_body(dict, &body)));
        assert!(
            out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedStream { object: 20, .. })),
            "{dict}: {:?}",
            out.proof.hits
        );
        assert!(!out.is_proven() && out.not_proven_reasons().contains(&NotProven::Leak), "{dict}");
        // R8 F1 (lead decision 11:58Z): stray content without resources cannot resolve /F1, so its
        // text is opaque and the region claim is withheld too.
        assert!(out.not_proven_reasons().contains(&NotProven::OpaqueText) && !out.region_cleared(), "{dict}: {:?}", out.not_proven_reasons());
    }
    // With its own resources the stray form's font resolves: a leak, and the in-region instance itself is gone.
    let out = run(&stray(stream_body("/Filter /FlateDecode /Subtype /Form /BBox [0 0 9 9] /Resources << /Font << /F1 6 0 R >> >>", &body)));
    assert_eq!(out.not_proven_reasons(), vec![NotProven::Leak]);
    assert!(out.region_cleared(), "the in-region instance itself is gone");

    // A form drawn from the page's resources stays kept text: the same word there is a collision.
    let mut objs = base_objs(format!("{page}\nq 1 0 0 1 0 0 cm /Fm1 Do Q").as_bytes(), "<< /Title (T) >>");
    objs[2].1 = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Annots [5 0 R] /Resources << /Font << /F1 6 0 R >> /XObject << /Fm1 20 0 R >> >> >>"
        .to_vec();
    objs.push((20, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 612 792]", b"BT /F1 12 Tf 300 300 Td (SECRET-ALPHA) Tj ET")));
    let out = run(&build(&objs, "/Info 7 0 R", b""));
    assert!(out.proof.hits.is_empty() && out.proof.kept_text_collisions.iter().any(|c| c.object == 20), "{:?}", out.proof);

    // Class 2 (finding 4): an image XObject whose codec input is a zlib stream of the removed string.
    let hidden = miniz_oxide::deflate::compress_to_vec_zlib(b"hidden SECRET-ALPHA payload", 6);
    for codec in ["JBIG2Decode", "CCITTFaxDecode", "DCTDecode", "JPXDecode"] {
        let out = run(&raster_doc(&format!("/Filter /{codec}"), &hidden, &page));
        assert!(
            out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedStream { object: 8, .. })),
            "{codec}: {:?}",
            out.proof.hits
        );
        // PLR-040: a zlib stream is not that codec's data, so the stream is not raster at all.
        assert!(!out.is_proven() && out.proof.raster_streams.is_empty(), "{codec}");
        assert!(out.proof.streams_not_decodable.iter().any(|(n, why)| *n == 8 && why.starts_with(codec)), "{codec}: {:?}", out.proof.streams_not_decodable);
    }
    // ... and a zlib bomb behind a raster filter name is refused like any bomb.
    let bomb = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 8 << 20], 6);
    let small = Limits { max_decoded_bytes: 1 << 20, ..Limits::default() };
    let pdf = raster_doc("/Filter /JBIG2Decode", &bomb, &page);
    assert_eq!(verify_payload_absent(&pdf, &[b"zzzzzz".to_vec()], &small, &token()).unwrap_err(), PressError::InputLimit("decoded_bytes"));

    // Class 3 (finding 5): a glyph box that cannot be computed (the CTM overflowed to infinity and
    // NaN) reads as touching the region, so the string is removed instead of silently kept.
    let scale = format!("1{} 0 0 1{} 0 0 cm\n", "0".repeat(38), "0".repeat(38));
    let nan_page = format!("{}{TEXT_IN_REGION}", scale.repeat(9));
    let out = run(&build(&base_objs(nan_page.as_bytes(), "<< /Title (T) >>"), "/Info 7 0 R", b""));
    assert!(independent_streams(&out.bytes).iter().all(|s| find(s, b"SECRET-ALPHA").is_none()), "the unlocatable string was kept");
    assert!(out.reports[0].removed_text.iter().any(|t| find(t, b"SECRET-ALPHA").is_some()), "{:?}", out.reports[0]);

    // Class 4 (cancellation windows): a needle across the 1 MiB search-window edge is found once.
    let objs = base_objs(page.as_bytes(), "<< /Title (T) >>");
    let at = find(&build(&objs, "/Info 7 0 R", b""), b"xref\n0 ").unwrap();
    let needle = b"straddle-needle-77";
    let edge = 2usize << 20;
    let mut junk = format!("%{}", " ".repeat(edge - 9 - at - 1)).into_bytes();
    junk.extend_from_slice(needle);
    junk.push(b'\n');
    let pdf = build(&objs, "/Info 7 0 R", &junk);
    let proof = verify_payload_absent(&pdf, &[needle.to_vec()], &Limits::default(), &token()).unwrap();
    let raw: Vec<usize> = proof.hits.iter().filter_map(|h| if let PayloadLocation::RawBytes { offset } = h.location { Some(offset) } else { None }).collect();
    assert_eq!(raw, vec![edge - 9], "{:?}", proof.hits);
    assert_eq!(find(&pdf, needle), Some(edge - 9));
}

// ------------------------------------------------- redaction gaps (slice 6)

#[test]
fn name_trees_submit_fields_and_embedded_content_redacted() {
    let catalog = "/Names << /JavaScript 30 0 R /Templates << /Names [(Budget Plan template) 33 0 R (other template) 34 0 R] >> \
                   /AP << /Kids [32 0 R] >> /EmbeddedFiles 40 0 R >> /OpenAction 35 0 R";
    let build_doc = |attachment: Vec<u8>| {
        let mut objs = minimal_objs(catalog, "20 0 R");
        objs.extend([
            (20, b"<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] /A << /S /ResetForm /Fields [(budget plan x)] >> >>".to_vec()),
            (30, b"<< /Names [(Budget Plan script) 31 0 R (keep script) 36 0 R] >>".to_vec()),
            (31, b"<< /S /JavaScript /JS (var a = 1;) >>".to_vec()),
            (32, b"<< /Limits [(a) (z)] /Names [(Budget Plan ap) 37 0 R] >>".to_vec()),
            (33, b"<< /Note (t) >>".to_vec()),
            (34, b"<< /Note (u) >>".to_vec()),
            (35, b"<< /S /SubmitForm /F (http://example.org/in) /Fields [(Budget Plan total) (other) 38 0 R] /Flags 4 >>".to_vec()),
            (36, b"<< /S /JavaScript /JS (var b = 2;) >>".to_vec()),
            (37, b"<< /Note (v) >>".to_vec()),
            (38, b"<< /FT /Tx /T (kept) >>".to_vec()),
            (40, b"<< /Names [(notes.txt) 41 0 R (clean.txt) 44 0 R] >>".to_vec()),
            (41, b"<< /Type /Filespec /F (notes.txt) /EF << /F 42 0 R >> >>".to_vec()),
            (
                42,
                stream_body(
                    "/Type /EmbeddedFile /Filter /FlateDecode",
                    &miniz_oxide::deflate::compress_to_vec_zlib(b"Confidential: the BUDGET PLAN for 2027", 6),
                ),
            ),
            (44, b"<< /Type /Filespec /F (clean.txt) /EF << /F 45 0 R >> >>".to_vec()),
            (45, attachment),
        ]);
        build(&objs, "", b"")
    };
    let needle = ["Budget Plan".to_string()];
    use TextOutcomeKind::{Dropped, KeptByPolicy, Replaced, Unverifiable};
    let plain = build_doc(stream_body("/Type /EmbeddedFile", b"nothing here"));

    let mut doc = open(&plain);
    let out = redact_text_fields_and_save(&mut doc, &needle, &TextFieldPolicy::default(), &token()).unwrap();
    assert_eq!(rows(&out.report, TextRole::NameTreeKey), vec![(1, false, Replaced, 1), (1, false, Replaced, 1), (30, false, Replaced, 1)]);
    assert_eq!(rows(&out.report, TextRole::ActionFields), vec![(20, true, Replaced, 1), (35, false, Replaced, 1)]);
    assert_eq!(rows(&out.report, TextRole::EmbeddedFileContent), vec![(41, false, Dropped, 1)]);
    assert!(out.report.complete && out.is_proven(), "{:?} {:?}", out.report, out.proof);
    assert!(find(&out.bytes.to_ascii_lowercase(), b"budget plan").is_none());
    assert!(independent_streams(&out.bytes).iter().all(|s| find(&s.to_ascii_lowercase(), b"budget plan").is_none()));
    // The clean attachment, the unrelated tree keys and the reference inside /Fields stay; the
    // matching attachment left the file; names read as opaque keys and marked strings.
    assert!(find(&out.bytes, b"nothing here").is_some() && find(&out.bytes, b"Confidential").is_none());
    let strings = all_strings(&out.bytes);
    for expect in ["keep script", "other template", "[redacted] total", "other", "[redacted] x", "clean.txt"] {
        assert!(strings.iter().any(|s| s == expect), "missing {expect:?} in {strings:?}");
    }
    assert!(strings.iter().filter(|s| s.starts_with("hskrd")).count() >= 3, "{strings:?}");
    let again = open(&out.bytes);
    assert_eq!(again.inspect().page_count, 1);

    // Every class has its policy flag; a disabled class is reported kept and the text stays.
    let off = TextFieldPolicy { name_trees: false, embedded_content: false, form_labels: false, ..TextFieldPolicy::default() };
    let mut doc = open(&plain);
    let kept = redact_text_fields_and_save(&mut doc, &needle, &off, &token()).unwrap();
    assert_eq!((kept.report.kept_by_policy, kept.report.complete), (6, false));
    assert!(rows(&kept.report, TextRole::NameTreeKey).iter().all(|r| r.2 == KeptByPolicy));
    assert_eq!(rows(&kept.report, TextRole::EmbeddedFileContent), vec![(41, false, KeptByPolicy, 1)]);
    assert!(!kept.is_proven() && leaks(&kept.bytes, "budget plan"));

    // An attachment that cannot be decoded cannot be judged: reported Unverifiable and kept.
    let undecodable = build_doc(stream_body("/Type /EmbeddedFile /Filter /JBIG2Decode", b"xx"));
    let mut doc = open(&undecodable);
    let unsure = redact_text_fields_and_save(&mut doc, &needle, &TextFieldPolicy::default(), &token()).unwrap();
    assert_eq!(rows(&unsure.report, TextRole::EmbeddedFileContent), vec![(41, false, Dropped, 1), (44, false, Unverifiable, 0)]);
    assert_eq!((unsure.report.unverifiable, unsure.report.complete), (1, false));
    assert!(!unsure.is_proven());
}

// ------------------------------------------------- remediation R7 pre-review B (SMB-PRESS, N1-N7)
// Lines marked R7-new-api use API added by this remediation; the red run on 0e48167 drops only those
// lines, and every test there fails on an API-stable assertion before them.

const R7_REGION: RedactionRegion = RedactionRegion { page_index: 0, rect: [60.0, 690.0, 200.0, 720.0] };

fn r7_run(pdf: &[u8]) -> hsk_studio_press::RedactedOutput {
    let mut doc = open(pdf);
    redact_and_save(&mut doc, &[R7_REGION], UnsupportedPolicy::Refuse, &token()).unwrap()
}

fn r7_page() -> String {
    format!("{TEXT_IN_REGION}\nBT /F1 12 Tf 72 400 Td (Public) Tj ET")
}

/// base_objs page plus a catalog-reachable object 20 (not drawn by anything).
fn r7_with_hidden(content: &str, hidden: Vec<u8>) -> Vec<u8> {
    let mut objs = base_objs(content.as_bytes(), "<< /Title (T) >>");
    objs[0].1 = b"<< /Type /Catalog /Pages 2 0 R /Hidden 20 0 R >>".to_vec();
    objs.push((20, hidden));
    build(&objs, "/Info 7 0 R", b"")
}

fn zl(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

/// ASCII85 encoding (ISO 32000-2 7.4.3), no `z` shortcut.
fn a85(data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    for c in data.chunks(4) {
        let mut g = [0u8; 4];
        g[..c.len()].copy_from_slice(c);
        let mut n = u64::from(u32::from_be_bytes(g));
        let mut d = [0u8; 5];
        for k in (0..5).rev() {
            d[k] = b'!' + (n % 85) as u8;
            n /= 85;
        }
        v.extend_from_slice(&d[..c.len() + 1]);
    }
    v.extend_from_slice(b"~>");
    v
}

fn raw_hit(p: &hsk_studio_press::PayloadProof) -> bool {
    p.hits.iter().any(|h| matches!(h.location, PayloadLocation::RawBytes { .. }))
}

/// N1: bytes after a filter's end-of-data marker and comments in a stream dictionary are file bytes
/// no decoder reads. At 0e48167 a decodable stream exempted every raw match in its object span.
#[test]
fn bytes_after_end_of_data_and_dictionary_comments_are_hits() {
    let page = r7_page();
    // The removed string in plain bytes right after the end-of-data marker of each filter. Where
    // the byte before it is not a letter or digit the raw scan reports it too; after the Adler-32
    // of a zlib stream it may be glued to a letter (whole-token rule), and TrailingData alone blocks.
    let plain_after: Vec<(&str, Vec<u8>, bool)> = vec![
        ("/Filter /ASCIIHexDecode", b"41>SECRET-ALPHA".to_vec(), true),
        ("/Filter /FlateDecode", [zl(b"x"), b"SECRET-ALPHA".to_vec()].concat(), false),
        ("/Filter /ASCII85Decode", b"87cURDZ~>SECRET-ALPHA".to_vec(), true),
        ("/Filter /RunLengthDecode", [vec![0u8, b'x', 128], b"SECRET-ALPHA".to_vec()].concat(), true),
    ];
    for (dict, data, raw) in plain_after {
        let out = r7_run(&r7_with_hidden(&page, stream_body(dict, &data)));
        assert!(!out.is_proven(), "{dict}: the removed string is in the saved file: {:?}", out.proof);
        assert!(find(&out.bytes, b"SECRET-ALPHA").is_some() && (!raw || raw_hit(&out.proof)), "{dict}: {:?}", out.proof);
        assert!(out.proof.trailing_data_streams.iter().any(|(n, _)| *n == 20), "{dict}"); // R7-new-api
        assert!(out.not_proven_reasons().contains(&NotProven::TrailingData), "{dict}"); // R7-new-api
    }
    // An encoded copy after the end of the deflate stream: no plain byte anywhere, never decoded.
    let out = r7_run(&r7_with_hidden(&page, stream_body("/Filter /FlateDecode", &[zl(b"x"), zl(b"SECRET-ALPHA")].concat())));
    assert!(!out.is_proven(), "a second zlib stream after the end of the first is never read");
    assert_eq!(out.not_proven_reasons(), vec![NotProven::TrailingData]); // R7-new-api
    assert_eq!(out.proof.trailing_data_streams, vec![(20, "FlateDecode".to_string())]); // R7-new-api
    // A comment inside the stream dictionary: the stream decodes cleanly, the comment is file text.
    let out = r7_run(&r7_with_hidden(&page, b"<< /Length 5 % SECRET-ALPHA\n>>\nstream\nhello\nendstream".to_vec()));
    assert!(!out.is_proven() && raw_hit(&out.proof), "{:?}", out.proof.hits);
    // A dictionary KEY spelled with # escapes is the same name: compared by decoded value.
    let out = r7_run(&r7_with_hidden(&page, b"<< /SECRET#2DALPHA 1 >>".to_vec()));
    assert!(!out.is_proven(), "a dictionary key holds the removed string");
    assert!(out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::StringObject { object: 20 })));
    // Other spellings in a stream nobody draws: UTF-16LE and hex.
    for spelled in [b"S\0E\0C\0R\0E\0T\0-\0A\0L\0P\0H\0A\0".to_vec(), b"5345435245542D414C504841".to_vec()] {
        let out = r7_run(&r7_with_hidden(&page, stream_body("", &spelled)));
        assert!(!out.is_proven(), "{:?}", String::from_utf8_lossy(&spelled));
    }
    // Control: white space after the marker is not data.
    let out = r7_run(&r7_with_hidden(&page, stream_body("/Filter /ASCIIHexDecode", b"41>\r\n \0")));
    assert!(out.is_proven(), "{:?}", out.not_proven_reasons());
}

/// N2: inline-image data is decoded through its implemented filters (and a zlib codec input behind
/// a raster codec name) and scanned. At 0e48167 it was scanned only in its encoded form.
#[test]
fn inline_image_data_is_decoded_and_scanned() {
    let page = r7_page();
    let pdf = |filter: &str, data: &[u8]| {
        let mut c = format!("{page}\nq 10 0 0 10 400 100 cm\nBI /W 4 /H 3 /CS /G /BPC 8 /F {filter} ID\n").into_bytes();
        c.extend_from_slice(data);
        c.extend_from_slice(b"\nEI\nQ");
        build(&base_objs(&c, "<< /Title (T) >>"), "/Info 7 0 R", b"")
    };
    let pixels = b"SECRET-ALPHA pixels";
    for (filter, data) in [("/Fl", zl(pixels)), ("[/Fl /DCT]", zl(&zl(pixels))), ("/AHx", hex_stream(pixels)), ("[/A85 /Fl]", a85(&zl(pixels)))] {
        // The proof over a file with the inline image decodes and scans its data.
        let source = pdf(filter, &data);
        let p = r7_verify(&source);
        assert!(p.hits.iter().any(|h| matches!(h.location, PayloadLocation::InlineImage { .. })), "{filter}: {:?}", p.hits); // R7-new-api
        // A redaction's output holds the image as an image XObject (canonical rewrite, R10-F1): its
        // decoded data is scanned like any stream.
        let out = r7_run(&source);
        assert!(!out.is_proven(), "{filter}: the removed string is in the decoded image data: {:?}", out.not_proven_reasons());
        assert!(out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedStream { .. })), "{filter}: {:?}", out.proof.hits);
    }
    // Control: pixels without the string are proven, the raster tail is listed (as an XObject now).
    let out = r7_run(&pdf("[/Fl /DCT]", &zl(&jpeg(b"plain"))));
    assert!(out.is_proven() && out.proof.raster_streams.iter().any(|r| !r.inline && r.filter == "DCTDecode"), "{:?}", out.not_proven_reasons());
}

/// Type0 test fonts: 23 has no /ToUnicode (unreadable), 24 maps codes 0101..010C to SECRET-ALPHA.
fn r7_fonts() -> Vec<(u32, Vec<u8>)> {
    let mut cmap = String::from("/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n1 begincodespacerange <0000> <FFFF> endcodespacerange\n12 beginbfchar\n");
    for (i, ch) in "SECRET-ALPHA".chars().enumerate() {
        cmap.push_str(&format!("<{:04X}> <{:04X}>\n", 0x0101 + i as u32, ch as u32));
    }
    cmap.push_str("endbfchar\nendcmap\nend end");
    vec![
        (21, b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /T >>".to_vec()),
        (23, b"<< /Type /Font /Subtype /Type0 /BaseFont /T /Encoding /Identity-H /DescendantFonts [21 0 R] >>".to_vec()),
        (24, b"<< /Type /Font /Subtype /Type0 /BaseFont /T /Encoding /Identity-H /DescendantFonts [21 0 R] /ToUnicode 25 0 R >>".to_vec()),
        (25, stream_body("", cmap.as_bytes())),
    ]
}

/// Codes of SECRET-ALPHA under font 24 (glyph ids under font 23).
const R7_CODES: &str = "<010101020103010401050106010701080109010A010B010C>";
const R7_RES: &str = "/Font << /F1 24 0 R /F2 6 0 R /F3 23 0 R >> /ExtGState << /GS1 << /Font [24 0 R 12] >> >> /XObject << /Im1 8 0 R >>";

/// A page with the test fonts: `content` (object 4, or a /Contents array with `more` as object 27),
/// an annotation appearance (object 26, outside any region) drawing `ap`, page XObjects `xobjects`,
/// and `extra` objects.
fn r7_font_doc(content: &str, more: Option<&str>, ap: Option<&str>, xobjects: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = base_objs(content.as_bytes(), "<< /Title (T) >>");
    let contents = if more.is_some() { "[4 0 R 27 0 R]" } else { "4 0 R" };
    let res = R7_RES.replace("/Im1 8 0 R", &format!("/Im1 8 0 R {xobjects}"));
    objs[2].1 = format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {contents} /Annots [5 0 R] /Resources << {res} >> >>").into_bytes();
    if let Some(m) = more {
        objs.push((27, stream_body("", m.as_bytes())));
    }
    if let Some(ap) = ap {
        objs[4].1 = b"<< /Type /Annot /Subtype /FreeText /Rect [300 100 400 140] /Contents (label) /AP << /N 26 0 R >> /P 3 0 R >>".to_vec();
        objs.push((26, stream_body(&format!("/Type /XObject /Subtype /Form /BBox [0 0 100 40] /Resources << {R7_RES} >>"), ap.as_bytes())));
    }
    objs.extend(r7_fonts());
    objs.extend(extra);
    build(&objs, "/Info 7 0 R", b"")
}

fn r7_verify(pdf: &[u8]) -> hsk_studio_press::PayloadProof {
    verify_payload_absent(pdf, &[b"SECRET-ALPHA".to_vec()], &Limits::default(), &token()).unwrap()
}

const R7_PLAIN: &str = "BT /F2 12 Tf 72 700 Td (Public) Tj ET";

/// N4, N6 and the same class at other sites: the font is graphics state (q/Q, gs /Font, Tf), it
/// carries across a page's content streams and into forms drawn by Do, and a form without its own
/// resources uses the caller's. At 0e48167 the proof read each stream on its own with the last Tf.
#[test]
fn font_state_follows_q_q_gs_and_do() {
    // Control: fonts read correctly, nothing to find.
    let p = r7_verify(&r7_font_doc(R7_PLAIN, None, Some("BT /F2 12 Tf q /F1 9 Tf Q 2 5 Td (plain) Tj ET"), "", vec![]));
    assert!(p.is_clean(), "{:?}", p.not_proven_reasons());
    // N4: Q restores F1 (its codes spell the removed string through its ToUnicode); F2 only inside q..Q.
    let p = r7_verify(&r7_font_doc(R7_PLAIN, None, Some(&format!("BT /F1 12 Tf q /F2 9 Tf Q 2 5 Td {R7_CODES} Tj ET")), "", vec![]));
    assert!(!p.is_clean(), "the appearance draws SECRET-ALPHA with F1 after Q");
    assert!(p.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedText { object: 26 })), "{:?}", p.hits);
    // N4: the font set by an ExtGState through gs.
    let p = r7_verify(&r7_font_doc(R7_PLAIN, None, Some(&format!("BT /GS1 gs 2 5 Td {R7_CODES} Tj ET")), "", vec![]));
    assert!(!p.is_clean() && p.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedText { object: 26 })), "{:?}", p.hits);
    // N4: the same q/Q trick with a font that has no ToUnicode hides unreadable text.
    let p = r7_verify(&r7_font_doc(R7_PLAIN, None, Some(&format!("BT /F3 12 Tf q /F2 9 Tf Q 2 5 Td {R7_CODES} Tj ET")), "", vec![]));
    assert!(!p.is_clean(), "unreadable text drawn after Q");
    assert!(p.not_proven_reasons().contains(&NotProven::OpaqueText) && p.opaque_text_streams == vec![26], "{:?}", p.not_proven_reasons());
    // N6: a form without its own /Resources drawn by the page uses the page resources (F3 unreadable).
    let legacy = |body: &str, own: &str| stream_body(&format!("/Type /XObject /Subtype /Form /BBox [0 0 612 792] {own}"), body.as_bytes());
    let p = r7_verify(&r7_font_doc(&format!("{R7_PLAIN}\nq /Fm1 Do Q"), None, None, "/Fm1 28 0 R", vec![(28, legacy(&format!("BT /F3 12 Tf 72 300 Td {R7_CODES} Tj ET"), ""))]));
    assert!(!p.is_clean(), "the legacy form draws unreadable text with the inherited F3");
    assert!(p.not_proven_reasons().contains(&NotProven::OpaqueText) && p.opaque_text_streams == vec![28], "{:?}", p.not_proven_reasons());
    // A form inherits the caller's FONT too: no Tf inside the form, F3 set by the page.
    let own = "/Resources << /Font << /F2 6 0 R >> >>";
    let p = r7_verify(&r7_font_doc(&format!("{R7_PLAIN}\nBT /F3 12 Tf ET q /Fm1 Do Q"), None, None, "/Fm1 28 0 R", vec![(28, legacy(&format!("BT 72 300 Td {R7_CODES} Tj ET"), own))]));
    assert!(!p.is_clean(), "the form draws with the caller's F3");
    assert_eq!(p.opaque_text_streams, vec![28]);
    // A page's content streams are one stream: Tf in the first applies to the second.
    let p = r7_verify(&r7_font_doc("BT /F3 12 Tf 72 300 Td", Some(&format!("{R7_CODES} Tj ET")), None, "", vec![]));
    assert!(!p.is_clean(), "the second content stream draws with F3 from the first");
    assert_eq!(p.opaque_text_streams, vec![27]);
    // Page content naming a font its resources do not hold cannot be checked.
    let p = r7_verify(&r7_font_doc(&format!("BT /F9 12 Tf 72 300 Td {R7_CODES} Tj ET"), None, None, "", vec![]));
    assert!(!p.is_clean(), "unresolved font in page content");
    assert!(p.not_proven_reasons().contains(&NotProven::OpaqueText) && p.opaque_text_streams == vec![4], "{:?}", p.not_proven_reasons());
}

/// N3 and N5: content no page draws is still read with its own resources, and content that cannot
/// be followed never reads clean. At 0e48167 stray streams were not parsed (c520095) and a parse
/// failure skipped every text check.
#[test]
fn unreadable_or_stray_content_never_reads_clean() {
    let page = r7_page();
    let form = "/Type /XObject /Subtype /Form /BBox [0 0 9 9] /Resources << /Font << /F1 24 0 R /F3 23 0 R >> >>";
    let stray = |dict: &str, body: &str| {
        let mut objs = base_objs(page.as_bytes(), "<< /Title (T) >>");
        objs[0].1 = b"<< /Type /Catalog /Pages 2 0 R /Hidden 20 0 R >>".to_vec();
        objs.push((20, stream_body(dict, body.as_bytes())));
        objs.extend(r7_fonts());
        build(&objs, "/Info 7 0 R", b"")
    };
    // Control: a stray form with readable text and no removed string is proven.
    let out = r7_run(&stray(form, "BT /F1 12 Tf <0101> Tj ET"));
    assert!(out.is_proven(), "{:?}", out.not_proven_reasons());
    // N3: a stray form with its own resources draws unreadable text.
    let out = r7_run(&stray(form, &format!("BT /F3 12 Tf {R7_CODES} Tj ET")));
    assert!(!out.is_proven(), "stray unreadable text");
    assert!(out.not_proven_reasons().contains(&NotProven::OpaqueText) && out.proof.opaque_text_streams == vec![20], "{:?}", out.not_proven_reasons());
    // N3: the same stray form drawing the removed string through a ToUnicode map is a leak.
    let out = r7_run(&stray(form, &format!("BT /F1 12 Tf {R7_CODES} Tj ET")));
    assert!(!out.is_proven() && out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedText { object: 20 })), "{:?}", out.proof.hits);
    // N3: an inline image with a Crypt filter in a stray stream blocks.
    let out = r7_run(&stray("", "q BI /W 1 /H 1 /CS /G /BPC 8 /F /Crypt ID\nx\nEI Q"));
    assert!(!out.is_proven(), "inline Crypt image in a stray stream");
    assert!(out.proof.streams_not_decodable.contains(&(20, "inline:Crypt".to_string())), "{:?}", out.proof.streams_not_decodable);
    // N5: an annotation appearance that does not tokenize (a stray '}') draws the removed string
    // through F1 before the bad token.
    let p = r7_verify(&r7_font_doc(R7_PLAIN, None, Some(&format!("BT /F1 12 Tf 2 5 Td {R7_CODES} Tj ET }}")), "", vec![]));
    assert!(!p.is_clean(), "unparseable appearance");
    assert!(p.unreadable_content_streams.iter().any(|(n, _)| *n == 26), "{:?}", p.unreadable_content_streams); // R7-new-api
    // Operands in one content stream of a page and their operator in the next: one logical stream
    // for a viewer, unreadable for a per-stream reading.
    let p = r7_verify(&r7_font_doc(&format!("BT /F3 12 Tf 72 300 Td {R7_CODES}"), Some("Tj ET"), None, "", vec![]));
    assert!(!p.is_clean(), "operands split from their operator across content streams");
    assert!(p.not_proven_reasons().contains(&NotProven::UnreadableContent), "{:?}", p.not_proven_reasons()); // R7-new-api
}

/// N7: only forms a page actually draws (Do) are kept text. At 0e48167 a form merely listed in the
/// page resources counted as kept, so the removed string inside it read as a collision.
#[test]
fn listed_but_undrawn_forms_are_not_kept_text() {
    let page = r7_page();
    let doc = |content: &str, xobjects: &str, extra: Vec<(u32, Vec<u8>)>| {
        let mut objs = base_objs(content.as_bytes(), "<< /Title (T) >>");
        objs[2].1 = format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Annots [5 0 R] /Resources << /Font << /F1 6 0 R >> /XObject << /Im1 8 0 R {xobjects} >> >> >>").into_bytes();
        objs.extend(extra);
        build(&objs, "/Info 7 0 R", b"")
    };
    let form = |body: &str, res: &str| stream_body(&format!("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> {res} >>"), body.as_bytes());
    let secret = "BT /F1 12 Tf 300 300 Td (SECRET-ALPHA) Tj ET";
    // Listed under the page resources, never drawn.
    let out = r7_run(&doc(&page, "/Fm9 20 0 R", vec![(20, form(secret, ""))]));
    assert!(!out.is_proven(), "an undrawn form is payload, not kept text");
    assert!(out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedStream { object: 20, .. })), "{:?}", out.proof);
    assert!(out.proof.kept_text_collisions.iter().all(|c| c.object != 20));
    // Listed by a drawn form, never drawn by it.
    let out = r7_run(&doc(&format!("{page}\nq /Fm1 Do Q"), "/Fm1 21 0 R", vec![(21, form("BT /F1 9 Tf (shell) Tj ET", "/XObject << /Fm9 20 0 R >>")), (20, form(secret, ""))]));
    assert!(!out.is_proven() && out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedStream { object: 20, .. })), "{:?}", out.proof);
    // Control: drawn by the page, the same form is kept text (a collision, not a leak).
    let out = r7_run(&doc(&format!("{page}\nq /Fm9 Do Q"), "/Fm9 20 0 R", vec![(20, form(secret, ""))]));
    assert!(out.is_proven() && out.proof.kept_text_collisions.iter().any(|c| c.object == 20), "{:?}", out.proof);
}

/// Smallest data with each raster codec's structure: (codec, data, extra dictionary entries).
fn r7_codec_samples() -> Vec<(&'static str, Vec<u8>, &'static str)> {
    vec![
        ("DCTDecode", jpeg(b"c"), ""),
        ("JPXDecode", vec![0xFF, 0x4F, 0xFF, 0x51, 0, 0, 0xFF, 0xD9], ""),
        // T.6: one white row (V0), EOFB.
        ("CCITTFaxDecode", vec![0x80, 0x08, 0x00, 0x80], "/DecodeParms << /K -1 /Columns 1 /Rows 1 >>"),
        // T.4 one-dimensional: white run 1.
        ("CCITTFaxDecode", vec![0x1C], "/DecodeParms << /K 0 /Columns 1 /Rows 1 >>"),
        // One end-of-page segment (type 49), page 1, no data.
        ("JBIG2Decode", vec![0, 0, 0, 1, 0x31, 0, 1, 0, 0, 0, 0], ""),
    ]
}

/// PLR-040 (lead decision, fail closed): a raster codec NAME does not make a stream pixels. Data
/// without the codec's structure (raw deflate, zlib after a junk byte, LZW, ASCII85, a JPEG with
/// bytes after EOI, a JBIG2 file header, CCITT rows that do not decode to /Width) is undecodable
/// and blocks. At 0e48167 every such stream was listed as raster and the proof read clean.
#[test]
fn raster_names_need_codec_structure() {
    let page = r7_page();
    let text = b"hidden SECRET-ALPHA payload";
    let payloads: Vec<(&str, Vec<u8>)> = vec![
        ("raw deflate", miniz_oxide::deflate::compress_to_vec(text, 6)),
        ("junk + zlib", [b"J".to_vec(), zl(text)].concat()),
        ("lzw", lzw_encode(text)),
        ("ascii85", a85(text)),
    ];
    let ccitt = "/DecodeParms << /K -1 /Columns 1 /Rows 1 >>";
    for codec in ["DCTDecode", "JPXDecode", "CCITTFaxDecode", "JBIG2Decode"] {
        for (what, data) in &payloads {
            let parms = if codec == "CCITTFaxDecode" { ccitt } else { "" };
            let p = r7_verify(&raster_doc(&format!("/Filter /{codec} {parms}"), data, &page));
            assert!(!p.is_clean() && p.raster_streams.is_empty(), "{codec} / {what}: {p:?}");
            assert!(p.streams_not_decodable.iter().any(|(n, why)| *n == 8 && why.starts_with(codec)), "{codec} / {what}: {:?}", p.streams_not_decodable);
        }
    }
    // Structure violations of otherwise plausible data.
    let mut after_eoi = jpeg(b"c");
    after_eoi.extend_from_slice(b"SECRET");
    let mut jbig2_file = vec![0x97, 0x4A, 0x42, 0x32, 0x0D, 0x0A, 0x1A, 0x0A, 0x01, 0, 0, 0, 1];
    jbig2_file.extend_from_slice(&[0, 0, 0, 1, 0x31, 0, 1, 0, 0, 0, 0]);
    for (codec, data, parms) in [
        ("DCTDecode", after_eoi, ""),
        ("JBIG2Decode", jbig2_file, ""),
        ("JPXDecode", vec![0xFF, 0x4F, 0xFF, 0x51, 0, 0], ""),
        ("CCITTFaxDecode", vec![0x80, 0x08, 0x00, 0x80], "/DecodeParms << /K -1 /Columns 2 /Rows 1 >>"),
        ("CCITTFaxDecode", vec![0x80, 0x08, 0x00, 0x80], "/DecodeParms << /K 1 /Columns 1 /Rows 1 >>"),
        ("CCITTFaxDecode", vec![0x80, 0x08, 0x00, 0x80, b'X'], "/DecodeParms << /K -1 /Columns 1 /Rows 1 >>"),
    ] {
        let p = r7_verify(&raster_doc(&format!("/Filter /{codec} {parms}"), &data, &page));
        assert!(!p.is_clean() && p.raster_streams.is_empty(), "{codec} {parms}: {p:?}");
    }
    // Control: the smallest well-formed data of each codec is raster and does not block.
    for (codec, data, parms) in r7_codec_samples() {
        let p = r7_verify(&raster_doc(&format!("/Filter /{codec} {parms}"), &data, &page));
        assert!(p.is_clean() && p.raster_streams.len() == 1, "{codec} {parms}: {p:?}");
    }
}

// ------------------------------------- SMB-PRESS-DIFFERENCES (simple-font encodings, /Differences)
// A simple font draws the glyph its /Encoding names for each byte code. At 1c3fd8c the proof and the
// redaction read simple-font codes as bytes (or through /ToUnicode only), so text drawn through
// /Differences glyph names read as proven. Fixed here: the encoding is decoded (base encoding,
// /Differences, Adobe Glyph List names) and a code nothing reads makes the text opaque.

/// Codes of SECRET-ALPHA under `DIFF_NAMES` from code 1 (S E C R E T - A L P H A).
const DIFF_CODES: &str = "<010203040205060708090A07>";
const DIFF_NAMES: &str = "/S /E /C /R /T /hyphen /A /L /P /H";

fn diff_font(differences: &str, extra: &str) -> Vec<u8> {
    format!("<< /Type /Font /Subtype /Type1 /BaseFont /Test /FirstChar 1 /LastChar 10 /Widths [600 600 600 600 600 600 600 600 600 600] /Encoding << /Type /Encoding /Differences [{differences}] >> {extra} >>").into_bytes()
}

/// A page with a FreeText annotation (outside any region) whose appearance 13 draws `DIFF_CODES`
/// through font 20 with the given /Differences array.
fn diff_ap_doc(differences: &str) -> Vec<u8> {
    let mut objs = minimal_objs("", "5 0 R");
    objs.extend([
        (5, b"<< /Type /Annot /Subtype /FreeText /Rect [100 100 300 140] /Contents (label) /AP << /N 13 0 R >> >>".to_vec()),
        (
            13,
            stream_body("/Type /XObject /Subtype /Form /BBox [0 0 200 40] /Resources << /Font << /F1 20 0 R >> >>", format!("BT /F1 12 Tf 2 5 Td {DIFF_CODES} Tj ET").as_bytes()),
        ),
        (20, diff_font(differences, "")),
    ]);
    build(&objs, "", b"")
}

fn verify_secret(pdf: &[u8]) -> hsk_studio_press::PayloadProof {
    verify_payload_absent(pdf, &[b"SECRET-ALPHA".to_vec()], &Limits::default(), &token()).unwrap()
}

/// Text drawn with /Differences glyph names is found by the proof and the redaction, in an annotation
/// appearance, in region-removed page text (its outline twin is redacted), in kept page text (a
/// collision) and in content no page draws. At 1c3fd8c every one of these read as proven.
#[test]
fn differences_encoded_text_is_found_proven_and_redacted() {
    let codes: Vec<u8> = vec![1, 2, 3, 4, 2, 5, 6, 7, 8, 9, 10, 7];
    let differences = format!("1 {DIFF_NAMES}");

    // (a) An appearance drawn through /Differences. Independent oracle: the file holds no plain
    // spelling of the text, only codes.
    let pdf = diff_ap_doc(&differences);
    assert!(find(&pdf, b"SECRET").is_none() && find(&pdf, b"ALPHA").is_none() && !leaks(&pdf, "secret-alpha"));
    let before = verify_secret(&pdf);
    assert!(before.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedText { object: 13 })), "{:?}", before.hits);
    assert!(!before.is_clean());
    // A different needle is not found by the same reading (the check is not vacuous).
    let other = verify_payload_absent(&pdf, &[b"OTHER-WORD".to_vec()], &Limits::default(), &token()).unwrap();
    assert!(other.is_clean(), "{:?}", other.not_proven_reasons());
    let mut doc = open(&pdf);
    let out = redact_text_fields_and_save(&mut doc, &["SECRET-ALPHA".to_string()], &TextFieldPolicy::default(), &token()).unwrap();
    assert_eq!(rows(&out.report, TextRole::Appearance), vec![(5, false, TextOutcomeKind::Dropped, 1)]);
    assert!(out.is_proven(), "{:?}", out.not_proven_reasons());
    assert!(find(&out.bytes, b"/N 13 0 R").is_none() && find(&out.bytes, b"010203040205060708090A07").is_none());
    assert!(independent_streams(&out.bytes).iter().all(|s| find(s, &codes).is_none() && find(s, b"010203040205060708090A07").is_none()));

    // (b) Region redaction of page text drawn through /Differences, with the same words in an outline
    // title and a second, kept instance of the codes outside the region.
    let region = RedactionRegion { page_index: 0, rect: [60.0, 690.0, 200.0, 720.0] };
    let objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>".to_vec()),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 20 0 R >> >> >>".to_vec()),
        (4, stream_body("", format!("BT /F1 12 Tf 72 700 Td {DIFF_CODES} Tj ET\nBT /F1 12 Tf 72 400 Td {DIFF_CODES} Tj ET").as_bytes())),
        (10, b"<< /Type /Outlines /First 11 0 R /Last 11 0 R /Count 1 >>".to_vec()),
        (11, b"<< /Title (SECRET-ALPHA plan) /Parent 10 0 R /Dest [3 0 R /Fit] >>".to_vec()),
        (20, diff_font(&differences, "")),
    ];
    let pdf = build(&objs, "", b"");
    let mut doc = open(&pdf);
    let plain = redact_and_save(&mut doc, &[region], UnsupportedPolicy::Refuse, &token()).unwrap();
    assert_eq!(plain.reports[0].removed_text, vec![codes.clone()]);
    assert_eq!(plain.reports[0].removed_unicode, vec![Some("SECRET-ALPHA".to_string())], "the redaction reads the codes through the encoding");
    // The outline title still names the text: a leak, found because the proof knows the text.
    assert!(!plain.is_proven(), "{:?}", plain.proof.hits);
    assert!(plain.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::StringObject { object: 11 })), "{:?}", plain.proof.hits);
    // The kept second instance, drawn through the same codes, is kept page text: a collision.
    assert_eq!(plain.proof.kept_text_collisions.len(), 1, "{:?}", plain.proof);
    let mut doc = open(&pdf);
    let fixed = redact_and_save_with(&mut doc, &[region], UnsupportedPolicy::Refuse, Some(&TextFieldPolicy::default()), &token()).unwrap();
    assert!(fixed.is_proven(), "{:?}", fixed.not_proven_reasons());
    assert_eq!(rows(fixed.text_fields.as_ref().unwrap(), TextRole::OutlineTitle), vec![(11, false, TextOutcomeKind::Replaced, 1)]);
    assert!(!leaks(&fixed.bytes, "secret-alpha") && find(&fixed.bytes, b"SECRET-ALPHA").is_none());
    assert_eq!(fixed.proof.kept_text_collisions.len(), 1);
    // Independent oracle: the removed instance left the page content, the kept one is intact.
    let kept: usize = independent_streams(&fixed.bytes).iter().map(|s| count(s, DIFF_CODES.as_bytes())).sum();
    assert_eq!(kept, 1);

    // (c) Content no page draws (a stray form) is read with its own resources: a leak.
    let mut objs = minimal_objs("/Hidden 30 0 R", "");
    objs.extend([
        (30, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 9 9] /Resources << /Font << /F1 20 0 R >> >>", format!("BT /F1 12 Tf {DIFF_CODES} Tj ET").as_bytes())),
        (20, diff_font(&differences, "")),
    ]);
    let p = verify_secret(&build(&objs, "", b""));
    assert!(p.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedText { object: 30 })), "{:?}", p.hits);
}

/// A code nothing can read makes the text opaque: glyph names outside the Adobe Glyph List and its
/// naming conventions (rule of dd1df63 for composite fonts, now for simple fonts and Type 3), codes
/// left to an unread built-in encoding, a malformed /Differences. /ToUnicode, a readable name and a
/// non-embedded font (implicit StandardEncoding) are the controls. At 1c3fd8c all of these read as
/// proven.
#[test]
fn unmappable_glyph_names_and_encodingless_fonts_are_opaque() {
    let doc_with = |font: Vec<u8>, extra: Vec<(u32, Vec<u8>)>, kept: &str| {
        let mut objs = minimal_objs("", "");
        objs[2].1 = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 6 0 R /F2 20 0 R >> >> >>".to_vec();
        objs[3].1 = stream_body("", format!("BT /F1 12 Tf 72 700 Td (SECRET-ALPHA) Tj ET\n{kept}").as_bytes());
        objs.extend([(6, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec()), (20, font)]);
        objs.extend(extra);
        build(&objs, "", b"")
    };
    let show = |codes: &str| format!("BT /F2 12 Tf 72 400 Td {codes} Tj ET");
    let type1 = |encoding: &str, extra: &str| format!("<< /Type /Font /Subtype /Type1 /BaseFont /Test {encoding} {extra} >>").into_bytes();
    let diffs = |d: &str| format!("/Encoding << /Type /Encoding /Differences [{d}] >>");
    let opaque = |what: &str, pdf: Vec<u8>| {
        let out = r7_run(&pdf);
        assert_eq!(out.proof.needles_searched, 1, "{what}");
        assert_eq!(out.not_proven_reasons(), vec![NotProven::OpaqueText], "{what}");
        assert!(!out.is_proven() && !out.region_cleared() && out.proof.opaque_text_streams.len() == 1, "{what}");
    };
    let proven = |what: &str, pdf: Vec<u8>| {
        let out = r7_run(&pdf);
        assert!(out.is_proven() && out.proof.opaque_text_streams.is_empty(), "{what}: {:?}", out.not_proven_reasons());
        out
    };
    let to_unicode = stream_body("", b"1 begincodespacerange <00> <FF> endcodespacerange 3 beginbfchar <01> <0041> <02> <0042> <03> <0043> endbfchar");

    // Glyph names outside the Adobe Glyph List.
    let unmappable = doc_with(type1(&diffs("1 /g1 /g2 /g3"), ""), vec![], &show("<010203>"));
    opaque("unmappable glyph names", unmappable.clone());
    // The text-field flow gives the same answer.
    let mut doc = open(&unmappable);
    let fields = redact_text_fields_and_save(&mut doc, &["SECRET-ALPHA".to_string()], &TextFieldPolicy::default(), &token()).unwrap();
    assert_eq!(fields.not_proven_reasons(), vec![NotProven::OpaqueText]);
    // Control: the same names read through /ToUnicode.
    proven("unmappable names with /ToUnicode", doc_with(type1(&diffs("1 /g1 /g2 /g3"), "/ToUnicode 21 0 R"), vec![(21, to_unicode.clone())], &show("<010203>")));
    // uniXXXX naming: a surrogate is no character, a character is.
    opaque("uniD800", doc_with(type1(&diffs("1 /uniD800"), ""), vec![], &show("<01>")));
    proven("uni0053", doc_with(type1(&diffs("1 /uni0053"), ""), vec![], &show("<01>")));
    // A malformed /Differences leaves every code unread.
    opaque("name before any code", doc_with(type1(&diffs("/S"), ""), vec![], &show("(abc)")));
    opaque("code out of range", doc_with(type1(&diffs("300 /S"), ""), vec![], &show("(abc)")));
    // No /Encoding: a non-embedded non-symbolic font is StandardEncoding; an embedded or symbolic
    // font's codes live in the font program, which is not read.
    let desc = |flags: u32, file: &str| (31, format!("<< /Type /FontDescriptor /FontName /ABCDEF+Test /Flags {flags} {file} >>").into_bytes());
    let described = type1("/FontDescriptor 31 0 R", "");
    proven("non-embedded, no /Encoding", doc_with(described.clone(), vec![desc(32, "")], &show("(abc)")));
    // (Embedded fonts carry /Widths: without them the glyph metrics are unknown, R10-F6.)
    let described_w = type1("/FontDescriptor 31 0 R", "/FirstChar 97 /Widths [500 500 500]");
    opaque("embedded, no /Encoding", doc_with(described_w.clone(), vec![desc(32, "/FontFile 32 0 R"), (32, stream_body("", b"%!PS-AdobeFont-1.0"))], &show("(abc)")));
    opaque("symbolic, no /Encoding", doc_with(described.clone(), vec![desc(4, "")], &show("(abc)")));
    proven("embedded with /ToUnicode", doc_with(type1("/FontDescriptor 31 0 R /ToUnicode 21 0 R", "/FirstChar 1 /Widths [500 500 500]"), vec![desc(32, "/FontFile 32 0 R"), (32, stream_body("", b"%!PS-AdobeFont-1.0")), (21, to_unicode.clone())], &show("<010203>")));
    // Type 3: glyph names come only from /Differences; unmappable names are opaque, readable ones are text.
    let type3 = |names: &str| {
        let procs: String = names.split_whitespace().map(|n| format!("{n} 33 0 R ")).collect();
        format!("<< /Type /Font /Subtype /Type3 /FontBBox [0 0 600 600] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << {procs} >> /Encoding << /Type /Encoding /Differences [1 {names}] >> /FirstChar 1 /LastChar 10 /Widths [600 600 600 600 600 600 600 600 600 600] >>")
            .into_bytes()
    };
    let glyph = (33, stream_body("", b"600 0 0 0 600 600 d1 0 0 600 600 re f"));
    // Since R8 F4 (lead decision 11:58Z) Type 3 text on the page makes a region not computable
    // (RegionIncomplete, RedactionClass::Type3Text), so these run with ReportAndContinue; the reading
    // of the Type 3 text is what this test checks.
    let type3_run = |pdf: Vec<u8>| {
        let mut doc = open(&pdf);
        redact_and_save(&mut doc, &[R7_REGION], UnsupportedPolicy::ReportAndContinue, &token()).unwrap()
    };
    let out = type3_run(doc_with(type3("/g1 /g2"), vec![glyph.clone()], &show("<0102>")));
    assert!(out.not_proven_reasons().contains(&NotProven::OpaqueText) && out.proof.opaque_text_streams.len() == 1, "Type 3, unmappable names: {:?}", out.not_proven_reasons());
    let read = type3_run(doc_with(type3(DIFF_NAMES), vec![glyph.clone()], &show(DIFF_CODES)));
    assert_eq!(read.reports[0].unredacted, vec![RedactionClass::Type3Text]); // R8-new-api
    assert!(!read.not_proven_reasons().contains(&NotProven::OpaqueText) && read.proof.hits.is_empty(), "Type 3, readable names: {:?}", read.not_proven_reasons());
    assert_eq!(read.proof.kept_text_collisions.len(), 1, "the Type 3 text is read: the kept instance is a collision, not invisible");
}

/// V4-08: the /Differences array is PDF syntax. Names are read after #xx escapes, a comment hides what
/// is inside it, and glyph-name conventions (uniXXXX, uXXXX, suffix after a period, underscore
/// components, either hex case) apply with any white space; text spelled this hostile way is still found.
#[test]
fn differences_names_are_read_as_pdf_syntax() {
    let spellings: [(&str, &str); 3] = [
        ("name escapes", "1 /#53 /#45 /#43 /#52 /#54 /hyphen /#41 /#4C /#50 /#48"),
        ("comment decoys", "1 /S % 9 /Z /Q\n /E /C /R /T /hyphen /A /L /P /H"),
        ("conventions, hex case and white space", "1\n/uni0053\t/u0045\r/C.alt /R_ /T /hyphen.sc /uni0041 /L /P /uni0048"),
    ];
    for (what, d) in spellings {
        let p = verify_secret(&diff_ap_doc(d));
        assert!(p.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedText { object: 13 })), "{what}: {:?}", p.hits);
    }
    // Decoys that must not read as the text: one glyph differs, the whole run starts one code later.
    for (what, d) in [("one glyph differs", "1 /S /E /C /R /T /hyphen /A /L /P /X"), ("code moved", "2 /S /E /C /R /T /hyphen /A /L /P /H")] {
        let p = verify_secret(&diff_ap_doc(d));
        assert!(p.hits.is_empty(), "{what}: {:?}", p.hits);
    }
}

/// V4-02 for the encoding reader. Reading the longest legal /Differences array is charged to the
/// decoded-byte budget, so a document that names thousands of such fonts runs out of
/// `max_decoded_total` (aggregate_over_budget) instead of spending seconds per font; a font dictionary
/// written directly into the resources and named by 50 000 Tf operators is read once; an array longer
/// than any legal one and codes at the integer edges (dims_u32_max) leave the font unread.
#[test]
fn hostile_differences_are_bounded() {
    // 15 blocks of 128 names: 1935 elements, the code restarts at 0 in each block.
    let blocks: String = (0..15).map(|_| format!("0 {}", "/A ".repeat(128))).collect();
    let font = |differences: &str| format!("<< /Type /Font /Subtype /Type1 /BaseFont /Test /Encoding << /Differences [{differences}] >> >>");
    let zzz = [b"zzzzzz".to_vec()];

    let n_fonts: u32 = 40;
    let mut objs = minimal_objs("", "");
    let names: Vec<String> = (0..n_fonts).map(|i| format!("/F{i} {} 0 R", 20 + i)).collect();
    objs[2].1 = format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << {} >> >> >>", names.join(" ")).into_bytes();
    objs[3].1 = stream_body("", (0..n_fonts).map(|i| format!("BT /F{i} 12 Tf (x) Tj ET\n")).collect::<String>().as_bytes());
    objs.extend((0..n_fonts).map(|i| (20 + i, font(&blocks).into_bytes())));
    let pdf = build(&objs, "", b"");
    let tight = Limits { max_decoded_total: 2 << 20, ..Limits::default() };
    assert_eq!(verify_payload_absent(&pdf, &zzz, &tight, &token()).unwrap_err(), PressError::InputLimit("decoded_total"));
    let p = verify_payload_absent(&pdf, &zzz, &Limits::default(), &token()).unwrap();
    assert!(p.is_clean(), "within the default budget the fonts read: {:?}", p.not_proven_reasons());

    // One direct font, 50 000 Tf: charged once. Uncached it would cost 50 000 x 125 KiB of budget.
    let mut objs = minimal_objs("", "");
    objs[2].1 = format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 {} >> >> >>", font(&blocks)).into_bytes();
    objs[3].1 = stream_body("", "BT /F1 12 Tf (x) Tj ET\n".repeat(50_000).as_bytes());
    let p = verify_payload_absent(&build(&objs, "", b""), &zzz, &Limits::default(), &token()).unwrap();
    assert!(p.is_clean(), "{:?}", p.not_proven_reasons());

    // Too long for any legal array, and codes at the integer edges: the font is unread (opaque).
    let too_long = format!("0 {}", "/A ".repeat(2100));
    for (what, differences) in [("too long", too_long.as_str()), ("u32 edge", "4294967295 /A"), ("i64 edge", "9223372036854775807 /A"), ("negative", "-1 /A")] {
        let mut objs = minimal_objs("", "");
        objs[2].1 = format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 {} >> >> >>", font(differences)).into_bytes();
        objs[3].1 = stream_body("", b"BT /F1 12 Tf (x) Tj ET");
        let p = verify_payload_absent(&build(&objs, "", b""), &zzz, &Limits::default(), &token()).unwrap();
        assert_eq!(p.not_proven_reasons(), vec![NotProven::OpaqueText], "{what}");
    }
}

// ------------------------------------------- R8 pre-review remediation (the proof fails closed)
// Lead decision 2026-10-11T11:58:11Z, validation/R8-prereview-press.json F1-F4, P1, D1-D4. Lines
// marked R8-new-api use API added by this remediation; the red run on 607d47c drops only those lines,
// and every test there fails on an API-stable assertion before them.

/// One page with the R7 test fonts (`R7_RES`: F1 = Type0 with /ToUnicode, F2 = Helvetica, F3 = Type0
/// without /ToUnicode) plus `fonts`, page content `content`, catalog entries `catalog`, `extra` objects.
fn r8_doc(content: &str, fonts: &str, catalog: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = base_objs(content.as_bytes(), "<< /Title (T) >>");
    objs[0].1 = format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>").into_bytes();
    let res = R7_RES.replace("/F3 23 0 R", &format!("/F3 23 0 R {fonts}"));
    objs[2].1 = format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Annots [5 0 R] /Resources << {res} >> >>").into_bytes();
    objs.extend(r7_fonts());
    objs.extend(extra);
    build(&objs, "/Info 7 0 R", b"")
}

/// The removed string in `R7_REGION` (Helvetica), kept text outside it, and `more`.
fn r8_page(more: &str) -> String {
    format!("BT /F2 12 Tf 72 700 Td (SECRET-ALPHA) Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET\n{more}")
}

/// A page whose FreeText annotation (outside any region) has appearance 13 drawing `content` with
/// font 20 (`font`) as /F1.
fn r8_ap_doc(font: Vec<u8>, content: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = minimal_objs("", "5 0 R");
    objs.extend([
        (5, b"<< /Type /Annot /Subtype /FreeText /Rect [100 100 300 140] /Contents (label) /AP << /N 13 0 R >> >>".to_vec()),
        (13, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 200 40] /Resources << /Font << /F1 20 0 R >> >>", content.as_bytes())),
        (20, font),
    ]);
    objs.extend(extra);
    build(&objs, "", b"")
}

/// Type0 font 29 whose /ToUnicode (28) names only code 0101 (S) of `R7_CODES`.
fn r8_partial_font() -> Vec<(u32, Vec<u8>)> {
    vec![
        (29, b"<< /Type /Font /Subtype /Type0 /BaseFont /T /Encoding /Identity-H /DescendantFonts [21 0 R] /ToUnicode 28 0 R >>".to_vec()),
        (28, stream_body("", b"1 begincodespacerange <0000> <FFFF> endcodespacerange 1 beginbfchar <0101> <0053> endbfchar")),
    ]
}

fn decoded_text_hit(p: &hsk_studio_press::PayloadProof, object: u32) -> bool {
    p.hits.iter().any(|h| h.location == PayloadLocation::DecodedText { object })
}

/// F1 (R6 FAIL #2 class): the proof reads the object table, not only the page tree. Page objects
/// outside it (a /Names /Templates template page, ISO 32000-2 12.7.6; a page reached only through an
/// outline /Dest) are read with their resources, a content stream nothing reaches is read with none
/// (its fonts unresolvable: opaque), and composite text whose /ToUnicode lacks a drawn code is opaque.
/// At 607d47c every one of these read as proven.
#[test]
fn partial_tounicode_and_unreachable_pages_are_read_or_opaque() {
    let page = r8_page("");
    let template = |font: &str, codes: &str| {
        vec![
            (30, format!("<< /Type /Page /MediaBox [0 0 612 792] /Resources << /Font << /F9 {font} >> >> /Contents 31 0 R >>").into_bytes()),
            (31, stream_body("", format!("BT /F9 12 Tf 72 300 Td {codes} Tj ET").as_bytes())),
        ]
    };
    let templates = "/Names << /Templates << /Names [(t) 30 0 R] >> >>";
    // Control: a template page with readable text that is not the removed string.
    let out = r7_run(&r8_doc(&page, "", templates, template("24 0 R", "<0101>")));
    assert!(out.is_proven(), "{:?}", out.not_proven_reasons());
    // Opaque composite text (font 23, no /ToUnicode) on a template page (the pre-review example).
    let out = r7_run(&r8_doc(&page, "", templates, template("23 0 R", R7_CODES)));
    assert!(!out.is_proven(), "opaque text on a template page read as proven");
    assert!(out.not_proven_reasons().contains(&NotProven::OpaqueText) && out.proof.opaque_text_streams == vec![31], "{:?}", out.not_proven_reasons());
    // The removed string drawn through /ToUnicode codes on a template page: a leak.
    let out = r7_run(&r8_doc(&page, "", templates, template("24 0 R", R7_CODES)));
    assert!(!out.is_proven() && decoded_text_hit(&out.proof, 31), "{:?}", out.proof.hits);
    // The same page object reached only through an outline destination.
    let mut extra = template("24 0 R", R7_CODES);
    extra.extend([
        (40, b"<< /Type /Outlines /First 41 0 R /Last 41 0 R /Count 1 >>".to_vec()),
        (41, b"<< /Title (Go) /Parent 40 0 R /Dest [30 0 R /Fit] >>".to_vec()),
    ]);
    let out = r7_run(&r8_doc(&page, "", "/Outlines 40 0 R", extra));
    assert!(!out.is_proven() && decoded_text_hit(&out.proof, 31), "{:?}", out.proof.hits);
    // A content stream nothing reaches as content (no /Subtype, no /Resources): read with no
    // resources, so /F1 cannot be resolved and its text is opaque.
    let hidden = (32, stream_body("", format!("BT /F1 12 Tf 72 300 Td {R7_CODES} Tj ET").as_bytes()));
    let out = r7_run(&r8_doc(&page, "", "/Hidden 32 0 R", vec![hidden]));
    assert!(!out.is_proven(), "unreached content stream read as proven");
    assert!(out.not_proven_reasons().contains(&NotProven::OpaqueText) && out.proof.opaque_text_streams == vec![32], "{:?}", out.not_proven_reasons());
    // Kept page text drawn with a composite font whose /ToUnicode names only the first code.
    let out = r7_run(&r8_doc(&r8_page(&format!("BT /F4 12 Tf 72 200 Td {R7_CODES} Tj ET")), "/F4 29 0 R", "", r8_partial_font()));
    assert!(!out.is_proven(), "partially mapped composite text read as proven");
    // (The redaction gave the page a new content stream object; it is the only opaque one.)
    assert!(out.not_proven_reasons().contains(&NotProven::OpaqueText) && out.proof.opaque_text_streams.len() == 1, "{:?} {:?}", out.not_proven_reasons(), out.proof.opaque_text_streams);
}

/// F2/D2: text one kerned TJ draws as one run is matched as one run (its readings and its bytes
/// joined) in appearances and stray content, with Helvetica, /ToUnicode codes and /Differences codes
/// split over TJ elements; a text-field pass drops such an appearance; kept page text stays a
/// collision. At 607d47c each of these read as clean.
#[test]
fn kerned_tj_runs_are_read_whole() {
    let ap = |content: &str| r7_verify(&r7_font_doc(R7_PLAIN, None, Some(content), "", vec![]));
    // Helvetica (StandardEncoding): the bytes are the text, split by a kerning number.
    let p = ap("BT /F2 12 Tf 2 5 Td [(SECRET-) -15 (ALPHA)] TJ ET");
    assert!(!p.is_clean() && decoded_text_hit(&p, 26), "kerned Helvetica appearance: {:?}", p.hits);
    // /ToUnicode codes split over two elements.
    let p = ap("BT /F1 12 Tf 2 5 Td [<0101010201030104010501060107> -15 <01080109010A010B010C>] TJ ET");
    assert!(!p.is_clean() && decoded_text_hit(&p, 26), "kerned ToUnicode appearance: {:?}", p.hits);
    // /Differences codes split over two elements.
    let split_diff = "BT /F1 12 Tf 2 5 Td [<01020304020506> -20 <0708090A07>] TJ ET";
    let pdf = r8_ap_doc(diff_font(&format!("1 {DIFF_NAMES}"), ""), split_diff, vec![]);
    let p = verify_secret(&pdf);
    assert!(!p.is_clean() && decoded_text_hit(&p, 13), "kerned /Differences appearance: {:?}", p.hits);
    // The text-field pass reads the run whole too: the appearance is dropped and the result proven.
    let mut doc = open(&pdf);
    let out = redact_text_fields_and_save(&mut doc, &["SECRET-ALPHA".to_string()], &TextFieldPolicy::default(), &token()).unwrap();
    assert_eq!(rows(&out.report, TextRole::Appearance), vec![(5, false, TextOutcomeKind::Dropped, 1)]);
    assert!(out.is_proven() && find(&out.bytes, b"/N 13 0 R").is_none(), "{:?}", out.not_proven_reasons());
    // Stray content (a form nothing draws, with its own resources).
    let form = stream_body("/Type /XObject /Subtype /Form /BBox [0 0 9 9] /Resources << /Font << /F2 6 0 R >> >>", b"BT /F2 12 Tf [(SECRET-) -15 (ALPHA)] TJ ET");
    let out = r7_run(&r8_doc(&r8_page(""), "", "/Hidden 33 0 R", vec![(33, form)]));
    assert!(!out.is_proven() && decoded_text_hit(&out.proof, 33), "kerned stray form: {:?}", out.proof.hits);
    // The removal side: kerned pieces each shorter than a distinctive needle are searched as the run
    // they draw; its twin in an outline title is a leak, and a text policy redacts it.
    let outline = vec![
        (40, b"<< /Type /Outlines /First 41 0 R /Last 41 0 R /Count 1 >>".to_vec()),
        (41, b"<< /Title (SECRET-ALPHA plan) /Parent 40 0 R >>".to_vec()),
    ];
    let pieces = "BT /F2 12 Tf 72 700 Td [(SECR) -10 (ET-A) -10 (LPHA)] TJ ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET";
    let pdf = r8_doc(pieces, "", "/Outlines 40 0 R", outline);
    let out = r7_run(&pdf);
    assert!(!out.is_proven() && out.proof.hits.iter().any(|h| h.location == PayloadLocation::StringObject { object: 41 }), "kerned removed pieces: {:?}", out.proof.hits);
    assert_eq!((out.reports[0].text_strings_removed, out.reports[0].removed_text.last()), (3, Some(&b"SECRET-ALPHA".to_vec())));
    let mut doc = open(&pdf);
    let with = redact_and_save_with(&mut doc, &[R7_REGION], UnsupportedPolicy::Refuse, Some(&TextFieldPolicy::default()), &token()).unwrap();
    assert!(with.is_proven() && !leaks(&with.bytes, "secret-alpha"), "{:?}", with.not_proven_reasons());
    // Controls: a different word split the same way is not a hit; kerned kept page text is a collision.
    let p = ap("BT /F2 12 Tf 2 5 Td [(SECRET-) -15 (BETA)] TJ ET");
    assert!(p.is_clean(), "{:?}", p.hits);
    let p = r7_verify(&r7_font_doc("BT /F2 12 Tf 72 300 Td [(SECRET-) -15 (ALPHA)] TJ ET", None, None, "", vec![]));
    assert!(p.is_clean() && p.kept_text_collisions.iter().any(|c| c.object == 4), "{:?}", p);
}

/// P1: a byte match inside kept page text is exempt only when the font draws those bytes as the same
/// letters. Kept "(SECRET-ALPHA) Tj" in a /Differences font that draws every capital as X is not the
/// removed text, only its bytes: a leak. At 607d47c it read as a kept-text collision (proven).
#[test]
fn kept_text_needs_a_drawn_reading() {
    let font = |names: &str| (33, format!("<< /Type /Font /Subtype /Type1 /BaseFont /Test /Encoding << /Type /Encoding /Differences [65 {names}] >> >>").into_bytes());
    let kept = r8_page("BT /F5 12 Tf 72 200 Td (SECRET-ALPHA) Tj ET");
    let out = r7_run(&r8_doc(&kept, "/F5 33 0 R", "", vec![font(&"/X ".repeat(26))]));
    assert!(!out.is_proven(), "bytes drawn as other glyphs read as kept text: {:?}", out.proof);
    assert!(out.not_proven_reasons().contains(&NotProven::Leak) && out.proof.kept_text_collisions.is_empty(), "{:?}", out.proof);
    // Controls: the names spelled as the letters they are, and plain Helvetica: kept text (collision).
    let az: String = ('A'..='Z').map(|c| format!("/{c} ")).collect();
    let out = r7_run(&r8_doc(&kept, "/F5 33 0 R", "", vec![font(&az)]));
    assert!(out.is_proven() && !out.proof.kept_text_collisions.is_empty(), "{:?}", out.not_proven_reasons());
    let out = r7_run(&r8_doc(&r8_page("BT /F2 12 Tf 72 200 Td (SECRET-ALPHA) Tj ET"), "", "", vec![]));
    assert!(out.is_proven() && !out.proof.kept_text_collisions.is_empty(), "{:?}", out.not_proven_reasons());
}

/// F3/D3: a removed string whose words cannot be read (composite font without /ToUnicode,
/// unmappable or empty-stem /Differences names, a partial /ToUnicode) was searched only as its codes,
/// so the same words in an outline title read as proven; and an encoding the selector's budget could
/// not pay for was silently dropped. At 607d47c each read as proven or succeeded.
#[test]
fn unreadable_removed_text_is_never_proven() {
    let titled = || {
        vec![
            (40, b"<< /Type /Outlines /First 41 0 R /Last 41 0 R /Count 1 >>".to_vec()),
            (41, b"<< /Title (SECRET-ALPHA plan) /Parent 40 0 R >>".to_vec()),
        ]
    };
    let region_text = |font: &str, codes: &str| format!("BT /{font} 12 Tf 72 700 Td {codes} Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET");
    // Composite font 23 (no /ToUnicode) draws the region text; its words live on in the outline title.
    let pdf = r8_doc(&region_text("F3", R7_CODES), "", "/Outlines 40 0 R", titled());
    let out = r7_run(&pdf);
    assert!(!out.is_proven(), "unreadable removed text read as proven");
    assert!(out.region_cleared(), "the in-region instance itself is gone: {:?}", out.not_proven_reasons());
    assert_eq!(out.not_proven_reasons(), vec![NotProven::UnreadableRemovedText]); // R8-new-api
    assert_eq!((out.unreadable_removed, out.reports[0].removed_unreadable.clone()), (1, vec![true])); // R8-new-api
    // A text policy cannot find the words either (independent oracle: the title still holds them).
    let mut doc = open(&pdf);
    let with = redact_and_save_with(&mut doc, &[R7_REGION], UnsupportedPolicy::Refuse, Some(&TextFieldPolicy::default()), &token()).unwrap();
    assert!(!with.is_proven() && leaks(&with.bytes, "secret-alpha plan"), "{:?}", with.not_proven_reasons());
    // Simple fonts whose names nothing reads (outside the glyph list; empty stems, D1).
    for names in ["/g1 /g2 /g3 /g4 /g5 /g6 /g7", "/.s1 /.s2 /.s3 /_ /__ /.A /.B"] {
        let font = (33, format!("<< /Type /Font /Subtype /Type1 /BaseFont /Test /Encoding << /Type /Encoding /Differences [1 {names}] >> >>").into_bytes());
        let mut extra = titled();
        extra.push(font);
        let out = r7_run(&r8_doc(&region_text("F5", "<01020304050607>"), "/F5 33 0 R", "/Outlines 40 0 R", extra));
        assert!(!out.is_proven() && out.region_cleared(), "{names}: {:?}", out.not_proven_reasons());
        assert_eq!(out.not_proven_reasons(), vec![NotProven::UnreadableRemovedText], "{names}"); // R8-new-api
    }
    // A composite font whose /ToUnicode lacks drawn codes: the reading is partial.
    let out = r7_run(&r8_doc(&region_text("F4", R7_CODES), "/F4 29 0 R", "", r8_partial_font()));
    assert_eq!(out.reports[0].removed_unreadable, vec![true]); // R8-new-api
    assert!(out.not_proven_reasons().contains(&NotProven::UnreadableRemovedText)); // R8-new-api
    // Control: readable composite text (font 24): the outline title is found (a leak), not unreadable.
    let out = r7_run(&r8_doc(&region_text("F1", R7_CODES), "", "/Outlines 40 0 R", titled()));
    assert_eq!(out.not_proven_reasons(), vec![NotProven::Leak]);
    // Budget: reading the region font's encoding needs more than the pass budget. The selector
    // refuses (InputLimit) instead of reading the text without its encoding.
    let blocks: String = (0..15).map(|_| format!("0 {}", "/A ".repeat(128))).collect();
    let big = (34, format!("<< /Type /Font /Subtype /Type1 /BaseFont /Test /Encoding << /Differences [{blocks}] >> >>").into_bytes());
    let pdf = r8_doc(&region_text("F6", "(SECRET-ALPHA)"), "/F6 34 0 R", "", vec![big]);
    let tight = Limits { max_decoded_total: 100_000, ..Limits::default() };
    let mut doc = PdfDoc::open(&pdf, &tight, &token()).unwrap();
    assert_eq!(doc.redact_region(&R7_REGION, UnsupportedPolicy::Refuse, &token()).unwrap_err(), PressError::InputLimit("decoded_total"));
}

/// F4/D4 (PLR-039 re-opened; lead decision 11:58Z): the selector does not compute Type 3 glyph
/// geometry (/FontMatrix) or vertical writing, so a region on a page drawing such text is never read
/// as cleared. At 607d47c a Type 3 font with /FontMatrix [1 0 0 1 0 0] drew SECRET at x 160..196 while
/// the selector placed it at 100.06..100.096: a region over it selected nothing and read as cleared
/// and proven; vertical text below its first glyph read the same.
#[test]
fn type3_and_vertical_text_never_read_as_cleared() {
    let widths = vec!["0.6"; 20].join(" ");
    let t3 = format!(
        "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [1 0 0 1 0 0] /CharProcs << /A 36 0 R /C 36 0 R /E 36 0 R /R 36 0 R /S 36 0 R /T 36 0 R >> /Encoding << /Type /Encoding /Differences [65 /A 67 /C 69 /E 82 /R /S /T] >> /FirstChar 65 /LastChar 84 /Widths [{widths}] >>"
    );
    let glyph = (36, stream_body("", b"0.6 0 0 0 0.6 0.7 d1 0 0 0.6 0.7 re f"));
    let content = "BT /T3 10 Tf 100 700 Td (AAAAAAAAAA) Tj (SECRET) Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET";
    let pdf = r8_doc(content, "/T3 35 0 R", "", vec![(35, t3.into_bytes()), glyph.clone()]);
    let region = RedactionRegion { page_index: 0, rect: [150.0, 690.0, 210.0, 715.0] };
    let mut doc = open(&pdf);
    let out = redact_and_save(&mut doc, &[region], UnsupportedPolicy::ReportAndContinue, &token()).unwrap();
    // Independent oracle: SECRET is still drawn in the region (x = 100 + 10 x 0.6 x 10 = 160..196).
    assert!(independent_streams(&out.bytes).iter().any(|s| find(s, b"(SECRET) Tj").is_some()), "the Type 3 run left the content");
    assert!(!out.region_cleared() && !out.is_proven(), "Type 3 text on the page read as cleared");
    assert!(out.not_proven_reasons().contains(&NotProven::RegionIncomplete), "{:?}", out.not_proven_reasons());
    assert_eq!(out.reports[0].unredacted, vec![RedactionClass::Type3Text]); // R8-new-api
    let mut doc = open(&pdf);
    let refused = redact_and_save(&mut doc, &[region], UnsupportedPolicy::Refuse, &token());
    assert_eq!(refused.unwrap_err(), PressError::UnsupportedRedactionClass(RedactionClass::Type3Text)); // R8-new-api

    // Vertical writing: /Identity-V, and an embedded CMap that sets /WMode 1; glyphs advance down the
    // page from y 700, the region lies below the first glyph.
    let below = RedactionRegion { page_index: 0, rect: [90.0, 600.0, 130.0, 650.0] };
    let vtext = format!("BT /V1 12 Tf 100 700 Td {R7_CODES} Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET");
    let cmap = (38, stream_body("/Type /CMap /CMapName /Test-Embedded", b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap /WMode 1 def 1 begincodespacerange <0000> <FFFF> endcodespacerange 1 begincidrange <0000> <FFFF> 0 endcidrange endcmap end end"));
    let font = |enc: &str| (37, format!("<< /Type /Font /Subtype /Type0 /BaseFont /T /Encoding {enc} /DescendantFonts [21 0 R] /ToUnicode 25 0 R >>").into_bytes());
    for enc in ["/Identity-V", "38 0 R"] {
        let mut doc = open(&r8_doc(&vtext, "/V1 37 0 R", "", vec![font(enc), cmap.clone()]));
        let out = redact_and_save(&mut doc, &[below], UnsupportedPolicy::ReportAndContinue, &token()).unwrap();
        assert!(independent_streams(&out.bytes).iter().any(|s| find(s, R7_CODES.as_bytes()).is_some()), "{enc}");
        assert!(!out.region_cleared(), "{enc}: vertical text read as cleared");
        assert_eq!(out.reports[0].unredacted, vec![RedactionClass::VerticalText], "{enc}"); // R8-new-api
    }
    // Controls: the same text written horizontally (/Identity-H), and a Type 3 font that is only listed.
    let mut doc = open(&r8_doc(&vtext, "/V1 37 0 R", "", vec![font("/Identity-H")]));
    let out = redact_and_save(&mut doc, &[below], UnsupportedPolicy::Refuse, &token()).unwrap();
    assert!(out.region_cleared() && out.is_proven(), "{:?}", out.not_proven_reasons());
    let listed_t3 = b"<< /Type /Font /Subtype /Type3 /FontMatrix [1 0 0 1 0 0] /CharProcs << >> /Encoding << /Differences [] >> /FirstChar 65 /LastChar 65 /Widths [0.6] >>";
    let listed = r8_doc("BT /F2 12 Tf 160 700 Td (SECRET) Tj ET", "/T3 35 0 R", "", vec![(35, listed_t3.to_vec())]);
    let mut doc = open(&listed);
    let out = redact_and_save(&mut doc, &[region], UnsupportedPolicy::Refuse, &token()).unwrap();
    assert!(out.region_cleared() && out.reports[0].text_strings_removed == 1, "{:?}", out.not_proven_reasons());
}

/// D1: only /.notdef is a glyph without text; a name whose stem is empty (/.s1, /_, /.A) names a glyph
/// the font draws that no reading turns into text: opaque. At 607d47c such names read as blank, so
/// text drawn with them was never checked and the proof read clean.
#[test]
fn empty_stem_glyph_names_are_opaque() {
    for names in ["/.s1 /.s2 /.s3 /.s4 /.s5 /.s6 /.s7 /.s8 /.s9 /.s10", "/_ /__ /.A /.B /._ /.x /.y /.z /.a /.b"] {
        let p = verify_secret(&diff_ap_doc(&format!("1 {names}")));
        assert!(!p.is_clean(), "{names}: text drawn with empty-stem glyph names was never read");
        assert_eq!((p.not_proven_reasons(), p.opaque_text_streams.clone()), (vec![NotProven::OpaqueText], vec![13]), "{names}");
    }
    // The pre-review case: a Type 3 font whose glyph procedures are named /.s1 ... in an appearance.
    let procs: String = (1..=10).map(|i| format!("/.s{i} 34 0 R ")).collect();
    let names: String = (1..=10).map(|i| format!("/.s{i} ")).collect();
    let t3 = format!("<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1 1] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << {procs}>> /Encoding << /Differences [1 {names}] >> /FirstChar 1 /LastChar 10 /Widths [600 600 600 600 600 600 600 600 600 600] >>");
    let glyph = (34, stream_body("", b"600 0 0 0 600 700 d1 0 0 600 700 re f"));
    let p = verify_secret(&r8_ap_doc(t3.into_bytes(), &format!("BT /F1 12 Tf 2 5 Td {DIFF_CODES} Tj ET"), vec![glyph]));
    assert!(!p.is_clean() && p.opaque_text_streams == vec![13], "Type 3 empty-stem names: {:?}", p.not_proven_reasons());
    // Control: /.notdef draws nothing.
    let p = verify_secret(&diff_ap_doc(&format!("1 {}", "/.notdef ".repeat(10))));
    assert!(p.is_clean(), "{:?}", p.not_proven_reasons());
}

// --------------------------------- R9 pre-review remediation (POSITIVE DETERMINATION)
// Lead decision 2026-10-11T13:10:26Z, validation/R9-prereview-press.json R9-F1..F3, R9-P1, R9-P2: one
// test per sub-case the lead listed. Each test collects every failing sub-case (`expect`) before it
// asserts, so the red run on 49731a5 names each one. Lines marked R9-new-api use API added by this
// remediation; the red run drops only those lines.

use hsk_studio_press::TextCause; // R9-new-api

/// Records a failed expectation (`what`) instead of stopping at the first one.
fn expect(red: &mut Vec<String>, ok: bool, what: impl std::fmt::Display) {
    if !ok {
        red.push(what.to_string());
    }
}

/// The vertical-text fixture of `type3_and_vertical_text_never_read_as_cleared`: R7_CODES drawn at
/// (100, 700) with font 37 (`/V1`, Type0 over CIDFont 21, ToUnicode 25) whose `/Encoding` is `enc`.
fn r9_cmap_doc(enc: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let vtext = format!("BT /V1 12 Tf 100 700 Td {R7_CODES} Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET");
    let font = (37, format!("<< /Type /Font /Subtype /Type0 /BaseFont /T /Encoding {enc} /DescendantFonts [21 0 R] /ToUnicode 25 0 R >>").into_bytes());
    let mut objs = vec![font];
    objs.extend(extra);
    r8_doc(&vtext, "/V1 37 0 R", "", objs)
}

/// An embedded CMap stream (object 39) with `dict` entries and `program` inside its data.
fn r9_cmap(dict: &str, program: &str) -> (u32, Vec<u8>) {
    let body = format!("/CIDInit /ProcSet findresource begin 12 dict begin begincmap {program} 1 begincodespacerange <0000> <FFFF> endcodespacerange 1 begincidrange <0000> <FFFF> 0 endcidrange endcmap end end");
    (39, stream_body(&format!("/Type /CMap /CMapName /Test-Embedded {dict}"), body.as_bytes()))
}

/// The region below the first glyph of `r9_cmap_doc` (a vertical run passes through it).
const R9_BELOW: RedactionRegion = RedactionRegion { page_index: 0, rect: [90.0, 600.0, 130.0, 650.0] };

fn r9_run(pdf: &[u8], region: RedactionRegion, policy: UnsupportedPolicy) -> Result<hsk_studio_press::RedactedOutput, PressError> {
    let mut doc = open(pdf);
    redact_and_save(&mut doc, &[region], policy, &token())
}

/// R9-F1 (predefined CMap /V): writing mode is read positively from the CMap. /V is vertical (ISO
/// 32000-2 9.7.5.2 Table 116) but does not end in -V; an embedded CMap can use it (/UseCMap, or
/// `usecmap` in its program). At 49731a5 each of these read as horizontal: the vertical run below its
/// first glyph was never selected and the region read as cleared and proven.
#[test]
fn writing_mode_is_read_from_the_cmap() {
    let mut red = Vec::new();
    let vertical: Vec<(&str, Vec<u8>)> = vec![
        ("predefined /V", r9_cmap_doc("/V", vec![])),
        ("embedded, /UseCMap /V", r9_cmap_doc("39 0 R", vec![r9_cmap("/UseCMap /V", "")])),
        ("embedded, program /V usecmap", r9_cmap_doc("39 0 R", vec![r9_cmap("", "/V usecmap")])),
        ("embedded, program /WMode 1", r9_cmap_doc("39 0 R", vec![r9_cmap("", "/WMode 1 def")])),
    ];
    for (what, pdf) in &vertical {
        let out = r9_run(pdf, R9_BELOW, UnsupportedPolicy::ReportAndContinue).unwrap();
        // Independent oracle: the vertical run is still drawn (it starts at y 700 and runs down).
        assert!(independent_streams(&out.bytes).iter().any(|s| find(s, R7_CODES.as_bytes()).is_some()), "{what}");
        expect(&mut red, !out.region_cleared() && !out.is_proven(), format!("{what}: vertical text read as cleared"));
        expect(&mut red, out.reports[0].unredacted == vec![RedactionClass::VerticalText], format!("{what}: {:?}", out.reports[0].unredacted));
        expect(&mut red, r9_run(pdf, R9_BELOW, UnsupportedPolicy::Refuse) == Err(PressError::UnsupportedRedactionClass(RedactionClass::VerticalText)), format!("{what}: not refused"));
        assert_eq!(out.region_undetermined, vec![(0, RedactionClass::VerticalText)], "{what}"); // R9-new-api
    }
    // Controls: horizontal predefined CMaps (Identity-H, H) and an embedded CMap with /WMode 0.
    for (what, pdf) in [
        ("Identity-H", r9_cmap_doc("/Identity-H", vec![])),
        // A predefined horizontal CMap whose codespace holds R7_CODES (/H's 2121..7E7E does not: R10-F4).
        ("UniJIS-UCS2-H", r9_cmap_doc("/UniJIS-UCS2-H", vec![])),
        ("embedded /WMode 0", r9_cmap_doc("39 0 R", vec![r9_cmap("/WMode 0", "/WMode 0 def /H usecmap")])),
    ] {
        let out = r9_run(&pdf, R9_BELOW, UnsupportedPolicy::Refuse).unwrap();
        assert!(out.region_cleared() && out.is_proven(), "{what}: {:?}", out.not_proven_reasons());
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R9-F1 (unknown CMap name): a CMap name outside ISO 32000-2 Table 116 (a real Adobe CMap the table
/// does not list, an invented one), a /WMode the table does not define and an embedded CMap that does
/// not tokenize are not known horizontal: the region is not cleared and the proof names the cause. At
/// 49731a5 every name not ending in -V read as horizontal.
#[test]
fn unknown_cmap_names_are_unresolved_writing_modes() {
    let mut red = Vec::new();
    let unresolved: Vec<(&str, Vec<u8>)> = vec![
        ("UniJIS2004-UTF16-H", r9_cmap_doc("/UniJIS2004-UTF16-H", vec![])),
        ("invented name", r9_cmap_doc("/Secret-H", vec![])),
        ("/WMode 2", r9_cmap_doc("39 0 R", vec![r9_cmap("/WMode 2", "")])),
        ("untokenizable program", r9_cmap_doc("39 0 R", vec![(39, stream_body("/Type /CMap", b"begincmap } endcmap"))])),
    ];
    for (what, pdf) in &unresolved {
        let out = r9_run(pdf, R9_BELOW, UnsupportedPolicy::ReportAndContinue).unwrap();
        expect(&mut red, !out.region_cleared() && !out.is_proven(), format!("{what}: unknown writing mode read as cleared"));
        expect(&mut red, r9_run(pdf, R9_BELOW, UnsupportedPolicy::Refuse).is_err(), format!("{what}: not refused"));
        assert_eq!(out.reports[0].unredacted, vec![RedactionClass::UnresolvedWritingMode], "{what}"); // R9-new-api
        assert!(out.not_proven_reasons().contains(&NotProven::UnresolvedWritingMode), "{what}"); // R9-new-api
        assert!(out.proof.undetermined_text.iter().any(|(_, c)| *c == TextCause::UnresolvedWritingMode), "{what}"); // R9-new-api
    }
    // The proof side: text drawn anywhere (an annotation appearance) with such a font.
    let font = b"<< /Type /Font /Subtype /Type0 /BaseFont /T /Encoding /Secret-H /DescendantFonts [21 0 R] /ToUnicode 25 0 R >>".to_vec();
    let cid = r7_fonts().into_iter().filter(|(n, _)| *n == 21 || *n == 25).collect();
    let p = verify_secret(&r8_ap_doc(font, "BT /F1 12 Tf 2 5 Td <0101> Tj ET", cid));
    expect(&mut red, !p.is_clean(), "an unresolved writing mode in an appearance read as clean");
    // Since R10-F4 a composite string is split by its encoding CMap's codespace: an unknown CMap has
    // none, so its codes are also unread (the lead decision 2026-10-11T15:00:20Z adds that cause).
    assert_eq!(p.not_proven_reasons(), vec![NotProven::OpaqueText, NotProven::UnresolvedWritingMode]); // R9-new-api
    assert_eq!(p.undetermined_text, vec![(13, TextCause::UnresolvedWritingMode), (13, TextCause::UnreadableCode)]); // R9-new-api
    assert!(red.is_empty(), "{red:#?}");
}

/// base_objs page (annotation 5, image 8) with `resources` (fonts, ExtGStates, XObjects besides Im1).
fn r9_page_doc(content: &str, resources: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = base_objs(content.as_bytes(), "<< /Title (T) >>");
    let resources = if resources.contains("/XObject << ") { resources.replace("/XObject << ", "/XObject << /Im1 8 0 R ") } else { format!("{resources} /XObject << /Im1 8 0 R >>") };
    objs[2].1 = format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Annots [5 0 R] /Resources << {resources} >> >>").into_bytes();
    objs.extend(extra);
    build(&objs, "/Info 7 0 R", b"")
}

/// A region inside the Helvetica 12 run of SECRET-ALPHA drawn from (100, 700) (x 100..about 192).
const R9_INSIDE: RedactionRegion = RedactionRegion { page_index: 0, rect: [120.0, 690.0, 180.0, 715.0] };

/// R9-F2 (font set by gs): the selector's font is graphics state like a viewer's; an ExtGState
/// `/Font [font size]` selects font and size (ISO 32000-2 8.4.5 Table 57), q/Q restore it. At 49731a5
/// the selector knew only `Tf`: the run got a zero-size box at its origin, was never removed, and the
/// region read as cleared and proven while SECRET-ALPHA was drawn across it.
#[test]
fn font_is_graphics_state_in_the_selector() {
    let mut red = Vec::new();
    let gs_res = "/Font << /F1 6 0 R >> /ExtGState << /GS9 << /Font [6 0 R 12] >> >>";
    for (what, content) in [
        ("gs, no Tf", "BT /GS9 gs 100 700 Td (SECRET-ALPHA) Tj ET\nBT /F1 12 Tf 72 300 Td (Public) Tj ET"),
        ("gs, then Tf inside q..Q", "BT /GS9 gs q /F1 30 Tf Q 100 700 Td (SECRET-ALPHA) Tj ET\nBT /F1 12 Tf 72 300 Td (Public) Tj ET"),
    ] {
        let out = r9_run(&r9_page_doc(content, gs_res, vec![]), R9_INSIDE, UnsupportedPolicy::Refuse).unwrap();
        expect(&mut red, out.reports[0].text_strings_removed == 1, format!("{what}: the run across the region was not removed"));
        // Independent oracle: the string left the page content.
        expect(&mut red, independent_streams(&out.bytes).iter().all(|s| find(s, b"SECRET-ALPHA").is_none()), format!("{what}: SECRET-ALPHA still drawn"));
        expect(&mut red, out.is_proven() && out.region_cleared(), format!("{what}: {:?}", out.not_proven_reasons()));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R9-F2 (font inherited by a form without its own /Font): a form XObject starts with the caller's
/// graphics state, its font included (ISO 32000-2 8.10.1), whatever its own /Resources hold. A Type 3
/// font set on the page and inherited into such a form (or selected by gs) is Type 3 text. At 49731a5
/// the selector re-resolved the inherited name in the form's own resources, found nothing, placed the
/// glyphs 10 pt apart instead of 20 and never marked the Type 3 text: the region read as cleared.
#[test]
fn fonts_inherited_into_forms_are_resolved() {
    let mut red = Vec::new();
    let t3 = (35, b"<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.002 0 0 0.002 0 0] /CharProcs << /S 36 0 R >> /Encoding << /Differences [83 /S] >> /FirstChar 65 /LastChar 90 /Widths [1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000 1000] >>".to_vec());
    let proc_ = (36, stream_body("", b"1000 0 0 0 1000 1000 d1 0 0 1000 1000 re f"));
    let form = (28, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Resources << /Font << /F2 6 0 R >> >>", b"BT 100 700 Td (SECRET) Tj ET"));
    let region = RedactionRegion { page_index: 0, rect: [170.0, 690.0, 215.0, 715.0] };
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("Tf on the page, inherited by the form", r9_page_doc("/T3 10 Tf q /Fm1 Do Q\nBT /F1 12 Tf 72 300 Td (Public) Tj ET", "/Font << /T3 35 0 R /F1 6 0 R >> /XObject << /Fm1 28 0 R >>", vec![t3.clone(), proc_.clone(), form.clone()])),
        ("gs on the page, inherited by the form", r9_page_doc("/GS3 gs q /Fm1 Do Q\nBT /F1 12 Tf 72 300 Td (Public) Tj ET", "/Font << /F1 6 0 R >> /ExtGState << /GS3 << /Font [35 0 R 10] >> >> /XObject << /Fm1 28 0 R >>", vec![t3.clone(), proc_.clone(), form])),
        ("gs in page text", r9_page_doc("BT /GS3 gs 100 700 Td (SECRET) Tj ET", "/Font << /F1 6 0 R >> /ExtGState << /GS3 << /Font [35 0 R 10] >> >>", vec![t3, proc_])),
    ];
    for (what, pdf) in &cases {
        expect(&mut red, r9_run(pdf, region, UnsupportedPolicy::Refuse) == Err(PressError::UnsupportedRedactionClass(RedactionClass::Type3Text)), format!("{what}: not refused"));
        let out = r9_run(pdf, region, UnsupportedPolicy::ReportAndContinue).unwrap();
        expect(&mut red, !out.region_cleared() && out.reports[0].unredacted == vec![RedactionClass::Type3Text], format!("{what}: {:?}", out.not_proven_reasons()));
        assert_eq!(out.region_undetermined, vec![(0, RedactionClass::Type3Text)], "{what}"); // R9-new-api
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R9-F2 (unresolvable font): text whose font is not determined (a name the resources do not hold, no
/// font selected at all) has unknown glyph widths: the region is never cleared and the proof names the
/// cause. At 49731a5 such text was placed as if its widths were known and the region read as cleared
/// and proven.
#[test]
fn undetermined_fonts_never_read_as_cleared() {
    let mut red = Vec::new();
    let unresolved = r9_page_doc("BT /F9 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET", "/Font << /F1 6 0 R >>", vec![]);
    expect(&mut red, r9_run(&unresolved, R9_INSIDE, UnsupportedPolicy::Refuse).is_err(), "unresolved font name: not refused");
    let out = r9_run(&unresolved, R9_INSIDE, UnsupportedPolicy::ReportAndContinue).unwrap();
    expect(&mut red, !out.region_cleared(), "unresolved font name: region read as cleared");
    // Text before any Tf (the font is graphics state: a later BT does not reset it, and nothing set it).
    let no_font = r9_page_doc("BT 72 300 Td (Public) Tj ET\nBT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET", "/Font << /F1 6 0 R >>", vec![]);
    let out = r9_run(&no_font, R9_INSIDE, UnsupportedPolicy::ReportAndContinue).unwrap();
    expect(&mut red, !out.region_cleared() && !out.is_proven(), "text with no font on the page read as cleared");
    expect(&mut red, out.not_proven_reasons().contains(&NotProven::OpaqueText), format!("no font: {:?}", out.not_proven_reasons()));
    assert_eq!(r9_run(&unresolved, R9_INSIDE, UnsupportedPolicy::Refuse), Err(PressError::UnsupportedRedactionClass(RedactionClass::UnresolvedFont))); // R9-new-api
    assert_eq!(out.reports[0].unredacted, vec![RedactionClass::UnresolvedFont]); // R9-new-api
    assert!(out.proof.undetermined_text.iter().any(|(_, c)| *c == TextCause::NoFont), "{:?}", out.proof.undetermined_text); // R9-new-api
    assert_eq!(out.region_undetermined, vec![(0, RedactionClass::UnresolvedFont)]); // R9-new-api
    assert!(red.is_empty(), "{red:#?}");
}

/// R9-F3 (split Tj in one BT): a run drawn by several show operators (or text objects, or a form a
/// stream draws) is read whole, in Helvetica, /ToUnicode and /Differences codes; the text-field pass
/// drops such an appearance; the whole-token rule exempts only what one string's reading proves. At
/// 49731a5 each split read as clean.
#[test]
fn split_runs_are_read_whole() {
    let mut red = Vec::new();
    let helv = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec();
    for content in [
        "BT /F1 12 Tf 2 5 Td (SECRET-) Tj (ALPHA) Tj ET",
        "BT /F1 12 Tf 2 5 Td (SECRET-) Tj 40 0 Td (ALPHA) Tj ET",
        "BT /F1 12 Tf 2 5 Td (SECRET-) Tj ET BT /F1 12 Tf 60 5 Td (ALPHA) ' ET",
    ] {
        let p = verify_secret(&r8_ap_doc(helv.clone(), content, vec![]));
        expect(&mut red, !p.is_clean() && decoded_text_hit(&p, 13), format!("{content}: {:?}", p.hits));
    }
    let p = r7_verify(&r7_font_doc(R7_PLAIN, None, Some("BT /F1 12 Tf 2 5 Td <0101010201030104010501060107> Tj <01080109010A010B010C> Tj ET"), "", vec![]));
    expect(&mut red, !p.is_clean() && decoded_text_hit(&p, 26), format!("ToUnicode split: {:?}", p.hits));
    let split_diff = r8_ap_doc(diff_font(&format!("1 {DIFF_NAMES}"), ""), "BT /F1 12 Tf 2 5 Td <01020304020506> Tj <0708090A07> Tj ET", vec![]);
    let p = verify_secret(&split_diff);
    expect(&mut red, !p.is_clean() && decoded_text_hit(&p, 13), format!("Differences split: {:?}", p.hits));
    // Split between an appearance and a form it draws (the form inherits font and resources).
    let mut objs = minimal_objs("", "5 0 R");
    objs.extend([
        (5, b"<< /Type /Annot /Subtype /FreeText /Rect [100 100 300 140] /Contents (label) /AP << /N 13 0 R >> >>".to_vec()),
        (13, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 200 40] /Resources << /Font << /F1 20 0 R >> /XObject << /Fm 31 0 R >> >>", b"BT /F1 12 Tf 2 5 Td (SECRET-) Tj ET /Fm Do")),
        (31, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 200 40]", b"BT 60 5 Td (ALPHA) Tj ET")),
        (20, helv.clone()),
    ]);
    let p = verify_secret(&build(&objs, "", b""));
    expect(&mut red, !p.is_clean() && decoded_text_hit(&p, 13), format!("split across Do: {:?}", p.hits));
    // The text-field pass reads the split run too and drops the appearance.
    let mut doc = open(&split_diff);
    let out = redact_text_fields_and_save(&mut doc, &["SECRET-ALPHA".to_string()], &TextFieldPolicy::default(), &token()).unwrap();
    expect(&mut red, rows(&out.report, TextRole::Appearance) == vec![(5, false, TextOutcomeKind::Dropped, 1)], format!("text fields: {:?}", out.report.outcomes));
    expect(&mut red, out.is_proven(), format!("text fields: {:?}", out.not_proven_reasons()));
    // The whole-token rule exempts only what the drawn reading proves: a match inside ONE string
    // glued to a letter of that string is another token (clean); a match that spans strings is never
    // suppressed, whatever letter precedes it (a hit).
    let p = verify_secret(&r8_ap_doc(helv.clone(), "BT /F1 12 Tf 2 5 Td (SECRET-ALPHAx) Tj ET", vec![]));
    assert!(p.is_clean(), "{:?}", p.hits);
    let p = verify_secret(&r8_ap_doc(helv, "BT /F1 12 Tf 2 5 Td (xSECRET-) Tj (ALPHA) Tj ET", vec![]));
    expect(&mut red, decoded_text_hit(&p, 13), format!("a concatenated match was suppressed: {:?}", p.hits));
    assert!(red.is_empty(), "{red:#?}");
}

/// R9-F3 (kerning-number word space): pdfTeX-style text writes no space glyphs; the needle SECRET ALPHA
/// removed from the region is drawn elsewhere as [(SECRET) -250 (ALPHA)] TJ in Helvetica (bytes equal
/// to the text). The spaced reading finds it. At 49731a5 the per-string spaced reading existed only
/// when it differed from the bytes, so the appearance read as clean and the result as proven.
#[test]
fn kerning_word_spaces_are_read() {
    let page = "BT /F2 12 Tf 72 700 Td (SECRET ALPHA) Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET";
    let out = r7_run(&r7_font_doc(page, None, Some("BT /F2 12 Tf 2 5 Td [(SECRET) -250 (ALPHA)] TJ ET"), "", vec![]));
    assert_eq!(out.reports[0].removed_text, vec![b"SECRET ALPHA".to_vec()]);
    assert!(!out.is_proven() && decoded_text_hit(&out.proof, 26), "kerning as the word space: {:?}", out.proof.hits);
    // Control: a different second word.
    let out = r7_run(&r7_font_doc(page, None, Some("BT /F2 12 Tf 2 5 Td [(SECRET) -250 (BETA)] TJ ET"), "", vec![]));
    assert!(out.is_proven(), "{:?}", out.proof.hits);
}

fn r9_outline() -> Vec<(u32, Vec<u8>)> {
    vec![
        (40, b"<< /Type /Outlines /First 41 0 R /Last 41 0 R /Count 1 >>".to_vec()),
        (41, b"<< /Title (SECRET-ALPHA plan) /Parent 40 0 R >>".to_vec()),
    ]
}

/// R9-F3 (removal side): the region draws `(SECRE) Tj (T-ALPHA) Tj`; consecutive removed strings of one
/// text object are recorded joined, so the outline twin SECRET-ALPHA is a leak and a text policy
/// redacts it. At 49731a5 only T-ALPHA was distinctive, the token rule kept it from matching inside
/// SECRET-ALPHA, and the result read as proven with the title intact.
#[test]
fn removed_runs_join_across_show_operators() {
    let pieces = "BT /F2 12 Tf 72 700 Td (SECRE) Tj (T-ALPHA) Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET";
    let pdf = r8_doc(pieces, "", "/Outlines 40 0 R", r9_outline());
    let out = r7_run(&pdf);
    assert!(!out.is_proven() && out.proof.hits.iter().any(|h| h.location == PayloadLocation::StringObject { object: 41 }), "{:?}", out.proof.hits);
    assert_eq!((out.reports[0].text_strings_removed, out.reports[0].removed_text.last()), (2, Some(&b"SECRET-ALPHA".to_vec())));
    // The same pieces in two text objects (one BT per piece, as some producers write words).
    let split_bt = "BT /F2 12 Tf 72 700 Td (SECRE) Tj ET BT /F2 12 Tf 132 700 Td (T-ALPHA) Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET";
    let out = r7_run(&r8_doc(split_bt, "", "/Outlines 40 0 R", r9_outline()));
    assert!(!out.is_proven() && out.proof.hits.iter().any(|h| h.location == PayloadLocation::StringObject { object: 41 }), "two text objects: {:?}", out.proof.hits);
    let mut doc = open(&pdf);
    let with = redact_and_save_with(&mut doc, &[R7_REGION], UnsupportedPolicy::Refuse, Some(&TextFieldPolicy::default()), &token()).unwrap();
    assert!(with.is_proven() && !leaks(&with.bytes, "secret-alpha"), "{:?}", with.not_proven_reasons());
    // Control: a run of short pieces of another word is proven.
    let out = r7_run(&r8_doc("BT /F2 12 Tf 72 700 Td (PUB) Tj (LIC) Tj ET", "", "/Outlines 40 0 R", r9_outline()));
    assert!(out.is_proven(), "{:?}", out.not_proven_reasons());
}

/// R9-F3 (one glyph per operator, unreadable): the region draws SECRET-ALPHA one 2-byte code per `Tj`
/// with a composite font that has no /ToUnicode (font 23, 4 pt so all twelve glyphs lie in the
/// region). No removed string can be read, whatever its length. At 49731a5 strings under 6 bytes were
/// not judged, nothing was searched, and the result read as proven with SECRET-ALPHA in the outline.
#[test]
fn glyph_by_glyph_unreadable_text_is_never_proven() {
    let glyphs: String = (1..=12).map(|i| format!("<01{i:02X}> Tj ")).collect();
    let pdf = r8_doc(&format!("BT /F3 4 Tf 72 700 Td {glyphs}ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET"), "", "/Outlines 40 0 R", r9_outline());
    let out = r7_run(&pdf);
    assert!(!out.is_proven(), "glyph-by-glyph unreadable removed text read as proven");
    assert!(out.not_proven_reasons().contains(&NotProven::UnreadableRemovedText), "{:?}", out.not_proven_reasons());
    assert_eq!(out.reports[0].text_strings_removed, 12);
    assert!(out.unreadable_removed >= 12 && out.reports[0].removed_unreadable.iter().all(|u| *u), "{:?}", out.reports[0].removed_unreadable);
}

/// R9-P1: Type 3 glyph procedures of a font dictionary written directly into a resource dictionary
/// are content, and no dictionary key (/Type /Metadata, /Subtype /XML) keeps a stream that tokenizes
/// from being read. At 49731a5 such a glyph procedure was never walked: text it drew with an escaped
/// literal was found by no spelling, and the result read as proven.
#[test]
fn direct_type3_glyph_procedures_are_read() {
    let mut red = Vec::new();
    let t3 = "/T3 << /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << /a 40 0 R >> /Encoding << /Differences [97 /a] >> /FirstChar 97 /LastChar 97 /Widths [500] /Resources << /Font << /F1 6 0 R >> >> >>";
    let glyph = (40, stream_body("/Type /Metadata /Subtype /XML", br"0 0 d0 BT /F1 12 Tf (SEC\122ET-ALPHA) Tj ET"));
    let out = r7_run(&r8_doc(&r8_page(""), t3, "", vec![glyph.clone()]));
    // Independent oracle: the source holds no plain spelling, only the escaped literal; the saved glyph
    // procedure is the canonical rewrite of the same operator (lead decision 2026-10-11T15:00:20Z).
    assert!(find(&glyph.1, b"SECRET-ALPHA").is_none() && find(&glyph.1, br"SEC\122ET-ALPHA").is_some());
    assert!(independent_streams(&out.bytes).iter().any(|s| find(s, b"(SECRET-ALPHA) Tj").is_some()));
    expect(&mut red, !out.is_proven() && decoded_text_hit(&out.proof, 40), format!("direct Type 3 glyph procedure not read: {:?}", out.proof.hits));
    // A stream nothing references as content, with misleading keys, drawing text with a font it cannot
    // resolve: read (stray), so its text is opaque.
    let hidden = (32, stream_body("/Type /Metadata /Subtype /XML", format!("BT /F1 12 Tf 72 300 Td {R7_CODES} Tj ET").as_bytes()));
    let out = r7_run(&r8_doc(&r8_page(""), "", "/Hidden 32 0 R", vec![hidden]));
    expect(&mut red, !out.is_proven() && out.proof.opaque_text_streams == vec![32], format!("keyed stray stream: {:?}", out.not_proven_reasons()));
    // Control: the same direct Type 3 font whose glyph procedure draws only a path is proven.
    let glyph = (40, stream_body("/Type /Metadata /Subtype /XML", b"500 0 0 0 500 500 d1 0 0 500 500 re f"));
    let out = r7_run(&r8_doc(&r8_page(""), t3, "", vec![glyph]));
    assert!(out.is_proven(), "{:?}", out.not_proven_reasons());
    assert!(red.is_empty(), "{red:#?}");
}

fn r9_titled(title: &str) -> Vec<(u32, Vec<u8>)> {
    vec![
        (40, b"<< /Type /Outlines /First 41 0 R /Last 41 0 R /Count 1 >>".to_vec()),
        (41, format!("<< /Title {title} /Parent 40 0 R >>").into_bytes()),
    ]
}

/// The proof finds the removed SECRET-ALPHA in outline title `title` (a leak) and a text policy
/// redacts it (proven): `red` records each failure for `what`.
fn r9_title_found(red: &mut Vec<String>, what: &str, title: &str) {
    let pdf = r8_doc(&r8_page(""), "", "/Outlines 40 0 R", r9_titled(title));
    let out = r7_run(&pdf);
    expect(red, !out.is_proven() && out.proof.hits.iter().any(|h| h.location == PayloadLocation::StringObject { object: 41 }), format!("{what}: title not found: {:?}", out.proof.hits));
    let mut doc = open(&pdf);
    let with = redact_and_save_with(&mut doc, &[R7_REGION], UnsupportedPolicy::Refuse, Some(&TextFieldPolicy::default()), &token()).unwrap();
    expect(red, with.is_proven(), format!("{what}: text policy left it: {:?}", with.not_proven_reasons()));
    expect(red, rows(with.text_fields.as_ref().unwrap(), TextRole::OutlineTitle) == vec![(41, false, TextOutcomeKind::Replaced, 1)], format!("{what}: title outcome"));
}

/// R9-P2 (language escapes): ESC..ESC in a text string is a language mark (ISO 32000-2 7.9.2.2);
/// pdf.js drops it (also in PDFDocEncoding), so the bookmark panel shows SECRET-ALPHA plan. At 49731a5
/// neither the proof nor the text-field pass found the title.
#[test]
fn language_escapes_and_invisible_characters_never_hide_text() {
    let mut red = Vec::new();
    let utf16 = format!("<FEFF{}>", "SEC\u{1B}en\u{1B}RET-ALPHA plan".encode_utf16().map(|u| format!("{u:04X}")).collect::<String>());
    r9_title_found(&mut red, "PDFDocEncoding language escape", r"(SEC\033en\033RET-ALPHA plan)");
    r9_title_found(&mut red, "UTF-16BE language escape", &utf16);
    // Control: another word with the same escape is not found.
    let out = r7_run(&r8_doc(&r8_page(""), "", "/Outlines 40 0 R", r9_titled(r"(SEC\033en\033RET-BETA plan)")));
    assert!(out.is_proven(), "{:?}", out.proof.hits);
    assert!(red.is_empty(), "{red:#?}");
}

/// R9-P2 (zero-width needle): characters that draw nothing (zero-width space and joiners, soft hyphen,
/// word joiner) and compatibility forms (NFKC: fullwidth, mathematical letters) never hide a removed
/// string: in text strings, in drawn text (a /ToUnicode map that reads a zero-width space or ESC between
/// letters) and in XMP. At 49731a5 the proof matched exact characters only and read each as proven.
#[test]
fn zero_width_and_compatibility_characters_never_hide_text() {
    let mut red = Vec::new();
    r9_title_found(&mut red, "zero-width space", &u16hex("SEC\u{200B}RET-ALPHA plan"));
    r9_title_found(&mut red, "soft hyphen and word joiner", &u16hex("SE\u{AD}CRET-AL\u{2060}PHA plan"));
    r9_title_found(&mut red, "fullwidth letters", &u16hex("\u{FF33}\u{FF25}\u{FF23}\u{FF32}\u{FF25}\u{FF34}-\u{FF21}\u{FF2C}\u{FF30}\u{FF28}\u{FF21} plan"));
    r9_title_found(&mut red, "mathematical bold letters", &u16hex("\u{1D412}\u{1D404}\u{1D402}\u{1D411}\u{1D404}\u{1D413}-ALPHA plan"));
    // Drawn text: /ToUnicode reads code 0110 as a zero-width space and 0111 as ESC.
    let mut cmap = String::from("1 begincodespacerange <0000> <FFFF> endcodespacerange 14 beginbfchar\n");
    for (i, ch) in "SECRET-ALPHA".chars().enumerate() {
        cmap.push_str(&format!("<{:04X}> <{:04X}>\n", 0x0101 + i as u32, ch as u32));
    }
    cmap.push_str("<0110> <200B>\n<0111> <001B>\nendbfchar");
    let font = b"<< /Type /Font /Subtype /Type0 /BaseFont /T /Encoding /Identity-H /DescendantFonts [21 0 R] /ToUnicode 22 0 R >>".to_vec();
    let cid = vec![(21, b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /T >>".to_vec()), (22, stream_body("", cmap.as_bytes()))];
    for codes in ["<010101020103011001040105010601070108010901110110010A010B010C>", "<0101010201030111010401050106010701080109010A010B010C0111>"] {
        let p = verify_secret(&r8_ap_doc(font.clone(), &format!("BT /F1 12 Tf 2 5 Td {codes} Tj ET"), cid.clone()));
        expect(&mut red, decoded_text_hit(&p, 13), format!("{codes}: {:?}", p.hits));
    }
    // XMP: a zero-width joiner inside the text of a metadata stream.
    let xmp = (50, stream_body("/Type /Metadata /Subtype /XML", "<x:xmpmeta><dc:title>SECRET\u{200D}-ALPHA</dc:title></x:xmpmeta>".as_bytes()));
    let out = r7_run(&r8_doc(&r8_page(""), "", "/Metadata 50 0 R", vec![xmp]));
    expect(&mut red, out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedStream { object: 50, .. })), format!("XMP: {:?}", out.proof.hits));
    // Control: another word with the same disguise is not found.
    let out = r7_run(&r8_doc(&r8_page(""), "", "/Outlines 40 0 R", r9_titled(&u16hex("SEC\u{200B}RET-BETA plan"))));
    assert!(out.is_proven(), "{:?}", out.proof.hits);
    assert!(red.is_empty(), "{red:#?}");
}

/// V4-02 for the determination pass: the text every root draws is charged to `max_decoded_total`
/// characters (every reading kind, joined and spaced), so content of many long strings runs out of the
/// budget (aggregate_over_budget) instead of building unbounded text; within the default budget it reads.
#[test]
fn determination_text_is_bounded() {
    let long = "x".repeat(4000);
    let content: String = (0..20).map(|_| format!("BT /F1 12 Tf 72 300 Td ({long}) Tj ET\n")).collect();
    let pdf = build(&base_objs(content.as_bytes(), "<< /Title (T) >>"), "/Info 7 0 R", b"");
    let tight = Limits { max_decoded_total: 200_000, ..Limits::default() };
    assert_eq!(verify_payload_absent(&pdf, &[b"zzzzzz".to_vec()], &tight, &token()).unwrap_err(), PressError::InputLimit("text_runs"));
    let p = verify_payload_absent(&pdf, &[b"zzzzzz".to_vec()], &Limits::default(), &token()).unwrap();
    assert!(p.is_clean(), "{:?}", p.not_proven_reasons());
}

// --------------------------------- R10 pre-review remediation (CANONICAL REWRITE)
// Lead decision 2026-10-11T15:00:20Z, validation/R10-prereview-press.json R10-F1..F7, R10-D1, R10-P1,
// P2, P4: one test per item. Each collects every failing sub-case (`expect`) before it asserts, so the
// red run on cc3eaae names each one. Lines marked R10-new-api use API added by this remediation; the
// red run drops only those lines.

use hsk_studio_press::RedactedOutput;

/// A page with the R7 test fonts (`R7_RES`) drawing `page`, and a FreeText annotation 5 at
/// [300 100 400 140] (outside every R10 region) whose appearance 26 draws `ap` with the resources
/// `ap_res`; catalog entries `catalog`; `extra` objects.
fn r10_ap_doc(page: &str, ap: &str, ap_res: &str, catalog: &str, extra: Vec<(u32, Vec<u8>)>) -> Vec<u8> {
    let mut objs = base_objs(page.as_bytes(), "<< /Title (T) >>");
    objs[0].1 = format!("<< /Type /Catalog /Pages 2 0 R {catalog} >>").into_bytes();
    objs[2].1 = format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Annots [5 0 R] /Resources << {R7_RES} >> >>").into_bytes();
    objs.extend(r7_fonts());
    objs[4].1 = b"<< /Type /Annot /Subtype /FreeText /Rect [300 100 400 140] /Contents (label) /AP << /N 26 0 R >> /P 3 0 R >>".to_vec();
    objs.push((26, stream_body(&format!("/Type /XObject /Subtype /Form /BBox [0 0 100 40] /Resources << {ap_res} >>"), ap.as_bytes())));
    objs.extend(extra);
    build(&objs, "/Info 7 0 R", b"")
}

/// Records whether a redaction removed exactly `removed`, left no stream drawing `needle`, and reads
/// as proven.
fn r10_removed(red: &mut Vec<String>, what: &str, out: &Result<RedactedOutput, PressError>, removed: &[u8], needle: &[u8]) {
    match out {
        Ok(out) => {
            expect(red, out.reports[0].removed_text == vec![removed.to_vec()], format!("{what}: removed {:?}", out.reports[0].removed_text));
            expect(red, independent_streams(&out.bytes).iter().all(|s| find(s, needle).is_none()), format!("{what}: still drawn in the saved streams"));
            expect(red, out.is_proven(), format!("{what}: {:?}", out.not_proven_reasons()));
        }
        Err(e) => expect(red, false, format!("{what}: refused {e:?}")),
    }
}

/// R10-F1 (inline-image end): pdf.js and MuPDF end unfiltered inline-image data where the data ends
/// (pdf.js accepts `EI` right after it, parser.js `findDefaultInlineStreamEnd`, issue10388), so
/// `ID AEI` is a one-byte image followed by content. At cc3eaae the crate took the first `EI` preceded
/// by white space as the end, read `Q BT .. (SECRET-ALPHA) Tj ET` as image data, selected nothing and
/// read the region as cleared and proven while SECRET-ALPHA was drawn in it.
#[test]
fn inline_image_end_follows_the_data() {
    let mut red = Vec::new();
    let region = RedactionRegion { page_index: 0, rect: [90.0, 690.0, 200.0, 715.0] };
    let page = |tail: &str| format!("q 1 0 0 1 500 50 cm BI /W 1 /H 1 /BPC 8 /CS /G ID AEI Q BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET{tail}\nBT /F1 12 Tf 72 300 Td (Public) Tj ET");
    // The text after the image is drawn and removed; a comment holds the later `EI` the crate took.
    let pdf = r9_page_doc(&page(" % EI"), "/Font << /F1 6 0 R >>", vec![]);
    let out = r9_run(&pdf, region, UnsupportedPolicy::Refuse);
    r10_removed(&mut red, "text after an image ending in AEI", &out, b"SECRET-ALPHA", b"SECRET-ALPHA");
    // Independent oracle: no inline image survives the canonical rewrite (it is an image XObject).
    if let Ok(out) = &out {
        expect(&mut red, independent_streams(&out.bytes).iter().all(|s| find(s, b" ID").is_none()), "an inline image survived the canonical rewrite");
    }
    // A stray `EI` after the content is an unknown operator to the strict parser: refused, never read
    // as more image data.
    let stray = r9_page_doc(&page(" EI"), "/Font << /F1 6 0 R >>", vec![]);
    expect(&mut red, matches!(r9_run(&stray, region, UnsupportedPolicy::Refuse), Err(PressError::Malformed(_))), "stray EI not refused");
    // Guard: image data holding `EI` + space, which pdf.js's look-ahead may take as the end (then it
    // draws the string after it), is unreadable to the proof.
    let ap = "BT /F1 12 Tf 2 5 Td BI /W 20 /H 1 /BPC 8 /CS /G ID EI (SECRET-ALPHA) Tj EI ET";
    let p = verify_secret(&r8_ap_doc(b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(), ap, vec![]));
    expect(&mut red, !p.is_clean(), "an inline image whose end viewers may read differently read as clean");
    assert!(red.is_empty(), "{red:#?}");
}

/// R10-F2 (writing-mode spellings, Identity names): pdf.js (cmap.js parseWMode) and MuPDF
/// (pdf_parse_wmode) take ANY integer after the name /WMode, whatever operator follows; pdf.js replaces
/// an embedded identity CMap named /Identity-V or /Identity-H by the built-in one. At cc3eaae only
/// `/WMode v def` with exactly two operands set a mode, and the CMap's own name was never read: each
/// vertical case below read as horizontal, the region below the first glyph as cleared.
#[test]
fn wmode_spellings_and_identity_names_are_read_as_viewers_do() {
    let mut red = Vec::new();
    let named = |name: &str, program: &str| {
        (39, stream_body("/Type /CMap", format!("/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /{name} def {program} 1 begincodespacerange <0000> <FFFF> endcodespacerange 1 begincidrange <0000> <FFFF> 0 endcidrange endcmap end end").as_bytes()))
    };
    let vertical: Vec<(&str, Vec<u8>)> = vec![
        ("/WMode 1 pop", r9_cmap_doc("39 0 R", vec![r9_cmap("", "/WMode 1 pop")])),
        ("<< /WMode 1 >> def", r9_cmap_doc("39 0 R", vec![r9_cmap("", "/D << /WMode 1 >> def")])),
        ("[/WMode 1] def", r9_cmap_doc("39 0 R", vec![r9_cmap("", "/D [/WMode 1] def")])),
        ("/A /WMode 1 def", r9_cmap_doc("39 0 R", vec![r9_cmap("", "/A /WMode 1 def")])),
        ("/WMode 1 then begincodespacerange", r9_cmap_doc("39 0 R", vec![r9_cmap("", "/CMapName /X def /WMode 1")])),
        ("embedded /Identity-V with horizontal content", r9_cmap_doc("39 0 R", vec![named("Identity-V", "")])),
        ("embedded /Identity-H with /WMode 1 pop", r9_cmap_doc("39 0 R", vec![named("Identity-H", "/WMode 1 pop")])),
    ];
    for (what, pdf) in &vertical {
        let out = r9_run(pdf, R9_BELOW, UnsupportedPolicy::ReportAndContinue).unwrap();
        // Independent oracle: the run is still drawn (it starts at y 700 and runs down).
        assert!(independent_streams(&out.bytes).iter().any(|s| find(s, R7_CODES.as_bytes()).is_some()), "{what}");
        expect(&mut red, !out.region_cleared() && !out.is_proven(), format!("{what}: vertical text read as cleared"));
        expect(&mut red, out.reports[0].unredacted == vec![RedactionClass::VerticalText], format!("{what}: {:?}", out.reports[0].unredacted));
        expect(&mut red, r9_run(pdf, R9_BELOW, UnsupportedPolicy::Refuse) == Err(PressError::UnsupportedRedactionClass(RedactionClass::VerticalText)), format!("{what}: not refused"));
    }
    // Controls: /WMode 0 in any spelling, and an /Identity-H identity CMap, are horizontal.
    for (what, pdf) in [
        ("/WMode 0 pop", r9_cmap_doc("39 0 R", vec![r9_cmap("", "/WMode 0 pop")])),
        ("embedded /Identity-H", r9_cmap_doc("39 0 R", vec![named("Identity-H", "")])),
    ] {
        let out = r9_run(&pdf, R9_BELOW, UnsupportedPolicy::Refuse).unwrap();
        assert!(out.region_cleared() && out.is_proven(), "{what}: {:?}", out.not_proven_reasons());
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R10-F3 (indirect /Subtype): any object may be indirect (ISO 32000 7.3.10) and pdf.js dereferences
/// /Subtype (primitives.js Dict.get; evaluator.js paintXObject). At cc3eaae a form or image whose
/// /Subtype was a reference was not drawn by the selector or the page walk (the region read as cleared
/// with SECRET-ALPHA drawn in it, an image under the region was never marked) and a Type 0 font with an
/// indirect /Subtype read as a simple horizontal font.
#[test]
fn indirect_subtypes_are_resolved() {
    let mut red = Vec::new();
    // (a) A form whose /Subtype is an indirect /Form draws SECRET-ALPHA in the region: removed.
    let form = (28, stream_body("/Type /XObject /Subtype 30 0 R /BBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >>", b"BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET"));
    let pdf = r9_page_doc("q /Fm1 Do Q\nBT /F1 12 Tf 72 300 Td (Public) Tj ET", "/Font << /F1 6 0 R >> /XObject << /Fm1 28 0 R >>", vec![form, (30, b"/Form".to_vec())]);
    r10_removed(&mut red, "form with an indirect /Subtype", &r9_run(&pdf, R9_INSIDE, UnsupportedPolicy::Refuse), b"SECRET-ALPHA", b"SECRET-ALPHA");
    // (b) An image whose /Subtype is an indirect /Image placed on the region: an image under the region.
    let img = (29, stream_body("/Type /XObject /Subtype 31 0 R /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", &[0u8]));
    let pdf = r9_page_doc("q 50 0 0 50 120 690 cm /Im9 Do Q\nBT /F1 12 Tf 72 300 Td (Public) Tj ET", "/Font << /F1 6 0 R >> /XObject << /Im9 29 0 R >>", vec![img, (31, b"/Image".to_vec())]);
    expect(&mut red, r9_run(&pdf, R9_INSIDE, UnsupportedPolicy::Refuse) == Err(PressError::UnsupportedRedactionClass(RedactionClass::Image)), "image with an indirect /Subtype not marked");
    // (c) A Type 0 font whose /Subtype is an indirect /Type0, /Encoding /Identity-V, and a /ToUnicode
    // that names every byte (so a simple reading is complete): vertical text, never cleared.
    let tu = (33, stream_body("", b"1 begincodespacerange <00> <FF> endcodespacerange 1 beginbfrange <00> <FF> <0041> endbfrange"));
    let font = (37, b"<< /Type /Font /Subtype 32 0 R /BaseFont /T /Encoding /Identity-V /DescendantFonts [21 0 R] /ToUnicode 33 0 R >>".to_vec());
    let pdf = r8_doc(&format!("BT /V1 12 Tf 100 700 Td {R7_CODES} Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET"), "/V1 37 0 R", "", vec![font, tu, (32, b"/Type0".to_vec())]);
    let out = r9_run(&pdf, R9_BELOW, UnsupportedPolicy::ReportAndContinue).unwrap();
    expect(&mut red, !out.region_cleared() && out.reports[0].unredacted.contains(&RedactionClass::VerticalText), format!("Type 0 with an indirect /Subtype: {:?}", out.reports[0].unredacted));
    assert!(red.is_empty(), "{red:#?}");
}

/// The tricky composite font of R10-F4: Identity-H (2-byte codes) whose /ToUnicode also maps 1-byte
/// codes, so splitting by the /ToUnicode codespace (1 byte first) reads QZQZ.. while viewers read
/// 2-byte codes as SECRET-ALPHA; 1-byte codes 7E..75 also spell the letters, so the lowest code the
/// map gives for a letter is not the drawn one.
fn r10_tricky_font() -> Vec<(u32, Vec<u8>)> {
    let mut tu = String::from("2 begincodespacerange <00> <FF> <0000> <FFFF> endcodespacerange\n31 beginbfchar\n");
    for (i, ch) in "SECRTALPH-".chars().enumerate() {
        let code = match ch {
            'S' => 0x153,
            'E' => 0x145,
            'C' => 0x143,
            'R' => 0x152,
            'T' => 0x154,
            'A' => 0x141,
            'L' => 0x14C,
            'P' => 0x150,
            'H' => 0x148,
            _ => 0x12D,
        };
        tu.push_str(&format!("<{code:04X}> <{:04X}>\n<{:02X}> <005A>\n<{:02X}> <{:04X}>\n", ch as u32, code & 0xFF, 0x7E - i, ch as u32));
    }
    tu.push_str("<01> <0051>\nendbfchar");
    vec![
        (60, b"<< /Type /Font /Subtype /Type0 /BaseFont /T /Encoding /Identity-H /DescendantFonts [21 0 R] /ToUnicode 61 0 R >>".to_vec()),
        (61, stream_body("", tu.as_bytes())),
    ]
}

/// SECRET-ALPHA as 2-byte codes of the tricky font.
const R10_CODES: &str = "<015301450143015201450154012D0141014C015001480141>";

/// R10-F4 (codes split by /ToUnicode): viewers split a Type 0 string by the font's ENCODING CMap
/// (pdf.js fonts.js cMap.readCharCode; Identity-H = 2 bytes) and look each code up in /ToUnicode by its
/// value. At cc3eaae the walker and the selector split by the /ToUnicode map's own code lengths (the
/// shortest that has a mapping), read QZQZ.., positively determined: an appearance copy of the removed
/// SECRET-ALPHA, and an outline twin of SECRET-ALPHA removed in those codes, read as proven.
#[test]
fn composite_codes_follow_the_encoding_codespace() {
    let mut red = Vec::new();
    let ap = format!("BT /C1 12 Tf 2 5 Td {R10_CODES} Tj ET");
    let pdf = r10_ap_doc(&r8_page(""), &ap, "/Font << /C1 60 0 R >>", "", r10_tricky_font());
    let out = r7_run(&pdf);
    expect(&mut red, !out.is_proven() && decoded_text_hit(&out.proof, 26), format!("appearance copy in 2-byte codes not found: {:?}", out.proof.hits));
    // The removal side: the region draws the codes, the outline holds the text.
    let mut extra = r10_tricky_font();
    extra.extend(r9_outline());
    let pdf = r8_doc(&format!("BT /C1 12 Tf 72 700 Td {R10_CODES} Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET"), "/C1 60 0 R", "/Outlines 40 0 R", extra);
    let out = r7_run(&pdf);
    expect(&mut red, out.reports[0].removed_unicode.first() == Some(&Some("SECRET-ALPHA".to_string())), format!("removed reading {:?}", out.reports[0].removed_unicode));
    expect(&mut red, !out.is_proven() && out.proof.hits.iter().any(|h| h.location == PayloadLocation::StringObject { object: 41 }), format!("outline twin not found: {:?}", out.proof.hits));
    assert!(red.is_empty(), "{red:#?}");
}

/// R10-F5 (operand position): pdf.js shifts extra LEADING operands off a fixed-arity operator and
/// executes it with the last ones (evaluator.js EvaluatorPreprocessor.read); MuPDF keeps the last
/// string or name. At cc3eaae the crate used the FIRST: the selector boxed `(A)` while `SECRET-ALPHA`
/// was drawn across the region, entered /Fm0 while viewers drew /Fm1, and the walker read an appearance
/// through /F9 or /GS0 while viewers used /F1 or /GS1: each read as cleared, proven or clean. The strict
/// parser refuses an operator with extra operands.
#[test]
fn extra_operands_are_refused() {
    let mut red = Vec::new();
    for content in ["BT /F1 12 Tf 100 700 Td (A) (SECRET-ALPHA) Tj ET", "BT /F1 12 Tf 100 700 Td [(A)] [(SECRET-ALPHA)] TJ ET"] {
        let pdf = r9_page_doc(&format!("{content}\nBT /F1 12 Tf 72 300 Td (Public) Tj ET"), "/Font << /F1 6 0 R >>", vec![]);
        expect(&mut red, matches!(r9_run(&pdf, R9_INSIDE, UnsupportedPolicy::Refuse), Err(PressError::Malformed(_))), format!("{content}: not refused"));
    }
    let fm = |n: u32, body: &str| (n, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> >>", body.as_bytes()));
    let pdf = r9_page_doc(
        "q /Fm0 /Fm1 Do Q\nBT /F1 12 Tf 72 300 Td (Public) Tj ET",
        "/Font << /F1 6 0 R >> /XObject << /Fm0 28 0 R /Fm1 29 0 R >>",
        vec![fm(28, "q Q"), fm(29, "BT /F1 12 Tf 100 700 Td (SECRET-ALPHA) Tj ET")],
    );
    expect(&mut red, matches!(r9_run(&pdf, R9_INSIDE, UnsupportedPolicy::Refuse), Err(PressError::Malformed(_))), "/Fm0 /Fm1 Do: not refused");
    // An appearance read through the FIRST name draws Helvetica codes; viewers use the /Differences
    // font that spells SECRET-ALPHA (escaped literal codes: no code spelling occurs in the stream).
    let codes = r"(\001\002\003\004\002\005\006\007\010\011\012\007)";
    let res = "/Font << /F9 6 0 R /F1 20 0 R >> /ExtGState << /GS0 << /Font [6 0 R 12] >> /GS1 << /Font [20 0 R 12] >> >>";
    for (what, ap) in [("/F9 /F1 12 Tf", format!("BT /F9 /F1 12 Tf 2 5 Td {codes} Tj ET")), ("/GS0 /GS1 gs", format!("BT /GS0 /GS1 gs 2 5 Td {codes} Tj ET"))] {
        let pdf = r10_ap_doc(R7_PLAIN, &ap, res, "", vec![(20, diff_font(&format!("1 {DIFF_NAMES}"), ""))]);
        let p = verify_secret(&pdf);
        expect(&mut red, !p.is_clean(), format!("{what}: an appearance read through the first operand read as clean"));
        expect(&mut red, p.unreadable_content_streams.iter().any(|(n, _)| *n == 26), format!("{what}: {:?}", p.unreadable_content_streams));
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R10-F6 (composite widths, signed size): viewers advance a composite font's glyphs by /W and /DW
/// (default 1000) per glyph (pdf.js evaluator.js extractWidths) and draw a negative font size rotated
/// 180 degrees (ISO 32000-2 9.4.4 Trm). At cc3eaae the selector advanced 1000 units per BYTE (glyphs 8
/// to 10 of a /DW 3000 run lay beyond its box) and boxed negative-size text at -0.25..+0.95 em above
/// the baseline while it is drawn below: both regions read as cleared with the text drawn in them.
#[test]
fn composite_geometry_uses_w_dw_and_signed_size() {
    let mut red = Vec::new();
    let codes = "<000100020003000400050006000700080009000A>";
    let objs = vec![
        (50, b"<< /Type /Font /Subtype /Type0 /BaseFont /T /Encoding /Identity-H /DescendantFonts [52 0 R] /ToUnicode 51 0 R >>".to_vec()),
        (51, stream_body("", b"1 begincodespacerange <0000> <FFFF> endcodespacerange 1 beginbfrange <0001> <000A> <0061> endbfrange")),
        (52, b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /T /DW 3000 >>".to_vec()),
    ];
    let pdf = r9_page_doc(&format!("BT /C1 12 Tf 100 700 Td {codes} Tj ET\nBT /F1 12 Tf 72 300 Td (Public) Tj ET"), "/Font << /F1 6 0 R /C1 50 0 R >>", objs);
    // Glyphs 8..10 are drawn at x 352, 388, 424.
    let region = RedactionRegion { page_index: 0, rect: [350.0, 690.0, 450.0, 715.0] };
    let decoded: Vec<u8> = (1u8..=10).flat_map(|c| [0, c]).collect();
    r10_removed(&mut red, "/DW 3000 run", &r9_run(&pdf, region, UnsupportedPolicy::Refuse), &decoded, codes.as_bytes());
    // Negative font size: Helvetica caps drawn from y 700 DOWN to about 691 and leftwards from x 100.
    let pdf = r9_page_doc("BT /F1 -12 Tf 100 700 Td (SECRET-ALPHA) Tj ET\nBT /F1 12 Tf 72 300 Td (Public) Tj ET", "/Font << /F1 6 0 R >>", vec![]);
    let region = RedactionRegion { page_index: 0, rect: [20.0, 689.0, 90.0, 696.0] };
    r10_removed(&mut red, "negative font size", &r9_run(&pdf, region, UnsupportedPolicy::Refuse), b"SECRET-ALPHA", b"SECRET-ALPHA");
    assert!(red.is_empty(), "{red:#?}");
}

/// R10-F6 (standard 14 metrics): a standard font without /Widths advances by the widths of Adobe's
/// AFM metrics (Helvetica `at` 1015, Symbol `arrowboth` 1042: more than the 1000 units per byte
/// cc3eaae assumed as an "upper bound"), and its glyph ink reaches beyond the advance (the glyph box).
/// At cc3eaae a region over the end of such a run read as cleared with the glyphs drawn in it.
#[test]
fn standard_fonts_use_their_afm_widths() {
    let mut red = Vec::new();
    // Twenty @ at 10 pt from x 100: the last glyph is inked to x 301.5 (AFM box of `at` 147..868).
    let ats = "@".repeat(20);
    let pdf = r9_page_doc(&format!("BT /F1 10 Tf 100 700 Td ({ats}) Tj ET\nBT /F1 12 Tf 72 300 Td (Public) Tj ET"), "/Font << /F1 6 0 R >>", vec![]);
    let region = RedactionRegion { page_index: 0, rect: [300.5, 690.0, 301.0, 715.0] };
    r10_removed(&mut red, "Helvetica @", &r9_run(&pdf, region, UnsupportedPolicy::Refuse), ats.as_bytes(), ats.as_bytes());
    // Thirty Symbol arrowboth (code 0xAB) at 10 pt: the last glyph is inked to x 412.
    let arrows = r"(\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253\253)";
    let symbol = (60, b"<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>".to_vec());
    let pdf = r9_page_doc(&format!("BT /S1 10 Tf 100 700 Td {arrows} Tj ET\nBT /F1 12 Tf 72 300 Td (Public) Tj ET"), "/Font << /F1 6 0 R /S1 60 0 R >>", vec![symbol]);
    let region = RedactionRegion { page_index: 0, rect: [405.0, 690.0, 410.0, 715.0] };
    match r9_run(&pdf, region, UnsupportedPolicy::Refuse) {
        Ok(out) => {
            expect(&mut red, out.reports[0].text_strings_removed == 1, "Symbol arrowboth run not removed");
            expect(&mut red, out.region_cleared(), format!("Symbol: {:?}", out.not_proven_reasons()));
        }
        Err(e) => expect(&mut red, false, format!("Symbol: refused {e:?}")),
    }
    assert!(red.is_empty(), "{red:#?}");
}

/// R10-F7 (ASCII streams): a removed string drawn with a ligature glyph (/Differences [128 /fi]) has the
/// needle text con\u{FB01}dential plan; its normalised form is found in an XMP packet only if the packet
/// is matched as normalised text. At cc3eaae streams of printable ASCII were matched by byte spellings
/// only, so the XMP twin read as proven.
#[test]
fn ascii_streams_are_matched_as_text() {
    let mut red = Vec::new();
    let lig = (60, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding << /Differences [128 /fi] >> >>".to_vec());
    let page = "BT /F5 12 Tf 72 700 Td (con\\200dential plan) Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET";
    let xmp = |words: &str| format!("<x:xmpmeta><dc:description>{words}</dc:description></x:xmpmeta>").into_bytes();
    for (what, body) in [
        ("uncompressed", stream_body("/Type /Metadata /Subtype /XML", &xmp("confidential plan"))),
        ("Flate", stream_body("/Type /Metadata /Subtype /XML /Filter /FlateDecode", &zl(&xmp("confidential plan")))),
    ] {
        let out = r7_run(&r8_doc(page, "/F5 60 0 R", "/Metadata 50 0 R", vec![lig.clone(), (50, body)]));
        expect(&mut red, out.reports[0].removed_unicode == vec![Some("con\u{FB01}dential plan".to_string())], format!("{what}: {:?}", out.reports[0].removed_unicode));
        expect(&mut red, !out.is_proven() && out.proof.hits.iter().any(|h| matches!(h.location, PayloadLocation::DecodedStream { object: 50, .. })), format!("{what}: XMP twin not found: {:?}", out.proof.hits));
    }
    // Control: other words in the packet.
    let out = r7_run(&r8_doc(page, "/F5 60 0 R", "/Metadata 50 0 R", vec![lig, (50, stream_body("/Type /Metadata /Subtype /XML", &xmp("confidential memo")))]));
    assert!(out.is_proven(), "{:?}", out.proof.hits);
    assert!(red.is_empty(), "{red:#?}");
}

/// R10-D1 (punctuation remaps): a /Differences or base-encoding remap of a quote or dash draws other
/// characters than the bytes spell (TeX fonts: 0x27 quoteright, OT1 0x7B endash). At cc3eaae the needle
/// was the BYTE text whenever the letters and digits agreed, so an outline twin written with the drawn
/// characters (hyperref writes U+2019, U+2013) read as proven.
#[test]
fn remapped_punctuation_is_searched_as_drawn() {
    let mut red = Vec::new();
    let tex = |differences: &str| (60, format!("<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman /Encoding << /BaseEncoding /WinAnsiEncoding /Differences [{differences}] >> >>").into_bytes());
    for (what, diff, shown, title) in [
        ("quoteright", "39 /quoteright", "(Smith's account)", "Smith\u{2019}s account"),
        ("endash", "123 /endash", "(1990{1995 audit)", "1990\u{2013}1995 audit"),
    ] {
        let page = format!("BT /F5 12 Tf 72 700 Td {shown} Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET");
        let mut extra = r9_titled(&u16hex(title));
        extra.push(tex(diff));
        let pdf = r8_doc(&page, "/F5 60 0 R", "/Outlines 40 0 R", extra);
        let out = r7_run(&pdf);
        expect(&mut red, !out.is_proven() && out.proof.hits.iter().any(|h| h.location == PayloadLocation::StringObject { object: 41 }), format!("{what}: drawn twin not found: {:?}", out.proof.hits));
        let mut doc = open(&pdf);
        let with = redact_and_save_with(&mut doc, &[R7_REGION], UnsupportedPolicy::Refuse, Some(&TextFieldPolicy::default()), &token()).unwrap();
        expect(&mut red, with.is_proven(), format!("{what}: text policy left it: {:?}", with.not_proven_reasons()));
    }
    // Control: the byte spelling of the same words is still found.
    let mut extra = r9_titled("(Smith's account)");
    extra.push(tex("39 /quoteright"));
    let out = r7_run(&r8_doc("BT /F5 12 Tf 72 700 Td (Smith's account) Tj ET\nBT /F2 12 Tf 72 300 Td (Public) Tj ET", "/F5 60 0 R", "/Outlines 40 0 R", extra));
    assert!(out.proof.hits.iter().any(|h| h.location == PayloadLocation::StringObject { object: 41 }), "{:?}", out.proof.hits);
    assert!(red.is_empty(), "{red:#?}");
}

/// R10-P1 and R10-P2: every Unicode Default_Ignorable_Code_Point is dropped for matching (Khmer
/// inherent vowels, shorthand format controls, Hangul fillers), and text under a right-to-left override
/// is matched in the order a bidi viewer shows it. At cc3eaae each title below read as proven.
#[test]
fn default_ignorables_and_overrides_never_hide_text() {
    let mut red = Vec::new();
    r9_title_found(&mut red, "Khmer inherent vowel U+17B4", &u16hex("SEC\u{17B4}RET-ALPHA plan"));
    r9_title_found(&mut red, "shorthand format control U+1BCA0", &u16hex("SECRET\u{1BCA0}-ALPHA plan"));
    r9_title_found(&mut red, "Hangul filler U+3164", &u16hex("SECRET-\u{3164}ALPHA plan"));
    r9_title_found(&mut red, "right-to-left override", &u16hex("\u{202E}AHPLA-TERCES\u{202C} plan"));
    // Control: another word under an override is not found.
    let out = r7_run(&r8_doc(&r8_page(""), "", "/Outlines 40 0 R", r9_titled(&u16hex("\u{202E}ATEB-TERCES\u{202C} plan"))));
    assert!(out.is_proven(), "{:?}", out.proof.hits);
    assert!(red.is_empty(), "{red:#?}");
}

/// R10-P4 (token exemption): the whole-token rule may exempt a match glued to a letter only when that
/// letter is drawn next to it. A /Widths entry of 50000 for `x` draws it 600 pt away (clipped) and
/// SECRET-ALPHA visibly alone; at cc3eaae the reading `xSECRET-ALPHA` exempted the match and the
/// appearance read as clean.
#[test]
fn token_exemption_needs_drawn_adjacency() {
    let font = |x: u32| {
        let widths: Vec<String> = (45..=120).map(|c| if c == 120 { x.to_string() } else { "600".to_string() }).collect();
        format!("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 45 /LastChar 120 /Widths [{}] >>", widths.join(" ")).into_bytes()
    };
    let far = verify_secret(&r8_ap_doc(font(50000), "BT /F1 12 Tf -598 5 Td (xSECRET-ALPHA) Tj ET", vec![]));
    assert!(!far.is_clean() && decoded_text_hit(&far, 13), "a glued letter drawn 600 pt away exempted the match: {:?}", far.hits);
    // Control: drawn next to it, the letter makes another word (xSECRET-ALPHA): exempt.
    let near = verify_secret(&r8_ap_doc(font(500), "BT /F1 12 Tf 2 5 Td (xSECRET-ALPHA) Tj ET", vec![]));
    assert!(near.is_clean(), "{:?}", near.hits);
}

/// Lead decision 2026-10-11T15:00:20Z: the redaction saves only content this crate wrote from its own
/// parse. Over fixtures of every kind (kerned runs, forms, Type 3 glyph procedures, appearances,
/// marked content, inline images, composite fonts) every content stream of the saved file is the
/// canonical serialisation of its parse; a content stream the rewrite has to leave as it was keeps the
/// result unproven (NotProven::NonCanonicalContent).
#[test]
fn canonical_rewrite_keeps_only_written_content() {
    let fixtures: Vec<(&str, Vec<u8>)> = vec![
        ("base", build(&base_objs(CONTENT.as_bytes(), "<< /Title (T) >>"), "/Info 7 0 R", b"")),
        ("marked content and paths", r8_doc(&r8_page("/Span << /ActualText (x) >> BDC q 1 0 0 1 5 5 cm 0.5 g 10 10 50 50 re f Q EMC % comment"), "", "", vec![])),
        ("composite appearance", r7_font_doc(R7_PLAIN, None, Some("BT /F1 12 Tf 2 5 Td <0101> Tj ET"), "", vec![])),
        ("inline image", r9_page_doc("q 10 0 0 10 400 100 cm BI /W 2 /H 1 /BPC 8 /CS /G /F /AHx ID 4142> EI Q\nBT /F1 12 Tf 72 700 Td (SECRET-ALPHA) Tj ET", "/Font << /F1 6 0 R >>", vec![])),
    ];
    for (what, pdf) in &fixtures {
        let out = r7_run(pdf);
        assert!(out.proof.non_canonical_streams.is_empty(), "{what}: {:?}", out.proof.non_canonical_streams); // R10-new-api
        // Independent oracle: no saved content stream holds a comment or an inline image.
        assert!(independent_streams(&out.bytes).iter().all(|s| find(s, b"% comment").is_none() && find(s, b" ID ").is_none()), "{what}");
    }
    // A form without resources that draws an inline image AND names a font cannot take an image
    // XObject without changing what its names mean: left as it was, never proven.
    let ap = "BT /F1 12 Tf 2 5 Td (label) Tj ET BI /W 1 /H 1 /BPC 8 /CS /G ID A EI";
    let mut objs = base_objs(r7_page().as_bytes(), "<< /Title (T) >>");
    objs[0].1 = b"<< /Type /Catalog /Pages 2 0 R /AcroForm << /DR << /Font << /F1 6 0 R >> >> >> >>".to_vec();
    objs[4].1 = b"<< /Type /Annot /Subtype /FreeText /Rect [300 100 400 140] /Contents (label) /AP << /N 26 0 R >> /P 3 0 R >>".to_vec();
    objs.push((26, stream_body("/Type /XObject /Subtype /Form /BBox [0 0 100 40]", ap.as_bytes())));
    let out = r7_run(&build(&objs, "/Info 7 0 R", b""));
    assert!(!out.is_proven() && out.not_proven_reasons().contains(&NotProven::NonCanonicalContent), "{:?}", out.not_proven_reasons()); // R10-new-api
    assert_eq!(out.proof.non_canonical_streams, vec![26]); // R10-new-api
}
