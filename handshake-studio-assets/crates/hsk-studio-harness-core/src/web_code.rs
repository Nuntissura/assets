//! Web code intelligence on the web host (lane SMB-WEB-CODE-1; [STU-WEB-011] to [STU-WEB-076]): the handlers of the
//! rows in `action::commands::web_code`, over the open web document and the engine modules `hsk_studio_web::{doc_types,
//! vocabulary, css_properties, hints, validation, formatting}` (their docs state the shipped data, the readings and what
//! is not done). A child of `web_commands`, so every edit is that host's source patch: one named step of the web
//! history with the caller's command context, saved with the document by `web.save` and back after `web.open`.
//!
//! Parameters: `key` is a node path or `#id` (as every `web.*` row); `offset`, `start`, `end` are byte offsets into the
//! source (`web.read include_source`), on character boundaries; `type_id` is a row of [STU-WEB-013] (default: the type
//! of the open file's name, else `html`). Results carry ranges as `[start, end]` and node keys; lists stop at 2000 rows
//! with a total.
//!
//! - `web.document_types` (`type_id`?, `file_name`?): the thirty-nine types (or one), the ten doctype declarations with
//!   their SHA-256, the closed kind and DTD-context sets, and for `file_name` its candidate types and served MIME type.
//! - `web.set_doctype` (`declaration_id`): replaces or inserts the declaration verbatim. Refusals `doctype_unknown`,
//!   `doctype_needs_xml_parser` (`xslt_1`), `nothing_to_change`.
//! - `web.tag_vocabulary` (`type_id`?, `tag`?): the libraries for the type, its tag names or one tag's definition
//!   (merged attributes, formatting rules, inspector), the twenty value types and the load diagnostics.
//!   `tag_unknown` when the tag does not resolve.
//! - `web.css_properties` (`property`?, `category`?): categories, control types, units, curated entries with options
//!   and numeric contracts, transition vocabulary, SVG web-export defaults, the gradient swatch constant, prefix rules.
//! - `web.media_records`: every `@media` block of the document's style elements as `{media_type, conditions, rules}`
//!   records, or the block's typed refusal.
//! - `web.css_rule` (`stylesheet` key of an HTML `style`, `media`?, `selector`, `declarations` object): sets
//!   (`"prop": "value"` or `{"number": n, "unit": "px"}`, composed through the property's contract) or removes
//!   (`"prop": null`) declarations of the rule, creating the media block and rule when absent; applied in key order; a
//!   prefix rule's emissions follow their property. Refusals `stylesheet_not_html_style`, `not_a_style_element`,
//!   `css_*`, `media_*`, `value_*`, `unit_*`, `css_property_absent`, `css_rule_absent`, `nothing_to_change`.
//! - `web.code_navigator` (`key`): the rules of the document's style sheets whose selector matches the element, and
//!   the count of rules outside the selector subset.
//! - `web.code_hints` (`offset`, `type_id`?): the hint group, pattern, typed prefix and matching items there.
//! - `web.regions` (`offset`?): the region-ownership receipts of the document (or the one holding `offset`).
//! - `web.validate` (`rule_set`? = `html_all`, `key`?, `type_id`?): typed diagnostics, never a rewrite.
//! - `web.lint` (`language` markup|css|script, `rules`? object of rule -> bool overrides, `key`?): diagnostics, the
//!   rules enabled and `not_run`; `script` refuses `script_lint_provider_unselected`, an unknown rule
//!   `lint_rule_unknown`.
//! - `web.format_source` (`key`?, `indent_char` space|tab, `indent_width` 1..16, `line_length` 0|20..1000,
//!   `attribute_wrap` keep|wrap_over_line_length, `dry_run`?): one step; `dry_run` returns the edits and changes
//!   nothing; already formatted text answers `changed: false` with no step (idempotent).
//! - `web.apply_comment` (`key` or `start`+`end`) and `web.remove_comment` (`offset`): comment syntax from the language
//!   there. Refusals `range_crosses_languages`, `comment_would_nest`, `range_splits_a_tag`, `already_commented`,
//!   `no_comment_here`.
//! - `web.balance_braces` (`start`, `end`?), `web.select_parent_tag` (`key`), `web.collapse_full_tag` (`key`),
//!   `web.collapse_selection` (`start`, `end`): ranges and keys only (fold state is the view's).
//! - `web.indent` / `web.outdent` (`key` or `start`+`end`, `indent_char`?, `indent_width`?): one step; lines in a
//!   verbatim element refuse `range_in_verbatim`.
//!
//! Every parse or apply failure of the engine refuses with the engine's code, and a refusal changes nothing.
#![deny(clippy::arithmetic_side_effects)]

use super::{MAX_ROWS, No, P, R, WebHost, bounded, key, key_text, no, patched, row_text};
use crate::action::CommandEntry;
use hsk_studio_accord::{CancellationToken, CommandContext};
use hsk_studio_web::css_properties::{self as css, Category, ControlType, DeclarationChange, NumericContract};
use hsk_studio_web::doc_types::{self, CodeError, DOCTYPES, DOCUMENT_TYPES, DocumentType, DtdContext, InternalKind};
use hsk_studio_web::formatting::{self, AttributeWrap, FormatOptions, IndentChar};
use hsk_studio_web::hints::{self, SchemeRegistry};
use hsk_studio_web::validation::{self, Diagnostic, Language, Report};
use hsk_studio_web::vocabulary::{TagRegistry, ValueType};
use hsk_studio_web::{NodeId, SourcePatch, TextEdit, WebDoc};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ops::Range;

/// The rows this module runs.
const COMMANDS: [&str; 20] = [
    "web.document_types",
    "web.set_doctype",
    "web.tag_vocabulary",
    "web.css_properties",
    "web.media_records",
    "web.css_rule",
    "web.code_navigator",
    "web.code_hints",
    "web.regions",
    "web.validate",
    "web.lint",
    "web.format_source",
    "web.apply_comment",
    "web.remove_comment",
    "web.balance_braces",
    "web.select_parent_tag",
    "web.collapse_full_tag",
    "web.collapse_selection",
    "web.indent",
    "web.outdent",
];

pub(super) fn owns(command_id: &str) -> bool {
    COMMANDS.contains(&command_id)
}

fn code(e: &CodeError) -> No {
    No(e.code, e.at.as_ref().map_or_else(String::new, |r| format!("{}..{}", r.start, r.end)), None)
}

fn span(r: &Range<usize>) -> Value {
    json!([r.start, r.end])
}

/// A byte offset parameter: a non-negative integer inside the source, on a character boundary.
fn offset(doc: &WebDoc, p: &P<'_>, name: &str) -> R<usize> {
    let v = p.0.get(name).ok_or(No("parameter_missing", name.to_string(), None))?;
    let n = v.as_u64().and_then(|n| usize::try_from(n).ok()).ok_or(No("parameter_invalid", name.to_string(), None))?;
    if n > doc.source().len() || !doc.source().is_char_boundary(n) {
        return no("offset_invalid", name);
    }
    Ok(n)
}

fn opt_offset(doc: &WebDoc, p: &P<'_>, name: &str) -> R<Option<usize>> {
    if p.0.get(name).is_none_or(Value::is_null) { Ok(None) } else { offset(doc, p, name).map(Some) }
}

fn node_of(doc: &WebDoc, text: &str) -> R<NodeId> {
    let k = key(doc, text)?;
    doc.resolve(&k).ok_or(No("no_such_node", bounded(text), None))
}

/// The range a selection-taking row works on: `key` (the node's span) or `start` and `end`.
fn selection(doc: &WebDoc, p: &P<'_>) -> R<Range<usize>> {
    match (p.opt_text("key")?, opt_offset(doc, p, "start")?, opt_offset(doc, p, "end")?) {
        (Some(k), None, None) => doc.node(node_of(doc, k)?).map(|n| n.span.clone()).ok_or(No("no_such_node", bounded(k), None)),
        (None, Some(s), Some(e)) if s <= e => Ok(s..e),
        (None, Some(_), Some(_)) => no("range_invalid", "start..end"),
        (None, _, _) => no("parameter_missing", "key"),
        (Some(_), _, _) => no("parameter_invalid", "key and start/end"),
    }
}

fn kind_json(k: InternalKind) -> &'static str {
    k.token()
}

fn type_json(t: &DocumentType) -> Value {
    json!({
        "type_id": t.type_id, "title": t.title, "internal_kind": kind_json(t.internal_kind), "windows_extensions": t.windows_extensions, "mac_extensions": t.mac_extensions,
        "mime_type": t.mime_type, "writes_byte_order_mark": t.writes_byte_order_mark, "dtd_context": t.dtd_context.map(DtdContext::token), "server_model_id": t.server_model_id,
        "starter_document_id": t.starter_document_id, "template": t.is_template(),
    })
}

fn contract_json(c: &NumericContract) -> Value {
    let b = |x: &css::Bound| json!({"value": x.value, "source": x.source.token()});
    json!({"hard_min": b(&c.hard_min), "hard_max": b(&c.hard_max), "soft_min": b(&c.soft_min), "soft_max": b(&c.soft_max), "default": c.default.token(), "unit": c.unit, "precision": c.precision, "step": c.step, "coarse_step": c.coarse_step, "fine_step": c.fine_step})
}

fn entry_json(e: &css::PropertyEntry) -> Value {
    json!({
        "property": e.property, "display_name": e.display_name(), "category": e.category.id(), "control_type": e.control_type.token(), "default_value": e.default_value.token(),
        "supports_negative_values": e.supports_negative_values, "option_count": e.option_count(),
        "options": e.option_list().iter().map(|o| o.map_or_else(|| json!({"separator": true}), |v| json!(v))).collect::<Vec<_>>(),
        "contract": css::numeric_contract(e).as_ref().map(contract_json),
    })
}

fn diagnostics_json(report: &Report) -> Value {
    let d = |x: &Diagnostic| json!({"severity": x.severity.token(), "rule_set_id": x.rule_set_id, "rule_id": x.rule_id, "byte_range": span(&x.byte_range), "element_path": x.element_path, "message": row_text(&x.message), "code": x.code});
    json!({"diagnostics": report.diagnostics.iter().take(MAX_ROWS).map(d).collect::<Vec<_>>(), "total": report.diagnostics.len(), "truncated": report.truncated, "not_run": report.not_run})
}

fn edits_json(edits: &[TextEdit]) -> Value {
    json!(edits.iter().take(MAX_ROWS).map(|e| json!({"span": span(&e.span), "replacement": row_text(&e.replacement)})).collect::<Vec<_>>())
}

fn indent_unit(p: &P<'_>) -> R<String> {
    let width = match p.0.get("indent_width") {
        None | Some(Value::Null) => 2,
        Some(v) => v.as_u64().filter(|n| (1..=16).contains(n)).ok_or(No("parameter_invalid", "indent_width".into(), None))?,
    };
    Ok(match p.opt_text("indent_char")? {
        None | Some("space") => " ".repeat(usize::try_from(width).unwrap_or(2)),
        Some("tab") => "\t".into(),
        Some(_) => return no("parameter_invalid", "indent_char"),
    })
}

fn format_options(p: &P<'_>) -> R<FormatOptions> {
    let mut o = FormatOptions::default();
    match p.opt_text("indent_char")? {
        None | Some("space") => {}
        Some("tab") => o.indent_char = IndentChar::Tab,
        Some(_) => return no("parameter_invalid", "indent_char"),
    }
    if let Some(v) = p.0.get("indent_width").filter(|v| !v.is_null()) {
        o.indent_width = v.as_u64().and_then(|n| u8::try_from(n).ok()).ok_or(No("parameter_invalid", "indent_width".into(), None))?;
    }
    if let Some(v) = p.0.get("line_length").filter(|v| !v.is_null()) {
        o.line_length = v.as_u64().and_then(|n| u16::try_from(n).ok()).ok_or(No("parameter_invalid", "line_length".into(), None))?;
    }
    match p.opt_text("attribute_wrap")? {
        None | Some("keep") => {}
        Some("wrap_over_line_length") => o.attribute_wrap = AttributeWrap::WrapOverLineLength,
        Some(_) => return no("parameter_invalid", "attribute_wrap"),
    }
    o.checked().map_err(|e| code(&e))
}

/// The declaration changes of `web.css_rule`: a string sets, `null` removes, `{number, unit}` composes.
fn changes(p: &P<'_>) -> R<Vec<DeclarationChange>> {
    let Some(Value::Object(map)) = p.0.get("declarations") else {
        return no("parameter_invalid", "declarations");
    };
    if map.is_empty() || map.len() > css::MAX_EDIT_DECLARATIONS {
        return no("parameter_invalid", "declarations");
    }
    let mut out = Vec::with_capacity(map.len());
    for (property, v) in map {
        let value = match v {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => {
                let number = o.get("number").and_then(Value::as_f64).ok_or(No("parameter_invalid", bounded(property), None))?;
                let unit = match o.get("unit") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(u)) => Some(u.as_str()),
                    Some(_) => return no("parameter_invalid", property),
                };
                if o.keys().any(|k| k != "number" && k != "unit") {
                    return no("parameter_unknown", property);
                }
                let entry = css::property(property).ok_or(No("value_needs_text", bounded(property), None))?;
                Some(css::compose_number(entry, number, unit).map_err(|e| No(e.code, bounded(property), None))?)
            }
            _ => return no("parameter_invalid", property),
        };
        out.push(DeclarationChange { property: property.clone(), value });
    }
    Ok(out)
}

impl WebHost {
    /// The document type a code row works with: `type_id`, else the open file name's first candidate, else `html`.
    fn code_type(&self, p: &P<'_>) -> R<&'static DocumentType> {
        if let Some(id) = p.opt_text("type_id")? {
            return doc_types::document_type(id).ok_or(No("type_unknown", bounded(id), None));
        }
        let by_name = self.path.as_ref().and_then(|path| path.file_name()).map(|n| doc_types::types_for_file_name(&n.to_string_lossy())).and_then(|list| list.first().copied());
        Ok(by_name.or_else(|| doc_types::document_type("html")).unwrap_or(&DOCUMENT_TYPES[0]))
    }

    /// Applies `edits` as one step labelled with the row's display name.
    fn code_edit(&mut self, row: &'static CommandEntry, edits: Vec<TextEdit>, ctx: &CommandContext, cancel: &CancellationToken, mut extra: Value) -> R<Value> {
        let doc = self.doc()?;
        let patch = SourcePatch::new(doc, edits);
        let mut out = self.edit(row.display_name, patch, ctx, cancel)?;
        if let (Value::Object(o), Value::Object(e)) = (&mut out, extra.take()) {
            o.extend(e);
        }
        Ok(out)
    }

    pub(super) fn code_command(&mut self, row: &'static CommandEntry, p: &P<'_>, ctx: &CommandContext, cancel: &CancellationToken) -> R<Value> {
        match row.command_id {
            "web.document_types" => {
                let types: Vec<Value> = match p.opt_text("type_id")? {
                    Some(id) => vec![type_json(doc_types::document_type(id).ok_or(No("type_unknown", bounded(id), None))?)],
                    None => DOCUMENT_TYPES.iter().map(type_json).collect(),
                };
                let file = p.opt_text("file_name")?.map(|name| {
                    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
                    let (mime, listed) = doc_types::mime_for_extension(ext);
                    json!({"file_name": bounded(name), "candidate_types": doc_types::types_for_file_name(name).iter().map(|t| t.type_id).collect::<Vec<_>>(), "served_mime_type": mime, "mime_listed": listed})
                });
                Ok(json!({
                    "types": types, "doctypes": DOCTYPES.iter().map(|d| json!({"declaration_id": d.declaration_id, "literal": d.literal, "sha256": d.sha256})).collect::<Vec<_>>(),
                    "default_doctype": doc_types::DEFAULT_DOCTYPE, "internal_kinds": InternalKind::ALL.map(InternalKind::token), "dtd_contexts": DtdContext::ALL.map(DtdContext::token),
                    "mime_map": doc_types::MIME_MAP.iter().map(|(e, m)| json!([e, m])).collect::<Vec<_>>(), "file": file,
                }))
            }
            "web.set_doctype" => {
                let doc = self.doc()?;
                let id = p.text("declaration_id")?;
                let decl = doc_types::doctype(id).ok_or(No("doctype_unknown", bounded(id), None))?;
                let edit = formatting::doctype_edit(doc, decl).map_err(|e| code(&e))?;
                self.code_edit(row, vec![edit], ctx, cancel, json!({"declaration_id": decl.declaration_id}))
            }
            "web.tag_vocabulary" => {
                let t = self.code_type(p)?;
                let vocabulary = TagRegistry::shipped();
                let libraries: Vec<Value> = vocabulary.libraries_for(t.type_id).iter().map(|l| json!({"library_id": l.library_id, "display_name": l.display_name, "namespace_prefix": l.namespace_prefix, "declared_tag_count": l.declared_tag_count, "tag_chooser_id": l.tag_chooser_id, "applies_to_document_types": l.applies_to_document_types})).collect();
                let diagnostics: Vec<Value> = vocabulary.load_diagnostics().iter().map(|d| json!({"code": d.code, "library_id": d.library_id, "subject": d.subject, "declared": d.declared, "loaded": d.loaded})).collect();
                let tag = match p.opt_text("tag")? {
                    None => None,
                    Some(name) => {
                        let def = vocabulary.tag(t.type_id, name).ok_or(No("tag_unknown", bounded(name), None))?;
                        let attrs = vocabulary.attributes_of(def);
                        Some(json!({
                            "tag_name": def.tag_name, "library_id": def.library_id, "has_end_tag": def.has_end_tag.token(), "tag_type": def.tag_type.map(|x| x.token()),
                            "formatting_rules": def.formatting_rules.pairs().into_iter().collect::<BTreeMap<_, _>>(), "property_inspector": vocabulary.inspector(def).token(), "groups": def.groups,
                            "attributes": attrs.iter().map(|a| json!({"name": a.name, "value_type": a.value_type.token(), "case_sensitive": a.case_sensitive, "allowed_values": a.allowed_values.iter().map(|v| json!({"value": v.value, "caption": v.caption})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
                        }))
                    }
                };
                Ok(json!({
                    "type_id": t.type_id, "libraries": libraries, "tags": vocabulary.tags_for(t.type_id).iter().map(|d| d.tag_name.as_str()).collect::<Vec<_>>(), "tag": tag,
                    "value_types": ValueType::ALL.map(ValueType::token), "load_diagnostics": diagnostics,
                }))
            }
            "web.css_properties" => {
                let category = p.opt_text("category")?.map(|c| Category::parse(c).ok_or(No("category_unknown", bounded(c), None))).transpose()?;
                let entries: Vec<Value> = match p.opt_text("property")? {
                    Some(name) => vec![entry_json(css::property(name).ok_or(No("property_not_curated", bounded(name), None))?)],
                    None => css::CURATED.iter().filter(|e| category.is_none_or(|c| e.category == c)).map(entry_json).collect(),
                };
                Ok(json!({
                    "categories": Category::ALL.map(|c| json!({"category_id": c.id(), "label": c.label(), "slots": c.slots(), "curated": css::properties_in(c).len()})),
                    "control_types": ControlType::ALL.map(ControlType::token), "length_units": css::LENGTH_UNITS, "resolution_units": css::RESOLUTION_UNITS, "entries": entries,
                    "transition": {"animatable_properties": &css::ANIMATABLE_PROPERTIES[..], "pseudo_classes": css::TRANSITION_PSEUDO_CLASSES, "timing_functions": css::TIMING_FUNCTIONS, "vendor_prefixes": css::TRANSITION_VENDOR_PREFIXES},
                    "svg_web_export_defaults": {"trim_to_art_bounds": css::SVG_WEB_EXPORT_DEFAULTS.trim_to_art_bounds, "responsive": css::SVG_WEB_EXPORT_DEFAULTS.responsive, "decimal_precision": css::SVG_WEB_EXPORT_DEFAULTS.decimal_precision, "decimal_precision_contract": contract_json(&css::SVG_DECIMAL_PRECISION_CONTRACT)},
                    "gradient_saved_swatches_max": css::GRADIENT_SAVED_SWATCHES_MAX,
                    "prefix_rules": css::shipped_prefix_rules().iter().map(|r| json!({"property": r.property, "value": r.value, "emissions": r.emissions.iter().map(|e| json!({"target_vendor": e.target_vendor, "emitted_property": e.emitted_property, "emitted_value": e.emitted_value})).collect::<Vec<_>>()})).collect::<Vec<_>>(),
                    "recognised_properties": css::recognised_properties().len(),
                }))
            }
            "web.media_records" => {
                let doc = self.doc()?;
                let blocks = css::document_media_records(doc);
                Ok(json!({"blocks": blocks.iter().take(MAX_ROWS).map(|b| json!({
                    "stylesheet": key_text(doc, b.style), "query": row_text(&b.query), "query_range": span(&b.query_range),
                    "records": b.records.as_ref().ok().map(|list| list.iter().map(|r| json!({"media_type": r.media_type, "conditions": r.conditions.iter().map(|c| json!({"feature": c.feature, "operator": c.operator, "value": c.value, "unit": c.unit})).collect::<Vec<_>>(), "rules": r.rules.iter().map(|s| row_text(s)).collect::<Vec<_>>()})).collect::<Vec<_>>()),
                    "error": b.records.as_ref().err().map(|e| e.code),
                })).collect::<Vec<_>>(), "total": blocks.len()}))
            }
            "web.css_rule" => {
                let doc = self.doc()?;
                let style = node_of(doc, p.text("stylesheet")?)?;
                let changes = changes(p)?;
                let outcome = css::rule_edit(doc, style, p.opt_text("media")?, p.text("selector")?, &changes, &css::shipped_prefix_rules()).map_err(|e| code(&e))?;
                let extra = json!({"created_media": outcome.created_media, "created_rule": outcome.created_rule, "set": outcome.set, "removed": outcome.removed});
                self.code_edit(row, vec![outcome.edit], ctx, cancel, extra)
            }
            "web.code_navigator" => {
                let doc = self.doc()?;
                let id = node_of(doc, p.text("key")?)?;
                let (rules, unsupported) = css::affecting_rules(doc, id).map_err(|e| code(&e))?;
                Ok(json!({"key": key_text(doc, id), "rules": rules.iter().take(MAX_ROWS).map(|r| json!({"stylesheet": key_text(doc, r.style), "media": r.media, "selector": row_text(&r.selector), "specificity": r.specificity, "range": span(&r.whole), "declarations": r.declarations})).collect::<Vec<_>>(), "total": rules.len(), "unsupported_rules": unsupported}))
            }
            "web.code_hints" => {
                let doc = self.doc()?;
                let at = offset(doc, p, "offset")?;
                let t = self.code_type(p)?;
                let vocabulary = TagRegistry::shipped();
                let hint = hints::hints_at(doc, &vocabulary, t.type_id, at).map_err(|e| code(&e))?;
                let groups = hints::hint_groups(&vocabulary, t.type_id);
                Ok(json!({
                    "offset": at, "type_id": t.type_id,
                    "hint": hint.map(|h| json!({"group_id": h.group_id, "pattern": row_text(&h.pattern), "prefix": row_text(&h.prefix), "scheme_id": h.scheme_id, "allow_multiple_values": h.allow_multiple_values, "items": h.items.iter().map(|i| json!({"label": i.label, "value": i.value, "icon_id": i.icon_id, "description": i.description})).collect::<Vec<_>>(), "total": h.total})),
                    "groups": groups.iter().map(|g| json!({"group_id": g.group_id, "hints": g.hints.len(), "items": g.item_count(), "described": g.described()})).collect::<Vec<_>>(),
                }))
            }
            "web.regions" => {
                let doc = self.doc()?;
                let at = opt_offset(doc, p, "offset")?;
                let schemes = SchemeRegistry::shipped();
                let all = hints::regions(doc, &schemes);
                let list: Vec<&hints::Region> = match at {
                    Some(o) => hints::region_at(&all, o).into_iter().collect(),
                    None => all.iter().collect(),
                };
                Ok(json!({"regions": list.iter().take(MAX_ROWS).map(|r| json!({"range": span(&r.range), "scheme_id": r.scheme_id, "candidates": r.candidates, "decision": r.decision, "open_delimiter": r.open_delimiter, "close_delimiter": r.close_delimiter, "enclosing": r.enclosing})).collect::<Vec<_>>(), "total": list.len(), "schemes": schemes.schemes().iter().map(|s| json!({"scheme_id": s.scheme_id, "priority": s.priority})).collect::<Vec<_>>()}))
            }
            "web.validate" => {
                let doc = self.doc()?;
                if let Some(set) = p.opt_text("rule_set")?
                    && set != "html_all"
                {
                    return no("rule_set_unknown", set);
                }
                let t = self.code_type(p)?;
                if t.dtd_context != Some(DtdContext::Html) {
                    return no("rule_set_not_for_type", t.type_id);
                }
                let scope = p.opt_text("key")?.map(|k| node_of(doc, k)).transpose()?;
                let report = validation::validate(doc, &TagRegistry::shipped(), t.type_id, &validation::html_all(), scope);
                let mut out = diagnostics_json(&report);
                out["rule_set_id"] = json!("html_all");
                out["type_id"] = json!(t.type_id);
                Ok(out)
            }
            "web.lint" => {
                let doc = self.doc()?;
                let language = p.text("language").and_then(|l| Language::parse(l).ok_or(No("parameter_invalid", "language".into(), None)))?;
                let overrides: BTreeMap<String, bool> = match p.0.get("rules") {
                    None | Some(Value::Null) => BTreeMap::new(),
                    Some(Value::Object(m)) => m.iter().map(|(k, v)| v.as_bool().map(|b| (k.clone(), b)).ok_or(No("parameter_invalid", bounded(k), None))).collect::<R<_>>()?,
                    Some(_) => return no("parameter_invalid", "rules"),
                };
                let scope = p.opt_text("key")?.map(|k| node_of(doc, k)).transpose()?;
                let report = validation::lint(doc, language, &overrides, scope).map_err(|e| code(&e))?;
                let mut out = diagnostics_json(&report);
                out["language"] = json!(language.token());
                out["enabled"] = json!(validation::enabled_rules(language, &overrides).unwrap_or_default());
                Ok(out)
            }
            "web.format_source" => {
                let doc = self.doc()?;
                let opts = format_options(p)?;
                let scope = p.opt_text("key")?.map(|k| node_of(doc, k)).transpose()?;
                let t = self.code_type(p)?;
                let edits = formatting::format_edits(doc, &TagRegistry::shipped(), t.type_id, scope, opts).map_err(|e| code(&e))?;
                let count = edits.len();
                if edits.is_empty() {
                    return Ok(json!({"changed": false, "edits": 0, "dry_run": p.flag("dry_run")?, "web": self.state_json()}));
                }
                if p.flag("dry_run")? {
                    // The diff is checked against the engine (applies and parses) without being kept.
                    let (next, _, _) = patched(doc, &SourcePatch::new(doc, edits.clone()), cancel)?;
                    return Ok(json!({"changed": true, "dry_run": true, "edits": count, "diff": edits_json(&edits), "sha256_after": next.sha256_hex(), "web": self.state_json()}));
                }
                self.code_edit(row, edits, ctx, cancel, json!({"changed": true, "dry_run": false, "edits": count}))
            }
            "web.apply_comment" => {
                let doc = self.doc()?;
                let range = selection(doc, p)?;
                let (edit, scheme) = formatting::comment_edit(doc, range).map_err(|e| code(&e))?;
                self.code_edit(row, vec![edit], ctx, cancel, json!({"scheme_id": scheme}))
            }
            "web.remove_comment" => {
                let doc = self.doc()?;
                let at = offset(doc, p, "offset")?;
                let (edit, scheme) = formatting::uncomment_edit(doc, at).map_err(|e| code(&e))?;
                self.code_edit(row, vec![edit], ctx, cancel, json!({"scheme_id": scheme}))
            }
            "web.balance_braces" => {
                let doc = self.doc()?;
                let start = offset(doc, p, "start")?;
                let end = opt_offset(doc, p, "end")?.unwrap_or(start);
                if end < start {
                    return no("range_invalid", "start..end");
                }
                let r = hints::balance(doc, start..end).map_err(|e| code(&e))?;
                Ok(json!({"range": span(&r)}))
            }
            "web.select_parent_tag" => {
                let doc = self.doc()?;
                let parent = hints::parent_tag(doc, node_of(doc, p.text("key")?)?).map_err(|e| code(&e))?;
                Ok(json!({"key": key_text(doc, parent), "range": doc.node(parent).map(|n| span(&n.span))}))
            }
            "web.collapse_full_tag" => {
                let doc = self.doc()?;
                let r = hints::full_tag_range(doc, node_of(doc, p.text("key")?)?).map_err(|e| code(&e))?;
                Ok(json!({"fold": span(&r)}))
            }
            "web.collapse_selection" => {
                let doc = self.doc()?;
                let r = hints::selection_range(doc, offset(doc, p, "start")?..offset(doc, p, "end")?).map_err(|e| code(&e))?;
                Ok(json!({"fold": span(&r)}))
            }
            "web.indent" | "web.outdent" => {
                let doc = self.doc()?;
                let range = selection(doc, p)?;
                let unit = indent_unit(p)?;
                let edits = formatting::indent_edits(doc, range, row.command_id == "web.outdent", &unit).map_err(|e| code(&e))?;
                let lines = edits.len();
                self.code_edit(row, edits, ctx, cancel, json!({"lines": lines}))
            }
            _ => no("unknown_command", row.command_id),
        }
    }
}
