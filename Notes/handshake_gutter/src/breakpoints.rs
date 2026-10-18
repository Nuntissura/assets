//! Breakpoint model + the debug-adapter publish event for the native code editor (WP-KERNEL-012
//! MT-007 — E1 code editor gutter).
//!
//! ## What this owns
//!
//! - [`BreakpointSet`] — the set of buffer lines that carry a breakpoint, behind a `HashSet<usize>`
//!   so a toggle is O(1) and the set never holds a duplicate line. The gutter renders a filled red
//!   circle on every line in this set and a click toggles it.
//! - [`BreakpointEvent`] — the typed message the `CodeEditorPanel`
//!   publishes when a breakpoint is set or cleared, so a FUTURE debug-adapter-protocol (DAP) client
//!   MT can consume it and forward `setBreakpoints` to the debuggee. This MT does NOT implement the
//!   DAP client; it only prepares the publish channel.
//!
//! ## Why a non-blocking, discard-on-disconnect publish (RISK-003 / MC-003)
//!
//! The DAP client is a future MT, so the receiver may not exist yet (or may have been dropped). The
//! MT red-team RISK-003 names `try_send`, but `std::sync::mpsc::Sender` has NO `try_send` — `try_send`
//! belongs to `SyncSender` on a BOUNDED channel. The KERNEL_BUILDER gate note in the MT contract
//! resolves this: the real intent is "non-blocking + discard on a dropped receiver", which an
//! UNBOUNDED `std::sync::mpsc::channel()` + `Sender::send(event).ok()` satisfies exactly — an unbounded
//! `send` never blocks, and `.ok()` discards the `Err` returned when the receiver was dropped. The
//! panel guards every publish with `if let Some(sender) = &self.breakpoint_sender { sender.send(event).ok(); }`
//! so a missing/disconnected DAP client is a benign no-op, never a hang. This mirrors the existing
//! `event_bus` "bus before producer" shape already proven in the WP-011 shell.

use std::collections::HashSet;

/// Whether a [`BreakpointEvent`] sets a new breakpoint or clears an existing one. The DAP client maps
/// this to adding/removing a `SourceBreakpoint` in the next `setBreakpoints` request for the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakpointAction {
    /// A breakpoint was added on the line (the line was NOT in the set before the toggle).
    Set,
    /// A breakpoint was removed from the line (the line WAS in the set before the toggle).
    Clear,
}

/// A typed breakpoint change published onto the debug-adapter channel when a gutter breakpoint is
/// toggled. Carries the file + 0-based buffer line + the action so a future DAP client can forward it
/// without re-reading editor state. `file_path` is the document's path (empty for an unsaved/in-memory
/// buffer — the DAP client treats an empty path as "no source mapping yet").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakpointEvent {
    /// The path of the file the breakpoint belongs to (empty when the buffer has no backing file yet).
    pub file_path: String,
    /// The 0-based buffer line the breakpoint sits on.
    pub line: usize,
    /// Whether this toggle set or cleared the breakpoint.
    pub action: BreakpointAction,
}

/// The set of buffer lines that carry a breakpoint. Backed by a `HashSet<usize>` so a toggle/contains
/// is O(1) and the same line is never stored twice. The gutter reads it to draw the breakpoint circles
/// and the gutter click handler toggles it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BreakpointSet {
    /// The 0-based buffer lines that have a breakpoint.
    lines: HashSet<usize>,
}

impl BreakpointSet {
    /// An empty breakpoint set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Toggle the breakpoint on `line`: add it if absent, remove it if present. Returns the resulting
    /// [`BreakpointAction`] ([`BreakpointAction::Set`] when the line gained a breakpoint,
    /// [`BreakpointAction::Clear`] when it lost one) so the caller can publish the matching
    /// [`BreakpointEvent`] without a second `contains` query. Idempotent in pairs: two toggles of the
    /// same line return to the original state (AC-002).
    pub fn toggle(&mut self, line: usize) -> BreakpointAction {
        if self.lines.remove(&line) {
            BreakpointAction::Clear
        } else {
            self.lines.insert(line);
            BreakpointAction::Set
        }
    }

    /// True when `line` carries a breakpoint.
    pub fn contains(&self, line: usize) -> bool {
        self.lines.contains(&line)
    }

    /// The lines that carry a breakpoint, ascending (sorted so the gutter draw order + tests are
    /// deterministic — a `HashSet` iteration order is otherwise unspecified). Borrows nothing of the
    /// caller; returns an owned sorted iterator so the gutter can collect markers without holding the
    /// set lock across egui calls.
    pub fn iter(&self) -> impl Iterator<Item = usize> {
        let mut v: Vec<usize> = self.lines.iter().copied().collect();
        v.sort_unstable();
        v.into_iter()
    }

    /// The number of breakpoints set.
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// True when no breakpoints are set.
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_adds_then_removes_returning_the_action() {
        let mut set = BreakpointSet::new();
        assert!(!set.contains(5), "no breakpoint on line 5 initially");
        // First toggle SETS the breakpoint.
        assert_eq!(set.toggle(5), BreakpointAction::Set);
        assert!(
            set.contains(5),
            "line 5 has a breakpoint after the first toggle"
        );
        assert_eq!(set.len(), 1);
        // Second toggle CLEARS it (idempotent in pairs — AC-002).
        assert_eq!(set.toggle(5), BreakpointAction::Clear);
        assert!(
            !set.contains(5),
            "line 5 breakpoint removed after the second toggle"
        );
        assert!(set.is_empty());
    }

    #[test]
    fn contains_is_per_line_and_independent() {
        let mut set = BreakpointSet::new();
        set.toggle(2);
        set.toggle(9);
        assert!(set.contains(2));
        assert!(set.contains(9));
        assert!(!set.contains(3), "an unrelated line has no breakpoint");
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn iter_returns_lines_ascending_without_duplicates() {
        let mut set = BreakpointSet::new();
        set.toggle(7);
        set.toggle(1);
        set.toggle(4);
        // Toggling the same line twice nets to no change (removed), so 4 must NOT appear.
        set.toggle(4);
        let lines: Vec<usize> = set.iter().collect();
        assert_eq!(
            lines,
            vec![1, 7],
            "iter yields the live breakpoints ascending, no dupes"
        );
    }

    #[test]
    fn breakpoint_event_carries_file_line_action() {
        // The shape a future DAP client consumes; built by the panel on a toggle.
        let ev = BreakpointEvent {
            file_path: "src/main.rs".to_owned(),
            line: 12,
            action: BreakpointAction::Set,
        };
        assert_eq!(ev.line, 12);
        assert_eq!(ev.action, BreakpointAction::Set);
        assert_eq!(ev.file_path, "src/main.rs");
    }
}
