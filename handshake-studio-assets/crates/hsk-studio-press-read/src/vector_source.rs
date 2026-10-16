//! Source colours and optional-content layers of a PDF's pages (lane SMB-ILLUSTRATOR-INTEROP-2,
//! for [`crate::read_vector_document`]). hayro-interpret hands its device colours already
//! converted to sRGB and marked content without its properties, so this module walks the same
//! content in the same order with Press's own object model (`PdfDoc`, the content tokenizer) and
//! keeps what the device does not see:
//!
//! - for every path the interpreter paints (`f F f* B B* b b* S s sh`, in content order with form
//!   XObjects followed by `Do`) the colour as written: gray, RGB, CMYK, CIE Lab (with its white
//!   point), ICC-based (the profile bytes kept once per document), a Separation (spot) ink with its
//!   tint and, for an exponential tint transform, its alternate colour at full tint, or a pattern;
//! - for every `BMC` / `BDC` its tag and, for `/OC` marked content naming an optional-content
//!   group, that group (the document's layers: `/OCProperties /OCGs` with name and default
//!   visibility from `/D`).
//!
//! The recording device consumes these events one by one and checks them against what the
//! interpreter painted (event counts, pattern against colour, the RGB of gray and RGB sources);
//! any disagreement drops the source information of the page with a typed note, never guesses.
//! Type 3 glyph procedures, annotation appearances and soft-mask groups are not walked (the
//! device does not consume events for them). A file Press cannot parse, a page whose content
//! cannot be tokenized, or a nesting past [`MAX_FORM_DEPTH`] gives no source information.
#![deny(clippy::arithmetic_side_effects)]
use crate::content;
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::pdf_edit::{PdfDoc, sha256_hex, text_string};
use crate::syntax::{Dict, Object};
use hsk_studio_accord::CancellationToken;
use std::collections::BTreeMap;

/// Deepest form XObject nesting walked.
pub const MAX_FORM_DEPTH: usize = 8;
/// Most ICC profiles kept per document.
pub const MAX_PROFILES: usize = 16;
/// Largest ICC profile kept (bytes).
pub const MAX_PROFILE_BYTES: usize = 8 << 20;
/// Most optional-content groups read.
pub const MAX_LAYERS: usize = 1024;

/// A colour space a source colour or a spot alternate is in.
#[derive(Clone, Debug, PartialEq)]
pub enum VectorSpace {
    Gray,
    Rgb,
    Cmyk,
    /// CIE Lab with its white point (XYZ).
    Lab { white: [f64; 3] },
    /// ICC-based: the index of the profile in [`crate::VectorDocument::profiles`].
    Icc(usize),
}

/// A colour as the content stream wrote it (components in the space's own ranges: 0..=1 for
/// gray, RGB, CMYK and ICC; L 0..=100, a and b as written for Lab).
#[derive(Clone, Debug, PartialEq)]
pub enum VectorColour {
    Process { space: VectorSpace, components: Vec<f64> },
    /// A Separation ink: `tint` 0..=1 and, when the tint transform is exponential (type 2), the
    /// alternate colour at full tint.
    Spot { name: String, tint: f64, alternate: Option<(VectorSpace, Vec<f64>)> },
}

/// An ICC profile a source colour names.
#[derive(Clone, Debug, PartialEq)]
pub struct VectorIcc {
    /// `/N` (1, 3 or 4).
    pub components: u8,
    pub bytes: Vec<u8>,
    pub sha256: String,
}

/// An optional-content group (an Illustrator layer).
#[derive(Clone, Debug, PartialEq)]
pub struct VectorLayer {
    pub name: String,
    /// Default visibility (`/OCProperties /D`).
    pub visible: bool,
}

/// What one painted path was painted with, as written.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SrcPaint {
    Colour(Option<VectorColour>),
    Pattern,
}

/// One `BMC` / `BDC`: its tag and the layer an `/OC` one names.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Mark {
    pub tag: Vec<u8>,
    pub layer: Option<usize>,
}

/// The source events of one page.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PageSource {
    pub paths: Vec<SrcPaint>,
    pub marks: Vec<Mark>,
    /// A Separation whose alternate could not be evaluated was seen.
    pub spot_without_alternate: bool,
    /// A colour space with no kept source form (DeviceN, Indexed, CalRGB, CalGray, ICC with an
    /// unusable profile) was painted with.
    pub converted: bool,
}

/// The document-level reading.
#[derive(Debug, Default)]
pub(crate) struct DocSource {
    pub pages: Vec<Option<PageSource>>,
    pub layers: Vec<VectorLayer>,
    pub profiles: Vec<VectorIcc>,
    /// The `/OutputIntents` destination profile (index into `profiles`).
    pub output_intent: Option<usize>,
    /// The bytes the interpreter should read: every layer switched on (hidden layers are then
    /// drawn and imported hidden); `None` when the source needed no rewrite or could not be saved.
    pub all_visible: Option<Vec<u8>>,
    /// Hidden layers exist but the all-visible copy could not be written.
    pub hidden_not_read: bool,
}

/// A colour space as the walker tracks it.
#[derive(Clone, Debug, PartialEq)]
enum Cs {
    Known(VectorSpace),
    Spot { name: String, alternate: Option<(VectorSpace, Vec<f64>)> },
    Pattern,
    /// Painted only as converted RGB (no kept source form).
    Other,
}

impl Cs {
    fn initial(&self) -> SrcPaint {
        match self {
            Self::Known(VectorSpace::Gray) => colour(VectorSpace::Gray, vec![0.0]),
            Self::Known(VectorSpace::Rgb) => colour(VectorSpace::Rgb, vec![0.0, 0.0, 0.0]),
            Self::Known(VectorSpace::Cmyk) => colour(VectorSpace::Cmyk, vec![0.0, 0.0, 0.0, 1.0]),
            Self::Known(s @ VectorSpace::Lab { .. }) => colour(s.clone(), vec![0.0, 0.0, 0.0]),
            Self::Known(s @ VectorSpace::Icc(_)) => colour(s.clone(), Vec::new()),
            Self::Spot { name, alternate } => SrcPaint::Colour(Some(VectorColour::Spot { name: name.clone(), tint: 1.0, alternate: alternate.clone() })),
            Self::Pattern => SrcPaint::Pattern,
            Self::Other => SrcPaint::Colour(None),
        }
    }
}

fn colour(space: VectorSpace, components: Vec<f64>) -> SrcPaint {
    SrcPaint::Colour(Some(VectorColour::Process { space, components }))
}

#[derive(Clone)]
struct Gs {
    fill_cs: Cs,
    stroke_cs: Cs,
    fill: SrcPaint,
    stroke: SrcPaint,
}

impl Gs {
    fn new() -> Self {
        let cs = Cs::Known(VectorSpace::Gray);
        Self { fill: cs.initial(), stroke: cs.initial(), fill_cs: cs.clone(), stroke_cs: cs }
    }
}

struct Walker<'d> {
    doc: &'d PdfDoc,
    limits: Limits,
    budget: usize,
    ops_left: usize,
    profiles: Vec<VectorIcc>,
    by_sha: BTreeMap<String, usize>,
    ocgs: BTreeMap<u32, usize>,
    out: PageSource,
    cancel: &'d CancellationToken,
}

/// Number of an indirect object reached through `o` (the last reference followed).
fn ref_num(o: &Object) -> Option<u32> {
    o.as_ref().map(|r| r.num)
}

impl<'d> Walker<'d> {
    fn get<'a>(&'a self, d: &'a Dict, key: &str) -> Option<&'a Object> {
        d.get(key).map(|o| self.doc.resolve(o))
    }

    fn profile(&mut self, stream: &Object) -> Option<(usize, u8)> {
        let Object::Stream(s) = self.doc.resolve(stream) else { return None };
        let n = s.dict.get("N").map(|o| self.doc.resolve(o)).and_then(Object::as_int).and_then(|n| u8::try_from(n).ok()).filter(|n| matches!(n, 1 | 3 | 4))?;
        let mut budget = self.budget.min(MAX_PROFILE_BYTES);
        let bytes = filters::decode_stream(s, &self.limits, &mut budget).ok()?;
        self.budget = self.budget.saturating_sub(bytes.len());
        if bytes.len() < 128 || bytes.len() > MAX_PROFILE_BYTES || bytes.get(36..40) != Some(b"acsp") {
            return None;
        }
        let sha = sha256_hex(&bytes);
        if let Some(i) = self.by_sha.get(&sha) {
            return Some((*i, n));
        }
        if self.profiles.len() >= MAX_PROFILES {
            return None;
        }
        let i = self.profiles.len();
        self.by_sha.insert(sha.clone(), i);
        self.profiles.push(VectorIcc { components: n, bytes, sha256: sha });
        Some((i, n))
    }

    fn space(&mut self, o: &Object, resources: Option<&Dict>, depth: usize) -> Cs {
        if depth > 4 {
            return Cs::Other;
        }
        let o = self.doc.resolve(o).clone();
        match &o {
            Object::Name(n) => match n.as_slice() {
                b"DeviceGray" | b"G" => Cs::Known(VectorSpace::Gray),
                b"DeviceRGB" | b"RGB" => Cs::Known(VectorSpace::Rgb),
                b"DeviceCMYK" | b"CMYK" => Cs::Known(VectorSpace::Cmyk),
                b"Pattern" => Cs::Pattern,
                other => {
                    let named = resources.and_then(|r| self.get(r, "ColorSpace")).and_then(Object::as_dict).and_then(|d| d.get_key(other)).cloned();
                    match named {
                        Some(v) => self.space(&v, resources, depth.saturating_add(1)),
                        None => Cs::Other,
                    }
                }
            },
            Object::Array(a) => {
                let family = a.first().and_then(Object::as_name).unwrap_or_default();
                match family {
                    b"ICCBased" => match a.get(1).and_then(|s| self.profile(s)) {
                        Some((i, _)) => Cs::Known(VectorSpace::Icc(i)),
                        None => Cs::Other,
                    },
                    b"Lab" => {
                        let white = a
                            .get(1)
                            .map(|d| self.doc.resolve(d))
                            .and_then(Object::as_dict)
                            .and_then(|d| d.get("WhitePoint"))
                            .map(|w| self.doc.resolve(w))
                            .and_then(Object::as_array)
                            .and_then(|w| {
                                let v: Vec<f64> = w.iter().filter_map(|x| self.doc.resolve(x).as_f64()).collect();
                                <[f64; 3]>::try_from(v).ok()
                            })
                            .unwrap_or([0.9642, 1.0, 0.8249]);
                        Cs::Known(VectorSpace::Lab { white })
                    }
                    b"Separation" => {
                        let name = a.get(1).and_then(Object::as_name).map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default();
                        if name == "All" || name == "None" {
                            return Cs::Other;
                        }
                        let alt = a.get(2).cloned().map(|x| self.space(&x, resources, depth.saturating_add(1)));
                        let alternate = match (alt, a.get(3).map(|f| self.doc.resolve(f)).and_then(Object::as_dict)) {
                            (Some(Cs::Known(space)), Some(f)) if f.get("FunctionType").map(|t| self.doc.resolve(t)).and_then(Object::as_int) == Some(2) => {
                                // f(1) of an exponential function is C1 (default [1.0]).
                                let c1: Vec<f64> = match f.get("C1").map(|c| self.doc.resolve(c)) {
                                    Some(Object::Array(c)) => c.iter().filter_map(|x| self.doc.resolve(x).as_f64()).collect(),
                                    _ => vec![1.0],
                                };
                                let n = match &space {
                                    VectorSpace::Gray => 1,
                                    VectorSpace::Rgb | VectorSpace::Lab { .. } => 3,
                                    VectorSpace::Cmyk => 4,
                                    VectorSpace::Icc(i) => self.profiles.get(*i).map_or(0, |p| usize::from(p.components)),
                                };
                                (c1.len() == n && c1.iter().all(|v| v.is_finite())).then_some((space, c1))
                            }
                            _ => None,
                        };
                        if alternate.is_none() {
                            self.out.spot_without_alternate = true;
                        }
                        Cs::Spot { name, alternate }
                    }
                    b"Pattern" => Cs::Pattern,
                    _ => Cs::Other,
                }
            }
            _ => Cs::Other,
        }
    }

    fn set_colour(&mut self, gs: &mut Gs, stroke: bool, operands: &[Object]) {
        let cs = if stroke { gs.stroke_cs.clone() } else { gs.fill_cs.clone() };
        let nums: Vec<f64> = operands.iter().filter_map(Object::as_f64).collect();
        let paint = if operands.last().is_some_and(|o| o.as_name().is_some()) {
            SrcPaint::Pattern
        } else {
            match cs {
                Cs::Known(space) => colour(space, nums),
                Cs::Spot { name, alternate } => SrcPaint::Colour(Some(VectorColour::Spot { name, tint: nums.first().copied().unwrap_or(1.0), alternate })),
                Cs::Pattern => SrcPaint::Pattern,
                Cs::Other => {
                    self.out.converted = true;
                    SrcPaint::Colour(None)
                }
            }
        };
        if stroke {
            gs.stroke = paint;
        } else {
            gs.fill = paint;
        }
    }

    fn stream_ops(&mut self, data: &[u8], resources: Option<&Dict>) -> Result<Vec<content::Op>> {
        let doc = self.doc;
        let res = resources.cloned();
        let lookup = move |name: &[u8]| -> Option<usize> {
            match name {
                b"DeviceGray" | b"G" => Some(1),
                b"DeviceRGB" | b"RGB" => Some(3),
                b"DeviceCMYK" | b"CMYK" => Some(4),
                _ => {
                    let d = res.as_ref()?.get("ColorSpace").map(|o| doc.resolve(o)).and_then(Object::as_dict)?;
                    let v = doc.resolve(d.get_key(name)?);
                    let a = v.as_array()?;
                    match a.first().and_then(Object::as_name)? {
                        b"ICCBased" => a.get(1).map(|s| doc.resolve(s)).and_then(Object::as_dict).and_then(|s| s.get("N")).and_then(Object::as_int).and_then(|n| usize::try_from(n).ok()),
                        b"Lab" | b"CalRGB" => Some(3),
                        b"CalGray" | b"Separation" | b"Indexed" => Some(1),
                        _ => None,
                    }
                }
            }
        };
        let parsed = content::tokenize(data, &self.limits, &lookup, &mut self.budget)?;
        self.ops_left = self.ops_left.checked_sub(parsed.ops.len()).ok_or(PressError::InputLimit("vector_source_ops"))?;
        Ok(parsed.ops)
    }

    fn walk(&mut self, data: &[u8], resources: Option<&Dict>, depth: usize, forms: &mut Vec<u32>) -> Result<()> {
        let ops = self.stream_ops(data, resources)?;
        self.walk_ops(ops, resources, depth, forms, Gs::new())
    }

    fn walk_ops(&mut self, ops: Vec<content::Op>, resources: Option<&Dict>, depth: usize, forms: &mut Vec<u32>, gs: Gs) -> Result<()> {
        let mut stack = vec![gs];
        for (n, op) in ops.iter().enumerate() {
            if n.is_multiple_of(256) {
                self.cancel.check().map_err(|_| PressError::Canceled)?;
            }
            self.step(op, resources, depth, forms, &mut stack)?;
        }
        Ok(())
    }

    fn step(&mut self, op: &content::Op, resources: Option<&Dict>, depth: usize, forms: &mut Vec<u32>, stack: &mut Vec<Gs>) -> Result<()> {
        let Some(gs) = stack.last().cloned() else { return Err(PressError::Malformed("vector_source_state")) };
        match op.name.as_slice() {
            b"q" => stack.push(gs),
            b"Q" => {
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            b"g" | b"G" | b"rg" | b"RG" | b"k" | b"K" => {
                let stroke = op.name.first().is_some_and(u8::is_ascii_uppercase);
                let space = match op.name.as_slice() {
                    b"g" | b"G" => VectorSpace::Gray,
                    b"rg" | b"RG" => VectorSpace::Rgb,
                    _ => VectorSpace::Cmyk,
                };
                let mut g = gs;
                if stroke {
                    g.stroke_cs = Cs::Known(space);
                } else {
                    g.fill_cs = Cs::Known(space);
                }
                self.set_colour(&mut g, stroke, &op.operands);
                set_top(stack, g);
            }
            b"cs" | b"CS" => {
                let stroke = op.name == b"CS";
                let cs = match op.operands.first() {
                    Some(o) => self.space(o, resources, 0),
                    None => Cs::Other,
                };
                let mut g = gs;
                if stroke {
                    g.stroke = cs.initial();
                    g.stroke_cs = cs;
                } else {
                    g.fill = cs.initial();
                    g.fill_cs = cs;
                }
                set_top(stack, g);
            }
            b"sc" | b"scn" | b"SC" | b"SCN" => {
                let stroke = op.name.first() == Some(&b'S');
                let mut g = gs;
                self.set_colour(&mut g, stroke, &op.operands);
                set_top(stack, g);
            }
            b"f" | b"F" | b"f*" => self.out.paths.push(gs.fill),
            b"S" | b"s" => self.out.paths.push(gs.stroke),
            b"B" | b"B*" | b"b" | b"b*" => {
                self.out.paths.push(gs.fill);
                self.out.paths.push(gs.stroke);
            }
            b"sh" => self.out.paths.push(SrcPaint::Pattern),
            b"BMC" => self.out.marks.push(Mark { tag: op.operands.first().and_then(Object::as_name).unwrap_or_default().to_vec(), layer: None }),
            b"BDC" => {
                let tag = op.operands.first().and_then(Object::as_name).unwrap_or_default().to_vec();
                let layer = if tag == b"OC" {
                    let target = match op.operands.get(1) {
                        Some(Object::Name(p)) => resources.and_then(|r| self.get(r, "Properties")).and_then(Object::as_dict).and_then(|d| d.get_key(p)).and_then(ref_num),
                        _ => None,
                    };
                    target.and_then(|t| self.ocgs.get(&t).copied())
                } else {
                    None
                };
                self.out.marks.push(Mark { tag, layer });
            }
            b"Do" => {
                let Some(name) = op.operands.first().and_then(Object::as_name) else { return Ok(()) };
                let Some(xref) = resources.and_then(|r| self.get(r, "XObject")).and_then(Object::as_dict).and_then(|d| d.get_key(name)).cloned() else { return Ok(()) };
                let num = ref_num(&xref);
                let Object::Stream(form) = self.doc.resolve(&xref) else { return Ok(()) };
                if !form.dict.get("Subtype").map(|s| self.doc.resolve(s)).is_some_and(|s| s.is_name("Form")) {
                    return Ok(());
                }
                if depth >= MAX_FORM_DEPTH || num.is_some_and(|n| forms.contains(&n)) {
                    return Err(PressError::InputLimit("vector_source_forms"));
                }
                let data = filters::decode_stream(form, &self.limits, &mut self.budget)?;
                let own = form.dict.get("Resources").map(|r| self.doc.resolve(r)).and_then(Object::as_dict).cloned();
                if let Some(n) = num {
                    forms.push(n);
                }
                // The form starts from the caller's colour state and leaves it unchanged
                // (ISO 32000-2 8.10.1: Do brackets the form in q/Q).
                let ops = self.stream_ops(&data, own.as_ref().or(resources));
                let r = ops.and_then(|ops| self.walk_ops(ops, own.as_ref().or(resources), depth.saturating_add(1), forms, gs));
                forms.pop();
                r?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn set_top(stack: &mut [Gs], g: Gs) {
    if let Some(top) = stack.last_mut() {
        *top = g;
    }
}

/// The optional-content groups of the catalog: (object number, layer) and whether any is hidden.
fn layers_of(doc: &PdfDoc) -> (BTreeMap<u32, usize>, Vec<VectorLayer>, bool) {
    let mut map = BTreeMap::new();
    let mut layers = Vec::new();
    let Ok(catalog) = doc.catalog() else { return (map, layers, false) };
    let Some(props) = catalog.get("OCProperties").map(|o| doc.resolve(o)).and_then(Object::as_dict) else { return (map, layers, false) };
    let Some(ocgs) = props.get("OCGs").map(|o| doc.resolve(o)).and_then(Object::as_array) else { return (map, layers, false) };
    let config = props.get("D").map(|o| doc.resolve(o)).and_then(Object::as_dict);
    let base_off = config.and_then(|d| d.get("BaseState")).map(|o| doc.resolve(o)).is_some_and(|b| b.is_name("OFF"));
    let listed = |key: &str| -> Vec<u32> {
        config.and_then(|d| d.get(key)).map(|o| doc.resolve(o)).and_then(Object::as_array).map(|a| a.iter().filter_map(ref_num).collect()).unwrap_or_default()
    };
    let (on, off) = (listed("ON"), listed("OFF"));
    let mut any_hidden = false;
    for o in ocgs.iter().take(MAX_LAYERS) {
        let Some(num) = ref_num(o) else { continue };
        if map.contains_key(&num) {
            continue;
        }
        let name = doc.dict_of(num).and_then(|d| d.get("Name")).map(|n| doc.resolve(n)).and_then(|n| match n {
            Object::Str(s) => Some(text_string(s)),
            _ => None,
        });
        let visible = if off.contains(&num) {
            false
        } else if on.contains(&num) {
            true
        } else {
            !base_off
        };
        any_hidden |= !visible;
        map.insert(num, layers.len());
        layers.push(VectorLayer { name: name.unwrap_or_else(|| format!("Layer {}", layers.len().saturating_add(1))), visible });
    }
    (map, layers, any_hidden)
}

/// The document's bytes with every optional-content group on (`/D /BaseState /ON`, no `/OFF`).
fn all_visible(mut doc: PdfDoc, cancel: &CancellationToken) -> Result<Vec<u8>> {
    let root = doc.trailer.get("Root").and_then(Object::as_ref).ok_or(PressError::Malformed("no_root"))?;
    let (props_num, d_num) = {
        let catalog = doc.dict_of(root.num).ok_or(PressError::Malformed("no_root"))?;
        let props_ref = catalog.get("OCProperties").and_then(ref_num);
        let props = catalog.get("OCProperties").map(|o| doc.resolve(o)).and_then(Object::as_dict).ok_or(PressError::Malformed("oc_properties"))?;
        (props_ref, props.get("D").and_then(ref_num))
    };
    let edit = |d: &mut Dict| {
        d.set("BaseState", Object::Name(b"ON".to_vec()));
        d.remove("OFF");
    };
    if let Some(n) = d_num {
        let e = doc.objects.get_mut(&n).ok_or(PressError::Malformed("oc_config"))?;
        if let Object::Dict(d) = &mut e.obj {
            edit(d);
        }
        e.span = None;
    } else {
        let holder = props_num.unwrap_or(root.num);
        let e = doc.objects.get_mut(&holder).ok_or(PressError::Malformed("oc_properties"))?;
        let props = match &mut e.obj {
            Object::Dict(d) if props_num.is_some() => Some(d),
            Object::Dict(d) => match d.get_mut("OCProperties") {
                Some(Object::Dict(p)) => Some(p),
                _ => None,
            },
            _ => None,
        };
        if let Some(Object::Dict(config)) = props.and_then(|p| p.get_mut("D")) {
            edit(config);
        }
        e.span = None;
    }
    doc.save_full(cancel)
}

/// Content streams of a page, decoded and joined.
fn page_content(doc: &PdfDoc, page: u32, limits: &Limits, budget: &mut usize) -> Result<Vec<u8>> {
    let d = doc.dict_of(page).ok_or(PressError::NoSuchPage)?;
    let streams: Vec<&Object> = match d.get("Contents").map(|c| doc.resolve(c)) {
        Some(Object::Array(a)) => a.iter().map(|o| doc.resolve(o)).collect(),
        Some(o @ Object::Stream(_)) => vec![o],
        _ => Vec::new(),
    };
    let mut out = Vec::new();
    for s in streams {
        if let Object::Stream(s) = s {
            out.extend(filters::decode_stream(s, limits, budget)?);
            out.push(b'\n');
        }
    }
    Ok(out)
}

/// Reads the source events of every page of `pdf` (module docs); `None` when Press cannot parse it.
pub(crate) fn read_sources(pdf: &[u8], cancel: &CancellationToken) -> Option<DocSource> {
    let limits = Limits::default();
    let doc = PdfDoc::open(pdf, &limits, cancel).ok()?;
    let (ocgs, layers, any_hidden) = layers_of(&doc);
    let mut out = DocSource { layers, ..DocSource::default() };
    let mut w = Walker { doc: &doc, limits, budget: limits.max_decoded_total, ops_left: limits.max_content_ops, profiles: Vec::new(), by_sha: BTreeMap::new(), ocgs, out: PageSource::default(), cancel };
    // The document CMYK (or other) intent: /OutputIntents [<< /DestOutputProfile stream >>].
    let intent = doc
        .catalog()
        .ok()
        .and_then(|c| c.get("OutputIntents"))
        .map(|o| doc.resolve(o))
        .and_then(Object::as_array)
        .and_then(|a| a.first())
        .map(|o| doc.resolve(o))
        .and_then(Object::as_dict)
        .and_then(|d| d.get("DestOutputProfile"))
        .cloned();
    if let Some(p) = intent {
        out.output_intent = w.profile(&p).map(|(i, _)| i);
    }
    for page in doc.pages.clone() {
        if cancel.check().is_err() {
            return None;
        }
        w.out = PageSource::default();
        let resources = doc.inherited(page, "Resources").and_then(Object::as_dict).cloned();
        let read = page_content(&doc, page, &limits, &mut w.budget).and_then(|data| w.walk(&data, resources.as_ref(), 0, &mut Vec::new()));
        let page_source = std::mem::take(&mut w.out);
        out.pages.push(read.ok().map(|()| page_source));
    }
    out.profiles = std::mem::take(&mut w.profiles);
    drop(w);
    if any_hidden {
        match all_visible(doc, cancel) {
            Ok(bytes) => out.all_visible = Some(bytes),
            Err(_) => out.hidden_not_read = true,
        }
    }
    Some(out)
}
