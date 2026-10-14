//! What a flattened page leaves behind in the document (R13-F1, R13-F2, R13-F6, R13-P1; lead decision
//! 2026-10-11T20:26:55Z): after `flatten.rs` replaced the page and detached it from inheritance, the
//! redaction save purges, by category below, what described or drew the OLD page: its annotations and widgets
//! that no kept page lists are dropped, the painted sources it reached that no kept content names and no kept
//! object references are dropped (one a kept object references stays and is reported), and the structure tree: by default
//! (Operator decision D28, `StructureTreePolicy::Strip`, [`strip_structure`]) the whole tree, identified by role and
//! reachability rather than by shape (R26-S1), is removed with the typed loss `accessibility_tags_removed` (tests
//! `default_redaction_strips_the_structure_tree`, `structure_strip_follows_the_tree_and_structure_destinations`,
//! `structure_strip_keeps_multimedia_of_kept_pages`); with the opt-out
//! (`StructureTreePolicy::KeepNotProven`), in a structure tree whose root is an indirect object, the structure elements that
//! the purge reads and that point at it stay with their text removed (best effort). Whatever the policy, when a save flattens at
//! least one region and the saved document still has a structure tree (or a dictionary with the shape of a structure element,
//! PLR-102), `RedactedOutput::residual_in_region` adds 1, `region_flattened` reads false and `NotProven::ResidualInRegion` is
//! reported (lead decision 2026-10-12T08:41:40Z, R23 FIX_FIRST); the redaction proof covers untagged documents and tagged
//! documents saved with the default strip.
//! So an image, form, annotation or widget value of a flattened page is not kept in the file by being merely listed
//! (tests `painted_sources_under_a_region_do_not_survive`, `structure_and_widgets_of_flattened_pages_are_removed`,
//! `tagged_documents_read_not_proven_whatever_the_tree_looks_like`).
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
//! - Structure ([`neutralize_structure`], R13-P1; best effort, STRUCTURE TREE = NOT PROVEN): every structure element that the
//!   purge reads and that points at a flattened page (its `/Pg` is one, the page's `/StructParents` entry of the parent
//!   tree names it, or it holds a marked-content or object reference on such a page or to a removed annotation) loses its
//!   text and attributes (`/Alt /ActualText /E /T /Lang /A /C /ID /Ref /AF /Phoneme /PhoneticAlphabet`, and `/Pg` when it is
//!   a flattened page) and the content items on flattened pages; its child elements on other pages stay. The parent-tree
//!   entries of the flattened pages and removed annotations are nulled. This edits only a structure tree root that is an
//!   indirect reference to a dictionary (`edited_structure_root`). The purge does not read a root written directly into the
//!   catalog or reached through a chain of references, an alias object (an object that is a reference to another), a `/K`
//!   that is a reference to an array or an array nested in a `/K` array, an element that only the `/ParentTree` or the
//!   `/IDTree` reaches, an element nested deeper than 64 levels, the `/IDTree` keys, the `/ClassMap`, or a grouping element
//!   without `/Pg` whose children are on a flattened page: their text stays in the file. So the removal is not what the
//!   save relies on. [`check_document`] reports the tree instead (R23 FIX_FIRST -> STRUCTURE TREE = NOT PROVEN, lead decision
//!   2026-10-12T08:41:40Z): when a save flattens at least one region and the saved document has a structure tree, a
//!   `/StructTreeRoot` that the catalog holds (directly or through any chain of references), any `/StructParents` or
//!   `/StructParent` entry left in the file, or a dictionary left anywhere in the file that has the shape of a structure
//!   element or of an outline item that names one (PLR-102: `/Type /StructElem` or `/StructTreeRoot`; `/S` as a name with
//!   `/P` and any text key of the element; an `/SE` entry), it adds 1 in `RedactedOutput::residual_in_region` (one count
//!   for all of them, not one per element), `region_flattened` reads false and `NotProven::ResidualInRegion` is reported.
//!   A document without one is not affected, a save with no region does not apply these rules (PLR-101), and a caller who
//!   strips the whole tree before the save gets `region_flattened` true again (tests
//!   `tagged_documents_read_not_proven_whatever_the_tree_looks_like`, `untagged_documents_and_stripped_trees_still_read_flattened`,
//!   `structure_tree_keys_are_read_as_names_not_text`, `structure_parents_left_in_the_file_are_reported`,
//!   `detached_structure_elements_are_reported`, `structure_shape_is_read_from_the_parsed_dictionary`,
//!   `structure_rules_apply_only_when_a_region_is_flattened`).
//! - Painted sources ([`drop_painted_sources`], R13-F6): the images, forms, patterns, shadings, graphics
//!   states and fonts the old page reached are dropped from every resource dictionary that lists them
//!   when nothing refers to them but a gated (`/XObject`, `/Pattern`, `/Shading`, `/ExtGState`, `/Font`)
//!   entry no kept content names (usage analysis below). A painted source (image, form, pattern, shading)
//!   that stays in the file is silent only under the minimal draw set ("Silent" below); every other painted source that
//!   stays is reported (`RedactedOutput::painted_sources_kept`, `NotProven::PaintedSourceKept`) and
//!   `region_flattened` reads false (R14-P1, R16, R17, R18). Only painted sources are reported: a font or a graphics
//!   state that stays because a kept page names it is not one, is not reported and does not change `region_flattened`
//!   (R19; test `a_font_or_graphics_state_that_stays_is_not_a_painted_source`). A painted source is reported when some
//!   content's usage cannot be determined for it, when a kept object only REFERENCES it (a catalog `/AF`, a `/Names`
//!   or `/Dests` name tree, the entries of a kept page's `/Resources` outside the five gated categories such as
//!   `/ColorSpace`, a `/Properties` entry of a kept page that names it directly and that the purge does not remove (a
//!   `/Properties` entry is reached only when kept content names it, PLR-104, so an object only an unnamed property list
//!   reaches is pruned like any unused source), a plain key of an object a kept page draws such as an image's `/Alternates`, an
//!   object used as a `/Resources` value or as a gated sub-dictionary, any other plain reference), and when a viewer
//!   draws it through anything but a direct `Do` of the image by a kept page (a form, tiling pattern, Type 3 glyph
//!   procedure, soft-mask group, mask, shading, annotation appearance, an image listed under another
//!   category, a page whose content streams are named elsewhere). Over-reporting is accepted. Such a source is not
//!   pruned (the kept structure is not edited). Tests
//!   `painted_source_referenced_by_a_kept_catalog_structure_is_reported`,
//!   `painted_source_referenced_but_not_drawn_by_a_kept_page_is_reported`,
//!   `painted_source_referenced_by_a_key_of_a_drawn_object_is_reported`,
//!   `painted_source_placed_as_a_resources_value_is_reported`,
//!   `painted_source_placed_as_a_gated_sub_dictionary_is_reported`,
//!   `painted_source_placed_as_an_appearance_state_is_reported`,
//!   `painted_source_named_where_it_is_not_drawn_is_reported`,
//!   `painted_source_shadowed_by_a_nearer_resource_entry_is_reported`,
//!   `painted_source_in_an_annotation_appearance_is_reported`,
//!   `painted_source_under_optional_content_is_reported`,
//!   `painted_source_named_through_a_stream_used_as_a_dictionary_is_reported`,
//!   `painted_source_drawn_by_an_image_or_shading_stream_read_as_content_is_reported`,
//!   `painted_source_selected_by_a_type3_font_pattern_or_soft_mask_is_reported`,
//!   `painted_source_drawn_by_anything_but_a_kept_pages_own_content_is_reported`,
//!   `painted_sources_rendered_with_a_drawn_image_are_reported`,
//!   `painted_source_is_reported_when_a_content_stream_of_the_page_is_named_elsewhere`.
//! - Marked-content property lists ([`drop_painted_sources`], PLR-104, Operator-confirmed plan 2026-10-14): `/Properties` is a
//!   gated category too. In every `/Properties` dictionary the old page reached (in its own `/Resources` object, a `/Properties`
//!   object its own `/Resources` named, the `/Resources` object or `/Properties` object it inherited from a `/Pages` node also
//!   when a kept page names that object from its own `/Resources` (R26-P1), the `/Resources` a kept page received from a node of
//!   its `/Parent` chain, and the `/Resources` of every stream and Type 3 font it reached), every entry that no `BDC` or `DP` of
//!   the content read names is
//!   removed, inline dictionaries and references alike; the entries kept content names stay (with their text: kept content
//!   invokes them), an optional-content group a kept page names among them. Counted in `RedactedOutput::residual_in_region`
//!   (fail closed, `region_flattened` false, `NotProven::ResidualInRegion`): such a dictionary whose scope's content could not be
//!   read (usage undetermined: every entry stays) or the usage analysis did not finish, one the purge cannot edit (a
//!   `/Resources` or `/Properties` value that is a stream or an alias object) holding an entry no kept content names whose
//!   property list has `/Alt`, `/ActualText`, `/E`, `/T` or `/Lang`, and a property list of a removed or unedited entry that
//!   stays in the file (another object references it) with one of those keys while no kept content names it. A property list
//!   written inline in the old page's content left with the page. Tests
//!   `marked_content_property_lists_of_flattened_pages_are_purged`, `property_lists_the_purge_cannot_clear_are_counted`.
//!
//! Usage analysis: every page of the tree is a scope whose names resolve in its own `/Resources` and in
//! every ancestor's (pdf.js merges them); a scope's content is decoded and strictly parsed and the names
//! of `Do`, `sh`, `scn`/`SCN`, `gs`, `Tf` and the property-list operand of `BDC` and `DP` mark the entries of `/XObject`,
//! `/Shading`, `/Pattern`, `/ExtGState`, `/Font` and `/Properties` it uses. A used form, tiling pattern, soft-mask group or Type 3 font WITHOUT
//! its own `/Resources` is read in the same scope; every stream (and Type 3 font) WITH its own
//! `/Resources`, however it is reached, is a scope of its own. Everything else is followed as a plain
//! reference (reached, not silent). Content that cannot be decoded or parsed makes every entry of its
//! scope used (kept, not silent). This analysis decides what is reached and so what is pruned; it does not decide
//! what is silent.
//!
//! Silent (R18, the minimal draw set; lead decision 2026-10-12T02:48:02Z): a painted source that stays in the file is
//! silent only when it is an image XObject stream (`/Subtype /Image`, no `/OC`) that a kept page draws with `Do`
//! directly in its own content stream(s): the `Do` is in the page's own `/Contents`, not in a form, tiling pattern,
//! Type 3 glyph procedure, annotation appearance, soft-mask group, shading or any other stream read as content, and
//! each stream of that `/Contents` is named by exactly one reference of the whole file (R19, `named_once`: a stream
//! the `/Contents` lists twice, or that anything else refers to, such as a form or another page, makes the page's
//! content non-exclusive and nothing it draws is silent; the pages whose streams are named once are read exactly, in
//! the order listed); it is
//! outside optional content (inside no marked content opened by `BDC` with the tag `/OC`, from the `BDC` to its
//! matching `EMC`, with the nesting of `BMC`, `BDC` and `EMC` tracked per operator across the streams of the page,
//! and the image has no `/OC`: optional content is not evaluated, it counts as possibly hidden); and its name resolves
//! through the page's own nearest resource entry (the nearest entry of that name in `/Resources` `/XObject` of the page
//! and of its ancestors; an entry a nearer one shadows is reached only), every container on that path (the page, each
//! ancestor node, the `/Resources` value, the `/XObject` value) being a dictionary object, not a stream. Nothing
//! else is silent, however a viewer draws it: a source that a form, tiling pattern, Type 3 font, soft-mask group,
//! image mask, shading or annotation appearance draws, one an operator selects without painting (`Tf`, `scn`, `gs`),
//! or one named through an object of another kind. Tests `painted_sources_under_a_region_do_not_survive`,
//! `painted_source_drawn_directly_by_a_kept_page_is_not_reported`, `painted_source_under_optional_content_is_reported`,
//! `painted_source_shadowed_by_a_nearer_resource_entry_is_reported`, and the tests listed above.
//!
//! Residual (PLR-084, lead decision 2026-10-12T05:10:24Z; D27 "narrow": no visibility analysis is added): silence is
//! syntactic. It means a kept page's own content stream draws the image directly with `Do`; the visibility of that draw
//! (a zero-scale `cm`, an empty clip, an alpha of 0, a later object painted over it) is not checked, so a silent source
//! can be invisible or covered on the kept page. Test `silent_painted_source_visibility_is_not_checked`.
#![deny(clippy::arithmetic_side_effects)]
use crate::content::parse_strict;
use crate::error::{Limits, MAX_REF_HOPS, PressError, Result};
use crate::filters;
use crate::flatten::{Flat, refs_in};
use crate::pdf_edit::PdfDoc;
use crate::reader::{Entry, check_cancel};
use crate::redaction::{purge_list, res};
use crate::show_text::resource_components;
use crate::syntax::{Dict, Lexer, ObjRef, Object, Stream, parse_standalone};
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

/// The structure tree root [`neutralize_structure`] edits: the catalog's `/StructTreeRoot` when it is an indirect reference
/// to a dictionary. Any other spelling (a dictionary written directly into the catalog, a chain of references) is left as
/// it is. The save reports a structure tree whatever its spelling ([`check_document`], STRUCTURE TREE = NOT PROVEN).
fn edited_structure_root(doc: &PdfDoc, catalog: u32) -> Option<ObjRef> {
    let r = doc.dict_of(catalog)?.get("StructTreeRoot").and_then(Object::as_ref)?;
    doc.dict_of(r.num).map(|_| r)
}

/// Neutralizes the structure elements that point at the flattened pages (module docs). Returns how many.
pub(crate) fn neutralize_structure(doc: &mut PdfDoc, flats: &[Flat], annots: &BTreeSet<u32>, cancel: &CancellationToken) -> Result<u32> {
    let root = catalog_num(doc)?;
    let Some(tree_ref) = edited_structure_root(doc, root) else { return Ok(0) };
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

// ------------------------------------------------------------------------------- structure strip (D28)

/// Entries that tie an object to the structure tree (D28 strip): a page's, an annotation's or a form's key into the parent tree,
/// and an outline item's structure element. The structure destination `/SD` of a `GoTo` action is one too (R26-S2), removed by
/// [`strip_links`] when the action's `/S` is the name `GoTo`.
const STRUCT_LINK_KEYS: [&str; 3] = ["StructParents", "StructParent", "SE"];

fn is_link_key(k: &[u8]) -> bool {
    STRUCT_LINK_KEYS.iter().any(|s| s.as_bytes() == k)
}

/// `d` is a `GoTo` action as [`strip_links`] reads it: `/S` is the name `GoTo` written directly.
fn is_goto(d: &Dict) -> bool {
    matches!(d.get("S"), Some(Object::Name(n)) if n.as_slice() == b"GoTo")
}

/// Removes [`STRUCT_LINK_KEYS`] (and the `/SD` of a `GoTo` action) from every dictionary inside `o`, to `cap` levels; true when
/// one was removed.
fn strip_links(o: &mut Object, depth: usize, cap: usize) -> bool {
    if depth > cap {
        return false;
    }
    let next = depth.saturating_add(1);
    match o {
        Object::Array(a) => a.iter_mut().fold(false, |changed, x| strip_links(x, next, cap) | changed),
        Object::Dict(_) | Object::Stream(_) => {
            let d = match o {
                Object::Dict(d) => d,
                Object::Stream(s) => &mut s.dict,
                _ => return false,
            };
            let goto = is_goto(d);
            let before = d.entries.len();
            d.entries.retain(|(k, _)| !is_link_key(k) && !(goto && k.as_slice() == b"SD"));
            let mut changed = d.entries.len() != before;
            for (_, v) in d.entries.iter_mut() {
                changed |= strip_links(v, next, cap);
            }
            changed
        }
        _ => false,
    }
}

/// The objects the structure links inside `o` name (to `cap` levels), read before [`strip_links`] removes them: the element of
/// an outline item's `/SE`, and the first element of a `GoTo` action's structure destination `/SD` (R26-S2: a structure element
/// by the action's definition, whatever keys it has).
fn link_targets(doc: &PdfDoc, o: &Object, depth: usize, cap: usize, out: &mut BTreeSet<u32>) {
    if depth > cap {
        return;
    }
    let next = depth.saturating_add(1);
    match o {
        Object::Array(a) => a.iter().for_each(|x| link_targets(doc, x, next, cap, out)),
        Object::Dict(_) | Object::Stream(_) => {
            let Some(d) = o.as_dict() else { return };
            if let Some(r) = d.get("SE").and_then(Object::as_ref) {
                out.insert(r.num);
            }
            if is_goto(d)
                && let Some(r) = d.get("SD").map(|v| doc.resolve(v)).and_then(Object::as_array).and_then(|a| a.first()).and_then(Object::as_ref)
            {
                out.insert(r.num);
            }
            d.entries.iter().for_each(|(_, v)| link_targets(doc, v, next, cap, out));
        }
        _ => {}
    }
}

/// `/Type` is the name `StructElem` or `StructTreeRoot`.
fn typed_structure(doc: &PdfDoc, d: &Dict) -> bool {
    d.get("Type").map(|v| doc.resolve(v)).is_some_and(|t| t.is_name("StructElem") || t.is_name("StructTreeRoot"))
}

/// A structure element by its own keys, read only where the structure tree (or a structure link) names an element: `/S` is a
/// name and `/Type`, when present, is `StructElem`; or `/Type` is `StructElem`. A dictionary with `/S` that sits anywhere else (a
/// media rendition `/S /MR`, a media clip `/S /MCD`, an action) is not one (R26-S1).
fn element_like(doc: &PdfDoc, d: &Dict) -> bool {
    let typ = d.get("Type").map(|t| doc.resolve(t));
    typ.is_some_and(|t| t.is_name("StructElem")) || (typ.is_none() && d.get("S").map(|v| doc.resolve(v)).is_some_and(|v| matches!(v, Object::Name(_))))
}

/// A content item of an element's `/K` written as an object: a marked-content reference or an object reference dictionary.
fn content_item(doc: &PdfDoc, d: &Dict) -> bool {
    d.get("Type").map(|t| doc.resolve(t)).is_some_and(|t| t.is_name("MCR") || t.is_name("OBJR"))
}

/// The role in which the structure tree names a value ([`structure_objects`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Role {
    /// A structure tree root: its `/K`, `/ParentTree`, `/IDTree`, `/ClassMap` and `/RoleMap` are followed.
    Root,
    /// A structure element (or an array of them, or a content item): its `/K`, `/A` and `/Ref` are followed.
    Element,
    /// A node of the `/ParentTree` number tree (or a `/Kids` array of them).
    NumberTree,
    /// A node of the `/IDTree` name tree (or a `/Kids` array of them).
    NameTree,
    /// A `/Nums` or `/Names` array: every second value is an element.
    Pairs,
    /// The `/ClassMap`: every value is an attribute object or an array of them.
    ClassMap,
    /// An attribute object (a dictionary with `/O`) or an array of them.
    Attribute,
    /// The `/RoleMap`: the dictionary itself.
    Whole,
}

/// A value [`structure_objects`] reads: written inside an object, or object `n`.
#[derive(Clone, Copy)]
enum Item<'a> {
    Val(&'a Object),
    Num(u32),
}

/// The objects of the structure tree, identified by their role and reachability (R26-S1, R26-S2, R26-S3), not by their shape:
/// starting from the catalog's `/StructTreeRoot` (`root`, read before the strip), every object whose `/Type` is `StructTreeRoot`
/// or `StructElem`, and the elements the structure links name (`links`: `/SE`, the first element of a `GoTo` action's `/SD`), it
/// follows a root's `/K` (elements), `/ParentTree` (number-tree nodes, `/Nums` arrays, the elements and element arrays they hold),
/// `/IDTree` (name-tree nodes, `/Names` arrays, elements), `/ClassMap` (the map and its attribute objects) and `/RoleMap`, an
/// element's `/K` (child elements, arrays of them, marked-content and object reference dictionaries), `/A` (attribute objects) and
/// `/Ref` (elements), and every element whose `/P` is an object of the tree. An object the tree names where an element belongs is
/// taken only when it is one by its keys ([`element_like`], [`content_item`]); the pages, annotations, streams and files an element
/// points at (`/Pg`, `/Obj`, `/Stm`, `/AF`) are not followed. An alias object (an object that is a reference) passes the role on
/// to its target and is not taken itself.
fn structure_objects(doc: &PdfDoc, root: Option<&Object>, links: &BTreeSet<u32>, cancel: &CancellationToken) -> Result<BTreeSet<u32>> {
    let objs = &doc.objects;
    let cap = doc.limits.depth_cap();
    // The elements that name each object as their parent (`/P`): a detached element belongs to the tree when its parent does.
    let mut children: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (num, e) in objs {
        if let Some(d) = e.obj.as_dict()
            && element_like(doc, d)
            && let Some(p) = d.get("P").and_then(Object::as_ref)
        {
            children.entry(p.num).or_default().push(*num);
        }
    }
    let mut work: Vec<(Item<'_>, Role, usize)> = Vec::new();
    if let Some(r) = root {
        work.push((Item::Val(r), Role::Root, 0));
    }
    for (num, e) in objs {
        if let Some(d) = e.obj.as_dict()
            && typed_structure(doc, d)
        {
            let role = if d.get("Type").map(|t| doc.resolve(t)).is_some_and(|t| t.is_name("StructTreeRoot")) { Role::Root } else { Role::Element };
            work.push((Item::Num(*num), role, 0));
        }
    }
    work.extend(links.iter().map(|n| (Item::Num(*n), Role::Element, 0)));
    let mut tree: BTreeSet<u32> = BTreeSet::new();
    let mut visited: BTreeSet<(u32, Role)> = BTreeSet::new();
    let mut steps = 0usize;
    while let Some((item, role, depth)) = work.pop() {
        steps = steps.saturating_add(1);
        if steps & 255 == 0 {
            check_cancel(cancel)?;
        }
        if depth > cap {
            continue;
        }
        let next = depth.saturating_add(1);
        let (num, value) = match item {
            Item::Num(n) => match objs.get(&n) {
                Some(e) => (Some(n), &e.obj),
                None => continue,
            },
            Item::Val(Object::Ref(r)) => match objs.get(&r.num) {
                Some(e) => (Some(r.num), &e.obj),
                None => continue,
            },
            Item::Val(v) => (None, v),
        };
        if let Some(n) = num {
            if !visited.insert((n, role)) {
                continue;
            }
            if matches!(value, Object::Ref(_)) {
                work.push((Item::Val(value), role, next));
                continue;
            }
        }
        let mut take = false;
        match (role, value) {
            (Role::Root, Object::Dict(d)) => {
                take = true;
                for (key, r) in [("K", Role::Element), ("ParentTree", Role::NumberTree), ("IDTree", Role::NameTree), ("ClassMap", Role::ClassMap), ("RoleMap", Role::Whole)] {
                    if let Some(v) = d.get(key) {
                        work.push((Item::Val(v), r, next));
                    }
                }
            }
            (Role::Element | Role::Attribute | Role::NumberTree | Role::NameTree, Object::Array(a)) => {
                take = true;
                work.extend(a.iter().map(|x| (Item::Val(x), role, next)));
            }
            (Role::Pairs, Object::Array(a)) => {
                take = true;
                work.extend(a.iter().skip(1).step_by(2).map(|x| (Item::Val(x), Role::Element, next)));
            }
            (Role::Element, o) => {
                let Some(d) = o.as_dict() else { continue };
                if content_item(doc, d) {
                    take = true;
                } else if element_like(doc, d) {
                    take = true;
                    for (key, r) in [("K", Role::Element), ("A", Role::Attribute), ("Ref", Role::Element)] {
                        if let Some(v) = d.get(key) {
                            work.push((Item::Val(v), r, next));
                        }
                    }
                }
            }
            (Role::NumberTree | Role::NameTree, Object::Dict(d)) => {
                take = true;
                if let Some(v) = d.get("Kids") {
                    work.push((Item::Val(v), role, next));
                }
                for key in ["Nums", "Names"] {
                    if let Some(v) = d.get(key) {
                        work.push((Item::Val(v), Role::Pairs, next));
                    }
                }
            }
            (Role::ClassMap, Object::Dict(d)) => {
                take = true;
                work.extend(d.entries.iter().map(|(_, v)| (Item::Val(v), Role::Attribute, next)));
            }
            (Role::Attribute, Object::Dict(d)) => take = holds(d, "O"),
            (Role::Whole, Object::Dict(_)) => take = true,
            _ => {}
        }
        if take && let Some(n) = num {
            tree.insert(n);
            if let Some(kids) = children.get(&n) {
                work.extend(kids.iter().map(|c| (Item::Num(*c), Role::Element, next)));
            }
        }
    }
    Ok(tree)
}

/// `d` is a structure dictionary the strip removes where it is written inside another object: `/Type` `StructElem` or
/// `StructTreeRoot`, or an element ([`element_like`]) whose `/P` is an object of the tree.
fn strip_target(doc: &PdfDoc, tree: &BTreeSet<u32>, d: &Dict) -> bool {
    typed_structure(doc, d) || (element_like(doc, d) && d.get("P").and_then(Object::as_ref).is_some_and(|p| tree.contains(&p.num)))
}

/// A direct dictionary the strip removes ([`strip_target`]) is nested inside `o` (`o` itself is not judged), to `cap` levels.
fn nested_shape(doc: &PdfDoc, tree: &BTreeSet<u32>, o: &Object, depth: usize, cap: usize) -> bool {
    if depth > cap {
        return false;
    }
    let next = depth.saturating_add(1);
    let shaped = |x: &Object| matches!(x, Object::Dict(d) if strip_target(doc, tree, d));
    match o {
        Object::Array(a) => a.iter().any(|x| shaped(x) || nested_shape(doc, tree, x, next, cap)),
        Object::Dict(_) | Object::Stream(_) => o.as_dict().is_some_and(|d| d.entries.iter().any(|(_, v)| shaped(v) || nested_shape(doc, tree, v, next, cap))),
        _ => false,
    }
}

/// Removes the direct dictionaries the strip removes ([`strip_target`]) nested inside `o` (an array element becomes null, a
/// dictionary entry is removed), to `cap` levels.
fn strip_nested_shapes(doc: &PdfDoc, tree: &BTreeSet<u32>, o: &mut Object, depth: usize, cap: usize) {
    if depth > cap {
        return;
    }
    let next = depth.saturating_add(1);
    let shaped = |x: &Object| matches!(x, Object::Dict(d) if strip_target(doc, tree, d));
    match o {
        Object::Array(a) => {
            for x in a.iter_mut() {
                if shaped(x) {
                    *x = Object::Null;
                } else {
                    strip_nested_shapes(doc, tree, x, next, cap);
                }
            }
        }
        Object::Dict(_) | Object::Stream(_) => {
            let d = match o {
                Object::Dict(d) => d,
                Object::Stream(s) => &mut s.dict,
                _ => return,
            };
            d.entries.retain(|(_, v)| !shaped(v));
            for (_, v) in d.entries.iter_mut() {
                strip_nested_shapes(doc, tree, v, next, cap);
            }
        }
        _ => {}
    }
}

/// Operator decision D28 (2026-10-14; the default of a redaction save that flattens a region, `StructureTreePolicy::Strip`):
/// removes the whole structure tree. The catalog's `/StructTreeRoot` and `/MarkInfo`, every `/StructParents`, `/StructParent`
/// and `/SE` entry of every dictionary and the `/SD` of every `GoTo` action, every object of the structure tree identified by its
/// role and reachability ([`structure_objects`]: the root, the elements, the parent-tree and id-tree nodes, the class map and
/// role map, attribute objects, content-item dictionaries, the elements `/SE` and `/SD` name, the elements whose `/P` is in the
/// tree) with every direct reference to it (`scrub_references`), and every structure dictionary written directly inside another
/// object ([`strip_target`]). A dictionary is not removed for its shape alone (R26-S1: a media rendition or media clip with `/S`,
/// `/P` and a text key stays; `check_document` still reads such a shape and reports it, test
/// `structure_strip_keeps_multimedia_of_kept_pages`). What only the tree reached is no longer reachable and is not written.
/// Content streams are not edited: marked-content operators (`BDC` with `/MCID`) stay. What the strip cannot reach (an element
/// inside an object stream the file keeps as a stream, an alias object of an element that another object keeps reachable) stays
/// and `check_document` reports it (fail closed). Returns whether a structure tree or a structure entry the saved file would hold
/// was removed (the typed loss `accessibility_tags_removed`); `/MarkInfo` alone is not one (R26-S4). Tests
/// `default_redaction_strips_the_structure_tree`, `structure_strip_follows_the_tree_and_structure_destinations`,
/// `structure_strip_reports_no_loss_without_structure`.
pub(crate) fn strip_structure(doc: &mut PdfDoc, cancel: &CancellationToken) -> Result<bool> {
    check_cancel(cancel)?;
    let root = catalog_num(doc)?;
    let cap = doc.limits.depth_cap();
    // Only what the saved file would hold counts as removed (the writer writes the reachable objects only).
    let live = reachable(&doc.objects, &trailer_roots(&doc.trailer));
    // Read before any edit: the root the catalog names and the elements the structure links name.
    let tree_root = doc.dict_of(root).and_then(|c| c.get("StructTreeRoot")).cloned();
    let mut links: BTreeSet<u32> = BTreeSet::new();
    for (i, e) in doc.objects.values().enumerate() {
        if i & 1023 == 0 {
            check_cancel(cancel)?;
        }
        link_targets(doc, &e.obj, 0, cap, &mut links);
    }
    let tree = structure_objects(doc, tree_root.as_ref(), &links, cancel)?;
    let mut removed = false;
    if let Some(e) = doc.objects.get_mut(&root)
        && let Object::Dict(c) = &mut e.obj
    {
        // R26-S4: the loss is a structure tree removed; `/MarkInfo` is removed too but does not count on its own.
        let had_tree = c.remove("StructTreeRoot").is_some_and(|v| !matches!(v, Object::Null));
        let had_mark = c.remove("MarkInfo").is_some();
        if had_tree || had_mark {
            touch(e);
        }
        removed |= had_tree;
    }
    for (i, (num, e)) in doc.objects.iter_mut().enumerate() {
        if i & 1023 == 0 {
            check_cancel(cancel)?;
        }
        if strip_links(&mut e.obj, 0, cap) {
            touch(e);
            removed |= live.contains(num);
        }
    }
    let mut nested: Vec<(u32, Object)> = Vec::new();
    for (i, (num, e)) in doc.objects.iter().enumerate() {
        if i & 1023 == 0 {
            check_cancel(cancel)?;
        }
        if !tree.contains(num) && nested_shape(doc, &tree, &e.obj, 0, cap) {
            let mut o = e.obj.clone();
            strip_nested_shapes(doc, &tree, &mut o, 0, cap);
            nested.push((*num, o));
        }
    }
    for (num, o) in nested {
        if let Some(e) = doc.objects.get_mut(&num) {
            e.obj = o;
            touch(e);
            removed |= live.contains(&num);
        }
    }
    if !tree.is_empty() {
        removed |= tree.iter().any(|n| live.contains(n));
        scrub_references(&mut doc.objects, &tree);
    }
    Ok(removed)
}

// ------------------------------------------------------------------------------- painted sources (F6)

/// Resource categories a content stream reaches only by naming an entry.
const GATED: [&str; 5] = ["XObject", "Pattern", "Shading", "ExtGState", "Font"];
/// The resource category of named marked-content property lists (`BDC`, `DP`; PLR-104): gated like [`GATED`] (an entry is
/// reached when kept content names it), purged by name (`purge_properties`), never pruned by object.
const PROPERTIES: &str = "Properties";
/// Keys of a property list whose text a viewer reads (alternate description, replacement text, expansion, title, language):
/// a property list of the old page that stays unnamed in the file with one of them is counted (PLR-104, fail closed).
const PROPERTY_TEXT_KEYS: [&str; 5] = ["Alt", "ActualText", "E", "T", "Lang"];

/// A resource dictionary of a scope's chain, and whether every container on the way to it is a dictionary object (R18):
/// the page or page-tree node that holds the `/Resources` entry (with the nodes between them) and the `/Resources`
/// value. A stream is read as a dictionary too, so what a name resolves to through it is still reached and not pruned,
/// but it is not `plain`: a name resolved through it is not silent (test
/// `painted_source_named_through_a_stream_used_as_a_dictionary_is_reported`).
#[derive(Clone, Copy)]
struct ResDict<'a> {
    dict: &'a Dict,
    plain: bool,
}

/// One scope: content streams and the resource dictionaries their names resolve in (nearest first).
struct Scope<'a> {
    streams: Vec<u32>,
    chain: Vec<ResDict<'a>>,
    /// The own content of a kept page whose content streams no other reference of the file names (`named_once`, R19):
    /// the one scope in which a `Do` can be silent (module docs, Silent). The content of any other kept page is read
    /// (so what it names is reached) but with this flag false.
    page: bool,
}

struct Usage<'a> {
    objs: &'a Objs,
    limits: &'a Limits,
    /// Pages and page-tree nodes: their `/Resources` is read by page scopes, not followed.
    page_holders: &'a BTreeSet<u32>,
    reached: BTreeSet<u32>,
    /// The image XObjects a kept page's own content draws with `Do` directly (module docs, Silent): the only painted
    /// sources (image, form, pattern, shading) that stay in the file without being reported. A font or a graphics
    /// state that stays is not a painted source and is not reported at all (R19; test
    /// `a_font_or_graphics_state_that_stays_is_not_a_painted_source`).
    silent: BTreeSet<u32>,
    traversed: BTreeSet<u32>,
    work: Vec<u32>,
    scopes: Vec<Scope<'a>>,
    scoped: BTreeSet<u32>,
    /// Resource dictionaries whose plain (not gated) entries were followed.
    plain_done: BTreeSet<usize>,
    budget: usize,
    steps: usize,
    incomplete: bool,
    /// PLR-104: the names each `/Properties` dictionary (by address) has used by a `BDC` or `DP` of the content read (every
    /// entry of that name in the scope's chain, the shadowed ones included: kept, not purged).
    props_used: BTreeMap<usize, BTreeSet<Vec<u8>>>,
    /// `/Properties` dictionaries of a scope whose content could not be read: every entry counts as used and the usage is
    /// undetermined (fail closed, `purge_properties`).
    props_undetermined: BTreeSet<usize>,
    /// Objects a used `/Properties` entry names (the property lists kept content invokes).
    props_named: BTreeSet<u32>,
}

fn ptr(d: &Dict) -> usize {
    d as *const Dict as usize
}

/// The `/SMask` `/G` transparency group of a graphics-state dictionary (read like a form: what it draws is reached).
fn smask_group(objs: &Objs, gs: &Dict) -> Option<u32> {
    gs.get("SMask").map(|m| res(objs, m)).and_then(Object::as_dict).and_then(|m| m.get("G")).and_then(Object::as_ref).map(|g| g.num)
}

/// Object `n` itself is an image XObject stream without an `/OC` (optional content is not evaluated, it counts as
/// possibly hidden): the one kind of object a kept page's `Do` makes silent (module docs, Silent; test
/// `painted_source_under_optional_content_is_reported`).
fn plain_image(objs: &Objs, n: u32) -> bool {
    let Some(Object::Stream(s)) = objs.get(&n).map(|e| &e.obj) else { return false };
    s.dict.get("OC").is_none() && s.dict.get("Subtype").map(|t| res(objs, t)).is_some_and(|t| t.is_name("Image"))
}

/// R19: the members of `wanted` (object numbers) that exactly one reference in the whole file names, counting every
/// object (stream dictionaries and direct containers included) and the trailer. A kept page's own content is read for
/// silence only when each of its content streams is one of these: a stream that its `/Contents` lists twice, or that
/// anything else refers to (a form, a pattern, a soft-mask group, a Type 3 glyph procedure, another page), is read in
/// more than one place, so the marked-content state `read_scope` carries from stream to stream of the page could be
/// wrong (a stream already read in the scope is skipped there). Over-reporting is accepted: a stream two pages share is
/// not exclusive either (test `painted_source_is_reported_when_a_content_stream_of_the_page_is_named_elsewhere`).
fn named_once(objs: &Objs, trailer: &Dict, wanted: &BTreeSet<u32>, cancel: &CancellationToken) -> Result<BTreeSet<u32>> {
    if wanted.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut count: BTreeMap<u32, usize> = BTreeMap::new();
    let mut refs: Vec<u32> = Vec::new();
    for (i, o) in objs.values().map(|e| &e.obj).chain(trailer.entries.iter().map(|(_, v)| v)).enumerate() {
        if i & 1023 == 0 {
            check_cancel(cancel)?;
        }
        refs.clear();
        refs_in(o, &mut refs);
        for n in refs.iter().filter(|n| wanted.contains(n)) {
            let c = count.entry(*n).or_insert(0);
            *c = c.saturating_add(1);
        }
    }
    Ok(count.into_iter().filter(|(_, c)| *c == 1).map(|(n, _)| n).collect())
}

impl<'a> Usage<'a> {
    fn mark(&mut self, n: u32) {
        self.reached.insert(n);
    }

    fn push(&mut self, n: u32) {
        self.mark(n);
        self.work.push(n);
    }

    /// Follows every reference inside `o` as a PLAIN reference: the objects are reached (so they are not
    /// pruned) and not silent (R16: only a direct draw is, module docs).
    fn follow(&mut self, o: &'a Object) {
        let mut stack: Vec<(&'a Object, usize)> = vec![(o, 0)];
        while let Some((o, depth)) = stack.pop() {
            if depth > 256 {
                continue;
            }
            let next = depth.saturating_add(1);
            match o {
                Object::Ref(r) => self.push(r.num),
                Object::Array(a) => stack.extend(a.iter().map(|x| (x, next))),
                Object::Dict(d) => stack.extend(d.entries.iter().map(|(_, v)| (v, next))),
                Object::Stream(s) => stack.extend(s.dict.entries.iter().map(|(_, v)| (v, next))),
                _ => {}
            }
        }
    }

    /// A resource dictionary of a scope: marks it (and its gated sub-dictionaries, `/Properties` included, PLR-104)
    /// reached and follows its plain entries (`/ColorSpace`, ...) as plain references. Reached, not silent: a resource dictionary and
    /// a gated sub-dictionary are reached by position, not drawn, whatever object a document places there. The result
    /// says whether the `/Resources` value is a dictionary object (R18).
    fn resources(&mut self, holder_value: &'a Object) -> Option<ResDict<'a>> {
        if let Some(r) = holder_value.as_ref() {
            self.mark(r.num);
        }
        let value = res(self.objs, holder_value);
        let d = value.as_dict()?;
        if self.plain_done.insert(ptr(d)) {
            for (k, v) in &d.entries {
                // PLR-104: `/Properties` is gated too: an entry is reached when kept content names it (`BDC`, `DP`).
                if GATED.iter().chain([&PROPERTIES]).any(|g| g.as_bytes() == k.as_slice()) {
                    if let Some(r) = v.as_ref() {
                        self.mark(r.num);
                    }
                } else {
                    self.follow(v);
                }
            }
        }
        Some(ResDict { dict: d, plain: matches!(value, Object::Dict(_)) })
    }

    /// Plain traversal of one object (module docs: scope holders' `/Resources` are not followed). Every key is a
    /// plain reference. A stream (and a Type 3 font) with its own `/Resources` is a scope of its own, read for what
    /// it names (so that is reached), not a page scope: nothing it draws is silent.
    fn traverse(&mut self, n: u32) {
        if !self.traversed.insert(n) {
            return;
        }
        let objs = self.objs;
        let Some(e) = objs.get(&n) else { return };
        let Some(d) = e.obj.as_dict() else {
            self.follow(&e.obj);
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
            self.follow(v);
        }
        if own_scope && let Some(rv) = d.get("Resources") {
            let Some(rd) = self.resources(rv) else { return };
            let streams: Vec<u32> = if type3 {
                d.get("CharProcs").map(|c| res(objs, c)).and_then(Object::as_dict).map(|p| p.entries.iter().filter_map(|(_, v)| v.as_ref()).map(|r| r.num).collect()).unwrap_or_default()
            } else {
                vec![n]
            };
            if self.scoped.insert(n) {
                self.scopes.push(Scope { streams, chain: vec![rd], page: false });
            }
        }
    }

    /// Follows every gated entry of a scope's dictionaries (its content could not be read). Every `/Properties` entry
    /// counts as used (kept) and its dictionary as undetermined (PLR-104, fail closed).
    fn all_used(&mut self, chain: &[ResDict<'a>]) {
        for r in chain {
            for g in GATED.iter().chain([&PROPERTIES]) {
                if let Some(sub) = r.dict.get(g).map(|s| res(self.objs, s)).and_then(Object::as_dict) {
                    for (k, v) in &sub.entries {
                        self.follow(v);
                        if *g == PROPERTIES {
                            self.props_used.entry(ptr(sub)).or_default().insert(k.clone());
                            self.props_undetermined.insert(ptr(sub));
                        }
                    }
                }
            }
        }
    }

    /// PLR-104: a `BDC` or `DP` names property list `name`: every entry of that name in the `/Properties` dictionaries of
    /// the scope's chain is used (kept) and reached.
    fn use_property(&mut self, chain: &[ResDict<'a>], name: &[u8]) {
        for r in chain {
            let Some(sub) = r.dict.get(PROPERTIES).map(|s| res(self.objs, s)).and_then(Object::as_dict) else { continue };
            let Some(target) = sub.get_key(name) else { continue };
            self.props_used.entry(ptr(sub)).or_default().insert(name.to_vec());
            if let Some(t) = target.as_ref() {
                self.props_named.insert(t.num);
            }
            self.follow(target);
        }
    }

    /// The entries `name` of category `cat` in the scope's dictionaries, nearest first: the first one is what a
    /// viewer resolves the name to, the others are shadowed by it (reached, not drawn). Each comes with whether it
    /// was found through dictionary objects only (the chain element and the category value, R18).
    fn lookup(&self, chain: &[ResDict<'a>], cat: &str, name: &[u8]) -> Vec<(&'a Object, bool)> {
        chain
            .iter()
            .filter_map(|r| {
                let sub = res(self.objs, r.dict.get(cat)?);
                Some((sub.as_dict()?.get_key(name)?, r.plain && matches!(sub, Object::Dict(_))))
            })
            .collect()
    }

    /// Reads one scope (module docs). `Err(())`: its content (or that of something it draws in the same
    /// scope) could not be read. `page`: the streams are the own content of a kept page (the only scope in which a
    /// `Do` can be silent); what a form, soft-mask group or Type 3 font without `/Resources` draws is read in the
    /// caller's chain and is not page content.
    fn read_scope(&mut self, streams: &[u32], chain: &[ResDict<'a>], page: bool, depth: usize, seen: &mut BTreeSet<u32>) -> std::result::Result<(), ()> {
        if depth > 16 {
            return Err(());
        }
        let objs = self.objs;
        // R17: the open marked-content sequences of this scope (BMC and BDC push, EMC pops), true for one opened by BDC
        // with the tag /OC (optional content). It carries over from one stream of the scope to the next, as a viewer
        // joins them.
        let mut marked: Vec<bool> = Vec::new();
        for num in streams {
            // A stream already read in this scope is not read again. A page scope with `page` true never meets one:
            // each of its streams is named by exactly one reference (`named_once`, R19), so it is listed once and no
            // nested read can have consumed it first.
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
            let cs = |n: &[u8]| chain.iter().find_map(|r| resource_components(objs, Some(r.dict), n));
            let parsed = parse_strict(&data, self.limits, &cs, &mut self.budget).map_err(|_| ())?;
            for op in &parsed.ops {
                match op.name.as_slice() {
                    b"BMC" => marked.push(false),
                    b"BDC" => marked.push(op.operands.first().and_then(Object::as_name) == Some(b"OC".as_slice())),
                    b"EMC" => {
                        marked.pop();
                    }
                    _ => {}
                }
                // PLR-104: a named property list (`/Tag /Name BDC`, `/Tag /Name DP`); an inline one is part of the content.
                if matches!(op.name.as_slice(), b"BDC" | b"DP")
                    && let Some(name) = op.operands.get(1).and_then(Object::as_name)
                {
                    self.use_property(chain, name);
                }
                let (cat, name) = match op.name.as_slice() {
                    b"Do" => ("XObject", op.operands.first()),
                    b"sh" => ("Shading", op.operands.first()),
                    b"gs" => ("ExtGState", op.operands.first()),
                    b"Tf" => ("Font", op.operands.first()),
                    b"scn" | b"SCN" => ("Pattern", op.operands.last()),
                    _ => continue,
                };
                let Some(name) = name.and_then(Object::as_name) else { continue };
                for (nearest, (target, plain)) in self.lookup(chain, cat, name).into_iter().enumerate() {
                    // R18: every named entry is reached (a plain reference). The one draw that is silent is `Do` of an
                    // image XObject by the own content of a kept page (module docs, Silent): the entry the name
                    // resolves to (the nearest; an entry it shadows is only reached), found through dictionary objects
                    // only, not optional content, inside no optional-content marked content (not evaluated: possibly
                    // hidden). Every other draw, nested content included, is reached only.
                    self.follow(target);
                    if page
                        && cat == "XObject"
                        && nearest == 0
                        && plain
                        && !marked.contains(&true)
                        && let Some(r) = target.as_ref()
                        && plain_image(objs, r.num)
                    {
                        self.silent.insert(r.num);
                    }
                    // Used in this scope: what has no resources of its own names in this scope.
                    let Some(d) = res(objs, target).as_dict() else { continue };
                    let inner: Vec<u32> = match cat {
                        "XObject" | "Pattern" if d.get("Resources").is_none() => target.as_ref().map(|r| vec![r.num]).unwrap_or_default(),
                        "ExtGState" => smask_group(objs, d)
                            .filter(|g| objs.get(g).and_then(|e| e.obj.as_dict()).is_some_and(|gd| gd.get("Resources").is_none()))
                            .map(|g| vec![g])
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
                        self.read_scope(&inner, chain, false, depth.saturating_add(1), seen)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn drain(&mut self, cancel: &CancellationToken) -> Result<()> {
        loop {
            if let Some(n) = self.work.pop() {
                self.steps = self.steps.saturating_add(1);
                if self.steps & 1023 == 0 {
                    check_cancel(cancel)?;
                }
                self.traverse(n);
                continue;
            }
            let Some(scope) = self.scopes.pop() else { return Ok(()) };
            let mut seen = BTreeSet::new();
            if self.read_scope(&scope.streams, &scope.chain, scope.page, 0, &mut seen).is_err() {
                self.all_used(&scope.chain);
            }
        }
    }
}

/// Drops the flattened pages' sources that no kept content names and no kept object references (module docs). Returns the painted sources
/// (images, forms, patterns, shadings; fonts and graphics states are not painted sources) that stay in the file without
/// being silent (module docs, Silent: an image the own content of a kept page draws with `Do` directly, the page's
/// content streams being named by one reference each): some usage is not determined, a kept object only references
/// them or places them where a viewer draws nothing from them, or a viewer draws them through anything else than that
/// direct `Do`: over-reporting is accepted (R14-P1, R16, R17, R18).
pub(crate) fn drop_painted_sources(doc: &mut PdfDoc, flats: &[Flat], cancel: &CancellationToken) -> Result<Purged> {
    let sources: BTreeSet<u32> = flats.iter().flat_map(|f| f.sources.iter().copied()).collect();
    if sources.is_empty() {
        return Ok(Purged::default());
    }
    let (reached, silent, incomplete, plan) = {
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
            silent: BTreeSet::new(),
            traversed: BTreeSet::new(),
            work: Vec::new(),
            scopes: Vec::new(),
            scoped: BTreeSet::new(),
            plain_done: BTreeSet::new(),
            budget: doc.limits.max_decoded_total,
            steps: 0,
            incomplete: false,
            props_used: BTreeMap::new(),
            props_undetermined: BTreeSet::new(),
            props_named: BTreeSet::new(),
        };
        // R14-P1: the trailer roots are followed as plain references only (reached, not silent): a painted
        // source that nothing draws but that a kept catalog structure merely points at (`/AF`, a `/Names` or
        // `/Dests` tree, `/OutputIntents`, ...) stays in the file, so it is reported, not silently kept. The
        // annotations a kept page lists, their appearance streams included, are reached through the page the same way.
        for r in trailer_roots(&doc.trailer) {
            u.push(r);
        }
        // Page scopes: own /Resources and every ancestor's. The page's own content is the one thing that can make a source
        // silent (R18); the entries of its /Resources outside the gated categories are plain references
        // (`Usage::resources`). A chain element is `plain` when the page, every node up to it and its `/Resources` value
        // are dictionary objects (R18: a stream there is read, so its names are reached, but nothing found through it
        // is silent).
        // R19: a page's content is page content for silence only when each of its content streams is named by exactly one
        // reference of the file (`named_once`); otherwise it is still read, but nothing it draws is silent.
        let contents: Vec<Vec<u32>> = doc.pages.iter().map(|p| objs.get(p).and_then(|e| e.obj.as_dict()).and_then(|d| crate::show_text::content_streams_of(objs, d)).unwrap_or_default()).collect();
        let wanted: BTreeSet<u32> = contents.iter().flatten().copied().collect();
        let once = named_once(objs, &doc.trailer, &wanted, cancel)?;
        for (p, streams) in doc.pages.iter().zip(contents) {
            let Some(pe) = objs.get(p) else { continue };
            let Some(pd) = pe.obj.as_dict() else { continue };
            let mut chain: Vec<ResDict> = Vec::new();
            let mut node = Some((pd, matches!(pe.obj, Object::Dict(_))));
            for _ in 0..=doc.limits.depth_cap() {
                let Some((d, nodes_plain)) = node else { break };
                if let Some(rv) = d.get("Resources")
                    && let Some(rd) = u.resources(rv)
                {
                    chain.push(ResDict { dict: rd.dict, plain: rd.plain && nodes_plain });
                }
                node = d.get("Parent").map(|x| res(objs, x)).and_then(|x| x.as_dict().map(|d| (d, nodes_plain && matches!(x, Object::Dict(_)))));
            }
            let page = streams.iter().all(|n| once.contains(n));
            u.scopes.push(Scope { streams, chain, page });
        }
        u.drain(cancel)?;
        let plan = plan_properties(objs, flats, &sources, &u);
        (u.reached, u.silent, u.incomplete, plan)
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
        // reported, and a property list the old page reached is undetermined (PLR-104, fail closed).
        let live = reachable(&doc.objects, &trailer_roots(&doc.trailer));
        let property_residual = u32::from(plan.lists > 0);
        return Ok(Purged { painted_kept: painted.iter().filter(|n| live.contains(n)).copied().collect(), property_residual });
    }
    let property_residual = apply_properties(doc, &plan);
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
    // PLR-104: a property list of the old page whose entry was purged (or could not be) that stays in the file, no kept
    // content names, and that holds a text key is counted (fail closed).
    let kept_text = plan
        .candidates
        .iter()
        .filter(|m| live.contains(m) && !plan.named.contains(m) && doc.objects.get(m).is_some_and(|e| property_text(&doc.objects, &e.obj)))
        .count();
    let property_residual = property_residual.saturating_add(u32::try_from(kept_text).unwrap_or(u32::MAX));
    Ok(Purged { painted_kept: painted.iter().filter(|n| live.contains(n) && !silent.contains(n)).copied().collect(), property_residual })
}

/// What [`drop_painted_sources`] leaves: the painted sources that stay without being silent (reported,
/// `NotProven::PaintedSourceKept`) and the property-list count of PLR-104 (`residual_in_region`).
#[derive(Default)]
pub(crate) struct Purged {
    pub painted_kept: Vec<u32>,
    /// PLR-104 (fail closed): property-list dictionaries the old page reached whose usage could not be determined, which the
    /// purge cannot edit and that hold an unnamed entry with a text key, and property lists of the old page left in the file
    /// unnamed with a text key (`plan_properties`, `apply_properties`).
    pub property_residual: u32,
}

/// Where a resource dictionary the old page reached lives (PLR-104).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ResAt {
    /// Object `n` is the resource dictionary (a dictionary object).
    Object(u32),
    /// The `/Resources` value written inside object `n` (a page, a stream or a Type 3 font) is the resource dictionary.
    In(u32),
    /// The `/Resources` the old page read was written inside the page dictionary (replaced by the flatten) or inside the `/Pages`
    /// node it inherited it from (emptied by `detach_from_page_tree`, R26-P1): only a `/Properties` object it names is still in
    /// the file (`old_resources` of the flattened page `n`).
    Old(u32),
}

/// Where a `/Properties` dictionary the purge edits lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum PropsAt {
    /// Object `n` is the `/Properties` dictionary (a dictionary object).
    Object(u32),
    /// Written inside the resource dictionary at the `ResAt`.
    Inline(ResAt),
}

/// The property-list purge (PLR-104), planned on the analysed document, applied after.
#[derive(Default)]
struct PropertyPlan {
    /// Entries to remove: no `BDC` or `DP` of the content read names them.
    remove: Vec<(PropsAt, BTreeSet<Vec<u8>>)>,
    /// Counted before the edit: undetermined usage, or a dictionary the purge cannot edit with an unnamed entry with text.
    counted: u32,
    /// `/Properties` entries in the reached dictionaries (non-zero: a property list of the old page is still listed).
    lists: usize,
    /// Objects named by removed or unedited entries (checked after the edit: `property_text`).
    candidates: BTreeSet<u32>,
    /// Objects a used entry names.
    named: BTreeSet<u32>,
}

/// `o` (resolved) is a dictionary that holds one of [`PROPERTY_TEXT_KEYS`].
fn property_text(objs: &Objs, o: &Object) -> bool {
    res(objs, o).as_dict().is_some_and(|d| PROPERTY_TEXT_KEYS.iter().any(|k| holds(d, k)))
}

/// The `/Resources` value the old page read: its own, else the one it inherited from the nearest node of its `/Parent` chain
/// that has one (`Flat::inherited_resources`, R26-P1), as written there.
fn old_resources(f: &Flat) -> Option<&Object> {
    f.old.get("Resources").or(f.inherited_resources.as_ref())
}

/// The resource dictionaries the old pages reached and where they live now (PLR-104): the `/Resources` object an old page read
/// (its own or, R26-P1, the one it inherited from a `/Pages` node; a dictionary another page may share, also as its own
/// `/Resources`), the `/Properties` object named by the `/Resources` dictionary an old page read when that dictionary was written
/// inside the page or the node (`ResAt::Old`), the `/Resources` a kept page received from a node of a flattened page's `/Parent`
/// chain (`Flat::heirs`), and the `/Resources` of every stream and Type 3 font the old page reached (`Flat::sources`).
fn reached_resources(objs: &Objs, flats: &[Flat], sources: &BTreeSet<u32>) -> BTreeSet<ResAt> {
    let mut out = BTreeSet::new();
    let at = |holder: u32, v: &Object| match v {
        Object::Ref(r) => Some(ResAt::Object(r.num)),
        Object::Dict(_) => Some(ResAt::In(holder)),
        _ => None,
    };
    for f in flats {
        match old_resources(f) {
            Some(Object::Ref(r)) => {
                out.insert(ResAt::Object(r.num));
            }
            Some(Object::Dict(_)) => {
                out.insert(ResAt::Old(f.page.object));
            }
            _ => {}
        }
        for p in &f.heirs {
            if let Some(a) = objs.get(p).and_then(|e| e.obj.as_dict()).and_then(|d| d.get("Resources")).and_then(|v| at(*p, v)) {
                out.insert(a);
            }
        }
    }
    for n in sources {
        let Some(e) = objs.get(n) else { continue };
        let Some(d) = e.obj.as_dict() else { continue };
        let type3 = d.get("Subtype").map(|t| res(objs, t)).is_some_and(|t| t.is_name("Type3"));
        if (matches!(e.obj, Object::Stream(_)) || type3)
            && let Some(a) = d.get("Resources").and_then(|v| at(*n, v))
        {
            out.insert(a);
        }
    }
    out
}

/// Plans the property-list purge (PLR-104) on the analysed document: in every `/Properties` dictionary the old pages reached,
/// the entries no `BDC` or `DP` of the content read names are removed (inline dictionaries and references alike); a dictionary
/// whose usage could not be determined keeps every entry and is counted; one the purge cannot edit (a `/Resources` or
/// `/Properties` value that is a stream, or reached through an alias object) is counted when an entry no content names holds a
/// text key.
fn plan_properties(objs: &Objs, flats: &[Flat], sources: &BTreeSet<u32>, u: &Usage<'_>) -> PropertyPlan {
    let mut plan = PropertyPlan { named: u.props_named.clone(), ..PropertyPlan::default() };
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    let none = BTreeSet::new();
    for at in reached_resources(objs, flats, sources) {
        // The resource dictionary, and whether the purge can edit it where it lives.
        let (rd, editable) = match at {
            ResAt::Object(n) => match objs.get(&n).map(|e| &e.obj) {
                Some(Object::Dict(d)) => (Some(d), true),
                Some(o) => (res(objs, o).as_dict(), false),
                None => (None, false),
            },
            ResAt::In(n) => match objs.get(&n).and_then(|e| e.obj.as_dict()).and_then(|d| d.get("Resources")) {
                Some(Object::Dict(d)) => (Some(d), true),
                _ => (None, false),
            },
            // Not in the file any more: only a `/Properties` value that is a reference is read (and edited) below.
            ResAt::Old(page) => match flats.iter().find(|f| f.page.object == page).and_then(old_resources) {
                Some(Object::Dict(d)) => (Some(d), false),
                _ => (None, false),
            },
        };
        let Some(rd) = rd else { continue };
        let Some(pv) = rd.get(PROPERTIES) else { continue };
        if matches!(at, ResAt::Old(_)) && !matches!(pv, Object::Ref(_)) {
            // Written inside the replaced page dictionary: it left the file with the page.
            continue;
        }
        let (props, place) = match pv {
            Object::Dict(p) if editable => (Some(p), Some(PropsAt::Inline(at))),
            Object::Ref(r) => match objs.get(&r.num).map(|e| &e.obj) {
                Some(Object::Dict(p)) => (Some(p), Some(PropsAt::Object(r.num))),
                Some(o) => (res(objs, o).as_dict(), None),
                None => (None, None),
            },
            o => (res(objs, o).as_dict(), None),
        };
        let Some(props) = props else { continue };
        if !seen.insert(ptr(props)) {
            continue;
        }
        plan.lists = plan.lists.saturating_add(props.entries.len());
        let used = u.props_used.get(&ptr(props)).unwrap_or(&none);
        if u.props_undetermined.contains(&ptr(props)) && !props.entries.is_empty() {
            plan.counted = plan.counted.saturating_add(1);
        }
        let unnamed: Vec<&(Vec<u8>, Object)> = props.entries.iter().filter(|(k, _)| !used.contains(k)).collect();
        plan.candidates.extend(unnamed.iter().filter_map(|(_, v)| v.as_ref()).map(|r| r.num));
        match place {
            Some(place) if !unnamed.is_empty() => plan.remove.push((place, unnamed.iter().map(|(k, _)| k.clone()).collect())),
            Some(_) => {}
            None => {
                if unnamed.iter().any(|(_, v)| property_text(objs, v)) {
                    plan.counted = plan.counted.saturating_add(1);
                }
            }
        }
    }
    plan
}

/// Applies the property-list purge (PLR-104). Returns the count planned before the edit (`PropertyPlan::counted`).
fn apply_properties(doc: &mut PdfDoc, plan: &PropertyPlan) -> u32 {
    for (place, names) in &plan.remove {
        let (num, inline) = match place {
            PropsAt::Object(n) => (*n, None),
            PropsAt::Inline(ResAt::Object(n)) => (*n, Some(false)),
            PropsAt::Inline(ResAt::In(n)) => (*n, Some(true)),
            PropsAt::Inline(ResAt::Old(_)) => continue,
        };
        let Some(e) = doc.objects.get_mut(&num) else { continue };
        let holder: Option<&mut Dict> = match &mut e.obj {
            Object::Dict(d) => Some(d),
            Object::Stream(s) => Some(&mut s.dict),
            _ => None,
        };
        let props: Option<&mut Dict> = match (holder, inline) {
            (Some(d), None) => Some(d),
            (Some(d), Some(false)) => match d.get_mut(PROPERTIES) {
                Some(Object::Dict(p)) => Some(p),
                _ => None,
            },
            (Some(d), Some(true)) => match d.get_mut("Resources") {
                Some(Object::Dict(r)) => match r.get_mut(PROPERTIES) {
                    Some(Object::Dict(p)) => Some(p),
                    _ => None,
                },
                _ => None,
            },
            (None, _) => None,
        };
        let Some(props) = props else { continue };
        let before = props.entries.len();
        props.entries.retain(|(k, _)| !names.contains(k));
        if props.entries.len() != before {
            touch(e);
        }
    }
    plan.counted
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

/// The saved document names a structure tree: the catalog holds a `/StructTreeRoot` that is not null, directly or through a
/// chain of references. A null value, a reference to an object the file does not hold and a chain that ends in either read as
/// no entry (the null object); a chain that does not end within `MAX_REF_HOPS` hops, a catalog the file does not yield and any
/// other value count as a tree (fail closed).
fn catalog_names_structure_tree(saved: &PdfDoc) -> bool {
    let Ok(catalog) = saved.catalog() else { return true };
    let Some(mut o) = catalog.get("StructTreeRoot") else { return false };
    for _ in 0..=MAX_REF_HOPS {
        match o {
            Object::Null => return false,
            Object::Ref(r) => match saved.objects.get(&r.num) {
                Some(e) => o = &e.obj,
                None => return false,
            },
            _ => return true,
        }
    }
    true
}

/// `v` is a reference chain that `PdfDoc::resolve` reads as null because of its hop bound (`MAX_REF_HOPS` dereferences without
/// reaching a direct value), not because it ends in a null or a missing object (PLR-105, R25-P3).
fn past_hop_bound(saved: &PdfDoc, v: &Object) -> bool {
    let mut o = v;
    for _ in 0..MAX_REF_HOPS {
        match o {
            Object::Ref(r) => match saved.objects.get(&r.num) {
                Some(e) => o = &e.obj,
                None => return false,
            },
            _ => return false,
        }
    }
    true
}

/// `d` holds `key` with a value other than the null object (a null value is an absent entry).
fn holds(d: &Dict, key: &str) -> bool {
    d.get(key).is_some_and(|v| !matches!(v, Object::Null))
}

/// `d` has the shape of a structure element, of a structure tree root, or of an outline item that names a structure element
/// (PLR-102, lead decision R24-P2: structure text survives in a file whose catalog names no tree). Read from the PARSED
/// dictionary (names decoded; comments and strings are not read as keys), a value that is a reference is followed (`resolve`, at most
/// `MAX_REF_HOPS` hops): `/Type` is the name `StructElem` or `StructTreeRoot`; or `/S` is a name and `/P` is held together with
/// any key of `STRUCT_TEXT_KEYS` (a dictionary with only `/S`, as an action or a border style has, is not one); or `/SE` is held
/// (the outline item entry that names a structure element).
fn structure_shaped(saved: &PdfDoc, d: &Dict) -> bool {
    let named = |key: &str, name: &str| d.get(key).map(|v| saved.resolve(v)).is_some_and(|v| v.is_name(name));
    // PLR-105 (R25-P3, fail closed): a `/Type` or `/S` the reader's hop bound reads as null counts, as the catalog rule does.
    let beyond = |key: &str| d.get(key).is_some_and(|v| past_hop_bound(saved, v));
    beyond("Type")
        || beyond("S")
        || named("Type", "StructElem")
        || named("Type", "StructTreeRoot")
        || (d.get("S").map(|v| saved.resolve(v)).is_some_and(|v| matches!(v, Object::Name(_)))
            && holds(d, "P")
            && STRUCT_TEXT_KEYS.iter().any(|k| holds(d, k)))
        || holds(d, "SE")
}

/// What `check_document` has found so far. `structure` is the one count of the structure rules (tree, `/StructParents`,
/// structure-element shape); `bad` is every other mismatch.
struct Scan<'a> {
    saved: &'a PdfDoc,
    flat_pages: &'a BTreeSet<u32>,
    /// The structure rules apply: the save flattened at least one region (PLR-101).
    structure_rules: bool,
    depth_cap: usize,
    bad: u32,
    structure: bool,
}

impl Scan<'_> {
    /// Reads `root` and everything nested in it, to `depth_cap`: a value deeper than that counts 1 (fail closed; a parsed file
    /// has none).
    fn value(&mut self, root: &Object) {
        let mut stack: Vec<(&Object, usize)> = vec![(root, 0)];
        while let Some((o, depth)) = stack.pop() {
            if depth > self.depth_cap {
                self.bad = self.bad.saturating_add(1);
                continue;
            }
            let next = depth.saturating_add(1);
            match o {
                Object::Array(a) => stack.extend(a.iter().map(|x| (x, next))),
                Object::Dict(_) | Object::Stream(_) => {
                    let Some(d) = o.as_dict() else { continue };
                    let on = |k: &str| d.get(k).and_then(Object::as_ref).is_some_and(|r| self.flat_pages.contains(&r.num));
                    if on("Pg") || (on("P") && d.get("Subtype").is_some()) {
                        self.bad = self.bad.saturating_add(1);
                    }
                    if self.structure_rules {
                        self.structure |= holds(d, "StructParents") || holds(d, "StructParent") || structure_shaped(self.saved, d);
                    }
                    stack.extend(d.entries.iter().map(|(_, v)| (v, next)));
                }
                _ => {}
            }
        }
    }

    /// Reads the members of an object stream the saved file holds as a stream (referenced by an object, so the writer kept it;
    /// no cross-reference entry names its members, so `saved.objects` does not hold them, K1 of the R24 pre-review). Decoded
    /// against the decode budget, parsed like the reader parses one (`N` and `First`, header pairs, members that overlap
    /// bounded by the decoded size, the node meter); a stream that cannot be decoded or parsed counts (fail closed).
    fn object_stream(&mut self, s: &Stream, budget: &mut usize, nodes_left: &mut usize) {
        let limits = &self.saved.limits;
        let Ok(data) = filters::decode_stream(s, limits, budget) else {
            self.structure = true;
            return;
        };
        let count = |key: &str| s.dict.get(key).and_then(Object::as_int).and_then(|v| usize::try_from(v).ok());
        let (Some(n), Some(first)) = (count("N"), count("First")) else {
            self.structure = true;
            return;
        };
        let mut header = Lexer::new(&data, 0, limits.max_depth);
        let mut parsed_bytes = 0usize;
        // PLR-105 (R25-P4): the bytes each member spans, to check that the members (and white space) cover the data after
        // `/First` and that only white space follows the header pairs.
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for _ in 0..n {
            // One header pair ("number offset") and its member at a time: nothing is collected, and the loop ends at the first
            // pair the data does not hold.
            let Ok(offset) = header.read_uint().and_then(|_| header.read_uint()) else {
                self.structure = true;
                return;
            };
            let at = usize::try_from(offset).ok().and_then(|o| first.checked_add(o)).filter(|a| *a < data.len());
            let Some(at) = at else {
                self.structure = true;
                return;
            };
            match parse_standalone(&data, at, limits.max_depth, nodes_left) {
                Ok((member, used)) => {
                    parsed_bytes = parsed_bytes.saturating_add(used);
                    if parsed_bytes > data.len() {
                        self.structure = true;
                        return;
                    }
                    spans.push((at, at.saturating_add(used)));
                    self.value(&member);
                }
                Err(_) => {
                    self.structure = true;
                    return;
                }
            }
        }
        // Bytes no reader reads as a member (R25-P4: `/N` smaller than the members the data holds, `/N 0` with data, bytes between
        // the header pairs and `/First`): they could hold anything, so they count (fail closed). White space and comments do not.
        let mut header_end = Lexer::new(&data, header.pos, limits.max_depth);
        header_end.skip_ws();
        if header_end.pos < first.min(data.len()) || header.pos > first {
            self.structure = true;
            return;
        }
        spans.sort_unstable();
        let mut pos = first;
        for (start, end) in spans {
            if start > pos {
                let mut gap = Lexer::new(&data, pos, limits.max_depth);
                gap.skip_ws();
                if gap.pos < start {
                    self.structure = true;
                    return;
                }
            }
            pos = pos.max(end);
        }
        let mut tail = Lexer::new(&data, pos, limits.max_depth);
        tail.skip_ws();
        if tail.pos < data.len() {
            self.structure = true;
        }
    }
}

/// Document-level check of the SAVED file (counted in `residual_in_region`): no `/XFA`, no `/NeedsRendering`, no annotation
/// whose `/P` and no structure item whose `/Pg` is a flattened page, and, when `flattens` (the save flattened at least one
/// region, PLR-101; a save with no region leaves the structure rules out and keeps the checks before them), no structure left
/// in the file: ONE count, however many of these hold and whatever the purge removed (R23 FIX_FIRST -> STRUCTURE TREE = NOT
/// PROVEN, lead decision 2026-10-12T08:41:40Z; PLR-102, lead decision R24-P2, fail closed): the catalog holds a
/// `/StructTreeRoot` (`catalog_names_structure_tree`), or any dictionary of the file holds a `/StructParents` or `/StructParent`
/// entry, or any dictionary has the shape of a structure element or of an outline item that names one (`structure_shaped`),
/// wherever it sits: reachable from the catalog under any key, or a member of an object stream the file holds as a stream.
/// Tests `tagged_documents_read_not_proven_whatever_the_tree_looks_like`,
/// `untagged_documents_and_stripped_trees_still_read_flattened`, `structure_tree_keys_are_read_as_names_not_text`,
/// `structure_parents_left_in_the_file_are_reported`, `detached_structure_elements_are_reported`,
/// `structure_rules_apply_only_when_a_region_is_flattened`. Every dictionary and stream dictionary of the file is read, to the
/// depth the parser admits (`Limits::depth_cap`); a value deeper than that counts 1 (fail closed; a parsed file has none).
/// PLR-105 (fail closed, test `unreadable_structure_shapes_are_counted`): a `/Type` or `/S` value that is a reference chain the
/// reader's hop bound reads as null counts as a shape; a stream with `/N` and `/First` is read as an object stream whatever its
/// `/Type`, and its bytes that are not white space, comments, the header pairs or members count; the saved trailer's values
/// (`/ID` is copied from the source) are read too.
pub(crate) fn check_document(saved: &PdfDoc, flat_pages: &BTreeSet<u32>, flattens: bool) -> u32 {
    let mut scan = Scan {
        saved,
        flat_pages,
        structure_rules: flattens,
        depth_cap: saved.limits.depth_cap(),
        bad: 0,
        structure: flattens && catalog_names_structure_tree(saved),
    };
    if let Ok(c) = saved.catalog() {
        if c.get("NeedsRendering").is_some() {
            scan.bad = scan.bad.saturating_add(1);
        }
        if c.get("AcroForm").map(|f| saved.resolve(f)).and_then(Object::as_dict).is_some_and(|f| f.get("XFA").is_some()) {
            scan.bad = scan.bad.saturating_add(1);
        }
    }
    let mut budget = saved.limits.max_decoded_total;
    let mut nodes_left = saved.limits.max_nodes;
    for e in saved.objects.values() {
        scan.value(&e.obj);
        // PLR-105 (R25-P4): a stream with `/N` and `/First` is read as an object stream whatever its `/Type` says.
        if flattens
            && let Object::Stream(s) = &e.obj
            && (s.dict.get("Type").map(|t| saved.resolve(t)).is_some_and(|t| t.is_name("ObjStm")) || (s.dict.get("N").is_some() && s.dict.get("First").is_some()))
        {
            scan.object_stream(s, &mut budget, &mut nodes_left);
        }
    }
    // PLR-105 (R25-P5): the saved trailer's values (the writer copies the source's `/ID` array as it is).
    for (_, v) in &saved.trailer.entries {
        scan.value(v);
    }
    scan.bad.saturating_add(u32::from(scan.structure))
}
