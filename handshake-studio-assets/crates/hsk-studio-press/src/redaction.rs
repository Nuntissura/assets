//! Irreversible region redaction for content that can be proven removed:
//! text-showing operators whose glyph run may touch the region are deleted from the page content
//! stream (the page then points at a NEW stream object; the old stream is unreachable and dropped
//! by the full save), and annotations whose `/Rect` touches the region are removed together with
//! every form-field reference to them. Anything else painted in the region (images, vectors,
//! form XObjects, shadings, inline images) cannot be removed with proof in this slice: by default
//! the operation REFUSES (fail closed) with `UnsupportedRedactionClass`.
//!
//! Conservative geometry: a glyph run's box spans its advance (exact when the font has `/Widths`,
//! otherwise 1000/1000 em per byte, an upper bound for the standard fonts) and
//! `[-0.25, +0.95]` em vertically, transformed through Tm x CTM. Whole strings are removed (a
//! string straddling the region edge is removed entirely). Over-removal is safe; under-removal is
//! not.
//! Images (image XObjects and inline images) cannot be erased pixel by pixel here: under
//! `UnsupportedPolicy::RemoveImages` an image whose placement touches the region is removed whole
//! (its `Do` / `BI..EI` leaves the content, the resource entry goes when nothing else uses it), under
//! the other policies it is an unsupported class. No pixel codec is involved.
#![deny(clippy::arithmetic_side_effects)]
use crate::cmap::{Cmap, parse_to_unicode};
use crate::content::{Op, parse_content};
use crate::error::{PressError, RedactionClass, Result};
use crate::filters;
use crate::pdf_edit::PdfDoc;
use crate::reader::{Entry, check_cancel};
use crate::syntax::{Dict, ObjRef, Object, Stream};
use crate::writer::{reachable, trailer_roots, write_object};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

const ASCENT: f64 = 0.95;
const DESCENT: f64 = 0.25;

/// Redaction target: a rectangle `[x0, y0, x1, y1]` in the page's default user space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RedactionRegion {
    pub page_index: u32,
    pub rect: [f64; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum UnsupportedPolicy {
    /// Fail closed with `UnsupportedRedactionClass` (default).
    #[default]
    Refuse,
    /// Redact what can be proven and list the rest in `RedactionReport::unredacted`.
    ReportAndContinue,
    /// Images and inline images whose placement touches a region are removed whole (listed in
    /// `RedactionReport::images_removed` / `inline_images_removed`); every other unsupported class
    /// still fails closed.
    RemoveImages,
}

/// SENSITIVE: `removed_text` holds the removed string operands; route it only to the verifier.
#[derive(Clone, Debug, PartialEq)]
pub struct RedactionReport {
    pub page_index: u32,
    pub text_strings_removed: u32,
    pub removed_text: Vec<Vec<u8>>,
    /// `removed_text[i]` as its font's `/ToUnicode` reads it (`None`: the font has no usable map).
    pub removed_unicode: Vec<Option<String>>,
    pub annotations_removed: Vec<ObjRef>,
    /// Image XObjects whose placement touched the region and was removed (`RemoveImages`).
    pub images_removed: Vec<ObjRef>,
    /// Inline images removed (`RemoveImages`).
    pub inline_images_removed: u32,
    pub form_fields_removed: u32,
    /// Classes present in the region that were NOT removed (only with `ReportAndContinue`).
    pub unredacted: Vec<RedactionClass>,
    /// A removed string had unknown glyph widths, so following text on its line may have moved.
    pub layout_shift_possible: bool,
    /// Every mark in the region was removed with proof (`unredacted` is empty).
    pub complete: bool,
}

type Mat = [f64; 6];
const IDENT: Mat = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

fn mul(a: &Mat, b: &Mat) -> Mat {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}
fn apply(m: &Mat, x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}
fn translate(tx: f64, ty: f64) -> Mat {
    [1.0, 0.0, 0.0, 1.0, tx, ty]
}
fn bbox(points: &[(f64, f64)]) -> [f64; 4] {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for (x, y) in points {
        b[0] = b[0].min(*x);
        b[1] = b[1].min(*y);
        b[2] = b[2].max(*x);
        b[3] = b[3].max(*y);
    }
    b
}
fn touches(a: &[f64; 4], b: &[f64; 4]) -> bool {
    a[0] <= b[2] && a[2] >= b[0] && a[1] <= b[3] && a[3] >= b[1]
}

#[derive(Clone)]
struct Gfx {
    ctm: Mat,
    tc: f64,
    tw: f64,
    th: f64,
    tl: f64,
    ts: f64,
    fs: f64,
    font: Vec<u8>,
    lw: f64,
}

#[derive(Clone)]
struct FontInfo {
    first: i64,
    widths: Vec<f64>,
    missing: f64,
    known: bool,
    composite: bool,
    /// `/ToUnicode` reading of the font, when it has a usable one.
    cmap: Option<Rc<Cmap>>,
}

struct Shown {
    hit: bool,
    adv: f64,
    exact: bool,
}

struct Interp<'a> {
    doc: &'a PdfDoc,
    resources: Option<&'a Dict>,
    region: [f64; 4],
    g: Gfx,
    stack: Vec<Gfx>,
    tm: Mat,
    tlm: Mat,
    path: Option<[f64; 4]>,
    fonts: BTreeMap<Vec<u8>, FontInfo>,
    unsupported: Vec<RedactionClass>,
    removed: Vec<Vec<u8>>,
    /// `removed[i]` read through its font's `/ToUnicode`.
    removed_unicode: Vec<Option<String>>,
    layout_shift: bool,
    /// Remove images whose placement touches the region (`UnsupportedPolicy::RemoveImages`).
    remove_images: bool,
    images_removed: Vec<ObjRef>,
    inline_removed: u32,
    /// Remaining decoded-byte allowance for form-XObject content.
    budget: usize,
    next_num: u32,
    /// Copy-on-write form XObjects created by this pass.
    new_objects: Vec<(u32, Entry)>,
    name_counter: u32,
}

/// Result of interpreting one content level (page or form).
struct Pass {
    out: Vec<u8>,
    changed: bool,
    /// New `/XObject` resource names for rewritten `Do` uses.
    additions: Vec<(Vec<u8>, ObjRef)>,
    /// Resource names whose every use at this level was rewritten.
    removals: Vec<Vec<u8>>,
}

const MAX_FORM_DEPTH: usize = 8;

struct XObj<'a> {
    /// `Some((stream, matrix))` for a form XObject; `None` for an image.
    form: Option<(&'a Stream, Mat)>,
    bbox: [f64; 4],
}

fn num(op: &Op, i: usize) -> Result<f64> {
    op.operands.get(i).and_then(Object::as_f64).ok_or(PressError::Malformed("content_operands"))
}
fn matrix(op: &Op) -> Result<Mat> {
    Ok([num(op, 0)?, num(op, 1)?, num(op, 2)?, num(op, 3)?, num(op, 4)?, num(op, 5)?])
}

impl<'a> Interp<'a> {
    fn new(doc: &'a PdfDoc, resources: Option<&'a Dict>, region: [f64; 4], budget: usize) -> Self {
        Self {
            budget,
            next_num: doc.objects.keys().next_back().copied().unwrap_or(0).saturating_add(1),
            new_objects: Vec::new(),
            name_counter: 0,
            doc,
            resources,
            region,
            g: Gfx { ctm: IDENT, tc: 0.0, tw: 0.0, th: 1.0, tl: 0.0, ts: 0.0, fs: 0.0, font: Vec::new(), lw: 1.0 },
            stack: Vec::new(),
            tm: IDENT,
            tlm: IDENT,
            path: None,
            fonts: BTreeMap::new(),
            unsupported: Vec::new(),
            removed: Vec::new(),
            removed_unicode: Vec::new(),
            layout_shift: false,
            remove_images: false,
            images_removed: Vec::new(),
            inline_removed: 0,
        }
    }

    fn font(&mut self, name: &[u8]) -> FontInfo {
        if let Some(f) = self.fonts.get(name) {
            return f.clone();
        }
        let none = FontInfo { first: 0, widths: Vec::new(), missing: 0.0, known: false, composite: false, cmap: None };
        let doc = self.doc;
        let fd = self
            .resources
            .and_then(|r| r.get("Font"))
            .map(|f| doc.resolve(f))
            .and_then(Object::as_dict)
            .and_then(|fonts| fonts.entries.iter().find(|(k, _)| k == name))
            .map(|(_, v)| doc.resolve(v))
            .and_then(Object::as_dict);
        let info = match fd {
            None => none,
            Some(fd) => {
                let cmap = self.load_to_unicode(fd);
                if fd.get("Subtype").is_some_and(|s| s.is_name("Type0")) {
                    FontInfo { composite: true, cmap, ..none }
                } else {
                    let first = fd.get("FirstChar").map(|x| doc.resolve(x)).and_then(Object::as_int).unwrap_or(0);
                    let widths: Vec<f64> = match fd.get("Widths").map(|w| doc.resolve(w)) {
                        Some(Object::Array(a)) => a.iter().map(|w| doc.resolve(w).as_f64().unwrap_or(0.0)).collect(),
                        _ => Vec::new(),
                    };
                    let missing = fd
                        .get("FontDescriptor")
                        .map(|d| doc.resolve(d))
                        .and_then(Object::as_dict)
                        .and_then(|d| d.get("MissingWidth"))
                        .and_then(|m| doc.resolve(m).as_f64())
                        .unwrap_or(0.0);
                    FontInfo { first, known: !widths.is_empty(), widths, missing, composite: false, cmap }
                }
            }
        };
        self.fonts.insert(name.to_vec(), info.clone());
        info
    }

    /// The font's `/ToUnicode` map, when it has a decodable one (decoded under the pass budget).
    fn load_to_unicode(&mut self, fd: &Dict) -> Option<Rc<Cmap>> {
        let Object::Stream(tu) = self.doc.resolve(fd.get("ToUnicode")?) else { return None };
        if !filters::is_decodable(&tu.dict) {
            return None;
        }
        let data = filters::decode_stream(tu, &self.doc.limits, &mut self.budget).ok()?;
        parse_to_unicode(&data).map(Rc::new)
    }

    /// `s` read through the current font's `/ToUnicode` map.
    fn unicode_of(&mut self, s: &[u8]) -> Option<String> {
        let name = self.g.font.clone();
        let font = self.font(&name);
        font.cmap.as_ref().map(|c| c.decode(s).into_iter().collect())
    }

    /// Reference of the image XObject a resource name selects.
    fn xobject_ref(&self, name: &[u8]) -> Option<ObjRef> {
        let doc = self.doc;
        let xo = self.resources?.get("XObject").map(|x| doc.resolve(x)).and_then(Object::as_dict)?;
        xo.entries.iter().find(|(k, _)| k == name).and_then(|(_, v)| v.as_ref())
    }

    /// Computes the glyph-run box for `s` at the current text position and advances Tm.
    fn show(&mut self, s: &[u8]) -> Shown {
        let name = self.g.font.clone();
        let font = self.font(&name);
        let fs = self.g.fs;
        let mut adv = 0.0f64;
        for &b in s {
            let w = if font.composite || !font.known {
                1000.0
            } else {
                let idx = i64::from(b).saturating_sub(font.first);
                if idx >= 0 && (idx as usize) < font.widths.len() { font.widths[idx as usize] } else { font.missing }
            };
            adv += w / 1000.0 * fs + self.g.tc + if b == 32 && !font.composite { self.g.tw } else { 0.0 };
        }
        let m = mul(&self.tm, &self.g.ctm);
        let (x0, x1) = (0.0, adv * self.g.th);
        let (y0, y1) = (self.g.ts - DESCENT * fs.abs(), self.g.ts + ASCENT * fs.abs());
        let pts = [apply(&m, x0, y0), apply(&m, x1, y0), apply(&m, x0, y1), apply(&m, x1, y1)];
        let hit = touches(&bbox(&pts), &self.region);
        self.tm = mul(&translate(adv * self.g.th, 0.0), &self.tm);
        Shown { hit, adv, exact: font.known && !font.composite }
    }

    /// Kerning number that reproduces the advance of a removed string, when exactly known.
    fn spacer(&mut self, shown: &Shown) -> Option<f64> {
        if shown.adv == 0.0 {
            return None;
        }
        if shown.exact && self.g.fs != 0.0 {
            Some(-shown.adv * 1000.0 / self.g.fs)
        } else {
            self.layout_shift = true;
            None
        }
    }

    fn next_line(&mut self) {
        self.tlm = mul(&translate(0.0, -self.g.tl), &self.tlm);
        self.tm = self.tlm;
    }

    fn add_point(&mut self, x: f64, y: f64) {
        let (px, py) = apply(&self.g.ctm, x, y);
        let b = self.path.get_or_insert([px, py, px, py]);
        b[0] = b[0].min(px);
        b[1] = b[1].min(py);
        b[2] = b[2].max(px);
        b[3] = b[3].max(py);
    }

    fn paint(&mut self, stroke: bool) {
        if let Some(mut b) = self.path.take() {
            if stroke {
                let det = (self.g.ctm[0] * self.g.ctm[3] - self.g.ctm[1] * self.g.ctm[2]).abs().sqrt();
                let grow = self.g.lw.abs().max(1.0) * det.max(1.0);
                b = [b[0] - grow, b[1] - grow, b[2] + grow, b[3] + grow];
            }
            if touches(&b, &self.region) {
                self.mark(RedactionClass::Vector);
            }
        }
    }

    fn mark(&mut self, class: RedactionClass) {
        if !self.unsupported.contains(&class) {
            self.unsupported.push(class);
        }
    }

    fn xobject(&self, name: &[u8]) -> Option<XObj<'a>> {
        let doc = self.doc;
        let xo = self.resources?.get("XObject").map(|x| doc.resolve(x)).and_then(Object::as_dict)?;
        let obj = xo.entries.iter().find(|(k, _)| k == name).map(|(_, v)| doc.resolve(v))?;
        let everything = [f64::MIN, f64::MIN, f64::MAX, f64::MAX];
        let d = obj.as_dict()?;
        if d.get("Subtype").is_some_and(|s| s.is_name("Form")) {
            let Object::Stream(stream) = obj else { return None };
            let m: Mat = match d.get("Matrix").map(|m| doc.resolve(m)).and_then(Object::as_array) {
                Some(a) if a.len() == 6 => {
                    let mut out = IDENT;
                    for (slot, v) in out.iter_mut().zip(a) {
                        match doc.resolve(v).as_f64() {
                            Some(x) => *slot = x,
                            // Unreadable matrix: assume the form covers everything (fail closed).
                            None => return Some(XObj { form: Some((stream, IDENT)), bbox: everything }),
                        }
                    }
                    out
                }
                _ => IDENT,
            };
            let full = mul(&m, &self.g.ctm);
            let area = match doc.rect_of(d.get("BBox")) {
                Some(b) => bbox(&[apply(&full, b[0], b[1]), apply(&full, b[2], b[1]), apply(&full, b[0], b[3]), apply(&full, b[2], b[3])]),
                None => everything,
            };
            Some(XObj { form: Some((stream, m)), bbox: area })
        } else if d.get("Subtype").is_some_and(|s| s.is_name("Image")) {
            let c = &self.g.ctm;
            Some(XObj { form: None, bbox: bbox(&[apply(c, 0.0, 0.0), apply(c, 1.0, 0.0), apply(c, 0.0, 1.0), apply(c, 1.0, 1.0)]) })
        } else {
            None
        }
    }

    fn alloc(&mut self) -> Result<u32> {
        let n = self.next_num;
        self.next_num = n.checked_add(1).ok_or(PressError::InputLimit("max_objects"))?;
        Ok(n)
    }

    fn fresh_name(&mut self, additions: &[(Vec<u8>, ObjRef)]) -> Vec<u8> {
        let doc = self.doc;
        loop {
            let candidate = format!("HskRd{}", self.name_counter).into_bytes();
            self.name_counter = self.name_counter.saturating_add(1);
            let in_resources = self
                .resources
                .and_then(|r| r.get("XObject"))
                .map(|x| doc.resolve(x))
                .and_then(Object::as_dict)
                .is_some_and(|x| x.get_key(&candidate).is_some());
            if !in_resources && !additions.iter().any(|(n, _)| *n == candidate) {
                return candidate;
            }
        }
    }

    /// Direct copy of `base` whose `/XObject` entry gains `additions` and loses `removals`.
    fn resources_with(&self, base: Option<&Dict>, additions: &[(Vec<u8>, ObjRef)], removals: &[Vec<u8>]) -> Dict {
        let mut res = base.cloned().unwrap_or_default();
        let mut xo = res.get("XObject").map(|x| self.doc.resolve(x)).and_then(Object::as_dict).cloned().unwrap_or_default();
        for name in removals {
            xo.remove_key(name);
        }
        for (name, r) in additions {
            xo.set_key(name, Object::Ref(*r));
        }
        res.set("XObject", Object::Dict(xo));
        res
    }

    /// Interprets a form XObject under the current CTM. Returns a copy-on-write replacement form
    /// when text inside it was removed; the original form is never modified.
    fn run_form(&mut self, stream: &'a Stream, matrix: Mat, cancel: &CancellationToken, depth: usize) -> Result<Option<ObjRef>> {
        let data = filters::decode_stream(stream, &self.doc.limits, &mut self.budget)?;
        let ops = parse_content(&data, &self.doc.limits)?;
        let doc = self.doc;
        let own_resources = stream.dict.get("Resources").map(|r| doc.resolve(r)).and_then(Object::as_dict);
        let parent_resources = self.resources;
        let saved = (
            self.g.clone(),
            std::mem::take(&mut self.stack),
            self.tm,
            self.tlm,
            self.path.take(),
            std::mem::take(&mut self.fonts),
        );
        self.g.ctm = mul(&matrix, &self.g.ctm);
        if own_resources.is_some() {
            self.resources = own_resources;
        }
        let pass = self.run(&ops, &data, cancel, depth);
        let effective = self.resources;
        (self.g, self.stack, self.tm, self.tlm, self.path, self.fonts) = saved;
        self.resources = parent_resources;
        let pass = pass?;
        if !pass.changed {
            return Ok(None);
        }
        let mut dict = stream.dict.clone();
        for key in ["Filter", "DecodeParms", "Length", "DL"] {
            dict.remove(key);
        }
        dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
        if !pass.additions.is_empty() || !pass.removals.is_empty() {
            let rebuilt = self.resources_with(effective, &pass.additions, &pass.removals);
            dict.set("Resources", Object::Dict(rebuilt));
        }
        let data = miniz_oxide::deflate::compress_to_vec_zlib(&pass.out, 6);
        let num = self.alloc()?;
        self.new_objects.push((num, Entry { generation: 0, obj: Object::Stream(Box::new(Stream { dict, data })), span: None }));
        Ok(Some(ObjRef { num, generation: 0 }))
    }

    fn run(&mut self, ops: &[Op], data: &[u8], cancel: &CancellationToken, depth: usize) -> Result<Pass> {
        let mut out: Vec<u8> = Vec::with_capacity(data.len());
        let mut changed = false;
        let mut additions: Vec<(Vec<u8>, ObjRef)> = Vec::new();
        let mut rewritten: BTreeSet<Vec<u8>> = BTreeSet::new();
        let mut uses: BTreeMap<Vec<u8>, u32> = BTreeMap::new();
        for op in ops {
            if op.name == b"Do"
                && let Some(n) = op.operands.first().and_then(Object::as_name)
            {
                {
                    let c = uses.entry(n.to_vec()).or_default();
                    *c = c.saturating_add(1);
                }
            }
        }
        for (i, op) in ops.iter().enumerate() {
            if i % 1024 == 0 {
                check_cancel(cancel)?;
            }
            let mut keep = true;
            match op.name.as_slice() {
                b"q" => {
                    if self.stack.len() >= 256 {
                        return Err(PressError::InputLimit("gstate_depth"));
                    }
                    self.stack.push(self.g.clone());
                }
                b"Q" => {
                    if let Some(g) = self.stack.pop() {
                        self.g = g;
                    }
                }
                b"cm" => self.g.ctm = mul(&matrix(op)?, &self.g.ctm),
                b"w" => self.g.lw = num(op, 0)?,
                b"BT" => {
                    self.tm = IDENT;
                    self.tlm = IDENT;
                }
                b"Tf" => {
                    self.g.font = op.operands.first().and_then(Object::as_name).ok_or(PressError::Malformed("content_operands"))?.to_vec();
                    self.g.fs = num(op, 1)?;
                }
                b"Tc" => self.g.tc = num(op, 0)?,
                b"Tw" => self.g.tw = num(op, 0)?,
                b"Tz" => self.g.th = num(op, 0)? / 100.0,
                b"TL" => self.g.tl = num(op, 0)?,
                b"Ts" => self.g.ts = num(op, 0)?,
                b"Td" | b"TD" => {
                    let (tx, ty) = (num(op, 0)?, num(op, 1)?);
                    if op.name == b"TD" {
                        self.g.tl = -ty;
                    }
                    self.tlm = mul(&translate(tx, ty), &self.tlm);
                    self.tm = self.tlm;
                }
                b"Tm" => {
                    self.tlm = matrix(op)?;
                    self.tm = self.tlm;
                }
                b"T*" => self.next_line(),
                b"Tj" | b"'" | b"\"" => {
                    let (si, prefix): (usize, Vec<u8>) = if op.name == b"\"" {
                        self.g.tw = num(op, 0)?;
                        self.g.tc = num(op, 1)?;
                        let mut p = Vec::new();
                        write_object(&Object::Real(self.g.tw), &mut p);
                        p.extend_from_slice(b" Tw ");
                        write_object(&Object::Real(self.g.tc), &mut p);
                        p.extend_from_slice(b" Tc\nT*\n");
                        (2, p)
                    } else if op.name == b"'" {
                        (0, b"T*\n".to_vec())
                    } else {
                        (0, Vec::new())
                    };
                    if op.name != b"Tj" {
                        self.next_line();
                    }
                    let Some(Object::Str(s)) = op.operands.get(si) else {
                        return Err(PressError::Malformed("content_operands"));
                    };
                    let shown = self.show(s);
                    if shown.hit {
                        self.removed.push(s.clone());
                        let u = self.unicode_of(s);
                        self.removed_unicode.push(u);
                        changed = true;
                        keep = false;
                        out.extend_from_slice(&prefix);
                        if let Some(n) = self.spacer(&shown) {
                            write_object(&Object::Array(vec![Object::Real(n)]), &mut out);
                            out.extend_from_slice(b" TJ\n");
                        }
                    }
                }
                b"TJ" => {
                    let Some(Object::Array(items)) = op.operands.first() else {
                        return Err(PressError::Malformed("content_operands"));
                    };
                    let mut new_items: Vec<Object> = Vec::with_capacity(items.len());
                    let mut any = false;
                    for item in items {
                        match item {
                            Object::Str(s) => {
                                let shown = self.show(s);
                                if shown.hit {
                                    any = true;
                                    self.removed.push(s.clone());
                                    let u = self.unicode_of(s);
                                    self.removed_unicode.push(u);
                                    if let Some(n) = self.spacer(&shown) {
                                        new_items.push(Object::Real(n));
                                    }
                                } else {
                                    new_items.push(item.clone());
                                }
                            }
                            Object::Int(_) | Object::Real(_) => {
                                let n = item.as_f64().unwrap_or(0.0);
                                self.tm = mul(&translate(-n / 1000.0 * self.g.fs * self.g.th, 0.0), &self.tm);
                                new_items.push(item.clone());
                            }
                            _ => return Err(PressError::Malformed("tj_element")),
                        }
                    }
                    if any {
                        changed = true;
                        keep = false;
                        write_object(&Object::Array(new_items), &mut out);
                        out.extend_from_slice(b" TJ\n");
                    }
                }
                b"m" | b"l" => {
                    let (x, y) = (num(op, 0)?, num(op, 1)?);
                    self.add_point(x, y);
                }
                b"c" => {
                    for k in 0..3usize {
                        let (x, y) = (num(op, k.saturating_mul(2))?, num(op, k.saturating_mul(2).saturating_add(1))?);
                        self.add_point(x, y);
                    }
                }
                b"v" | b"y" => {
                    for k in 0..2usize {
                        let (x, y) = (num(op, k.saturating_mul(2))?, num(op, k.saturating_mul(2).saturating_add(1))?);
                        self.add_point(x, y);
                    }
                }
                b"re" => {
                    let (x, y, w, h) = (num(op, 0)?, num(op, 1)?, num(op, 2)?, num(op, 3)?);
                    for (px, py) in [(x, y), (x + w, y), (x, y + h), (x + w, y + h)] {
                        self.add_point(px, py);
                    }
                }
                b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*" => self.paint(true),
                b"f" | b"F" | b"f*" => self.paint(false),
                b"n" => self.path = None,
                b"sh" => self.mark(RedactionClass::Shading),
                b"BI" => {
                    let c = self.g.ctm;
                    let b = bbox(&[apply(&c, 0.0, 0.0), apply(&c, 1.0, 0.0), apply(&c, 0.0, 1.0), apply(&c, 1.0, 1.0)]);
                    if touches(&b, &self.region) {
                        if self.remove_images {
                            self.inline_removed = self.inline_removed.saturating_add(1);
                            changed = true;
                            keep = false;
                        } else {
                            self.mark(RedactionClass::InlineImage);
                        }
                    }
                }
                b"Do" => {
                    let name = op.operands.first().and_then(Object::as_name).ok_or(PressError::Malformed("content_operands"))?;
                    if let Some(x) = self.xobject(name)
                        && touches(&x.bbox, &self.region)
                    {
                        match x.form {
                            None if self.remove_images => {
                                if let Some(r) = self.xobject_ref(name)
                                    && !self.images_removed.contains(&r)
                                {
                                    self.images_removed.push(r);
                                }
                                if let Some(count) = uses.get_mut(name) {
                                    *count = count.saturating_sub(1);
                                }
                                rewritten.insert(name.to_vec());
                                changed = true;
                                keep = false;
                            }
                            None => self.mark(RedactionClass::Image),
                            Some(_) if depth >= MAX_FORM_DEPTH => self.mark(RedactionClass::FormXObject),
                            Some((stream, matrix)) => {
                                if let Some(copy) = self.run_form(stream, matrix, cancel, depth.saturating_add(1))? {
                                    let fresh = self.fresh_name(&additions);
                                    out.push(b'/');
                                    write_name_bytes(&fresh, &mut out);
                                    out.extend_from_slice(b" Do
");
                                    if let Some(count) = uses.get_mut(name) {
                                        *count = count.saturating_sub(1);
                                    }
                                    rewritten.insert(name.to_vec());
                                    additions.push((fresh, copy));
                                    changed = true;
                                    keep = false;
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            if keep {
                out.extend_from_slice(&data[op.start..op.end]);
                out.push(b'\n');
            }
        }
        let removals: Vec<Vec<u8>> = rewritten.into_iter().filter(|n| uses.get(n).copied().unwrap_or(0) == 0).collect();
        Ok(Pass { out, changed, additions, removals })
    }
}

fn write_name_bytes(name: &[u8], out: &mut Vec<u8>) {
    let mut tmp = Vec::new();
    write_object(&Object::Name(name.to_vec()), &mut tmp);
    out.extend_from_slice(&tmp[1..]); // drop the leading '/', already written by the caller
}

pub(crate) fn res<'a>(m: &'a BTreeMap<u32, Entry>, mut o: &'a Object) -> &'a Object {
    static NULL: Object = Object::Null;
    for _ in 0..16 {
        match o {
            Object::Ref(r) => match m.get(&r.num) {
                Some(e) => o = &e.obj,
                None => return &NULL,
            },
            _ => return o,
        }
    }
    &NULL
}

fn purge_list(
    m: &mut BTreeMap<u32, Entry>,
    items: Vec<Object>,
    removed: &mut BTreeSet<u32>,
    depth: usize,
    max_depth: usize,
    fields_removed: &mut u32,
) -> Result<Vec<Object>> {
    if depth > max_depth {
        return Err(PressError::InputLimit("depth"));
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let Some(r) = item.as_ref() else {
            out.push(item);
            continue;
        };
        if removed.contains(&r.num) {
            *fields_removed = fields_removed.saturating_add(1);
            continue;
        }
        let kids: Option<Vec<Object>> = m
            .get(&r.num)
            .and_then(|e| e.obj.as_dict())
            .and_then(|d| d.get("Kids"))
            .map(|k| res(m, k))
            .and_then(Object::as_array)
            .map(<[Object]>::to_vec);
        if let Some(kids) = kids {
            let before = kids.len();
            let kept = purge_list(m, kids, removed, depth.saturating_add(1), max_depth, fields_removed)?;
            if kept.len() < before {
                if kept.is_empty() {
                    removed.insert(r.num);
                    *fields_removed = fields_removed.saturating_add(1);
                    continue;
                }
                if let Some(Entry { obj: Object::Dict(d), span, .. }) = m.get_mut(&r.num) {
                    d.set("Kids", Object::Array(kept));
                    *span = None;
                }
            }
        }
        out.push(item);
    }
    Ok(out)
}

impl PdfDoc {
    fn page_content_streams(&self, page: u32) -> Result<Vec<u32>> {
        let d = self.dict_of(page).ok_or(PressError::NoSuchPage)?;
        let Some(c) = d.get("Contents") else { return Ok(Vec::new()) };
        let nums = match (c, self.resolve(c)) {
            (Object::Ref(r), Object::Stream(_)) => vec![r.num],
            (_, Object::Array(a)) => a.iter().filter_map(Object::as_ref).map(|r| r.num).collect(),
            (_, Object::Null) => Vec::new(),
            _ => return Err(PressError::Malformed("page_contents")),
        };
        Ok(nums)
    }

    /// Irreversibly removes the text-showing operators and annotations that touch `region` from
    /// the in-memory document. Call `save_full` (or `redact_and_save`) afterwards; nothing is
    /// proven until the saved bytes pass `verify_payload_absent`.
    pub fn redact_region(
        &mut self,
        region: &RedactionRegion,
        policy: UnsupportedPolicy,
        cancel: &CancellationToken,
    ) -> Result<RedactionReport> {
        self.guard_mutation(cancel)?;
        let page_num = *self.pages.get(region.page_index as usize).ok_or(PressError::NoSuchPage)?;
        let r = region.rect;
        if !r.iter().all(|v| v.is_finite()) {
            return Err(PressError::Malformed("region"));
        }
        let rect = [r[0].min(r[2]), r[1].min(r[3]), r[0].max(r[2]), r[1].max(r[3])];

        // 1. Page content: decode, tokenize, interpret, rewrite.
        let stream_nums = self.page_content_streams(page_num)?;
        let mut budget = self.limits.max_decoded_total;
        let mut data: Vec<u8> = Vec::new();
        for n in &stream_nums {
            check_cancel(cancel)?;
            let Some(Entry { obj: Object::Stream(s), .. }) = self.objects.get(n) else {
                return Err(PressError::Malformed("content_stream"));
            };
            data.extend_from_slice(&filters::decode_stream(s, &self.limits, &mut budget)?);
            data.push(b'\n');
        }
        let ops = parse_content(&data, &self.limits)?;
        let resources = self.inherited(page_num, "Resources").and_then(Object::as_dict);
        let mut interp = Interp::new(self, resources, rect, budget);
        interp.remove_images = policy == UnsupportedPolicy::RemoveImages;
        let pass = interp.run(&ops, &data, cancel, 0)?;
        let page_resources = if pass.additions.is_empty() && pass.removals.is_empty() {
            None
        } else {
            Some(interp.resources_with(resources, &pass.additions, &pass.removals))
        };
        let Interp { unsupported, removed, removed_unicode, layout_shift, new_objects, next_num, images_removed, inline_removed, .. } = interp;
        let (new_content, changed) = (pass.out, pass.changed);
        if policy != UnsupportedPolicy::ReportAndContinue
            && let Some(class) = unsupported.first()
        {
            return Err(PressError::UnsupportedRedactionClass(*class));
        }

        // 2. Stage every mutation on a copy; commit only after the reachability proof.
        let mut staged = self.objects.clone();
        staged.extend(new_objects);
        if changed {
            let new_num = next_num;
            let mut dict = Dict::default();
            dict.set("Filter", Object::Name(b"FlateDecode".to_vec()));
            let data = miniz_oxide::deflate::compress_to_vec_zlib(&new_content, 6);
            staged.insert(new_num, Entry { generation: 0, obj: Object::Stream(Box::new(Stream { dict, data })), span: None });
            let page = staged.get_mut(&page_num).ok_or(PressError::NoSuchPage)?;
            if let Object::Dict(d) = &mut page.obj {
                d.set("Contents", Object::Ref(ObjRef { num: new_num, generation: 0 }));
                if let Some(rebuilt) = page_resources {
                    d.set("Resources", Object::Dict(rebuilt));
                }
            }
            page.span = None;
        }

        // 3. Annotations touching the region (plus popups / children of removed annotations).
        let items: Vec<Object> = match self.dict_of(page_num).and_then(|d| d.get("Annots")).map(|a| self.resolve(a)) {
            Some(Object::Array(a)) => a.clone(),
            _ => Vec::new(),
        };
        let mut removed_refs: BTreeSet<u32> = BTreeSet::new();
        for item in &items {
            if let Some(rf) = item.as_ref() {
                let hit = self
                    .dict_of(rf.num)
                    .and_then(|ad| self.rect_of(ad.get("Rect")))
                    .is_some_and(|ar| touches(&ar, &rect));
                if hit {
                    removed_refs.insert(rf.num);
                }
            }
        }
        loop {
            let before = removed_refs.len();
            for item in &items {
                let Some(rf) = item.as_ref() else { continue };
                let Some(ad) = self.dict_of(rf.num) else { continue };
                if removed_refs.contains(&rf.num)
                    && let Some(p) = ad.get("Popup").and_then(Object::as_ref)
                {
                    removed_refs.insert(p.num);
                }
                if ad.get("Parent").and_then(Object::as_ref).is_some_and(|p| removed_refs.contains(&p.num)) {
                    removed_refs.insert(rf.num);
                }
            }
            if removed_refs.len() == before {
                break;
            }
        }
        let kept: Vec<Object> = items
            .iter()
            .filter(|i| match i {
                Object::Ref(rf) => !removed_refs.contains(&rf.num),
                Object::Dict(d) => !self.rect_of(d.get("Rect")).is_some_and(|ar| touches(&ar, &rect)),
                _ => true,
            })
            .cloned()
            .collect();
        let mut annotations_removed: Vec<ObjRef> =
            removed_refs.iter().filter_map(|n| self.objects.get(n).map(|e| ObjRef { num: *n, generation: e.generation })).collect();
        annotations_removed.sort();
        let mut form_fields_removed = 0u32;
        if kept.len() < items.len() {
            let page = staged.get_mut(&page_num).ok_or(PressError::NoSuchPage)?;
            if let Object::Dict(d) = &mut page.obj {
                d.set("Annots", Object::Array(kept));
            }
            page.span = None;
            // 4. Drop every AcroForm field reference to the removed widgets (and emptied parents).
            let root = self.trailer.get("Root").and_then(Object::as_ref).ok_or(PressError::Malformed("no_root"))?.num;
            let form_target: Option<(u32, bool)> = match self.catalog()?.get("AcroForm") {
                Some(Object::Ref(r)) => Some((r.num, false)),
                Some(Object::Dict(_)) => Some((root, true)),
                _ => None,
            };
            if let Some((holder, nested)) = form_target {
                let fields: Option<Vec<Object>> = {
                    let form = if nested {
                        staged.get(&holder).and_then(|e| e.obj.as_dict()).and_then(|c| c.get("AcroForm")).and_then(Object::as_dict)
                    } else {
                        staged.get(&holder).and_then(|e| e.obj.as_dict())
                    };
                    form.and_then(|f| f.get("Fields")).map(|f| res(&staged, f)).and_then(Object::as_array).map(<[Object]>::to_vec)
                };
                if let Some(fields) = fields {
                    let before = fields.len();
                    let new_fields = purge_list(&mut staged, fields, &mut removed_refs, 1, self.limits.max_depth, &mut form_fields_removed)?;
                    if new_fields.len() < before
                        && let Some(e) = staged.get_mut(&holder)
                    {
                        let target = if nested {
                            match &mut e.obj {
                                Object::Dict(c) => c
                                    .entries
                                    .iter_mut()
                                    .find(|(k, _)| k == b"AcroForm")
                                    .and_then(|(_, v)| if let Object::Dict(d) = v { Some(d) } else { None }),
                                _ => None,
                            }
                        } else if let Object::Dict(d) = &mut e.obj {
                            Some(d)
                        } else {
                            None
                        };
                        if let Some(d) = target {
                            d.set("Fields", Object::Array(new_fields));
                        }
                        e.span = None;
                    }
                }
            }
        }

        // 5. Proof: no removed annotation or field object remains reachable from the trailer.
        let live = reachable(&staged, &trailer_roots(&self.trailer));
        if removed_refs.iter().any(|n| live.contains(n)) {
            return Err(PressError::RedactionIncomplete);
        }
        self.objects = staged;
        let mut unredacted = unsupported;
        unredacted.sort_by_key(|c| c.as_str());
        Ok(RedactionReport {
            page_index: region.page_index,
            text_strings_removed: u32::try_from(removed.len()).unwrap_or(u32::MAX),
            removed_text: removed,
            removed_unicode,
            annotations_removed,
            images_removed,
            inline_images_removed: inline_removed,
            form_fields_removed,
            complete: unredacted.is_empty(),
            unredacted,
            layout_shift_possible: layout_shift,
        })
    }
}
