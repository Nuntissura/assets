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
