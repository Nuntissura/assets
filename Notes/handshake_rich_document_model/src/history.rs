//! Bounded undo/redo history for the rich-text document (WP-KERNEL-012 MT-011).
//!
//! [`UndoManager`] stores a `VecDeque<TransactionReceipt>` of applied transactions,
//! capped at [`DEFAULT_HISTORY_CAP`] (200). Pushing past the cap drops the OLDEST
//! entry, so a document edited for hours without a save cannot OOM the undo stack
//! (red-team RISK-6).
//!
//! Undo applies a receipt's INVERSE steps (already ordered by
//! [`super::transform::apply_transaction`] so they undo in one batch). Redo applies
//! the FORWARD steps. The undo/redo cursor tracks how many entries from the end have
//! been undone, so a fresh push after some undos truncates the redo tail (the
//! standard linear-history model — no redo branch).
//!
//! The manager re-runs the steps through `apply_transaction`, which re-validates the
//! schema and re-rolls-back on error, so an undo can never corrupt the document.

use std::collections::VecDeque;

use super::node::BlockNode;
use super::transform::{
    apply_transaction, ActorKind, Transaction, TransactionReceipt, TransformError,
};

/// Default maximum number of transactions retained for undo (MT-011 contract: 200).
pub const DEFAULT_HISTORY_CAP: usize = 200;

/// A bounded undo/redo stack over [`TransactionReceipt`]s.
#[derive(Debug, Clone)]
pub struct UndoManager {
    /// Applied transactions, oldest at the front. Length never exceeds `cap`.
    history: VecDeque<TransactionReceipt>,
    /// How many entries from the END have been undone (the redo depth). `0` means
    /// the cursor is at the newest entry (nothing to redo).
    undone: usize,
    /// The history cap; pushing past it drops the oldest entry.
    cap: usize,
}

impl UndoManager {
    /// A manager with the default 200-entry cap.
    pub fn new() -> Self {
        Self::with_cap(DEFAULT_HISTORY_CAP)
    }

    /// A manager with an explicit cap (the cap test uses 200; smaller caps are
    /// useful in focused tests). A cap of 0 is clamped to 1 so at least one undo is
    /// always possible.
    pub fn with_cap(cap: usize) -> Self {
        Self {
            history: VecDeque::new(),
            undone: 0,
            cap: cap.max(1),
        }
    }

    /// Record an applied transaction's receipt. Any pending redo tail (entries that
    /// were undone) is discarded first — a new edit forks linear history. If the
    /// history is at the cap, the OLDEST entry is dropped (RISK-6).
    pub fn push(&mut self, receipt: TransactionReceipt) {
        // Discard the redo tail: drop the `undone` newest entries.
        for _ in 0..self.undone {
            self.history.pop_back();
        }
        self.undone = 0;

        self.history.push_back(receipt);
        while self.history.len() > self.cap {
            self.history.pop_front();
        }
    }

    /// True when there is at least one transaction left to undo.
    pub fn can_undo(&self) -> bool {
        self.undone < self.history.len()
    }

    /// True when there is at least one undone transaction to redo.
    pub fn can_redo(&self) -> bool {
        self.undone > 0
    }

    /// Number of retained transactions (for the cap test).
    pub fn len(&self) -> usize {
        self.history.len()
    }

    /// True when no transactions are retained.
    pub fn is_empty(&self) -> bool {
        self.history.is_empty()
    }

    /// The newest retained transaction receipt, if any (read-only).
    ///
    /// WP-KERNEL-012 MT-072 remediation item 5: a mounted-editor proof must show an ATTRIBUTABLE
    /// receipt for the edit a custom keybinding chord produced — the receipt carries the actor kind +
    /// actor id and the exact forward/inverse steps, so a test can prove WHICH actor performed WHICH
    /// document mutation through the real input route rather than merely observing that the document
    /// changed. Read-only: the undo/redo cursor is untouched.
    pub fn last_receipt(&self) -> Option<&TransactionReceipt> {
        self.history.back()
    }

    /// Undo the most recent not-yet-undone transaction by applying its inverse steps
    /// to `doc`. Returns `Ok(true)` if an undo happened, `Ok(false)` if there was
    /// nothing to undo, or the transform error if the inverse failed (which leaves
    /// `doc` rolled back to before the undo attempt).
    pub fn undo(&mut self, doc: &mut BlockNode) -> Result<bool, TransformError> {
        if !self.can_undo() {
            return Ok(false);
        }
        // The entry to undo is `undone` steps back from the end.
        let idx = self.history.len() - 1 - self.undone;
        let receipt = &self.history[idx];
        let tx = Transaction::new(receipt.inverse.clone(), ActorKind::System, "undo");
        apply_transaction(doc, tx)?;
        self.undone += 1;
        Ok(true)
    }

    /// Redo the most recently undone transaction by re-applying its forward steps.
    /// Returns `Ok(true)` if a redo happened, `Ok(false)` if there was nothing to
    /// redo, or the transform error if the forward re-apply failed.
    pub fn redo(&mut self, doc: &mut BlockNode) -> Result<bool, TransformError> {
        if !self.can_redo() {
            return Ok(false);
        }
        // The entry to redo is the one most recently undone: `undone-1` from the end.
        let idx = self.history.len() - self.undone;
        let receipt = &self.history[idx];
        let tx = Transaction::new(receipt.forward.clone(), ActorKind::System, "redo");
        apply_transaction(doc, tx)?;
        self.undone -= 1;
        Ok(true)
    }
}

impl Default for UndoManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::node::BlockNode;
    use super::super::transform::Step;
    use super::*;

    fn doc() -> BlockNode {
        BlockNode::doc(vec![BlockNode::paragraph("")])
    }

    fn type_at_end(text: &str) -> Transaction {
        // Build an InsertText appending `text` at the end of the single paragraph's
        // text leaf. The test pre-knows the path is [0,0].
        Transaction::operator(vec![Step::InsertText {
            path: vec![0, 0],
            char_offset: usize::MAX, // RopeText clamps to append at end
            text: text.to_string(),
        }])
    }

    #[test]
    fn three_pushes_three_undos_restore_original() {
        let mut d = doc();
        let original = d.clone();
        let mut um = UndoManager::new();
        for chunk in ["a", "b", "c"] {
            let r = apply_transaction(&mut d, type_at_end(chunk)).unwrap();
            um.push(r);
        }
        assert_eq!(
            d.children[0].as_block().unwrap().children[0]
                .as_text()
                .unwrap()
                .text
                .to_string(),
            "abc"
        );
        um.undo(&mut d).unwrap();
        um.undo(&mut d).unwrap();
        um.undo(&mut d).unwrap();
        assert_eq!(d, original);
        assert!(!um.can_undo());
        assert!(um.can_redo());
    }

    #[test]
    fn redo_after_undo() {
        let mut d = doc();
        let mut um = UndoManager::new();
        let r = apply_transaction(&mut d, type_at_end("x")).unwrap();
        um.push(r);
        um.undo(&mut d).unwrap();
        assert_eq!(
            d.children[0].as_block().unwrap().children[0]
                .as_text()
                .unwrap()
                .text
                .to_string(),
            ""
        );
        um.redo(&mut d).unwrap();
        assert_eq!(
            d.children[0].as_block().unwrap().children[0]
                .as_text()
                .unwrap()
                .text
                .to_string(),
            "x"
        );
    }

    #[test]
    fn cap_drops_oldest_at_201() {
        let mut um = UndoManager::with_cap(DEFAULT_HISTORY_CAP);
        let mut d = doc();
        for _ in 0..201 {
            let r = apply_transaction(&mut d, type_at_end("z")).unwrap();
            um.push(r);
        }
        assert_eq!(um.len(), 200, "history must cap at 200 after 201 pushes");
    }

    #[test]
    fn new_push_truncates_redo_tail() {
        let mut d = doc();
        let mut um = UndoManager::new();
        um.push(apply_transaction(&mut d, type_at_end("a")).unwrap());
        um.push(apply_transaction(&mut d, type_at_end("b")).unwrap());
        um.undo(&mut d).unwrap(); // undo "b"
        assert!(um.can_redo());
        // A fresh edit while a redo is pending forks history (drops the redo tail).
        um.push(apply_transaction(&mut d, type_at_end("c")).unwrap());
        assert!(!um.can_redo());
        assert_eq!(um.len(), 2); // "a" and "c"
    }
}
