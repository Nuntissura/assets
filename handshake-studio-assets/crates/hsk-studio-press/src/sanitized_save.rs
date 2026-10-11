//! Sanitized full save and the independent "full reachable payload" proof.
//!
//! `verify_payload_absent` re-opens the OUTPUT bytes with the bounded reader (it shares no state
//! with the redaction code) and looks for the removed strings in: every raw byte of the file, every
//! decoded stream (Flate/ASCIIHex/ASCII85/RunLength, object streams included), every PDF
//! string/name object (hex and escaped spellings compare by decoded value), every string operand of
//! decoded content streams, plus the UTF-16BE spelling of ASCII needles. It also reports
//! incremental sections, objects present in the cross-reference table but unreachable from the
//! trailer, and `endobj` markers with no cross-reference entry (orphans an exporter left behind).
//! Streams whose filter is not implemented (DCT, JPX, CCITT, JBIG2, LZW, Crypt) can only be scanned
//! raw and are listed in `streams_not_decodable`; the proof never claims more than it scanned.
use crate::content::parse_content;
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::pdf_edit::PdfDoc;
use crate::reader::{check_cancel, find, parse_document};
use crate::redaction::{RedactionRegion, RedactionReport, UnsupportedPolicy};
use crate::syntax::{Dict, Object};
use crate::writer::{reachable, trailer_roots, write_document};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};

const MAX_HITS: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PayloadLocation {
    RawBytes { offset: usize },
    DecodedStream { object: u32, offset: usize },
    StringObject { object: u32 },
    ContentString { object: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadHit {
    /// Index into the needle list passed to the verifier.
    pub needle: usize,
    pub location: PayloadLocation,
}

/// A needle found as (part of) a text-showing operand of a decoded content stream: the same text
/// legitimately occurs in kept content. Reported separately; never counted as a leak.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextCollision {
    pub needle: usize,
    pub object: u32,
    pub count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayloadProof {
    pub raw_bytes_scanned: usize,
    pub objects_checked: u32,
    pub streams_decoded: u32,
    /// Streams scanned raw only: `(object number, filter name or reason)`.
    pub streams_not_decodable: Vec<(u32, String)>,
    /// Cross-reference sections in the output (1 = fresh full save, no incremental tail).
    pub xref_sections: u32,
    /// Objects in the cross-reference table that are unreachable from the trailer.
    pub unreachable_objects: Vec<u32>,
    /// `endobj` markers beyond the objects the cross-reference table lists (orphan bodies).
    pub unlisted_object_markers: u32,
    /// Leaks: needle found outside kept text-showing operands (see `verify_payload_absent`).
    pub hits: Vec<PayloadHit>,
    /// Needle also present as kept text-showing operand text (not a leak).
    pub kept_text_collisions: Vec<TextCollision>,
}

impl PayloadProof {
    /// No needle found anywhere scanned, a single xref section, no unreachable or unlisted
    /// objects.
    pub fn is_clean(&self) -> bool {
        self.hits.is_empty() && self.xref_sections <= 1 && self.unreachable_objects.is_empty() && self.unlisted_object_markers == 0
    }
    /// Every stream was decoded and scanned (otherwise see `streams_not_decodable`).
    pub fn fully_decoded(&self) -> bool {
        self.streams_not_decodable.is_empty()
    }
}

fn variants(needles: &[Vec<u8>]) -> Vec<(usize, Vec<u8>)> {
    let mut out = Vec::new();
    for (i, n) in needles.iter().enumerate() {
        if n.is_empty() {
            continue;
        }
        out.push((i, n.clone()));
        if n.is_ascii() {
            let mut u = Vec::with_capacity(n.len() * 2);
            for b in n {
                u.extend_from_slice(&[0, *b]);
            }
            out.push((i, u));
        }
    }
    out
}

fn scan(hay: &[u8], vars: &[(usize, Vec<u8>)]) -> Vec<(usize, usize)> {
    let mut hits = Vec::new();
    for (i, v) in vars {
        let mut from = 0usize;
        while let Some(at) = find(&hay[from..], v) {
            hits.push((*i, from + at));
            from += at + 1;
            if hits.len() >= MAX_HITS {
                return hits;
            }
        }
    }
    hits
}

fn contains(s: &[u8], vars: &[(usize, Vec<u8>)]) -> Option<usize> {
    vars.iter().find(|(_, v)| find(s, v).is_some()).map(|(i, _)| *i)
}

fn looks_like_content(d: &Dict) -> bool {
    let sub_ok = match d.get("Subtype") {
        None => true,
        Some(s) => s.is_name("Form"),
    };
    let type_ok = !matches!(d.get("Type").and_then(Object::as_name), Some(b"XRef" | b"ObjStm" | b"Metadata" | b"EmbeddedFile"));
    sub_ok && type_ok
}

/// Scans the saved bytes for `needles` (see module docs). Needles are raw string bytes; empty
/// needles are ignored.
///
/// Context rule (what makes the proof usable for natural-language documents without weakening
/// it): in decoded page/form content streams a needle that appears as (part of) a TEXT-SHOWING
/// operand (Tj, TJ, ', ") is a kept-text collision, reported in `kept_text_collisions`, because
/// the same word legitimately occurs in kept text (the in-region instance is proven gone by the
/// residual pass in [`redact_and_save`]). A needle anywhere else is a leak and lands in `hits`:
/// raw bytes outside decoded streams (headers, object text, xref, orphan bodies), string/name
/// objects (metadata, annotation contents, outlines, ActualText), non-show content operands
/// (marked-content properties), inline-image data, comments, and every decoded non-content stream
/// (XMP, embedded files, thumbnails, private data, object streams).
pub fn verify_payload_absent(
    output: &[u8],
    needles: &[Vec<u8>],
    limits: &Limits,
    cancel: &CancellationToken,
) -> Result<PayloadProof> {
    let vars = variants(needles);
    let parsed = parse_document(output, limits, cancel)?;
    let mut proof = PayloadProof {
        raw_bytes_scanned: output.len(),
        objects_checked: 0,
        streams_decoded: 0,
        streams_not_decodable: Vec::new(),
        xref_sections: parsed.xref_sections as u32,
        unreachable_objects: Vec::new(),
        unlisted_object_markers: 0,
        hits: Vec::new(),
        kept_text_collisions: Vec::new(),
    };
    let raw_hits = scan(output, &vars);
    let live = reachable(&parsed.objects, &trailer_roots(&parsed.trailer));
    proof.unreachable_objects = parsed.objects.keys().copied().filter(|n| !live.contains(n)).collect();
    let markers = output.windows(6).filter(|w| *w == b"endobj").count();
    let listed = parsed.objects.values().filter(|e| e.span.is_some()).count();
    proof.unlisted_object_markers = markers.saturating_sub(listed) as u32;

    // Raw spans of streams that decode successfully: their bytes are judged in decoded context.
    let mut covered: Vec<(usize, usize)> = Vec::new();
    let mut collisions: BTreeMap<(usize, u32), u32> = BTreeMap::new();
    let mut budget = limits.max_decoded_total;
    for (i, (num, entry)) in parsed.objects.iter().enumerate() {
        if i % 64 == 0 {
            check_cancel(cancel)?;
        }
        proof.objects_checked += 1;
        let mut stack: Vec<&Object> = vec![&entry.obj];
        while let Some(o) = stack.pop() {
            match o {
                Object::Str(s) | Object::Name(s) => {
                    if let Some(needle) = contains(s, &vars) {
                        proof.hits.push(PayloadHit { needle, location: PayloadLocation::StringObject { object: *num } });
                    }
                }
                Object::Array(a) => stack.extend(a.iter()),
                Object::Dict(d) => stack.extend(d.entries.iter().map(|(_, v)| v)),
                Object::Stream(s) => stack.extend(s.dict.entries.iter().map(|(_, v)| v)),
                _ => {}
            }
        }
        let Object::Stream(s) = &entry.obj else { continue };
        if !filters::is_decodable(&s.dict) {
            let name = s
                .dict
                .get("Filter")
                .map(|f| match f {
                    Object::Name(n) => String::from_utf8_lossy(n).into_owned(),
                    Object::Array(a) => a.iter().filter_map(|x| x.as_name().map(|n| String::from_utf8_lossy(n).into_owned())).collect::<Vec<_>>().join("+"),
                    _ => "unknown".into(),
                })
                .unwrap_or_default();
            proof.streams_not_decodable.push((*num, name));
            continue;
        }
        match filters::decode_stream(s, limits, &mut budget) {
            Ok(bytes) => {
                proof.streams_decoded += 1;
                if let Some(span) = entry.span {
                    covered.push(span);
                }
                let ops = if looks_like_content(&s.dict) { parse_content(&bytes, limits).ok() } else { None };
                let is_show = |name: &[u8]| matches!(name, b"Tj" | b"TJ" | b"'" | b"\"");
                let show_spans: Vec<(usize, usize)> =
                    ops.iter().flatten().filter(|op| is_show(&op.name)).map(|op| (op.start, op.end)).collect();
                for (needle, offset) in scan(&bytes, &vars) {
                    if !show_spans.iter().any(|(a, b)| offset >= *a && offset < *b) {
                        proof.hits.push(PayloadHit { needle, location: PayloadLocation::DecodedStream { object: *num, offset } });
                    }
                }
                for op in ops.iter().flatten() {
                    let show = is_show(&op.name);
                    let mut st: Vec<&Object> = op.operands.iter().collect();
                    while let Some(o) = st.pop() {
                        match o {
                            Object::Str(x) => {
                                if let Some(needle) = contains(x, &vars) {
                                    if show {
                                        *collisions.entry((needle, *num)).or_default() += 1;
                                    } else {
                                        proof.hits.push(PayloadHit { needle, location: PayloadLocation::ContentString { object: *num } });
                                    }
                                }
                            }
                            Object::Array(a) => st.extend(a.iter()),
                            Object::Dict(d) => st.extend(d.entries.iter().map(|(_, v)| v)),
                            _ => {}
                        }
                    }
                }
            }
            Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
            Err(_) => proof.streams_not_decodable.push((*num, "decode_error".into())),
        }
        if proof.hits.len() > MAX_HITS {
            proof.hits.truncate(MAX_HITS);
            break;
        }
    }
    for (needle, offset) in raw_hits {
        if !covered.iter().any(|(a, b)| offset >= *a && offset < *b) {
            proof.hits.push(PayloadHit { needle, location: PayloadLocation::RawBytes { offset } });
        }
    }
    proof.hits.truncate(MAX_HITS);
    proof.kept_text_collisions = collisions.into_iter().map(|((needle, object), count)| TextCollision { needle, object, count }).collect();
    Ok(proof)
}

/// Output of [`redact_and_save`]: the saved bytes, what each region removed, and the
/// independent proof over the saved bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct RedactedOutput {
    pub bytes: Vec<u8>,
    pub reports: Vec<RedactionReport>,
    /// Payload proof over the saved bytes for the distinctive removed strings.
    pub proof: PayloadProof,
    /// Removed strings shorter than [`MIN_DISTINCTIVE_NEEDLE`] bytes (typical for kerned words
    /// and subset-font glyph codes). A byte scan cannot prove those absent because they legitimately
    /// occur elsewhere, so they are NOT part of `proof`; `residual_in_region` covers them.
    pub short_needles: u32,
    /// Text strings, annotations and unsupported marks a fresh redaction pass over the SAVED
    /// bytes still finds in the regions (0 = the saved bytes hold nothing the region test selects).
    pub residual_in_region: u32,
}

impl RedactedOutput {
    /// Every region fully removed (nothing left in `unredacted`), the saved bytes pass the payload
    /// proof for every distinctive removed string, and a fresh pass over the saved bytes finds
    /// nothing left in the regions.
    pub fn is_proven(&self) -> bool {
        self.reports.iter().all(|r| r.complete) && self.proof.is_clean() && self.residual_in_region == 0
    }
}

/// Removed strings at least this long (bytes) are verified by byte scan.
pub const MIN_DISTINCTIVE_NEEDLE: usize = 6;

/// Redacts every region, full-saves, and verifies the saved bytes against the removed strings.
/// On error the document is restored to its state before the call.
pub fn redact_and_save(
    doc: &mut PdfDoc,
    regions: &[RedactionRegion],
    policy: UnsupportedPolicy,
    cancel: &CancellationToken,
) -> Result<RedactedOutput> {
    let backup = doc.objects.clone();
    let result = (|| -> Result<RedactedOutput> {
        let mut reports = Vec::with_capacity(regions.len());
        for region in regions {
            reports.push(doc.redact_region(region, policy, cancel)?);
        }
        let bytes = doc.save_full(cancel)?;
        let all: Vec<Vec<u8>> = reports.iter().flat_map(|r| r.removed_text.iter().cloned()).collect();
        let needles: Vec<Vec<u8>> = all.iter().filter(|n| n.len() >= MIN_DISTINCTIVE_NEEDLE).cloned().collect();
        let short_needles = (all.len() - needles.len()) as u32;
        let proof = verify_payload_absent(&bytes, &needles, &doc.limits, cancel)?;
        // Fresh re-open of the saved bytes: the region test must now select nothing.
        let mut saved = PdfDoc::open(&bytes, &doc.limits, cancel)?;
        let mut residual = 0u32;
        for region in regions {
            let again = saved.redact_region(region, UnsupportedPolicy::ReportAndContinue, cancel)?;
            residual = residual
                .saturating_add(again.text_strings_removed)
                .saturating_add(again.annotations_removed.len() as u32)
                .saturating_add(again.unredacted.len() as u32);
        }
        Ok(RedactedOutput { bytes, reports, proof, short_needles, residual_in_region: residual })
    })();
    if result.is_err() {
        doc.objects = backup;
    }
    result
}

/// Selective sanitization classes applied by [`sanitize_full_rewrite`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SanitizePolicy {
    pub drop_info_metadata: bool,
    pub drop_xmp: bool,
    pub drop_embedded_files: bool,
    pub drop_javascript: bool,
    /// Page and form `/Thumb` thumbnail images.
    pub drop_thumbnails: bool,
    /// Application private data: `/PieceInfo` on the catalog, pages and forms.
    pub drop_private_data: bool,
}

impl SanitizePolicy {
    pub const ALL: Self = Self {
        drop_info_metadata: true,
        drop_xmp: true,
        drop_embedded_files: true,
        drop_javascript: true,
        drop_thumbnails: true,
        drop_private_data: true,
    };
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SanitizeReport {
    /// `(class, count)` for every class that removed something.
    pub removed: Vec<(String, u32)>,
}

fn visit_dicts(o: &mut Object, f: &mut dyn FnMut(&mut Dict), depth: usize) {
    if depth > 64 {
        return;
    }
    match o {
        Object::Dict(d) => {
            f(d);
            for (_, v) in &mut d.entries {
                visit_dicts(v, f, depth + 1);
            }
        }
        Object::Stream(s) => {
            f(&mut s.dict);
            for (_, v) in &mut s.dict.entries {
                visit_dicts(v, f, depth + 1);
            }
        }
        Object::Array(a) => a.iter_mut().for_each(|v| visit_dicts(v, f, depth + 1)),
        _ => {}
    }
}

/// Full rewrite with the selected payload classes removed. The output holds only objects
/// reachable from the trailer after the removals (fresh file, no incremental tail, no orphans).
pub fn sanitize_full_rewrite(
    doc: &PdfDoc,
    policy: &SanitizePolicy,
    cancel: &CancellationToken,
) -> Result<(Vec<u8>, SanitizeReport)> {
    doc.guard_mutation(cancel)?;
    let mut staged = doc.objects.clone();
    let mut trailer = doc.trailer.clone();
    let mut counts: BTreeMap<&'static str, u32> = BTreeMap::new();
    if policy.drop_info_metadata && trailer.remove("Info").is_some() {
        *counts.entry("info_metadata").or_default() += 1;
    }
    let js_nums: BTreeSet<u32> = staged
        .iter()
        .filter(|(_, e)| e.obj.as_dict().and_then(|d| d.get("S")).is_some_and(|s| s.is_name("JavaScript")))
        .map(|(n, _)| *n)
        .collect();
    let is_js = |o: &Object| match o {
        Object::Ref(r) => js_nums.contains(&r.num),
        Object::Dict(d) => d.get("S").is_some_and(|s| s.is_name("JavaScript")),
        _ => false,
    };
    for entry in staged.values_mut() {
        let mut touched = false;
        visit_dicts(
            &mut entry.obj,
            &mut |d: &mut Dict| {
                let mut hit = |key: &str, class: &'static str, d: &mut Dict| {
                    if d.remove(key).is_some() {
                        *counts.entry(class).or_default() += 1;
                        touched = true;
                    }
                };
                if policy.drop_xmp {
                    hit("Metadata", "xmp", d);
                }
                if policy.drop_thumbnails {
                    hit("Thumb", "thumbnails", d);
                }
                if policy.drop_private_data {
                    hit("PieceInfo", "private_data", d);
                }
                if policy.drop_embedded_files {
                    hit("EmbeddedFiles", "embedded_files", d);
                    hit("AF", "embedded_files", d);
                    if d.get("Subtype").is_some_and(|s| s.is_name("FileAttachment")) {
                        hit("FS", "embedded_files", d);
                    }
                }
                if policy.drop_javascript {
                    hit("JavaScript", "javascript", d);
                    hit("AA", "javascript", d);
                    for key in ["A", "OpenAction"] {
                        if d.get(key).is_some_and(is_js) {
                            hit(key, "javascript", d);
                        }
                    }
                }
            },
            0,
        );
        if touched {
            entry.span = None;
        }
    }
    let bytes = write_document(&doc.version, &staged, &doc.source, &trailer, &doc.limits, cancel)?;
    Ok((bytes, SanitizeReport { removed: counts.into_iter().map(|(k, v)| (k.to_owned(), v)).collect() }))
}
