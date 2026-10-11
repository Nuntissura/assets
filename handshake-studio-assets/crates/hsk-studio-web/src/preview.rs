//! Structured (non-pixel) preview over one source revision: element tree with ARIA role, accessible name,
//! focusability and keyboard tab order, plus the inline `<style>` rules active at a viewport width. The
//! same `WebDoc` that drives the editor drives the preview, and the result carries its source revision
//! (STU-ARC-015). No layout, no full cascade: only `display` is resolved, from simple selectors
//! (`tag`, `*`, `.class`, `#id`, compounds and comma lists) by importance, origin, specificity and order.
//! Everything outside that is listed in `unsupported`, never guessed. Nothing is fetched or executed.

use crate::css::{
    Compound, Declaration, ElemInfo, RuleRefusal, Stylesheet, parse_declarations, parse_selectors,
    parse_stylesheet,
};
use crate::dom::{ContentModel, NodeId, NodeKind, WebDoc};
use crate::error::{LimitKind, WebError};
use crate::responsive::matches_any;
use hsk_studio_accord::CancellationToken;
use std::collections::{HashMap, HashSet};
use std::ops::Range;

/// Work cap for rule x element matching.
const MAX_MATCH_WORK: usize = 20_000_000;
const MAX_MATCHES_LISTED: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HiddenReason {
    HiddenAttribute,
    InputHidden,
    InlineStyle,
    StyleRule,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreviewNode {
    pub id: NodeId,
    pub depth: usize,
    /// Lower-cased tag name.
    pub tag: String,
    pub role: Option<String>,
    pub name: Option<String>,
    pub heading_level: Option<u8>,
    pub focusable: bool,
    pub tab_index: Option<i32>,
    /// False when `display:none` (or the `hidden` attribute) applies at this width; descendants are omitted.
    pub rendered: bool,
    pub hidden: Option<HiddenReason>,
    /// Inside (or on) an `aria-hidden="true"` element.
    pub a11y_hidden: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActiveRule {
    pub selector: String,
    pub selector_span: Range<usize>,
    /// Highest specificity among the selectors of the list that matched at least one element, else the highest.
    pub specificity: u32,
    pub declarations: Vec<Declaration>,
    /// Query spans of the enclosing `@media` rules, outermost first (all match at this width).
    pub media: Vec<Range<usize>>,
    pub matched_count: usize,
    /// Up to 1024 matched elements.
    pub matched: Vec<NodeId>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum UnsupportedRuleReason {
    Rule(RuleRefusal),
    /// Rules inside an `@media` whose query is outside the subset (the typed error is kept).
    MediaQuery(WebError),
}

#[derive(Clone, Debug, PartialEq)]
pub struct UnsupportedRule {
    pub span: Range<usize>,
    pub reason: UnsupportedRuleReason,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PreviewTree {
    pub source_revision: u64,
    pub viewport_width_px: f64,
    /// Elements in document order, rendered ones only below an unrendered ancestor omitted.
    pub nodes: Vec<PreviewNode>,
    /// Keyboard order: positive `tabindex` ascending, then natural order; negative and disabled skipped.
    pub tab_order: Vec<NodeId>,
    pub active_rules: Vec<ActiveRule>,
    pub unsupported: Vec<UnsupportedRule>,
    /// Interactive or image nodes in the accessibility tree that have no accessible name.
    pub unnamed: Vec<NodeId>,
}

const NOT_RENDERED: &[&str] = &[
    "head", "script", "style", "template", "title", "meta", "link", "base",
];
const SECTIONING: &[&str] = &["article", "aside", "main", "nav", "section"];
const NAME_FROM_CONTENT: &[&str] = &["button", "link", "heading", "columnheader"];
const NEEDS_NAME: &[&str] = &[
    "button",
    "link",
    "img",
    "textbox",
    "checkbox",
    "radio",
    "combobox",
    "slider",
    "spinbutton",
];

fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

struct Maps {
    ids: HashMap<String, NodeId>,
    labels_for: HashMap<String, NodeId>,
}

fn attr(doc: &WebDoc, id: NodeId, name: &str) -> Option<String> {
    doc.attribute(id, name).and_then(|a| doc.attr_value(a))
}

fn has_attr(doc: &WebDoc, id: NodeId, name: &str) -> bool {
    doc.attribute(id, name).is_some()
}

fn input_type(doc: &WebDoc, id: NodeId) -> String {
    attr(doc, id, "type")
        .map(|t| t.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "text".into())
}

fn role_of(
    doc: &WebDoc,
    id: NodeId,
    tag: &str,
    sectioning_depth: usize,
) -> (Option<String>, Option<u8>) {
    if let Some(role) = attr(doc, id, "role") {
        let first = role
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if !first.is_empty() && first.bytes().all(|c| c.is_ascii_lowercase() || c == b'-') {
            let level = tag
                .strip_prefix('h')
                .and_then(|n| n.parse::<u8>().ok())
                .filter(|n| (1..=6).contains(n));
            return (Some(first), level);
        }
    }
    let named = has_attr(doc, id, "aria-label") || has_attr(doc, id, "aria-labelledby");
    let role = match tag {
        "nav" => "navigation",
        "main" => "main",
        "aside" => "complementary",
        "article" => "article",
        "header" if sectioning_depth == 0 => "banner",
        "footer" if sectioning_depth == 0 => "contentinfo",
        "section" if named => "region",
        "form" => "form",
        "button" => "button",
        "a" | "area" if has_attr(doc, id, "href") => "link",
        "img" => {
            if attr(doc, id, "alt").is_some_and(|a| a.is_empty()) {
                "presentation"
            } else {
                "img"
            }
        }
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            return (Some("heading".into()), tag[1..].parse().ok());
        }
        "ul" | "ol" => "list",
        "li" => "listitem",
        "table" => "table",
        "tr" => "row",
        "td" => "cell",
        "th" => "columnheader",
        "select" => "combobox",
        "textarea" => "textbox",
        "dialog" => "dialog",
        "hr" => "separator",
        "progress" => "progressbar",
        "input" => match input_type(doc, id).as_str() {
            "hidden" => return (None, None),
            "checkbox" => "checkbox",
            "radio" => "radio",
            "range" => "slider",
            "number" => "spinbutton",
            "submit" | "button" | "reset" | "image" => "button",
            _ => "textbox",
        },
        _ => return (None, None),
    };
    (Some(role.to_owned()), None)
}

fn label_text(doc: &WebDoc, id: NodeId, maps: &Maps) -> Option<String> {
    if let Some(html_id) = attr(doc, id, "id")
        && let Some(&label) = maps.labels_for.get(&html_id)
    {
        let text = normalize(&doc.text_content(label));
        if !text.is_empty() {
            return Some(text);
        }
    }
    let mut cur = doc.node(id)?.parent;
    while let Some(p) = cur {
        if doc
            .tag_name(p)
            .is_some_and(|t| t.eq_ignore_ascii_case("label"))
        {
            let text = normalize(&doc.text_content(p));
            return (!text.is_empty()).then_some(text);
        }
        cur = doc.node(p)?.parent;
    }
    None
}

fn accessible_name(
    doc: &WebDoc,
    id: NodeId,
    tag: &str,
    role: Option<&str>,
    maps: &Maps,
) -> Option<String> {
    if let Some(v) = attr(doc, id, "aria-labelledby") {
        let parts: Vec<String> = v
            .split_whitespace()
            .filter_map(|i| maps.ids.get(i))
            .map(|&n| normalize(&doc.text_content(n)))
            .filter(|t| !t.is_empty())
            .collect();
        if !parts.is_empty() {
            return Some(parts.join(" "));
        }
    }
    if let Some(v) = attr(doc, id, "aria-label").map(|v| normalize(&v))
        && !v.is_empty()
    {
        return Some(v);
    }
    let from_attr = |name: &str| {
        attr(doc, id, name)
            .map(|v| normalize(&v))
            .filter(|v| !v.is_empty())
    };
    let own = match tag {
        "img" | "area" => from_attr("alt"),
        "input" => match input_type(doc, id).as_str() {
            "image" => from_attr("alt"),
            "button" | "submit" | "reset" => from_attr("value"),
            _ => label_text(doc, id, maps),
        },
        "textarea" | "select" => label_text(doc, id, maps),
        _ => None,
    };
    if own.is_some() {
        return own;
    }
    if role.is_some_and(|r| NAME_FROM_CONTENT.contains(&r)) {
        let text = normalize(&doc.text_content(id));
        if !text.is_empty() {
            return Some(text);
        }
    }
    from_attr("title")
}

fn parse_tabindex(doc: &WebDoc, id: NodeId) -> Option<i32> {
    attr(doc, id, "tabindex")?.trim().parse::<i32>().ok()
}

fn natively_focusable(doc: &WebDoc, id: NodeId, tag: &str) -> bool {
    match tag {
        "a" | "area" => has_attr(doc, id, "href"),
        "button" | "select" | "textarea" => !has_attr(doc, id, "disabled"),
        "input" => input_type(doc, id) != "hidden" && !has_attr(doc, id, "disabled"),
        "iframe" | "summary" => true,
        _ => false,
    }
}

/// Winning `display` candidate: (important, inline, specificity, order, is_none). Greater wins.
type DisplayWin = (bool, bool, u32, usize, bool);

fn last_display(decls: &[Declaration]) -> Option<&Declaration> {
    decls.iter().rev().find(|d| d.property == "display")
}

struct PendingRule {
    selectors: Vec<Compound>,
    public: ActiveRule,
}

/// Build the preview of `doc` at `viewport_width_px` (finite, >= 0).
pub fn preview_tree(
    doc: &WebDoc,
    viewport_width_px: f64,
    cancel: &CancellationToken,
) -> Result<PreviewTree, WebError> {
    cancel.check()?;
    if !viewport_width_px.is_finite() || viewport_width_px < 0.0 {
        return Err(WebError::InvalidViewport);
    }
    let limits = *doc.limits();

    // 1. Style sources: every <style> raw body, in document order.
    let mut sheet = Stylesheet::default();
    let mut ids: HashMap<String, NodeId> = HashMap::new();
    let mut labels_for: HashMap<String, NodeId> = HashMap::new();
    let mut infos: Vec<Option<ElemInfo>> = Vec::with_capacity(doc.len());
    for id in 0..doc.len() {
        if id & 0x3ff == 0 {
            cancel.check()?;
        }
        let node = doc.node(id).ok_or(WebError::NoSuchNode)?;
        if node.kind != NodeKind::Element {
            infos.push(None);
            continue;
        }
        let tag = doc.tag_name(id).unwrap_or("").to_ascii_lowercase();
        let html_id = attr(doc, id, "id");
        if let Some(v) = &html_id {
            ids.entry(v.clone()).or_insert(id);
        }
        if tag == "label"
            && let Some(f) = attr(doc, id, "for")
        {
            labels_for.entry(f).or_insert(id);
        }
        if tag == "style" && node.content == ContentModel::RawText {
            for &child in &node.children {
                if let Some(raw) = doc.node(child).filter(|c| c.kind == NodeKind::Raw) {
                    parse_stylesheet(
                        doc.slice(&raw.span),
                        raw.span.start,
                        &mut sheet,
                        &limits,
                        cancel,
                    )?;
                }
            }
        }
        let classes = attr(doc, id, "class")
            .map(|c| c.split_whitespace().map(str::to_owned).collect())
            .unwrap_or_default();
        infos.push(Some(ElemInfo {
            tag,
            id: html_id,
            classes,
        }));
    }
    let maps = Maps { ids, labels_for };

    // 2. Rules active at this width, in source order.
    let mut unsupported: Vec<UnsupportedRule> = sheet
        .unsupported
        .iter()
        .map(|(span, r)| UnsupportedRule {
            span: span.clone(),
            reason: UnsupportedRuleReason::Rule(*r),
        })
        .collect();
    let mut reported_media: HashSet<usize> = HashSet::new();
    let mut pending: Vec<PendingRule> = Vec::new();
    'rules: for rule in &sheet.rules {
        let mut chain = Vec::new();
        let mut cur = rule.media;
        while let Some(m) = cur {
            chain.push(m);
            cur = sheet.media[m].parent;
        }
        chain.reverse();
        for &m in &chain {
            match &sheet.media[m].breakpoints {
                Ok(list) if matches_any(list, viewport_width_px) => {}
                Ok(_) => continue 'rules,
                Err(e) => {
                    if reported_media.insert(m) {
                        unsupported.push(UnsupportedRule {
                            span: sheet.media[m].query_span.clone(),
                            reason: UnsupportedRuleReason::MediaQuery(e.clone()),
                        });
                    }
                    continue 'rules;
                }
            }
        }
        let selector = doc.source()[rule.selector_span.clone()].to_owned();
        let Some(selectors) = parse_selectors(&selector) else {
            unsupported.push(UnsupportedRule {
                span: rule.selector_span.clone(),
                reason: UnsupportedRuleReason::Rule(RuleRefusal::Selector),
            });
            continue;
        };
        let specificity = selectors
            .iter()
            .map(Compound::specificity)
            .max()
            .unwrap_or(0);
        pending.push(PendingRule {
            selectors,
            public: ActiveRule {
                selector,
                selector_span: rule.selector_span.clone(),
                specificity,
                declarations: parse_declarations(&doc.source()[rule.decl_span.clone()]),
                media: chain
                    .iter()
                    .map(|&m| sheet.media[m].query_span.clone())
                    .collect(),
                matched_count: 0,
                matched: Vec::new(),
            },
        });
    }
    if pending.len().saturating_mul(doc.len()) > MAX_MATCH_WORK {
        return Err(WebError::LimitExceeded {
            limit: LimitKind::Rules,
            span: 0..0,
        });
    }

    // 3. Match rules against elements; resolve the winning `display` per element.
    let mut display: Vec<Option<DisplayWin>> = vec![None; doc.len()];
    for (order, rule) in pending.iter_mut().enumerate() {
        if order & 0x3f == 0 {
            cancel.check()?;
        }
        let decl = last_display(&rule.public.declarations).cloned();
        for (id, info) in infos.iter().enumerate() {
            let Some(info) = info else { continue };
            let Some(spec) = rule
                .selectors
                .iter()
                .filter(|s| s.matches(info))
                .map(Compound::specificity)
                .max()
            else {
                continue;
            };
            rule.public.matched_count += 1;
            if rule.public.matched.len() < MAX_MATCHES_LISTED {
                rule.public.matched.push(id);
            }
            if let Some(d) = &decl {
                let win: DisplayWin = (
                    d.important,
                    false,
                    spec,
                    order,
                    d.value.trim().eq_ignore_ascii_case("none"),
                );
                if display[id].is_none_or(|cur| win > cur) {
                    display[id] = Some(win);
                }
            }
        }
    }
    for id in 0..doc.len() {
        if infos[id].is_none() {
            continue;
        }
        if let Some(style) = attr(doc, id, "style")
            && let Some(d) = last_display(&parse_declarations(&style))
        {
            let win: DisplayWin = (
                d.important,
                true,
                0,
                0,
                d.value.trim().eq_ignore_ascii_case("none"),
            );
            if display[id].is_none_or(|cur| win > cur) {
                display[id] = Some(win);
            }
        }
    }

    // 4. Walk rendered elements in document order.
    let mut nodes: Vec<PreviewNode> = Vec::new();
    let mut unnamed: Vec<NodeId> = Vec::new();
    let mut work: Vec<(NodeId, usize, bool, usize)> = Vec::new();
    if let Some(root) = doc.node(doc.root()) {
        work.extend(root.children.iter().rev().map(|&c| (c, 0, false, 0)));
    }
    while let Some((id, depth, a11y_inherited, sectioning)) = work.pop() {
        if nodes.len() & 0x3ff == 0 {
            cancel.check()?;
        }
        let Some(info) = infos[id].as_ref() else {
            continue;
        };
        if NOT_RENDERED.contains(&info.tag.as_str()) {
            continue;
        }
        let tag = info.tag.clone();
        let hidden = match display[id] {
            Some((_, inline, _, _, true)) => Some(if inline {
                HiddenReason::InlineStyle
            } else {
                HiddenReason::StyleRule
            }),
            Some(_) => None,
            None if has_attr(doc, id, "hidden") => Some(HiddenReason::HiddenAttribute),
            None if tag == "input" && input_type(doc, id) == "hidden" => {
                Some(HiddenReason::InputHidden)
            }
            None => None,
        };
        let a11y_hidden = a11y_inherited
            || attr(doc, id, "aria-hidden").is_some_and(|v| v.trim().eq_ignore_ascii_case("true"));
        let (role, heading_level) = role_of(doc, id, &tag, sectioning);
        let name = if hidden.is_some() {
            None
        } else {
            accessible_name(doc, id, &tag, role.as_deref(), &maps)
        };
        let tab_index = parse_tabindex(doc, id);
        let focusable = hidden.is_none()
            && (natively_focusable(doc, id, &tag)
                || (tab_index.is_some() && !has_attr(doc, id, "disabled")));
        if hidden.is_none()
            && !a11y_hidden
            && name.is_none()
            && role.as_deref().is_some_and(|r| NEEDS_NAME.contains(&r))
        {
            unnamed.push(id);
        }
        nodes.push(PreviewNode {
            id,
            depth,
            tag: tag.clone(),
            role,
            name,
            heading_level,
            focusable,
            tab_index,
            rendered: hidden.is_none(),
            hidden,
            a11y_hidden,
        });
        if hidden.is_none() {
            let next_sectioning = sectioning + usize::from(SECTIONING.contains(&tag.as_str()));
            if let Some(node) = doc.node(id) {
                work.extend(
                    node.children
                        .iter()
                        .rev()
                        .map(|&c| (c, depth + 1, a11y_hidden, next_sectioning)),
                );
            }
        }
    }

    // 5. Keyboard order.
    let mut positive: Vec<(i32, NodeId)> = Vec::new();
    let mut natural: Vec<NodeId> = Vec::new();
    for n in nodes.iter().filter(|n| n.focusable) {
        match n.tab_index {
            Some(t) if t < 0 => {}
            Some(t) if t > 0 => positive.push((t, n.id)),
            _ => natural.push(n.id),
        }
    }
    positive.sort_by_key(|&(t, _)| t);
    let tab_order = positive
        .into_iter()
        .map(|(_, id)| id)
        .chain(natural)
        .collect();

    Ok(PreviewTree {
        source_revision: doc.revision(),
        viewport_width_px,
        nodes,
        tab_order,
        active_rules: pending.into_iter().map(|p| p.public).collect(),
        unsupported,
        unnamed,
    })
}
