//! Bounds on what the flattening renderer receives (R13-F4, R13-F5; lead decision 2026-10-11T20:26:55Z),
//! checked on the isolated one-page document BEFORE hayro-interpret sees a byte of it. Every violation
//! is a typed refusal (`PressError::FlattenRefused`) and the document is restored, not a fallback to the
//! original page (tests `renderer_input_is_bounded_before_rendering`, `renderer_input_images_are_bounded`).
//!
//! Why: hayro-syntax 0.7.2 reads arrays and dictionaries recursively without a depth limit
//! (`Object::skip` / `Array::skip` / `Dict::skip`), hayro-interpret 0.7.0 parses Type 4 function
//! procedures recursively (`parse_procedure_inner`), and hayro-postscript 0.1.0 (embedded CMaps)
//! recurses once per `<<` or `>>` token in a row. A Rust stack overflow is not a panic: it ABORTS the
//! process (it is not unwound, so neither `catch_unwind` nor the render thread's join turns it into a
//! refusal), so the nesting
//! bound below is what prevents it; the render thread's large stack (`flatten.rs`) is margin, not the
//! guard. hayro also allocates an image's full `width x height x components` sample planes whatever
//! the data length (`get_components`, `fix_image_length`), so image sizes are bounded here as well.
//!
//! Nesting ([`FlattenCause::NestingDepth`], bound `Limits::max_depth` clamped to 256):
//! - every content stream of the isolated document (page content and every stream with a content role:
//!   forms, tiling patterns, Type 3 glyph procedures, soft-mask groups, streams with `/Resources`) is
//!   tokenized with this crate's content tokenizer, whose object parser refuses nesting past the bound;
//!   a content stream it cannot tokenize at all refuses as [`FlattenCause::RendererInput`];
//! - every other stream the renderer may parse as PDF or PostScript syntax (functions, CMaps, any
//!   stream not in a binary role) is scanned lexically: `[`, `{` and `<<` nesting, and runs of `<<` /
//!   `>>` tokens, within the bound (strings, hex strings and comments are skipped). Binary roles that
//!   are NOT scanned: image XObjects and their `/SMask` / `/Mask` images, font programs
//!   (`/FontFile`, `/FontFile2`, `/FontFile3`, parsed by skrifa/read-fonts), ICC profiles, sampled
//!   (type 0) functions, mesh shadings (types 4-7), `/JBIG2Globals`, metadata and embedded files;
//! - every colour space and function the renderer builds recursively (`/Indexed` and `/Pattern` bases,
//!   `/Separation` / `/DeviceN` / `/NChannel` alternates and tint transforms, `/ICCBased` `/Alternate`,
//!   stitching-function `/Functions`): no reference cycle and at most the same number of levels
//!   (hayro-interpret 0.7.0 `ColorSpaceType::new_inner` and `Function::new` have no cycle guard).
//!
//! Images ([`FlattenCause::ImageBudget`]): every image XObject (its `/Subtype` resolved), every
//! `/SMask` and `/Mask` image and every inline image of every content stream, with the abbreviated
//! keys hayro also reads (`/W` `/H` `/BPC` `/CS` `/IM`, the largest value of both spellings): width
//! and height (and, for a raster codec, the size its own header declares: `raster::codec_dims`) give
//! `pixels`; components come from the colour space (a name the resources do not resolve counts as the
//! largest component count in the document, at least 4). Refused when `pixels > max_decoded_bytes`,
//! when `pixels x (components x 6 + 15) > max_render_image_bytes`, or when sample data that is not a
//! codec's input holds fewer bytes than `ceil(width x components x bpc / 8) x height` (an image whose
//! data does not back its declared samples, including one this crate could not decode). Tests
//! `renderer_input_is_bounded_before_rendering`, `renderer_input_images_are_bounded` and the unit tests
//! below (`dims_u32_max`, nesting scans).
//!
//! Second caller (SMB-PARSER-HOSTILE-1): [`reader_input_within`] runs the same nesting bounds (and,
//! for `render_page`, the image bounds) on Press's reading of a file before the vector reader or the
//! render-back hands the original bytes to hayro (tests `press/tests/hostile_vector_read.rs`).
//!
//! Residual (DESCRIPTOR `redaction.flatten.renderer_residual`): CPU time inside hayro (font parsing,
//! codec decoding) is not cancellable; codec-internal bitmaps beyond the dimensions read here (JBIG2
//! symbol dictionaries, halftone patterns, JPEG 2000 tiles) are bounded only by the decoders' own
//! limits (65535 / 60000 per side); font programs are parsed by skrifa/read-fonts without this scan.
#![deny(clippy::arithmetic_side_effects)]
use crate::content::{self, inline_dict, inline_value_name};
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::flatten::{FlattenCause, refs_in};
use crate::pdf_edit::PdfDoc;
use crate::raster::{self, codec_dims, is_raster_filter};
use crate::reader::{Entry, check_cancel};
use crate::redaction::res;
use crate::show_text::{content_role_streams, resource_components};
use crate::syntax::{Dict, Object, Stream, is_ws};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};

type Objs = BTreeMap<u32, Entry>;

fn refuse(c: FlattenCause) -> PressError {
    PressError::FlattenRefused(c)
}

/// Lexical nesting of PDF / PostScript syntax within `cap` (module docs): `[`, `{` and `<<` open, `]`,
/// `}` and `>>` close; a run of `<<` / `>>` tokens separated only by white space and comments counts
/// as nesting too (hayro-postscript recurses once per token of such a run).
pub(crate) fn syntax_nesting_within(data: &[u8], cap: usize) -> bool {
    let (mut depth, mut run, mut i) = (0usize, 0usize, 0usize);
    let n = data.len();
    while i < n {
        let b = data.get(i).copied().unwrap_or(0);
        let next = data.get(i.saturating_add(1)).copied();
        match b {
            b'%' => {
                while i < n && !matches!(data.get(i), Some(10 | 13)) {
                    i = i.saturating_add(1);
                }
                continue;
            }
            b'(' => {
                run = 0;
                let mut level = 0usize;
                while i < n {
                    match data.get(i).copied().unwrap_or(0) {
                        b'\\' => i = i.saturating_add(1),
                        b'(' => level = level.saturating_add(1),
                        b')' => {
                            level = level.saturating_sub(1);
                            if level == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    i = i.saturating_add(1);
                }
            }
            b'<' if next == Some(b'<') => {
                depth = depth.saturating_add(1);
                run = run.saturating_add(1);
                i = i.saturating_add(1);
            }
            b'>' if next == Some(b'>') => {
                depth = depth.saturating_sub(1);
                run = run.saturating_add(1);
                i = i.saturating_add(1);
            }
            b'<' if next == Some(b'~') => {
                run = 0;
                while i < n && !(data.get(i) == Some(&b'~') && data.get(i.saturating_add(1)) == Some(&b'>')) {
                    i = i.saturating_add(1);
                }
                i = i.saturating_add(1);
            }
            b'<' => {
                run = 0;
                while i < n && data.get(i) != Some(&b'>') {
                    i = i.saturating_add(1);
                }
            }
            b'[' | b'{' => {
                run = 0;
                depth = depth.saturating_add(1);
            }
            b']' | b'}' => {
                run = 0;
                depth = depth.saturating_sub(1);
            }
            _ if is_ws(b) => {}
            _ => run = 0,
        }
        if depth > cap || run > cap {
            return false;
        }
        i = i.saturating_add(1);
    }
    true
}

/// The objects hayro-interpret builds recursively from `o` while it constructs it (color.rs
/// `ColorSpaceType::new_inner`, function/mod.rs `Function::new`): the base of an `/Indexed` and a
/// `/Pattern` colour space, the alternate space and tint transform of `/Separation`, `/DeviceN` and
/// `/NChannel`, the `/Alternate` of an `/ICCBased` profile, the `/Functions` of a stitching (type 3)
/// function.
fn chain_children<'a>(objs: &'a Objs, o: &'a Object) -> Vec<&'a Object> {
    match res(objs, o) {
        Object::Array(a) => {
            let Some(first) = a.first().map(|f| res(objs, f)).and_then(Object::as_name).map(inline_value_name) else { return Vec::new() };
            match first {
                b"Indexed" | b"Pattern" => a.get(1).into_iter().collect(),
                b"Separation" | b"DeviceN" | b"NChannel" => a.get(2..4).map(|s| s.iter().collect()).unwrap_or_default(),
                b"ICCBased" => a.get(1).map(|s| res(objs, s)).and_then(Object::as_dict).and_then(|d| d.get("Alternate")).into_iter().collect(),
                _ => Vec::new(),
            }
        }
        d @ (Object::Dict(_) | Object::Stream(_)) => {
            let Some(d) = d.as_dict() else { return Vec::new() };
            if d.get("FunctionType").map(|t| res(objs, t)).and_then(Object::as_int) != Some(3) {
                return Vec::new();
            }
            match d.get("Functions").map(|f| res(objs, f)) {
                Some(Object::Array(fs)) => fs.iter().collect(),
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

/// Every colour space and function of the document is built by the renderer without a reference cycle
/// and with at most `cap` levels (module docs: a cycle recurses until the stack overflows).
fn construction_chains_within(objs: &Objs, cap: usize) -> bool {
    // Height of every node reached (key: address of the resolved object), computed iteratively.
    let key = |o: &Object| res(objs, o) as *const Object as usize;
    let mut height: BTreeMap<usize, usize> = BTreeMap::new();
    let mut roots: Vec<&Object> = Vec::new();
    for e in objs.values() {
        let mut stack: Vec<(&Object, usize)> = vec![(&e.obj, 0)];
        while let Some((o, depth)) = stack.pop() {
            if depth > 64 {
                continue;
            }
            let next = depth.saturating_add(1);
            if !chain_children(objs, o).is_empty() {
                roots.push(o);
            }
            match o {
                Object::Array(a) => stack.extend(a.iter().map(|x| (x, next))),
                Object::Dict(d) => stack.extend(d.entries.iter().map(|(_, v)| (v, next))),
                Object::Stream(s) => stack.extend(s.dict.entries.iter().map(|(_, v)| (v, next))),
                _ => {}
            }
        }
    }
    for root in roots {
        // Post-order DFS: (node, children visited); a node on the path again is a cycle.
        let mut path: Vec<usize> = Vec::new();
        let mut stack: Vec<(&Object, bool)> = vec![(root, false)];
        while let Some((o, expanded)) = stack.pop() {
            let k = key(o);
            if expanded {
                path.pop();
                let h = chain_children(objs, o).iter().map(|c| height.get(&key(c)).copied().unwrap_or(0)).max().map_or(1, |m| m.saturating_add(1));
                if h > cap {
                    return false;
                }
                height.insert(k, h);
                continue;
            }
            if height.contains_key(&k) {
                continue;
            }
            if path.contains(&k) || path.len() > cap {
                return false;
            }
            path.push(k);
            stack.push((o, true));
            for c in chain_children(objs, o) {
                if path.contains(&key(c)) {
                    return false;
                }
                stack.push((c, false));
            }
        }
    }
    true
}

/// A positive dimension value as hayro may read it (integer or real, rounded up).
fn dim_value(o: &Object) -> Option<u64> {
    match o {
        Object::Int(v) => u64::try_from(*v).ok().filter(|v| *v > 0),
        Object::Real(v) if v.is_finite() && *v > 0.0 => Some(if *v >= 1.8e19 { u64::MAX } else { v.ceil() as u64 }),
        _ => None,
    }
}

/// Largest value of any spelling of a key.
fn largest(objs: &Objs, d: &Dict, keys: &[&str]) -> Option<u64> {
    keys.iter().filter_map(|k| d.get(k).map(|v| res(objs, v))).filter_map(dim_value).max()
}

/// Colour components of a colour-space value (names and arrays; a resource name through `resources`).
fn color_components(objs: &Objs, v: &Object, resources: &[Option<&Dict>], depth: usize) -> Option<u64> {
    if depth > 4 {
        return None;
    }
    match res(objs, v) {
        Object::Name(n) => match inline_value_name(n) {
            b"DeviceGray" | b"CalGray" | b"Indexed" | b"Separation" | b"Pattern" => Some(1),
            b"DeviceRGB" | b"CalRGB" | b"Lab" => Some(3),
            b"DeviceCMYK" => Some(4),
            other => resources.iter().flatten().find_map(|r| {
                let cs = r.get("ColorSpace").map(|c| res(objs, c)).and_then(Object::as_dict)?.get_key(other)?;
                color_components(objs, cs, resources, depth.saturating_add(1))
            }),
        },
        Object::Array(a) => {
            let first = a.first().map(|f| res(objs, f)).and_then(Object::as_name).map(inline_value_name)?;
            match first {
                b"ICCBased" => a.get(1).map(|s| res(objs, s)).and_then(Object::as_dict).and_then(|s| s.get("N")).map(|n| res(objs, n)).and_then(Object::as_int).and_then(|n| u64::try_from(n).ok()),
                b"Indexed" | b"Separation" | b"CalGray" | b"DeviceGray" | b"Pattern" => Some(1),
                b"CalRGB" | b"Lab" | b"DeviceRGB" => Some(3),
                b"DeviceCMYK" => Some(4),
                b"DeviceN" | b"NChannel" => a.get(1).map(|x| res(objs, x)).and_then(Object::as_array).map(|x| u64::try_from(x.len()).unwrap_or(u64::MAX)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The largest component count any colour space of the document declares (at least 4): the count an
/// image whose colour space is not resolved is sized with.
fn document_components(objs: &Objs) -> u64 {
    let mut best = 4u64;
    for e in objs.values() {
        let mut stack: Vec<(&Object, usize)> = vec![(&e.obj, 0)];
        while let Some((o, depth)) = stack.pop() {
            if depth > 64 {
                continue;
            }
            let next = depth.saturating_add(1);
            match o {
                Object::Array(a) => {
                    if a.first().is_some_and(|f| f.is_name("DeviceN") || f.is_name("NChannel") || f.is_name("ICCBased"))
                        && let Some(n) = color_components(objs, o, &[], 0)
                    {
                        best = best.max(n);
                    }
                    stack.extend(a.iter().map(|x| (x, next)));
                }
                Object::Dict(d) => stack.extend(d.entries.iter().map(|(_, v)| (v, next))),
                Object::Stream(s) => stack.extend(s.dict.entries.iter().map(|(_, v)| (v, next))),
                _ => {}
            }
        }
    }
    best
}

/// One image as the renderer would decode it.
struct ImageInput<'a> {
    dict: &'a Dict,
    /// Decoded sample bytes, or the codec's input when `codec` is set.
    data: &'a [u8],
    codec: Option<Vec<u8>>,
}

/// Image size bounds (module docs). `check_backing` verifies the declared samples are backed by the
/// data; the reader pass (SMB-PARSER-HOSTILE-1 F5) leaves it off and passes the still-encoded data,
/// so the pixel bound alone caps what hayro allocates per image (up to 150 Mpx at any colour space
/// and bit depth), without decoding the whole image to measure it.
fn image_bounds(objs: &Objs, img: &ImageInput<'_>, resources: &[Option<&Dict>], doc_comps: u64, limits: &Limits, check_backing: bool) -> Result<()> {
    let d = img.dict;
    let (Some(mut w), Some(mut h)) = (largest(objs, d, &["W", "Width"]), largest(objs, d, &["H", "Height"])) else {
        // hayro draws no image without both dimensions (x_object.rs: ImageXObject::new).
        return Ok(());
    };
    let mask = ["IM", "ImageMask"].iter().any(|k| matches!(d.get(k).map(|v| res(objs, v)), Some(Object::Bool(true))));
    let declared: Option<u64> = if mask {
        Some(1)
    } else {
        ["CS", "ColorSpace"].iter().filter_map(|k| d.get(k)).filter_map(|v| color_components(objs, v, resources, 0)).max()
    };
    let mut comps = declared.unwrap_or(doc_comps);
    let bpc = if mask {
        1
    } else {
        ["BPC", "BitsPerComponent"].iter().filter_map(|k| d.get(k).map(|v| res(objs, v))).filter_map(Object::as_int).filter_map(|v| u64::try_from(v).ok()).max().unwrap_or(8)
    };
    if let Some(codec) = &img.codec
        && let Some((cw, ch, cc)) = codec_dims(codec, img.data, d)
    {
        w = w.max(cw);
        h = h.max(ch);
        comps = comps.max(cc);
    }
    let budget = || refuse(FlattenCause::ImageBudget);
    let px = w.checked_mul(h).ok_or_else(budget)?;
    if px > u64::try_from(limits.max_decoded_bytes).unwrap_or(u64::MAX) {
        return Err(budget());
    }
    let per_px = comps.checked_mul(6).and_then(|v| v.checked_add(15)).ok_or_else(budget)?;
    if px.checked_mul(per_px).is_none_or(|bytes| bytes > limits.max_render_image_bytes) {
        return Err(budget());
    }
    if check_backing && img.codec.is_none() {
        // Declared samples must be backed by data (the renderer allocates them whatever the data holds).
        let lower = declared.unwrap_or(1);
        let bits = w.checked_mul(lower).and_then(|v| v.checked_mul(bpc.max(1))).ok_or_else(budget)?;
        let row = bits.checked_add(7).ok_or_else(budget)? / 8;
        let need = row.checked_mul(h).ok_or_else(budget)?;
        if u64::try_from(img.data.len()).unwrap_or(u64::MAX) < need {
            return Err(budget());
        }
    }
    Ok(())
}

/// Streams a dictionary or array references in a binary role (module docs; SMB-PARSER-HOSTILE-1 F6:
/// a data stream the renderer never parses as syntax). Collects from `root` into `out`: `/ICCBased`
/// profile streams, font programs, `/JBIG2Globals`, embedded files, metadata, the `/CIDToGIDMap` and
/// `/CIDSet` streams of a CIDFont, and the `/HT` halftone stream of an `/ExtGState`.
fn binary_refs(root: &Object, out: &mut BTreeSet<u32>) {
    let mut stack: Vec<(&Object, usize)> = vec![(root, 0)];
    while let Some((o, depth)) = stack.pop() {
        if depth > 64 {
            continue;
        }
        let next = depth.saturating_add(1);
        match o {
            Object::Array(a) => {
                if a.first().is_some_and(|f| f.is_name("ICCBased"))
                    && let Some(r) = a.get(1).and_then(Object::as_ref)
                {
                    out.insert(r.num);
                }
                stack.extend(a.iter().map(|x| (x, next)));
            }
            Object::Dict(_) | Object::Stream(_) => {
                let Some(d) = o.as_dict() else { continue };
                for key in ["FontFile", "FontFile2", "FontFile3", "JBIG2Globals", "EF", "Metadata", "CIDToGIDMap", "CIDSet", "HT"] {
                    if let Some(r) = d.get(key).and_then(Object::as_ref) {
                        out.insert(r.num);
                    }
                }
                stack.extend(d.entries.iter().map(|(_, v)| (v, next)));
            }
            _ => {}
        }
    }
}

/// Streams in a binary role (module docs): not scanned as syntax.
fn binary_streams(objs: &Objs) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    for (num, e) in objs {
        if let Object::Stream(s) = &e.obj {
            let d = &s.dict;
            let int = |k: &str| d.get(k).map(|v| res(objs, v)).and_then(Object::as_int);
            // A sampled (type 0) function, a mesh shading (types 4-7), a threshold / multi-cell
            // halftone (HalftoneType 6, 10, 16; F6), or a metadata / embedded-file stream.
            if int("FunctionType") == Some(0)
                || int("ShadingType").is_some_and(|t| (4..=7).contains(&t))
                || int("HalftoneType").is_some_and(|t| matches!(t, 6 | 10 | 16))
                || d.get("Type").map(|t| res(objs, t)).is_some_and(|t| t.is_name("Metadata") || t.is_name("EmbeddedFile"))
            {
                out.insert(*num);
            }
        }
        binary_refs(&e.obj, &mut out);
    }
    out
}

/// Image streams: `/Subtype` resolves to `/Image`, and the `/SMask` and `/Mask` streams of those.
fn image_streams(objs: &Objs) -> BTreeSet<u32> {
    let mut out = BTreeSet::new();
    for (num, e) in objs {
        if let Object::Stream(s) = &e.obj
            && s.dict.get("Subtype").map(|t| res(objs, t)).is_some_and(|t| t.is_name("Image"))
        {
            out.insert(*num);
            for key in ["SMask", "Mask"] {
                if let Some(r) = s.dict.get(key).and_then(Object::as_ref)
                    && matches!(objs.get(&r.num).map(|e| &e.obj), Some(Object::Stream(_)))
                {
                    out.insert(r.num);
                }
            }
        }
    }
    out
}

/// The XObject-like dictionary of an inline image: full key names, abbreviated filter names spelled
/// out (the filters accept both).
fn inline_image_dict(operands: &[Object]) -> Dict {
    let mut d = inline_dict(operands);
    let expand = |o: &Object| -> Object {
        match o {
            Object::Name(n) => Object::Name(inline_value_name(n).to_vec()),
            Object::Array(a) => Object::Array(a.iter().map(|x| x.as_name().map_or_else(|| x.clone(), |n| Object::Name(inline_value_name(n).to_vec()))).collect()),
            other => other.clone(),
        }
    };
    if let Some(f) = d.get("Filter").map(expand) {
        d.set("Filter", f);
    }
    d
}

/// The last filter name of a dictionary's chain, when it is a raster codec.
fn codec_of(d: &Dict) -> Option<Vec<u8>> {
    let last = match d.get("Filter")? {
        Object::Name(n) => n.clone(),
        Object::Array(a) => a.last()?.as_name()?.to_vec(),
        _ => return None,
    };
    is_raster_filter(&last).then_some(last)
}

/// Bounds of one content stream (module docs) and, when `images` is set, of its inline images.
/// `max_nodes` caps the parsed operands of the stream (F4: an array operand amplifies memory far
/// past the operator cap); `usize::MAX` leaves it unmetered (the flatten, which isolates one page).
fn content_bounds(objs: &Objs, data: &[u8], resources: &[Option<&Dict>], doc_comps: u64, limits: &Limits, budget: &mut usize, images: bool, max_nodes: usize, check_backing: bool) -> Result<()> {
    let cs = |n: &[u8]| resources.iter().find_map(|r| resource_components(objs, *r, n));
    let parsed = match content::tokenize_bounded(data, limits, &cs, budget, max_nodes) {
        Ok(p) => p,
        Err(PressError::InputLimit("depth")) => return Err(refuse(FlattenCause::NestingDepth)),
        Err(e @ (PressError::Canceled | PressError::InputLimit("decoded_total" | "decoded_bytes" | "nodes" | "operands" | "max_content_ops"))) => return Err(e),
        Err(_) => return Err(refuse(FlattenCause::RendererInput)),
    };
    if !images {
        return Ok(());
    }
    for op in parsed.ops.iter().filter(|o| o.name == b"BI") {
        let Some((a, b)) = op.inline_data else { continue };
        let raw = data.get(a..b).unwrap_or(&[]);
        let dict = inline_image_dict(&op.operands);
        let stream = Stream { dict: dict.clone(), data: raw.to_vec() };
        let (bytes, codec) = match codec_of(&dict) {
            Some(c) => match raster::decode_prefix(&stream, limits, budget) {
                Ok(d) => (d.data, Some(c)),
                Err(e @ (PressError::Canceled | PressError::InputLimit(_))) => return Err(e),
                Err(_) => (Vec::new(), Some(c)),
            },
            None if filters::is_decodable(&dict) => match filters::decode_chain(&stream, limits, budget) {
                Ok(d) => (d.data, None),
                Err(e @ (PressError::Canceled | PressError::InputLimit(_))) => return Err(e),
                Err(_) => (Vec::new(), None),
            },
            None => (Vec::new(), None),
        };
        image_bounds(objs, &ImageInput { dict: &dict, data: &bytes, codec }, resources, doc_comps, limits, check_backing)?;
    }
    Ok(())
}

/// Checks the isolated document (module docs). `page_contents` and `page_resources` are those of the
/// isolated page (not yet in `objs`).
pub(crate) fn check(objs: &Objs, page_contents: &[u32], page_resources: Option<&Dict>, limits: &Limits, cancel: &CancellationToken) -> Result<()> {
    let contents: BTreeMap<u32, Option<&Dict>> = page_contents.iter().map(|n| (*n, page_resources)).collect();
    bounds(objs, &contents, page_resources, true, usize::MAX, limits, cancel)
}

/// The bounds of [`check`] over `objs`. `contents`: page content streams with their page's
/// resources; `fallback`: the resources every other content-role stream also reads (the flatten's
/// one page); `images`: whether image sizes are bounded too.
fn bounds(objs: &Objs, contents: &BTreeMap<u32, Option<&Dict>>, fallback: Option<&Dict>, images_too: bool, max_content_nodes: usize, limits: &Limits, cancel: &CancellationToken) -> Result<()> {
    bounds_with_extra(objs, contents, fallback, images_too, max_content_nodes, true, &BTreeSet::new(), limits, cancel)
}

/// [`bounds`] plus `extra_binary` stream numbers the caller knows are data (F6: binary roles the
/// walk of `objs` alone misses, from the resolved page resources and the catalog OC properties), and
/// `check_backing` (F5: the reader pass leaves it off and the pixel bound caps the image).
#[allow(clippy::too_many_arguments)]
fn bounds_with_extra(objs: &Objs, contents: &BTreeMap<u32, Option<&Dict>>, fallback: Option<&Dict>, images_too: bool, max_content_nodes: usize, check_backing: bool, extra_binary: &BTreeSet<u32>, limits: &Limits, cancel: &CancellationToken) -> Result<()> {
    let cap = limits.depth_cap();
    let roles = content_role_streams(objs);
    let mut binary = binary_streams(objs);
    binary.extend(extra_binary.iter().copied());
    let images = image_streams(objs);
    let doc_comps = if images_too { document_components(objs) } else { 4 };
    if !construction_chains_within(objs, cap) {
        return Err(refuse(FlattenCause::NestingDepth));
    }
    let mut budget = limits.max_decoded_total;
    for (i, (num, e)) in objs.iter().enumerate() {
        if i & 63 == 0 {
            check_cancel(cancel)?;
        }
        let Object::Stream(s) = &e.obj else { continue };
        let own = s.dict.get("Resources").map(|r| res(objs, r)).and_then(Object::as_dict);
        let role = roles.get(num).map(|r| r.resources);
        let page = contents.get(num);
        if page.is_some() || role.is_some() {
            let resources = [role.flatten().or(own), page.copied().unwrap_or(fallback)];
            content_bounds(objs, &s.data, &resources, doc_comps, limits, &mut budget, images_too, max_content_nodes, check_backing)?;
        } else if !binary.contains(num) && !images.contains(num) && !syntax_nesting_within(&s.data, cap) {
            return Err(refuse(FlattenCause::NestingDepth));
        }
        if images_too && images.contains(num) {
            let codec = codec_of(&s.dict);
            image_bounds(objs, &ImageInput { dict: &s.dict, data: &s.data, codec }, &[own, page.copied().unwrap_or(fallback)], doc_comps, limits, check_backing)?;
        }
    }
    Ok(())
}

/// Reader-bounds-pass limits (SMB-PARSER-HOSTILE-1 F5/F7, lead ruling 2026-11-08). A bound here
/// stops hostile amplification (small input, huge output, deep recursion); it never refuses a big
/// legitimate print file.
///
/// Largest bytes one syntax stream may decode to; above it the decode is `InputLimit("decoded_bytes")`.
const READER_STREAM_CEILING: usize = 2 << 30;
/// A syntax stream over this size whose decoded/encoded ratio is above [`READER_MAX_RATIO`] is a
/// decompression bomb: `InputLimit("decoded_ratio")`. Below it, any ratio is read (a small legitimate
/// stream can compress very well).
const READER_RATIO_FLOOR: usize = 256 << 20;
const READER_MAX_RATIO: usize = 1024;
/// Decoded bytes one call (one page for render, the whole closure for vector read) may charge across
/// its syntax streams, and separately across its images: there is no whole-document total (F7).
const READER_CALL_BUDGET: usize = 4 << 30;
/// Largest image one page may draw, in pixels, at any colour space and bit depth (F5): the pixel
/// bound alone caps what hayro allocates per image.
const READER_IMAGE_PIXELS: usize = 150_000_000;
/// Parsed operand nodes per content stream (F4) and operators per content stream (F7).
const READER_MAX_CONTENT_NODES: usize = 50_000_000;
const READER_MAX_CONTENT_OPS: usize = 50_000_000;

/// The tuned [`Limits`] the reader pass hands [`bounds_with_extra`]: the document's nesting depth and
/// byte budgets, but the reader's own content-operator cap, a 4 GiB per-call decode budget, the
/// 150 Mpx image pixel cap expressed through `max_decoded_bytes`, and the renderer-memory formula
/// disabled (`max_render_image_bytes` = `u64::MAX`) so a 150 Mpx image passes at any colour space and
/// bit depth.
fn reader_limits(doc_limits: &Limits) -> Limits {
    Limits {
        max_content_ops: READER_MAX_CONTENT_OPS,
        max_decoded_bytes: READER_IMAGE_PIXELS,
        max_decoded_total: READER_CALL_BUDGET,
        max_render_image_bytes: u64::MAX,
        ..*doc_limits
    }
}

/// A stream hayro parses as PDF or PostScript syntax, decoded leniently for the reader pass
/// (SMB-PARSER-HOSTILE-1 F7): a damaged Flate tail keeps its prefix (`warning` true) and is not a
/// refusal; a decompression bomb and the per-call budget are. A filter this crate does not decode
/// cannot be bounded and is refused (hayro's own decoders could hand the parser text this pass never
/// saw). Returns `(bytes, warning)`.
fn reader_syntax_data(s: &Stream, budget: &mut usize) -> Result<(Vec<u8>, bool)> {
    if s.dict.get("Filter").is_none() && s.dict.get("F").is_none() {
        let data = s.data.clone();
        *budget = budget.checked_sub(data.len()).ok_or(PressError::InputLimit("decoded_total"))?;
        return Ok((data, false));
    }
    if !filters::is_decodable(&s.dict) {
        return Err(refuse(FlattenCause::RendererInput));
    }
    let encoded = s.data.len().max(1);
    // A stream this size may decode to at most this much before its ratio passes READER_MAX_RATIO; a
    // bomb is stopped at the cap, so its decode never allocates past ~READER_RATIO_FLOOR (the ratio
    // rule allows any ratio only below that floor). A legitimate big low-ratio stream caps at the
    // per-stream ceiling.
    let ratio_cap = encoded.saturating_mul(READER_MAX_RATIO).max(READER_RATIO_FLOOR).min(READER_STREAM_CEILING);
    let (data, warning) = match filters::decode_chain_lenient(s, ratio_cap) {
        Ok(out) => out,
        // Past the ratio cap below the hard ceiling: a decompression bomb.
        Err(PressError::InputLimit("decoded_bytes")) if ratio_cap < READER_STREAM_CEILING => return Err(PressError::InputLimit("decoded_ratio")),
        Err(e) => return Err(e),
    };
    *budget = budget.checked_sub(data.len()).ok_or(PressError::InputLimit("decoded_total"))?;
    Ok((data, warning))
}

/// The pass's causes as the readers' errors.
fn reader_error(e: PressError) -> PressError {
    match e {
        PressError::FlattenRefused(FlattenCause::NestingDepth) => PressError::InputLimit("depth"),
        PressError::FlattenRefused(FlattenCause::ImageBudget) => PressError::InputLimit("render_image"),
        PressError::FlattenRefused(_) => PressError::Malformed("reader_input"),
        other => other,
    }
}

/// The bounds of [`check`] for the readers that hand the ORIGINAL file bytes to hayro (SMB-PARSER-
/// HOSTILE-1 item 4): `vector_read.rs` `read` (`page` `None`: every page, nesting only, because the
/// vector recorder never decodes an image: hayro-interpret 0.7.0 `ImageXObject::new` reads no
/// sample and `Recorder::draw_image` only notes it) and press-write `render_page` (`page`
/// `Some(index)`: that page, nesting and image sizes, because its painter decodes images).
///
/// Run on `doc`, Press's own reading of the same bytes, before hayro sees a byte: every object
/// reachable from the pages' (inherited) `/Resources`, `/Contents` and `/Group` and from the
/// catalog's `/OCProperties`; every stream hayro parses as syntax (page contents, content-role
/// streams, every stream not in a binary or image role) decoded with this crate's decoders, and
/// refused when it cannot be (a filter it does not decode, data it cannot decode: hayro's own
/// decoders could hand the parser text this pass never saw); image streams are size-bounded from
/// their dictionary and codec header on the render route (`page` `Some`) at up to 150 Mpx, never
/// decoded whole. Errors: nesting or a construction cycle `InputLimit("depth")`, an image past
/// 150 Mpx `InputLimit("render_image")`, a decompression bomb `InputLimit("decoded_ratio")`, the
/// per-call 4 GiB budget `InputLimit("decoded_total")`, a content stream past 50 M operators or
/// nodes `InputLimit("max_content_ops"|"nodes")`, an unreadable syntax stream
/// `Malformed("reader_input")`, cancellation `Canceled`. A damaged Flate tail keeps its prefix and
/// is not a refusal (F7). Encryption is out of scope (F3, lead ruling 2026-11-08): this crate cannot
/// decrypt, so an encrypted file is passed through unbounded, the behaviour before this lane (hayro
/// decrypts and reads it). The callers refuse a file `PdfDoc::open` refuses (lead ruling OQ1).
/// Residual (F8): hayro reads the ORIGINAL bytes through its own xref and page tree; where its xref
/// handling diverges from Press's (a lazy repair, a page Press's walk does not list) the objects
/// hayro then reads were not bounded here. Tests `press/tests/hostile_vector_read.rs`, the mutation
/// sweep `press/tests/hostile_mutation_pdf.rs`.
#[doc(hidden)]
pub fn reader_input_within(doc: &PdfDoc, page: Option<usize>, cancel: &CancellationToken) -> Result<()> {
    // F3: Press cannot decrypt (reader.rs does not expand an encrypted file's object streams), so an
    // encrypted file cannot be bounded here; pass it through, the behaviour before this lane.
    if doc.encrypted {
        return Ok(());
    }
    let limits = reader_limits(&doc.limits);
    let pages: Vec<u32> = match page {
        Some(i) => vec![*doc.pages.get(i).ok_or(PressError::NoSuchPage)?],
        None => doc.pages.clone(),
    };
    let mut work: Vec<u32> = Vec::new();
    let mut contents: BTreeMap<u32, Option<&Dict>> = BTreeMap::new();
    // F6: ICC / font / halftone streams referenced directly from a page's (possibly inline) resources
    // or group, which `binary_streams`' walk of `objs` alone misses.
    let mut extra_binary: BTreeSet<u32> = BTreeSet::new();
    for &p in &pages {
        let pd = doc.dict_of(p).ok_or(PressError::Malformed("page_node"))?;
        let resources = doc.inherited(p, "Resources").filter(|o| matches!(o, Object::Dict(_)));
        let page_contents = pd.get("Contents");
        let group = pd.get("Group");
        for o in [resources, page_contents, group].into_iter().flatten() {
            refs_in(o, &mut work);
        }
        for o in [resources, group].into_iter().flatten() {
            binary_refs(o, &mut extra_binary);
        }
        let nums: Vec<u32> = match page_contents.map(|c| (c, doc.resolve(c))) {
            Some((_, Object::Array(a))) => a.iter().filter_map(Object::as_ref).map(|r| r.num).collect(),
            Some((Object::Ref(r), Object::Stream(_))) => vec![r.num],
            _ => Vec::new(),
        };
        for n in nums {
            contents.entry(n).or_insert(resources.and_then(Object::as_dict));
        }
    }
    if let Some(o) = doc.catalog().ok().and_then(|c| c.get("OCProperties")) {
        refs_in(o, &mut work);
        binary_refs(o, &mut extra_binary);
    }
    let mut objs: Objs = BTreeMap::new();
    while let Some(num) = work.pop() {
        if objs.contains_key(&num) {
            continue;
        }
        let Some(e) = doc.objects.get(&num) else { continue };
        if objs.len() & 255 == 0 {
            check_cancel(cancel)?;
        }
        refs_in(&e.obj, &mut work);
        objs.insert(num, Entry { generation: e.generation, obj: e.obj.clone(), span: None, data_at: None });
    }
    let roles: BTreeSet<u32> = content_role_streams(&objs).keys().copied().collect();
    let mut binary = binary_streams(&objs);
    binary.extend(extra_binary.iter().copied());
    let images = image_streams(&objs);
    let images_too = page.is_some();
    // F7: one 4 GiB budget per call for the syntax streams (images charge their own budget in bounds).
    let mut budget = READER_CALL_BUDGET;
    for (i, (num, e)) in objs.iter_mut().enumerate() {
        if i & 63 == 0 {
            check_cancel(cancel)?;
        }
        let Object::Stream(s) = &mut e.obj else { continue };
        // Parsed as syntax: page contents, content-role streams, and every stream not a binary or
        // image role. Images are size-bounded from their header below, never decoded here (F5).
        let parsed = contents.contains_key(num) || roles.contains(num) || !(binary.contains(num) || images.contains(num));
        if parsed {
            let (data, _warning) = reader_syntax_data(s, &mut budget).map_err(reader_error)?;
            s.dict.remove("Filter");
            s.dict.remove("DecodeParms");
            s.data = data;
        }
    }
    bounds_with_extra(&objs, &contents, None, images_too, READER_MAX_CONTENT_NODES, false, &extra_binary, &limits, cancel).map_err(reader_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::ObjRef;

    fn entry(obj: Object) -> Entry {
        Entry { generation: 0, obj, span: None, data_at: None }
    }

    fn image(dict: &[(&str, Object)], data: &[u8]) -> Objs {
        let mut d = Dict::default();
        d.set("Subtype", Object::Name(b"Image".to_vec()));
        for (k, v) in dict {
            d.set(k, v.clone());
        }
        let mut m = Objs::new();
        m.insert(1, entry(Object::Stream(Box::new(Stream { dict: d, data: data.to_vec() }))));
        m
    }

    fn checked(objs: &Objs) -> Result<()> {
        check(objs, &[], None, &Limits::default(), &CancellationToken::default())
    }

    #[test]
    fn nesting_scans_count_brackets_procedures_and_dictionary_runs() {
        assert!(syntax_nesting_within(b"[1 [2 [3]]] << /A << /B 1 >> >> { { 1 } if }", 3));
        assert!(!syntax_nesting_within(&[b'['; 40], 32), "40 nested arrays");
        assert!(!syntax_nesting_within(&[b'{'; 40], 32), "40 nested procedures (Type 4)");
        let run: Vec<u8> = b"<< ".repeat(40);
        assert!(!syntax_nesting_within(&run, 32), "a run of 40 << tokens");
        let closers: Vec<u8> = b">>".repeat(40);
        assert!(!syntax_nesting_within(&closers, 32), "a run of 40 >> tokens recurses in hayro-postscript");
        // Strings, hex strings and comments hide their brackets.
        let mut quiet = b"(".to_vec();
        quiet.extend_from_slice(&[b'['; 100]);
        quiet.extend_from_slice(b") <5B5B5B> % [[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[[\n");
        assert!(syntax_nesting_within(&quiet, 2));
        // Escaped parentheses do not end a string early.
        let mut escaped = b"(\\) ".to_vec();
        escaped.extend_from_slice(&[b'['; 100]);
        escaped.push(b')');
        assert!(syntax_nesting_within(&escaped, 2));
    }

    #[test]
    fn deep_streams_outside_content_roles_are_refused() {
        let mut m = Objs::new();
        let mut d = Dict::default();
        d.set("FunctionType", Object::Int(4));
        let mut data = vec![b'{'; 5000];
        data.extend(vec![b'}'; 5000]);
        m.insert(7, entry(Object::Stream(Box::new(Stream { dict: d, data }))));
        assert_eq!(checked(&m), Err(PressError::FlattenRefused(FlattenCause::NestingDepth)));
        // A font program is binary: not scanned.
        let mut fd = Dict::default();
        fd.set("Type", Object::Name(b"FontDescriptor".to_vec()));
        fd.set("FontFile2", Object::Ref(ObjRef { num: 7, generation: 0 }));
        m.insert(8, entry(Object::Dict(fd)));
        assert_eq!(checked(&m), Ok(()));
    }

    #[test]
    fn colour_space_and_function_cycles_are_refused() {
        let r = |n: u32| Object::Ref(ObjRef { num: n, generation: 0 });
        let name = |n: &str| Object::Name(n.as_bytes().to_vec());
        // An /Indexed space whose base is itself.
        let mut m = Objs::new();
        m.insert(5, entry(Object::Array(vec![name("Indexed"), r(5), Object::Int(0), Object::Str(vec![0])])));
        assert_eq!(checked(&m), Err(PressError::FlattenRefused(FlattenCause::NestingDepth)));
        // A stitching function that lists itself.
        let mut f = Dict::default();
        f.set("FunctionType", Object::Int(3));
        f.set("Functions", Object::Array(vec![r(7)]));
        let mut m = Objs::new();
        m.insert(7, entry(Object::Dict(f)));
        assert_eq!(checked(&m), Err(PressError::FlattenRefused(FlattenCause::NestingDepth)));
        // A chain of 40 /Separation alternates (no cycle) is past the default 32 levels.
        let mut m = Objs::new();
        for n in 1..=40u32 {
            m.insert(n, entry(Object::Array(vec![name("Separation"), name("S"), if n < 40 { r(n + 1) } else { name("DeviceGray") }, Object::Null])));
        }
        assert_eq!(checked(&m), Err(PressError::FlattenRefused(FlattenCause::NestingDepth)));
        // An ordinary /Indexed over /ICCBased with an /Alternate: built.
        let mut icc = Dict::default();
        icc.set("N", Object::Int(3));
        icc.set("Alternate", name("DeviceRGB"));
        let mut m = Objs::new();
        m.insert(3, entry(Object::Stream(Box::new(Stream { dict: icc, data: Vec::new() }))));
        m.insert(4, entry(Object::Array(vec![name("Indexed"), Object::Array(vec![name("ICCBased"), r(3)]), Object::Int(0), Object::Str(vec![0, 0, 0])])));
        assert_eq!(checked(&m), Ok(()));
    }

    #[test]
    fn dims_u32_max() {
        // Abbreviated keys are read as hayro reads them: /W /H u32::MAX on a one-byte image.
        let max = Object::Int(i64::from(u32::MAX));
        let m = image(&[("W", max.clone()), ("H", max), ("CS", Object::Name(b"G".to_vec())), ("BPC", Object::Int(8))], &[0]);
        assert_eq!(checked(&m), Err(PressError::FlattenRefused(FlattenCause::ImageBudget)));
        // The full spelling small, the abbreviation huge: the larger one counts.
        let m = image(&[("Width", Object::Int(1)), ("W", Object::Int(70_000)), ("Height", Object::Int(70_000)), ("ColorSpace", Object::Name(b"DeviceGray".to_vec()))], &[0]);
        assert_eq!(checked(&m), Err(PressError::FlattenRefused(FlattenCause::ImageBudget)));
        // Real-valued dimensions round up.
        let m = image(&[("Width", Object::Real(1.0e30)), ("Height", Object::Int(1)), ("ColorSpace", Object::Name(b"DeviceGray".to_vec()))], &[0]);
        assert_eq!(checked(&m), Err(PressError::FlattenRefused(FlattenCause::ImageBudget)));
    }

    #[test]
    fn image_memory_and_data_length_are_bounded() {
        let gray = Object::Name(b"DeviceGray".to_vec());
        // Exactly backed: 4 x 2 gray at 8 bits = 8 bytes.
        assert_eq!(checked(&image(&[("Width", Object::Int(4)), ("Height", Object::Int(2)), ("ColorSpace", gray.clone()), ("BitsPerComponent", Object::Int(8))], &[0; 8])), Ok(()));
        // One byte short.
        assert_eq!(
            checked(&image(&[("Width", Object::Int(4)), ("Height", Object::Int(2)), ("ColorSpace", gray.clone()), ("BitsPerComponent", Object::Int(8))], &[0; 7])),
            Err(PressError::FlattenRefused(FlattenCause::ImageBudget))
        );
        // Within max_decoded_bytes pixels but over the renderer memory estimate: 8000 x 8000 CMYK.
        let cmyk = image(&[("Width", Object::Int(8000)), ("Height", Object::Int(8000)), ("ColorSpace", Object::Name(b"DeviceCMYK".to_vec())), ("Filter", Object::Name(b"DCTDecode".to_vec()))], &[0xFF, 0xD8]);
        assert_eq!(checked(&cmyk), Err(PressError::FlattenRefused(FlattenCause::ImageBudget)));
        // An unresolved colour space counts as the document's largest component count.
        let mut m = image(&[("Width", Object::Int(100)), ("Height", Object::Int(100)), ("ColorSpace", Object::Name(b"CS9".to_vec()))], &[0; 10_000]);
        m.insert(2, entry(Object::Array(vec![Object::Name(b"DeviceN".to_vec()), Object::Array(vec![Object::Name(b"A".to_vec()); 40_000]), Object::Name(b"DeviceGray".to_vec()), Object::Null])));
        assert_eq!(checked(&m), Err(PressError::FlattenRefused(FlattenCause::ImageBudget)));
    }

    #[test]
    fn codec_headers_size_the_image() {
        // JPEG SOF0 declaring 30000 x 30000 x 3 under a 10 x 10 dictionary.
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x11, 0x08];
        jpeg.extend_from_slice(&30_000u16.to_be_bytes());
        jpeg.extend_from_slice(&30_000u16.to_be_bytes());
        jpeg.push(3);
        assert_eq!(codec_dims(b"DCTDecode", &jpeg, &Dict::default()), Some((30_000, 30_000, 3)));
        // JPEG 2000 codestream SIZ 50000 x 50000, 4 components.
        let mut j2k = vec![0xFF, 0x4F, 0xFF, 0x51, 0x00, 0x2F, 0x00, 0x00];
        j2k.extend_from_slice(&50_000u32.to_be_bytes());
        j2k.extend_from_slice(&50_000u32.to_be_bytes());
        j2k.extend_from_slice(&[0; 24]);
        j2k.extend_from_slice(&4u16.to_be_bytes());
        assert_eq!(codec_dims(b"JPXDecode", &j2k, &Dict::default()), Some((50_000, 50_000, 4)));
        let m = image(&[("Width", Object::Int(10)), ("Height", Object::Int(10)), ("Filter", Object::Name(b"JPXDecode".to_vec()))], &j2k);
        assert_eq!(checked(&m), Err(PressError::FlattenRefused(FlattenCause::ImageBudget)));
        // JBIG2 page information segment 60000 x 60000 (segment 0, type 48, page 1, 19 data bytes).
        let mut jb = vec![0, 0, 0, 0, 48, 0, 1, 0, 0, 0, 19];
        jb.extend_from_slice(&60_000u32.to_be_bytes());
        jb.extend_from_slice(&60_000u32.to_be_bytes());
        jb.extend_from_slice(&[0; 11]);
        assert_eq!(codec_dims(b"JBIG2Decode", &jb, &Dict::default()), Some((60_000, 60_000, 1)));
        // CCITT: /Columns and /Rows of the decode parameters.
        let mut d = Dict::default();
        d.set("Filter", Object::Name(b"CCITTFaxDecode".to_vec()));
        let mut p = Dict::default();
        p.set("Columns", Object::Int(100_000));
        p.set("Rows", Object::Int(100_000));
        d.set("DecodeParms", Object::Dict(p));
        assert_eq!(codec_dims(b"CCITTFaxDecode", &[], &d), Some((100_000, 100_000, 1)));
    }
}
