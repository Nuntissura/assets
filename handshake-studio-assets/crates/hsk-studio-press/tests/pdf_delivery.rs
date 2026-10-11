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

fn open(bytes: &[u8]) -> PdfDoc {
    PdfDoc::open(bytes, &Limits::default(), &token()).expect("open")
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
    assert!(report.complete && report.layout_shift_possible, "standard font without /Widths: shift honestly reported");
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
    let objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
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
    let objs: Vec<(u32, Vec<u8>)> = vec![
        (1, b"<< /Type /Catalog /Pages 2 0 R /Outlines 10 0 R >>".to_vec()),
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

    // Every other codec of the class is listed under its own name and does not block.
    for codec in ["DCTDecode", "JPXDecode", "CCITTFaxDecode", "JBIG2Decode"] {
        let pdf = raster_doc(&format!("/Filter /{codec}"), b"codestream bytes", &outside);
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

    // Inline images: same class and same rules.
    let inline = |at: &str, filter: &str| {
        format!("{TEXT_IN_REGION}\nq 50 0 0 50 {at} cm BI /W 1 /H 1 /CS /G /BPC 8 /F /{filter} ID XXJPEGDATAXX\nEI Q")
    };
    let out = run(&raster_doc("", b"x", &inline("300 300", "DCT")), UnsupportedPolicy::Refuse).unwrap();
    assert!(out.proof.raster_streams.iter().any(|r| r.inline && r.filter == "DCT" && r.class == RASTER_CLASS));
    assert!(out.is_proven(), "{:?}", out.proof);
    let near = raster_doc("", b"x", &inline("80 660", "DCT"));
    assert_eq!(run(&near, UnsupportedPolicy::Refuse).unwrap_err(), PressError::UnsupportedRedactionClass(RedactionClass::InlineImage));
    let removed = run(&near, UnsupportedPolicy::RemoveImages).unwrap();
    assert_eq!(removed.reports[0].inline_images_removed, 1);
    assert!(removed.is_proven() && independent_streams(&removed.bytes).iter().all(|s| find(s, b"XXJPEGDATAXX").is_none()));
    // An inline image with an unknown filter blocks, like a stream would.
    let out = run(&raster_doc("", b"x", &inline("300 300", "Foo")), UnsupportedPolicy::Refuse).unwrap();
    assert!(out.proof.streams_not_decodable.iter().any(|(_, f)| f == "inline:Foo") && !out.is_proven());
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
        ("u32 edge, valid", b"1 beginbfrange <FFFFFFFE> <FFFFFFFF> <FFFF> endbfrange".to_vec(), 0),
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

    // No text-needle search requested (the only removed string is shorter than the distinctive
    // minimum, so nothing was searched for): the unreadable text is listed but does not block.
    let out = run(&doc_with(cid_text, "(ab)"));
    assert_eq!((out.proof.needles_searched, out.short_needles), (0, 1));
    assert_eq!(out.proof.opaque_text_streams.len(), 1);
    assert!(out.is_proven() && out.region_cleared(), "{:?}", out.not_proven_reasons());

    // The text-field flow gives the same answer.
    let mut doc = open(&doc_with(cid_text, "(SECRET-ALPHA)"));
    let fields = redact_text_fields_and_save(&mut doc, &["SECRET-ALPHA".to_string()], &TextFieldPolicy::default(), &token()).unwrap();
    assert_eq!(fields.not_proven_reasons(), vec![NotProven::OpaqueText]);
    assert!(!fields.is_proven());
}
