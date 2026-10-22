//! Menu and window descriptors (lane SMB-HARNESS-SHELL-2; [STU-ARC-014]: "Controls MUST implement
//! one reusable panel/control mechanic with typed capability configuration for text fields,
//! sliders, swatches, menus, windows ... The same descriptors MUST feed structured tools,
//! AccessKit, UserManual and Argus").
//!
//! A [`MenuBar`] is a tree of [`MenuNode`]s (titles, submenus, items). Each node carries its
//! command id (the AccessKit author id is `menu.<id>`, MENU-INV-5 of [STU-SHL-017]) and ONE
//! [`Availability`] evaluation that every projection reads: the egui renderer
//! ([`menu_bar_ui`]: disabled + hover reason, never hidden, [STU-SHL-018]; the remedy of an
//! `INAPPLICABLE_HERE` node as an enabled sibling leaf, [STU-SHL-020] / [STU-SHL-052]), the agent
//! JSON ([`MenuBar::agent_json`]) and the UserManual topics ([`MenuBar::manual_topics`]). The
//! states and reason codes are the closed sets of [STU-SHL-050] / [STU-SHL-051].
//!
//! [`MenuBar::audit`] is the test-time check of the menu law the descriptors can see: unique valid
//! ids (MENU-INV-2), depth cap ([STU-SHL-021]), reasons on every non-available node
//! ([STU-SHL-019]), a known, available remedy on every `INAPPLICABLE_HERE` node ([STU-SHL-020]),
//! no empty label (`descriptor_display_name_empty`) and no vendor product name in a label or id
//! (`vendor_product_name_in_display_name`, [STU-SHL-008] / [STU-SHL-251] / [STU-SHL-252]).
//! [`WindowSpec`] describes a window (title and items) the same way and is audited with the same
//! codes. Proof: `tests/menu_descriptors.rs`.
#![deny(clippy::arithmetic_side_effects)]

use crate::descriptor::valid_id;
use crate::projection::ManualTopic;
use serde::Serialize;
use serde_json::{Value as Json, json};

/// Schema of [`MenuBar::agent_json`].
pub const MENU_SCHEMA: &str = "hsk.studio.controls.menu@1";
/// Schema of [`WindowSpec::agent_json`].
pub const WINDOW_SCHEMA: &str = "hsk.studio.controls.window@1";
/// Deepest node below a top-level title, registry expansion included ([STU-SHL-021]).
pub const MAX_MENU_DEPTH: usize = 4;
/// Most nodes one bar or window holds (audited, `menu_too_many_nodes`).
pub const MAX_MENU_NODES: usize = 8192;
/// Longest label, in characters.
pub const MAX_LABEL_CHARS: usize = 256;

/// Vendor product names of the source suites ([STU-SECTION-003]; the provenance keys of the panel
/// catalogue [STU-SHL-113]: affinity, aftereffects, dreamweaver, figma, illustrator, indesign,
/// lightroom_classic, photoshop, premiere; plus the vendor and FigJam named in [STU-SECTION-003]).
/// Matched as whole tokens or as two adjacent tokens joined ("After Effects", "PhotoShop"), never
/// as raw substrings, so "Photo", "Design" or "fig" alone are not matches. Affinity's own product
/// words (Photo, Designer, Publisher) are common words and are caught through "Affinity".
pub const VENDOR_PRODUCT_NAMES: &[&str] =
    &["adobe", "photoshop", "illustrator", "indesign", "aftereffects", "premiere", "lightroom", "dreamweaver", "affinity", "figma", "figjam"];

/// Lowercase word tokens of `text`: runs of ASCII letters and digits, also split where a lowercase
/// letter or digit is followed by an uppercase letter (camel case). Other characters separate.
fn tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() && prev_lower && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
            cur.push(c.to_ascii_lowercase());
        } else {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_lower = false;
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// The vendor product names `text` contains (each once, in [`VENDOR_PRODUCT_NAMES`] order).
pub fn vendor_product_names(text: &str) -> Vec<&'static str> {
    let t = tokens(text);
    let mut words: Vec<String> = t.clone();
    words.extend(t.windows(2).map(|w| format!("{}{}", w[0], w[1])));
    VENDOR_PRODUCT_NAMES.iter().copied().filter(|v| words.iter().any(|w| w == v)).collect()
}

/// Availability state ([STU-SHL-050], closed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AvailabilityState {
    Available,
    InapplicableHere,
    NotInThisDocument,
}

impl AvailabilityState {
    pub fn code(self) -> &'static str {
        match self {
            Self::Available => "AVAILABLE",
            Self::InapplicableHere => "INAPPLICABLE_HERE",
            Self::NotInThisDocument => "NOT_IN_THIS_DOCUMENT",
        }
    }
}

/// Reason code ([STU-SHL-051], closed: exactly twelve members).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReasonCode {
    NoSuchContainer,
    WrongLayerKind,
    DocFeatureAbsent,
    NeedsSelection,
    WrongSelectionKind,
    WrongColorMode,
    CapabilityNotGranted,
    CapabilityFlagAbsent,
    LeaseHeldByOtherActor,
    ReadOnlyDocument,
    ExpressionDriven,
    NotImplementedYet,
}

impl ReasonCode {
    pub const ALL: [ReasonCode; 12] = [
        Self::NoSuchContainer,
        Self::WrongLayerKind,
        Self::DocFeatureAbsent,
        Self::NeedsSelection,
        Self::WrongSelectionKind,
        Self::WrongColorMode,
        Self::CapabilityNotGranted,
        Self::CapabilityFlagAbsent,
        Self::LeaseHeldByOtherActor,
        Self::ReadOnlyDocument,
        Self::ExpressionDriven,
        Self::NotImplementedYet,
    ];
    pub fn code(self) -> &'static str {
        match self {
            Self::NoSuchContainer => "NO_SUCH_CONTAINER",
            Self::WrongLayerKind => "WRONG_LAYER_KIND",
            Self::DocFeatureAbsent => "DOC_FEATURE_ABSENT",
            Self::NeedsSelection => "NEEDS_SELECTION",
            Self::WrongSelectionKind => "WRONG_SELECTION_KIND",
            Self::WrongColorMode => "WRONG_COLOR_MODE",
            Self::CapabilityNotGranted => "CAPABILITY_NOT_GRANTED",
            Self::CapabilityFlagAbsent => "CAPABILITY_FLAG_ABSENT",
            Self::LeaseHeldByOtherActor => "LEASE_HELD_BY_OTHER_ACTOR",
            Self::ReadOnlyDocument => "READ_ONLY_DOCUMENT",
            Self::ExpressionDriven => "EXPRESSION_DRIVEN",
            Self::NotImplementedYet => "NOT_IMPLEMENTED_YET",
        }
    }
}

/// One availability evaluation ([STU-SHL-019]: the same record feeds tooltip, AccessKit and the
/// command API).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Availability {
    pub state: AvailabilityState,
    pub reason_code: Option<ReasonCode>,
    pub reason_text: Option<String>,
    /// `INAPPLICABLE_HERE`: the ONE enabled command that would make the predicate true.
    pub remedy_command_id: Option<String>,
    /// `NOT_IN_THIS_DOCUMENT`: the profile clause that owns the element (when known).
    pub profile_clause: Option<String>,
}

impl Availability {
    pub fn available() -> Self {
        Self { state: AvailabilityState::Available, reason_code: None, reason_text: None, remedy_command_id: None, profile_clause: None }
    }
    pub fn inapplicable(code: ReasonCode, text: &str, remedy_command_id: &str) -> Self {
        Self {
            state: AvailabilityState::InapplicableHere,
            reason_code: Some(code),
            reason_text: Some(text.to_owned()),
            remedy_command_id: Some(remedy_command_id.to_owned()),
            profile_clause: None,
        }
    }
    pub fn not_in_document(code: ReasonCode, text: &str, profile_clause: Option<&str>) -> Self {
        Self {
            state: AvailabilityState::NotInThisDocument,
            reason_code: Some(code),
            reason_text: Some(text.to_owned()),
            remedy_command_id: None,
            profile_clause: profile_clause.map(str::to_owned),
        }
    }
    pub fn is_available(&self) -> bool {
        self.state == AvailabilityState::Available
    }
    /// The reason line every renderer shows (tooltip text, AccessKit description, command
    /// refusal): `<STATE> <REASON_CODE>: <reason text>[; remedy <id>][; clause <clause>]`.
    /// Empty when available.
    pub fn reason_line(&self) -> String {
        if self.is_available() {
            return String::new();
        }
        let mut s = format!(
            "{} {}: {}",
            self.state.code(),
            self.reason_code.map_or("", ReasonCode::code),
            self.reason_text.as_deref().unwrap_or("")
        );
        if let Some(r) = &self.remedy_command_id {
            s.push_str(&format!("; remedy {r}"));
        }
        if let Some(c) = &self.profile_clause {
            s.push_str(&format!("; clause {c}"));
        }
        s
    }
    pub fn json(&self) -> Json {
        json!({
            "availability_state": self.state.code(),
            "reason_code": self.reason_code.map(ReasonCode::code),
            "reason_text": self.reason_text,
            "remedy_command_id": self.remedy_command_id,
            "profile_clause": self.profile_clause,
        })
    }
}

/// Node kind of the menu tree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MenuNodeKind {
    /// A top-level menu title.
    Title,
    /// A node listing children (an authored submenu or a registry expansion).
    Submenu,
    /// A leaf dispatching its command id.
    Item,
}

/// One menu node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct MenuNode {
    /// Command id of a leaf, node id of a title or submenu (AccessKit author id `menu.<id>`).
    pub id: String,
    pub label: String,
    pub kind: MenuNodeKind,
    pub shortcut: Option<String>,
    /// No panel or inspector home ([STU-SHL-040]).
    pub menu_only: bool,
    /// A toggle currently on (rendered selected).
    pub selected: bool,
    /// One-sentence description (UserManual topic body and agent JSON).
    pub help: String,
    /// The label is operator data (a recent file path), not a Studio name: the naming checks
    /// (vendor names, length) do not apply to it; the id checks still do.
    pub data_label: bool,
    pub availability: Availability,
    pub children: Vec<MenuNode>,
}

impl MenuNode {
    fn new(id: &str, label: &str, kind: MenuNodeKind, children: Vec<MenuNode>) -> Self {
        Self {
            id: id.to_owned(),
            label: label.to_owned(),
            kind,
            shortcut: None,
            menu_only: false,
            selected: false,
            help: String::new(),
            data_label: false,
            availability: Availability::available(),
            children,
        }
    }
    pub fn title(id: &str, label: &str, children: Vec<MenuNode>) -> Self {
        Self::new(id, label, MenuNodeKind::Title, children)
    }
    pub fn submenu(id: &str, label: &str, children: Vec<MenuNode>) -> Self {
        Self::new(id, label, MenuNodeKind::Submenu, children)
    }
    pub fn item(id: &str, label: &str) -> Self {
        Self::new(id, label, MenuNodeKind::Item, Vec::new())
    }
    pub fn with_shortcut(mut self, shortcut: &str) -> Self {
        self.shortcut = Some(shortcut.to_owned());
        self
    }
    pub fn with_help(mut self, help: &str) -> Self {
        self.help = help.to_owned();
        self
    }
    pub fn with_availability(mut self, availability: Availability) -> Self {
        self.availability = availability;
        self
    }
    pub fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }
    pub fn menu_only(mut self) -> Self {
        self.menu_only = true;
        self
    }
    /// Marks the label as operator data ([`MenuNode::data_label`]).
    pub fn data_label(mut self) -> Self {
        self.data_label = true;
        self
    }
    /// AccessKit author id: `menu.<id>` (MENU-INV-5).
    pub fn author_id(&self) -> String {
        author_id(&self.id)
    }
}

/// `menu.<id>` (MENU-INV-5).
pub fn author_id(id: &str) -> String {
    format!("menu.{id}")
}

/// Author id of the enabled remedy sibling leaf rendered after an `INAPPLICABLE_HERE` node.
pub fn remedy_author_id(id: &str) -> String {
    format!("menu.{id}.remedy")
}

/// One finding of [`MenuBar::audit`] / [`WindowSpec::audit`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DescriptorIssue {
    pub code: &'static str,
    pub node: String,
    pub detail: String,
}

fn issue(code: &'static str, node: &str, detail: String) -> DescriptorIssue {
    DescriptorIssue { code, node: node.to_owned(), detail }
}

/// Label and id checks shared by menus and windows (`data_label`: the label is operator data, only
/// emptiness and control characters are checked on it).
fn audit_names(id: &str, label: &str, data_label: bool, out: &mut Vec<DescriptorIssue>) {
    if !valid_id(id) {
        out.push(issue("descriptor_id_invalid", id, "ids are lowercase ascii, digits, '.', '_', '-', at most 96 characters".into()));
    }
    if label.trim().is_empty() {
        out.push(issue("descriptor_display_name_empty", id, String::new()));
    } else if (!data_label && label.chars().count() > MAX_LABEL_CHARS) || label.chars().any(char::is_control) {
        out.push(issue("descriptor_display_name_invalid", id, format!("at most {MAX_LABEL_CHARS} characters, no control characters")));
    }
    let checked: &[(&str, &str)] = if data_label { &[("id", id)] } else { &[("label", label), ("id", id)] };
    for (what, text) in checked.iter().copied() {
        let names = vendor_product_names(text);
        if !names.is_empty() {
            out.push(issue("vendor_product_name_in_display_name", id, format!("{what} '{text}' names {}", names.join(", "))));
        }
    }
}

/// A menu bar: its top-level titles in order.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct MenuBar {
    pub titles: Vec<MenuNode>,
}

/// One node of a depth-first walk: the label path from the title, the depth below the title
/// (title 0) and the node.
pub struct Walked<'a> {
    pub path: Vec<&'a str>,
    pub depth: usize,
    pub node: &'a MenuNode,
}

impl MenuBar {
    pub fn new(titles: Vec<MenuNode>) -> Self {
        Self { titles }
    }

    /// Every node, depth first in menu order.
    pub fn walk(&self) -> Vec<Walked<'_>> {
        fn go<'a>(n: &'a MenuNode, path: &[&'a str], depth: usize, out: &mut Vec<Walked<'a>>) {
            let mut p = path.to_vec();
            p.push(n.label.as_str());
            out.push(Walked { path: p.clone(), depth, node: n });
            for c in &n.children {
                go(c, &p, depth.saturating_add(1), out);
            }
        }
        let mut out = Vec::new();
        for t in &self.titles {
            go(t, &[], 0, &mut out);
        }
        out
    }

    /// The node with `id`.
    pub fn find(&self, id: &str) -> Option<&MenuNode> {
        self.walk().into_iter().find(|w| w.node.id == id).map(|w| w.node)
    }

    /// Makes every `INAPPLICABLE_HERE` remedy name an AVAILABLE leaf ([STU-SHL-020]: ONE enabled
    /// command): a remedy that is itself not available is replaced by its own remedy (at most
    /// [`MAX_MENU_DEPTH`] hops); when no available remedy is reached the node becomes
    /// `NOT_IN_THIS_DOCUMENT` with the same reason code and text (no single action reaches it).
    pub fn resolve_remedies(&mut self) {
        let snapshot = self.clone();
        let usable = |id: &str| snapshot.find(id).is_some_and(|n| n.kind == MenuNodeKind::Item && n.availability.is_available());
        fn fix(n: &mut MenuNode, snapshot: &MenuBar, usable: &dyn Fn(&str) -> bool) {
            if n.availability.state == AvailabilityState::InapplicableHere {
                let mut remedy = n.availability.remedy_command_id.clone();
                let mut hops = 0usize;
                while let Some(r) = remedy.clone() {
                    if usable(&r) || hops >= MAX_MENU_DEPTH {
                        break;
                    }
                    hops = hops.saturating_add(1);
                    remedy = snapshot
                        .find(&r)
                        .filter(|m| m.availability.state == AvailabilityState::InapplicableHere)
                        .and_then(|m| m.availability.remedy_command_id.clone());
                }
                match remedy.filter(|r| usable(r)) {
                    Some(r) => n.availability.remedy_command_id = Some(r),
                    None => {
                        n.availability.state = AvailabilityState::NotInThisDocument;
                        n.availability.remedy_command_id = None;
                    }
                }
            }
            for c in &mut n.children {
                fix(c, snapshot, usable);
            }
        }
        for t in &mut self.titles {
            fix(t, &snapshot, &usable);
        }
    }

    /// The menu-law findings (empty = clean). See the module docs for the codes.
    pub fn audit(&self) -> Vec<DescriptorIssue> {
        let mut out = Vec::new();
        let walked = self.walk();
        if walked.len() > MAX_MENU_NODES {
            out.push(issue("menu_too_many_nodes", "", format!("{} > {MAX_MENU_NODES}", walked.len())));
            return out;
        }
        let mut seen: Vec<&str> = Vec::with_capacity(walked.len());
        for w in &walked {
            let n = w.node;
            audit_names(&n.id, &n.label, n.data_label, &mut out);
            if seen.contains(&n.id.as_str()) {
                out.push(issue("menu_id_duplicate", &n.id, "every id appears once (MENU-INV-2)".into()));
            }
            seen.push(n.id.as_str());
            match (n.kind, w.depth) {
                (MenuNodeKind::Title, 0) => {}
                (MenuNodeKind::Title, _) => out.push(issue("menu_title_nested", &n.id, String::new())),
                (_, 0) => out.push(issue("menu_top_level_not_title", &n.id, String::new())),
                _ => {}
            }
            if w.depth > MAX_MENU_DEPTH {
                out.push(issue("menu_depth_exceeded", &n.id, format!("depth {} > {MAX_MENU_DEPTH} (STU-SHL-021)", w.depth)));
            }
            if n.kind == MenuNodeKind::Item && !n.children.is_empty() {
                out.push(issue("menu_item_has_children", &n.id, String::new()));
            }
            if n.kind != MenuNodeKind::Item && n.children.is_empty() && n.availability.is_available() {
                out.push(issue("menu_submenu_empty", &n.id, "an empty node needs a typed reason (STU-SHL-038)".into()));
            }
            let a = &n.availability;
            if !a.is_available() && (a.reason_code.is_none() || a.reason_text.as_deref().is_none_or(|t| t.trim().is_empty())) {
                out.push(issue("availability_reason_missing", &n.id, "STU-SHL-019".into()));
            }
            if a.state == AvailabilityState::InapplicableHere {
                match a.remedy_command_id.as_deref() {
                    None => out.push(issue("inapplicable_without_remedy", &n.id, "STU-SHL-020".into())),
                    Some(r) => match walked.iter().find(|x| x.node.id == r) {
                        None => out.push(issue("remedy_unknown", &n.id, r.to_owned())),
                        Some(x) if !x.node.availability.is_available() || x.node.kind != MenuNodeKind::Item => {
                            out.push(issue("remedy_not_available", &n.id, r.to_owned()))
                        }
                        Some(_) => {}
                    },
                }
            }
        }
        out
    }

    /// Model-navigable projection: every node with its menu path, author id and availability.
    pub fn agent_json(&self) -> Json {
        let nodes: Vec<Json> = self
            .walk()
            .iter()
            .map(|w| {
                let n = w.node;
                json!({
                    "id": n.id, "author_id": n.author_id(), "label": n.label, "kind": n.kind, "menu_path": w.path,
                    "depth": w.depth, "shortcut": n.shortcut, "menu_only": n.menu_only, "selected": n.selected, "help": n.help,
                    "availability": n.availability.json(),
                    "children": n.children.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({"schema": MENU_SCHEMA, "titles": self.titles.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), "nodes": nodes})
    }

    /// UserManual topics: one per title (its contents) and one per leaf (path, command id,
    /// shortcut, help and the current availability line).
    pub fn manual_topics(&self) -> Vec<ManualTopic> {
        self.walk()
            .iter()
            .map(|w| {
                let n = w.node;
                let mut body = format!("Menu path: {}. Command id `{}`.", w.path.join(" > "), n.id);
                if let Some(s) = &n.shortcut {
                    body.push_str(&format!(" Shortcut {s}."));
                }
                if !n.help.is_empty() {
                    body.push_str(&format!("\n{}", n.help));
                }
                if !n.children.is_empty() {
                    let labels: Vec<&str> = n.children.iter().map(|c| c.label.as_str()).collect();
                    body.push_str(&format!("\nContains: {}.", labels.join(", ")));
                }
                if !n.availability.is_available() {
                    body.push_str(&format!("\nNow: {}.", n.availability.reason_line()));
                }
                ManualTopic { id: n.author_id(), title: n.label.clone(), body }
            })
            .collect()
    }
}

/// Role of one window item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowRole {
    /// A button.
    Action,
    /// A text field.
    Field,
    /// A choice among options.
    Choice,
    /// A check box.
    Toggle,
    /// A list of rows (generated content).
    List,
    /// Read-only text.
    Text,
}

/// One item of a window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WindowItem {
    pub id: String,
    pub label: String,
    pub role: WindowRole,
    pub help: String,
}

/// A window descriptor: its stable id, title and items.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WindowSpec {
    pub id: String,
    pub title: String,
    pub help: String,
    pub items: Vec<WindowItem>,
}

impl WindowSpec {
    pub fn new(id: &str, title: &str, help: &str) -> Self {
        Self { id: id.to_owned(), title: title.to_owned(), help: help.to_owned(), items: Vec::new() }
    }
    pub fn with(mut self, id: &str, label: &str, role: WindowRole, help: &str) -> Self {
        self.items.push(WindowItem { id: id.to_owned(), label: label.to_owned(), role, help: help.to_owned() });
        self
    }
    pub fn item(&self, id: &str) -> Option<&WindowItem> {
        self.items.iter().find(|i| i.id == id)
    }
    /// The label of item `id` (empty when absent; the audit of the caller's spec proves presence).
    pub fn label(&self, id: &str) -> &str {
        self.item(id).map_or("", |i| i.label.as_str())
    }
    /// Unique valid ids, non-empty labels, no vendor product name (same codes as the menu audit).
    pub fn audit(&self) -> Vec<DescriptorIssue> {
        let mut out = Vec::new();
        if self.items.len() > MAX_MENU_NODES {
            out.push(issue("menu_too_many_nodes", &self.id, format!("{} > {MAX_MENU_NODES}", self.items.len())));
            return out;
        }
        audit_names(&self.id, &self.title, false, &mut out);
        let mut seen: Vec<&str> = vec![self.id.as_str()];
        for i in &self.items {
            audit_names(&i.id, &i.label, false, &mut out);
            if seen.contains(&i.id.as_str()) {
                out.push(issue("menu_id_duplicate", &i.id, String::new()));
            }
            seen.push(i.id.as_str());
        }
        out
    }
    pub fn agent_json(&self) -> Json {
        json!({"schema": WINDOW_SCHEMA, "id": self.id, "title": self.title, "help": self.help, "items": self.items})
    }
    pub fn manual_topic(&self) -> ManualTopic {
        let items: Vec<String> = self.items.iter().map(|i| format!("{} (`{}`, {:?}): {}", i.label, i.id, i.role, i.help)).collect();
        ManualTopic { id: self.id.clone(), title: self.title.clone(), body: format!("{}\nItems: {}.", self.help, items.join("; ")) }
    }
}

/// One node the renderer drew this frame.
#[derive(Clone, Debug)]
pub struct RenderedNode {
    pub author_id: String,
    pub id: egui::Id,
    pub rect: egui::Rect,
    pub enabled: bool,
    /// The reason line ([`Availability::reason_line`]) of a disabled node (empty when enabled).
    pub reason: String,
}

/// What [`menu_bar_ui`] drew and what was clicked.
#[derive(Clone, Debug, Default)]
pub struct MenuUi {
    /// The command id of the clicked leaf (or of the clicked remedy leaf's remedy).
    pub clicked: Option<String>,
    pub nodes: Vec<RenderedNode>,
}

impl MenuUi {
    pub fn drawn(&self) -> Vec<String> {
        self.nodes.iter().map(|n| n.author_id.clone()).collect()
    }
}

/// Draws `bar` with egui's menu bar: every node at every level, a node that is not available
/// greyed with its reason line as hover text (never hidden), a toggle drawn selected, and after an
/// `INAPPLICABLE_HERE` leaf an enabled sibling leaf `-> <remedy label>` (author id
/// [`remedy_author_id`]) dispatching the remedy command. Widget ids are `id_of(author id)`.
pub fn menu_bar_ui(ui: &mut egui::Ui, bar: &MenuBar, id_of: &dyn Fn(&str) -> egui::Id) -> MenuUi {
    let mut out = MenuUi::default();
    egui::MenuBar::new().ui(ui, |ui| {
        for t in &bar.titles {
            node_ui(ui, bar, t, id_of, &mut out);
        }
    });
    out
}

fn node_ui(ui: &mut egui::Ui, bar: &MenuBar, n: &MenuNode, id_of: &dyn Fn(&str) -> egui::Id, out: &mut MenuUi) {
    let author = n.author_id();
    let enabled = n.availability.is_available();
    let reason = n.availability.reason_line();
    match n.kind {
        MenuNodeKind::Title | MenuNodeKind::Submenu => {
            let r = ui
                .push_id(id_of(&author), |ui| {
                    ui.add_enabled_ui(enabled, |ui| {
                        ui.menu_button(n.label.as_str(), |ui| {
                            for c in &n.children {
                                node_ui(ui, bar, c, id_of, out);
                            }
                        })
                        .response
                    })
                    .inner
                })
                .inner;
            let r = if enabled { r } else { r.on_disabled_hover_text(&reason) };
            out.nodes.push(RenderedNode { author_id: author, id: r.id, rect: r.rect, enabled, reason });
        }
        MenuNodeKind::Item => {
            let mut button = egui::Button::new(n.label.as_str()).selected(n.selected);
            if let Some(s) = &n.shortcut {
                button = button.shortcut_text(s.as_str());
            }
            let r = ui.push_id(id_of(&author), |ui| ui.add_enabled(enabled, button)).inner;
            let r = if enabled { r } else { r.on_disabled_hover_text(&reason) };
            if r.clicked() {
                out.clicked = Some(n.id.clone());
                ui.close();
            }
            out.nodes.push(RenderedNode { author_id: author, id: r.id, rect: r.rect, enabled, reason });
            if let Some(remedy) = n.availability.remedy_command_id.as_deref().filter(|_| n.availability.state == AvailabilityState::InapplicableHere) {
                let label = bar.find(remedy).map_or(remedy, |m| m.label.as_str());
                let remedy_author = remedy_author_id(&n.id);
                let r = ui.push_id(id_of(&remedy_author), |ui| ui.button(format!("-> {label}"))).inner;
                if r.clicked() {
                    out.clicked = Some(remedy.to_owned());
                    ui.close();
                }
                out.nodes.push(RenderedNode { author_id: remedy_author, id: r.id, rect: r.rect, enabled: true, reason: String::new() });
            }
        }
    }
}
