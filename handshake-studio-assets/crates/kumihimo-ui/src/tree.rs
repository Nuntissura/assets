//! Retained node arena, one tree per panel, rebuilt by a pure view function only when the panel's inputs change
//! (shell-architect position "Retained or immediate mode"). Nodes are keyed by `author_id`; hover, press and focus live
//! in [`Interaction`], keyed by the same id, so a rebuild never loses them.
use crate::display::{DisplayList, IconId, ImageId};
use crate::geom::{Point, Rect, Scale};
use crate::layout::{LayoutPort, StackLayout};
use crate::text::Text;
use crate::tokens::{ColorToken, StyleKind, TextToken, Theme};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Row,
    Column,
}
/// Size along one axis, logical px.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Size {
    Fixed(f32),
    /// Share of the space left on the parent's main axis; full extent on its cross axis.
    Fill,
    /// The content's measured size.
    Hug,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
}
/// Stack layout input of one node (no Taffy: decision `layout_and_text`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LayoutSpec {
    pub axis: Axis,
    pub w: Size,
    pub h: Size,
    pub gap: f32,
    /// left, top, right, bottom.
    pub pad: [f32; 4],
    /// Placement of children across the axis.
    pub cross: Align,
    /// Placement of the children group along the axis when nothing fills.
    pub main: Align,
    /// Smallest width a Hug node takes (button.min_w).
    pub min_w: f32,
}
impl Default for LayoutSpec {
    fn default() -> Self {
        Self { axis: Axis::Row, w: Size::Hug, h: Size::Hug, gap: 0.0, pad: [0.0; 4], cross: Align::Center, main: Align::Start, min_w: 0.0 }
    }
}

/// AccessKit role names (components.json `role`); the host's AccessKit adapter maps them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Generic,
    Group,
    Toolbar,
    Button,
    TabList,
    Tab,
    Slider,
    SpinButton,
    Tree,
    TreeItem,
    Label,
    Image,
    CheckBox,
}
impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Generic => "GenericContainer",
            Role::Group => "Group",
            Role::Toolbar => "Toolbar",
            Role::Button => "Button",
            Role::TabList => "TabList",
            Role::Tab => "Tab",
            Role::Slider => "Slider",
            Role::SpinButton => "SpinButton",
            Role::Tree => "Tree",
            Role::TreeItem => "TreeItem",
            Role::Label => "Label",
            Role::Image => "Image",
            Role::CheckBox => "CheckBox",
        }
    }
}

/// What a node draws. Colours are tokens; `None` text colours inherit the enclosing widget's state colour.
#[derive(Clone, Debug, PartialEq)]
pub enum Paint {
    None,
    Fill(ColorToken),
    /// A widget surface from the state table of `kind` (fill, edges, focus ring).
    Surface { kind: StyleKind, radius: f32 },
    /// `weight` 0 = the token's weight; otherwise the weight layout, checks and emit all use (F2).
    Label { text: String, token: TextToken, weight: u16, color: Option<ColorToken>, align: Align, ellipsis: bool },
    /// A numeric field value, right aligned, tabular figures, unit in `text.tertiary`.
    Value { text: String, unit: Option<String> },
    Icon { icon: IconId, size: f32 },
    /// Slider rail: value fraction and soft range fractions.
    Rail { frac: f32, soft: (f32, f32) },
    /// A 1 x `h` seam or divider in `edge.subtle`.
    Divider,
    TagEdge(ColorToken),
    Thumbnail(Option<ImageId>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Flags {
    pub focusable: bool,
    pub disabled: bool,
    pub selected: bool,
    pub toggled: bool,
    pub hovered: bool,
    pub pressed: bool,
    pub focused: bool,
    /// Children are clipped to this node's bounds.
    pub clip: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Click,
    Focus,
    Increment,
    Decrement,
    SetValue,
    Expand,
    Collapse,
}
impl Action {
    pub fn name(self) -> &'static str {
        match self {
            Action::Click => "click",
            Action::Focus => "focus",
            Action::Increment => "increment",
            Action::Decrement => "decrement",
            Action::SetValue => "set_value",
            Action::Expand => "expand",
            Action::Collapse => "collapse",
        }
    }
}

/// Numeric value of a field or slider (STU-MDL-105 numeric fields).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Numeric {
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// Stable id (STU-MDL-100 grammar, chosen by the caller). Empty = decorative, not in the semantic tree.
    pub author_id: String,
    pub role: Role,
    pub layout: LayoutSpec,
    pub paint: Paint,
    pub label: Option<String>,
    pub value: Option<String>,
    pub numeric: Option<Numeric>,
    pub flags: Flags,
    pub actions: Vec<Action>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    /// Computed by layout, logical px.
    pub bounds: Rect,
}
impl Node {
    pub fn new(author_id: impl Into<String>, role: Role) -> Self {
        Self {
            author_id: author_id.into(),
            role,
            layout: LayoutSpec::default(),
            paint: Paint::None,
            label: None,
            value: None,
            numeric: None,
            flags: Flags::default(),
            actions: Vec::new(),
            parent: None,
            children: Vec::new(),
            bounds: Rect::default(),
        }
    }
    /// A decorative node (no id, generic role).
    pub fn deco() -> Self {
        Self::new("", Role::Generic)
    }
    pub fn layout(mut self, l: LayoutSpec) -> Self {
        self.layout = l;
        self
    }
    pub fn size(mut self, w: Size, h: Size) -> Self {
        self.layout.w = w;
        self.layout.h = h;
        self
    }
    pub fn paint(mut self, p: Paint) -> Self {
        self.paint = p;
        self
    }
    pub fn label(mut self, l: impl Into<String>) -> Self {
        self.label = Some(l.into());
        self
    }
    pub fn value(mut self, v: impl Into<String>) -> Self {
        self.value = Some(v.into());
        self
    }
    pub fn actions(mut self, a: &[Action]) -> Self {
        self.actions = a.to_vec();
        self.flags.focusable = !a.is_empty();
        self
    }
    pub fn flags(mut self, f: impl FnOnce(&mut Flags)) -> Self {
        f(&mut self.flags);
        self
    }
    pub fn interactive(&self) -> bool {
        !self.actions.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BuildError {
    /// Two nodes carry the same author_id (ids must be stable and unique, STU-MDL-100).
    DuplicateId(String),
    /// An author_id outside the STU-MDL-100/103 grammar.
    InvalidId(String),
    /// A distinct author_id whose stable_id (STU-MDL-103 item 2) equals an earlier node's.
    DuplicateStableId(String),
    /// `end` without a matching `begin`, or `finish` with open nodes.
    Unbalanced,
    /// Over [`MAX_NODES`].
    TooManyNodes,
}

/// Most nodes one panel tree holds.
pub const MAX_NODES: usize = 65_536;

/// One panel's node arena. `NodeId(0)` is the root.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tree {
    pub nodes: Vec<Node>,
}
impl Tree {
    pub fn root(&self) -> NodeId {
        NodeId(0)
    }
    pub fn get(&self, id: NodeId) -> &Node {
        &self.nodes[id.0 as usize]
    }
    pub fn get_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.0 as usize]
    }
    pub fn len(&self) -> usize {
        self.nodes.len()
    }
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
    pub fn find(&self, author_id: &str) -> Option<NodeId> {
        if author_id.is_empty() {
            return None;
        }
        self.nodes.iter().position(|n| n.author_id == author_id).map(|i| NodeId(i as u32))
    }
    /// Node ids in tree (reading) order, depth first.
    pub fn preorder(&self) -> Vec<NodeId> {
        let mut out = Vec::with_capacity(self.nodes.len());
        if self.nodes.is_empty() {
            return out;
        }
        let mut stack = vec![NodeId(0)];
        while let Some(id) = stack.pop() {
            out.push(id);
            stack.extend(self.get(id).children.iter().rev().copied());
        }
        out
    }
    pub fn is_ancestor(&self, a: NodeId, mut b: NodeId) -> bool {
        while let Some(p) = self.get(b).parent {
            if p == a {
                return true;
            }
            b = p;
        }
        false
    }
    /// True when this node or an ancestor is disabled.
    pub fn effectively_disabled(&self, mut id: NodeId) -> bool {
        loop {
            let n = self.get(id);
            if n.flags.disabled {
                return true;
            }
            match n.parent {
                Some(p) => id = p,
                None => return false,
            }
        }
    }
}

/// Builds one tree. Authoring reads like immediate mode: `begin`/`end` containers, `leaf` for leaves.
pub struct Builder<'a> {
    tree: Tree,
    stack: Vec<NodeId>,
    ui: &'a Interaction,
    error: Option<BuildError>,
    ids: std::collections::HashSet<String>,
    stable_ids: std::collections::HashSet<String>,
}
impl<'a> Builder<'a> {
    pub fn new(root: Node, ui: &'a Interaction) -> Self {
        let mut b = Self { tree: Tree::default(), stack: Vec::new(), ui, error: None, ids: Default::default(), stable_ids: Default::default() };
        let id = b.add(root);
        b.stack.push(id);
        b
    }
    fn add(&mut self, mut node: Node) -> NodeId {
        if self.tree.nodes.len() >= MAX_NODES {
            self.error.get_or_insert(BuildError::TooManyNodes);
            return NodeId(0);
        }
        if !node.author_id.is_empty() {
            if !crate::semantic::valid_author_id(&node.author_id) {
                self.error.get_or_insert(BuildError::InvalidId(node.author_id.clone()));
            }
            if !self.ids.insert(node.author_id.clone()) {
                self.error.get_or_insert(BuildError::DuplicateId(node.author_id.clone()));
            } else if !self.stable_ids.insert(crate::semantic::stable_id(&node.author_id)) {
                self.error.get_or_insert(BuildError::DuplicateStableId(node.author_id.clone()));
            }
            node.flags.hovered = self.ui.hovered.as_deref() == Some(node.author_id.as_str());
            node.flags.pressed = self.ui.pressed.as_deref() == Some(node.author_id.as_str());
            node.flags.focused = self.ui.focused.as_deref() == Some(node.author_id.as_str());
        }
        let id = NodeId(self.tree.nodes.len() as u32);
        node.parent = self.stack.last().copied();
        node.children.clear();
        if let Some(p) = node.parent {
            self.tree.get_mut(p).children.push(id);
        }
        self.tree.nodes.push(node);
        id
    }
    /// Opens a container; children go into it until [`Builder::end`].
    pub fn begin(&mut self, node: Node) -> NodeId {
        let id = self.add(node);
        self.stack.push(id);
        id
    }
    pub fn end(&mut self) {
        if self.stack.len() <= 1 {
            self.error.get_or_insert(BuildError::Unbalanced);
        } else {
            self.stack.pop();
        }
    }
    pub fn leaf(&mut self, node: Node) -> NodeId {
        self.add(node)
    }
    pub fn interaction(&self) -> &Interaction {
        self.ui
    }
    pub fn finish(self) -> Result<Tree, BuildError> {
        if let Some(e) = self.error {
            return Err(e);
        }
        if self.stack.len() != 1 {
            return Err(BuildError::Unbalanced);
        }
        Ok(self.tree)
    }
}

/// Pointer, press and focus state keyed by author_id; `rev` changes whenever any of it changes. Ownership (lead ruling
/// F7): one Interaction per window. The window routes pointer events to the panel under the pointer (`pointer_move`
/// with that panel's tree), and each [`Panel`] keys on its own slice of this state, so a hover rebuilds only the panels
/// it enters or leaves. Cross-panel focus order (the F6 key region cycle of components.json) is not here yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Interaction {
    pub hovered: Option<String>,
    pub pressed: Option<String>,
    pub focused: Option<String>,
    /// Focus ring shows on keyboard focus only (components.json universal rules).
    pub focus_visible: bool,
    rev: u64,
}
impl Interaction {
    pub fn rev(&self) -> u64 {
        self.rev
    }
    fn set(slot: &mut Option<String>, v: Option<String>, rev: &mut u64) -> bool {
        if *slot != v {
            *slot = v;
            *rev = rev.wrapping_add(1);
            true
        } else {
            false
        }
    }
    /// Hover follows the pointer, except while a widget owns the pointer (STU-SHL-104). True when state changed.
    pub fn pointer_move(&mut self, tree: &Tree, p: Point) -> bool {
        if self.pressed.is_some() {
            return false;
        }
        let hit = crate::hit::hit_test(tree, p).map(|id| tree.get(id).author_id.clone());
        Self::set(&mut self.hovered, hit, &mut self.rev)
    }
    /// Press claims at press (STU-SHL-101); focus moves to the pressed control without a visible ring.
    pub fn pointer_down(&mut self, tree: &Tree, p: Point) -> bool {
        let hit = crate::hit::hit_test(tree, p).filter(|id| !tree.effectively_disabled(*id));
        let id = hit.map(|id| tree.get(id).author_id.clone());
        let mut changed = Self::set(&mut self.pressed, id.clone(), &mut self.rev);
        if id.is_some() {
            changed |= Self::set(&mut self.focused, id, &mut self.rev);
            if self.focus_visible {
                self.focus_visible = false;
                self.rev = self.rev.wrapping_add(1);
                changed = true;
            }
        }
        changed
    }
    /// Release: activates the pressed control when released inside it (button rule), returning its author_id.
    pub fn pointer_up(&mut self, tree: &Tree, p: Point) -> Option<String> {
        let pressed = self.pressed.take()?;
        self.rev = self.rev.wrapping_add(1);
        let over = crate::hit::hit_test(tree, p).map(|id| tree.get(id).author_id.clone());
        (over.as_deref() == Some(pressed.as_str())).then_some(pressed)
    }
    /// Tab / Shift+Tab through the tree's focus order; shows the ring. Returns the new focus.
    pub fn focus_step(&mut self, tree: &Tree, back: bool) -> Option<String> {
        let next = crate::hit::focus_step(tree, self.focused.as_deref(), back).map(|id| tree.get(id).author_id.clone());
        Self::set(&mut self.focused, next.clone(), &mut self.rev);
        if !self.focus_visible {
            self.focus_visible = true;
            self.rev = self.rev.wrapping_add(1);
        }
        next
    }
}

/// Everything a panel's look depends on besides its view state.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FrameKey {
    state_rev: u64,
    /// This panel's slice of the interaction state (F7): hover, press and focus only when the id is in this tree.
    ui_slice: u64,
    /// [`Text::epoch`]: a new text system (fonts) re-shapes (F6).
    text_epoch: u64,
    viewport: [u32; 4],
    scale: u32,
    theme: Theme,
}

/// A retained panel: tree, display list and the key they were built for. Idle frames rebuild nothing.
#[derive(Default)]
pub struct Panel {
    key: Option<FrameKey>,
    tree: Tree,
    list: DisplayList,
    builds: u64,
}
impl Panel {
    pub fn tree(&self) -> &Tree {
        &self.tree
    }
    pub fn display_list(&self) -> &DisplayList {
        &self.list
    }
    /// How many times this panel has been rebuilt.
    pub fn builds(&self) -> u64 {
        self.builds
    }
    /// Forces the next [`Panel::update`] to rebuild (a font, icon or resource change the key cannot see).
    pub fn invalidate(&mut self) {
        self.key = None;
    }
    /// The part of `ui` this panel draws: one window-wide Interaction can serve every panel, and a hover in one panel
    /// changes only the slices of the panels it enters or leaves (F7).
    fn ui_slice(tree: &Tree, ui: &Interaction) -> u64 {
        let mut h: u64 = 0xCBF2_9CE4_8422_2325;
        let mut eat = |tag: u8, id: Option<&str>| {
            let id = id.filter(|s| tree.find(s).is_some()).unwrap_or("");
            for b in std::iter::once(tag).chain(id.bytes()) {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0000_0100_0000_01B3);
            }
        };
        eat(1, ui.hovered.as_deref());
        eat(2, ui.pressed.as_deref());
        eat(3, ui.focused.as_deref());
        let ring = ui.focus_visible && ui.focused.as_deref().is_some_and(|s| tree.find(s).is_some());
        eat(if ring { 4 } else { 5 }, None);
        h
    }
    /// Rebuilds tree, layout and display list when any input changed; returns whether it did. `view` is called only on
    /// a rebuild.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        state_rev: u64,
        ui: &Interaction,
        viewport: Rect,
        scale: Scale,
        theme: Theme,
        text: &mut Text,
        view: impl FnOnce(&mut Builder<'_>),
    ) -> Result<bool, BuildError> {
        let mut key = FrameKey {
            state_rev,
            ui_slice: Self::ui_slice(&self.tree, ui),
            text_epoch: text.epoch(),
            viewport: [viewport.x0.to_bits(), viewport.y0.to_bits(), viewport.x1.to_bits(), viewport.y1.to_bits()],
            scale: scale.get().to_bits(),
            theme,
        };
        if self.key == Some(key) {
            return Ok(false);
        }
        let mut b = Builder::new(
            Node::new("", Role::Group).layout(LayoutSpec { axis: Axis::Column, w: Size::Fill, h: Size::Fill, cross: Align::Start, ..LayoutSpec::default() }),
            ui,
        );
        view(&mut b);
        let mut tree = b.finish()?;
        StackLayout.layout(&mut tree, NodeId(0), viewport, text);
        self.list = crate::emit::emit(&tree, ui, theme, text);
        // Keyed on the slice of the tree just built, so the next idle frame compares like with like.
        key.ui_slice = Self::ui_slice(&tree, ui);
        self.tree = tree;
        self.key = Some(key);
        self.builds = self.builds.saturating_add(1);
        Ok(true)
    }
}
