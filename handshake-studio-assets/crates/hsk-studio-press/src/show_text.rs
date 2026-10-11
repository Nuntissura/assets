//! Text drawn by content streams (page content, form XObjects, annotation appearances), read
//! through each font's `/ToUnicode` CMap where there is one. Two views of the same strings:
//! `all` is what a redaction scan matches (single-byte reading plus ToUnicode text); `mapped` is the
//! text that ONLY a ToUnicode map reveals (composite-font codes, custom encodings), which a byte
//! scan cannot see and the payload proof therefore adds.
//! Bounded: a ToUnicode stream is decoded under the caller's decoded-byte budget (aggregate), parsed
//! by `cmap` (entry and range limits), and cached per font object.
#![deny(clippy::arithmetic_side_effects)]
use crate::cmap::{Cmap, parse_to_unicode};
use crate::content::{Op, parse_content};
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::reader::Entry;
use crate::redaction::res;
use crate::syntax::{Dict, Object};
use std::collections::BTreeMap;
use std::rc::Rc;

pub(crate) type Objs = BTreeMap<u32, Entry>;

pub(crate) enum FontText {
    /// Single-byte codes read as Latin-1 (what the byte-level proof sees).
    Simple,
    /// `/ToUnicode` decodes the codes.
    Cmap { cmap: Cmap, composite: bool },
    /// Composite font without a usable `/ToUnicode`: the codes cannot be read.
    Opaque,
}

/// Fonts already read, by font object number.
#[derive(Default)]
pub(crate) struct FontCache(BTreeMap<u32, Rc<FontText>>);

pub(crate) struct Shown {
    pub all: Vec<char>,
    pub mapped: Vec<char>,
    /// Text was drawn with a composite font that has no usable `/ToUnicode`.
    pub opaque: bool,
}

fn latin1(s: &[u8]) -> Vec<char> {
    s.iter().map(|b| char::from(*b)).collect()
}

fn strings<'o>(o: &'o Object, out: &mut Vec<&'o [u8]>) {
    match o {
        Object::Str(s) => out.push(s),
        Object::Array(a) => a.iter().for_each(|x| strings(x, out)),
        _ => {}
    }
}

fn read_font(objs: &Objs, fd: &Dict, limits: &Limits, budget: &mut usize) -> Result<FontText> {
    let composite = fd.get("Subtype").is_some_and(|s| s.is_name("Type0"));
    if let Some(Object::Stream(tu)) = fd.get("ToUnicode").map(|v| res(objs, v))
        && filters::is_decodable(&tu.dict)
    {
        match filters::decode_stream(tu, limits, budget) {
            Ok(data) => {
                if let Some(cmap) = parse_to_unicode(&data) {
                    return Ok(FontText::Cmap { cmap, composite });
                }
            }
            Err(e @ (PressError::InputLimit(_) | PressError::Canceled)) => return Err(e),
            Err(_) => {}
        }
    }
    Ok(if composite { FontText::Opaque } else { FontText::Simple })
}

/// The font a resource name selects (`None`: not a font dictionary).
pub(crate) fn lookup_font(
    objs: &Objs,
    fonts: Option<&Dict>,
    name: &[u8],
    limits: &Limits,
    budget: &mut usize,
    cache: &mut FontCache,
) -> Result<Option<Rc<FontText>>> {
    let Some(v) = fonts.and_then(|f| f.get_key(name)) else { return Ok(None) };
    let key = if let Object::Ref(r) = v { Some(r.num) } else { None };
    if let Some(k) = key
        && let Some(hit) = cache.0.get(&k)
    {
        return Ok(Some(Rc::clone(hit)));
    }
    let Some(fd) = res(objs, v).as_dict() else { return Ok(None) };
    let font = Rc::new(read_font(objs, fd, limits, budget)?);
    if let Some(k) = key {
        cache.0.insert(k, Rc::clone(&font));
    }
    Ok(Some(font))
}

/// Text shown by the show operators of already tokenized content.
pub(crate) fn show_text_ops(
    objs: &Objs,
    resources: Option<&Dict>,
    ops: &[Op],
    limits: &Limits,
    budget: &mut usize,
    cache: &mut FontCache,
) -> Result<Shown> {
    let fonts = resources.and_then(|r| r.get("Font")).map(|f| res(objs, f)).and_then(Object::as_dict);
    let mut shown = Shown { all: Vec::new(), mapped: Vec::new(), opaque: false };
    let mut cur: Option<Rc<FontText>> = None;
    for op in ops {
        match op.name.as_slice() {
            b"Tf" => {
                cur = match op.operands.first().and_then(Object::as_name) {
                    Some(n) => lookup_font(objs, fonts, n, limits, budget, cache)?,
                    None => None,
                };
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => {
                let mut strs = Vec::new();
                op.operands.iter().for_each(|o| strings(o, &mut strs));
                for s in strs {
                    match cur.as_deref() {
                        Some(FontText::Cmap { cmap, composite }) => {
                            let decoded = cmap.decode(s);
                            let raw = latin1(s);
                            shown.all.extend(decoded.iter().copied());
                            if !*composite {
                                shown.all.push(' ');
                                shown.all.extend(raw.iter().copied());
                            }
                            if *composite || decoded != raw {
                                shown.mapped.extend(decoded);
                                shown.mapped.push(' ');
                            }
                        }
                        Some(FontText::Opaque) => {
                            shown.opaque |= !s.is_empty();
                            shown.all.extend(latin1(s));
                        }
                        Some(FontText::Simple) | None => shown.all.extend(latin1(s)),
                    }
                }
                shown.all.push(' ');
            }
            _ => {}
        }
    }
    Ok(shown)
}

/// [`show_text_ops`] over raw content bytes; unparsable content is read as raw bytes.
pub(crate) fn show_text(
    objs: &Objs,
    resources: Option<&Dict>,
    data: &[u8],
    limits: &Limits,
    budget: &mut usize,
    cache: &mut FontCache,
) -> Result<Shown> {
    match parse_content(data, limits) {
        Ok(ops) => show_text_ops(objs, resources, &ops, limits, budget, cache),
        Err(_) => Ok(Shown { all: latin1(data), mapped: Vec::new(), opaque: false }),
    }
}

/// The `/Resources` that apply to a page: its own or the nearest ancestor's.
fn inherited_resources<'a>(objs: &'a Objs, page: &'a Dict) -> Option<&'a Dict> {
    let mut d = page;
    for _ in 0..32 {
        if let Some(r) = d.get("Resources").map(|r| res(objs, r)).and_then(Object::as_dict) {
            return Some(r);
        }
        d = d.get("Parent").map(|p| res(objs, p)).and_then(Object::as_dict)?;
    }
    None
}

/// Resources dictionary that applies to each page-content stream and form XObject, by object
/// number (legacy forms without their own `/Resources` are not mapped).
pub(crate) fn content_resources(objs: &Objs) -> BTreeMap<u32, &Dict> {
    let mut out = BTreeMap::new();
    for (num, e) in objs {
        match &e.obj {
            Object::Stream(s) if s.dict.get("Subtype").is_some_and(|t| t.is_name("Form")) => {
                if let Some(r) = s.dict.get("Resources").map(|r| res(objs, r)).and_then(Object::as_dict) {
                    out.insert(*num, r);
                }
            }
            Object::Dict(d) if d.get("Type").is_some_and(|t| t.is_name("Page")) => {
                let (Some(r), Some(c)) = (inherited_resources(objs, d), d.get("Contents")) else { continue };
                let mut refs: Vec<u32> = Vec::new();
                match c {
                    Object::Ref(rf) => match objs.get(&rf.num).map(|x| &x.obj) {
                        Some(Object::Array(a)) => refs.extend(a.iter().filter_map(Object::as_ref).map(|x| x.num)),
                        _ => refs.push(rf.num),
                    },
                    Object::Array(a) => refs.extend(a.iter().filter_map(Object::as_ref).map(|x| x.num)),
                    _ => {}
                }
                for n in refs {
                    out.insert(n, r);
                }
            }
            _ => {}
        }
    }
    out
}
