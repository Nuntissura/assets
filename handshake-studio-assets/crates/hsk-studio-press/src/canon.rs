//! The canonical content writer (lead decision 2026-10-11T15:00:20Z, press R10 pre-review, CANONICAL
//! REWRITE). The redaction save re-serialises EVERY content stream it keeps (page content, form
//! XObjects, Type 3 glyph procedures, annotation appearances, tiling patterns, streams with
//! resources: every stream a viewer may interpret as content) from this crate's own strict parse
//! (`content::parse_strict`) in one canonical syntax, so the saved file holds only bytes this crate
//! wrote and a viewer reads the same operators the proof read:
//! - one operation per line: its operands separated by one space, then the operator, then `\n`; each
//!   operator carries exactly its operands (`content::check_op`); no comments;
//! - integers in decimal; reals in the shortest decimal that reads back as the same number, always
//!   with a `.`; names with `#xx` for every byte that is not a regular printable character (and `#`
//!   itself); strings whose bytes are all printable ASCII as literals with `\`, `(` and `)` escaped,
//!   every other string in upper-case hexadecimal; arrays and dictionaries with single spaces;
//! - no inline image: every `BI`..`ID`..`EI` becomes an image XObject with an explicit `/Length` and
//!   the same dictionary (abbreviations spelled out, ISO 32000-2 8.9.7 Tables 91 and 92; a colour
//!   space named by a resource replaced by the resource's value) and data, drawn by `Do` under a fresh
//!   `/HskImN` name of the resources the stream is read with (R10-F1);
//! - a page's content streams become one new stream for that page (they are one stream for a viewer).
//!
//! A stream this crate cannot parse completely, or whose inline image it cannot move into the
//! resources without changing what other resource names mean, is NOT copied as proven: it stays as it
//! was and the payload proof reports it (`PayloadProof::unreadable_content_streams` when it does not
//! parse, `PayloadProof::non_canonical_streams` otherwise), which makes `RedactedOutput::is_proven`
//! and `region_cleared` false (`NotProven::UnreadableContent`, `NotProven::NonCanonicalContent`).
//! The proof checks every content-role stream of the saved file: its decoded bytes must be exactly the
//! canonical serialisation of its parse ([`is_canonical`]).
//!
//! Tests: `canonical_rewrite_round_trips` (parse(write(parse(x))) == parse(x) on the crate's content
//! fixtures), `inline_image_end_follows_the_data`, `extra_operands_are_refused` and the unit tests
//! below.
#![deny(clippy::arithmetic_side_effects)]
use crate::content::{Op, Parsed, inline_dict, inline_key, inline_value_name, joins_cleanly, parse_strict};
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::reader::{Entry, check_cancel};
use crate::redaction::res;
use crate::show_text::{Objs, content_role_streams, pages, pages_outside_tree, resource_components};
use crate::syntax::{Dict, ObjRef, Object, Stream};
use crate::text_fields::appearance_roots;
use crate::writer::{reachable, trailer_roots, write_name};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};

/// Longest number the tokenizer reads back (`syntax.rs` `MAX_NUMBER_LEN`).
const MAX_NUMBER_LEN: usize = 64;

fn write_real(v: f64, out: &mut Vec<u8>) -> Result<()> {
    if !v.is_finite() {
        return Err(PressError::Malformed("canonical_number"));
    }
    // Rust's `Display` for f64 is the shortest decimal that reads back as the same value, never in
    // exponent form; a `.` keeps it a real when read back.
    let mut s = format!("{v}");
    if !s.contains('.') {
        s.push_str(".0");
    }
    if s.len() > MAX_NUMBER_LEN {
        return Err(PressError::Malformed("canonical_number"));
    }
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

fn write_str(s: &[u8], out: &mut Vec<u8>) {
    if s.iter().all(|b| (0x20..=0x7E).contains(b)) {
        out.push(b'(');
        for &b in s {
            if matches!(b, b'(' | b')' | b'\\') {
                out.push(b'\\');
            }
            out.push(b);
        }
        out.push(b')');
    } else {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        out.push(b'<');
        for &b in s {
            out.push(HEX[usize::from(b >> 4)]);
            out.push(HEX[usize::from(b & 15)]);
        }
        out.push(b'>');
    }
}

/// Canonical bytes of one content operand (module docs). References and streams cannot occur in
/// content and are refused.
pub(crate) fn write_obj(o: &Object, out: &mut Vec<u8>) -> Result<()> {
    match o {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Int(v) => out.extend_from_slice(v.to_string().as_bytes()),
        Object::Real(v) => write_real(*v, out)?,
        Object::Name(n) => write_name(n, out),
        Object::Str(s) => write_str(s, out),
        Object::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_obj(item, out)?;
            }
            out.push(b']');
        }
        Object::Dict(d) => {
            out.extend_from_slice(b"<<");
            for (i, (k, v)) in d.entries.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                write_name(k, out);
                out.push(b' ');
                write_obj(v, out)?;
            }
            out.extend_from_slice(b">>");
        }
        Object::Ref(_) | Object::Stream(_) => return Err(PressError::Malformed("canonical_operand")),
    }
    Ok(())
}

/// One operation in canonical form. `BI` is refused unless `transport` is set: the redaction
/// selector's intermediate output keeps an inline image as `BI <dict> ID`, one `\n`, its exact data,
/// `\nEI\n` (it parses back to the same image), and [`canonicalize`] turns it into an XObject.
pub(crate) fn write_op(op: &Op, data: &[u8], transport: bool, out: &mut Vec<u8>) -> Result<()> {
    if op.name == b"BI" {
        if !transport {
            return Err(PressError::Malformed("canonical_inline_image"));
        }
        out.extend_from_slice(b"BI");
        for o in &op.operands {
            out.push(b' ');
            write_obj(o, out)?;
        }
        out.extend_from_slice(b" ID\n");
        let (a, b) = op.inline_data.ok_or(PressError::Malformed("inline_image"))?;
        out.extend_from_slice(data.get(a..b).ok_or(PressError::Malformed("inline_image"))?);
        out.extend_from_slice(b"\nEI\n");
        return Ok(());
    }
    for o in &op.operands {
        write_obj(o, out)?;
        out.push(b' ');
    }
    out.extend_from_slice(&op.name);
    out.push(b'\n');
    Ok(())
}

/// Canonical bytes of a whole operation list (no inline image allowed).
pub(crate) fn write_ops(ops: &[Op], data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    for op in ops {
        write_op(op, data, false, &mut out)?;
    }
    Ok(out)
}

/// `data` is exactly the canonical serialisation of its own strict parse `parsed` (no inline image,
/// nothing but canonical operations).
pub(crate) fn is_canonical(data: &[u8], parsed: &Parsed) -> bool {
    !parsed.trailing_operands && write_ops(&parsed.ops, data).is_ok_and(|w| w == data)
}

/// What [`canonicalize`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Canonicalized {
    /// Content streams left as they were, with the reason (they do not parse completely, or an inline
    /// image could not move into the resources).
    pub refused: Vec<(u32, &'static str)>,
    /// Inline images turned into image XObjects.
    pub inline_images: u32,
}

/// Resource names an operation looks up (fonts, ExtGStates, XObjects, shadings, colour spaces other
/// than the device and pattern families, patterns, property lists).
fn uses_named_resource(op: &Op) -> bool {
    let first_name = || op.operands.first().and_then(Object::as_name);
    match op.name.as_slice() {
        b"Tf" | b"gs" | b"Do" | b"sh" => true,
        b"CS" | b"cs" => !matches!(first_name(), Some(b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK" | b"Pattern")),
        b"SCN" | b"scn" => matches!(op.operands.last(), Some(Object::Name(_))),
        b"BDC" | b"DP" => matches!(op.operands.get(1), Some(Object::Name(_))),
        b"BI" => matches!(inline_dict(&op.operands).get("ColorSpace"), Some(Object::Name(n)) if !matches!(inline_value_name(n), b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK"))
            || matches!(inline_dict(&op.operands).get("ColorSpace"), Some(Object::Array(a)) if a.get(1).and_then(Object::as_name).is_some_and(|n| !matches!(inline_value_name(n), b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK"))),
        _ => false,
    }
}

/// An inline image's colour space as an image XObject's `/ColorSpace` value: abbreviations spelled
/// out, a resource name replaced by the resource value (`None`: not resolvable).
fn xobject_color_space(objs: &Objs, v: &Object, resources: Option<&Dict>) -> Option<Object> {
    let named = |n: &[u8]| -> Option<Object> {
        match inline_value_name(n) {
            full @ (b"DeviceGray" | b"DeviceRGB" | b"DeviceCMYK") => Some(Object::Name(full.to_vec())),
            other => resources?.get("ColorSpace").map(|c| res(objs, c)).and_then(Object::as_dict).and_then(|c| c.get_key(other)).cloned(),
        }
    };
    match v {
        Object::Name(n) => named(n),
        Object::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for (i, x) in a.iter().enumerate() {
                out.push(match (i, x) {
                    (0, Object::Name(n)) => Object::Name(inline_value_name(n).to_vec()),
                    (1, Object::Name(n)) => named(n)?,
                    _ => x.clone(),
                });
            }
            Some(Object::Array(out))
        }
        _ => None,
    }
}

/// The image XObject an inline image becomes (module docs).
fn image_xobject(objs: &Objs, op: &Op, data: &[u8], resources: Option<&Dict>) -> Option<Stream> {
    let mut dict = Dict::default();
    dict.set("Type", Object::Name(b"XObject".to_vec()));
    dict.set("Subtype", Object::Name(b"Image".to_vec()));
    for pair in op.operands.chunks(2) {
        let [Object::Name(k), v] = pair else { return None };
        let key = inline_key(k);
        let value = match key {
            b"Length" => continue,
            b"ColorSpace" => xobject_color_space(objs, v, resources)?,
            b"Filter" => match v {
                Object::Name(n) => Object::Name(inline_value_name(n).to_vec()),
                Object::Array(a) => Object::Array(a.iter().map(|x| x.as_name().map(|n| Object::Name(inline_value_name(n).to_vec()))).collect::<Option<Vec<_>>>()?),
                other => other.clone(),
            },
            _ => v.clone(),
        };
        dict.set_key(key, value);
    }
    let (a, b) = op.inline_data?;
    Some(Stream { dict, data: data.get(a..b)?.to_vec() })
}

/// One content stream to rewrite.
struct Plan {
    /// `None`: a new stream for page `page`; `Some(n)`: stream `n` rewritten in place.
    target: Option<u32>,
    page: u32,
    data: Vec<u8>,
    ops: Vec<Op>,
    /// Where new image XObject names go: page dictionaries, the stream itself, Type 3 fonts.
    holders: Vec<u32>,
    resources: Option<Dict>,
}

/// Decodes and strictly parses a stream (reason on failure).
fn parse_stream(objs: &Objs, num: u32, resources: Option<&Dict>, limits: &Limits, budget: &mut usize) -> Result<std::result::Result<(Vec<u8>, Parsed), &'static str>> {
    let Some(Object::Stream(s)) = objs.get(&num).map(|e| &e.obj) else { return Ok(Err("not_a_stream")) };
    let data = match filters::decode_stream(s, limits, budget) {
        Ok(d) => d,
        Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
        Err(_) => return Ok(Err("content_unreadable")),
    };
    let cs = |n: &[u8]| resource_components(objs, resources, n);
    match parse_strict(&data, limits, &cs, budget) {
        Ok(p) => Ok(Ok((data, p))),
        Err(e @ (PressError::InputLimit("decoded_total" | "decoded_bytes") | PressError::Canceled)) => Err(e),
        Err(PressError::Malformed(why) | PressError::InputLimit(why)) => Ok(Err(why)),
        Err(_) => Ok(Err("content_unreadable")),
    }
}

/// Rewrites every content stream of the live document canonically (module docs). Called by the
/// redaction save right before the full save; the caller restores `objects` on error.
pub(crate) fn canonicalize(objects: &mut BTreeMap<u32, Entry>, trailer: &Dict, limits: &Limits, cancel: &CancellationToken) -> Result<Canonicalized> {
    let mut report = Canonicalized::default();
    let mut budget = limits.max_decoded_total;
    let mut plans: Vec<Plan> = Vec::new();
    {
        let objs: &Objs = objects;
        let live = reachable(objs, &trailer_roots(trailer));
        let tree = pages(objs, trailer);
        let tree_set: BTreeSet<u32> = tree.iter().map(|(n, _, _)| *n).collect();
        let mut page_list: Vec<(u32, Vec<u32>, Option<&Dict>)> = tree;
        page_list.extend(pages_outside_tree(objs, &tree_set));
        for (page, segments, resources) in page_list.into_iter().filter(|(p, _, _)| live.contains(p)) {
            check_cancel(cancel)?;
            let mut data = Vec::new();
            let mut ops: Vec<Op> = Vec::new();
            let mut failed: Option<(u32, &'static str)> = None;
            let mut prev: Option<(Vec<u8>, Parsed)> = None;
            for (i, seg) in segments.iter().enumerate() {
                let (seg_data, parsed) = match parse_stream(objs, *seg, resources, limits, &mut budget)? {
                    Ok(x) => x,
                    Err(why) => {
                        failed = Some((*seg, why));
                        break;
                    }
                };
                if let Some((pd, pp)) = &prev
                    && !joins_cleanly(pd, pp, &seg_data)
                {
                    failed = Some((*seg, "segments_merge"));
                    break;
                }
                if parsed.trailing_operands && i.saturating_add(1) < segments.len() {
                    failed = Some((*seg, "operands_across_streams"));
                    break;
                }
                // Offsets of the joined data: shift every span of this segment.
                let base = data.len();
                data.extend_from_slice(&seg_data);
                for op in &parsed.ops {
                    let mut op = op.clone();
                    op.inline_data = op.inline_data.and_then(|(a, b)| Some((a.checked_add(base)?, b.checked_add(base)?)));
                    ops.push(op);
                }
                prev = Some((seg_data, parsed));
            }
            match failed {
                Some(f) => report.refused.push(f),
                None if segments.is_empty() => {}
                // One stream without inline images: rewritten in place (its canonical bytes do not
                // depend on the page, so a stream several pages share stays one stream).
                None if segments.len() == 1 && !ops.iter().any(|op| op.name == b"BI") => {
                    plans.push(Plan { target: segments.first().copied(), page, data, ops, holders: Vec::new(), resources: resources.cloned() });
                }
                None => plans.push(Plan { target: None, page, data, ops, holders: vec![page], resources: resources.cloned() }),
            }
        }
        let mut streams: BTreeMap<u32, (Option<&Dict>, Vec<Option<u32>>)> = BTreeMap::new();
        for n in appearance_roots(objs) {
            let own = match objs.get(&n).map(|e| &e.obj) {
                Some(Object::Stream(s)) => s.dict.get("Resources").map(|r| res(objs, r)).and_then(Object::as_dict),
                _ => None,
            };
            streams.entry(n).or_insert((own, Vec::new()));
        }
        for (n, role) in content_role_streams(objs) {
            let e = streams.entry(n).or_insert((role.resources, Vec::new()));
            e.1.extend(role.type3_owners);
        }
        for (num, (resources, owners)) in streams.into_iter().filter(|(n, _)| live.contains(n)) {
            check_cancel(cancel)?;
            let (data, parsed) = match parse_stream(objs, num, resources, limits, &mut budget)? {
                Ok(x) => x,
                Err(why) => {
                    report.refused.push((num, why));
                    continue;
                }
            };
            let images = parsed.ops.iter().any(|op| op.name == b"BI");
            let holders: Vec<u32> = if !images {
                Vec::new()
            } else if owners.is_empty() {
                vec![num]
            } else if let Some(h) = owners.iter().copied().collect::<Option<Vec<u32>>>() {
                h
            } else {
                report.refused.push((num, "inline_image_holder"));
                continue;
            };
            // A holder without resources gets new ones: only when nothing else is looked up by name.
            let bare = holders.iter().any(|h| objs.get(h).and_then(|e| e.obj.as_dict()).is_some_and(|d| d.get("Resources").is_none()));
            if images && bare && parsed.ops.iter().any(|op| op.name != b"BI" && uses_named_resource(op)) {
                report.refused.push((num, "inline_image_holder"));
                continue;
            }
            plans.push(Plan { target: Some(num), page: 0, data, ops: parsed.ops, holders, resources: resources.cloned() });
        }
    }
    let mut next = objects.keys().next_back().copied().unwrap_or(0);
    let mut alloc = || -> Result<u32> {
        next = next.checked_add(1).ok_or(PressError::InputLimit("max_objects"))?;
        Ok(next)
    };
    let mut counter = 0u32;
    for plan in plans {
        check_cancel(cancel)?;
        let mut out = Vec::with_capacity(plan.data.len());
        let mut additions: Vec<(Vec<u8>, ObjRef)> = Vec::new();
        let mut refused = false;
        for op in &plan.ops {
            if op.name == b"BI" {
                let Some(image) = image_xobject(objects, op, &plan.data, plan.resources.as_ref()) else {
                    refused = true;
                    break;
                };
                let name = loop {
                    let candidate = format!("HskIm{counter}").into_bytes();
                    counter = counter.checked_add(1).ok_or(PressError::InputLimit("names"))?;
                    let taken = plan.holders.iter().any(|h| {
                        resources_of(objects, *h, plan.target == Some(*h)).and_then(|r| r.get("XObject")).map(|x| res(objects, x)).and_then(Object::as_dict).is_some_and(|x| x.get_key(&candidate).is_some())
                    });
                    if !taken {
                        break candidate;
                    }
                };
                let num = alloc()?;
                objects.insert(num, Entry { generation: 0, obj: Object::Stream(Box::new(image)), span: None, data_at: None });
                additions.push((name.clone(), ObjRef { num, generation: 0 }));
                write_op(&Op { name: b"Do".to_vec(), operands: vec![Object::Name(name)], start: 0, end: 0, inline_data: None }, &[], false, &mut out)?;
                report.inline_images = report.inline_images.saturating_add(1);
            } else {
                write_op(op, &plan.data, false, &mut out)?;
            }
        }
        let stream_num = plan.target.unwrap_or(0);
        if refused {
            report.refused.push((if plan.target.is_some() { stream_num } else { plan.page }, "inline_image_color_space"));
            continue;
        }
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(&out, 6);
        match plan.target {
            Some(n) => {
                let Some(Entry { obj: Object::Stream(s), span, .. }) = objects.get_mut(&n) else { continue };
                for key in ["Filter", "DecodeParms", "Length", "DL"] {
                    s.dict.remove(key);
                }
                s.dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
                s.data = compressed;
                *span = None;
            }
            None => {
                let mut dict = Dict::default();
                dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
                let num = alloc()?;
                objects.insert(num, Entry { generation: 0, obj: Object::Stream(Box::new(Stream { dict, data: compressed })), span: None, data_at: None });
                if let Some(Entry { obj: Object::Dict(d), span, .. }) = objects.get_mut(&plan.page) {
                    d.set("Contents", Object::Ref(ObjRef { num, generation: 0 }));
                    *span = None;
                }
            }
        }
        for holder in &plan.holders {
            if !additions.is_empty() {
                add_xobjects(objects, *holder, plan.target == Some(*holder), plan.resources.as_ref(), &additions);
            }
        }
    }
    Ok(report)
}

/// The resources a holder looks names up in: a page's own or inherited ones (`page`), else the
/// dictionary's (or stream dictionary's) `/Resources`.
fn resources_of(objects: &Objs, holder: u32, stream: bool) -> Option<&Dict> {
    let d = objects.get(&holder)?.obj.as_dict()?;
    if !stream && d.get("Contents").is_some() {
        return crate::show_text::inherited_resources(objects, d);
    }
    d.get("Resources").map(|r| res(objects, r)).and_then(Object::as_dict)
}

/// Adds `/XObject` entries to a holder's resources, as a direct copy on the holder (a page gets a copy
/// of its inherited resources; a shared resource dictionary is never edited in place).
fn add_xobjects(objects: &mut BTreeMap<u32, Entry>, holder: u32, stream: bool, inherited: Option<&Dict>, additions: &[(Vec<u8>, ObjRef)]) {
    let base: Dict = if !stream && objects.get(&holder).and_then(|e| e.obj.as_dict()).is_some_and(|d| d.get("Contents").is_some()) {
        inherited.cloned().unwrap_or_default()
    } else {
        resources_of(objects, holder, stream).cloned().unwrap_or_default()
    };
    let mut resources = base;
    let mut xo = resources.get("XObject").map(|x| res(objects, x)).and_then(Object::as_dict).cloned().unwrap_or_default();
    for (name, r) in additions {
        xo.set_key(name, Object::Ref(*r));
    }
    resources.set("XObject", Object::Dict(xo));
    if let Some(e) = objects.get_mut(&holder) {
        let dict = match &mut e.obj {
            Object::Dict(d) => Some(d),
            Object::Stream(s) => Some(&mut s.dict),
            _ => None,
        };
        if let Some(d) = dict {
            d.set("Resources", Object::Dict(resources));
            e.span = None;
        }
    }
}

#[cfg(test)]
#[allow(clippy::arithmetic_side_effects)]
mod tests {
    use super::*;
    use crate::content::no_color_spaces;

    fn parse(s: &[u8]) -> Parsed {
        let mut budget = usize::MAX;
        parse_strict(s, &Limits::default(), &no_color_spaces, &mut budget).unwrap()
    }

    fn same(a: &[Op], b: &[Op]) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.name == y.name && x.operands == y.operands)
    }

    #[test]
    fn canonical_form_reads_back_identically() {
        let src: &[u8] = b"q 0.12345678901 0 0 -1 1.0 3. cm BT /F#201 12 Tf (a\\(b\\)\\\\c) Tj <0001FF> Tj [(x) -250.5 <41>] TJ 1 2 (y) \" ET /Span <</ActualText (Z) /K [1 2.5 true null]>> BDC EMC % comment\n -0.0 g";
        let first = parse(src);
        let written = write_ops(&first.ops, src).unwrap();
        let again = parse(&written);
        assert!(same(&first.ops, &again.ops), "{}", String::from_utf8_lossy(&written));
        assert!(is_canonical(&written, &again));
        assert!(!is_canonical(src, &first), "comments, spacing and spellings are not canonical");
        assert_eq!(write_ops(&again.ops, &written).unwrap(), written, "idempotent");
        // One form per value: printable literals, hexadecimal otherwise, reals keep a point.
        let mut out = Vec::new();
        for o in [Object::Str(b"a(b)".to_vec()), Object::Str(vec![0, 0x41]), Object::Real(3.0), Object::Real(-0.0), Object::Name(b"a b#".to_vec())] {
            write_obj(&o, &mut out).unwrap();
            out.push(b' ');
        }
        assert_eq!(out, b"(a\\(b\\)) <0041> 3.0 -0.0 /a#20b#23 ".to_vec());
        // Numbers that would not read back are refused, not rounded.
        assert!(write_obj(&Object::Real(1.0e-60 * 1.234_567_890_123_456_7), &mut Vec::new()).is_err());
        assert!(write_obj(&Object::Real(f64::NAN), &mut Vec::new()).is_err());
        // Inline images are no canonical content.
        let img: &[u8] = b"BI /W 1 /H 1 /BPC 8 /CS /G ID A EI";
        let p = parse(img);
        assert!(write_ops(&p.ops, img).is_err() && !is_canonical(img, &p));
        let mut t = Vec::new();
        write_op(&p.ops[0], img, true, &mut t).unwrap();
        assert!(same(&parse(&t).ops, &p.ops), "transport form reads back");
    }

    #[test]
    fn inline_images_become_xobjects() {
        let img: &[u8] = b"BI /W 1 /H 1 /BPC 8 /CS /CS0 /F /AHx /I true ID 41> EI";
        let mut budget = usize::MAX;
        let cs = |n: &[u8]| (n == b"CS0").then_some(1);
        let p = parse_strict(img, &Limits::default(), &cs, &mut budget).unwrap();
        let mut res = Dict::default();
        let mut cs_dict = Dict::default();
        cs_dict.set("CS0", Object::Array(vec![Object::Name(b"CalGray".to_vec()), Object::Dict(Dict::default())]));
        res.set("ColorSpace", Object::Dict(cs_dict));
        let objs = Objs::new();
        let x = image_xobject(&objs, &p.ops[0], img, Some(&res)).unwrap();
        assert_eq!(x.data, b"41>");
        assert_eq!(x.dict.get("Filter"), Some(&Object::Name(b"ASCIIHexDecode".to_vec())));
        assert_eq!(x.dict.get("Interpolate"), Some(&Object::Bool(true)));
        assert!(matches!(x.dict.get("ColorSpace"), Some(Object::Array(a)) if a[0].is_name("CalGray")));
        assert!(image_xobject(&objs, &p.ops[0], img, None).is_none(), "an unresolvable colour space refuses");
    }
}
