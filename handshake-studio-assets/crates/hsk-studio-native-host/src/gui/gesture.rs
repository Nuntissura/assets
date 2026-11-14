//! Pointer gesture arbitration (SMB-GUI-GESTURE-GATE-1, STU-SHL-100..108): one owner at a time, claimed at PRESS for
//! the whole gesture with no threshold (STU-SHL-101); a ScrubValue drag accumulates in VALUE space from screen-space
//! deltas (`delta = dx - dy`, STU-SHL-188) at `max(q, soft_range / 200)` per logical px (STU-SHL-191), times the
//! continuously sampled modifier magnitude (STU-SHL-106), clamped to the hard bounds at every sample so dragging back
//! returns at once (STU-SHL-192); consecutive wheel notches on the field coalesce into one transaction closed by
//! [`WHEEL_IDLE_MS`] of idle, the pointer leaving, focus loss, a new action or Escape (STU-SHL-107). Every gesture
//! closes into at most ONE commit intent; Escape and an unchanged value close into none.
//!
//! A gesture keeps its START value and its net displacement, never a running value: the value is computed once from
//! both, so what it commits depends on where the gesture ended and not on how many samples carried it there (review
//! F3: a running sum of 0.05 steps rounded a half quantum differently by sample count).
//!
//! Pure state: positions and modifiers in, the observability value (STU-SHL-108), the cursor request (STU-SHL-104) and
//! commit intents out. No winit, Viewer or window type, so a later move into kumihimo-ui (decision O4) is a file move.
//! The caller (the GUI frame) runs the commit through the Viewer and counts its outcome.
use crate::event::{CursorRequest, Mods};

/// STU-SHL-107: consecutive wheel notches coalesce until this much idle time.
pub const WHEEL_IDLE_MS: u64 = 400;

/// The closed gesture-kind set of STU-SHL-108. This host produces `scrub` and `none`; the others are named for the
/// node's vocabulary (tab, panel, splitter and pan drags are not built, STU-SHL-102 rule-2 refusal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Scrub,
    TabDrag,
    PanelDrag,
    Splitter,
    Pan,
    None,
}

impl Kind {
    pub fn word(self) -> &'static str {
        match self {
            Kind::Scrub => "scrub",
            Kind::TabDrag => "tab-drag",
            Kind::PanelDrag => "panel-drag",
            Kind::Splitter => "splitter",
            Kind::Pan => "pan",
            Kind::None => "none",
        }
    }
}

/// A numeric field's bounds, precision and unit (the ParamSpec part a ScrubValue reads).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldSpec {
    pub hard: (f64, f64),
    pub soft: (f64, f64),
    pub precision: u8,
    pub unit: &'static str,
}

/// Layer opacity: harness-core panel.rs:96-101 `NumberSpec::slider(1, (0.0, 100.0), (0.0, 100.0))` with unit `%`
/// (controls descriptor.rs:111-124: precision 1, quantum 0.1). A NAMED deviation from STU-SHL-185 (the field should
/// read its ParamSpec by id): native-host has no path to the layer's control descriptor yet (lead ruling, next-lane
/// candidate).
pub const OPACITY: FieldSpec = FieldSpec { hard: (0.0, 100.0), soft: (0.0, 100.0), precision: 1, unit: "%" };

impl FieldSpec {
    fn scale(&self) -> f64 {
        10f64.powi(i32::from(self.precision))
    }
    /// STU-SHL-191: `max(q, soft_range / 200)` per logical px or wheel notch, before modifiers.
    pub fn step(&self) -> f64 {
        (1.0 / self.scale()).max((self.soft.1 - self.soft.0) / 200.0)
    }
    fn clamp(&self, v: f64) -> f64 {
        v.clamp(self.hard.0, self.hard.1)
    }
    /// The ONE clamp-and-quantize path of a drag, a wheel and SetValue (STU-SHL-107, STU-SHL-192): clamped to the hard
    /// bounds and rounded to the precision. None for a non-finite value (a typed refusal for the caller).
    pub fn settle(&self, v: f64) -> Option<f64> {
        v.is_finite().then(|| (self.clamp(v) * self.scale()).round() / self.scale())
    }
    /// The rendered number at the precision (STU-SHL-193), without the unit.
    pub fn text(&self, v: f64) -> String {
        format!("{:.*}", usize::from(self.precision), v)
    }
}

/// STU-SHL-106: Shift x10, Ctrl /10, Shift+Ctrl x100; Alt is never a magnitude.
pub fn magnitude(m: Mods) -> f64 {
    tenths(m) / 10.0
}

/// The magnitude of `m` in tenths of a step, the unit a gesture's displacement is summed in: a whole number for every
/// modifier, so whole pixels and notches sum exactly.
fn tenths(m: Mods) -> f64 {
    match (m.shift, m.ctrl) {
        (true, true) => 1000.0,
        (true, false) => 100.0,
        (false, true) => 1.0,
        (false, false) => 10.0,
    }
}

/// A gesture's value as its start plus its net displacement in tenths of a step. The displacement is clamped at every
/// sample to what the hard bounds leave, so the value holds at a bound and dragging back returns at once (STU-SHL-192);
/// fractional displacement is kept (STU-SHL-191).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Travel {
    /// The value at the gesture's start, inside the hard bounds.
    start: f64,
    /// Net displacement in tenths of a step.
    units: f64,
}

impl Travel {
    fn new(spec: &FieldSpec, before: f64) -> Self {
        Self { start: spec.clamp(if before.is_finite() { before } else { spec.hard.0 }), units: 0.0 }
    }
    /// Adds `units` tenths of a step. A sum that is not finite is ignored (the displacement stays where it was).
    fn add(&mut self, spec: &FieldSpec, units: f64) {
        let unit = spec.step() / 10.0;
        let next = self.units + units;
        let (lo, hi) = ((spec.hard.0 - self.start) / unit, (spec.hard.1 - self.start) / unit);
        if next.is_finite() && lo <= hi {
            self.units = next.clamp(lo, hi);
        }
    }
    /// The value now: computed once from the start and the net displacement, inside the hard bounds, not quantized.
    fn value(&self, spec: &FieldSpec) -> f64 {
        spec.clamp(self.start + self.units * spec.step() / 10.0)
    }
}

/// One gesture's commit intent: write `after` (settled) to `target` through the field `owner`'s command.
#[derive(Clone, Debug, PartialEq)]
pub struct Commit {
    pub owner: String,
    pub target: String,
    pub before: f64,
    pub after: f64,
}

/// The frame receipt's gesture counters (codes and counters only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Gestures begun: drags, wheel transactions, SetValue actions.
    pub gestures: u64,
    /// Commits the Viewer accepted, and those it refused.
    pub commits: u64,
    pub refusals: u64,
    /// Escapes.
    pub aborts: u64,
    /// Gestures that closed with the value unchanged (no commit, STU-SHL-104: never a no-op entry).
    pub unchanged: u64,
}

#[derive(Clone, Debug, PartialEq)]
struct Scrub {
    owner: String,
    target: String,
    spec: FieldSpec,
    /// Physical client px of the press and of the last sample (they may lie outside the window).
    press: (i32, i32),
    last: (i32, i32),
    before: f64,
    travel: Travel,
    moved: bool,
    aborted: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Wheel {
    owner: String,
    target: String,
    spec: FieldSpec,
    before: f64,
    travel: Travel,
    idle_ms: u64,
}

/// The frame's arbiter: at most one drag (which owns the pointer) or one wheel transaction (which does not).
#[derive(Debug, Default)]
pub struct Arbiter {
    scrub: Option<Scrub>,
    wheel: Option<Wheel>,
    /// The press point the cursor shows at after a drag that moved, until the next pointer move.
    restore: Option<(i32, i32)>,
    pub counters: Counters,
}

impl Arbiter {
    /// A drag owns the pointer (STU-SHL-101, -104).
    pub fn owns_pointer(&self) -> bool {
        self.scrub.is_some()
    }
    pub fn wheel_open(&self) -> bool {
        self.wheel.is_some()
    }
    /// The value of studio.gesture.active: `<author_id> <kind>` while a widget owns the pointer, else `none`.
    pub fn active(&self) -> String {
        match &self.scrub {
            Some(s) => format!("{} {}", s.owner, Kind::Scrub.word()),
            None => Kind::None.word().to_owned(),
        }
    }
    /// The kind owning the pointer.
    pub fn kind(&self) -> Kind {
        if self.scrub.is_some() { Kind::Scrub } else { Kind::None }
    }
    /// The settled in-gesture value `owner` shows for `target`, while a gesture holds one (never a history entry).
    pub fn live(&self, owner: &str, target: &str) -> Option<f64> {
        if let Some(s) = self.scrub.as_ref().filter(|s| s.owner == owner && s.target == target) {
            return s.spec.settle(s.travel.value(&s.spec));
        }
        self.wheel.as_ref().filter(|w| w.owner == owner && w.target == target).and_then(|w| w.spec.settle(w.travel.value(&w.spec)))
    }

    /// Press on a field: the field claims the pointer for the whole gesture (STU-SHL-101).
    pub fn press(&mut self, owner: &str, target: &str, spec: FieldSpec, before: f64, at: (i32, i32)) {
        self.restore = None;
        self.counters.gestures = self.counters.gestures.saturating_add(1);
        let travel = Travel::new(&spec, before);
        self.scrub = Some(Scrub { owner: owner.to_owned(), target: target.to_owned(), spec, press: at, last: at, before, travel, moved: false, aborted: false });
    }

    /// One pointer sample at physical `at`: the value moves by `(dx - dy) / scale` logical px times the step and the
    /// magnitude of `mods` (sampled now). True when the shown value changed. Bounded: one step of arithmetic in i64
    /// and f64 whatever the coordinates.
    pub fn motion(&mut self, at: (i32, i32), mods: Mods, scale: f32) -> bool {
        let Some(s) = self.scrub.as_mut() else { return false };
        let dx = i64::from(at.0) - i64::from(s.last.0);
        let dy = i64::from(at.1) - i64::from(s.last.1);
        s.last = at;
        if dx == 0 && dy == 0 {
            return false;
        }
        s.moved = true;
        if s.aborted {
            return false;
        }
        let shown = s.spec.settle(s.travel.value(&s.spec));
        // Logical px times the magnitude, in tenths of a step: whole numbers at scale 1, so the sum is exact.
        let spec = s.spec;
        s.travel.add(&spec, (dx - dy) as f64 * tenths(mods) / f64::from(scale.max(0.01)));
        s.spec.settle(s.travel.value(&s.spec)) != shown
    }

    /// Escape: a drag is aborted (its value back to the pre-press value; the pointer stays claimed until release), an
    /// open wheel transaction is discarded. True when something was aborted.
    pub fn escape(&mut self) -> bool {
        if let Some(s) = self.scrub.as_mut().filter(|s| !s.aborted) {
            s.aborted = true;
            s.travel.units = 0.0;
            self.counters.aborts = self.counters.aborts.saturating_add(1);
            return true;
        }
        if self.wheel.take().is_some() {
            self.counters.aborts = self.counters.aborts.saturating_add(1);
            return true;
        }
        false
    }

    /// Release ends the drag: its one commit intent, or none (aborted, or the value unchanged). After a drag that
    /// moved, the cursor shows at the press point.
    pub fn release(&mut self) -> Option<Commit> {
        let s = self.scrub.take()?;
        if s.moved {
            self.restore = Some(s.press);
        }
        if s.aborted {
            return None;
        }
        let value = s.travel.value(&s.spec);
        self.close(s.owner, s.target, s.spec, s.before, value)
    }

    /// Wheel `notches` over a field (positive: up, the value grows): opens or extends the transaction; True when the
    /// shown value changed. A transaction on another field or target is closed first by the caller.
    pub fn wheel(&mut self, owner: &str, target: &str, spec: FieldSpec, before: f64, notches: f64, mods: Mods) -> bool {
        if self.wheel.as_ref().is_some_and(|w| w.owner != owner || w.target != target) {
            return false;
        }
        if self.wheel.is_none() {
            self.counters.gestures = self.counters.gestures.saturating_add(1);
            let travel = Travel::new(&spec, before);
            self.wheel = Some(Wheel { owner: owner.to_owned(), target: target.to_owned(), spec, before, travel, idle_ms: 0 });
        }
        let Some(w) = self.wheel.as_mut() else { return false };
        w.idle_ms = 0;
        let shown = w.spec.settle(w.travel.value(&w.spec));
        let spec = w.spec;
        w.travel.add(&spec, notches * tenths(mods));
        w.spec.settle(w.travel.value(&w.spec)) != shown
    }
    /// Whether an open wheel transaction belongs to another field or target than these.
    pub fn wheel_elsewhere(&self, owner: &str, target: &str) -> bool {
        self.wheel.as_ref().is_some_and(|w| w.owner != owner || w.target != target)
    }

    /// Closes an open wheel transaction now (the pointer left the field, focus loss, a new action, a modal): its one
    /// commit intent, or none.
    pub fn close_wheel(&mut self) -> Option<Commit> {
        let w = self.wheel.take()?;
        let value = w.travel.value(&w.spec);
        self.close(w.owner, w.target, w.spec, w.before, value)
    }

    /// The host clock advanced `ms`: the wheel transaction closes after [`WHEEL_IDLE_MS`] of idle.
    pub fn tick(&mut self, ms: u64) -> Option<Commit> {
        let w = self.wheel.as_mut()?;
        w.idle_ms = w.idle_ms.saturating_add(ms);
        if w.idle_ms < WHEEL_IDLE_MS {
            return None;
        }
        self.close_wheel()
    }

    /// Milliseconds until a gesture timer fires (the wheel idle window), if one is pending.
    pub fn pending_ms(&self) -> Option<u64> {
        self.wheel.as_ref().map(|w| WHEEL_IDLE_MS.saturating_sub(w.idle_ms).max(1))
    }

    /// Focus loss finalizes valid work once (STU-SHL-194): a drag commits as a release would (no cursor restore), a
    /// wheel transaction closes.
    pub fn finalize(&mut self) -> Option<Commit> {
        if let Some(s) = self.scrub.take() {
            let value = s.travel.value(&s.spec);
            return if s.aborted { None } else { self.close(s.owner, s.target, s.spec, s.before, value) };
        }
        self.close_wheel()
    }

    fn close(&mut self, owner: String, target: String, spec: FieldSpec, before: f64, value: f64) -> Option<Commit> {
        let after = spec.settle(value)?;
        if spec.settle(before) == Some(after) {
            self.counters.unchanged = self.counters.unchanged.saturating_add(1);
            return None;
        }
        Some(Commit { owner, target, before, after })
    }

    /// The cursor request: hidden while a drag that moved owns the pointer, column-resize over a field (and during a
    /// press that has not moved), the press point after a drag, else the arrow. Never a pointer move during a drag.
    pub fn cursor(&self, over_field: bool) -> CursorRequest {
        match &self.scrub {
            Some(s) if s.moved => CursorRequest::Hidden,
            Some(_) => CursorRequest::ColumnResize,
            None => match self.restore {
                Some((x, y)) => CursorRequest::ShowAt { x, y },
                None if over_field => CursorRequest::ColumnResize,
                None => CursorRequest::Default,
            },
        }
    }
    /// The next pointer move after a drag ends the press-point request.
    pub fn clear_restore(&mut self) {
        self.restore = None;
    }
}
