//! Editor jump history — the back/forward navigation stack (WP-KERNEL-012 MT-052 — E1 GO-menu).
//!
//! This module is the pure, egui-free core behind **Navigate Back (Alt+Left)** and **Navigate Forward
//! (Alt+Right)**. It records cursor positions across files whenever the user performs a NAVIGATION JUMP
//! (go-to-definition, find-references result open, symbol/outline jump, goto-line) and lets the user walk
//! that stack backward/forward, restoring the prior file + position — VS Code's editor-navigation stack.
//!
//! ## VS Code semantics (the four behaviors that matter)
//!
//! 1. **Cross-file** ([`JumpEntry`] carries its own `file_path`): Navigate Back can restore a position in
//!    a DIFFERENT file than the one currently focused (AC-004 / RISK-005). The stack is not per-file.
//! 2. **Forward-tail truncation** (RISK-004 / MC-004): making a NEW jump while the history cursor is not
//!    at the tail (i.e. after some Backs) DISCARDS the forward entries before pushing — so Navigate
//!    Forward never jumps to a stale, unrelated location.
//! 3. **Coalescing**: consecutive [`record`](JumpHistory::record)s pointing at the same file within a
//!    small line tolerance ([`COALESCE_LINE_TOLERANCE`]) collapse, so trivial cursor moves do not flood
//!    the stack. Only navigation jumps call `record` (the panel wires `record` at the four jump dispatch
//!    sites only — NOT typing/arrow moves, RISK-006 / MC-006).
//! 4. **Cap** ([`MAX_ENTRIES`]): the stack is capped, dropping the OLDEST entry when full.
//!
//! ## Cursor model
//!
//! [`JumpHistory`] keeps a `Vec<JumpEntry>` and a `cursor` index that is the position the user is
//! "currently at" within the history. [`back`](JumpHistory::back) moves the cursor toward the past and
//! returns the prior location (recording the CURRENT location so Forward can return to it);
//! [`forward`](JumpHistory::forward) moves toward the future. This is in-memory SESSION state only — no
//! SurrealDB/EventLedger persistence (the MT is pure frontend; jump positions come from in-process
//! editor cursor state).

use std::path::PathBuf;

use super::navigation::BufferPosition;

/// The maximum number of jump entries retained. When a push would exceed this, the OLDEST entry is
/// dropped (and the cursor index re-based so it still points at the same logical entry). VS Code uses a
/// similar bounded stack; 50 is a sensible cap for an editing session.
pub const MAX_ENTRIES: usize = 50;

/// Two consecutive jumps to the SAME file whose lines are within this many lines of each other coalesce
/// into one entry (the newer position replaces the older), so trivial near-position jumps do not flood
/// the stack. A small >0 tolerance folds adjacent-line jumps too (VS Code's "same region" coalescing) —
/// the contract's "within a small line tolerance".
pub const COALESCE_LINE_TOLERANCE: usize = 1;

/// One recorded jump location: a file path + a [`BufferPosition`] within that file. Each entry carries
/// its OWN `file_path` so Navigate Back can restore a position in a file other than the one currently
/// focused (the cross-file requirement, AC-004).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JumpEntry {
    /// The file the recorded position lives in. May differ from the currently-focused file.
    pub file_path: PathBuf,
    /// The `(line, column)` position within `file_path`.
    pub position: BufferPosition,
}

impl JumpEntry {
    /// A jump entry at `position` within `file_path`.
    pub fn new(file_path: impl Into<PathBuf>, position: BufferPosition) -> Self {
        Self {
            file_path: file_path.into(),
            position,
        }
    }

    /// True when `other` points at the same file within [`COALESCE_LINE_TOLERANCE`] lines — the coalesce
    /// predicate.
    fn coalesces_with(&self, other: &JumpEntry) -> bool {
        self.file_path == other.file_path
            && self.position.line.abs_diff(other.position.line) <= COALESCE_LINE_TOLERANCE
    }
}

/// The back/forward jump stack. `entries` is the recorded history (oldest first); `cursor` is the index
/// the user is currently positioned at within it.
///
/// Invariants:
/// - `cursor <= entries.len()`. `cursor == entries.len()` means "at the tail / present" — there is
///   nothing forward of here. `cursor == 0` with a non-empty stack means the user has walked all the way
///   back.
/// - A NEW [`record`](Self::record) while `cursor < entries.len()` truncates `entries[cursor..]` (the
///   stale forward tail) before pushing (RISK-004).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JumpHistory {
    entries: Vec<JumpEntry>,
    cursor: usize,
}

impl JumpHistory {
    /// A fresh, empty jump history.
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            cursor: 0,
        }
    }

    /// Record a navigation jump. Called by the panel at the PRE-jump location of the four navigation
    /// jump dispatch sites (goto-def, find-references, outline/symbol, goto-line) — NOT on ordinary
    /// typing/arrow moves (RISK-006).
    ///
    /// Semantics:
    /// - **Forward-tail truncation** (RISK-004): if the cursor is not at the tail (the user navigated
    ///   back and is now making a new jump), the stale forward entries `entries[cursor..]` are discarded.
    /// - **Coalesce**: if the new entry coalesces with the entry immediately before the cursor (same file
    ///   within [`COALESCE_LINE_TOLERANCE`]), the prior entry's position is UPDATED in place rather than
    ///   pushing a duplicate — trivial near-position jumps do not flood the stack.
    /// - **Push + cap**: otherwise push the new entry and advance the cursor to the tail. If the stack
    ///   exceeds [`MAX_ENTRIES`], drop the oldest entry (and keep the cursor at the tail).
    pub fn record(&mut self, entry: JumpEntry) {
        // RISK-004: a new jump while not at the tail truncates the stale forward tail first.
        if self.cursor < self.entries.len() {
            self.entries.truncate(self.cursor);
        }
        // Coalesce with the immediately-preceding entry (the last one, since we just truncated to the
        // cursor) when it points at the same file within tolerance.
        if let Some(last) = self.entries.last_mut() {
            if entry.coalesces_with(last) {
                last.position = entry.position;
                self.cursor = self.entries.len();
                return;
            }
        }
        self.entries.push(entry);
        // Cap: drop the oldest when over the limit.
        if self.entries.len() > MAX_ENTRIES {
            let overflow = self.entries.len() - MAX_ENTRIES;
            self.entries.drain(0..overflow);
        }
        self.cursor = self.entries.len();
    }

    /// Navigate BACK: move the history cursor one entry toward the past and return the prior location to
    /// jump to, recording `current` (the location the user is jumping FROM) at the tail so a subsequent
    /// [`forward`](Self::forward) can return to it. Returns `None` when there is nothing to go back to
    /// (the cursor is already at the oldest entry / the stack is empty), leaving the cursor unchanged.
    ///
    /// On the FIRST Back (cursor at the tail), `current` is appended so Forward has a destination — this
    /// is how VS Code lets Alt+Right return you to where you pressed Alt+Left.
    pub fn back(&mut self, current: JumpEntry) -> Option<JumpEntry> {
        if self.entries.is_empty() || self.cursor == 0 {
            return None; // nothing in the past.
        }
        // If we are at the tail, append `current` so Forward can return here. The current location is
        // NOT one of the recorded jump origins (those were recorded at jump time); appending it keeps the
        // forward path symmetric without coalescing it away.
        if self.cursor == self.entries.len() {
            self.entries.push(current);
            // The appended `current` sits at index `cursor`; we will step back PAST the entries that
            // precede it. `cursor` still names the entry we are leaving.
        }
        self.cursor -= 1;
        self.entries.get(self.cursor).cloned()
    }

    /// Navigate FORWARD: move the history cursor one entry toward the future and return that location.
    /// Returns `None` when already at the tail (nothing forward), leaving the cursor unchanged.
    pub fn forward(&mut self) -> Option<JumpEntry> {
        if self.cursor + 1 >= self.entries.len() {
            return None; // already at (or past) the newest entry.
        }
        self.cursor += 1;
        self.entries.get(self.cursor).cloned()
    }

    /// True when [`back`](Self::back) would return a location (there is history in the past).
    pub fn can_back(&self) -> bool {
        !self.entries.is_empty() && self.cursor > 0
    }

    /// True when [`forward`](Self::forward) would return a location (there is history in the future).
    pub fn can_forward(&self) -> bool {
        self.cursor + 1 < self.entries.len()
    }

    /// The number of entries currently retained (for tests / diagnostics).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when the history holds no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: usize) -> BufferPosition {
        BufferPosition::new(line, 0)
    }

    fn entry(path: &str, line: usize) -> JumpEntry {
        JumpEntry::new(path, pos(line))
    }

    #[test]
    fn new_history_is_empty_and_cannot_navigate() {
        let h = JumpHistory::new();
        assert!(h.is_empty());
        assert!(!h.can_back());
        assert!(!h.can_forward());
    }

    #[test]
    fn back_returns_the_prior_location_after_a_jump() {
        // AC-003: after a goto-def jump from a.rs:10 to (currently at) a.rs:80, Navigate Back restores
        // a.rs:10.
        let mut h = JumpHistory::new();
        // record() is called with the PRE-jump location (where the user jumped FROM).
        h.record(entry("a.rs", 10));
        assert!(
            h.can_back(),
            "after recording a jump origin, Back is available"
        );
        let restored = h.back(entry("a.rs", 80));
        assert_eq!(restored, Some(entry("a.rs", 10)));
    }

    #[test]
    fn forward_returns_to_the_location_back_left_from() {
        let mut h = JumpHistory::new();
        h.record(entry("a.rs", 10));
        let back = h.back(entry("a.rs", 80));
        assert_eq!(back, Some(entry("a.rs", 10)));
        assert!(h.can_forward(), "after a Back, Forward is available");
        let fwd = h.forward();
        assert_eq!(
            fwd,
            Some(entry("a.rs", 80)),
            "Forward returns to where Back left from"
        );
        assert!(!h.can_forward(), "Forward is exhausted at the tail");
    }

    #[test]
    fn new_jump_after_back_truncates_the_forward_tail() {
        // AC-003 / RISK-004 / MC-004: jump origins 10, 20, 30; Back twice; a NEW jump truncates the
        // forward entries so Forward cannot reach the stale 30.
        let mut h = JumpHistory::new();
        h.record(entry("a.rs", 10));
        h.record(entry("a.rs", 20));
        h.record(entry("a.rs", 30));
        // Walk back: from the tail, Back appends current and returns 30, then 20.
        assert_eq!(h.back(entry("a.rs", 99)), Some(entry("a.rs", 30)));
        assert_eq!(h.back(entry("a.rs", 30)), Some(entry("a.rs", 20)));
        assert!(h.can_forward(), "a forward tail exists after two Backs");
        // A NEW jump truncates the forward tail.
        h.record(entry("b.rs", 5));
        assert!(
            !h.can_forward(),
            "the stale forward tail was truncated by the new jump"
        );
        // Forward now yields nothing (no stale 30/99).
        assert_eq!(h.forward(), None);
    }

    #[test]
    fn cross_file_back_restores_the_source_file_path() {
        // AC-004 / RISK-005: record a jump FROM file A, the user is now in file B; Back returns an entry
        // whose file_path == A (not the currently focused B).
        let mut h = JumpHistory::new();
        h.record(entry("a.rs", 42)); // jumped FROM a.rs:42 (into b.rs).
        let restored = h.back(entry("b.rs", 7)); // currently in b.rs.
        let restored = restored.expect("Back returns the source entry");
        assert_eq!(
            restored.file_path,
            PathBuf::from("a.rs"),
            "Back restores file A, not the focused B"
        );
        assert_eq!(restored.position, pos(42));
    }

    #[test]
    fn consecutive_same_line_jumps_coalesce() {
        // RISK-006-adjacent: trivial repeated jumps to the same file+line collapse to one entry.
        let mut h = JumpHistory::new();
        h.record(entry("a.rs", 10));
        h.record(entry("a.rs", 10));
        h.record(entry("a.rs", 10));
        assert_eq!(h.len(), 1, "same file+line jumps coalesce into one entry");
    }

    #[test]
    fn adjacent_line_jumps_within_tolerance_coalesce() {
        // VS Code "same region" coalescing: same file, lines within COALESCE_LINE_TOLERANCE collapse.
        let mut h = JumpHistory::new();
        h.record(entry("a.rs", 10));
        h.record(entry("a.rs", 10 + COALESCE_LINE_TOLERANCE));
        assert_eq!(
            h.len(),
            1,
            "adjacent-line same-file jumps within tolerance coalesce"
        );
    }

    #[test]
    fn distant_line_jumps_beyond_tolerance_do_not_coalesce() {
        let mut h = JumpHistory::new();
        h.record(entry("a.rs", 10));
        h.record(entry("a.rs", 10 + COALESCE_LINE_TOLERANCE + 5));
        assert_eq!(
            h.len(),
            2,
            "same-file jumps beyond tolerance are distinct entries"
        );
    }

    #[test]
    fn different_file_jumps_do_not_coalesce() {
        let mut h = JumpHistory::new();
        h.record(entry("a.rs", 10));
        h.record(entry("b.rs", 10));
        assert_eq!(
            h.len(),
            2,
            "different files are distinct entries even at the same line"
        );
    }

    #[test]
    fn stack_is_capped_at_max_entries_dropping_oldest() {
        let mut h = JumpHistory::new();
        // Record MAX_ENTRIES + 5 distinct (different-file) jumps so none coalesce.
        for i in 0..(MAX_ENTRIES + 5) {
            h.record(entry(&format!("f{i}.rs"), i));
        }
        assert_eq!(h.len(), MAX_ENTRIES, "stack capped at MAX_ENTRIES");
        // The oldest (f0..f4) were dropped; walking all the way back reaches f5, not f0.
        let mut last = None;
        // Drain backward to the oldest retained entry.
        let mut guard = 0;
        while h.can_back() {
            last = h.back(entry("cur.rs", 999));
            guard += 1;
            assert!(guard < MAX_ENTRIES + 10, "back loop terminates");
        }
        let oldest = last.expect("at least one back");
        assert_eq!(
            oldest.file_path,
            PathBuf::from("f5.rs"),
            "oldest 5 entries dropped by the cap"
        );
    }

    #[test]
    fn back_at_the_oldest_returns_none_without_moving() {
        let mut h = JumpHistory::new();
        h.record(entry("a.rs", 10));
        assert_eq!(h.back(entry("a.rs", 80)), Some(entry("a.rs", 10)));
        // Already at the oldest: a further Back is None and does not move the cursor.
        assert_eq!(h.back(entry("a.rs", 10)), None);
        assert!(!h.can_back());
        // Forward still works (returns to 80).
        assert_eq!(h.forward(), Some(entry("a.rs", 80)));
    }

    #[test]
    fn forward_at_the_tail_returns_none() {
        let mut h = JumpHistory::new();
        h.record(entry("a.rs", 10));
        // No Back happened, so there is no future.
        assert_eq!(h.forward(), None);
        assert!(!h.can_forward());
    }
}
