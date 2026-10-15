//! Prototype player (STU-PRO-104, STU-PRO-131, STU-PRO-138/139; lane SMB-INTEROP-FIG-PROTOTYPE): a
//! deterministic state machine over the stored `StudioReaction` records of a Folio document. A
//! session shows one screen (a frame node: artboard or frame layer), keeps the navigation history
//! stack and the open overlays, runs transitions on an explicit millisecond clock with their stored
//! duration and cubic-bezier easing, and fires `AFTER_TIMEOUT` reactions of the current screen when
//! the clock passes their delay. Input is dispatched as typed triggers on node ids
//! (`prototype.dispatch_trigger`), never synthetic OS input; [`PlayerSession::state`] is the
//! observable frame state (`prototype.capture_frame` renders it elsewhere).
//!
//! Semantics (Studio readings; Figma parity UNVERIFIED, oracle D5):
//! - a trigger on a node with no matching reaction bubbles to its nearest ancestor that has one;
//! - a reaction's actions run in order and a navigation (`NODE`, `BACK`, `CLOSE`) ends the array
//!   (STU-PRO-104); `NAVIGATE` pushes the history, `BACK` pops it (an empty stack is a diagnostic,
//!   STU-PRO-131), `OVERLAY` opens an overlay, `CLOSE` closes the top overlay, `SWAP` replaces the top
//!   overlay (or the screen), `SCROLL_TO` records the scroll target, `CHANGE_TO` records the variant
//!   state of the instance it sits in (runtime state, never an override record, STU-PRO-133);
//! - a dispatch while a transition runs completes that transition first;
//! - `ON_HOVER` applies its actions on hover-enter and restores the previous state on hover-leave;
//!   `MOUSE_ENTER`/`MOUSE_LEAVE` fire once;
//! - `URL`, `SET_VARIABLE` (written to the session's runtime variables, STU-PRO-113) and
//!   `SET_VARIABLE_MODE` never change the screen; unsupported triggers are inert and reported.
//!
//! Bounds: history and overlay stacks at most [`MAX_STACK`]; one dispatch runs at most
//! [`MAX_ACTIONS_PER_DISPATCH`] actions; the clock saturates.
use hsk_studio_folio::{
    CubicBezier, Navigation, NodeKind, Parent, StudioAction, StudioDesignRecords, StudioDocument,
    StudioReaction, StudioTrigger, StudioValue,
};
use std::collections::BTreeMap;

/// Deepest navigation history and overlay stack.
pub const MAX_STACK: usize = 256;
/// Most actions one dispatch executes.
pub const MAX_ACTIONS_PER_DISPATCH: usize = 1024;

/// A trigger dispatched on a node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayerInput {
    Click(String),
    HoverEnter(String),
    HoverLeave(String),
    MouseDown(String),
    MouseUp(String),
    Press(String),
}

/// A running transition between two screen states.
#[derive(Clone, Debug, PartialEq)]
pub struct ActiveTransition {
    pub from: String,
    pub to: String,
    /// `DISSOLVE`, `SMART_ANIMATE`, ... (`TransitionType` debug name).
    pub kind: String,
    pub start_ms: u64,
    pub duration_ms: u32,
    pub easing: CubicBezier,
}

/// What the running prototype shows.
#[derive(Clone, Debug, PartialEq)]
pub struct PlayerState {
    pub screen: String,
    pub overlays: Vec<String>,
    pub history_depth: usize,
    pub scroll_target: Option<String>,
    /// Instance layer -> variant node it changed to (runtime state).
    pub variants: BTreeMap<String, String>,
    pub variables: BTreeMap<String, StudioValue>,
    /// (transition, raw progress 0..=1, eased progress) while one runs.
    pub transition: Option<(ActiveTransition, f64, f64)>,
    pub clock_ms: u64,
}

/// What a dispatch or a clock advance did.
#[derive(Clone, Debug, PartialEq)]
pub enum PlayerEvent {
    Navigated { from: String, to: String },
    TransitionStarted { to: String, duration_ms: u32 },
    TransitionEnded { to: String },
    OverlayOpened(String),
    OverlayClosed(String),
    ScrolledTo(String),
    VariantChanged { instance: String, to: String },
    VariableSet(String),
    TimeoutFired { node: String, after_ms: u32 },
    /// A runtime diagnostic (empty back stack, inert trigger, unsupported action, no reaction).
    Diagnostic(&'static str),
}

#[derive(Clone, Debug)]
struct Snapshot {
    screen: String,
    overlays: Vec<String>,
    history: Vec<String>,
    variants: BTreeMap<String, String>,
}

/// One prototype session over a document and its design records (copied in at start: the
/// session owns its reaction table and tree, so a host can keep it beside the document).
#[derive(Clone, Debug)]
pub struct PlayerSession {
    reactions: BTreeMap<String, Vec<StudioReaction>>,
    parents: BTreeMap<String, String>,
    instances: BTreeMap<String, ()>,
    screen: String,
    history: Vec<String>,
    overlays: Vec<String>,
    scroll_target: Option<String>,
    variants: BTreeMap<String, String>,
    variables: BTreeMap<String, StudioValue>,
    transition: Option<ActiveTransition>,
    clock_ms: u64,
    /// Clock time the current screen became visible (after-delay timers count from it).
    shown_ms: u64,
    fired: Vec<(String, usize)>,
    hover: BTreeMap<String, Snapshot>,
}

/// Eased progress of a cubic bezier with end points (0,0) and (1,1) at time fraction `t`.
pub fn cubic_bezier_ease(b: &CubicBezier, t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let bez = |p1: f64, p2: f64, u: f64| {
        let v = 1.0 - u;
        3.0 * v * v * u * p1 + 3.0 * v * u * u * p2 + u * u * u
    };
    // Bisection on x(u) = t (x is monotone for x1, x2 in 0..=1, which Folio enforces).
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..64 {
        let mid = (lo + hi) / 2.0;
        if bez(b.x1, b.x2, mid) < t {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(b.y1, b.y2, (lo + hi) / 2.0)
}

fn parent_of(p: &Parent) -> Option<&str> {
    match p {
        Parent::Node(n) => Some(n.node_id.as_str()),
        _ => None,
    }
}

impl PlayerSession {
    /// A session showing `start` (a frame node of `doc`). `None` when `start` is not a node.
    pub fn start(doc: &StudioDocument, design: &StudioDesignRecords, start: &str) -> Option<Self> {
        let mut parents = BTreeMap::new();
        let mut instances = BTreeMap::new();
        let mut known = false;
        for l in &doc.layers {
            known |= l.layer_id == start;
            if let Some(p) = parent_of(&l.parent) {
                parents.insert(l.layer_id.clone(), p.to_owned());
            }
            if l.kind == "component_instance" || l.kind == "group" {
                instances.insert(l.layer_id.clone(), ());
            }
        }
        for a in &doc.artboards {
            known |= a.artboard_id == start;
            if let Some(p) = parent_of(&a.parent) {
                parents.insert(a.artboard_id.clone(), p.to_owned());
            }
        }
        if !known {
            return None;
        }
        let reactions = design
            .nodes
            .iter()
            .filter(|n| !n.reactions.is_empty() && n.node.node_kind != NodeKind::PageSpread)
            .map(|n| (n.node.node_id.clone(), n.reactions.clone()))
            .collect();
        Some(Self {
            reactions,
            parents,
            instances,
            screen: start.to_owned(),
            history: Vec::new(),
            overlays: Vec::new(),
            scroll_target: None,
            variants: BTreeMap::new(),
            variables: BTreeMap::new(),
            transition: None,
            clock_ms: 0,
            shown_ms: 0,
            fired: Vec::new(),
            hover: BTreeMap::new(),
        })
    }

    pub fn state(&self) -> PlayerState {
        PlayerState {
            screen: self.screen.clone(),
            overlays: self.overlays.clone(),
            history_depth: self.history.len(),
            scroll_target: self.scroll_target.clone(),
            variants: self.variants.clone(),
            variables: self.variables.clone(),
            transition: self.transition.as_ref().map(|t| {
                let elapsed = self.clock_ms.saturating_sub(t.start_ms) as f64;
                let raw = if t.duration_ms == 0 { 1.0 } else { (elapsed / f64::from(t.duration_ms)).clamp(0.0, 1.0) };
                (t.clone(), raw, cubic_bezier_ease(&t.easing, raw))
            }),
            clock_ms: self.clock_ms,
        }
    }

    fn matches(t: &StudioTrigger, input: &PlayerInput) -> bool {
        matches!(
            (t, input),
            (StudioTrigger::OnClick {}, PlayerInput::Click(_))
                | (StudioTrigger::OnHover {}, PlayerInput::HoverEnter(_))
                | (StudioTrigger::MouseEnter {}, PlayerInput::HoverEnter(_))
                | (StudioTrigger::MouseLeave {}, PlayerInput::HoverLeave(_))
                | (StudioTrigger::MouseDown {}, PlayerInput::MouseDown(_))
                | (StudioTrigger::MouseUp {}, PlayerInput::MouseUp(_))
                | (StudioTrigger::OnPress {}, PlayerInput::Press(_))
        )
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            screen: self.screen.clone(),
            overlays: self.overlays.clone(),
            history: self.history.clone(),
            variants: self.variants.clone(),
        }
    }

    /// Completes a running transition (its destination becomes the screen state).
    fn settle(&mut self, events: &mut Vec<PlayerEvent>) {
        if let Some(t) = self.transition.take() {
            self.shown_ms = t.start_ms.saturating_add(u64::from(t.duration_ms));
            events.push(PlayerEvent::TransitionEnded { to: t.to });
        }
    }

    /// Dispatches one trigger (`prototype.dispatch_trigger`).
    pub fn dispatch(&mut self, input: &PlayerInput) -> Vec<PlayerEvent> {
        let mut events = Vec::new();
        self.settle(&mut events);
        let node = match input {
            PlayerInput::Click(n)
            | PlayerInput::HoverEnter(n)
            | PlayerInput::HoverLeave(n)
            | PlayerInput::MouseDown(n)
            | PlayerInput::MouseUp(n)
            | PlayerInput::Press(n) => n.clone(),
        };
        // ON_HOVER: leaving restores the state saved on enter.
        if let PlayerInput::HoverLeave(n) = input
            && let Some(s) = self.hover.remove(n)
        {
            self.screen = s.screen;
            self.overlays = s.overlays;
            self.history = s.history;
            self.variants = s.variants;
            events.push(PlayerEvent::Navigated { from: node.clone(), to: self.screen.clone() });
        }
        // Bubble: the node, then its ancestors, until one holds a matching reaction.
        let mut cur = Some(node.as_str().to_owned());
        let mut depth = 0usize;
        while let Some(id) = cur {
            depth = depth.saturating_add(1);
            if depth > 256 {
                break;
            }
            if let Some(r) = self
                .reactions
                .get(id.as_str())
                .and_then(|list| list.iter().find(|r| r.trigger.as_ref().is_some_and(|t| Self::matches(t, input))))
                .cloned()
            {
                if let (Some(StudioTrigger::OnHover {}), PlayerInput::HoverEnter(n)) = (&r.trigger, input) {
                    let snap = self.snapshot();
                    self.hover.insert(n.clone(), snap);
                }
                let actions = r.actions.clone();
                self.run(&id, &actions, &mut events);
                return events;
            }
            cur = self.parents.get(id.as_str()).cloned();
        }
        events.push(PlayerEvent::Diagnostic("no_reaction_for_trigger"));
        events
    }

    fn container_instance(&self, node: &str) -> Option<String> {
        let mut cur = self.parents.get(node);
        while let Some(id) = cur {
            if self.instances.contains_key(id) {
                return Some(id.clone());
            }
            cur = self.parents.get(id);
        }
        None
    }

    fn run(&mut self, on: &str, actions: &[StudioAction], events: &mut Vec<PlayerEvent>) {
        for a in actions.iter().take(MAX_ACTIONS_PER_DISPATCH) {
            match a {
                StudioAction::Back {} => {
                    match self.history.pop() {
                        Some(prev) => {
                            events.push(PlayerEvent::Navigated { from: self.screen.clone(), to: prev.clone() });
                            self.screen = prev;
                            self.shown_ms = self.clock_ms;
                            self.fired.clear();
                        }
                        None => events.push(PlayerEvent::Diagnostic("back_on_empty_history")),
                    }
                    return;
                }
                StudioAction::Close {} => {
                    match self.overlays.pop() {
                        Some(o) => events.push(PlayerEvent::OverlayClosed(o)),
                        None => events.push(PlayerEvent::Diagnostic("close_without_overlay")),
                    }
                    return;
                }
                StudioAction::Url { .. } => events.push(PlayerEvent::Diagnostic("url_not_opened_in_player")),
                StudioAction::SetVariable { variable_id, variable_value } => {
                    self.variables.insert(variable_id.clone(), variable_value.clone());
                    events.push(PlayerEvent::VariableSet(variable_id.clone()));
                }
                StudioAction::SetVariableMode { .. } => events.push(PlayerEvent::Diagnostic("variable_mode_not_modelled")),
                StudioAction::Node { destination_id, navigation, transition, .. } => {
                    let Some(dest) = destination_id.clone() else {
                        events.push(PlayerEvent::Diagnostic("node_action_without_destination"));
                        return;
                    };
                    match navigation {
                        Navigation::Navigate => {
                            if self.history.len() >= MAX_STACK {
                                events.push(PlayerEvent::Diagnostic("history_over_limit"));
                                return;
                            }
                            let from = std::mem::replace(&mut self.screen, dest.clone());
                            self.history.push(from.clone());
                            self.fired.clear();
                            events.push(PlayerEvent::Navigated { from: from.clone(), to: dest.clone() });
                            match transition {
                                Some(t) => {
                                    let easing = t
                                        .easing
                                        .easing_function_cubic_bezier
                                        .clone()
                                        .unwrap_or(CubicBezier { x1: 0.0, y1: 0.0, x2: 1.0, y2: 1.0 });
                                    if t.easing.easing_function_spring.is_some() {
                                        events.push(PlayerEvent::Diagnostic("spring_easing_played_linear"));
                                    }
                                    self.transition = Some(ActiveTransition {
                                        from,
                                        to: dest.clone(),
                                        kind: format!("{:?}", t.transition_type),
                                        start_ms: self.clock_ms,
                                        duration_ms: t.duration,
                                        easing,
                                    });
                                    events.push(PlayerEvent::TransitionStarted { to: dest, duration_ms: t.duration });
                                }
                                None => self.shown_ms = self.clock_ms,
                            }
                        }
                        Navigation::Overlay => {
                            if self.overlays.len() >= MAX_STACK {
                                events.push(PlayerEvent::Diagnostic("overlay_over_limit"));
                                return;
                            }
                            self.overlays.push(dest.clone());
                            events.push(PlayerEvent::OverlayOpened(dest));
                        }
                        Navigation::Swap => match self.overlays.last_mut() {
                            Some(top) => {
                                *top = dest.clone();
                                events.push(PlayerEvent::OverlayOpened(dest));
                            }
                            None => {
                                let from = std::mem::replace(&mut self.screen, dest.clone());
                                events.push(PlayerEvent::Navigated { from, to: dest });
                            }
                        },
                        Navigation::ScrollTo => {
                            self.scroll_target = Some(dest.clone());
                            events.push(PlayerEvent::ScrolledTo(dest));
                        }
                        Navigation::ChangeTo => {
                            let instance = self.container_instance(on).unwrap_or_else(|| on.to_owned());
                            self.variants.insert(instance.clone(), dest.clone());
                            events.push(PlayerEvent::VariantChanged { instance, to: dest });
                        }
                    }
                    return;
                }
            }
        }
    }

    /// Advances the clock by `ms`: finishes transitions and fires the `AFTER_TIMEOUT` reactions of
    /// the current screen whose delay has passed (each once per showing of the screen).
    pub fn advance(&mut self, ms: u64) -> Vec<PlayerEvent> {
        let mut events = Vec::new();
        self.clock_ms = self.clock_ms.saturating_add(ms);
        if let Some(t) = &self.transition
            && self.clock_ms >= t.start_ms.saturating_add(u64::from(t.duration_ms))
        {
            self.settle(&mut events);
        }
        if self.transition.is_some() {
            return events;
        }
        let screen = self.screen.clone();
        if let Some(list) = self.reactions.get(screen.as_str()).cloned() {
            for (i, r) in list.iter().enumerate() {
                if let Some(StudioTrigger::AfterTimeout { timeout }) = &r.trigger {
                    let due = self.shown_ms.saturating_add(u64::from(*timeout));
                    if self.clock_ms >= due && !self.fired.contains(&(screen.clone(), i)) {
                        self.fired.push((screen.clone(), i));
                        events.push(PlayerEvent::TimeoutFired { node: screen.clone(), after_ms: *timeout });
                        let actions = r.actions.clone();
                        self.run(&screen, &actions, &mut events);
                        break;
                    }
                }
            }
        }
        events
    }
}
