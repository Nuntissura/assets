//! The Studio menu tree (lane SMB-GUI-MENU-1, build-order B6): the presentation tree every host draws, with ONE
//! availability evaluation and ONE execution path shared with the process drive surface.
//!
//! Presentation: the nineteen titles of [STU-SHL-011] in the closed order and the designed tree of [STU-SHL-023] to
//! [STU-SHL-037] ([`TREE`], folded once into a [`MenuBar`] by [`template`], one `OnceLock`, [STU-SHL-015]): labels,
//! hierarchy and ids. A node the spec lists without an id gets a DERIVED id: its parent's id (a title's namespace
//! `studio.<title>`) plus `.` plus its label in snake case, `_2` on a collision ([`Meta::derived`], [STU-SHL-241]
//! `derived`). Author ids are `menu.<id>` in kebab case (MENU-INV-5): [`node_author_id`]. A second appearance of a
//! command names its primary leaf ([`ALIASES`], `MenuNode::alias_of`, MENU-INV-2).
//!
//! Availability ([STU-SHL-046], MENU-INV-4): [`Viewer::studio_menu`] is the one function. For a leaf whose id is a
//! shell command ([`ShellCommand::from_id`]) the record IS the shell's own ([`Viewer::menu_bar`], the record
//! `state_json()["shell"]["menu"]` publishes and the drive surface's `studio.*` path gates on), adjusted only by the
//! host ([`MenuContext::hosts`]: a command the host cannot show is `NOT_IN_THIS_DOCUMENT NOT_IN_THIS_HOST`). For a leaf
//! that runs a catalog command ([`LEAVES`]) it is the one clause predicate ([`evaluate`]) over the document [`Profile`]
//! ([STU-SHL-047]) and the [`MenuState`]. WORKSPACE > Panels and FILE > Open Recent carry the shell's own leaves.
//! Everything else is `NOT_IN_THIS_DOCUMENT NOT_IMPLEMENTED_YET`; an expansion whose leaf source is not recovered
//! ships empty with that typed reason ([STU-SHL-038]).
//!
//! Remedies ([STU-SHL-020], lead ruling F6/Q3): a remedy is named only when running it makes the node available: one
//! unmet clause, a remedy whose effect is that clause ([`remedy_of`]), and the remedy leaf available now; otherwise the
//! node is `NOT_IN_THIS_DOCUMENT` with the clause. Redo has none. The worked example holds in any document: MOTION >
//! Keyframes (the spec's listed leaves, [STU-SHL-032]) is `INAPPLICABLE_HERE NO_SUCH_CONTAINER` with DOCUMENT >
//! Timelines > New Timeline as its remedy, and New Timeline needs no container.
//!
//! Execution: [`Viewer::run_menu_leaf_in`] runs a shell leaf through `Viewer::shell_command` and a catalog leaf through
//! [`run_catalog`], the executor the drive surface calls for the same command ids, with parameters built from the
//! selection ([`leaf_params`]); no step is added or dropped beside the command's own, so undo names and selection are
//! the command's. Attribution is the viewer's run context, the caller's.
//!
//! Requires ([STU-SHL-049]): a conjunction of [`Clause`]s of the twelve closed kinds; [`Clause::parse`] refuses
//! anything else with a typed code. Reason codes are [STU-SHL-051]'s twelve plus the host additions NEEDS_TOOL and
//! NOT_IN_THIS_HOST (SD proposals); the reason text is generated from the code and clause ([`reason_text`]).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::commands::leading_code;
use super::shell::ShellCommand;
use hsk_studio_controls::menu::{Availability, AvailabilityState, MenuBar, MenuNode, MenuNodeKind, ReasonCode};
use hsk_studio_harness_core::manual::{DriveParam, viewer_row_of};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::OnceLock;

/// The descriptor types the menu is made of, so a host names them without a new crate edge.
pub use hsk_studio_controls::menu as descriptors;

/// The nineteen titles of [STU-SHL-011], as node ids, in the closed order.
pub const TITLES: [&str; 19] =
    ["file", "edit", "view", "document", "select", "object", "type", "color", "effects", "motion", "insert", "code", "automate", "workspace", "go", "run", "help", "editors", "operator"];

/// Most clauses one requires expression holds, and its longest text.
pub const MAX_CLAUSES: usize = 12;
pub const MAX_REQUIRES_BYTES: usize = 256;

/// The `menu.<id>` author id of a node, in kebab case (MENU-INV-5; the [STU-MDL-100] charset has no `_`).
pub fn node_author_id(id: &str) -> String {
    format!("menu.{}", id.replace('_', "-"))
}

/// The author id of the enabled remedy sibling row after an `INAPPLICABLE_HERE` node.
pub fn remedy_author_id(id: &str) -> String {
    format!("{}.remedy", node_author_id(id))
}

// ---------------------------------------------------------------------------------------------------------------
// Requires clauses ([STU-SHL-049]).

/// A document container kind ([STU-SHL-047]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContainerKind {
    Artboard,
    PageSpread,
    Timeline,
    Board,
    SiteTree,
}

impl ContainerKind {
    pub const ALL: [ContainerKind; 5] = [Self::Artboard, Self::PageSpread, Self::Timeline, Self::Board, Self::SiteTree];
    pub fn token(self) -> &'static str {
        match self {
            Self::Artboard => "artboard",
            Self::PageSpread => "page_spread",
            Self::Timeline => "timeline",
            Self::Board => "board",
            Self::SiteTree => "site_tree",
        }
    }
    pub fn from_token(t: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.token() == t)
    }
}

/// `capability_flag:` values (closed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CapabilityFlag {
    Gpu,
    MlModelPresent,
    RawDecoder,
    VideoDecoder,
}

impl CapabilityFlag {
    pub const ALL: [CapabilityFlag; 4] = [Self::Gpu, Self::MlModelPresent, Self::RawDecoder, Self::VideoDecoder];
    pub fn token(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::MlModelPresent => "ml_model_present",
            Self::RawDecoder => "raw_decoder",
            Self::VideoDecoder => "video_decoder",
        }
    }
}

/// `color_mode:` values (closed).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ColorMode {
    Rgb,
    Cmyk,
    Grayscale,
    Lab,
    Indexed,
    Hdr,
}

impl ColorMode {
    pub const ALL: [ColorMode; 6] = [Self::Rgb, Self::Cmyk, Self::Grayscale, Self::Lab, Self::Indexed, Self::Hdr];
    pub fn token(self) -> &'static str {
        match self {
            Self::Rgb => "rgb",
            Self::Cmyk => "cmyk",
            Self::Grayscale => "grayscale",
            Self::Lab => "lab",
            Self::Indexed => "indexed",
            Self::Hdr => "hdr",
        }
    }
    /// The Folio `colour_mode` value ([STU-RAS-145]: 1 grayscale, 2 rgb, 3 cmyk, 4 lab, 6 indexed); others have no
    /// clause value.
    pub fn from_folio(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Grayscale,
            2 => Self::Rgb,
            3 => Self::Cmyk,
            4 => Self::Lab,
            6 => Self::Indexed,
            _ => return None,
        })
    }
}

/// The twelve clause kinds, in the order of [STU-SHL-049].
pub const CLAUSE_KINDS: [&str; 12] =
    ["container", "layer_kind", "doc_feature", "selection", "selection_kind", "selection_count", "tool", "capability", "capability_flag", "mode", "color_mode", "task_scope"];

/// One requires clause ([STU-SHL-049], closed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clause {
    Container(ContainerKind),
    LayerKind(String),
    DocFeature(String),
    /// `selection:>=N`.
    SelectionAtLeast(u32),
    SelectionKind(String),
    SelectionCount(u32),
    Tool(String),
    Capability(String),
    CapabilityFlag(CapabilityFlag),
    /// `mode:read_only` true, `mode:edit` false.
    ReadOnly(bool),
    ColorMode(ColorMode),
    /// `task_scope:<id|null>`.
    TaskScope(Option<String>),
}

/// Why a requires text was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequiresError {
    /// `requires_kind_unknown`, `requires_value_invalid`, `requires_too_long`, `requires_empty`.
    pub code: &'static str,
}

fn token_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.'))
}

impl Clause {
    /// Parses one clause, `<kind>:<value>`; a kind outside the twelve, a value outside a closed set or a malformed
    /// value is a typed refusal.
    pub fn parse(text: &str) -> Result<Clause, RequiresError> {
        let bad = |code| Err(RequiresError { code });
        if text.len() > MAX_REQUIRES_BYTES {
            return bad("requires_too_long");
        }
        let Some((kind, value)) = text.trim().split_once(':') else { return bad("requires_kind_unknown") };
        let word = |v: &str| if token_ok(v) { Ok(v.to_owned()) } else { Err(RequiresError { code: "requires_value_invalid" }) };
        let count = |v: &str| v.parse::<u32>().map_err(|_| RequiresError { code: "requires_value_invalid" });
        Ok(match kind {
            "container" => Clause::Container(ContainerKind::from_token(value).ok_or(RequiresError { code: "requires_value_invalid" })?),
            "layer_kind" => Clause::LayerKind(word(value)?),
            "doc_feature" => Clause::DocFeature(word(value)?),
            "selection" => Clause::SelectionAtLeast(count(value.strip_prefix(">=").ok_or(RequiresError { code: "requires_value_invalid" })?)?),
            "selection_kind" => Clause::SelectionKind(word(value)?),
            "selection_count" => Clause::SelectionCount(count(value)?),
            "tool" => Clause::Tool(word(value)?),
            "capability" => Clause::Capability(word(value)?),
            "capability_flag" => Clause::CapabilityFlag(CapabilityFlag::ALL.into_iter().find(|f| f.token() == value).ok_or(RequiresError { code: "requires_value_invalid" })?),
            "mode" => match value {
                "edit" => Clause::ReadOnly(false),
                "read_only" => Clause::ReadOnly(true),
                _ => return bad("requires_value_invalid"),
            },
            "color_mode" => Clause::ColorMode(ColorMode::ALL.into_iter().find(|m| m.token() == value).ok_or(RequiresError { code: "requires_value_invalid" })?),
            "task_scope" => Clause::TaskScope(if value == "null" { None } else { Some(word(value)?) }),
            _ => return bad("requires_kind_unknown"),
        })
    }

    /// The clause's kind, one of [`CLAUSE_KINDS`].
    pub fn kind(&self) -> &'static str {
        match self {
            Clause::Container(_) => "container",
            Clause::LayerKind(_) => "layer_kind",
            Clause::DocFeature(_) => "doc_feature",
            Clause::SelectionAtLeast(_) => "selection",
            Clause::SelectionKind(_) => "selection_kind",
            Clause::SelectionCount(_) => "selection_count",
            Clause::Tool(_) => "tool",
            Clause::Capability(_) => "capability",
            Clause::CapabilityFlag(_) => "capability_flag",
            Clause::ReadOnly(_) => "mode",
            Clause::ColorMode(_) => "color_mode",
            Clause::TaskScope(_) => "task_scope",
        }
    }

    /// The clause as text, the form [`Clause::parse`] reads.
    pub fn text(&self) -> String {
        let v = match self {
            Clause::Container(k) => k.token().to_owned(),
            Clause::LayerKind(s) | Clause::DocFeature(s) | Clause::SelectionKind(s) | Clause::Tool(s) | Clause::Capability(s) => s.clone(),
            Clause::SelectionAtLeast(n) => format!(">={n}"),
            Clause::SelectionCount(n) => n.to_string(),
            Clause::CapabilityFlag(f) => f.token().to_owned(),
            Clause::ReadOnly(r) => (if *r { "read_only" } else { "edit" }).to_owned(),
            Clause::ColorMode(m) => m.token().to_owned(),
            Clause::TaskScope(s) => s.clone().unwrap_or_else(|| "null".to_owned()),
        };
        format!("{}:{v}", self.kind())
    }

    /// The reason code an unmet clause gives: [STU-SHL-051]'s, `tool` NEEDS_TOOL (host addition, lead ruling Q7),
    /// `task_scope` DOC_FEATURE_ABSENT (UNVERIFIED reading: the closed set names none).
    pub fn reason_code(&self) -> ReasonCode {
        match self {
            Clause::Container(_) => ReasonCode::NoSuchContainer,
            Clause::LayerKind(_) => ReasonCode::WrongLayerKind,
            Clause::DocFeature(_) | Clause::TaskScope(_) => ReasonCode::DocFeatureAbsent,
            Clause::SelectionAtLeast(_) | Clause::SelectionCount(_) => ReasonCode::NeedsSelection,
            Clause::Tool(_) => ReasonCode::NeedsTool,
            Clause::SelectionKind(_) => ReasonCode::WrongSelectionKind,
            Clause::Capability(_) => ReasonCode::CapabilityNotGranted,
            Clause::CapabilityFlag(_) => ReasonCode::CapabilityFlagAbsent,
            Clause::ReadOnly(_) => ReasonCode::ReadOnlyDocument,
            Clause::ColorMode(_) => ReasonCode::WrongColorMode,
        }
    }
}

/// Parses a conjunction, clauses joined by ` & ` (at most [`MAX_CLAUSES`], [`MAX_REQUIRES_BYTES`] bytes).
pub fn parse_requires(text: &str) -> Result<Vec<Clause>, RequiresError> {
    if text.len() > MAX_REQUIRES_BYTES {
        return Err(RequiresError { code: "requires_too_long" });
    }
    let parts: Vec<&str> = text.split('&').map(str::trim).collect();
    if parts.iter().any(|p| p.is_empty()) {
        return Err(RequiresError { code: "requires_empty" });
    }
    if parts.len() > MAX_CLAUSES {
        return Err(RequiresError { code: "requires_too_long" });
    }
    parts.into_iter().map(Clause::parse).collect()
}

// ---------------------------------------------------------------------------------------------------------------
// The profile ([STU-SHL-047], [STU-SHL-054]) and the state the predicate reads.

/// The document profile: declared container kinds and feature flags, plus the colour mode `color_mode:` reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Profile {
    pub containers: BTreeSet<ContainerKind>,
    pub flags: BTreeSet<String>,
    pub color_mode: Option<ColorMode>,
}

impl Profile {
    /// The profile the open document declares: `artboard` for artboards or a canvas without page spreads,
    /// `page_spread`, `timeline` for sequences; flags `pixel_canvas`, `vector_network`, `text` from its layer kinds.
    /// It is the document's own content, so it persists through save and reopen; an undo of the step that added a
    /// container takes the container away explicitly (not silently, [STU-SHL-048]). Board and site tree containers
    /// have no document record here yet (never derived). No document: empty.
    pub fn of(viewer: &Viewer) -> Profile {
        let mut p = Profile::default();
        let Some(snap) = viewer.run.snapshot() else { return p };
        let doc = snap.document();
        if !doc.artboards.is_empty() || doc.page_spreads.is_empty() {
            p.containers.insert(ContainerKind::Artboard);
        }
        if !doc.page_spreads.is_empty() {
            p.containers.insert(ContainerKind::PageSpread);
        }
        if !doc.sequences.is_empty() {
            p.containers.insert(ContainerKind::Timeline);
        }
        for l in &doc.layers {
            let flag = match l.kind.as_str() {
                "raster" => "pixel_canvas",
                "vector" => "vector_network",
                "text" => "text",
                _ => continue,
            };
            p.flags.insert(flag.to_owned());
        }
        p.color_mode = ColorMode::from_folio(doc.colour_mode);
        p
    }

    /// The profile signature ([STU-SHL-054]): the sorted container kinds; flags are not part of it.
    pub fn signature(&self) -> Vec<&'static str> {
        let mut s: Vec<&'static str> = self.containers.iter().map(|k| k.token()).collect();
        s.sort_unstable();
        s
    }

    /// The named shape of the signature with [STU-SHL-054]'s tie-break for `{artboard}`: `pixel_canvas` raster,
    /// `vector_network` vector, `raw_develop` photo, in that order (the creating preset is not recorded yet).
    pub fn named_shape(&self) -> Option<&'static str> {
        let has = |f: &str| self.flags.contains(f);
        match self.signature().as_slice() {
            ["artboard"] if has("pixel_canvas") => Some("raster"),
            ["artboard"] if has("vector_network") => Some("vector"),
            ["artboard"] if has("raw_develop") => Some("photo"),
            ["artboard"] => Some("raster"),
            ["page_spread"] => Some("layout"),
            ["artboard", "timeline"] => Some("composition"),
            ["page_spread", "timeline"] => Some("layout"),
            ["timeline"] => Some("sequence"),
            ["artboard", "site_tree"] => Some("web"),
            _ => None,
        }
    }

    pub fn json(&self) -> Value {
        json!({"containers": self.signature(), "flags": self.flags, "color_mode": self.color_mode.map(ColorMode::token), "signature": self.signature(), "named_shape": self.named_shape()})
    }
}

/// The selected layer the predicate reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selected {
    pub layer_id: String,
    pub kind: String,
}

/// Everything the one predicate reads: the profile, transient features (`undo_step`, `redo_step`, `layers`), the
/// selection, the tool. Grants, capability flags, read-only mode and the task scope are constant inputs here (lead
/// ruling F11: recorded not_proven). There is no layout preset input: a Layout Preset cannot gate ([STU-SHL-055]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MenuState {
    pub profile: Profile,
    pub transient: BTreeSet<String>,
    pub selected: Option<Selected>,
    pub layer_ids: Vec<String>,
    pub tool: Option<String>,
    pub granted: BTreeSet<String>,
    pub capability_flags: BTreeSet<CapabilityFlag>,
    pub read_only: bool,
    pub task_scope: Option<String>,
}

impl MenuState {
    /// The state of `viewer` with the host's active `tool`.
    pub fn of(viewer: &Viewer, tool: Option<&str>) -> MenuState {
        let rows = viewer.layers();
        let history = viewer.run.history();
        let has = |k: &str| history[k].as_array().is_some_and(|a| !a.is_empty());
        let mut transient = BTreeSet::new();
        for (on, flag) in [(has("undo"), "undo_step"), (has("redo"), "redo_step"), (!rows.is_empty(), "layers")] {
            if on {
                transient.insert(flag.to_owned());
            }
        }
        let selected = viewer.selected.as_deref().and_then(|id| rows.iter().find(|r| r.layer_id == id)).map(|r| Selected { layer_id: r.layer_id.clone(), kind: r.kind.clone() });
        MenuState { profile: Profile::of(viewer), transient, selected, layer_ids: rows.into_iter().map(|r| r.layer_id).collect(), tool: tool.map(str::to_owned), ..MenuState::default() }
    }

    fn feature(&self, f: &str) -> bool {
        self.profile.flags.contains(f) || self.transient.contains(f)
    }
    fn selection_count(&self) -> u32 {
        u32::from(self.selected.is_some())
    }
}

/// An unmet clause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unmet {
    pub clause: Clause,
    pub code: ReasonCode,
    pub text: String,
}

fn article(word: &str) -> &'static str {
    if word.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" }
}

/// The reason text of an unmet clause, generated from the code and the clause ([STU-SHL-019]: prose is generated from
/// the closed code, never authored per node).
pub fn reason_text(clause: &Clause) -> String {
    match clause {
        Clause::Container(k) => {
            let w = k.token().replace('_', " ");
            format!("needs {} {w} container", article(&w))
        }
        Clause::LayerKind(k) => format!("needs a selected {k} layer"),
        Clause::DocFeature(f) if f == "undo_step" => "nothing to undo".to_owned(),
        Clause::DocFeature(f) if f == "redo_step" => "nothing to redo".to_owned(),
        Clause::DocFeature(f) if f == "layers" => "the document has no layers".to_owned(),
        Clause::DocFeature(f) => format!("needs {}", f.replace('_', " ")),
        Clause::SelectionAtLeast(n) => format!("needs at least {n} selected layer{}", if *n == 1 { "" } else { "s" }),
        Clause::SelectionKind(k) => format!("needs {} selected {k}", article(k)),
        Clause::SelectionCount(n) => format!("needs exactly {n} selected"),
        Clause::Tool(t) => format!("needs the {} tool", t.replace('_', " ")),
        Clause::Capability(c) => format!("capability {c} is not granted"),
        Clause::CapabilityFlag(f) => format!("no {} on this machine", f.token().replace('_', " ")),
        Clause::ReadOnly(true) => "only in a read-only document".to_owned(),
        Clause::ReadOnly(false) => "the document is read-only".to_owned(),
        Clause::ColorMode(m) => format!("needs {} colour mode", m.token()),
        Clause::TaskScope(Some(s)) => format!("needs the {s} task scope"),
        Clause::TaskScope(None) => "leave the task scope first".to_owned(),
    }
}

fn holds(c: &Clause, state: &MenuState) -> bool {
    match c {
        Clause::Container(k) => state.profile.containers.contains(k),
        Clause::LayerKind(k) | Clause::SelectionKind(k) => state.selected.as_ref().is_some_and(|s| &s.kind == k),
        Clause::DocFeature(f) => state.feature(f),
        Clause::SelectionAtLeast(n) => state.selection_count() >= *n,
        Clause::SelectionCount(n) => state.selection_count() == *n,
        Clause::Tool(t) => state.tool.as_deref() == Some(t.as_str()),
        Clause::Capability(c) => state.granted.contains(c),
        Clause::CapabilityFlag(f) => state.capability_flags.contains(f),
        Clause::ReadOnly(r) => state.read_only == *r,
        Clause::ColorMode(m) => state.profile.color_mode == Some(*m),
        Clause::TaskScope(s) => state.task_scope == *s,
    }
}

/// THE availability predicate ([STU-SHL-046]): every unmet clause of the conjunction, in order (empty: it holds).
pub fn evaluate(requires: &[Clause], state: &MenuState) -> Vec<Unmet> {
    requires.iter().filter(|c| !holds(c, state)).map(|c| Unmet { clause: c.clone(), code: c.reason_code(), text: reason_text(c) }).collect()
}

/// The one command whose effect IS the clause ([STU-SHL-020], lead ruling F6): New Timeline adds a timeline, First
/// Object selects a layer, Group selects the group it makes, the tool's own leaf picks the tool. None otherwise.
pub fn remedy_of(clause: &Clause) -> Option<String> {
    match clause {
        Clause::Container(ContainerKind::Timeline) => Some("studio.motion.timeline_doc.new_timeline".to_owned()),
        Clause::SelectionAtLeast(1) => Some("studio.select.navigate_selection.first_object".to_owned()),
        Clause::SelectionKind(k) if k == "group" => Some("studio.object.group".to_owned()),
        Clause::Tool(t) => Some(format!("studio.workspace.tool.{t}")),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Leaves that run catalog commands.

/// How a leaf builds its command's parameters from the state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// `layer.select` on the top, bottom, next above, next below layer, or none.
    SelectFirst,
    SelectLast,
    SelectAbove,
    SelectBelow,
    Deselect,
    /// `{layer: <selected>}`.
    Selected,
    /// `{layer: <selected>, up: <bool>}`.
    SelectedUp(bool),
    /// `{layer: <selected>, visible: <bool>}`.
    SelectedVisible(bool),
    /// `{group: <selected>}`.
    SelectedGroup,
    /// `{layers: <selected>, name}`: the spec declares no default name; the leaf's label is used (recorded reading).
    GroupSelected(&'static str),
    /// `{name}`: the spec declares no default name; the leaf's own word is used (recorded reading).
    Named(&'static str),
    /// `{name, preset_id}`: the built-in preset that best matches the document ([`new_timeline_preset`]).
    NewTimeline,
}

/// One designed leaf that runs a catalog command (a `RUN_COMMANDS` row or a `viewer.<method>` row of the catalog).
#[derive(Clone, Copy, Debug)]
pub struct Leaf {
    pub id: &'static str,
    pub command: &'static str,
    pub rule: Rule,
    pub requires: &'static str,
}

const fn leaf_row(id: &'static str, command: &'static str, rule: Rule, requires: &'static str) -> Leaf {
    Leaf { id, command, rule, requires }
}

/// Every catalog leaf (primary appearances only; [`ALIASES`] name the second ones). Add Track… is not here: the spec
/// declares no defaults for its dialog (lead ruling Q5, dialog pending).
pub const LEAVES: &[Leaf] = &[
    leaf_row("studio.select.deselect", "layer.select", Rule::Deselect, "selection:>=1"),
    leaf_row("studio.select.navigate_selection.first_object", "layer.select", Rule::SelectFirst, "doc_feature:layers"),
    leaf_row("studio.select.navigate_selection.last_object", "layer.select", Rule::SelectLast, "doc_feature:layers"),
    leaf_row("studio.select.navigate_selection.next_object_above", "layer.select", Rule::SelectAbove, "selection:>=1"),
    leaf_row("studio.select.navigate_selection.next_object_below", "layer.select", Rule::SelectBelow, "selection:>=1"),
    leaf_row("studio.object.arrange.bring_forward", "layer.shift_layer", Rule::SelectedUp(true), "selection:>=1"),
    leaf_row("studio.object.arrange.send_backward", "layer.shift_layer", Rule::SelectedUp(false), "selection:>=1"),
    leaf_row("studio.object.group", "layer.group_layers", Rule::GroupSelected("Group"), "selection:>=1"),
    leaf_row("studio.object.ungroup", "layer.ungroup", Rule::SelectedGroup, "selection_kind:group"),
    leaf_row("studio.object.hide.hide_selection", "layer.set_visible", Rule::SelectedVisible(false), "selection:>=1"),
    leaf_row("studio.object.layer.new", "layer.new_raster_layer", Rule::Named("Layer"), "container:artboard"),
    leaf_row("studio.object.layer.duplicate_layer", "layer.duplicate_layer", Rule::Selected, "selection:>=1"),
    leaf_row("studio.object.layer.delete_layer", "layer.delete_layer", Rule::Selected, "selection:>=1"),
    leaf_row("studio.motion.timeline_doc.new_timeline", "video.project_new_sequence", Rule::NewTimeline, "mode:edit"),
];

/// Second appearances of a command (MENU-INV-2): (alias leaf, primary leaf). An alias shows its primary's record and
/// runs its primary.
pub const ALIASES: &[(&str, &str)] = &[
    ("studio.edit.duplicate", "studio.object.layer.duplicate_layer"),
    ("studio.insert.layer.pixel_layer", "studio.object.layer.new"),
    ("studio.history.step_backward", "studio.history.undo"),
    ("studio.history.step_forward", "studio.history.redo"),
];

/// Requires of designed submenus no leaf runs: MOTION > Keyframes holds keyframe commands, which need a timeline.
pub const NODE_REQUIRES: &[(&str, &str)] = &[("studio.motion.keyframe", "container:timeline")];

/// The catalog leaf `id`.
pub fn leaf(id: &str) -> Option<&'static Leaf> {
    LEAVES.iter().find(|l| l.id == id)
}

/// The frame rate New Timeline uses when the document declares none (lead ruling Q5): 24 frames per second.
pub const NEW_TIMELINE_FPS: u64 = 24;

/// The built-in sequence preset New Timeline uses (lead ruling Q5): among the shipped presets at
/// [`NEW_TIMELINE_FPS`], square pixels and progressive scan, the first whose frame size is the document's size, else
/// the first at the spec's default size 1920 x 1080 ([STU-VID-011]). The document's size is its first artboard's, else
/// its first page spread's, else the rendered frame's (rounded to whole pixels).
pub fn new_timeline_preset(viewer: &Viewer) -> Option<String> {
    use hsk_studio_folio as folio;
    let ticks = hsk_studio_accord::TICKS_PER_SECOND.checked_div(NEW_TIMELINE_FPS)?;
    let size = viewer.run.snapshot().and_then(|s| {
        let doc = s.document();
        let (w, h) = doc
            .artboards
            .first()
            .map(|a| (a.bounds.width.value, a.bounds.height.value))
            .or_else(|| doc.page_spreads.first().map(|p| (p.bounds.width.value, p.bounds.height.value)))?;
        let (w, h) = (w.round(), h.round());
        (w > 0.0 && h > 0.0 && w <= f64::from(u32::MAX) && h <= f64::from(u32::MAX)).then(|| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let s = (w as u32, h as u32);
            s
        })
    });
    let size = size.or_else(|| viewer.frame().map(|f| (f.width, f.height)));
    let set = folio::shipped_sequence_presets();
    let fits = |p: &&folio::StudioSequencePreset| {
        let s = &p.settings;
        s.frame_rate.ticks_per_frame == ticks && s.pixel_aspect_ratio.numerator == s.pixel_aspect_ratio.denominator && matches!(s.field_order, folio::SequenceFieldOrder::Progressive)
    };
    let sized = |w: u32, h: u32| set.presets.iter().filter(fits).find(|p| p.settings.frame_size.width == w && p.settings.frame_size.height == h).map(|p| p.preset_id.clone());
    size.and_then(|(w, h)| sized(w, h)).or_else(|| sized(1920, 1080))
}

/// A leaf whose parameters cannot be built from the state, or a command that refused.
#[derive(Clone, Debug, PartialEq)]
pub struct MenuRefusal {
    /// `menu_node_unknown`, `menu_not_a_leaf`, `menu_not_runnable`, the reason code in lower case for a node that is
    /// not available, a parameter code of [`run_catalog`], or the command's own refusal code.
    pub code: String,
    pub availability: Option<Availability>,
    /// The command that refused, when one ran.
    pub command: Option<String>,
    pub detail: Value,
}

impl MenuRefusal {
    fn new(code: &str) -> Box<Self> {
        Box::new(MenuRefusal { code: code.to_owned(), availability: None, command: None, detail: Value::Null })
    }
}

/// The lower-case form of a reason code (`NOT_IMPLEMENTED_YET` -> `not_implemented_yet`).
pub fn reason_code_lower(a: &Availability) -> String {
    a.reason_code.map_or("unavailable", ReasonCode::code).to_ascii_lowercase()
}

/// The parameters `leaf` runs its command with in `state` (text values, the drive surface's form).
pub fn leaf_params(viewer: &Viewer, leaf: &Leaf, state: &MenuState) -> Result<BTreeMap<String, String>, Box<MenuRefusal>> {
    let mut p = BTreeMap::new();
    let sel = || state.selected.as_ref().map(|s| s.layer_id.clone()).ok_or_else(|| MenuRefusal::new("needs_selection"));
    let at = state.selected.as_ref().and_then(|s| state.layer_ids.iter().position(|l| *l == s.layer_id));
    let pick = |i: Option<usize>| i.and_then(|i| state.layer_ids.get(i)).cloned().ok_or_else(|| MenuRefusal::new("no_next_object"));
    match leaf.rule {
        Rule::SelectFirst => {
            p.insert("layer".into(), pick(Some(0))?);
        }
        Rule::SelectLast => {
            p.insert("layer".into(), pick(state.layer_ids.len().checked_sub(1))?);
        }
        Rule::SelectAbove => {
            p.insert("layer".into(), pick(at.and_then(|i| i.checked_sub(1)))?);
        }
        Rule::SelectBelow => {
            p.insert("layer".into(), pick(at.and_then(|i| i.checked_add(1)))?);
        }
        Rule::Deselect => {}
        Rule::Selected => {
            p.insert("layer".into(), sel()?);
        }
        Rule::SelectedUp(up) => {
            p.insert("layer".into(), sel()?);
            p.insert("up".into(), up.to_string());
        }
        Rule::SelectedVisible(v) => {
            p.insert("layer".into(), sel()?);
            p.insert("visible".into(), v.to_string());
        }
        Rule::SelectedGroup => {
            p.insert("group".into(), sel()?);
        }
        Rule::GroupSelected(name) => {
            p.insert("layers".into(), sel()?);
            p.insert("name".into(), name.into());
        }
        Rule::Named(name) => {
            p.insert("name".into(), name.into());
        }
        Rule::NewTimeline => {
            p.insert("name".into(), "Timeline".into());
            p.insert("preset_id".into(), new_timeline_preset(viewer).ok_or_else(|| MenuRefusal::new("no_matching_preset"))?);
        }
    }
    Ok(p)
}

/// A parameter refusal of [`run_catalog`] before anything ran: the drive surface's codes and detail.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogRefusal {
    pub code: &'static str,
    pub detail: Value,
}

/// The command ids [`run_catalog`] runs (the drive surface routes the same ids here).
pub const CATALOG_COMMANDS: [&str; 10] =
    ["layer.select", "layer.new_raster_layer", "layer.duplicate_layer", "layer.delete_layer", "layer.shift_layer", "layer.ungroup", "layer.group_layers", "layer.set_visible", "video.project_new_sequence", "video.project_add_tracks"];

fn declared(command_id: &str) -> Vec<(&'static str, &'static str, bool)> {
    if command_id == "layer.set_visible" {
        return vec![("layer", "text", true), ("visible", "boolean", true)];
    }
    viewer_row_of(command_id).map(|r| r.params.iter().map(|p: &DriveParam| (p.key, p.kind, p.required)).collect()).unwrap_or_default()
}

/// THE executor of the catalog commands menus run ([`CATALOG_COMMANDS`]), shared with the drive surface: one place
/// checks the declared parameters (`parameter_unknown`, `parameter_missing`, `parameter_invalid` with the drive
/// surface's detail) and calls the command's Viewer method. `None`: not one of these commands. `Some(Ok(Err(message)))`:
/// the command's own refusal text. `layer.set_visible` answers the new revision, the rest what their method returns.
pub fn run_catalog(viewer: &mut Viewer, command_id: &str, params: &BTreeMap<String, String>) -> Option<Result<Result<Value, String>, CatalogRefusal>> {
    if !CATALOG_COMMANDS.contains(&command_id) {
        return None;
    }
    let decl = declared(command_id);
    let refuse = |code: &'static str, detail: Value| Some(Err(CatalogRefusal { code, detail }));
    if let Some(key) = params.keys().find(|k| !decl.iter().any(|(d, _, _)| d == k)) {
        return refuse("parameter_unknown", json!({"key": key}));
    }
    if let Some((key, _, _)) = decl.iter().find(|(k, _, required)| *required && !params.contains_key(*k)) {
        return refuse("parameter_missing", json!({"key": key}));
    }
    let text = |k: &str| params.get(k).map_or("", String::as_str);
    let flag = |k: &str| match text(k) {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(CatalogRefusal { code: "parameter_invalid", detail: json!({"key": k, "expected": "boolean"}) }),
    };
    let list = |k: &str, expected: &'static str| -> Result<Vec<String>, CatalogRefusal> {
        let t = text(k);
        if t.trim_start().starts_with('[') {
            return serde_json::from_str::<Vec<String>>(t).map_err(|_| CatalogRefusal { code: "parameter_invalid", detail: json!({"key": k, "expected": expected}) });
        }
        Ok(t.split(',').filter(|s| !s.is_empty()).map(str::to_string).collect())
    };
    let into = |r: Result<String, String>| r.map(Value::String);
    let num = |r: Result<u64, String>| r.map(Value::from);
    Some(Ok(match command_id {
        "layer.select" => {
            viewer.select(params.get("layer").cloned());
            Ok(Value::Null)
        }
        "layer.new_raster_layer" => into(viewer.new_raster_layer(text("name"))),
        "layer.duplicate_layer" => into(viewer.duplicate_layer(text("layer"))),
        "layer.delete_layer" => num(viewer.delete_layer(text("layer"))),
        "layer.shift_layer" => match flag("up") {
            Ok(up) => num(viewer.shift_layer(text("layer"), up)),
            Err(e) => return Some(Err(e)),
        },
        "layer.ungroup" => num(viewer.ungroup(text("group"))),
        "layer.group_layers" => match list("layers", "a list of text") {
            Ok(layers) => into(viewer.group_layers(&layers, text("name"))),
            Err(e) => return Some(Err(e)),
        },
        "layer.set_visible" => match flag("visible") {
            Ok(v) => num(viewer.set_visible(text("layer"), v)),
            Err(e) => return Some(Err(e)),
        },
        "video.project_new_sequence" => into(viewer.project_new_sequence(text("name"), text("preset_id"))),
        _ => {
            let kinds = ["video", "audio", "caption", "submix"];
            let Some(kind) = kinds.iter().find(|k| **k == text("kind")).and_then(|k| super::project::track_kind(k)) else {
                return refuse("parameter_invalid", json!({"key": "kind", "expected": kinds.join("|")}));
            };
            match list("names", "JSON list of strings") {
                Ok(names) => viewer.project_add_tracks(text("sequence_id"), kind, &names).map(Value::from),
                Err(e) => return Some(Err(e)),
            }
        }
    }))
}

// ---------------------------------------------------------------------------------------------------------------
// STU-SHL-013 / MENU-INV-1 and MENU-INV-3: commands no leaf runs are declared, each with a typed reason.

/// Why a catalog command has no menu leaf in this build (palette_only, [STU-SHL-013], MENU-INV-1). The split is by the
/// command's verb (lane notes: the per-command reading is recorded not_proven until a menu-path lane maps each one); a
/// query named by a noun (vocabulary, records, parameter specs, hints) counts as a read (SMB-FIX-MENU-CATALOG-COVERAGE-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unreached {
    /// An authoring command no leaf runs in this build yet: its menu path is a next-lane candidate.
    MenuPathPending,
    /// A read, record, staging or plumbing command (summaries, entries, submits, catalog and state queries): drive and
    /// palette only.
    NoMenuHome,
}

impl Unreached {
    pub fn code(self) -> &'static str {
        match self {
            Self::MenuPathPending => "menu_path_pending",
            Self::NoMenuHome => "no_menu_home",
        }
    }
}

/// Every catalog command (the `ui_seed` catalog: `RUN_COMMANDS`, `timeline.*`, the canonical viewer rows) that no
/// leaf runs, with its typed reason. A command in neither this list nor a leaf fails `menu_inv_1` (a registry bug).
pub const PALETTE_ONLY: &[(&str, Unreached)] = &[
    ("action.play", Unreached::MenuPathPending),
    ("adjustment.apply", Unreached::MenuPathPending),
    ("adjustment.create", Unreached::MenuPathPending),
    ("adjustment.edit", Unreached::MenuPathPending),
    ("artboard.add", Unreached::MenuPathPending),
    ("artboard.delete", Unreached::MenuPathPending),
    ("artboard.move", Unreached::MenuPathPending),
    ("artboard.rename", Unreached::MenuPathPending),
    ("artboard.resize", Unreached::MenuPathPending),
    ("artboard.set_options", Unreached::MenuPathPending),
    ("asset.embed_link", Unreached::MenuPathPending),
    ("asset.fit", Unreached::MenuPathPending),
    ("asset.list_placements", Unreached::NoMenuHome),
    ("asset.place", Unreached::MenuPathPending),
    ("asset.update_all_modified", Unreached::MenuPathPending),
    ("audio.bind_sound", Unreached::MenuPathPending),
    ("audio.export_mix", Unreached::MenuPathPending),
    ("audio.export_stems", Unreached::MenuPathPending),
    ("audio.out_close", Unreached::MenuPathPending),
    ("audio.out_devices", Unreached::MenuPathPending),
    ("audio.out_open", Unreached::MenuPathPending),
    ("audio.out_run", Unreached::MenuPathPending),
    ("audio.out_state", Unreached::MenuPathPending),
    ("audio.out_transport", Unreached::MenuPathPending),
    ("audio.waveform_open_clip", Unreached::MenuPathPending),
    ("board.add", Unreached::MenuPathPending),
    ("board.connector.create", Unreached::MenuPathPending),
    ("board.connector.set", Unreached::MenuPathPending),
    ("board.export_png", Unreached::MenuPathPending),
    ("board.node.delete", Unreached::MenuPathPending),
    ("board.node.move", Unreached::MenuPathPending),
    ("board.node.read", Unreached::NoMenuHome),
    ("board.node.set_fill", Unreached::MenuPathPending),
    ("board.node.set_text", Unreached::MenuPathPending),
    ("board.read", Unreached::NoMenuHome),
    ("board.section.create", Unreached::MenuPathPending),
    ("board.section.set", Unreached::MenuPathPending),
    ("board.shape_with_text.create", Unreached::MenuPathPending),
    ("board.shape_with_text.set", Unreached::MenuPathPending),
    ("board.sticky.create", Unreached::MenuPathPending),
    ("board.sticky.set", Unreached::MenuPathPending),
    ("catalog.list", Unreached::NoMenuHome),
    ("catalog.resolve", Unreached::NoMenuHome),
    ("command.outcome", Unreached::NoMenuHome),
    ("component.add_property", Unreached::MenuPathPending),
    ("component.create_from_selection", Unreached::MenuPathPending),
    ("component.create_instance", Unreached::MenuPathPending),
    ("component.create_one_per_top_level_node", Unreached::MenuPathPending),
    ("component.delete_property", Unreached::MenuPathPending),
    ("component.edit_property", Unreached::MenuPathPending),
    ("component.list_instances", Unreached::NoMenuHome),
    ("component.set_description", Unreached::MenuPathPending),
    ("component.set_property_reference", Unreached::MenuPathPending),
    ("component.sync_instances", Unreached::MenuPathPending),
    ("component_set.combine_as_variants", Unreached::MenuPathPending),
    ("component_set.set_default_variant", Unreached::MenuPathPending),
    ("composition.add_audio_layer", Unreached::MenuPathPending),
    ("composition.add_effect", Unreached::MenuPathPending),
    ("composition.add_layer", Unreached::MenuPathPending),
    ("composition.add_mask", Unreached::MenuPathPending),
    ("composition.add_roto_stroke", Unreached::MenuPathPending),
    ("composition.add_shape_item", Unreached::MenuPathPending),
    ("composition.add_text_animator", Unreached::MenuPathPending),
    ("composition.add_text_selector", Unreached::MenuPathPending),
    ("composition.delete", Unreached::MenuPathPending),
    ("composition.duplicate_layer", Unreached::MenuPathPending),
    ("composition.new", Unreached::MenuPathPending),
    ("composition.precompose", Unreached::MenuPathPending),
    ("composition.queue_add", Unreached::MenuPathPending),
    ("composition.queue_add_module", Unreached::MenuPathPending),
    ("composition.queue_apply_template", Unreached::MenuPathPending),
    ("composition.queue_duplicate", Unreached::MenuPathPending),
    ("composition.queue_remove", Unreached::MenuPathPending),
    ("composition.queue_remove_module", Unreached::MenuPathPending),
    ("composition.queue_reorder", Unreached::MenuPathPending),
    ("composition.queue_set_module", Unreached::MenuPathPending),
    ("composition.queue_set_settings", Unreached::MenuPathPending),
    ("composition.read", Unreached::NoMenuHome),
    ("composition.remove_audio_layer", Unreached::MenuPathPending),
    ("composition.remove_effect", Unreached::MenuPathPending),
    ("composition.remove_layer", Unreached::MenuPathPending),
    ("composition.remove_mask", Unreached::MenuPathPending),
    ("composition.remove_shape_item", Unreached::MenuPathPending),
    ("composition.reorder_layer", Unreached::MenuPathPending),
    ("composition.set_camera", Unreached::MenuPathPending),
    ("composition.set_corner_pin", Unreached::MenuPathPending),
    ("composition.set_effect", Unreached::MenuPathPending),
    ("composition.set_layer_timing", Unreached::MenuPathPending),
    ("composition.set_light", Unreached::MenuPathPending),
    ("composition.set_mask", Unreached::MenuPathPending),
    ("composition.set_material", Unreached::MenuPathPending),
    ("composition.set_motion_blur", Unreached::MenuPathPending),
    ("composition.set_parent", Unreached::MenuPathPending),
    ("composition.set_roto", Unreached::MenuPathPending),
    ("composition.set_settings", Unreached::MenuPathPending),
    ("composition.set_shape_item", Unreached::MenuPathPending),
    ("composition.set_solid", Unreached::MenuPathPending),
    ("composition.set_switches", Unreached::MenuPathPending),
    ("composition.set_text_animator", Unreached::MenuPathPending),
    ("composition.set_time_remap", Unreached::MenuPathPending),
    ("composition.set_track_matte", Unreached::MenuPathPending),
    ("composition.set_work_area", Unreached::MenuPathPending),
    ("delivery.add", Unreached::MenuPathPending),
    ("delivery.cancel", Unreached::MenuPathPending),
    ("delivery.list", Unreached::NoMenuHome),
    ("delivery.receipts", Unreached::MenuPathPending),
    ("delivery.remove", Unreached::MenuPathPending),
    ("delivery.run", Unreached::MenuPathPending),
    ("design.auto_layout_control", Unreached::MenuPathPending),
    ("design.grid_append_child_at", Unreached::MenuPathPending),
    ("design.grid_reorder_columns", Unreached::MenuPathPending),
    ("design.grid_reorder_rows", Unreached::MenuPathPending),
    ("design.import_fig", Unreached::MenuPathPending),
    ("design.instance.inspect", Unreached::NoMenuHome),
    ("design.instance.reset_all", Unreached::MenuPathPending),
    ("design.instance.reset_override", Unreached::MenuPathPending),
    ("design.instance.set_override", Unreached::MenuPathPending),
    ("design.instance.set_property", Unreached::MenuPathPending),
    ("design.instance.swap_component", Unreached::MenuPathPending),
    ("design.instance.switch_variant", Unreached::MenuPathPending),
    ("design.set_auto_layout_mode", Unreached::MenuPathPending),
    ("design.set_auto_layout_spacing", Unreached::MenuPathPending),
    ("design.set_child_sizing", Unreached::MenuPathPending),
    ("design.set_grid_child", Unreached::MenuPathPending),
    ("design.set_grid_gaps", Unreached::MenuPathPending),
    ("design.set_grid_options", Unreached::MenuPathPending),
    ("design.set_grid_tracks", Unreached::MenuPathPending),
    ("document.add_artboard", Unreached::MenuPathPending),
    ("document.add_page", Unreached::MenuPathPending),
    ("document.checkpoint", Unreached::NoMenuHome),
    ("document.delete_artboard", Unreached::MenuPathPending),
    ("document.delete_page", Unreached::MenuPathPending),
    ("document.go_to_artboard", Unreached::MenuPathPending),
    ("document.go_to_page", Unreached::MenuPathPending),
    ("document.import", Unreached::MenuPathPending),
    ("document.move_artboard", Unreached::MenuPathPending),
    ("document.new", Unreached::MenuPathPending),
    ("document.new_document", Unreached::MenuPathPending),
    ("document.open", Unreached::MenuPathPending),
    ("document.record_host_outcome", Unreached::NoMenuHome),
    ("document.rename_artboard", Unreached::MenuPathPending),
    ("document.rename_page", Unreached::MenuPathPending),
    ("document.reopen", Unreached::MenuPathPending),
    ("document.resize_artboard", Unreached::MenuPathPending),
    ("document.resize_page", Unreached::MenuPathPending),
    ("document.save", Unreached::MenuPathPending),
    ("document.set_artboard_options", Unreached::MenuPathPending),
    ("document.set_document_entry", Unreached::NoMenuHome),
    ("document.set_export_recipes", Unreached::MenuPathPending),
    ("document.set_export_settings", Unreached::MenuPathPending),
    ("document.stage_tile", Unreached::NoMenuHome),
    ("document.submit_layout", Unreached::NoMenuHome),
    ("document.submit_layout_and_entry", Unreached::NoMenuHome),
    ("document.submit_sequence", Unreached::NoMenuHome),
    ("document.submit_sequence_and_entry", Unreached::NoMenuHome),
    ("document.submit_sequence_edits", Unreached::NoMenuHome),
    ("document.submit_sequence_edits_and_entry", Unreached::NoMenuHome),
    ("document.summary", Unreached::NoMenuHome),
    ("document.verify", Unreached::NoMenuHome),
    ("effect.apply", Unreached::MenuPathPending),
    ("effect.parameters", Unreached::NoMenuHome),
    ("effect.read", Unreached::NoMenuHome),
    ("effect.remove", Unreached::MenuPathPending),
    ("effect.reorder", Unreached::MenuPathPending),
    ("effect.toggle", Unreached::MenuPathPending),
    ("effect.update", Unreached::MenuPathPending),
    ("export.recipe_delete", Unreached::MenuPathPending),
    ("export.recipe_save", Unreached::MenuPathPending),
    ("export.recipes", Unreached::MenuPathPending),
    ("export.run", Unreached::MenuPathPending),
    ("export.set_settings", Unreached::MenuPathPending),
    ("expression.set_source", Unreached::MenuPathPending),
    ("instance.detach", Unreached::MenuPathPending),
    ("instance.get_main_component", Unreached::NoMenuHome),
    ("keyframe.add_at_current", Unreached::MenuPathPending),
    ("keyframe.copy", Unreached::MenuPathPending),
    ("keyframe.delete", Unreached::MenuPathPending),
    ("keyframe.deselect_all", Unreached::MenuPathPending),
    ("keyframe.ease", Unreached::MenuPathPending),
    ("keyframe.move", Unreached::MenuPathPending),
    ("keyframe.paste", Unreached::MenuPathPending),
    ("keyframe.select", Unreached::MenuPathPending),
    ("keyframe.selection", Unreached::MenuPathPending),
    ("keyframe.set", Unreached::MenuPathPending),
    ("keyframe.set_interpolation", Unreached::MenuPathPending),
    ("keyframe.shift", Unreached::MenuPathPending),
    ("keyframe.shift_selected", Unreached::MenuPathPending),
    ("keyframe.toggle_at_current", Unreached::MenuPathPending),
    ("keyframe.toggle_hold", Unreached::MenuPathPending),
    ("layer.delete_layout_layers", Unreached::MenuPathPending),
    ("layer.move_layer", Unreached::MenuPathPending),
    ("layer.panel", Unreached::MenuPathPending),
    ("layer.panel_set", Unreached::MenuPathPending),
    ("layer.rename", Unreached::MenuPathPending),
    ("layer.restructure", Unreached::MenuPathPending),
    ("layer.restructure_and_entry", Unreached::NoMenuHome),
    ("layer.set_blend_mode", Unreached::MenuPathPending),
    ("layer.set_layers_tiles_and_entry", Unreached::NoMenuHome),
    ("layer.set_opacity", Unreached::MenuPathPending),
    ("layer.set_tiles", Unreached::MenuPathPending),
    ("layer.set_tiles_and_entry", Unreached::NoMenuHome),
    ("layer.set_vector_paths", Unreached::MenuPathPending),
    ("layer.set_vector_paths_and_entry", Unreached::NoMenuHome),
    ("layout.apply_character_style", Unreached::MenuPathPending),
    ("layout.apply_paragraph_style", Unreached::MenuPathPending),
    ("layout.autoflow", Unreached::MenuPathPending),
    ("layout.change", Unreached::MenuPathPending),
    ("layout.character_attributes", Unreached::NoMenuHome),
    ("layout.define_language", Unreached::MenuPathPending),
    ("layout.define_stroke_style", Unreached::MenuPathPending),
    ("layout.delete_stroke_style", Unreached::MenuPathPending),
    ("layout.draw_frame", Unreached::MenuPathPending),
    ("layout.export_idml", Unreached::MenuPathPending),
    ("layout.export_layout_idml", Unreached::MenuPathPending),
    ("layout.export_layout_pdf", Unreached::MenuPathPending),
    ("layout.export_pdf", Unreached::MenuPathPending),
    ("layout.find", Unreached::MenuPathPending),
    ("layout.layout_open", Unreached::MenuPathPending),
    ("layout.open", Unreached::MenuPathPending),
    ("layout.page.add_parent_item", Unreached::MenuPathPending),
    ("layout.page.apply_parent", Unreached::MenuPathPending),
    ("layout.page.delete_page", Unreached::MenuPathPending),
    ("layout.page.detach_item", Unreached::MenuPathPending),
    ("layout.page.insert_page", Unreached::MenuPathPending),
    ("layout.page.move_page", Unreached::MenuPathPending),
    ("layout.page.new_parent", Unreached::MenuPathPending),
    ("layout.page.override_item", Unreached::MenuPathPending),
    ("layout.page.remove_override", Unreached::MenuPathPending),
    ("layout.preflight", Unreached::MenuPathPending),
    ("layout.redo", Unreached::MenuPathPending),
    ("layout.render_pdf_page", Unreached::MenuPathPending),
    ("layout.replace_text", Unreached::MenuPathPending),
    ("layout.set_character_attributes", Unreached::MenuPathPending),
    ("layout.set_columns", Unreached::MenuPathPending),
    ("layout.set_language", Unreached::MenuPathPending),
    ("layout.set_preflight_severity", Unreached::MenuPathPending),
    ("layout.smart_reflow", Unreached::MenuPathPending),
    ("layout.style.add_grep_rule", Unreached::MenuPathPending),
    ("layout.style.clear_overrides", Unreached::MenuPathPending),
    ("layout.style.delete_style", Unreached::MenuPathPending),
    ("layout.style.edit_style", Unreached::MenuPathPending),
    ("layout.style.new_style", Unreached::MenuPathPending),
    ("layout.style.remove_grep_rule", Unreached::MenuPathPending),
    ("layout.style.set_character_attributes", Unreached::MenuPathPending),
    ("layout.table.delete_column", Unreached::MenuPathPending),
    ("layout.table.delete_row", Unreached::MenuPathPending),
    ("layout.table.delete_table", Unreached::MenuPathPending),
    ("layout.table.insert_column", Unreached::MenuPathPending),
    ("layout.table.insert_row", Unreached::MenuPathPending),
    ("layout.table.insert_table", Unreached::MenuPathPending),
    ("layout.table.merge_cells", Unreached::MenuPathPending),
    ("layout.table.set_cell_text", Unreached::MenuPathPending),
    ("layout.thread", Unreached::MenuPathPending),
    ("layout.thread_info", Unreached::MenuPathPending),
    ("layout.undo", Unreached::MenuPathPending),
    ("layout.unthread", Unreached::MenuPathPending),
    ("mixer.add_effect", Unreached::MenuPathPending),
    ("mixer.add_send", Unreached::MenuPathPending),
    ("mixer.effect_enabled", Unreached::MenuPathPending),
    ("mixer.effect_param", Unreached::MenuPathPending),
    ("mixer.master_volume", Unreached::MenuPathPending),
    ("mixer.move_effect", Unreached::MenuPathPending),
    ("mixer.mute", Unreached::MenuPathPending),
    ("mixer.open", Unreached::MenuPathPending),
    ("mixer.output", Unreached::MenuPathPending),
    ("mixer.pan", Unreached::MenuPathPending),
    ("mixer.remove_effect", Unreached::MenuPathPending),
    ("mixer.remove_send", Unreached::MenuPathPending),
    ("mixer.render", Unreached::MenuPathPending),
    ("mixer.send", Unreached::MenuPathPending),
    ("mixer.solo", Unreached::MenuPathPending),
    ("mixer.state", Unreached::NoMenuHome),
    ("mixer.volume", Unreached::MenuPathPending),
    ("monitor.bind_program_media", Unreached::MenuPathPending),
    ("monitor.key", Unreached::MenuPathPending),
    ("monitor.open", Unreached::MenuPathPending),
    ("monitor.open_source_media", Unreached::MenuPathPending),
    ("monitor.source_edit", Unreached::MenuPathPending),
    ("monitor.source_mark", Unreached::MenuPathPending),
    ("multitrack.clip_gain", Unreached::MenuPathPending),
    ("multitrack.lane", Unreached::MenuPathPending),
    ("multitrack.lane_cycle", Unreached::MenuPathPending),
    ("multitrack.lane_point_add", Unreached::MenuPathPending),
    ("multitrack.lane_point_delete", Unreached::MenuPathPending),
    ("multitrack.lane_point_move", Unreached::MenuPathPending),
    ("multitrack.lane_show", Unreached::MenuPathPending),
    ("multitrack.open", Unreached::MenuPathPending),
    ("multitrack.waveforms", Unreached::MenuPathPending),
    ("node.clear_explicit_variable_mode_for_collection", Unreached::MenuPathPending),
    ("node.set_explicit_variable_mode_for_collection", Unreached::MenuPathPending),
    ("page.add", Unreached::MenuPathPending),
    ("page.delete", Unreached::MenuPathPending),
    ("page.rename", Unreached::MenuPathPending),
    ("page.resize", Unreached::MenuPathPending),
    ("panel.align_distribute.align", Unreached::MenuPathPending),
    ("panel.align_distribute.distribute", Unreached::MenuPathPending),
    ("panel.appearance.clear_appearance", Unreached::MenuPathPending),
    ("panel.appearance.reduce_to_basic_appearance", Unreached::MenuPathPending),
    ("panel.appearance.set_fill", Unreached::MenuPathPending),
    ("panel.audio_batch.batch_add", Unreached::MenuPathPending),
    ("panel.audio_batch.batch_clear", Unreached::MenuPathPending),
    ("panel.audio_batch.batch_run", Unreached::MenuPathPending),
    ("panel.brushes.add_brush", Unreached::MenuPathPending),
    ("panel.brushes.apply_brush", Unreached::MenuPathPending),
    ("panel.brushes.remove_brush", Unreached::MenuPathPending),
    ("panel.envelope_distort.expand_envelope", Unreached::MenuPathPending),
    ("panel.envelope_distort.make_envelope", Unreached::MenuPathPending),
    ("panel.envelope_distort.release_envelope", Unreached::MenuPathPending),
    ("panel.graphic_styles.add_graphic_style", Unreached::MenuPathPending),
    ("panel.graphic_styles.apply_graphic_style", Unreached::MenuPathPending),
    ("panel.graphic_styles.break_graphic_style", Unreached::MenuPathPending),
    ("panel.graphic_styles.merge_graphic_style", Unreached::MenuPathPending),
    ("panel.graphic_styles.redefine_graphic_style", Unreached::MenuPathPending),
    ("panel.masks.make_clip", Unreached::MenuPathPending),
    ("panel.masks.make_mask", Unreached::MenuPathPending),
    ("panel.masks.release_masks", Unreached::MenuPathPending),
    ("panel.path_operations.bind_live_text", Unreached::MenuPathPending),
    ("panel.path_operations.convert_text_kind", Unreached::MenuPathPending),
    ("panel.path_operations.edit_path", Unreached::MenuPathPending),
    ("panel.path_operations.expand_shape", Unreached::MenuPathPending),
    ("panel.path_operations.offset_path", Unreached::MenuPathPending),
    ("panel.path_operations.outline_stroke", Unreached::MenuPathPending),
    ("panel.path_operations.promote_network", Unreached::MenuPathPending),
    ("panel.stroke.set_stroke", Unreached::MenuPathPending),
    ("panel.stroke.set_stroke_markers", Unreached::MenuPathPending),
    ("panel.stroke.swap_stroke_markers", Unreached::MenuPathPending),
    ("panel.swatches.add_swatch", Unreached::MenuPathPending),
    ("panel.swatches.apply_swatch", Unreached::MenuPathPending),
    ("panel.swatches.detach_swatch", Unreached::MenuPathPending),
    ("panel.transform.transform", Unreached::MenuPathPending),
    ("panel.transform.transform_each", Unreached::MenuPathPending),
    ("panel.vector_export.export_pdf", Unreached::MenuPathPending),
    ("panel.vector_export.export_svg", Unreached::MenuPathPending),
    ("panel.vector_import.import_vector", Unreached::MenuPathPending),
    ("photo.clear_develop_export_stage", Unreached::MenuPathPending),
    ("photo.clear_selection", Unreached::MenuPathPending),
    ("photo.develop.add_brush_dabs", Unreached::MenuPathPending),
    ("photo.develop.add_colour_range", Unreached::MenuPathPending),
    ("photo.develop.add_correction", Unreached::MenuPathPending),
    ("photo.develop.add_distraction", Unreached::MenuPathPending),
    ("photo.develop.add_luminance_range", Unreached::MenuPathPending),
    ("photo.develop.add_mask", Unreached::MenuPathPending),
    ("photo.develop.add_point_colour", Unreached::MenuPathPending),
    ("photo.develop.add_spot", Unreached::MenuPathPending),
    ("photo.develop.apply_preset", Unreached::MenuPathPending),
    ("photo.develop.as_shot_white", Unreached::MenuPathPending),
    ("photo.develop.clear_enable", Unreached::MenuPathPending),
    ("photo.develop.clear_lens_profile", Unreached::MenuPathPending),
    ("photo.develop.clear_look", Unreached::MenuPathPending),
    ("photo.develop.grain_seed", Unreached::MenuPathPending),
    ("photo.develop.grayscale", Unreached::MenuPathPending),
    ("photo.develop.kelvin", Unreached::MenuPathPending),
    ("photo.develop.lens_profile", Unreached::MenuPathPending),
    ("photo.develop.lens_toggles", Unreached::MenuPathPending),
    ("photo.develop.look", Unreached::MenuPathPending),
    ("photo.develop.mask_flags", Unreached::MenuPathPending),
    ("photo.develop.move_spot_source", Unreached::MenuPathPending),
    ("photo.develop.remove_correction", Unreached::MenuPathPending),
    ("photo.develop.remove_distraction", Unreached::MenuPathPending),
    ("photo.develop.remove_mask", Unreached::MenuPathPending),
    ("photo.develop.remove_point_colour", Unreached::MenuPathPending),
    ("photo.develop.remove_spot", Unreached::MenuPathPending),
    ("photo.develop.save_preset", Unreached::MenuPathPending),
    ("photo.develop.set", Unreached::MenuPathPending),
    ("photo.develop.set_crop", Unreached::MenuPathPending),
    ("photo.develop.set_enable", Unreached::MenuPathPending),
    ("photo.develop.set_many", Unreached::MenuPathPending),
    ("photo.develop.set_point_curve", Unreached::MenuPathPending),
    ("photo.develop.set_spot", Unreached::MenuPathPending),
    ("photo.develop.snapshot.apply", Unreached::MenuPathPending),
    ("photo.develop.snapshot.delete", Unreached::MenuPathPending),
    ("photo.develop.snapshot.save", Unreached::MenuPathPending),
    ("photo.develop.upright", Unreached::MenuPathPending),
    ("photo.develop.upright_guides", Unreached::MenuPathPending),
    ("photo.develop.vignette_style", Unreached::MenuPathPending),
    ("photo.export", Unreached::MenuPathPending),
    ("photo.export_document", Unreached::MenuPathPending),
    ("photo.fill_click", Unreached::MenuPathPending),
    ("photo.fill_drag", Unreached::MenuPathPending),
    ("photo.fill_gradient", Unreached::MenuPathPending),
    ("photo.fill_selection", Unreached::MenuPathPending),
    ("photo.flood_fill", Unreached::MenuPathPending),
    ("photo.info.add_sampler", Unreached::MenuPathPending),
    ("photo.info.move_sampler", Unreached::MenuPathPending),
    ("photo.info.remove_sampler", Unreached::MenuPathPending),
    ("photo.info.set_model", Unreached::MenuPathPending),
    ("photo.info.set_sample_scope", Unreached::MenuPathPending),
    ("photo.info.set_sample_size", Unreached::MenuPathPending),
    ("photo.info.set_sampler_model", Unreached::MenuPathPending),
    ("photo.info_eyedropper", Unreached::MenuPathPending),
    ("photo.liquify", Unreached::MenuPathPending),
    ("photo.load_develop_watermark", Unreached::MenuPathPending),
    ("photo.paint", Unreached::MenuPathPending),
    ("photo.ps_click", Unreached::MenuPathPending),
    ("photo.refine_end", Unreached::MenuPathPending),
    ("photo.refine_stroke", Unreached::MenuPathPending),
    ("photo.select_ellipse", Unreached::MenuPathPending),
    ("photo.select_lasso", Unreached::MenuPathPending),
    ("photo.select_magnetic", Unreached::MenuPathPending),
    ("photo.select_quick", Unreached::MenuPathPending),
    ("photo.select_rect", Unreached::MenuPathPending),
    ("photo.select_wand", Unreached::MenuPathPending),
    ("photo.set_brush", Unreached::MenuPathPending),
    ("photo.set_combine", Unreached::MenuPathPending),
    ("photo.set_develop", Unreached::MenuPathPending),
    ("photo.set_develop_export_field", Unreached::MenuPathPending),
    ("photo.set_develop_threads", Unreached::MenuPathPending),
    ("photo.set_tool", Unreached::MenuPathPending),
    ("photo.undo_selection", Unreached::MenuPathPending),
    ("project_bin.import", Unreached::MenuPathPending),
    ("project_bin.move", Unreached::MenuPathPending),
    ("project_bin.new_folder", Unreached::MenuPathPending),
    ("project_bin.remove", Unreached::MenuPathPending),
    ("proof.custom", Unreached::MenuPathPending),
    ("proof.proof_apply_draft", Unreached::MenuPathPending),
    ("proof.proof_close_page", Unreached::MenuPathPending),
    ("proof.proof_set_save_name", Unreached::MenuPathPending),
    ("proof.save_preset", Unreached::MenuPathPending),
    ("proof.select", Unreached::MenuPathPending),
    ("proof.select_colour_vision", Unreached::MenuPathPending),
    ("proof.set_gamut_style", Unreached::MenuPathPending),
    ("proof.set_gamut_warning", Unreached::MenuPathPending),
    ("proof.set_proof_colours", Unreached::MenuPathPending),
    ("property.disable_animation", Unreached::MenuPathPending),
    ("property.enable_animation", Unreached::MenuPathPending),
    ("property.read", Unreached::NoMenuHome),
    ("property.set_value", Unreached::MenuPathPending),
    ("prototype.advance", Unreached::MenuPathPending),
    ("prototype.capture_frame", Unreached::MenuPathPending),
    ("prototype.click", Unreached::MenuPathPending),
    ("prototype.dispatch_trigger", Unreached::MenuPathPending),
    ("prototype.end_session", Unreached::MenuPathPending),
    ("prototype.hover", Unreached::MenuPathPending),
    ("prototype.read_runtime_variables", Unreached::NoMenuHome),
    ("prototype.restart", Unreached::MenuPathPending),
    ("prototype.start_session", Unreached::MenuPathPending),
    ("prototype.state", Unreached::NoMenuHome),
    ("sequence.add_tracks", Unreached::MenuPathPending),
    ("sequence.new", Unreached::MenuPathPending),
    ("sequence.place_clip", Unreached::MenuPathPending),
    ("shell.close", Unreached::MenuPathPending),
    ("shell.dialog_accept", Unreached::MenuPathPending),
    ("shell.dialog_cancel", Unreached::MenuPathPending),
    ("shell.help_close", Unreached::MenuPathPending),
    ("shell.help_search", Unreached::MenuPathPending),
    ("spectral.apply", Unreached::MenuPathPending),
    ("spectral.open", Unreached::MenuPathPending),
    ("spectral.set_region", Unreached::MenuPathPending),
    ("spectral.state", Unreached::NoMenuHome),
    ("timeline.back_to_parent", Unreached::MenuPathPending),
    ("timeline.clip_nudge", Unreached::MenuPathPending),
    ("timeline.clip_track_nudge", Unreached::MenuPathPending),
    ("timeline.composite_preview", Unreached::MenuPathPending),
    ("timeline.edit_next", Unreached::MenuPathPending),
    ("timeline.edit_previous", Unreached::MenuPathPending),
    ("timeline.extend_edit", Unreached::MenuPathPending),
    ("timeline.lift", Unreached::MenuPathPending),
    ("timeline.make_subsequence", Unreached::MenuPathPending),
    ("timeline.marker_add", Unreached::MenuPathPending),
    ("timeline.marker_next", Unreached::MenuPathPending),
    ("timeline.marker_previous", Unreached::MenuPathPending),
    ("timeline.move_clip", Unreached::MenuPathPending),
    ("timeline.multi_frame_step", Unreached::MenuPathPending),
    ("timeline.nest", Unreached::MenuPathPending),
    ("timeline.open_nested", Unreached::MenuPathPending),
    ("timeline.razor", Unreached::MenuPathPending),
    ("timeline.ripple_delete", Unreached::MenuPathPending),
    ("timeline.ripple_trim_in", Unreached::MenuPathPending),
    ("timeline.ripple_trim_out", Unreached::MenuPathPending),
    ("timeline.roll", Unreached::MenuPathPending),
    ("timeline.scroll", Unreached::MenuPathPending),
    ("timeline.scrub", Unreached::MenuPathPending),
    ("timeline.select", Unreached::MenuPathPending),
    ("timeline.slide", Unreached::MenuPathPending),
    ("timeline.slide_nudge", Unreached::MenuPathPending),
    ("timeline.slip", Unreached::MenuPathPending),
    ("timeline.slip_nudge", Unreached::MenuPathPending),
    ("timeline.snap", Unreached::MenuPathPending),
    ("timeline.trim_in", Unreached::MenuPathPending),
    ("timeline.trim_nudge", Unreached::MenuPathPending),
    ("timeline.trim_out", Unreached::MenuPathPending),
    ("timeline.trim_session_commit", Unreached::MenuPathPending),
    ("timeline.trim_session_enter", Unreached::MenuPathPending),
    ("timeline.trim_session_rollback", Unreached::MenuPathPending),
    ("timeline.trim_session_type", Unreached::MenuPathPending),
    ("timeline.zoom_fit", Unreached::MenuPathPending),
    ("timeline.zoom_in", Unreached::MenuPathPending),
    ("timeline.zoom_out", Unreached::MenuPathPending),
    ("transform.free_transform", Unreached::MenuPathPending),
    ("transform.move", Unreached::MenuPathPending),
    ("transform.reset", Unreached::MenuPathPending),
    ("variable.create", Unreached::MenuPathPending),
    ("variable.create_alias", Unreached::NoMenuHome),
    ("variable.remove", Unreached::MenuPathPending),
    ("variable.remove_code_syntax", Unreached::MenuPathPending),
    ("variable.resolve_for_consumer", Unreached::NoMenuHome),
    ("variable.set_code_syntax", Unreached::MenuPathPending),
    ("variable.set_value_for_mode", Unreached::MenuPathPending),
    ("variable_collection.add_mode", Unreached::MenuPathPending),
    ("variable_collection.create", Unreached::MenuPathPending),
    ("variable_collection.remove", Unreached::MenuPathPending),
    ("variable_collection.remove_mode", Unreached::MenuPathPending),
    ("variable_collection.rename_mode", Unreached::MenuPathPending),
    ("vector.add_folio_swatch", Unreached::MenuPathPending),
    ("vector.add_font", Unreached::MenuPathPending),
    ("vector.add_graphic_style", Unreached::MenuPathPending),
    ("vector.add_swatch", Unreached::MenuPathPending),
    ("vector.align_layers", Unreached::MenuPathPending),
    ("vector.apply_brush", Unreached::MenuPathPending),
    ("vector.apply_graphic_style", Unreached::MenuPathPending),
    ("vector.apply_swatch", Unreached::MenuPathPending),
    ("vector.apply_swatch_tinted", Unreached::MenuPathPending),
    ("vector.bind_live_text", Unreached::MenuPathPending),
    ("vector.bind_live_text_with_font", Unreached::MenuPathPending),
    ("vector.blob_brush_stroke", Unreached::MenuPathPending),
    ("vector.break_graphic_style", Unreached::MenuPathPending),
    ("vector.clear_appearance", Unreached::MenuPathPending),
    ("vector.clear_width", Unreached::MenuPathPending),
    ("vector.convert_text_kind", Unreached::MenuPathPending),
    ("vector.create_gradient_mesh", Unreached::MenuPathPending),
    ("vector.create_outlines", Unreached::MenuPathPending),
    ("vector.curvature_point", Unreached::MenuPathPending),
    ("vector.detach_swatch", Unreached::MenuPathPending),
    ("vector.direct_drag", Unreached::MenuPathPending),
    ("vector.distribute_layers", Unreached::MenuPathPending),
    ("vector.draw_shape", Unreached::MenuPathPending),
    ("vector.edit_folio_swatch", Unreached::MenuPathPending),
    ("vector.edit_gradient_mesh", Unreached::MenuPathPending),
    ("vector.edit_network", Unreached::MenuPathPending),
    ("vector.edit_path", Unreached::MenuPathPending),
    ("vector.edit_swatch", Unreached::MenuPathPending),
    ("vector.expand_envelope", Unreached::MenuPathPending),
    ("vector.expand_shape", Unreached::MenuPathPending),
    ("vector.export_vector_pdf", Unreached::MenuPathPending),
    ("vector.export_vector_svg", Unreached::MenuPathPending),
    ("vector.knife_cut", Unreached::MenuPathPending),
    ("vector.layer_bounds", Unreached::MenuPathPending),
    ("vector.live_filter_apply", Unreached::MenuPathPending),
    ("vector.live_filter_bake", Unreached::MenuPathPending),
    ("vector.live_filter_read", Unreached::MenuPathPending),
    ("vector.live_filter_remove", Unreached::MenuPathPending),
    ("vector.live_filter_reorder", Unreached::MenuPathPending),
    ("vector.live_filter_toggle", Unreached::MenuPathPending),
    ("vector.live_filter_update", Unreached::MenuPathPending),
    ("vector.make_clip", Unreached::MenuPathPending),
    ("vector.make_envelope", Unreached::MenuPathPending),
    ("vector.make_vector_mask", Unreached::MenuPathPending),
    ("vector.merge_graphic_style", Unreached::MenuPathPending),
    ("vector.move_envelope_point", Unreached::MenuPathPending),
    ("vector.network_paint_bucket", Unreached::MenuPathPending),
    ("vector.network_pen", Unreached::MenuPathPending),
    ("vector.network_region_swatch", Unreached::MenuPathPending),
    ("vector.offset_path_layer", Unreached::MenuPathPending),
    ("vector.outline_stroke_layer", Unreached::MenuPathPending),
    ("vector.pathfinder_layers", Unreached::MenuPathPending),
    ("vector.pen_end", Unreached::MenuPathPending),
    ("vector.pen_point", Unreached::MenuPathPending),
    ("vector.pencil_stroke", Unreached::MenuPathPending),
    ("vector.place_text_with_font", Unreached::MenuPathPending),
    ("vector.promote_network", Unreached::MenuPathPending),
    ("vector.recolor_artwork", Unreached::MenuPathPending),
    ("vector.redefine_graphic_style", Unreached::MenuPathPending),
    ("vector.reduce_to_basic_appearance", Unreached::MenuPathPending),
    ("vector.release_envelope", Unreached::MenuPathPending),
    ("vector.release_masks", Unreached::MenuPathPending),
    ("vector.remove_brush", Unreached::MenuPathPending),
    ("vector.reshape_drag", Unreached::MenuPathPending),
    ("vector.set_document_cmyk_profile", Unreached::MenuPathPending),
    ("vector.set_fill", Unreached::MenuPathPending),
    ("vector.set_live_shape", Unreached::MenuPathPending),
    ("vector.set_stroke", Unreached::MenuPathPending),
    ("vector.set_stroke_markers", Unreached::MenuPathPending),
    ("vector.set_tool_options", Unreached::MenuPathPending),
    ("vector.set_width_point", Unreached::MenuPathPending),
    ("vector.shape_builder_layers", Unreached::MenuPathPending),
    ("vector.swap_stroke_markers", Unreached::MenuPathPending),
    ("vector.text_click", Unreached::MenuPathPending),
    ("vector.text_on_path_to_outlines", Unreached::MenuPathPending),
    ("vector.transform_each_layers", Unreached::MenuPathPending),
    ("vector.transform_layers", Unreached::MenuPathPending),
    ("vector.width_drag", Unreached::MenuPathPending),
    ("video.project_add_tracks", Unreached::MenuPathPending),
    ("video.project_bind", Unreached::MenuPathPending),
    ("video.project_import", Unreached::MenuPathPending),
    ("video.project_move_items", Unreached::MenuPathPending),
    ("video.project_new_folder", Unreached::MenuPathPending),
    ("video.project_place_item", Unreached::MenuPathPending),
    ("video.project_remove_items", Unreached::MenuPathPending),
    ("waveform.amplify", Unreached::MenuPathPending),
    ("waveform.copy", Unreached::MenuPathPending),
    ("waveform.cut", Unreached::MenuPathPending),
    ("waveform.delete", Unreached::MenuPathPending),
    ("waveform.effect", Unreached::MenuPathPending),
    ("waveform.fade", Unreached::MenuPathPending),
    ("waveform.gain_envelope", Unreached::MenuPathPending),
    ("waveform.insert_silence", Unreached::MenuPathPending),
    ("waveform.invert", Unreached::MenuPathPending),
    ("waveform.noise_reduce", Unreached::MenuPathPending),
    ("waveform.normalize", Unreached::MenuPathPending),
    ("waveform.paste", Unreached::MenuPathPending),
    ("waveform.peaks", Unreached::MenuPathPending),
    ("waveform.reverse", Unreached::MenuPathPending),
    ("waveform.select", Unreached::MenuPathPending),
    ("waveform.silence", Unreached::MenuPathPending),
    ("waveform.snap", Unreached::MenuPathPending),
    ("waveform.state", Unreached::NoMenuHome),
    ("web.apply_comment", Unreached::MenuPathPending),
    ("web.assets", Unreached::MenuPathPending),
    ("web.balance_braces", Unreached::MenuPathPending),
    ("web.change_link", Unreached::MenuPathPending),
    ("web.code_hints", Unreached::NoMenuHome),
    ("web.code_navigator", Unreached::NoMenuHome),
    ("web.collapse_full_tag", Unreached::MenuPathPending),
    ("web.collapse_selection", Unreached::MenuPathPending),
    ("web.css_properties", Unreached::NoMenuHome),
    ("web.css_rule", Unreached::MenuPathPending),
    ("web.document_types", Unreached::NoMenuHome),
    ("web.export", Unreached::MenuPathPending),
    ("web.export_recipe", Unreached::MenuPathPending),
    ("web.format_source", Unreached::MenuPathPending),
    ("web.indent", Unreached::MenuPathPending),
    ("web.insert_element", Unreached::MenuPathPending),
    ("web.insert_fragment", Unreached::MenuPathPending),
    ("web.instance_param", Unreached::MenuPathPending),
    ("web.instance_repeat", Unreached::MenuPathPending),
    ("web.library_item_insert", Unreached::MenuPathPending),
    ("web.library_item_update", Unreached::MenuPathPending),
    ("web.lint", Unreached::MenuPathPending),
    ("web.media", Unreached::MenuPathPending),
    ("web.media_records", Unreached::NoMenuHome),
    ("web.move_file", Unreached::MenuPathPending),
    ("web.open", Unreached::MenuPathPending),
    ("web.outdent", Unreached::MenuPathPending),
    ("web.preview", Unreached::MenuPathPending),
    ("web.publish", Unreached::MenuPathPending),
    ("web.read", Unreached::NoMenuHome),
    ("web.redo", Unreached::MenuPathPending),
    ("web.regions", Unreached::NoMenuHome),
    ("web.remote_target", Unreached::MenuPathPending),
    ("web.remove_attribute", Unreached::MenuPathPending),
    ("web.remove_comment", Unreached::MenuPathPending),
    ("web.remove_node", Unreached::MenuPathPending),
    ("web.save", Unreached::MenuPathPending),
    ("web.select_parent_tag", Unreached::MenuPathPending),
    ("web.set_attribute", Unreached::MenuPathPending),
    ("web.set_doctype", Unreached::MenuPathPending),
    ("web.set_text", Unreached::MenuPathPending),
    ("web.site", Unreached::MenuPathPending),
    ("web.site_report", Unreached::MenuPathPending),
    ("web.snippet", Unreached::MenuPathPending),
    ("web.snippet_insert", Unreached::MenuPathPending),
    ("web.snippets", Unreached::NoMenuHome),
    ("web.synchronize", Unreached::MenuPathPending),
    ("web.tag_vocabulary", Unreached::NoMenuHome),
    ("web.template_instance", Unreached::MenuPathPending),
    ("web.template_regions", Unreached::NoMenuHome),
    ("web.template_update", Unreached::MenuPathPending),
    ("web.undo", Unreached::MenuPathPending),
    ("web.validate", Unreached::MenuPathPending),
    ("web.verify", Unreached::NoMenuHome),
];

/// The menu home of every registry domain (MENU-INV-3: every registry domain is reachable from the menu tree): the
/// designed node its commands belong under, whether or not a host runs those leaves yet.
pub const DOMAIN_HOMES: &[(&str, &str)] = &[
    ("action", "studio.automate.actions"),
    ("adjustment", "studio.effect.adjustment"),
    ("artboard", "studio.document.artboards"),
    ("asset", "studio.interop.place"),
    ("audio", "studio.motion.audio"),
    ("board", "studio.document.boards"),
    ("catalog", "studio.automate.command_api"),
    ("command", "studio.automate.command_api"),
    ("component", "studio.object.component"),
    ("component_set", "studio.object.component"),
    ("composition", "studio.motion.composition"),
    ("delivery", "studio.motion.render_preview"),
    ("design", "studio.object.auto_layout"),
    ("document", "document"),
    ("effect", "studio.effect.apply"),
    ("export", "studio.interop.recipe"),
    ("expression", "studio.motion.expression"),
    ("history", "studio.history"),
    ("instance", "studio.object.component"),
    ("keyframe", "studio.motion.keyframe"),
    ("layer", "studio.object.layer"),
    ("layout", "studio.document.pages_spreads"),
    ("mixer", "studio.motion.audio"),
    ("monitor", "studio.motion.playback"),
    ("multitrack", "studio.motion.tracks"),
    ("node", "studio.color.colour_variables"),
    ("page", "studio.document.pages_spreads"),
    ("panel", "studio.workspace.panel"),
    ("photo", "studio.document.develop"),
    ("project_bin", "studio.insert.media"),
    ("proof", "studio.color.proof"),
    ("property", "studio.motion.keyframe"),
    ("prototype", "studio.motion.prototype"),
    ("sequence", "studio.motion.timeline_doc"),
    ("shell", "workspace"),
    ("spectral", "studio.motion.audio"),
    ("timeline", "studio.motion.edit_actions"),
    ("transform", "studio.object.transform"),
    ("variable", "studio.color.colour_variables"),
    ("variable_collection", "studio.color.colour_variables"),
    ("vector", "studio.object.path"),
    ("video", "studio.motion.timeline_doc"),
    ("waveform", "studio.motion.audio"),
    ("web", "studio.web.site"),
];

// ---------------------------------------------------------------------------------------------------------------
// The template and the evaluated bar.

/// What the tree text says about a node beyond its descriptor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    /// An expansion node ([STU-SHL-038]): leaves come from a registry, not from the tree.
    pub expansion: bool,
    /// The id was derived from the label (module docs).
    pub derived: bool,
    /// A placeholder for generated leaves (`*(…, generated)*` in the spec).
    pub generated: bool,
    /// An enumeration the spec counts without listing.
    pub enumeration: bool,
    /// An alias appearance the spec marks (INSERT's alias leaves).
    pub alias: bool,
    /// The clause of [STU-SHL-023] to [STU-SHL-037] that designs it (its title's); 0 for the shell titles.
    pub clause: u16,
}

/// The parsed designed tree: every node AVAILABLE, plus [`Meta`] by id.
pub struct Template {
    pub bar: MenuBar,
    pub meta: BTreeMap<String, Meta>,
}

/// The designed tree: `kind|id|label|flags`, two spaces of indent per level; kinds T title, S submenu, I item,
/// E expansion; flags d derived id, m menu-only, a alias, g generated leaves, n enumeration not listed. Generated
/// from [STU-SHL-023] to [STU-SHL-037] of the Master Spec v02.229 (an expansion's inline "includes" list is its leaves).
pub const TREE: &str = r#"
T|file|FILE|
  S|studio.document.new|New|
    I|studio.document.new.blank_document|Blank Document…|d
    I|studio.document.new.from_document_preset|From Document Preset…|d
    I|studio.document.new.from_template|From Template…|d
    I|studio.document.new.from_clipboard|From Clipboard|d
    I|studio.document.new.from_asset_library|From Asset Library…|d
    I|studio.document.new.duplicate_of_current_document|Duplicate of Current Document|d
    I|studio.document.new.document_presets|Document Presets…|d
  I|studio.document.open|Open…|
  S|studio.document.open_recent|Open Recent|
    E|studio.document.open_recent.recent_documents|Recent documents|dg
  I|studio.document.open_from_asset_library|Open From Asset Library…|
  I|studio.document.browse_assets|Browse Assets…|
  I|studio.document.close|Close|
  I|studio.document.close_all|Close All|
  I|studio.document.close_others|Close Others|
  I|studio.document.save|Save|
  I|studio.document.save_as|Save As…|
  I|studio.document.save_copy|Save a Copy…|
  I|studio.document.save_template|Save as Template…|
  I|studio.document.save_all|Save All|
  I|studio.document.revert|Revert|
  S|studio.interop.place|Place|
    I|studio.interop.place.place_linked|Place Linked…|d
    I|studio.interop.place.place_embedded|Place Embedded…|d
    I|studio.interop.place.place_from_asset_library|Place from Asset Library…|d
    I|studio.interop.place.place_multiple|Place Multiple…|d
    I|studio.interop.place.relink|Relink…|d
    I|studio.interop.place.relink_to_folder|Relink to Folder…|d
    I|studio.interop.place.update_all_links|Update All Links|d
    I|studio.interop.place.embed_link|Embed Link|d
    I|studio.interop.place.unembed|Unembed…|d
    I|studio.interop.place.edit_original|Edit Original|d
    I|studio.interop.place.go_to_link|Go to Link|d
    I|studio.interop.place.link_info|Link Info…|d
  E|studio.interop.import|Import|
  E|studio.interop.export|Export|
  S|studio.interop.recipe|Export Recipes|
    I|studio.interop.recipe.manage_export_recipes|Manage Export Recipes…|d
    I|studio.interop.recipe.save_current_as_recipe|Save Current as Recipe…|d
    I|studio.interop.recipe.run_recipe|Run Recipe…|d
    I|studio.interop.recipe.export_queue|Export Queue|d
  S|studio.web.publish|Publish|
    I|studio.web.publish.publish_site|Publish Site…|d
    I|studio.web.publish.put_selected_files|Put Selected Files|d
    I|studio.web.publish.get_selected_files|Get Selected Files|d
    I|studio.web.publish.check_in|Check In|d
    I|studio.web.publish.check_out|Check Out|d
    I|studio.web.publish.undo_check_out|Undo Check Out|d
    I|studio.web.publish.synchronize_sitewide|Synchronize Sitewide…|d
    I|studio.web.publish.cloaking|Cloaking|d
  S|studio.web.site|Site Management|
    I|studio.web.site.new_site|New Site…|d
    I|studio.web.site.manage_sites|Manage Sites…|d
    I|studio.web.site.site_setup|Site Setup…|d
    I|studio.web.site.server_setup|Server Setup…|d
    I|studio.web.site.site_reports|Site Reports…|d
    I|studio.web.site.check_links_sitewide|Check Links Sitewide|d
    I|studio.web.site.change_link_sitewide|Change Link Sitewide…|d
    I|studio.web.site.recreate_site_cache|Recreate Site Cache|d
    I|studio.web.site.advanced|Advanced|d
  S|studio.layout.package|Package|
    I|studio.layout.package.package_document|Package Document…|d
    I|studio.layout.package.package_options|Package Options…|d
    I|studio.layout.package.copy_linked_assets|Copy Linked Assets|d
    I|studio.layout.package.copy_fonts|Copy Fonts|d
    I|studio.layout.package.include_report|Include Report|d
    I|studio.layout.package.package_for_print_provider|Package for Print Provider…|d
  S|studio.layout.preflight|Preflight|
    I|studio.layout.preflight.run_preflight|Run Preflight|d
    I|studio.layout.preflight.preflight_profiles|Preflight Profiles…|d
    I|studio.layout.preflight.define_profile|Define Profile…|d
    I|studio.layout.preflight.embed_profile_in_document|Embed Profile in Document|d
    I|studio.layout.preflight.preflight_report|Preflight Report…|d
    I|studio.layout.preflight.preflight_panel|Preflight Panel|d
  S|studio.layout.print|Print|
    I|studio.layout.print.print|Print…|d
    I|studio.layout.print.print_presets|Print Presets…|d
    I|studio.layout.print.print_booklet|Print Booklet…|d
    I|studio.layout.print.print_tiles|Print Tiles…|d
    I|studio.layout.print.printer_marks_bleed|Printer Marks & Bleed…|d
    I|studio.layout.print.page_setup|Page Setup…|d
    I|studio.layout.print.print_selection_only|Print Selection Only|d
  S|studio.document.info|Document Info|
    I|studio.document.info.document_properties|Document Properties…|d
    I|studio.document.info.file_metadata|File Metadata…|d
    I|studio.document.info.document_statistics|Document Statistics…|d
    I|studio.document.info.fonts_used|Fonts Used…|d
    I|studio.document.info.links_used|Links Used…|d
    I|studio.document.info.colours_used|Colours Used…|d
  S|studio.history.version|Version History|
    I|studio.history.version.browse_versions|Browse Versions…|d
    I|studio.history.version.create_named_version|Create Named Version…|d
    I|studio.history.version.restore_version|Restore Version…|d
    I|studio.history.version.compare_versions|Compare Versions…|d
    I|studio.history.version.ledger_receipts_for_this_document|Ledger Receipts for This Document…|d
T|edit|EDIT|
  I|studio.history.undo|Undo|
  I|studio.history.redo|Redo|
  S|studio.history|History|
    I|studio.history.step_backward|Step Backward|d
    I|studio.history.step_forward|Step Forward|d
    I|studio.history.history_panel|History Panel|d
    I|studio.history.new_snapshot|New Snapshot…|d
    I|studio.history.delete_snapshot|Delete Snapshot|d
    I|studio.history.restore_snapshot|Restore Snapshot|d
    I|studio.history.purge_history|Purge History|d
    I|studio.history.history_options|History Options…|d
  I|studio.clipboard.cut|Cut|
  I|studio.clipboard.copy|Copy|
  I|studio.clipboard.copy_merged|Copy Merged|
  S|studio.clipboard.copy_as|Copy As|
    I|studio.clipboard.copy_as.svg|SVG|d
    I|studio.clipboard.copy_as.css|CSS|d
    I|studio.clipboard.copy_as.png|PNG|d
    I|studio.clipboard.copy_as.code|Code|d
    I|studio.clipboard.copy_as.command_json|Command JSON|d
    I|studio.clipboard.copy_as.object_id|Object Id|d
  I|studio.clipboard.paste|Paste|
  S|studio.clipboard.paste_special|Paste Special|
    I|studio.clipboard.paste_special.paste_in_place|Paste in Place|d
    I|studio.clipboard.paste_special.paste_in_front|Paste in Front|d
    I|studio.clipboard.paste_special.paste_behind|Paste Behind|d
    I|studio.clipboard.paste_special.paste_into|Paste Into|d
    I|studio.clipboard.paste_special.paste_outside|Paste Outside|d
    I|studio.clipboard.paste_special.paste_without_formatting|Paste Without Formatting|d
    I|studio.clipboard.paste_special.paste_on_all_artboards|Paste on All Artboards|d
    I|studio.clipboard.paste_special.paste_attributes|Paste Attributes|d
    I|studio.clipboard.paste_special.paste_effects|Paste Effects|d
    I|studio.clipboard.paste_special.paste_develop_settings|Paste Develop Settings|d
  I|studio.clipboard.copy_property_links|Copy with Property Links|
  I|studio.clipboard.copy_property_links_relative|Copy with Relative Property Links|
  I|studio.clipboard.copy_expression|Copy Expression Only|
  I|studio.clipboard.paste_keyframes_reversed|Paste Reversed Keyframes|
  I|studio.edit.clear|Clear|d
  I|studio.edit.duplicate|Duplicate|d
  I|studio.edit.duplicate_to_document|Duplicate to Document…|d
  I|studio.edit.repeat_last_command|Repeat Last Command|d
  S|studio.find|Find & Replace|
    I|studio.find.find_change|Find / Change…|d
    I|studio.find.find_next|Find Next|d
    I|studio.find.find_previous|Find Previous|d
    I|studio.find.find_in_all_open_documents|Find in All Open Documents|d
    I|studio.find.find_in_site_folder|Find in Site / Folder…|d
    I|studio.find.regex_find|Regex Find…|d
    I|studio.find.find_font|Find Font…|d
    I|studio.find.find_colour|Find Colour…|d
    I|studio.find.find_object|Find Object…|d
    I|studio.find.find_glyph|Find Glyph…|d
    I|studio.find.query_presets|Query Presets…|d
    I|studio.find.recent_searches|Recent Searches|d
    I|studio.find.replace_all_in_selection|Replace All in Selection|d
  S|studio.typography.spelling|Spelling & Language|
    I|studio.typography.spelling.check_spelling|Check Spelling…|d
    I|studio.typography.spelling.dynamic_spelling|Dynamic Spelling|d
    I|studio.typography.spelling.autocorrect|Autocorrect|d
    I|studio.typography.spelling.ignore_spelling|Ignore Spelling|d
    I|studio.typography.spelling.add_to_user_dictionary|Add to User Dictionary|d
    I|studio.typography.spelling.user_dictionary|User Dictionary…|d
    I|studio.typography.spelling.hyphenation_exceptions|Hyphenation Exceptions…|d
    I|studio.typography.spelling.language|Language…|d
  S|studio.layout.changes|Track Changes|
    I|studio.layout.changes.enable_track_changes|Enable Track Changes|d
    I|studio.layout.changes.show_changes|Show Changes|d
    I|studio.layout.changes.accept_change|Accept Change|d
    I|studio.layout.changes.reject_change|Reject Change|d
    I|studio.layout.changes.accept_all_changes|Accept All Changes|d
    I|studio.layout.changes.reject_all_changes|Reject All Changes|d
    I|studio.layout.changes.accept_all_by_author|Accept All by Author…|d
    I|studio.layout.changes.reject_all_by_author|Reject All by Author…|d
    I|studio.layout.changes.next_change|Next Change|d
    I|studio.layout.changes.previous_change|Previous Change|d
    I|studio.layout.changes.change_info|Change Info…|d
  S|studio.layout.notes|Notes & Comments|
    I|studio.layout.notes.new_note|New Note|d
    I|studio.layout.notes.new_comment|New Comment|d
    I|studio.layout.notes.next_note|Next Note|d
    I|studio.layout.notes.previous_note|Previous Note|d
    I|studio.layout.notes.convert_note_to_text|Convert Note to Text|d
    I|studio.layout.notes.remove_all_notes|Remove All Notes|d
    I|studio.layout.notes.resolve_comment|Resolve Comment|d
    I|studio.layout.notes.notes_mode|Notes Mode|d
  S|studio.interop.assignment|Assignments|
    I|studio.interop.assignment.add_selection_to_assignment|Add Selection to Assignment|d
    I|studio.interop.assignment.add_all_stories_to_assignment|Add All Stories to Assignment|d
    I|studio.interop.assignment.add_all_graphics_to_assignment|Add All Graphics to Assignment|d
    I|studio.interop.assignment.add_layer_to_assignment|Add Layer to Assignment|d
    I|studio.interop.assignment.new_assignment|New Assignment…|d
    I|studio.interop.assignment.check_in_check_out|Check In / Check Out|d
    I|studio.interop.assignment.update_assignment|Update Assignment|d
    I|studio.interop.assignment.package_assignment|Package Assignment…|d
  E|studio.app.preferences|Preferences|m
  S|studio.app.customise|Keyboard & Menus|
    I|studio.app.customise.keyboard_shortcuts|Keyboard Shortcuts…|d
    I|studio.app.customise.menu_customisation|Menu Customisation…|d
    I|studio.app.customise.toolbar_customisation|Toolbar Customisation…|d
    I|studio.app.customise.import_shortcut_set|Import Shortcut Set…|d
    I|studio.app.customise.export_shortcut_set|Export Shortcut Set…|d
    I|studio.app.customise.reset_shortcuts_to_default|Reset Shortcuts to Default|d
    I|studio.app.customise.shortcut_conflict_report|Shortcut Conflict Report…|d
    I|studio.app.customise.keyboard_layout|Keyboard Layout…|d
  S|studio.app.capability|Capability Grants|
    I|studio.app.capability.review_capability_grants|Review Capability Grants…|d
    I|studio.app.capability.grant_capability|Grant Capability…|d
    I|studio.app.capability.revoke_capability|Revoke Capability…|d
    I|studio.app.capability.capability_audit_log|Capability Audit Log…|d
T|view|VIEW|
  I|studio.view.zoom_in|Zoom In|d
  I|studio.view.zoom_out|Zoom Out|d
  S|studio.view.zoom_to|Zoom To|
    I|studio.view.zoom_to.fit_page_in_window|Fit Page in Window|d
    I|studio.view.zoom_to.fit_spread_in_window|Fit Spread in Window|d
    I|studio.view.zoom_to.fit_artboard_in_window|Fit Artboard in Window|d
    I|studio.view.zoom_to.fit_all_artboards|Fit All Artboards|d
    I|studio.view.zoom_to.fit_selection_in_window|Fit Selection in Window|d
    I|studio.view.zoom_to.actual_size|Actual Size|d
    I|studio.view.zoom_to.200|200%|d
    I|studio.view.zoom_to.400|400%|d
    I|studio.view.zoom_to.zoom_to|Zoom To…|d
    I|studio.view.zoom_to.zoom_to_pixel_grid|Zoom to Pixel Grid|d
  S|studio.view.screen_mode|Screen Mode|
    I|studio.view.screen_mode.normal|Normal|d
    I|studio.view.screen_mode.preview|Preview|d
    I|studio.view.screen_mode.bleed|Bleed|d
    I|studio.view.screen_mode.slug|Slug|d
    I|studio.view.screen_mode.presentation|Presentation|d
    I|studio.view.screen_mode.full_screen|Full Screen|d
    I|studio.view.screen_mode.trim_view|Trim View|d
  S|studio.view.display|Display Quality|
    I|studio.view.display.fast|Fast|d
    I|studio.view.display.typical|Typical|d
    I|studio.view.display.high_quality|High Quality|d
    I|studio.view.display.pixel_preview|Pixel Preview|d
    I|studio.view.display.outline_wireframe|Outline / Wireframe|d
    I|studio.view.display.overprint_preview|Overprint Preview|d
    I|studio.view.display.gpu_preview|GPU Preview|d
    I|studio.view.display.allow_object_level_display_settings|Allow Object-Level Display Settings|d
    I|studio.view.display.clear_object_level_display_settings|Clear Object-Level Display Settings|d
  S|studio.view.view_layout|View Layout|
    I|studio.view.view_layout.1_view|1 View|d
    I|studio.view.view_layout.2_views|2 Views|d
    I|studio.view.view_layout.4_views|4 Views|d
    I|studio.view.view_layout.share_view_options|Share View Options|d
  S|studio.color.proof|Proof|
    I|studio.color.proof.proof_setup|Proof Setup…|d
    I|studio.color.proof.proof_colours|Proof Colours|d
    I|studio.color.proof.gamut_warning|Gamut Warning|d
    I|studio.color.proof.colour_vision_proof|Colour-Vision Proof…|d
  S|studio.view.rulers|Rulers & Measurement|
    I|studio.view.rulers.show_rulers|Show Rulers|d
    I|studio.view.rulers.ruler_units|Ruler Units…|d
    I|studio.view.rulers.change_ruler_origin|Change Ruler Origin|d
    I|studio.view.rulers.reset_ruler_origin|Reset Ruler Origin|d
    I|studio.view.rulers.video_ruler|Video Ruler|d
    I|studio.view.rulers.measurement_tool_overlay|Measurement Tool Overlay|d
  S|studio.view.guides|Guides|
    I|studio.view.guides.show_guides|Show Guides|d
    I|studio.view.guides.lock_guides|Lock Guides|d
    I|studio.view.guides.clear_guides|Clear Guides|d
    I|studio.view.guides.new_guide|New Guide…|d
    I|studio.view.guides.new_guides_from_selection|New Guides from Selection|d
    I|studio.view.guides.guides_in_back|Guides in Back|d
    I|studio.view.guides.smart_guides|Smart Guides|d
    I|studio.view.guides.guide_templates|Guide Templates|d
    I|studio.view.guides.manage_guides|Manage Guides…|d
  S|studio.view.grids|Grids|
    I|studio.view.grids.show_document_grid|Show Document Grid|d
    I|studio.view.grids.show_baseline_grid|Show Baseline Grid|d
    I|studio.view.grids.show_layout_grid|Show Layout Grid|d
    I|studio.view.grids.show_pixel_grid|Show Pixel Grid|d
    I|studio.view.grids.snap_to_document_grid|Snap to Document Grid|d
    I|studio.view.grids.snap_to_baseline_grid|Snap to Baseline Grid|d
    I|studio.view.grids.grid_setup|Grid Setup…|d
  S|studio.view.snap|Snap To|
    I|studio.view.snap.snap_to_guides|Snap to Guides|d
    I|studio.view.snap.snap_to_grid|Snap to Grid|d
    I|studio.view.snap.snap_to_objects|Snap to Objects|d
    I|studio.view.snap.snap_to_point|Snap to Point|d
    I|studio.view.snap.snap_to_pixel|Snap to Pixel|d
    I|studio.view.snap.snap_to_glyph|Snap to Glyph|d
    I|studio.view.snap.snap_to_timeline_frames|Snap to Timeline Frames|d
    I|studio.view.snap.snapping_options|Snapping Options…|d
  S|studio.view.extras|Extras|
    I|studio.view.extras.show_extras|Show Extras|d
    I|studio.view.extras.show_edges|Show Edges|d
    I|studio.view.extras.show_frame_edges|Show Frame Edges|d
    I|studio.view.extras.show_text_threads|Show Text Threads|d
    I|studio.view.extras.show_hidden_characters|Show Hidden Characters|d
    I|studio.view.extras.show_bounding_box|Show Bounding Box|d
    I|studio.view.extras.show_transparency_grid|Show Transparency Grid|d
    I|studio.view.extras.show_slices|Show Slices|d
    I|studio.view.extras.show_annotations|Show Annotations|d
    I|studio.view.extras.show_live_corner_widgets|Show Live Corner Widgets|d
    I|studio.view.extras.show_gaps|Show Gaps|d
    I|studio.view.extras.extras_options|Extras Options…|d
  S|studio.view.rotate|Rotate View|
    I|studio.view.rotate.rotate_90_cw|Rotate 90° CW|d
    I|studio.view.rotate.rotate_90_ccw|Rotate 90° CCW|d
    I|studio.view.rotate.rotate_180|Rotate 180°|d
    I|studio.view.rotate.rotate_view|Rotate View…|d
    I|studio.view.rotate.reset_rotation|Reset Rotation|d
  S|studio.code.view_options|Code View Options|
    I|studio.code.view_options.word_wrap|Word Wrap|d
    I|studio.code.view_options.line_numbers|Line Numbers|d
    I|studio.code.view_options.syntax_colouring|Syntax Colouring|d
    I|studio.code.view_options.auto_indent|Auto Indent|d
    I|studio.code.view_options.highlight_invalid_code|Highlight Invalid Code|d
    I|studio.code.view_options.hidden_characters|Hidden Characters|d
    I|studio.code.view_options.code_folding|Code Folding|d
  S|studio.web.preview|Live Preview|
    I|studio.web.preview.live_view|Live View|d
    I|studio.web.preview.real_time_preview|Real-time Preview|d
    I|studio.web.preview.preview_in_browser|Preview in Browser…|d
    I|studio.web.preview.preview_in_secondary_browser|Preview in Secondary Browser|d
    I|studio.web.preview.device_preview|Device Preview…|d
    I|studio.web.preview.responsive_widths|Responsive Widths…|d
    I|studio.web.preview.manage_browsers|Manage Browsers…|d
  S|studio.code.related|Related Files|
    I|studio.code.related.show_related_files|Show Related Files|d
    I|studio.code.related.next_related_file|Next Related File|d
    I|studio.code.related.previous_related_file|Previous Related File|d
    I|studio.code.related.related_files_options|Related Files Options…|d
  S|studio.video.overlays|Video Overlays|
    I|studio.video.overlays.safe_margins|Safe Margins|d
    I|studio.video.overlays.title_safe|Title Safe|d
    I|studio.video.overlays.action_safe|Action Safe|d
    I|studio.video.overlays.transparency_grid|Transparency Grid|d
    I|studio.video.overlays.broadcast_range_warning|Broadcast Range Warning|d
    I|studio.video.overlays.scopes_overlay|Scopes Overlay|d
  S|studio.typography.story_editor|Story Editor|
    I|studio.typography.story_editor.open_story_editor|Open Story Editor|d
    I|studio.typography.story_editor.story_editor_display|Story Editor Display…|d
    I|studio.typography.story_editor.show_depth_ruler|Show Depth Ruler|d
    I|studio.typography.story_editor.show_style_name_column|Show Style Name Column|d
    I|studio.typography.story_editor.show_paragraph_break_marks|Show Paragraph Break Marks|d
  I|studio.document.structure_view|Structure View|
T|document|DOCUMENT|
  I|studio.document.document_setup|Document Setup…|d
  I|studio.document.canvas_size|Canvas Size…|d
  I|studio.document.image_size|Image Size…|d
  I|studio.document.resample|Resample…|d
  I|studio.document.crop_to_selection|Crop to Selection|d
  I|studio.document.trim|Trim…|d
  I|studio.document.reveal_all|Reveal All|d
  S|studio.document.rotate_document|Rotate Document|d
    I|studio.document.rotate_document.90_clockwise|90° Clockwise|d
    I|studio.document.rotate_document.90_counter_clockwise|90° Counter-Clockwise|d
    I|studio.document.rotate_document.180|180°|d
    I|studio.document.rotate_document.arbitrary|Arbitrary…|d
    I|studio.document.rotate_document.flip_horizontal|Flip Horizontal|d
    I|studio.document.rotate_document.flip_vertical|Flip Vertical|d
  S|studio.document.colour_mode|Colour Mode|d
    I|studio.document.colour_mode.bitmap|Bitmap|d
    I|studio.document.colour_mode.greyscale|Greyscale|d
    I|studio.document.colour_mode.duotone|Duotone…|d
    I|studio.document.colour_mode.indexed_colour|Indexed Colour…|d
    I|studio.document.colour_mode.rgb|RGB|d
    I|studio.document.colour_mode.cmyk|CMYK|d
    I|studio.document.colour_mode.lab|Lab|d
    I|studio.document.colour_mode.multichannel|Multichannel|d
  S|studio.document.bit_depth|Bit Depth|d
    I|studio.document.bit_depth.8_bits_channel|8 bits / channel|d
    I|studio.document.bit_depth.16_bits_channel|16 bits / channel|d
    I|studio.document.bit_depth.32_bits_channel|32 bits / channel|d
    I|studio.document.bit_depth.hdr_toning|HDR Toning…|d
  S|studio.document.artboards|Artboards|d
    I|studio.document.artboards.new_artboard|New Artboard|d
    I|studio.document.artboards.new_artboard_from_selection|New Artboard from Selection|d
    I|studio.document.artboards.duplicate_artboard|Duplicate Artboard|d
    I|studio.document.artboards.delete_artboard|Delete Artboard|d
    I|studio.document.artboards.delete_empty_artboards|Delete Empty Artboards|d
    I|studio.document.artboards.artboard_options|Artboard Options…|d
    I|studio.document.artboards.rearrange_all_artboards|Rearrange All Artboards…|d
    I|studio.document.artboards.fit_artboard_to_artwork|Fit Artboard to Artwork|d
    I|studio.document.artboards.rename_artboard|Rename Artboard…|d
    I|studio.document.artboards.go_to_artboard|Go to Artboard…|d
    I|studio.document.artboards.convert_selection_to_artboard|Convert Selection to Artboard|d
    I|studio.document.artboards.artboard_presets|Artboard Presets…|d
  S|studio.document.pages_spreads|Pages & Spreads|d
    I|studio.document.pages_spreads.insert_pages|Insert Pages…|d
    I|studio.document.pages_spreads.move_pages|Move Pages…|d
    I|studio.document.pages_spreads.duplicate_spread|Duplicate Spread|d
    I|studio.document.pages_spreads.delete_pages|Delete Pages…|d
    I|studio.document.pages_spreads.allow_document_pages_to_shuffle|Allow Document Pages to Shuffle|d
    I|studio.document.pages_spreads.allow_selected_spread_to_shuffle|Allow Selected Spread to Shuffle|d
    I|studio.document.pages_spreads.go_to_page|Go to Page…|d
    I|studio.document.pages_spreads.first_page|First Page|d
    I|studio.document.pages_spreads.previous_page|Previous Page|d
    I|studio.document.pages_spreads.next_page|Next Page|d
    I|studio.document.pages_spreads.last_page|Last Page|d
    I|studio.document.pages_spreads.next_spread|Next Spread|d
    I|studio.document.pages_spreads.previous_spread|Previous Spread|d
    I|studio.document.pages_spreads.page_transitions|Page Transitions…|d
    I|studio.document.pages_spreads.page_size|Page Size…|d
  S|studio.document.parent_pages|Parent Pages|d
    I|studio.document.parent_pages.new_parent|New Parent…|d
    I|studio.document.parent_pages.apply_parent_to_pages|Apply Parent to Pages…|d
    I|studio.document.parent_pages.save_as_parent|Save as Parent|d
    I|studio.document.parent_pages.load_parents|Load Parents…|d
    I|studio.document.parent_pages.override_all_parent_page_items|Override All Parent Page Items|d
    I|studio.document.parent_pages.detach_selection_from_parent|Detach Selection from Parent|d
    I|studio.document.parent_pages.remove_all_local_overrides|Remove All Local Overrides|d
    I|studio.document.parent_pages.parent_page_options|Parent Page Options…|d
    I|studio.document.parent_pages.go_to_first_parent|Go to First Parent|d
  S|studio.document.sections_numbering|Sections & Numbering|d
    I|studio.document.sections_numbering.numbering_section_options|Numbering & Section Options…|d
    I|studio.document.sections_numbering.start_section|Start Section|d
    I|studio.document.sections_numbering.end_section|End Section|d
    I|studio.document.sections_numbering.section_marker|Section Marker…|d
    I|studio.document.sections_numbering.page_numbering_style|Page Numbering Style…|d
  S|studio.document.liquid_layout|Liquid Layout|d
    I|studio.document.liquid_layout.adjust_layout|Adjust Layout…|d
    I|studio.document.liquid_layout.liquid_page_rule|Liquid Page Rule…|d
    I|studio.document.liquid_layout.create_alternate_layout|Create Alternate Layout…|d
    I|studio.document.liquid_layout.alternate_layout_options|Alternate Layout Options…|d
  S|studio.document.long_document|Long Document|d
    I|studio.document.long_document.table_of_contents|Table of Contents|d
    I|studio.document.long_document.index|Index|d
    I|studio.document.long_document.cross_references|Cross-References|d
    I|studio.document.long_document.notes_references|Notes & References|d
    I|studio.document.long_document.book|Book|d
    I|studio.document.long_document.text_variables|Text Variables|d
    I|studio.document.long_document.conditional_text|Conditional Text|d
  S|studio.motion.composition|Compositions|
    I|studio.motion.composition.new_composition|New Composition|d
    I|studio.motion.composition.composition_settings|Composition Settings…|d
    I|studio.motion.composition.crop_composition_to_selected_layer_bounds|Crop Composition to Selected Layer Bounds|d
    I|studio.motion.composition.crop_composition_to_region_of_interest|Crop Composition to Region of Interest|d
    I|studio.motion.composition.composition_flowchart|Composition Flowchart|d
    I|studio.motion.composition.composition_mini_flowchart|Composition Mini-Flowchart|d
    I|studio.motion.composition.pre_render|Pre-render|d
    I|studio.motion.composition.preview|Preview|d
    I|studio.motion.composition.save_frame_as|Save Frame As…|d
    I|studio.motion.composition.save_current_preview|Save Current Preview|d
    I|studio.motion.composition.set_poster_time|Set Poster Time|d
    I|studio.motion.composition.responsive_time_settings|Responsive Time Settings…|d
    I|studio.motion.composition.trim_composition_to_work_area|Trim Composition to Work Area|d
    I|studio.motion.composition.nest_composition|Nest Composition|d
  S|studio.motion.timeline_doc|Timelines|
    I|studio.motion.timeline_doc.new_timeline|New Timeline|d
    I|studio.motion.timeline_doc.timeline_settings|Timeline Settings…|d
    I|studio.motion.timeline_doc.duration|Duration…|d
    I|studio.motion.timeline_doc.frame_rate|Frame Rate…|d
    I|studio.motion.timeline_doc.resolution|Resolution…|d
    I|studio.motion.timeline_doc.nest_timeline|Nest Timeline|d
    I|studio.motion.timeline_doc.timeline_presets|Timeline Presets…|d
    I|studio.motion.timeline_doc.delete_timeline|Delete Timeline|d
    I|studio.motion.timeline_doc.timeline_from_selection|Timeline from Selection|d
  S|studio.document.boards|Boards|d
    I|studio.document.boards.new_board|New Board|d
    I|studio.document.boards.board_settings|Board Settings…|d
    I|studio.document.boards.infinite_canvas|Infinite Canvas|d
    I|studio.document.boards.facilitation_timer|Facilitation Timer…|d
    I|studio.document.boards.voting_session|Voting Session…|d
    I|studio.document.boards.follow_presenter|Follow Presenter|d
    I|studio.document.boards.board_templates|Board Templates…|d
  S|studio.document.site_tree|Site Tree|d
    I|studio.document.site_tree.new_page|New Page|d
    I|studio.document.site_tree.new_folder|New Folder|d
    I|studio.document.site_tree.page_properties|Page Properties…|d
    I|studio.document.site_tree.document_type|Document Type…|d
    I|studio.document.site_tree.apply_template_to_page|Apply Template to Page…|d
    I|studio.document.site_tree.detach_from_template|Detach from Template|d
    I|studio.document.site_tree.make_template|Make Template|d
    I|studio.document.site_tree.editable_regions|Editable Regions…|d
    I|studio.document.site_tree.site_map|Site Map|d
  S|studio.document.develop|Develop|d
    I|studio.document.develop.open_in_develop|Open in Develop|d
    I|studio.document.develop.reset_develop|Reset Develop|d
    I|studio.document.develop.copy_develop_settings|Copy Develop Settings|d
    I|studio.document.develop.paste_develop_settings|Paste Develop Settings|d
    I|studio.document.develop.sync_develop_settings|Sync Develop Settings…|d
    I|studio.document.develop.auto_tone|Auto Tone|d
    I|studio.document.develop.auto_white_balance|Auto White Balance|d
    I|studio.document.develop.snapshots|Snapshots|d
    I|studio.document.develop.process_version|Process Version|d
    I|studio.document.develop.camera_profile|Camera Profile|d
    I|studio.document.develop.enhance|Enhance|d
    I|studio.document.develop.develop_presets|Develop Presets…|d
    I|studio.document.develop.workflow_options|Workflow Options…|d
    I|studio.document.develop.apply_to_raster_document|Apply to Raster Document|d
  S|studio.document.grids_guides_setup|Grids & Guides Setup|d
    I|studio.document.grids_guides_setup.margins_columns|Margins & Columns…|d
    I|studio.document.grids_guides_setup.baseline_grid|Baseline Grid…|d
    I|studio.document.grids_guides_setup.document_grid|Document Grid…|d
    I|studio.document.grids_guides_setup.layout_grid|Layout Grid…|d
    I|studio.document.grids_guides_setup.bleed_slug|Bleed & Slug…|d
    I|studio.document.grids_guides_setup.frame_grid|Frame Grid…|d
  S|studio.document.fonts|Fonts|d
    I|studio.document.fonts.manage_fonts|Manage Fonts…|d
    I|studio.document.fonts.document_fonts|Document Fonts…|d
    I|studio.document.fonts.missing_fonts|Missing Fonts…|d
    I|studio.document.fonts.font_substitution_rules|Font Substitution Rules…|d
    I|studio.document.fonts.package_fonts|Package Fonts|d
    I|studio.document.fonts.add_web_font|Add Web Font…|d
  S|studio.document.accessibility_structure|Accessibility & Structure|d
    I|studio.document.accessibility_structure.alt_text|Alt Text…|d
    I|studio.document.accessibility_structure.articles_reading_order|Articles / Reading Order…|d
    I|studio.document.accessibility_structure.tag_structure|Tag Structure…|d
    I|studio.document.accessibility_structure.export_tagging|Export Tagging…|d
    I|studio.document.accessibility_structure.accessibility_check|Accessibility Check…|d
    I|studio.document.accessibility_structure.contrast_report|Contrast Report…|d
    I|studio.document.accessibility_structure.xml_structure|XML Structure…|d
  I|studio.document.document_metadata|Document Metadata…|d
  I|studio.document.profile|Document Profile…|
T|select|SELECT|
  I|studio.select.all|All|d
  I|studio.select.all_on_active_artboard|All on Active Artboard|d
  I|studio.select.all_on_spread|All on Spread|d
  I|studio.select.deselect|Deselect|d
  I|studio.select.reselect|Reselect|d
  I|studio.select.inverse|Inverse|d
  S|studio.select.select_by|Select By|d
    I|studio.select.select_by.layer_kind|Layer Kind…|d
    I|studio.select.select_by.fill_colour|Fill Colour|d
    I|studio.select.select_by.stroke_colour|Stroke Colour|d
    I|studio.select.select_by.stroke_weight|Stroke Weight|d
    I|studio.select.select_by.blending_mode|Blending Mode|d
    I|studio.select.select_by.opacity|Opacity|d
    I|studio.select.select_by.graphic_style|Graphic Style|d
    I|studio.select.select_by.object_style|Object Style|d
    I|studio.select.select_by.font|Font|d
    I|studio.select.select_by.paragraph_style|Paragraph Style|d
    I|studio.select.select_by.component_instance|Component / Instance|d
    I|studio.select.select_by.tag_or_label|Tag or Label…|d
    I|studio.select.select_by.same_appearance|Same Appearance|d
    I|studio.select.select_by.similar_objects|Similar Objects|d
  S|studio.select.modify|Modify|d
    I|studio.select.modify.grow|Grow|d
    I|studio.select.modify.shrink|Shrink|d
    I|studio.select.modify.border|Border…|d
    I|studio.select.modify.smooth|Smooth…|d
    I|studio.select.modify.expand|Expand…|d
    I|studio.select.modify.contract|Contract…|d
    I|studio.select.modify.feather|Feather…|d
    I|studio.select.modify.refine_edge|Refine Edge…|d
  S|studio.select.select_by_content|Select By Content|d
    I|studio.select.select_by_content.colour_range|Colour Range…|d
    I|studio.select.select_by_content.luminance_range|Luminance Range…|d
    I|studio.select.select_by_content.focus_area|Focus Area…|d
    I|studio.select.select_by_content.subject|Subject|d
    I|studio.select.select_by_content.sky|Sky|d
    I|studio.select.select_by_content.transparent_areas|Transparent Areas|d
  S|studio.select.selection_from|Selection From|d
    I|studio.select.selection_from.layer_transparency|Layer Transparency|d
    I|studio.select.selection_from.layer_mask|Layer Mask|d
    I|studio.select.selection_from.vector_path|Vector Path|d
    I|studio.select.selection_from.channel|Channel…|d
    I|studio.select.selection_from.text|Text|d
    I|studio.select.selection_from.guides|Guides|d
    I|studio.select.selection_from.artboard_bounds|Artboard Bounds|d
  S|studio.select.selection_sets|Selection Sets|d
    I|studio.select.selection_sets.save_selection|Save Selection…|d
    I|studio.select.selection_sets.load_selection|Load Selection…|d
    I|studio.select.selection_sets.edit_selection_sets|Edit Selection Sets…|d
    I|studio.select.selection_sets.delete_selection_set|Delete Selection Set|d
  S|studio.select.navigate_selection|Navigate Selection|d
    I|studio.select.navigate_selection.next_object_above|Next Object Above|d
    I|studio.select.navigate_selection.next_object_below|Next Object Below|d
    I|studio.select.navigate_selection.first_object|First Object|d
    I|studio.select.navigate_selection.last_object|Last Object|d
    I|studio.select.navigate_selection.select_parent|Select Parent|d
    I|studio.select.navigate_selection.select_child|Select Child|d
    I|studio.select.navigate_selection.select_siblings|Select Siblings|d
    I|studio.select.navigate_selection.select_parent_tag|Select Parent Tag|d
  I|studio.select.edit_in_isolation|Edit in Isolation|d
  I|studio.select.exit_isolation|Exit Isolation|d
  I|studio.select.quick_mask_mode|Quick Mask Mode|d
  I|studio.select.edit_selection_as_mask|Edit Selection as Mask|d
  I|studio.select.lock_selection|Lock Selection|d
  I|studio.select.select_all_unlocked|Select All Unlocked|d
T|object|OBJECT|
  S|studio.object.transform|Transform|d
    I|studio.object.transform.free_transform|Free Transform|d
    I|studio.object.transform.move|Move…|d
    I|studio.object.transform.scale|Scale…|d
    I|studio.object.transform.rotate|Rotate…|d
    I|studio.object.transform.shear|Shear…|d
    I|studio.object.transform.reflect|Reflect…|d
    I|studio.object.transform.flip_horizontal|Flip Horizontal|d
    I|studio.object.transform.flip_vertical|Flip Vertical|d
    I|studio.object.transform.rotate_90_cw|Rotate 90° CW|d
    I|studio.object.transform.rotate_90_ccw|Rotate 90° CCW|d
    I|studio.object.transform.rotate_180|Rotate 180°|d
    I|studio.object.transform.distort|Distort|d
    I|studio.object.transform.perspective|Perspective|d
    I|studio.object.transform.skew|Skew|d
    I|studio.object.transform.warp|Warp…|d
    I|studio.object.transform.puppet_warp|Puppet Warp|d
    I|studio.object.transform.content_aware_scale|Content-Aware Scale|d
    I|studio.object.transform.transform_again|Transform Again|d
    I|studio.object.transform.transform_each|Transform Each…|d
    I|studio.object.transform.reset_transform|Reset Transform|d
    I|studio.object.transform.transform_reference_point|Transform Reference Point…|d
  S|studio.object.arrange|Arrange|d
    I|studio.object.arrange.bring_to_front|Bring to Front|d
    I|studio.object.arrange.bring_forward|Bring Forward|d
    I|studio.object.arrange.send_backward|Send Backward|d
    I|studio.object.arrange.send_to_back|Send to Back|d
    I|studio.object.arrange.send_to_current_layer|Send to Current Layer|d
    I|studio.object.arrange.move_to_artboard|Move to Artboard…|d
    I|studio.object.arrange.move_to_page|Move to Page…|d
  E|studio.object.align|Align & Distribute|
  I|studio.object.group|Group|d
  I|studio.object.ungroup|Ungroup|d
  S|studio.object.lock|Lock|d
    I|studio.object.lock.lock_selection|Lock Selection|d
    I|studio.object.lock.unlock_all|Unlock All|d
    I|studio.object.lock.lock_others|Lock Others|d
    I|studio.object.lock.lock_position|Lock Position|d
    I|studio.object.lock.lock_guides|Lock Guides|d
  S|studio.object.hide|Hide|d
    I|studio.object.hide.hide_selection|Hide Selection|d
    I|studio.object.hide.show_all|Show All|d
    I|studio.object.hide.hide_others|Hide Others|d
  I|studio.object.rename|Rename…|d
  I|studio.object.object_label_script_id|Object Label / Script Id…|dm
  S|studio.object.layer|Layer|
    I|studio.object.layer.new|New|d
    I|studio.object.layer.duplicate_layer|Duplicate Layer…|d
    I|studio.object.layer.delete_layer|Delete Layer|d
    I|studio.object.layer.merge_down|Merge Down|d
    I|studio.object.layer.merge_visible|Merge Visible|d
    I|studio.object.layer.merge_selected|Merge Selected|d
    I|studio.object.layer.flatten_document|Flatten Document|d
    I|studio.object.layer.rasterise|Rasterise|d
    I|studio.object.layer.convert_to|Convert To|d
    E|studio.object.layer.blending_mode|Blending Mode|dn
    E|studio.object.layer.track_matte|Track Matte|dn
    I|studio.object.layer.opacity|Opacity…|d
    I|studio.object.layer.fill_opacity|Fill Opacity…|d
    I|studio.object.layer.knockout|Knockout|d
    I|studio.object.layer.isolate_blending|Isolate Blending|d
    E|studio.object.layer.frame_blending|Frame Blending|dn
    I|studio.object.layer.layer_colour_tag|Layer Colour Tag…|d
    I|studio.object.layer.layer_comps|Layer Comps…|d
    I|studio.object.layer.layer_states|Layer States…|d
    I|studio.object.layer.3d_layer|3D Layer|d
    I|studio.object.layer.guide_layer|Guide Layer|d
    I|studio.object.layer.environment_layer|Environment Layer|d
    I|studio.object.layer.layer_styles|Layer Styles|d
    I|studio.object.layer.layer_settings|Layer Settings…|d
    I|studio.object.layer.material|Material|d
    I|studio.object.layer.camera|Camera|d
    I|studio.object.layer.light|Light|d
    I|studio.object.layer.pre_compose|Pre-compose…|d
    I|studio.object.layer.auto_trace|Auto-trace…|d
    I|studio.object.layer.create_shapes_from_vector_layer|Create Shapes from Vector Layer|d
    I|studio.object.layer.open_layer|Open Layer|d
    I|studio.object.layer.open_layer_source|Open Layer Source|d
    I|studio.object.layer.add_marker|Add Marker|d
  S|studio.object.mask|Mask|
    I|studio.object.mask.new_mask|New Mask|d
    I|studio.object.mask.add_layer_mask|Add Layer Mask|d
    I|studio.object.mask.add_vector_mask|Add Vector Mask|d
    I|studio.object.mask.reveal_all|Reveal All|d
    I|studio.object.mask.hide_all|Hide All|d
    I|studio.object.mask.from_selection|From Selection|d
    I|studio.object.mask.from_transparency|From Transparency|d
    I|studio.object.mask.make_clipping_mask|Make Clipping Mask|d
    I|studio.object.mask.release_clipping_mask|Release Clipping Mask|d
    I|studio.object.mask.apply_mask|Apply Mask|d
    I|studio.object.mask.disable_mask|Disable Mask|d
    I|studio.object.mask.delete_mask|Delete Mask|d
    I|studio.object.mask.invert_mask|Invert Mask|d
    I|studio.object.mask.refine_mask|Refine Mask…|d
    I|studio.object.mask.link_unlink_mask|Link/Unlink Mask|d
    I|studio.object.mask.edit_mask|Edit Mask|d
    I|studio.object.mask.mask_density_feather|Mask Density & Feather…|d
    I|studio.object.mask.mask_shape|Mask Shape…|d
    I|studio.object.mask.mask_feather|Mask Feather…|d
    I|studio.object.mask.mask_opacity|Mask Opacity…|d
    I|studio.object.mask.mask_expansion|Mask Expansion…|d
    E|studio.object.mask.mode|Mode|dn
    I|studio.object.mask.inverted|Inverted|d
    I|studio.object.mask.locked|Locked|d
    I|studio.object.mask.closed|Closed|d
    I|studio.object.mask.rotobezier|RotoBezier|d
    I|studio.object.mask.motion_blur|Motion Blur|d
    I|studio.object.mask.feather_falloff|Feather Falloff|d
    I|studio.object.mask.free_transform_points|Free Transform Points|d
    I|studio.object.mask.set_first_vertex|Set First Vertex|d
    I|studio.object.mask.lock_other_masks|Lock Other Masks|d
    I|studio.object.mask.hide_locked_masks|Hide Locked Masks|d
    I|studio.object.mask.unlock_all_masks|Unlock All Masks|d
    I|studio.object.mask.remove_mask|Remove Mask|d
    I|studio.object.mask.remove_all_masks|Remove All Masks|d
    I|studio.object.mask.reset_mask|Reset Mask|d
  S|studio.object.path|Path|d
    I|studio.object.path.join|Join|d
    I|studio.object.path.average|Average…|d
    I|studio.object.path.average_and_join|Average and Join|d
    I|studio.object.path.outline_stroke|Outline Stroke|d
    I|studio.object.path.offset_path|Offset Path…|d
    I|studio.object.path.simplify|Simplify…|d
    I|studio.object.path.add_anchor_points|Add Anchor Points|d
    I|studio.object.path.remove_anchor_points|Remove Anchor Points|d
    I|studio.object.path.convert_point|Convert Point|d
    I|studio.object.path.split_path_at_selected_points|Split Path at Selected Points|d
    I|studio.object.path.reverse_path_direction|Reverse Path Direction|d
    I|studio.object.path.clean_up|Clean Up…|d
    I|studio.object.path.close_path|Close Path|d
    I|studio.object.path.open_path|Open Path|d
    I|studio.object.path.path_direction|Path Direction…|d
    I|studio.object.path.convert_to_vector_network|Convert to Vector Network|d
  S|studio.object.boolean|Boolean|d
    I|studio.object.boolean.union|Union|d
    I|studio.object.boolean.subtract_front|Subtract Front|d
    I|studio.object.boolean.subtract_back|Subtract Back|d
    I|studio.object.boolean.intersect|Intersect|d
    I|studio.object.boolean.exclude|Exclude|d
    I|studio.object.boolean.divide|Divide|d
    I|studio.object.boolean.trim|Trim|d
    I|studio.object.boolean.merge|Merge|d
    I|studio.object.boolean.crop|Crop|d
    I|studio.object.boolean.outline|Outline|d
    I|studio.object.boolean.hard_mix|Hard Mix|d
    I|studio.object.boolean.soft_mix|Soft Mix…|d
    I|studio.object.boolean.trap|Trap…|d
    I|studio.object.boolean.make_compound_path|Make Compound Path|d
    I|studio.object.boolean.release_compound_path|Release Compound Path|d
    I|studio.object.boolean.repeat_last_boolean|Repeat Last Boolean|d
  S|studio.object.shape|Shape|d
    I|studio.object.shape.convert_shape|Convert Shape|d
    I|studio.object.shape.corner_options|Corner Options…|d
    I|studio.object.shape.live_corners|Live Corners|d
    I|studio.object.shape.expand|Expand…|d
    I|studio.object.shape.expand_appearance|Expand Appearance|d
    I|studio.object.shape.edit_shape_parameters|Edit Shape Parameters…|d
    I|studio.object.shape.reset_shape|Reset Shape|d
  S|studio.object.frame_fitting|Frame & Fitting|d
    I|studio.object.frame_fitting.frame_type|Frame Type|d
    I|studio.object.frame_fitting.fitting|Fitting|d
    I|studio.object.frame_fitting.content|Content|d
    I|studio.object.frame_fitting.anchored_object|Anchored Object|d
    I|studio.object.frame_fitting.clipping_path|Clipping Path|d
    I|studio.object.frame_fitting.caption|Caption|d
    I|studio.object.frame_fitting.text_frame_options|Text Frame Options…|d
    I|studio.object.frame_fitting.text_wrap|Text Wrap…|d
    I|studio.object.frame_fitting.flex_container_options|Flex Container Options…|d
  E|studio.layout.table|Table|
  S|studio.object.component|Component|d
    I|studio.object.component.create_component|Create Component|d
    I|studio.object.component.create_component_set|Create Component Set|d
    I|studio.object.component.add_variant|Add Variant|d
    I|studio.object.component.component_properties|Component Properties…|d
    I|studio.object.component.detach_instance|Detach Instance|d
    I|studio.object.component.swap_instance|Swap Instance…|d
    I|studio.object.component.reset_all_overrides|Reset All Overrides|d
    I|studio.object.component.go_to_main_component|Go to Main Component|d
    I|studio.object.component.publish_to_library|Publish to Library…|d
    I|studio.object.component.update_from_library|Update from Library|d
    I|studio.object.component.library_manager|Library Manager…|d
    I|studio.object.component.break_symbol_link|Break Symbol Link|d
  S|studio.object.auto_layout|Auto Layout|d
    I|studio.object.auto_layout.add_auto_layout|Add Auto Layout|d
    I|studio.object.auto_layout.remove_auto_layout|Remove Auto Layout|d
    I|studio.object.auto_layout.direction_horizontal|Direction: Horizontal|d
    I|studio.object.auto_layout.direction_vertical|Direction: Vertical|d
    I|studio.object.auto_layout.direction_wrap|Direction: Wrap|d
    I|studio.object.auto_layout.spacing|Spacing…|d
    I|studio.object.auto_layout.padding|Padding…|d
    I|studio.object.auto_layout.alignment|Alignment…|d
    I|studio.object.auto_layout.absolute_position|Absolute Position|d
    I|studio.object.auto_layout.resizing|Resizing…|d
  S|studio.object.constraints|Constraints|d
    I|studio.object.constraints.left|Left|d
    I|studio.object.constraints.right|Right|d
    I|studio.object.constraints.top|Top|d
    I|studio.object.constraints.bottom|Bottom|d
    I|studio.object.constraints.centre|Centre|d
    I|studio.object.constraints.scale|Scale|d
    I|studio.object.constraints.stretch|Stretch|d
    I|studio.object.constraints.constraint_options|Constraint Options…|d
  S|studio.object.layout_grid|Layout Grid|d
    I|studio.object.layout_grid.add_layout_grid|Add Layout Grid|d
    I|studio.object.layout_grid.remove_layout_grid|Remove Layout Grid|d
    I|studio.object.layout_grid.columns|Columns…|d
    I|studio.object.layout_grid.rows|Rows…|d
    I|studio.object.layout_grid.grid|Grid…|d
    I|studio.object.layout_grid.save_grid_style|Save Grid Style…|d
  S|studio.object.blend|Blend|d
    I|studio.object.blend.make_blend|Make Blend|d
    I|studio.object.blend.release_blend|Release Blend|d
    I|studio.object.blend.blend_options|Blend Options…|d
    I|studio.object.blend.expand_blend|Expand Blend|d
    I|studio.object.blend.replace_spine|Replace Spine|d
    I|studio.object.blend.reverse_spine|Reverse Spine|d
    I|studio.object.blend.reverse_front_to_back|Reverse Front to Back|d
  S|studio.object.procedural|Procedural|d
    I|studio.object.procedural.live_paint|Live Paint|d
    I|studio.object.procedural.image_trace|Image Trace|d
    I|studio.object.procedural.gradient_mesh|Gradient Mesh|d
    I|studio.object.procedural.envelope_distort|Envelope Distort|d
    I|studio.object.procedural.repeat|Repeat|d
    I|studio.object.procedural.intertwine|Intertwine|d
    I|studio.object.procedural.pattern|Pattern|d
    I|studio.object.procedural.global_edit|Global Edit|d
    I|studio.object.procedural.offset_path_repeat|Offset Path Repeat…|d
  S|studio.object.clip|Clip|d
    I|studio.object.clip.speed_duration|Speed / Duration…|d
    I|studio.object.clip.enable|Enable|d
    I|studio.object.clip.link_unlink|Link / Unlink|d
    I|studio.object.clip.synchronize|Synchronize…|d
    I|studio.object.clip.nest|Nest…|d
    I|studio.object.clip.make_subclip|Make Subclip…|d
    I|studio.object.clip.merge_clips|Merge Clips…|d
    I|studio.object.clip.multi_camera|Multi-Camera|d
    I|studio.object.clip.replace|Replace|d
    I|studio.object.clip.render|Render|d
    I|studio.object.clip.restore|Restore|d
    I|studio.object.clip.source_settings|Source Settings…|d
    I|studio.object.clip.interpret_footage|Interpret Footage…|d
    I|studio.object.clip.modify_audio_channels|Modify Audio Channels…|d
    I|studio.object.clip.frame_hold_options|Frame Hold Options…|d
    I|studio.object.clip.scale_to_frame_size|Scale to Frame Size|d
    I|studio.object.clip.set_to_frame_size|Set to Frame Size|d
  S|studio.object.styles_appearance|Styles & Appearance|d
    I|studio.object.styles_appearance.object_styles|Object Styles…|d
    I|studio.object.styles_appearance.graphic_styles|Graphic Styles…|d
    I|studio.object.styles_appearance.create_style_from_selection|Create Style from Selection|d
    I|studio.object.styles_appearance.redefine_style|Redefine Style|d
    I|studio.object.styles_appearance.break_link_to_style|Break Link to Style|d
    I|studio.object.styles_appearance.load_styles|Load Styles…|d
    I|studio.object.styles_appearance.appearance_add_new_fill|Appearance: Add New Fill|d
    I|studio.object.styles_appearance.appearance_add_new_stroke|Appearance: Add New Stroke|d
    I|studio.object.styles_appearance.appearance_clear_appearance|Appearance: Clear Appearance|d
    I|studio.object.styles_appearance.appearance_reduce_to_basic|Appearance: Reduce to Basic|d
    I|studio.object.styles_appearance.copy_appearance|Copy Appearance|d
    I|studio.object.styles_appearance.paste_appearance|Paste Appearance|d
  I|studio.object.object_export_options|Object Export Options…|d
  I|studio.object.object_metadata|Object Metadata…|d
  I|studio.object.capture_appearance_from_selection|Capture Appearance from Selection|d
T|type|TYPE|
  S|studio.type.font|Font|d
    E|studio.type.font.installed_font_families|Installed font families|dg
    I|studio.type.font.recent_fonts|Recent Fonts|d
    I|studio.type.font.favourite_fonts|Favourite Fonts|d
    I|studio.type.font.font_filter|Font Filter…|d
    I|studio.type.font.add_to_favourites|Add to Favourites|d
    I|studio.type.font.font_preview_size|Font Preview Size…|d
  S|studio.type.size|Size|d
    E|studio.type.size.preset_size_ladder|Preset size ladder|dg
    I|studio.type.size.other_size|Other Size…|d
    I|studio.type.size.increase_size|Increase Size|d
    I|studio.type.size.decrease_size|Decrease Size|d
    I|studio.type.size.restore_default_size|Restore Default Size|d
  S|studio.type.character|Character|d
    I|studio.type.character.kerning|Kerning|d
    I|studio.type.character.tracking|Tracking…|d
    I|studio.type.character.leading|Leading…|d
    I|studio.type.character.baseline_shift|Baseline Shift…|d
    I|studio.type.character.horizontal_scale|Horizontal Scale…|d
    I|studio.type.character.vertical_scale|Vertical Scale…|d
    I|studio.type.character.skew|Skew…|d
    I|studio.type.character.case|Case|d
    I|studio.type.character.position|Position|d
    I|studio.type.character.underline_strike|Underline & Strike|d
    I|studio.type.character.bold|Bold|d
    I|studio.type.character.italic|Italic|d
    I|studio.type.character.no_break|No Break|d
    I|studio.type.character.ligatures|Ligatures|d
    I|studio.type.character.language|Language…|d
    I|studio.type.character.character_colour|Character Colour…|d
  S|studio.type.paragraph|Paragraph|d
    I|studio.type.paragraph.align|Align|d
    I|studio.type.paragraph.indents|Indents…|d
    I|studio.type.paragraph.space_before_after|Space Before / After…|d
    I|studio.type.paragraph.drop_caps|Drop Caps…|d
    I|studio.type.paragraph.hyphenation|Hyphenation…|d
    I|studio.type.paragraph.justification|Justification…|d
    I|studio.type.paragraph.keep_options|Keep Options…|d
    I|studio.type.paragraph.span_split_columns|Span / Split Columns…|d
    I|studio.type.paragraph.balance_ragged_lines|Balance Ragged Lines|d
    I|studio.type.paragraph.paragraph_rules|Paragraph Rules…|d
    I|studio.type.paragraph.paragraph_shading|Paragraph Shading…|d
    I|studio.type.paragraph.paragraph_borders|Paragraph Borders…|d
    I|studio.type.paragraph.composer|Composer|d
    I|studio.type.paragraph.bullets_numbering|Bullets & Numbering|d
    I|studio.type.paragraph.tabs|Tabs…|d
  E|studio.typography.opentype|OpenType|
  S|studio.type.variable_fonts|Variable Fonts|d
    I|studio.type.variable_fonts.axes|Axes…|d
    I|studio.type.variable_fonts.named_instances|Named Instances|d
    I|studio.type.variable_fonts.reset_axes|Reset Axes|d
    I|studio.type.variable_fonts.create_named_instance|Create Named Instance…|d
  S|studio.type.styles|Styles|d
    I|studio.type.styles.character_styles|Character Styles…|d
    I|studio.type.styles.paragraph_styles|Paragraph Styles…|d
    I|studio.type.styles.table_styles|Table Styles…|d
    I|studio.type.styles.cell_styles|Cell Styles…|d
    I|studio.type.styles.create_style_from_selection|Create Style from Selection|d
    I|studio.type.styles.redefine_style|Redefine Style|d
    I|studio.type.styles.break_link_to_style|Break Link to Style|d
    I|studio.type.styles.clear_overrides|Clear Overrides|d
    I|studio.type.styles.load_styles_from_document|Load Styles from Document…|d
    I|studio.type.styles.next_style|Next Style…|d
    I|studio.type.styles.style_conflict_report|Style Conflict Report…|d
  S|studio.type.insert_special_character|Insert Special Character|d
    I|studio.type.insert_special_character.symbols|Symbols|d
    I|studio.type.insert_special_character.markers|Markers|d
    I|studio.type.insert_special_character.hyphens_dashes|Hyphens & Dashes|d
    I|studio.type.insert_special_character.quotation_marks|Quotation Marks|d
    I|studio.type.insert_special_character.other|Other|d
    I|studio.type.insert_special_character.unicode_value|Unicode Value…|d
  S|studio.type.insert_white_space|Insert White Space|d
    I|studio.type.insert_white_space.em_space|Em Space|d
    I|studio.type.insert_white_space.en_space|En Space|d
    I|studio.type.insert_white_space.non_breaking_space|Non-breaking Space|d
    I|studio.type.insert_white_space.thin_space|Thin Space|d
    I|studio.type.insert_white_space.hair_space|Hair Space|d
    I|studio.type.insert_white_space.flush_space|Flush Space|d
    I|studio.type.insert_white_space.figure_space|Figure Space|d
    I|studio.type.insert_white_space.punctuation_space|Punctuation Space|d
    I|studio.type.insert_white_space.sixth_quarter_third_space|Sixth / Quarter / Third Space|d
  S|studio.type.insert_break|Insert Break|d
    I|studio.type.insert_break.line_break|Line Break|d
    I|studio.type.insert_break.column_break|Column Break|d
    I|studio.type.insert_break.frame_break|Frame Break|d
    I|studio.type.insert_break.page_break|Page Break|d
    I|studio.type.insert_break.odd_even_page_break|Odd / Even Page Break|d
    I|studio.type.insert_break.paragraph_return|Paragraph Return|d
    I|studio.type.insert_break.discretionary_hyphen|Discretionary Hyphen|d
    I|studio.type.insert_break.indent_to_here|Indent to Here|d
    I|studio.type.insert_break.right_indent_tab|Right Indent Tab|d
    I|studio.type.insert_break.zero_width_joiner_non_joiner|Zero-Width Joiner / Non-Joiner|d
  S|studio.type.glyphs|Glyphs|d
    I|studio.type.glyphs.glyphs_panel|Glyphs Panel|d
    I|studio.type.glyphs.alternates_for_selection|Alternates for Selection|d
    I|studio.type.glyphs.add_to_glyph_favourites|Add to Glyph Favourites|d
    I|studio.type.glyphs.remove_from_glyph_favourites|Remove from Glyph Favourites|d
    I|studio.type.glyphs.glyph_sets|Glyph Sets…|d
    I|studio.type.glyphs.recently_used_glyphs|Recently Used Glyphs|d
  S|studio.type.type_on_a_path|Type on a Path|d
    I|studio.type.type_on_a_path.make_type_on_a_path|Make Type on a Path|d
    I|studio.type.type_on_a_path.type_on_a_path_options|Type on a Path Options…|d
    I|studio.type.type_on_a_path.delete_type_from_path|Delete Type from Path|d
    I|studio.type.type_on_a_path.flip_direction|Flip Direction|d
    I|studio.type.type_on_a_path.effect|Effect|d
    I|studio.type.type_on_a_path.align_to_path|Align to Path…|d
    I|studio.type.type_on_a_path.spacing|Spacing…|d
  S|studio.type.threading|Threading|d
    I|studio.type.threading.thread_text_frames|Thread Text Frames|d
    I|studio.type.threading.unthread|Unthread|d
    I|studio.type.threading.show_text_threads|Show Text Threads|d
    I|studio.type.threading.autoflow|Autoflow|d
    I|studio.type.threading.smart_text_reflow|Smart Text Reflow|d
    I|studio.type.threading.load_overset_text|Load Overset Text|d
    I|studio.type.threading.overset_report|Overset Report…|d
  S|studio.type.convert|Convert|d
    I|studio.type.convert.create_outlines|Create Outlines|d
    I|studio.type.convert.convert_to_point_text|Convert to Point Text|d
    I|studio.type.convert.convert_to_area_text|Convert to Area Text|d
    I|studio.type.convert.convert_to_frame_grid|Convert to Frame Grid|d
    I|studio.type.convert.convert_to_table|Convert to Table|d
    I|studio.type.convert.convert_bullets_to_text|Convert Bullets to Text|d
    I|studio.type.convert.convert_numbering_to_text|Convert Numbering to Text|d
  S|studio.type.story|Story|d
    I|studio.type.story.optical_margin_alignment|Optical Margin Alignment|d
    I|studio.type.story.story_direction|Story Direction|d
    I|studio.type.story.story_options|Story Options…|d
    I|studio.type.story.fill_with_placeholder_text|Fill with Placeholder Text|d
  S|studio.type.cjk_complex_scripts|CJK & Complex Scripts|d
    I|studio.type.cjk_complex_scripts.mojikumi_settings|Mojikumi Settings…|d
    I|studio.type.cjk_complex_scripts.kinsoku_settings|Kinsoku Settings…|d
    I|studio.type.cjk_complex_scripts.kinsoku_break_type|Kinsoku Break Type|d
    I|studio.type.cjk_complex_scripts.kinsoku_hang_type|Kinsoku Hang Type|d
    I|studio.type.cjk_complex_scripts.ruby|Ruby…|d
    I|studio.type.cjk_complex_scripts.kenten|Kenten…|d
    I|studio.type.cjk_complex_scripts.tate_chu_yoko|Tate-Chu-Yoko…|d
    I|studio.type.cjk_complex_scripts.warichu|Warichu…|d
    I|studio.type.cjk_complex_scripts.grid_alignment|Grid Alignment|d
    I|studio.type.cjk_complex_scripts.character_alignment|Character Alignment|d
    I|studio.type.cjk_complex_scripts.character_direction|Character Direction|d
    I|studio.type.cjk_complex_scripts.keyboard_direction|Keyboard Direction|d
    I|studio.type.cjk_complex_scripts.digits|Digits|d
    I|studio.type.cjk_complex_scripts.cjk_leading_model|CJK Leading Model|d
    I|studio.type.cjk_complex_scripts.writing_direction|Writing Direction|d
  S|studio.typography.animator|Text Animators|
    S|studio.typography.animator.animate_property|Animate Property|d
      I|studio.typography.animator.animate_property.all_transform_properties|All Transform Properties|d
      I|studio.typography.animator.animate_property.anchor_point|Anchor Point|d
      I|studio.typography.animator.animate_property.position|Position|d
      I|studio.typography.animator.animate_property.scale|Scale|d
      I|studio.typography.animator.animate_property.rotation|Rotation|d
      I|studio.typography.animator.animate_property.skew|Skew|d
      I|studio.typography.animator.animate_property.opacity|Opacity|d
      I|studio.typography.animator.animate_property.blur|Blur|d
      I|studio.typography.animator.animate_property.character_offset|Character Offset|d
      I|studio.typography.animator.animate_property.character_value|Character Value|d
      I|studio.typography.animator.animate_property.fill_colour|Fill Colour|d
      I|studio.typography.animator.animate_property.stroke_colour|Stroke Colour|d
      I|studio.typography.animator.animate_property.stroke_width|Stroke Width|d
      I|studio.typography.animator.animate_property.line_anchor|Line Anchor|d
      I|studio.typography.animator.animate_property.line_spacing|Line Spacing|d
      I|studio.typography.animator.animate_property.tracking|Tracking|d
      I|studio.typography.animator.animate_property.enable_per_character_3d|Enable Per-character 3D|d
      I|studio.typography.animator.animate_property.variable_font_axes|Variable Font Axes|d
      I|studio.typography.animator.animate_property.all_font_axes|All Font Axes|d
    S|studio.typography.animator.add_selector|Add Selector|d
      I|studio.typography.animator.add_selector.range|Range|d
      I|studio.typography.animator.add_selector.wiggly|Wiggly|d
      I|studio.typography.animator.add_selector.expression|Expression|d
    S|studio.typography.animator.fill_animator_sub_properties|Fill Animator Sub-properties|d
      I|studio.typography.animator.fill_animator_sub_properties.brightness|Brightness|d
      I|studio.typography.animator.fill_animator_sub_properties.hue|Hue|d
      I|studio.typography.animator.fill_animator_sub_properties.opacity|Opacity|d
      I|studio.typography.animator.fill_animator_sub_properties.rgb|RGB|d
      I|studio.typography.animator.fill_animator_sub_properties.saturation|Saturation|d
      I|studio.typography.animator.fill_animator_sub_properties.colour|Colour|d
    E|studio.typography.animator.stroke_animator_sub_properties|Stroke Animator Sub-properties|dn
    I|studio.typography.animator.remove_all_text_animators|Remove All Text Animators|d
    I|studio.typography.animator.text_path_options|Text Path Options…|d
    I|studio.typography.animator.more_options|More Options…|d
  I|studio.type.show_hidden_characters|Show Hidden Characters|d
  I|studio.type.text_macros|Text Macros…|d
T|color|COLOR|
  I|studio.color.colour_settings|Colour Settings…|dm
  S|studio.color.working_spaces|Working Spaces|d
    I|studio.color.working_spaces.rgb_working_space|RGB Working Space…|d
    I|studio.color.working_spaces.cmyk_working_space|CMYK Working Space…|d
    I|studio.color.working_spaces.greyscale_working_space|Greyscale Working Space…|d
    I|studio.color.working_spaces.spot_working_space|Spot Working Space…|d
    I|studio.color.working_spaces.transparency_blend_space|Transparency Blend Space…|d
  S|studio.color.colour_management_policies|Colour Management Policies|d
    I|studio.color.colour_management_policies.policies|Policies…|d
    I|studio.color.colour_management_policies.profile_mismatch_warnings|Profile Mismatch Warnings|d
    I|studio.color.colour_management_policies.missing_profile_warnings|Missing Profile Warnings|d
    I|studio.color.colour_management_policies.rendering_intent|Rendering Intent…|d
    I|studio.color.colour_management_policies.black_point_compensation|Black Point Compensation|d
    I|studio.color.colour_management_policies.engine|Engine…|d
  S|studio.color.document_profile|Document Profile|d
    I|studio.color.document_profile.assign_profile|Assign Profile…|d
    I|studio.color.document_profile.convert_to_profile|Convert to Profile…|d
    I|studio.color.document_profile.embed_profile|Embed Profile|d
    I|studio.color.document_profile.remove_profile|Remove Profile|d
    I|studio.color.document_profile.install_profile|Install Profile…|d
    I|studio.color.document_profile.profile_info|Profile Info…|d
  S|studio.color.proof_2|Proof|d
    I|studio.color.proof_2.proof_setup|Proof Setup…|d
    I|studio.color.proof_2.custom_proof_condition|Custom Proof Condition…|d
    I|studio.color.proof_2.proof_colours|Proof Colours|d
    I|studio.color.proof_2.gamut_warning|Gamut Warning|d
    I|studio.color.proof_2.colour_vision_proof|Colour-Vision Proof…|d
    I|studio.color.proof_2.save_proof_preset|Save Proof Preset…|d
  E|studio.color.swatch|Swatches & Colour Assets|
  S|studio.color.spot_colour_ink|Spot Colour & Ink|d
    I|studio.color.spot_colour_ink.new_spot_colour|New Spot Colour…|d
    I|studio.color.spot_colour_ink.convert_spot_to_process|Convert Spot to Process|d
    I|studio.color.spot_colour_ink.convert_process_to_spot|Convert Process to Spot|d
    I|studio.color.spot_colour_ink.ink_manager|Ink Manager…|d
    I|studio.color.spot_colour_ink.ink_aliases|Ink Aliases…|d
    I|studio.color.spot_colour_ink.named_colour_libraries|Named Colour Libraries…|d
    I|studio.color.spot_colour_ink.overprint_fill|Overprint Fill|d
    I|studio.color.spot_colour_ink.overprint_stroke|Overprint Stroke|d
    I|studio.color.spot_colour_ink.overprint_black|Overprint Black…|d
  S|studio.color.separations_output|Separations & Output|d
    I|studio.color.separations_output.separations_preview|Separations Preview|d
    I|studio.color.separations_output.overprint_preview|Overprint Preview|d
    I|studio.color.separations_output.flattener_preview|Flattener Preview|d
    I|studio.color.separations_output.flattener_presets|Flattener Presets…|d
    I|studio.color.separations_output.ink_limit_warning|Ink Limit Warning…|d
    I|studio.color.separations_output.trap_presets|Trap Presets…|d
    I|studio.color.separations_output.total_area_coverage|Total Area Coverage…|d
  S|studio.color.recolour|Recolour|d
    I|studio.color.recolour.recolour_artwork|Recolour Artwork…|d
    I|studio.color.recolour.colour_harmony|Colour Harmony…|d
    I|studio.color.recolour.global_edit_colour|Global Edit Colour|d
    I|studio.color.recolour.adjust_colour_balance|Adjust Colour Balance…|d
    I|studio.color.recolour.blend_front_to_back|Blend Front to Back|d
    I|studio.color.recolour.blend_horizontally|Blend Horizontally|d
    I|studio.color.recolour.blend_vertically|Blend Vertically|d
    I|studio.color.recolour.convert_to_greyscale|Convert to Greyscale|d
    I|studio.color.recolour.convert_to_cmyk|Convert to CMYK|d
    I|studio.color.recolour.convert_to_rgb|Convert to RGB|d
    I|studio.color.recolour.invert_colours|Invert Colours|d
    I|studio.color.recolour.saturate|Saturate…|d
  S|studio.color.luts|LUTs|d
    I|studio.color.luts.apply_lut|Apply LUT…|d
    I|studio.color.luts.load_lut|Load LUT…|d
    I|studio.color.luts.export_lut|Export LUT…|d
    I|studio.color.luts.lut_library|LUT Library…|d
    I|studio.color.luts.sort_luts_by_name|Sort LUTs by Name|d
    I|studio.color.luts.remove_lut|Remove LUT|d
  S|studio.color.colour_picker_sampling|Colour Picker & Sampling|d
    I|studio.color.colour_picker_sampling.colour_picker|Colour Picker…|d
    I|studio.color.colour_picker_sampling.eyedropper_options|Eyedropper Options…|d
    I|studio.color.colour_picker_sampling.sample_size|Sample Size…|d
    I|studio.color.colour_picker_sampling.sample_from_screen|Sample from Screen|d
    I|studio.color.colour_picker_sampling.swap_fill_and_stroke|Swap Fill and Stroke|d
    I|studio.color.colour_picker_sampling.default_fill_and_stroke|Default Fill and Stroke|d
    I|studio.color.colour_picker_sampling.copy_colour_value|Copy Colour Value|d
  S|studio.color.gradients_patterns|Gradients & Patterns|d
    I|studio.color.gradients_patterns.new_gradient|New Gradient…|d
    I|studio.color.gradients_patterns.edit_gradient|Edit Gradient…|d
    I|studio.color.gradients_patterns.gradient_type|Gradient Type…|d
    I|studio.color.gradients_patterns.reverse_gradient|Reverse Gradient|d
    I|studio.color.gradients_patterns.gradient_libraries|Gradient Libraries…|d
    I|studio.color.gradients_patterns.new_pattern|New Pattern…|d
    I|studio.color.gradients_patterns.pattern_libraries|Pattern Libraries…|d
  S|studio.color.colour_variables|Colour Variables|d
    I|studio.color.colour_variables.create_colour_variable|Create Colour Variable…|d
    I|studio.color.colour_variables.bind_selection_to_variable|Bind Selection to Variable…|d
    I|studio.color.colour_variables.variable_collections|Variable Collections…|d
    I|studio.color.colour_variables.mode_switch|Mode Switch…|d
    I|studio.color.colour_variables.detach_variable|Detach Variable|d
T|effects|EFFECTS|
  I|studio.effects.last_effect|Last Effect|d
  I|studio.effects.last_effect_options|Last Effect Options…|d
  E|studio.effect.apply|Apply Effect|
  E|studio.effect.adjustment|Adjustments|
  S|studio.effects.layer_effects|Layer Effects|d
    I|studio.effects.layer_effects.drop_shadow|Drop Shadow…|d
    I|studio.effects.layer_effects.inner_shadow|Inner Shadow…|d
    I|studio.effects.layer_effects.outer_glow|Outer Glow…|d
    I|studio.effects.layer_effects.inner_glow|Inner Glow…|d
    I|studio.effects.layer_effects.bevel_emboss|Bevel & Emboss…|d
    I|studio.effects.layer_effects.satin|Satin…|d
    I|studio.effects.layer_effects.colour_overlay|Colour Overlay…|d
    I|studio.effects.layer_effects.gradient_overlay|Gradient Overlay…|d
    I|studio.effects.layer_effects.pattern_overlay|Pattern Overlay…|d
    I|studio.effects.layer_effects.stroke|Stroke…|d
    I|studio.effects.layer_effects.layer_blur|Layer Blur…|d
    I|studio.effects.layer_effects.3d_effect|3D Effect…|d
    I|studio.effects.layer_effects.global_light|Global Light…|d
    I|studio.effects.layer_effects.scale_effects|Scale Effects…|d
    I|studio.effects.layer_effects.copy_layer_style|Copy Layer Style|d
    I|studio.effects.layer_effects.paste_layer_style|Paste Layer Style|d
    I|studio.effects.layer_effects.clear_layer_style|Clear Layer Style|d
    I|studio.effects.layer_effects.create_layer_from_style|Create Layer from Style|d
    I|studio.effects.layer_effects.hide_all_effects|Hide All Effects|d
  S|studio.effects.live_filters|Live Filters|d
    I|studio.effects.live_filters.add_live_filter|Add Live Filter…|d
    I|studio.effects.live_filters.edit_live_filter|Edit Live Filter…|d
    I|studio.effects.live_filters.disable_live_filter|Disable Live Filter|d
    I|studio.effects.live_filters.rasterise_live_filter|Rasterise Live Filter|d
    I|studio.effects.live_filters.live_filter_mask|Live Filter Mask…|d
    I|studio.effects.live_filters.reorder_live_filters|Reorder Live Filters…|d
  S|studio.effects.effect_stack|Effect Stack|d
    I|studio.effects.effect_stack.reorder_effects|Reorder Effects…|d
    I|studio.effects.effect_stack.duplicate_effect|Duplicate Effect|d
    I|studio.effects.effect_stack.delete_effect|Delete Effect|d
    I|studio.effects.effect_stack.effect_opacity|Effect Opacity…|d
    I|studio.effects.effect_stack.effect_blend_mode|Effect Blend Mode…|d
    I|studio.effects.effect_stack.mask_effect_with_selection|Mask Effect with Selection|d
    I|studio.effects.effect_stack.disable_all_effects|Disable All Effects|d
    I|studio.effects.effect_stack.reset_all_parameters|Reset All Parameters|d
    I|studio.effects.effect_stack.copy_effect_stack|Copy Effect Stack|d
    I|studio.effects.effect_stack.paste_effect_stack|Paste Effect Stack|d
    I|studio.effects.effect_stack.manage_effects|Manage Effects…|d
  S|studio.effects.interactive_effect_workspaces|Interactive Effect Workspaces|d
    I|studio.effects.interactive_effect_workspaces.liquify|Liquify…|d
    I|studio.effects.interactive_effect_workspaces.vanishing_point|Vanishing Point…|d
    I|studio.effects.interactive_effect_workspaces.lens_correction|Lens Correction…|d
    I|studio.effects.interactive_effect_workspaces.wide_angle_correction|Wide Angle Correction…|d
    I|studio.effects.interactive_effect_workspaces.raw_filter|Raw Filter…|d
    I|studio.effects.interactive_effect_workspaces.blur_gallery|Blur Gallery…|d
    I|studio.effects.interactive_effect_workspaces.effect_gallery|Effect Gallery…|d
    I|studio.effects.interactive_effect_workspaces.displace|Displace…|d
    I|studio.effects.interactive_effect_workspaces.content_aware_fill|Content-Aware Fill…|d
    I|studio.effects.interactive_effect_workspaces.select_refine|Select & Refine…|d
  S|studio.effects.effect_presets_styles|Effect Presets & Styles|d
    I|studio.effects.effect_presets_styles.save_effect_preset|Save Effect Preset…|d
    I|studio.effects.effect_presets_styles.apply_effect_preset|Apply Effect Preset…|d
    I|studio.effects.effect_presets_styles.manage_effect_presets|Manage Effect Presets…|d
    I|studio.effects.effect_presets_styles.import_presets|Import Presets…|d
    I|studio.effects.effect_presets_styles.export_presets|Export Presets…|d
    I|studio.effects.effect_presets_styles.effect_styles|Effect Styles…|d
  S|studio.effects.effect_rendering|Effect Rendering|d
    I|studio.effects.effect_rendering.gpu_rendering|GPU Rendering|d
    I|studio.effects.effect_rendering.cpu_fallback|CPU Fallback|d
    I|studio.effects.effect_rendering.render_quality|Render Quality…|d
    I|studio.effects.effect_rendering.purge_effect_cache|Purge Effect Cache|d
    I|studio.effects.effect_rendering.cross_backend_equivalence_report|Cross-Backend Equivalence Report…|d
  I|studio.effects.convert_for_non_destructive_effects|Convert for Non-Destructive Effects|d
  I|studio.effects.expand_appearance|Expand Appearance|d
  I|studio.effects.rasterise_effect|Rasterise Effect|d
T|motion|MOTION|
  S|studio.motion.playback|Playback|d
    I|studio.motion.playback.play_pause|Play / Pause|d
    I|studio.motion.playback.play_in_to_out|Play In to Out|d
    I|studio.motion.playback.play_around_playhead|Play Around Playhead|d
    I|studio.motion.playback.loop|Loop|d
    I|studio.motion.playback.shuttle_left_right|Shuttle Left / Right|d
    I|studio.motion.playback.step_forward_1_frame|Step Forward 1 Frame|d
    I|studio.motion.playback.step_back_1_frame|Step Back 1 Frame|d
    I|studio.motion.playback.playback_resolution|Playback Resolution…|d
    I|studio.motion.playback.playback_options|Playback Options…|d
  S|studio.motion.go_to|Go To|d
    I|studio.motion.go_to.start_of_timeline|Start of Timeline|d
    I|studio.motion.go_to.end_of_timeline|End of Timeline|d
    I|studio.motion.go_to.next_edit_point|Next Edit Point|d
    I|studio.motion.go_to.previous_edit_point|Previous Edit Point|d
    I|studio.motion.go_to.next_keyframe|Next Keyframe|d
    I|studio.motion.go_to.previous_keyframe|Previous Keyframe|d
    I|studio.motion.go_to.next_marker|Next Marker|d
    I|studio.motion.go_to.previous_marker|Previous Marker|d
    I|studio.motion.go_to.in_point|In Point|d
    I|studio.motion.go_to.out_point|Out Point|d
    I|studio.motion.go_to.time|Time…|d
  E|studio.motion.keyframe|Keyframes|
    I|studio.motion.keyframe.set_keyframe|Set Keyframe|d
    I|studio.motion.keyframe.toggle_hold_keyframe|Toggle Hold Keyframe|d
    S|studio.motion.keyframe.interpolation|Interpolation|d
      I|studio.motion.keyframe.interpolation.linear|Linear|d
      I|studio.motion.keyframe.interpolation.bezier|Bezier|d
      I|studio.motion.keyframe.interpolation.continuous_bezier|Continuous Bezier|d
      I|studio.motion.keyframe.interpolation.auto_bezier|Auto Bezier|d
      I|studio.motion.keyframe.interpolation.hold|Hold|d
      I|studio.motion.keyframe.interpolation.current_settings|Current Settings|d
    I|studio.motion.keyframe.keyframe_velocity|Keyframe Velocity…|d
    I|studio.motion.keyframe.keyframe_assistant|Keyframe Assistant|d
    I|studio.motion.keyframe.ease_in|Ease In|d
    I|studio.motion.keyframe.ease_out|Ease Out|d
    I|studio.motion.keyframe.ease_both|Ease Both|d
    I|studio.motion.keyframe.nudge_earlier_later|Nudge Earlier / Later|d
    I|studio.motion.keyframe.select_all_visible_keyframes|Select All Visible Keyframes|d
    I|studio.motion.keyframe.deselect_all_keyframes|Deselect All Keyframes|d
    I|studio.motion.keyframe.separate_dimensions|Separate Dimensions|d
  S|studio.motion.expression|Expression|
    I|studio.motion.expression.add_expression|Add Expression|d
    I|studio.motion.expression.toggle_expression|Toggle Expression|d
    I|studio.motion.expression.convert_expression_to_keyframes|Convert Expression to Keyframes|d
    I|studio.motion.expression.show_expression_errors|Show Expression Errors|d
    I|studio.motion.expression.pick_whip|Pick Whip…|d
    I|studio.motion.expression.expression_editor_panel|Expression Editor Panel|d
  E|studio.motion.marker|Markers|
  S|studio.motion.in_out|In & Out|d
    I|studio.motion.in_out.mark_in|Mark In|d
    I|studio.motion.in_out.mark_out|Mark Out|d
    I|studio.motion.in_out.mark_clip|Mark Clip|d
    I|studio.motion.in_out.mark_selection|Mark Selection|d
    I|studio.motion.in_out.clear_in|Clear In|d
    I|studio.motion.in_out.clear_out|Clear Out|d
    I|studio.motion.in_out.clear_in_and_out|Clear In and Out|d
  S|studio.motion.edit_actions|Edit Actions|d
    I|studio.motion.edit_actions.insert|Insert|d
    I|studio.motion.edit_actions.overwrite|Overwrite|d
    I|studio.motion.edit_actions.extract|Extract|d
    I|studio.motion.edit_actions.lift|Lift|d
    I|studio.motion.edit_actions.ripple_delete|Ripple Delete|d
    I|studio.motion.edit_actions.add_edit|Add Edit|d
    I|studio.motion.edit_actions.add_edit_to_all_tracks|Add Edit to All Tracks|d
    I|studio.motion.edit_actions.split_at_playhead|Split at Playhead|d
    I|studio.motion.edit_actions.join_through_edits|Join Through Edits|d
    I|studio.motion.edit_actions.trim_in_out|Trim In / Out|d
    I|studio.motion.edit_actions.roll_edit|Roll Edit|d
    I|studio.motion.edit_actions.slip|Slip|d
    I|studio.motion.edit_actions.slide|Slide|d
    I|studio.motion.edit_actions.close_gap|Close Gap|d
    I|studio.motion.edit_actions.trim_mode|Trim Mode…|d
  S|studio.motion.tracks|Tracks|d
    I|studio.motion.tracks.add_track|Add Track…|d
    I|studio.motion.tracks.delete_track|Delete Track…|d
    I|studio.motion.tracks.add_tracks_to_match_source|Add Tracks to Match Source|d
    I|studio.motion.tracks.track_height|Track Height|d
    I|studio.motion.tracks.lock_track|Lock Track|d
    I|studio.motion.tracks.sync_lock|Sync Lock|d
    I|studio.motion.tracks.target_track|Target Track|d
    I|studio.motion.tracks.mute_solo|Mute / Solo|d
    I|studio.motion.tracks.track_output_toggle|Track Output Toggle|d
  S|studio.motion.transitions|Transitions|d
    I|studio.motion.transitions.apply_default_video_transition|Apply Default Video Transition|d
    I|studio.motion.transitions.apply_default_audio_transition|Apply Default Audio Transition|d
    I|studio.motion.transitions.apply_transition_to_selection|Apply Transition to Selection|d
    I|studio.motion.transitions.transition_duration|Transition Duration…|d
    I|studio.motion.transitions.set_default_transition|Set Default Transition…|d
    I|studio.motion.transitions.transition_presets|Transition Presets…|d
  S|studio.motion.speed_time|Speed & Time|d
    I|studio.motion.speed_time.speed_duration|Speed / Duration…|d
    I|studio.motion.speed_time.time_remap|Time Remap|d
    I|studio.motion.speed_time.reverse|Reverse|d
    I|studio.motion.speed_time.freeze_frame|Freeze Frame…|d
    I|studio.motion.speed_time.frame_blending|Frame Blending|d
    I|studio.motion.speed_time.optical_flow|Optical Flow|d
    I|studio.motion.speed_time.time_stretch|Time Stretch…|d
  S|studio.motion.audio|Audio|d
    I|studio.motion.audio.audio_gain|Audio Gain…|d
    I|studio.motion.audio.normalise|Normalise…|d
    I|studio.motion.audio.audio_channels|Audio Channels…|d
    I|studio.motion.audio.generate_audio_waveform|Generate Audio Waveform|d
    I|studio.motion.audio.auto_ducking|Auto-Ducking…|d
    I|studio.motion.audio.add_submix_track|Add Submix Track|d
    I|studio.motion.audio.mute|Mute|d
    I|studio.motion.audio.solo|Solo|d
    I|studio.motion.audio.loudness_report|Loudness Report…|d
  S|studio.motion.captions|Captions|d
    I|studio.motion.captions.new_caption_track|New Caption Track…|d
    I|studio.motion.captions.import_captions|Import Captions…|d
    I|studio.motion.captions.export_captions|Export Captions…|d
    I|studio.motion.captions.caption_settings|Caption Settings…|d
    I|studio.motion.captions.merge_captions|Merge Captions|d
    I|studio.motion.captions.split_caption|Split Caption|d
    I|studio.motion.captions.transcribe|Transcribe…|d
    I|studio.motion.captions.caption_sidecar_settings|Caption Sidecar Settings…|d
  S|studio.motion.tracking|Tracking|
    I|studio.motion.tracking.track_motion|Track Motion|d
    I|studio.motion.tracking.track_camera|Track Camera|d
    I|studio.motion.tracking.track_mask|Track Mask|d
    I|studio.motion.tracking.track_this_property|Track This Property|d
    I|studio.motion.tracking.stabilise|Stabilise|d
    I|studio.motion.tracking.tracker_panel|Tracker Panel|d
    I|studio.motion.tracking.mask_tracking_options|Mask Tracking Options…|d
  S|studio.motion.render_preview|Render & Preview|d
    I|studio.motion.render_preview.render_in_to_out|Render In to Out|d
    I|studio.motion.render_preview.render_effects_in_work_area|Render Effects in Work Area|d
    I|studio.motion.render_preview.render_audio|Render Audio|d
    I|studio.motion.render_preview.render_entire_timeline|Render Entire Timeline|d
    I|studio.motion.render_preview.delete_render_files|Delete Render Files|d
    I|studio.motion.render_preview.preview_quality|Preview Quality…|d
    I|studio.motion.render_preview.cache_settings|Cache Settings…|d
    I|studio.motion.render_preview.render_queue_panel|Render Queue Panel|d
  S|studio.motion.motion_presets|Motion Presets|d
    I|studio.motion.motion_presets.apply_animation_style|Apply Animation Style…|d
    I|studio.motion.motion_presets.save_animation_style|Save Animation Style…|d
    I|studio.motion.motion_presets.manage_motion_presets|Manage Motion Presets…|d
    I|studio.motion.motion_presets.install_motion_template|Install Motion Template…|d
    I|studio.motion.motion_presets.browse_presets|Browse Presets…|d
    I|studio.motion.motion_presets.recent_animation_presets|Recent Animation Presets|d
  S|studio.motion.motion_aids|Motion Aids|d
    I|studio.motion.motion_aids.onion_skin|Onion Skin|d
    I|studio.motion.motion_aids.onion_skin_options|Onion Skin Options…|d
    I|studio.motion.motion_aids.motion_blur|Motion Blur|d
    I|studio.motion.motion_aids.motion_path_display|Motion Path Display|d
    I|studio.motion.motion_aids.ghosting|Ghosting…|d
  S|studio.motion.reveal|Reveal|
    I|studio.motion.reveal.reveal_properties_with_keyframes|Reveal Properties with Keyframes|d
    I|studio.motion.reveal.reveal_all_modified_properties|Reveal All Modified Properties|d
    I|studio.motion.reveal.reveal_effects|Reveal Effects|d
    I|studio.motion.reveal.reveal_expressions|Reveal Expressions|d
    I|studio.motion.reveal.extend_reveal_to_selection|Extend Reveal to Selection|d
  S|studio.motion.prototype|Prototype|d
    I|studio.motion.prototype.flows|Flows|d
    I|studio.motion.prototype.connections|Connections|d
    I|studio.motion.prototype.triggers|Triggers|d
    I|studio.motion.prototype.actions|Actions|d
    I|studio.motion.prototype.animation|Animation|d
    I|studio.motion.prototype.overlays_scroll|Overlays & Scroll|d
    I|studio.motion.prototype.presentation|Presentation|d
    I|studio.motion.prototype.interactive_components|Interactive Components…|d
    I|studio.motion.prototype.interactive_document_settings|Interactive Document Settings…|d
  I|studio.motion.auto_keyframe|Enable Auto-Keyframe|
T|insert|INSERT|
  I|studio.insert.place|Place…|d
  S|studio.insert.layer|Layer|d
    I|studio.insert.layer.pixel_layer|Pixel Layer|d
    I|studio.insert.layer.vector_layer|Vector Layer|d
    I|studio.insert.layer.text_layer|Text Layer|d
    I|studio.insert.layer.adjustment_layer|Adjustment Layer…|d
    I|studio.insert.layer.fill_layer|Fill Layer…|d
    I|studio.insert.layer.live_filter_layer|Live Filter Layer…|d
    I|studio.insert.layer.mask_layer|Mask Layer|d
    I|studio.insert.layer.group|Group|d
    I|studio.insert.layer.frame|Frame|d
    I|studio.insert.layer.artboard|Artboard|d
    I|studio.insert.layer.solid|Solid|d
    I|studio.insert.layer.null|Null|d
    I|studio.insert.layer.camera|Camera…|d
    I|studio.insert.layer.light|Light…|d
    I|studio.insert.layer.guide_layer|Guide Layer|d
    I|studio.insert.layer.adjustment_layer_2|Adjustment Layer|d
    I|studio.insert.layer.composition_layer|Composition Layer|d
  E|studio.insert.shape|Shape|
  S|studio.insert.text|Text|d
    I|studio.insert.text.point_text|Point Text|d
    I|studio.insert.text.area_text|Area Text|d
    I|studio.insert.text.text_on_path|Text on Path|d
    I|studio.insert.text.text_frame|Text Frame|d
    I|studio.insert.text.frame_grid|Frame Grid|d
    I|studio.insert.text.placeholder_text|Placeholder Text|d
    I|studio.insert.text.filler_text|Filler Text…|d
  S|studio.insert.table|Table|d
    I|studio.insert.table.insert_table|Insert Table…|d
    I|studio.insert.table.convert_text_to_table|Convert Text to Table…|d
    I|studio.insert.table.insert_rows|Insert Rows…|d
    I|studio.insert.table.insert_columns|Insert Columns…|d
  S|studio.insert.page_element|Page Element|d
    I|studio.insert.page_element.current_page_number|Current Page Number|d
    I|studio.insert.page_element.next_page_number|Next Page Number|d
    I|studio.insert.page_element.previous_page_number|Previous Page Number|d
    I|studio.insert.page_element.section_marker|Section Marker|d
    I|studio.insert.page_element.running_header|Running Header…|d
    I|studio.insert.page_element.footnote|Footnote|d
    I|studio.insert.page_element.endnote|Endnote|d
    I|studio.insert.page_element.sidenote|Sidenote|d
    I|studio.insert.page_element.index_marker|Index Marker|d
    I|studio.insert.page_element.cross_reference|Cross-Reference…|d
    I|studio.insert.page_element.hyperlink|Hyperlink…|d
    I|studio.insert.page_element.bookmark|Bookmark…|d
    I|studio.insert.page_element.anchor|Anchor…|d
    I|studio.insert.page_element.text_variable|Text Variable…|d
    I|studio.insert.page_element.conditional_text|Conditional Text…|d
  S|studio.insert.component_library|Component & Library|d
    I|studio.insert.component_library.insert_component|Insert Component…|d
    I|studio.insert.component_library.insert_instance|Insert Instance…|d
    I|studio.insert.component_library.insert_from_library|Insert from Library…|d
    I|studio.insert.component_library.insert_symbol|Insert Symbol…|d
    I|studio.insert.component_library.insert_variable|Insert Variable…|d
    I|studio.insert.component_library.insert_style_token|Insert Style Token…|d
  S|studio.insert.media|Media|d
    I|studio.insert.media.image|Image…|d
    I|studio.insert.media.video|Video…|d
    I|studio.insert.media.audio|Audio…|d
    I|studio.insert.media.animated_gif|Animated GIF…|d
    I|studio.insert.media.3d_model|3D Model…|d
    I|studio.insert.media.svg|SVG…|d
    I|studio.insert.media.pdf_page|PDF Page…|d
    I|studio.insert.media.barcode_qr_code|Barcode / QR Code…|d
    I|studio.insert.media.colour_bars_tone|Colour Bars & Tone|d
    I|studio.insert.media.counting_leader|Counting Leader|d
    I|studio.insert.media.black_video_transparent_video|Black Video / Transparent Video|d
  E|studio.insert.web|Web Element|
  S|studio.insert.whiteboard_object|Whiteboard Object|d
    I|studio.insert.whiteboard_object.sticky_note|Sticky Note|d
    I|studio.insert.whiteboard_object.connector|Connector|d
    I|studio.insert.whiteboard_object.section|Section|d
    I|studio.insert.whiteboard_object.stamp|Stamp|d
    I|studio.insert.whiteboard_object.vote_marker|Vote Marker|d
    I|studio.insert.whiteboard_object.timer_widget|Timer Widget|d
    I|studio.insert.whiteboard_object.diagram_from_text|Diagram from Text…|d
    I|studio.insert.whiteboard_object.widget|Widget…|d
  S|studio.insert.snippet_sample|Snippet & Sample|d
    I|studio.insert.snippet_sample.insert_snippet|Insert Snippet…|d
    I|studio.insert.snippet_sample.new_snippet|New Snippet…|d
    I|studio.insert.snippet_sample.recent_snippets|Recent Snippets|d
    I|studio.insert.snippet_sample.sample_content|Sample Content…|d
  I|studio.insert.insert_marker|Insert Marker|ad
  I|studio.insert.insert_special_character|Insert Special Character|ad
T|code|CODE|
  S|studio.code.view_mode|View Mode|d
    I|studio.code.view_mode.code|Code|d
    I|studio.code.view_mode.split|Split|d
    I|studio.code.view_mode.design|Design|d
    I|studio.code.view_mode.live|Live|d
    I|studio.code.view_mode.split_vertically_horizontally|Split Vertically / Horizontally|d
    I|studio.code.view_mode.switch_views|Switch Views|d
  S|studio.code.source_formatting|Source Formatting|d
    I|studio.code.source_formatting.apply_source_formatting|Apply Source Formatting|d
    I|studio.code.source_formatting.apply_to_selection|Apply to Selection|d
    I|studio.code.source_formatting.indent|Indent|d
    I|studio.code.source_formatting.outdent|Outdent|d
    I|studio.code.source_formatting.balance_braces|Balance Braces|d
    I|studio.code.source_formatting.convert_tabs_to_spaces|Convert Tabs to Spaces|d
    I|studio.code.source_formatting.code_format_settings|Code Format Settings…|d
    I|studio.code.source_formatting.clean_up_markup|Clean Up Markup…|d
  S|studio.code.edit_code|Edit Code|d
    I|studio.code.edit_code.comment_selection|Comment Selection|d
    I|studio.code.edit_code.uncomment_selection|Uncomment Selection|d
    I|studio.code.edit_code.wrap_tag|Wrap Tag…|d
    I|studio.code.edit_code.remove_tag|Remove Tag|d
    I|studio.code.edit_code.quick_tag_editor|Quick Tag Editor…|d
    I|studio.code.edit_code.edit_tag|Edit Tag…|d
    I|studio.code.edit_code.collapse_selection|Collapse Selection|d
    I|studio.code.edit_code.collapse_outside_selection|Collapse Outside Selection|d
    I|studio.code.edit_code.expand_all|Expand All|d
    I|studio.code.edit_code.go_to_line|Go to Line…|d
    I|studio.code.edit_code.toggle_breakpoint|Toggle Breakpoint|d
  S|studio.code.refactor|Refactor|d
    I|studio.code.refactor.rename_symbol|Rename Symbol…|d
    I|studio.code.refactor.extract_to_style_sheet|Extract to Style Sheet…|d
    I|studio.code.refactor.extract_to_include|Extract to Include…|d
    I|studio.code.refactor.move_inline_styles|Move Inline Styles…|d
    I|studio.code.refactor.convert_table_to_layout|Convert Table to Layout…|d
  S|studio.code.style_sheets|Style Sheets|d
    I|studio.code.style_sheets.new_rule|New Rule…|d
    I|studio.code.style_sheets.edit_rule|Edit Rule…|d
    I|studio.code.style_sheets.attach_style_sheet|Attach Style Sheet…|d
    I|studio.code.style_sheets.design_time_style_sheets|Design-Time Style Sheets…|d
    I|studio.code.style_sheets.media_queries|Media Queries…|d
    I|studio.code.style_sheets.transitions|Transitions…|d
    I|studio.code.style_sheets.css_designer|CSS Designer|d
    I|studio.code.style_sheets.export_styles_as_css|Export Styles as CSS…|d
  S|studio.code.validate|Validate|d
    I|studio.code.validate.validate_markup|Validate Markup|d
    I|studio.code.validate.validate_style_sheets|Validate Style Sheets|d
    I|studio.code.validate.browser_compatibility_check|Browser Compatibility Check|d
    I|studio.code.validate.accessibility_check|Accessibility Check|d
    I|studio.code.validate.check_links|Check Links|d
    I|studio.code.validate.check_spelling|Check Spelling|d
    I|studio.code.validate.validation_report|Validation Report…|d
  S|studio.code.assistance|Assistance|d
    I|studio.code.assistance.code_hints|Code Hints|d
    I|studio.code.assistance.show_code_hints_now|Show Code Hints Now|d
    I|studio.code.assistance.code_hint_preferences|Code Hint Preferences…|d
    I|studio.code.assistance.abbreviation_expansion|Abbreviation Expansion|d
    I|studio.code.assistance.snippets|Snippets…|d
    I|studio.code.assistance.live_code|Live Code|d
    I|studio.code.assistance.inspect_mode|Inspect Mode|d
  S|studio.code.server|Server|d
    I|studio.code.server.server_behaviours|Server Behaviours…|d
    I|studio.code.server.bindings|Bindings…|d
    I|studio.code.server.databases|Databases…|d
    I|studio.code.server.test_connection|Test Connection|d
    I|studio.code.server.define_data_source|Define Data Source…|d
    I|studio.code.server.server_settings|Server Settings…|d
  I|studio.code.open_in_shell|Open in Shell Editor|
T|automate|AUTOMATE|
  S|studio.automate.actions|Actions|d
    I|studio.automate.actions.new_action|New Action…|d
    I|studio.automate.actions.new_action_set|New Action Set…|d
    I|studio.automate.actions.record|Record|d
    I|studio.automate.actions.stop_recording|Stop Recording|d
    I|studio.automate.actions.play_action|Play Action|d
    I|studio.automate.actions.insert_stop|Insert Stop…|d
    I|studio.automate.actions.insert_menu_item|Insert Menu Item…|d
    I|studio.automate.actions.insert_path|Insert Path|d
    I|studio.automate.actions.insert_conditional|Insert Conditional…|d
    I|studio.automate.actions.action_options|Action Options…|d
    I|studio.automate.actions.playback_options|Playback Options…|d
    I|studio.automate.actions.load_actions|Load Actions…|d
    I|studio.automate.actions.save_actions|Save Actions…|d
    I|studio.automate.actions.reset_actions|Reset Actions|d
    I|studio.automate.actions.clear_all_actions|Clear All Actions|d
  S|studio.automate.macros|Macros|d
    I|studio.automate.macros.record_macro|Record Macro|d
    I|studio.automate.macros.play_macro|Play Macro|d
    I|studio.automate.macros.edit_macro|Edit Macro…|d
    I|studio.automate.macros.delete_macro|Delete Macro|d
    I|studio.automate.macros.macro_library|Macro Library…|d
    I|studio.automate.macros.duplicate_macro_category|Duplicate Macro Category|d
  S|studio.automate.batch|Batch|d
    I|studio.automate.batch.batch|Batch…|d
    I|studio.automate.batch.create_droplet|Create Droplet…|d
    I|studio.automate.batch.image_processor|Image Processor…|d
    I|studio.automate.batch.batch_rename|Batch Rename…|d
    I|studio.automate.batch.batch_export|Batch Export…|d
    I|studio.automate.batch.batch_preflight|Batch Preflight…|d
    I|studio.automate.batch.batch_convert_format|Batch Convert Format…|d
    I|studio.automate.batch.batch_progress|Batch Progress…|d
  S|studio.automate.scripts|Scripts|d
    I|studio.automate.scripts.run_script|Run Script…|d
    I|studio.automate.scripts.browse_for_script|Browse for Script…|d
    I|studio.automate.scripts.script_editor|Script Editor…|d
    I|studio.automate.scripts.script_library|Script Library…|d
    I|studio.automate.scripts.script_properties|Script Properties…|d
    I|studio.automate.scripts.script_events|Script Events…|d
    I|studio.automate.scripts.enable_attached_scripts|Enable Attached Scripts|d
    I|studio.automate.scripts.reload_scripts|Reload Scripts|d
    I|studio.automate.scripts.script_console|Script Console|d
  S|studio.automate.command_api|Command API|dm
    I|studio.automate.command_api.command_browser|Command Browser…|d
    I|studio.automate.command_api.dry_run_command|Dry Run Command…|d
    I|studio.automate.command_api.command_console|Command Console|d
    I|studio.automate.command_api.descriptor_inspector|Descriptor Inspector…|d
    I|studio.automate.command_api.copy_command_id|Copy Command Id|d
    I|studio.automate.command_api.copy_command_json|Copy Command JSON|d
    I|studio.automate.command_api.replay_command_batch|Replay Command Batch…|d
    I|studio.automate.command_api.export_command_index|Export Command Index…|d
  S|studio.automate.data_driven|Data-Driven|d
    I|studio.automate.data_driven.data_merge|Data Merge…|d
    I|studio.automate.data_driven.select_data_source|Select Data Source…|d
    I|studio.automate.data_driven.preview_data|Preview Data|d
    I|studio.automate.data_driven.create_merged_document|Create Merged Document…|d
    I|studio.automate.data_driven.define_variables|Define Variables…|d
    I|studio.automate.data_driven.data_sets|Data Sets…|d
    I|studio.automate.data_driven.import_data_set|Import Data Set…|d
    I|studio.automate.data_driven.export_data_set|Export Data Set…|d
    I|studio.automate.data_driven.xml_import|XML Import…|d
    I|studio.automate.data_driven.xml_export|XML Export…|d
    I|studio.automate.data_driven.convert_text_to_field|Convert Text to Field…|d
  S|studio.automate.plugins|Plugins|d
    I|studio.automate.plugins.manage_plugins|Manage Plugins…|d
    I|studio.automate.plugins.install_plugin_from_file|Install Plugin from File…|d
    I|studio.automate.plugins.plugin_capabilities|Plugin Capabilities…|d
    I|studio.automate.plugins.plugin_console|Plugin Console|d
    I|studio.automate.plugins.reload_plugins|Reload Plugins|d
    I|studio.automate.plugins.plugin_sandbox_report|Plugin Sandbox Report…|d
    E|studio.automate.plugins.installed_plugin_commands|Installed plugin commands|dg
  S|studio.automate.model_lane|Model Lane|dm
    I|studio.automate.model_lane.propose_edit|Propose Edit…|d
    I|studio.automate.model_lane.review_proposals|Review Proposals…|d
    I|studio.automate.model_lane.approval_inbox|Approval Inbox|d
    I|studio.automate.model_lane.validation_report|Validation Report…|d
    I|studio.automate.model_lane.replay_proposal|Replay Proposal…|d
    I|studio.automate.model_lane.rejected_proposals|Rejected Proposals…|d
    I|studio.automate.model_lane.actor_attribution|Actor Attribution…|d
    I|studio.automate.model_lane.lease_status|Lease Status…|d
  S|studio.automate.jobs|Jobs|d
    I|studio.automate.jobs.job_queue|Job Queue|d
    I|studio.automate.jobs.cancel_job|Cancel Job|d
    I|studio.automate.jobs.job_log|Job Log…|d
    I|studio.automate.jobs.render_workers|Render Workers…|d
    I|studio.automate.jobs.process_ledger|Process Ledger…|d
  S|studio.automate.find_change_as_batch|Find & Change as Batch|d
    I|studio.automate.find_change_as_batch.run_query_on_document|Run Query on Document|d
    I|studio.automate.find_change_as_batch.run_query_on_book|Run Query on Book|d
    I|studio.automate.find_change_as_batch.run_query_on_folder|Run Query on Folder…|d
    I|studio.automate.find_change_as_batch.run_query_on_site|Run Query on Site…|d
    I|studio.automate.find_change_as_batch.save_query|Save Query…|d
T|workspace|WORKSPACE|
  S|studio.workspace.layout_preset|Layout Presets|
    E|studio.workspace.layout_preset.one_leaf_per_shipped_layout_preset|One leaf per shipped Layout Preset|dg
    I|studio.workspace.layout_preset.save_layout_preset|Save Layout Preset…|d
    I|studio.workspace.layout_preset.reset_layout|Reset Layout|d
    I|studio.workspace.layout_preset.manage_layout_presets|Manage Layout Presets…|d
    I|studio.workspace.layout_preset.next_layout_preset|Next Layout Preset|d
  E|studio.workspace.panel|Panels|
  E|studio.workspace.tool|Tools|
  S|studio.workspace.panel_layout|Panel Layout|d
    I|studio.workspace.panel_layout.save_layout_preset|Save Layout Preset…|d
    I|studio.workspace.panel_layout.manage_layout_presets|Manage Layout Presets…|d
    I|studio.workspace.panel_layout.reset_layout|Reset Layout|d
    I|studio.workspace.panel_layout.import_layout_preset|Import Layout Preset…|d
    I|studio.workspace.panel_layout.export_layout_preset|Export Layout Preset…|d
    I|studio.workspace.panel_layout.collapse_all_panels|Collapse All Panels|d
    I|studio.workspace.panel_layout.expand_all_panels|Expand All Panels|d
    I|studio.workspace.panel_layout.float_panel|Float Panel|d
    I|studio.workspace.panel_layout.dock_panel|Dock Panel|d
    I|studio.workspace.panel_layout.hide_all_panels|Hide All Panels|d
  S|studio.workspace.toolbox|Toolbox|d
    I|studio.workspace.toolbox.show_tool_rail|Show Tool Rail|d
    I|studio.workspace.toolbox.customise_tool_rail|Customise Tool Rail…|d
    I|studio.workspace.toolbox.tool_presets|Tool Presets…|d
    I|studio.workspace.toolbox.show_context_bar|Show Context Bar|d
    I|studio.workspace.toolbox.reset_all_tools|Reset All Tools|d
    I|studio.workspace.toolbox.tool_cycle_groups|Tool Cycle Groups…|d
  S|studio.workspace.chrome|Chrome|d
    I|studio.workspace.chrome.context_bar|Context Bar|d
    I|studio.workspace.chrome.status_bar|Status Bar|d
    I|studio.workspace.chrome.ruler_bar|Ruler Bar|d
    I|studio.workspace.chrome.rails|Rails…|d
    I|studio.workspace.chrome.application_frame|Application Frame|d
  S|studio.workspace.presence|Presence|d
    I|studio.workspace.presence.show_collaborators|Show Collaborators|d
    I|studio.workspace.presence.show_agent_activity|Show Agent Activity|d
    I|studio.workspace.presence.follow_actor|Follow Actor…|d
    I|studio.workspace.presence.conflict_view|Conflict View|d
    I|studio.workspace.presence.presence_settings|Presence Settings…|d
  S|studio.workspace.arrange_documents|Arrange Documents|d
    I|studio.workspace.arrange_documents.new_editor_group_for_document|New Editor Group for Document|d
    I|studio.workspace.arrange_documents.cascade|Cascade|d
    I|studio.workspace.arrange_documents.tile|Tile|d
    I|studio.workspace.arrange_documents.consolidate_all|Consolidate All|d
    I|studio.workspace.arrange_documents.float_all_in_windows|Float All in Windows|d
    I|studio.workspace.arrange_documents.move_document_to_group|Move Document to Group…|d
    I|studio.workspace.arrange_documents.next_document|Next Document|d
    I|studio.workspace.arrange_documents.previous_document|Previous Document|d
    I|studio.workspace.arrange_documents.split_document_view|Split Document View|d
  S|studio.workspace.task_scope|Task Scopes|
    E|studio.workspace.task_scope.one_leaf_per_shipped_task_scope|One leaf per shipped Task Scope|dg
    I|studio.workspace.task_scope.exit_task_scope|Exit Task Scope|d
T|go|GO|
T|run|RUN|
T|help|HELP|
  S|studio.help.studio_manual|Studio Manual|d
    I|studio.help.studio_manual.open_studio_manual|Open Studio Manual|d
    I|studio.help.studio_manual.search_manual|Search Manual…|d
    I|studio.help.studio_manual.help_for_this_command|Help for This Command|d
    I|studio.help.studio_manual.help_for_this_panel|Help for This Panel|d
    I|studio.help.studio_manual.help_for_this_tool|Help for This Tool|d
    I|studio.help.studio_manual.domain_guides|Domain Guides…|d
  I|studio.help.full_command_index|Full Command Index…|dm
  I|studio.help.keyboard_shortcut_reference|Keyboard Shortcut Reference…|d
  S|studio.help.diagnostics|Diagnostics|dm
    I|studio.help.diagnostics.render_harness|Render Harness…|d
    I|studio.help.diagnostics.visual_debugger|Visual Debugger|d
    I|studio.help.diagnostics.flight_recorder|Flight Recorder|d
    I|studio.help.diagnostics.validation_report|Validation Report…|d
    I|studio.help.diagnostics.system_report|System Report…|d
    I|studio.help.diagnostics.gpu_report|GPU Report…|d
    I|studio.help.diagnostics.accessibility_tree_snapshot|Accessibility Tree Snapshot…|d
  I|studio.help.about_studio|About Studio|d
T|editors|EDITORS|
T|operator|OPERATOR|
"#;

struct Row<'a> {
    depth: usize,
    kind: &'a str,
    id: &'a str,
    label: &'a str,
    flags: &'a str,
}

fn rows_of(text: &str) -> Vec<Row<'_>> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| {
            let body = l.trim_start_matches(' ');
            let depth = l.len().saturating_sub(body.len()).checked_div(2).unwrap_or(0);
            let mut f = body.splitn(4, '|');
            Some(Row { depth, kind: f.next()?, id: f.next()?, label: f.next()?, flags: f.next().unwrap_or("") })
        })
        .collect()
}

fn title_clause(id: &str) -> u16 {
    match id {
        "file" => 23,
        "edit" => 24,
        "view" => 25,
        "document" => 26,
        "select" => 27,
        "object" => 28,
        "type" => 29,
        "color" => 30,
        "effects" => 31,
        "motion" => 32,
        "insert" => 33,
        "code" => 34,
        "automate" => 35,
        "workspace" => 36,
        "help" => 37,
        _ => 0,
    }
}

fn fold(rows: &[Row<'_>], at: &mut usize, depth: usize, clause: u16, meta: &mut BTreeMap<String, Meta>) -> Vec<MenuNode> {
    let mut out = Vec::new();
    while let Some(r) = rows.get(*at).filter(|r| r.depth == depth) {
        *at = at.saturating_add(1);
        let clause = if r.kind == "T" { title_clause(r.id) } else { clause };
        let children = if rows.get(*at).is_some_and(|n| n.depth > depth) { fold(rows, at, depth.saturating_add(1), clause, meta) } else { Vec::new() };
        let flag = |c: char| r.flags.contains(c);
        let mut n = match r.kind {
            "T" => MenuNode::title(r.id, r.label, children),
            "I" => MenuNode::item(r.id, r.label),
            _ => MenuNode::submenu(r.id, r.label, children),
        };
        if flag('m') {
            n = n.menu_only();
        }
        if let Some((_, primary)) = ALIASES.iter().find(|(a, _)| *a == r.id) {
            n = n.alias(primary);
        }
        meta.insert(r.id.to_owned(), Meta { expansion: r.kind == "E", derived: flag('d'), generated: flag('g'), enumeration: flag('n'), alias: flag('a'), clause });
        out.push(n);
    }
    out
}

/// The designed tree, parsed once.
pub fn template() -> &'static Template {
    static T: OnceLock<Template> = OnceLock::new();
    T.get_or_init(|| {
        let rows = rows_of(TREE);
        let mut meta = BTreeMap::new();
        let mut at = 0usize;
        let titles = fold(&rows, &mut at, 0, 0, &mut meta);
        Template { bar: MenuBar::new(titles), meta }
    })
}

fn nyi(text: &str) -> Availability {
    Availability::not_in_document(ReasonCode::NotImplementedYet, text, None)
}

/// The text of a node no host runs yet.
pub const NOT_BUILT_TEXT: &str = "not built in this host yet";
/// The text of a shell command or panel the host does not show (reason NOT_IN_THIS_HOST).
pub const NOT_IN_THIS_HOST_TEXT: &str = "not shown in this window";

fn requires_of(texts: &str) -> Vec<Clause> {
    // The tables are this module's constants; a malformed one is a test failure (designed_tree...), never a panic.
    parse_requires(texts).unwrap_or_default()
}

/// What the host showing the menu can show: a shell command it cannot show is NOT_IN_THIS_HOST (lead ruling 2). The
/// process drive surface and the egui shell show every shell command ([`MenuContext::everything`]).
pub struct MenuContext<'a> {
    pub tool: Option<&'a str>,
    pub hosts: &'a dyn Fn(ShellCommand) -> bool,
}

impl MenuContext<'static> {
    /// A host that shows every shell command (the egui shell, the drive surface).
    pub fn everything() -> MenuContext<'static> {
        MenuContext { tool: None, hosts: &|_| true }
    }
}

/// The shell's own subtrees that fill designed nodes: FILE > Open Recent (recent files, Clear Recent) and WORKSPACE >
/// Panels (the panel registry).
pub const SHELL_FILLED: [&str; 2] = ["studio.document.open_recent", "studio.workspace.panel"];

impl Viewer {
    /// The document profile (one function for every host).
    pub fn menu_profile(&self) -> Profile {
        Profile::of(self)
    }

    /// THE menu evaluation ([STU-SHL-046], MENU-INV-4; module docs): the designed tree with the shell's subtrees
    /// grafted, one record per node, and the state it read.
    pub fn studio_menu(&self, ctx: &MenuContext<'_>) -> (MenuBar, MenuState) {
        let state = MenuState::of(self, ctx.tool);
        let shell = self.menu_bar();
        // The shell's records by id, read once: no designed node walks the shell tree (review F11).
        let walked = shell.walk();
        let records: HashMap<&str, &MenuNode> = walked.iter().map(|w| (w.node.id.as_str(), w.node)).collect();
        let t = template();
        let mut bar = t.bar.clone();
        fn graft(n: &mut MenuNode, records: &HashMap<&str, &MenuNode>) {
            if SHELL_FILLED.contains(&n.id.as_str())
                && let Some(s) = records.get(n.id.as_str())
            {
                n.children = s.children.clone();
            }
            for c in &mut n.children {
                graft(c, records);
            }
        }
        fn apply(n: &mut MenuNode, t: &Template, records: &HashMap<&str, &MenuNode>, state: &MenuState, ctx: &MenuContext<'_>) {
            for c in &mut n.children {
                apply(c, t, records, state, ctx);
            }
            if let Some(s) = records.get(n.id.as_str())
                && let Some(cmd) = ShellCommand::from_id(&n.id)
            {
                let mut a = s.availability.clone();
                if a.is_available() && !(ctx.hosts)(cmd) {
                    a = Availability::not_in_document(ReasonCode::NotInThisHost, NOT_IN_THIS_HOST_TEXT, None);
                }
                n.availability = a;
                n.selected = s.selected;
                return;
            }
            let meta = t.meta.get(&n.id).copied().unwrap_or_default();
            let requires = match leaf(&n.id) {
                Some(l) => Some(requires_of(l.requires)),
                None => NODE_REQUIRES.iter().find(|(id, _)| *id == n.id).map(|(_, r)| requires_of(r)),
            };
            let buildable = leaf(&n.id).is_some() || (n.kind != MenuNodeKind::Item && !n.children.is_empty());
            n.availability = if !buildable {
                match n.kind {
                    MenuNodeKind::Title => nyi("a shell menu; not in this host yet"),
                    MenuNodeKind::Item => nyi(NOT_BUILT_TEXT),
                    _ if meta.generated => nyi("generated from its registry; not in this host yet"),
                    _ if meta.enumeration => nyi("enumeration not listed yet"),
                    _ => nyi("leaf source not recovered yet (STU-SHL-038)"),
                }
            } else {
                let unmet = requires.map(|r| evaluate(&r, state)).unwrap_or_default();
                match unmet.as_slice() {
                    [] => Availability::available(),
                    [u] => match remedy_of(&u.clause) {
                        Some(r) => Availability::inapplicable(u.code, &u.text, &r),
                        None => Availability::not_in_document(u.code, &u.text, Some(&u.clause.text())),
                    },
                    [u, ..] => Availability::not_in_document(u.code, &u.text, Some(&u.clause.text())),
                }
            };
        }
        for title in &mut bar.titles {
            graft(title, &records);
            apply(title, t, &records, &state, ctx);
        }
        // A remedy is kept only when the remedy leaf is available now (one step makes the node available, lead ruling
        // F6); the shell's records keep the shell's remedies when this host can run them.
        fn usable_ids(n: &MenuNode, out: &mut HashSet<String>) {
            if n.kind == MenuNodeKind::Item && n.availability.is_available() {
                out.insert(n.id.clone());
            }
            for c in &n.children {
                usable_ids(c, out);
            }
        }
        let mut usable = HashSet::new();
        for title in &bar.titles {
            usable_ids(title, &mut usable);
        }
        fn fix(n: &mut MenuNode, usable: &HashSet<String>) {
            if n.availability.state == AvailabilityState::InapplicableHere && !n.availability.remedy_command_id.as_ref().is_some_and(|r| usable.contains(r)) {
                n.availability.state = AvailabilityState::NotInThisDocument;
                n.availability.remedy_command_id = None;
            }
            for c in &mut n.children {
                fix(c, usable);
            }
        }
        for title in &mut bar.titles {
            fix(title, &usable);
        }
        // An alias shows its primary's record (MENU-INV-2).
        let primaries: HashMap<&str, Availability> = ALIASES.iter().filter_map(|(_, p)| bar.find(p).map(|m| (*p, m.availability.clone()))).collect();
        fn alias(n: &mut MenuNode, primaries: &HashMap<&str, Availability>) {
            if let Some(a) = n.alias_of.as_deref().and_then(|p| primaries.get(p)) {
                n.availability = a.clone();
            }
            for c in &mut n.children {
                alias(c, primaries);
            }
        }
        for title in &mut bar.titles {
            alias(title, &primaries);
        }
        (bar, state)
    }

    /// Evaluates the menu for `ctx` and runs leaf `id` ([`Viewer::run_menu_leaf_in`]): the drive surface's path for a
    /// designed leaf that is not a shell command.
    pub fn run_menu_leaf(&mut self, ctx: &MenuContext<'_>, id: &str) -> Result<MenuRun, Box<MenuRefusal>> {
        let (bar, state) = self.studio_menu(ctx);
        self.run_menu_leaf_in(&bar, &state, id)
    }

    /// Runs leaf `id` of `bar` (the evaluation the host rendered, MENU-INV-4): a node that is not available is refused
    /// with its record and runs nothing; an alias runs its primary; a shell leaf runs `Viewer::shell_command`; a catalog
    /// leaf runs [`run_catalog`] with [`leaf_params`].
    pub fn run_menu_leaf_in(&mut self, bar: &MenuBar, state: &MenuState, id: &str) -> Result<MenuRun, Box<MenuRefusal>> {
        let node = bar.find(id).ok_or_else(|| MenuRefusal::new("menu_node_unknown"))?;
        if node.kind != MenuNodeKind::Item {
            return Err(MenuRefusal::new("menu_not_a_leaf"));
        }
        if !node.availability.is_available() {
            return Err(Box::new(MenuRefusal { code: reason_code_lower(&node.availability), availability: Some(node.availability.clone()), command: None, detail: node.availability.json() }));
        }
        let target = node.alias_of.clone().unwrap_or_else(|| id.to_owned());
        // The step this leaf made, if any: a top step whose correlation was not on the stack before (an undo exposes an
        // older step; a capped stack keeps its length).
        let steps = |v: &Viewer| v.run.history()["undo_steps"].as_array().map(|s| s.iter().map(|x| x["correlation"].clone()).collect::<Vec<_>>()).unwrap_or_default();
        let steps_before = steps(self);
        let ops_before = self.run.ops().len();
        let (command, params, result) = if let Some(cmd) = ShellCommand::from_id(&target) {
            (target.clone(), BTreeMap::new(), self.shell_command(cmd).map(|()| Value::Null))
        } else if let Some(l) = leaf(&target) {
            let params = leaf_params(self, l, state).map_err(|e| Box::new(MenuRefusal { command: Some(l.command.to_owned()), ..*e }))?;
            match run_catalog(self, l.command, &params) {
                Some(Ok(r)) => (l.command.to_owned(), params, r),
                Some(Err(e)) => return Err(Box::new(MenuRefusal { code: e.code.to_owned(), availability: None, command: Some(l.command.to_owned()), detail: e.detail })),
                None => return Err(MenuRefusal::new("menu_not_runnable")),
            }
        } else {
            return Err(MenuRefusal::new("menu_not_runnable"));
        };
        match result {
            Ok(value) => {
                let made = steps(self).last().is_some_and(|c| !steps_before.contains(c));
                let undo_name = made.then(|| self.run.history()["undo"].as_array().and_then(|s| s.last()).cloned()).flatten();
                Ok(MenuRun { id: id.to_owned(), command, params, result: value, undo_name })
            }
            Err(message) => {
                let from_ops = self.run.ops().get(ops_before..).and_then(|new| new.iter().rev().find_map(|o| o["error"].as_str().map(str::to_owned)));
                let code = from_ops.or_else(|| leading_code(&message)).unwrap_or_else(|| "command_refused".to_owned());
                Err(Box::new(MenuRefusal { code, availability: None, command: Some(command), detail: json!({"message": message}) }))
            }
        }
    }
}

/// A leaf that ran: the command id (a shell leaf's own id, or the catalog id), its parameters, its result and the undo
/// step it named (None: no step).
#[derive(Clone, Debug, PartialEq)]
pub struct MenuRun {
    pub id: String,
    pub command: String,
    pub params: BTreeMap<String, String>,
    pub result: Value,
    pub undo_name: Option<Value>,
}

/// Counts of an evaluated bar: nodes, available, greyed, inapplicable (with a remedy).
pub fn counts(bar: &MenuBar) -> (usize, usize, usize, usize) {
    let w = bar.walk();
    let available = w.iter().filter(|x| x.node.availability.is_available()).count();
    let inapplicable = w.iter().filter(|x| x.node.availability.state == AvailabilityState::InapplicableHere).count();
    (w.len(), available, w.len().saturating_sub(available), inapplicable)
}

/// The model-navigable projection of an evaluated bar plus the profile: [`MenuBar::agent_json`] with `profile`.
pub fn bar_json(bar: &MenuBar, state: &MenuState) -> Value {
    let mut v = bar.agent_json();
    v["profile"] = state.profile.json();
    let (nodes, available, greyed, inapplicable) = counts(bar);
    v["counts"] = json!({"nodes": nodes, "available": available, "greyed": greyed, "inapplicable": inapplicable});
    v
}

/// Catalog commands a shell leaf runs through its Viewer method (EDIT > Undo is `Viewer::undo`, which is `Run::undo`).
/// TYPE > Convert > Create Outlines is `PanelAction::CreateOutlines`, which `panel.path_operations.create_outlines` runs.
pub const SHELL_RUNS: &[(&str, &str)] =
    &[("studio.history.undo", "history.undo"), ("studio.history.redo", "history.redo"), ("studio.type.convert.create_outlines", "panel.path_operations.create_outlines")];

/// The catalog command ids menu leaves run (catalog leaves, their aliases, and [`SHELL_RUNS`]).
pub fn reached_commands() -> BTreeSet<&'static str> {
    LEAVES.iter().map(|l| l.command).chain(SHELL_RUNS.iter().map(|(_, c)| *c)).collect()
}
