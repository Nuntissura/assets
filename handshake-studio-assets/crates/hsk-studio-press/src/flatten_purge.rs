//! What a flattened page leaves behind in the document (R13-F1, R13-F2, R13-F6, R13-P1; lead decision
//! 2026-10-11T20:26:55Z): after `flatten.rs` replaced the page and detached it from inheritance, the
//! redaction save drops everything that described or drew the OLD page and that nothing kept still
//! draws, so an image, form, annotation, widget value or structure text of a flattened page does not stay
//! in the file by being merely listed (tests `painted_sources_under_a_region_do_not_survive`,
//! `structure_and_widgets_of_flattened_pages_are_removed`).
//!
//! - XFA ([`strip_xfa`], R13-F2): `/AcroForm /XFA` and the catalog's `/NeedsRendering` are removed from
//!   every redaction output. Choice (strip, not refuse): XFA is deprecated in ISO 32000-2 and is a second
//!   copy of the form (template and datasets) that viewers such as pdf.js render INSTEAD of the PDF
//!   pages when `/NeedsRendering` is true and `/Fields` is empty (pdf.js `PDFDocument.xfaFactory`);
//!   stripping makes every viewer draw the PDF pages, the flattened ones included, and removes the
//!   datasets copy of field values that page redaction does not edit. A dynamic (pure) XFA form keeps only
//!   its PDF pages (often a placeholder page). Refusing would have blocked every XFA form, static ones
//!   included. `RedactedOutput::xfa_removed` reports it (test `xfa_is_stripped_from_redaction_outputs`).
//! - Annotations and widgets ([`remove_annotations`], R13-P1): every annotation a viewer drew on a
//!   flattened page (its own `/Annots`, else the nearest ancestor's, as pdf.js reads them), every
//!   annotation whose `/P` is a flattened page, and their popups, unless a kept page lists the annotation
//!   in its own `/Annots`; their AcroForm field entries are purged (emptied parents too) and every other
//!   direct reference to them is dropped (dictionary key removed, array element nulled; `/CO` entries
//!   removed); none may stay reachable (`RedactionIncomplete` otherwise).
//! - Structure ([`neutralize_structure`], R13-P1): every structure element that points at a flattened
//!   page (its `/Pg` is one, the page's `/StructParents` entry of the parent tree names it, or it holds a
//!   marked-content or object reference on such a page or to a removed annotation) loses its text and
//!   attributes (`/Alt /ActualText /E /T /Lang /A /C /ID /Ref /AF /Phoneme /PhoneticAlphabet`, and `/Pg`
//!   when it is a flattened page) and the content items on flattened pages; its child elements on other
//!   pages stay. The parent-tree entries of the flattened pages and removed annotations are nulled.
//! - Painted sources ([`drop_painted_sources`], R13-F6): the images, forms, patterns, shadings, graphics
//!   states and fonts the old page reached are dropped from every resource dictionary that lists them
//!   when no kept content DRAWS them (usage analysis below); a source that stays only because some
//!   content's usage cannot be determined is reported (`RedactedOutput::painted_sources_kept`,
//!   `NotProven::PaintedSourceKept`).
//!
//! Usage analysis: every page of the tree is a scope whose names resolve in its own `/Resources` and in
//! every ancestor's (pdf.js merges them); a scope's content is decoded and strictly parsed and the names
//! of `Do`, `sh`, `scn`/`SCN`, `gs` and `Tf` mark the entries of `/XObject`, `/Shading`, `/Pattern`,
//! `/ExtGState` and `/Font` it uses. A used form, tiling pattern, soft-mask group or Type 3 font WITHOUT
//! its own `/Resources` is read in the same scope; every stream (and Type 3 font) WITH its own
//! `/Resources`, however it is reached, is a scope of its own. Everything else is followed as a plain
//! reference. Content that cannot be decoded or parsed makes every entry of its scope used (kept,
//! uncertain). Test `painted_sources_under_a_region_do_not_survive`.
#![deny(clippy::arithmetic_side_effects)]
use crate::content::parse_strict;
use crate::error::{Limits, PressError, Result};
use crate::filters;
use crate::flatten::Flat;
use crate::pdf_edit::PdfDoc;
use crate::reader::{Entry, check_cancel};
use crate::redaction::{purge_list, res};
use crate::show_text::resource_components;
use crate::syntax::{Dict, Object};
use crate::writer::{reachable, trailer_roots};
use hsk_studio_accord::CancellationToken;
use std::collections::{BTreeMap, BTreeSet};

type Objs = BTreeMap<u32, Entry>;

fn catalog_num(doc: &PdfDoc) -> Result<u32> {
    doc.trailer.get("Root").and_then(Object::as_ref).map(|r| r.num).ok_or(PressError::Malformed("no_root"))
}

fn touch(e: &mut Entry) {
    e.span = None;
}

/// Removes `/AcroForm /XFA` and `/NeedsRendering` (module docs). `true` when something was removed.
pub(crate) fn strip_xfa(doc: &mut PdfDoc) -> Result<bool> {
    let root = catalog_num(doc)?;
    let form_ref = doc.dict_of(root).and_then(|c| c.get("AcroForm")).and_then(Object::as_ref).map(|r| r.num);
    let mut removed = false;
    if let Some(e) = doc.objects.get_mut(&root)
        && let Object::Dict(c) = &mut e.obj
    {
        let mut hit = c.remove("NeedsRendering").is_some();
        if let Some(Object::Dict(f)) = c.get_mut("AcroForm") {
            hit |= f.remove("XFA").is_some();
        }
        if hit {
            touch(e);
            removed = true;
        }
    }
    if let Some(n) = form_ref
        && let Some(e) = doc.objects.get_mut(&n)
        && let Object::Dict(f) = &mut e.obj
        && f.remove("XFA").is_some()
    {
        touch(e);
        removed = true;
    }
    Ok(removed)
}

/// Annotations of the flattened pages (module docs), before any removal.
pub(crate) fn page_annotations(doc: &PdfDoc, flats: &[Flat]) -> BTreeSet<u32> {
    let pages: BTreeSet<u32> = flats.iter().map(|f| f.page.object).collect();
    let mut set: BTreeSet<u32> = flats.iter().flat_map(|f| f.annots.iter().copied()).collect();
    for (num, e) in &doc.objects {
        let Some(d) = e.obj.as_dict() else { continue };
        let annotation = d.get("Subtype").is_some() || d.get("Type").map(|t| doc.resolve(t)).is_some_and(|t| t.is_name("Annot"));
        if annotation && d.get("P").and_then(Object::as_ref).is_some_and(|p| pages.contains(&p.num)) && !pages.contains(num) {
            set.insert(*num);
        }
    }
    // Popups of those (a popup of a popup is not a thing, but the chain is followed to a fixpoint).
    for _ in 0..8 {
        let popups: Vec<u32> = set.iter().filter_map(|n| doc.dict_of(*n)).filter_map(|d| d.get("Popup")).filter_map(Object::as_ref).map(|r| r.num).collect();
        let before = set.len();
        set.extend(popups);
        if set.len() == before {
            break;
        }
    }
    // A kept page that lists an annotation in its own /Annots keeps it.
    for p in doc.pages.iter().filter(|p| !pages.contains(p)) {
        if let Some(Object::Array(a)) = doc.dict_of(*p).and_then(|d| d.get("Annots")).map(|a| doc.resolve(a)) {
            for r in a.iter().filter_map(Object::as_ref) {
                set.remove(&r.num);
            }
        }
    }
    set
}

/// Drops every direct reference to `gone` from the other objects (module docs).
fn scrub_references(objects: &mut Objs, gone: &BTreeSet<u32>) {
    fn scrub(o: &mut Object, gone: &BTreeSet<u32>, depth: usize) -> bool {
        if depth > 64 {
            return false;
        }
        let next = depth.saturating_add(1);
        let mut changed = false;
        match o {
            Object::Array(a) => {
                for x in a.iter_mut() {
                    if matches!(x, Object::Ref(r) if gone.contains(&r.num)) {
                        *x = Object::Null;
                        changed = true;
                    } else {
                        changed |= scrub(x, gone, next);
                    }
                }
            }
            Object::Dict(_) | Object::Stream(_) => {
                let d = match o {
                    Object::Dict(d) => d,
                    Object::Stream(s) => &mut s.dict,
                    _ => return false,
                };
                let before = d.entries.len();
                d.entries.retain(|(_, v)| !matches!(v, Object::Ref(r) if gone.contains(&r.num)));
                changed |= d.entries.len() != before;
                for (_, v) in d.entries.iter_mut() {
                    changed |= scrub(v, gone, next);
                }
            }
            _ => {}
        }
        changed
    }
    for (num, e) in objects.iter_mut() {
        if gone.contains(num) {
            continue;
        }
        if scrub(&mut e.obj, gone, 0) {
            touch(e);
        }
    }
}

/// Removes the annotations `set` (module docs): AcroForm fields, `/CO`, every other reference.
pub(crate) fn remove_annotations(doc: &mut PdfDoc, set: &BTreeSet<u32>, cancel: &CancellationToken) -> Result<u32> {
    if set.is_empty() {
        return Ok(0);
    }
    check_cancel(cancel)?;
    let mut removed = set.clone();
    let root = catalog_num(doc)?;
    let form_target: Option<(u32, bool)> = match doc.dict_of(root).and_then(|c| c.get("AcroForm")) {
        Some(Object::Ref(r)) => Some((r.num, false)),
        Some(Object::Dict(_)) => Some((root, true)),
        _ => None,
    };
    let mut fields_removed = 0u32;
    if let Some((holder, nested)) = form_target {
        let form_of = |objs: &Objs| -> Option<Dict> {
            let d = objs.get(&holder)?.obj.as_dict()?;
            if nested { d.get("AcroForm").and_then(Object::as_dict).cloned() } else { Some(d.clone()) }
        };
        if let Some(mut form) = form_of(&doc.objects) {
            let mut changed = false;
            if let Some(fields) = form.get("Fields").map(|f| res(&doc.objects, f)).and_then(Object::as_array).map(<[Object]>::to_vec) {
                let before = fields.len();
                let kept = purge_list(&mut doc.objects, fields, &mut removed, 1, doc.limits.depth_cap(), &mut fields_removed)?;
                if kept.len() != before {
                    form.set("Fields", Object::Array(kept));
                    changed = true;
                }
            }
            if let Some(co) = form.get("CO").map(|c| res(&doc.objects, c)).and_then(Object::as_array).map(<[Object]>::to_vec) {
                let kept: Vec<Object> = co.iter().filter(|o| !matches!(o, Object::Ref(r) if removed.contains(&r.num))).cloned().collect();
                if kept.len() != co.len() {
                    form.set("CO", Object::Array(kept));
                    changed = true;
                }
            }
            if changed && let Some(e) = doc.objects.get_mut(&holder) {
                match &mut e.obj {
                    Object::Dict(c) if nested => c.set("AcroForm", Object::Dict(form)),
                    Object::Dict(d) => *d = form,
                    _ => {}
                }
                touch(e);
            }
        }
    }
    scrub_references(&mut doc.objects, &removed);
    let live = reachable(&doc.objects, &trailer_roots(&doc.trailer));
    if removed.iter().any(|n| live.contains(n)) {
        return Err(PressError::RedactionIncomplete);
    }
    Ok(u32::try_from(set.len()).unwrap_or(u32::MAX))
}

/// Keys a structure element that points at a flattened page loses (module docs).
const STRUCT_TEXT_KEYS: [&str; 12] = ["Alt", "ActualText", "E", "T", "Lang", "A", "C", "ID", "Ref", "AF", "Phoneme", "PhoneticAlphabet"];

struct StructPass<'a> {
    flat: &'a BTreeSet<u32>,
    annots: &'a BTreeSet<u32>,
    mapped: BTreeSet<u32>,
    neutralized: u32,
}

impl StructPass<'_> {
    /// A content item (marked-content or object reference dictionary) is on a flattened page (its own
    /// `/Pg`, else the element's page, else the element is named by a flattened page's parent-tree entry)
    /// or references a removed annotation.
    fn item_on_flat(&self, d: &Dict, page: Option<u32>, mapped: bool) -> bool {
        if d.get("Obj").and_then(Object::as_ref).is_some_and(|r| self.annots.contains(&r.num)) {
            return true;
        }
        match d.get("Pg").and_then(Object::as_ref).map(|r| r.num) {
            Some(p) => self.flat.contains(&p),
            None => page.is_some_and(|p| self.flat.contains(&p)) || mapped,
        }
    }

    /// Processes one element dictionary in place: returns its child element references (with the page
    /// they inherit) and whether it was neutralized.
    fn element(&mut self, objs: &Objs, d: &mut Dict, inherited: Option<u32>, mapped: bool, depth: usize, out: &mut Vec<(u32, Option<u32>)>) {
        let own = d.get("Pg").and_then(Object::as_ref).map(|r| r.num);
        let page = own.or(inherited);
        let mut pointing = own.is_some_and(|p| self.flat.contains(&p)) || mapped;
        let items: Vec<Object> = match d.get("K") {
            Some(Object::Array(a)) => a.clone(),
            Some(other) => vec![other.clone()],
            None => Vec::new(),
        };
        let mut kept: Vec<Object> = Vec::with_capacity(items.len());
        for item in items {
            let on_flat = match &item {
                Object::Int(_) => page.is_some_and(|p| self.flat.contains(&p)) || mapped,
                Object::Dict(k) if k.get("S").is_none() => self.item_on_flat(k, page, mapped),
                Object::Dict(_) => false,
                Object::Ref(r) => match objs.get(&r.num).and_then(|e| e.obj.as_dict()) {
                    Some(k) if k.get("S").is_some() => {
                        out.push((r.num, page));
                        false
                    }
                    Some(k) => self.item_on_flat(k, page, mapped),
                    None => false,
                },
                _ => false,
            };
            if on_flat {
                pointing = true;
                continue;
            }
            let mut item = item;
            if let Object::Dict(k) = &mut item
                && k.get("S").is_some()
                && depth < 64
            {
                self.element(objs, k, page, false, depth.saturating_add(1), out);
            }
            kept.push(item);
        }
        if pointing {
            for key in STRUCT_TEXT_KEYS {
                d.remove(key);
            }
            if own.is_some_and(|p| self.flat.contains(&p)) {
                d.remove("Pg");
            }
            self.neutralized = self.neutralized.saturating_add(1);
        }
        match kept.len() {
            0 => {
                d.remove("K");
            }
            1 if !matches!(d.get("K"), Some(Object::Array(_))) => d.set("K", kept.remove(0)),
            _ => d.set("K", Object::Array(kept)),
        }
    }
}

/// Nulls the `/Nums` entries of one parent-tree node whose keys are in `keys` (an indirect `/Nums` array
/// is edited in `objects`); collects the elements they named. Returns whether `node` itself changed and
/// its `/Kids`.
fn scrub_nums(objects: &mut Objs, node: &mut Dict, keys: &BTreeSet<i64>, named: &mut BTreeSet<u32>) -> (bool, Vec<u32>) {
    let kids: Vec<u32> = match node.get("Kids").map(|k| res(objects, k)) {
        Some(Object::Array(a)) => a.iter().filter_map(Object::as_ref).map(|r| r.num).collect(),
        _ => Vec::new(),
    };
    let (indirect, mut nums) = match node.get("Nums") {
        Some(Object::Array(a)) => (None, a.clone()),
        Some(Object::Ref(r)) => match objects.get(&r.num).map(|e| &e.obj) {
            Some(Object::Array(a)) => (Some(r.num), a.clone()),
            _ => return (false, kids),
        },
        _ => return (false, kids),
    };
    let mut changed = false;
    for pair in nums.chunks_mut(2) {
        let [Object::Int(k), v] = pair else { continue };
        if !keys.contains(k) {
            continue;
        }
        match res(objects, v) {
            Object::Array(a) => named.extend(a.iter().filter_map(Object::as_ref).map(|r| r.num)),
            Object::Dict(_) => {
                if let Some(r) = v.as_ref() {
                    named.insert(r.num);
                }
            }
            _ => {}
        }
        *v = Object::Null;
        changed = true;
    }
    if !changed {
        return (false, kids);
    }
    match indirect {
        Some(n) => {
            if let Some(e) = objects.get_mut(&n) {
                e.obj = Object::Array(nums);
                touch(e);
            }
            (false, kids)
        }
        None => {
            node.set("Nums", Object::Array(nums));
            (true, kids)
        }
    }
}

/// Nulls the parent-tree entries whose keys are in `keys` (module docs) and returns the elements they
/// named. A direct root node is edited inside `root` (the structure tree root's copy).
fn scrub_parent_tree(objects: &mut Objs, root: &mut Dict, keys: &BTreeSet<i64>, depth_cap: usize) -> BTreeSet<u32> {
    let mut named = BTreeSet::new();
    let mut work: Vec<(u32, usize)> = Vec::new();
    match root.get("ParentTree").cloned() {
        Some(Object::Ref(r)) => work.push((r.num, 0)),
        Some(Object::Dict(mut d)) => {
            let (changed, kids) = scrub_nums(objects, &mut d, keys, &mut named);
            work.extend(kids.into_iter().map(|k| (k, 1)));
            if changed {
                root.set("ParentTree", Object::Dict(d));
            }
        }
        _ => return named,
    }
    let mut seen = BTreeSet::new();
    while let Some((n, depth)) = work.pop() {
        if depth > depth_cap || !seen.insert(n) {
            continue;
        }
        let Some(mut node) = objects.get(&n).and_then(|e| e.obj.as_dict()).cloned() else { continue };
        let (changed, kids) = scrub_nums(objects, &mut node, keys, &mut named);
        work.extend(kids.into_iter().map(|k| (k, depth.saturating_add(1))));
        if changed && let Some(e) = objects.get_mut(&n) {
            e.obj = Object::Dict(node);
            touch(e);
        }
    }
    named
}

/// Neutralizes the structure elements that point at the flattened pages (module docs). Returns how many.
pub(crate) fn neutralize_structure(doc: &mut PdfDoc, flats: &[Flat], annots: &BTreeSet<u32>, cancel: &CancellationToken) -> Result<u32> {
    let root = catalog_num(doc)?;
    let Some(tree_ref) = doc.dict_of(root).and_then(|c| c.get("StructTreeRoot")).and_then(Object::as_ref) else { return Ok(0) };
    let Some(tree) = doc.dict_of(tree_ref.num).cloned() else { return Ok(0) };
    let flat: BTreeSet<u32> = flats.iter().map(|f| f.page.object).collect();
    let mut keys: BTreeSet<i64> = flats.iter().filter_map(|f| f.old.get("StructParents").map(|v| doc.resolve(v)).and_then(Object::as_int)).collect();
    for a in annots {
        if let Some(k) = doc.dict_of(*a).and_then(|d| d.get("StructParent")).map(|v| doc.resolve(v)).and_then(Object::as_int) {
            keys.insert(k);
        }
    }
    // The root's /K and a direct /ParentTree are processed inside a copy of the root.
    let mut root_copy = tree.clone();
    let depth_cap = doc.limits.depth_cap();
    let mapped = scrub_parent_tree(&mut doc.objects, &mut root_copy, &keys, depth_cap);
    let mut pass = StructPass { flat: &flat, annots, mapped, neutralized: 0 };
    let mut work: Vec<(u32, Option<u32>)> = Vec::new();
    let top: Vec<Object> = match root_copy.get("K") {
        Some(Object::Array(a)) => a.clone(),
        Some(o) => vec![o.clone()],
        None => Vec::new(),
    };
    let mut new_top = Vec::with_capacity(top.len());
    for mut item in top {
        match &mut item {
            Object::Ref(r) => work.push((r.num, None)),
            Object::Dict(d) if d.get("S").is_some() => {
                let objs = &doc.objects;
                pass.element(objs, d, None, false, 0, &mut work);
            }
            _ => {}
        }
        new_top.push(item);
    }
    if !new_top.is_empty() {
        root_copy.set("K", if new_top.len() == 1 && !matches!(tree.get("K"), Some(Object::Array(_))) { new_top.remove(0) } else { Object::Array(new_top) });
    }
    if root_copy != tree
        && let Some(e) = doc.objects.get_mut(&tree_ref.num)
    {
        e.obj = Object::Dict(root_copy);
        touch(e);
    }
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    let mut steps = 0usize;
    let mut extra: Vec<u32> = pass.mapped.iter().copied().collect();
    loop {
        let (n, inherited) = match work.pop() {
            Some(x) => x,
            None => match extra.pop() {
                Some(n) => (n, None),
                None => break,
            },
        };
        if !seen.insert(n) {
            continue;
        }
        steps = steps.saturating_add(1);
        if steps & 255 == 0 {
            check_cancel(cancel)?;
        }
        if steps > doc.limits.max_objects {
            return Err(PressError::InputLimit("structure"));
        }
        let Some(mut d) = doc.objects.get(&n).and_then(|e| e.obj.as_dict()).filter(|d| d.get("S").is_some()).cloned() else { continue };
        let before = d.clone();
        let is_mapped = pass.mapped.contains(&n);
        let objs = &doc.objects;
        pass.element(objs, &mut d, inherited, is_mapped, 0, &mut work);
        if d != before
            && let Some(e) = doc.objects.get_mut(&n)
        {
            e.obj = Object::Dict(d);
            touch(e);
        }
    }
    Ok(pass.neutralized)
}

// ------------------------------------------------------------------------------- painted sources (F6)

/// Resource categories a content stream reaches only by naming an entry.
const GATED: [&str; 5] = ["XObject", "Pattern", "Shading", "ExtGState", "Font"];

/// One scope: content streams and the resource dictionaries their names resolve in (nearest first).
struct Scope<'a> {
    streams: Vec<u32>,
    chain: Vec<&'a Dict>,
    certain: bool,
}

struct Usage<'a> {
    objs: &'a Objs,
    limits: &'a Limits,
    /// Pages and page-tree nodes: their `/Resources` is read by page scopes, not followed.
    page_holders: &'a BTreeSet<u32>,
    reached: BTreeSet<u32>,
    certain: BTreeSet<u32>,
    traversed: BTreeSet<(u32, bool)>,
    work: Vec<(u32, bool)>,
    scopes: Vec<Scope<'a>>,
    scoped: BTreeSet<(u32, bool)>,
    /// Resource dictionaries whose plain (not gated) entries were followed.
    plain_done: BTreeSet<(usize, bool)>,
    budget: usize,
    steps: usize,
    incomplete: bool,
}

fn ptr(d: &Dict) -> usize {
    d as *const Dict as usize
}

impl<'a> Usage<'a> {
    fn mark(&mut self, n: u32, certain: bool) {
        self.reached.insert(n);
        if certain {
            self.certain.insert(n);
        }
    }

    fn push(&mut self, n: u32, certain: bool) {
        self.mark(n, certain);
        self.work.push((n, certain));
    }

    /// Follows every reference inside `o` (plain).
    fn follow(&mut self, o: &'a Object, certain: bool) {
        let mut stack: Vec<(&'a Object, usize)> = vec![(o, 0)];
        while let Some((o, depth)) = stack.pop() {
            if depth > 256 {
                continue;
            }
            let next = depth.saturating_add(1);
            match o {
                Object::Ref(r) => self.push(r.num, certain),
                Object::Array(a) => stack.extend(a.iter().map(|x| (x, next))),
                Object::Dict(d) => stack.extend(d.entries.iter().map(|(_, v)| (v, next))),
                Object::Stream(s) => stack.extend(s.dict.entries.iter().map(|(_, v)| (v, next))),
                _ => {}
            }
        }
    }

    /// A resource dictionary of a scope: marks it (and its gated sub-dictionaries) reached and follows
    /// its plain entries.
    fn resources(&mut self, holder_value: &'a Object, certain: bool) -> Option<&'a Dict> {
        if let Some(r) = holder_value.as_ref() {
            self.mark(r.num, certain);
        }
        let d = res(self.objs, holder_value).as_dict()?;
        if self.plain_done.insert((ptr(d), certain)) {
            for (k, v) in &d.entries {
                if GATED.iter().any(|g| g.as_bytes() == k.as_slice()) {
                    if let Some(r) = v.as_ref() {
                        self.mark(r.num, certain);
                    }
                } else {
                    self.follow(v, certain);
                }
            }
        }
        Some(d)
    }

    /// Plain traversal of one object (module docs: scope holders' `/Resources` are not followed).
    fn traverse(&mut self, n: u32, certain: bool) {
        if !self.traversed.insert((n, certain)) {
            return;
        }
        let objs = self.objs;
        let Some(e) = objs.get(&n) else { return };
        let Some(d) = e.obj.as_dict() else {
            self.follow(&e.obj, certain);
            return;
        };
        let stream = matches!(e.obj, Object::Stream(_));
        let type3 = d.get("Subtype").map(|t| res(objs, t)).is_some_and(|t| t.is_name("Type3"));
        let page_holder = self.page_holders.contains(&n);
        let own_scope = (stream || type3) && d.get("Resources").is_some();
        for (k, v) in &d.entries {
            if k == b"Resources" && (page_holder || own_scope) {
                continue;
            }
            self.follow(v, certain);
        }
        if own_scope && let Some(rv) = d.get("Resources") {
            let Some(rd) = self.resources(rv, certain) else { return };
            let streams: Vec<u32> = if type3 {
                d.get("CharProcs").map(|c| res(objs, c)).and_then(Object::as_dict).map(|p| p.entries.iter().filter_map(|(_, v)| v.as_ref()).map(|r| r.num).collect()).unwrap_or_default()
            } else {
                vec![n]
            };
            if self.scoped.insert((n, certain)) {
                self.scopes.push(Scope { streams, chain: vec![rd], certain });
            }
        }
    }

    /// Follows every gated entry of a scope's dictionaries (its content could not be read).
    fn all_used(&mut self, chain: &[&'a Dict]) {
        for d in chain {
            for g in GATED {
                if let Some(sub) = d.get(g).map(|s| res(self.objs, s)).and_then(Object::as_dict) {
                    for (_, v) in &sub.entries {
                        self.follow(v, false);
                    }
                }
            }
        }
    }

    /// The entry `name` of category `cat` in the scope's dictionaries (nearest first).
    fn lookup(&self, chain: &[&'a Dict], cat: &str, name: &[u8]) -> Vec<&'a Object> {
        chain.iter().filter_map(|d| d.get(cat).map(|s| res(self.objs, s)).and_then(Object::as_dict).and_then(|s| s.get_key(name))).collect()
    }

    /// Reads one scope (module docs). `Err(())`: its content (or that of something it draws in the same
    /// scope) could not be read.
    fn read_scope(&mut self, streams: &[u32], chain: &[&'a Dict], certain: bool, depth: usize, seen: &mut BTreeSet<u32>) -> std::result::Result<(), ()> {
        if depth > 16 {
            return Err(());
        }
        let objs = self.objs;
        for num in streams {
            if !seen.insert(*num) {
                continue;
            }
            self.steps = self.steps.saturating_add(1);
            if self.steps > self.limits.max_objects {
                self.incomplete = true;
                return Err(());
            }
            let Some(Object::Stream(s)) = objs.get(num).map(|e| &e.obj) else { return Err(()) };
            let data = filters::decode_stream(s, self.limits, &mut self.budget).map_err(|_| ())?;
            let cs = |n: &[u8]| chain.iter().find_map(|d| resource_components(objs, Some(*d), n));
            let parsed = parse_strict(&data, self.limits, &cs, &mut self.budget).map_err(|_| ())?;
            for op in &parsed.ops {
                let (cat, name) = match op.name.as_slice() {
                    b"Do" => ("XObject", op.operands.first()),
                    b"sh" => ("Shading", op.operands.first()),
                    b"gs" => ("ExtGState", op.operands.first()),
                    b"Tf" => ("Font", op.operands.first()),
                    b"scn" | b"SCN" => ("Pattern", op.operands.last()),
                    _ => continue,
                };
                let Some(name) = name.and_then(Object::as_name) else { continue };
                for target in self.lookup(chain, cat, name) {
                    self.follow(target, certain);
                    // Drawn in this scope: what has no resources of its own names in this scope.
                    let Some(d) = res(objs, target).as_dict() else { continue };
                    let inner: Vec<u32> = match cat {
                        "XObject" | "Pattern" if d.get("Resources").is_none() => target.as_ref().map(|r| vec![r.num]).unwrap_or_default(),
                        "ExtGState" => d
                            .get("SMask")
                            .map(|m| res(objs, m))
                            .and_then(Object::as_dict)
                            .and_then(|m| m.get("G"))
                            .and_then(Object::as_ref)
                            .filter(|g| objs.get(&g.num).and_then(|e| e.obj.as_dict()).is_some_and(|gd| gd.get("Resources").is_none()))
                            .map(|g| vec![g.num])
                            .unwrap_or_default(),
                        "Font" if d.get("Resources").is_none() && d.get("Subtype").map(|t| res(objs, t)).is_some_and(|t| t.is_name("Type3")) => d
                            .get("CharProcs")
                            .map(|c| res(objs, c))
                            .and_then(Object::as_dict)
                            .map(|p| p.entries.iter().filter_map(|(_, v)| v.as_ref()).map(|r| r.num).collect())
                            .unwrap_or_default(),
                        _ => Vec::new(),
                    };
                    // Only streams are content (an image XObject is not read).
                    let inner: Vec<u32> = inner
                        .into_iter()
                        .filter(|n| matches!(objs.get(n).map(|e| &e.obj), Some(Object::Stream(s)) if !s.dict.get("Subtype").map(|t| res(objs, t)).is_some_and(|t| t.is_name("Image"))))
                        .collect();
                    if !inner.is_empty() {
                        self.read_scope(&inner, chain, certain, depth.saturating_add(1), seen)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn drain(&mut self, cancel: &CancellationToken) -> Result<()> {
        loop {
            if let Some((n, certain)) = self.work.pop() {
                self.steps = self.steps.saturating_add(1);
                if self.steps & 1023 == 0 {
                    check_cancel(cancel)?;
                }
                self.traverse(n, certain);
                continue;
            }
            let Some(scope) = self.scopes.pop() else { return Ok(()) };
            let mut seen = BTreeSet::new();
            if self.read_scope(&scope.streams, &scope.chain, scope.certain, 0, &mut seen).is_err() {
                self.all_used(&scope.chain);
            }
        }
    }
}

/// Drops the flattened pages' sources no kept content draws (module docs). Returns the painted sources
/// (images, forms, patterns, shadings) that stay in the file only because some usage is not determined.
pub(crate) fn drop_painted_sources(doc: &mut PdfDoc, flats: &[Flat], cancel: &CancellationToken) -> Result<Vec<u32>> {
    let sources: BTreeSet<u32> = flats.iter().flat_map(|f| f.sources.iter().copied()).collect();
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    let (reached, certain, incomplete) = {
        let objs: &Objs = &doc.objects;
        let mut holders: BTreeSet<u32> = doc.pages.iter().copied().collect();
        // Page-tree nodes: every node on a page's /Parent chain.
        for p in &doc.pages {
            let mut n = *p;
            for _ in 0..doc.limits.depth_cap() {
                let Some(parent) = objs.get(&n).and_then(|e| e.obj.as_dict()).and_then(|d| d.get("Parent")).and_then(Object::as_ref) else { break };
                if !holders.insert(parent.num) {
                    break;
                }
                n = parent.num;
            }
        }
        let mut u = Usage {
            objs,
            limits: &doc.limits,
            page_holders: &holders,
            reached: BTreeSet::new(),
            certain: BTreeSet::new(),
            traversed: BTreeSet::new(),
            work: Vec::new(),
            scopes: Vec::new(),
            scoped: BTreeSet::new(),
            plain_done: BTreeSet::new(),
            budget: doc.limits.max_decoded_total,
            steps: 0,
            incomplete: false,
        };
        for r in trailer_roots(&doc.trailer) {
            u.push(r, true);
        }
        // Page scopes: own /Resources and every ancestor's.
        for p in &doc.pages {
            let Some(pd) = objs.get(p).and_then(|e| e.obj.as_dict()) else { continue };
            let mut chain: Vec<&Dict> = Vec::new();
            let mut node = Some(pd);
            for _ in 0..=doc.limits.depth_cap() {
                let Some(d) = node else { break };
                if let Some(rv) = d.get("Resources")
                    && let Some(rd) = u.resources(rv, true)
                {
                    chain.push(rd);
                }
                node = d.get("Parent").map(|x| res(objs, x)).and_then(Object::as_dict);
            }
            let streams = crate::show_text::content_streams_of(objs, pd).unwrap_or_default();
            u.scopes.push(Scope { streams, chain, certain: true });
        }
        u.drain(cancel)?;
        (u.reached, u.certain, u.incomplete)
    };
    let painted: BTreeSet<u32> = sources
        .iter()
        .filter(|n| {
            doc.dict_of(**n).is_some_and(|d| {
                d.get("Subtype").map(|t| doc.resolve(t)).is_some_and(|t| t.is_name("Image") || t.is_name("Form"))
                    || d.get("PatternType").is_some()
                    || d.get("ShadingType").is_some()
            })
        })
        .copied()
        .collect();
    if incomplete {
        // The analysis did not finish: nothing is dropped; every painted source still in the file is
        // reported.
        let live = reachable(&doc.objects, &trailer_roots(&doc.trailer));
        return Ok(painted.iter().filter(|n| live.contains(n)).copied().collect());
    }
    let gone: BTreeSet<u32> = sources.iter().filter(|n| !reached.contains(n)).copied().collect();
    if !gone.is_empty() {
        // Every listing of a dropped source is a gated entry nobody uses (module docs).
        let mut subdicts: BTreeSet<u32> = BTreeSet::new();
        for e in doc.objects.values_mut() {
            let mut changed = false;
            prune_gated(&mut e.obj, &gone, &mut subdicts, &mut changed, 0);
            if changed {
                touch(e);
            }
        }
        for n in subdicts {
            if let Some(e) = doc.objects.get_mut(&n)
                && let Object::Dict(d) = &mut e.obj
            {
                let before = d.entries.len();
                d.entries.retain(|(_, v)| !matches!(v, Object::Ref(r) if gone.contains(&r.num)));
                if d.entries.len() != before {
                    touch(e);
                }
            }
        }
    }
    let live = reachable(&doc.objects, &trailer_roots(&doc.trailer));
    Ok(painted.iter().filter(|n| live.contains(n) && !certain.contains(n)).copied().collect())
}

/// Removes gated entries naming `gone` from every resource dictionary inside `o`; gated sub-dictionaries
/// that are indirect objects are collected in `subdicts`.
fn prune_gated(o: &mut Object, gone: &BTreeSet<u32>, subdicts: &mut BTreeSet<u32>, changed: &mut bool, depth: usize) {
    if depth > 64 {
        return;
    }
    let next = depth.saturating_add(1);
    match o {
        Object::Array(a) => a.iter_mut().for_each(|x| prune_gated(x, gone, subdicts, changed, next)),
        Object::Dict(_) | Object::Stream(_) => {
            let d = match o {
                Object::Dict(d) => d,
                Object::Stream(s) => &mut s.dict,
                _ => return,
            };
            for (k, v) in d.entries.iter_mut() {
                if GATED.iter().any(|g| g.as_bytes() == k.as_slice()) {
                    match v {
                        Object::Dict(sub) => {
                            let before = sub.entries.len();
                            sub.entries.retain(|(_, x)| !matches!(x, Object::Ref(r) if gone.contains(&r.num)));
                            *changed |= sub.entries.len() != before;
                        }
                        Object::Ref(r) => {
                            subdicts.insert(r.num);
                        }
                        _ => {}
                    }
                } else {
                    prune_gated(v, gone, subdicts, changed, next);
                }
            }
        }
        _ => {}
    }
}

/// Document-level check of the SAVED file (counted in `residual_in_region`): no `/XFA`, no
/// `/NeedsRendering`, no annotation whose `/P` and no structure item whose `/Pg` is a flattened page.
pub(crate) fn check_document(saved: &PdfDoc, flat_pages: &BTreeSet<u32>) -> u32 {
    let mut bad = 0u32;
    if let Ok(c) = saved.catalog() {
        if c.get("NeedsRendering").is_some() {
            bad = bad.saturating_add(1);
        }
        if c.get("AcroForm").map(|f| saved.resolve(f)).and_then(Object::as_dict).is_some_and(|f| f.get("XFA").is_some()) {
            bad = bad.saturating_add(1);
        }
    }
    for e in saved.objects.values() {
        let mut stack: Vec<(&Object, usize)> = vec![(&e.obj, 0)];
        while let Some((o, depth)) = stack.pop() {
            if depth > 64 {
                continue;
            }
            let next = depth.saturating_add(1);
            match o {
                Object::Array(a) => stack.extend(a.iter().map(|x| (x, next))),
                Object::Dict(_) | Object::Stream(_) => {
                    let Some(d) = o.as_dict() else { continue };
                    let on = |k: &str| d.get(k).and_then(Object::as_ref).is_some_and(|r| flat_pages.contains(&r.num));
                    if on("Pg") || (on("P") && d.get("Subtype").is_some()) {
                        bad = bad.saturating_add(1);
                    }
                    stack.extend(d.entries.iter().map(|(_, v)| (v, next)));
                }
                _ => {}
            }
        }
    }
    bad
}
