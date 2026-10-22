//! Base-aligned three-way text merge (WP-KERNEL-012 MT-009).

use std::ops::Range;

use handshake_text_mechanics::buffer::TextBuffer;
use crate::diff::{DiffEngine, DiffStatus};

/// The change classification of a [`MergeBlock`]. Mirrors the React `RichDocumentMergeStatus`
/// (`local_only | remote_only | both_same | conflict`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeStatus {
    /// Only the LOCAL side changed this line range relative to base.
    LocalOnly,
    /// Only the REMOTE side changed this line range relative to base.
    RemoteOnly,
    /// Both sides changed this line range to the SAME value (no conflict).
    BothSame,
    /// Both sides changed this line range to DIFFERENT values (a conflict needing a choice).
    Conflict,
}

/// Which side's content the operator (or an agent) chose for a [`MergeBlock`]. Mirrors the React
/// `RichDocumentMergeChoice` (`local | remote | both`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeChoice {
    /// Keep the local version.
    Local,
    /// Keep the remote version.
    Remote,
    /// Keep both versions (local lines followed by remote lines).
    Both,
}

/// One block of a three-way merge plan: the half-open LINE ranges it covers in the base, local, and
/// remote buffers, its [`MergeStatus`], and the operator's [`MergeChoice`] once made. Mirrors the
/// React `RichDocumentMergeBlock`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeBlock {
    /// Half-open line range in the BASE buffer this block aligns to.
    pub base_lines: Range<usize>,
    /// Half-open line range in the LOCAL buffer.
    pub local_lines: Range<usize>,
    /// Half-open line range in the REMOTE buffer.
    pub remote_lines: Range<usize>,
    /// The change classification relative to base.
    pub status: MergeStatus,
    /// The chosen side, set when the operator/agent resolves a `Conflict` (or pre-set for the
    /// non-conflict statuses so [`MergeEngine::apply`] can emit a merged buffer without a choice).
    pub chosen: Option<MergeChoice>,
}

/// Stateless three-way merge engine. Aligns local-vs-base and remote-vs-base line diffs into merge
/// blocks (MT step computing local/remote diffs then aligning) and applies the chosen sides into a
/// merged buffer.
pub struct MergeEngine;

impl MergeEngine {
    /// Compute the three-way merge blocks for `base` / `local` / `remote` (MT contract
    /// `three_way`). Computes base->local and base->remote diff spans, then aligns those spans by
    /// base range / insertion point. This is deliberately NOT a raw same-index line walk: insertions
    /// and deletions before a later edit must not shift the other side into a false conflict. For each
    /// aligned change group it classifies whether local and/or remote changed relative to base:
    ///   - neither changed -> NO block emitted (the line is unchanged, like the React port which
    ///     `continue`s when `!localChanged && !remoteChanged`).
    ///   - only local changed  -> [`MergeStatus::LocalOnly`] (chosen pre-set to `Local`).
    ///   - only remote changed -> [`MergeStatus::RemoteOnly`] (chosen pre-set to `Remote`).
    ///   - both changed to the same value -> [`MergeStatus::BothSame`] (chosen pre-set to `Local`).
    ///   - both changed differently -> [`MergeStatus::Conflict`] (chosen `None` — needs a choice).
    pub fn three_way(
        base: &TextBuffer,
        local: &TextBuffer,
        remote: &TextBuffer,
    ) -> Vec<MergeBlock> {
        let base_text = base.to_string();
        let local_text = local.to_string();
        let remote_text = remote.to_string();
        let local_lines = lines_from_text(&local_text);
        let remote_lines = lines_from_text(&remote_text);

        let local_changes = side_changes_from_base(&base_text, &local_text);
        let remote_changes = side_changes_from_base(&base_text, &remote_text);
        let groups = collect_change_groups(local_changes, remote_changes);

        groups
            .into_iter()
            .map(|group| {
                let (status, chosen) = match (group.has_local, group.has_remote) {
                    (true, false) => (MergeStatus::LocalOnly, Some(MergeChoice::Local)),
                    (false, true) => (MergeStatus::RemoteOnly, Some(MergeChoice::Remote)),
                    (true, true) => {
                        let local_content = line_slice(&local_lines, &group.local_lines);
                        let remote_content = line_slice(&remote_lines, &group.remote_lines);
                        if local_content == remote_content {
                            (MergeStatus::BothSame, Some(MergeChoice::Local))
                        } else {
                            (MergeStatus::Conflict, None)
                        }
                    }
                    (false, false) => {
                        unreachable!("change groups always contain at least one side")
                    }
                };
                MergeBlock {
                    base_lines: group.base_lines,
                    local_lines: group.local_lines,
                    remote_lines: group.remote_lines,
                    status,
                    chosen,
                }
            })
            .collect()
    }

    /// Apply a resolved merge plan to produce the merged buffer text (MT step 5). The plan is walked by
    /// base ranges, so insertions and deletions do not corrupt later line indexes. Unchanged base ranges
    /// are emitted from `base`; changed ranges are emitted from the selected side. A `Conflict` block
    /// with no `chosen` preserves BOTH sides (local first, then remote), so an unresolved preview is total
    /// without silently dropping either side.
    pub fn apply(
        base: &TextBuffer,
        local: &TextBuffer,
        remote: &TextBuffer,
        blocks: &[MergeBlock],
    ) -> TextBuffer {
        let base_lines = buffer_lines(base);
        let local_lines = buffer_lines(local);
        let remote_lines = buffer_lines(remote);

        let mut ordered: Vec<&MergeBlock> = blocks.iter().collect();
        ordered.sort_by_key(|b| (b.base_lines.start, b.base_lines.end));
        let mut merged: Vec<String> = Vec::new();
        let mut base_cursor = 0usize;

        for block in ordered {
            let start = block.base_lines.start.min(base_lines.len());
            if start > base_cursor {
                merged.extend(base_lines[base_cursor..start].iter().cloned());
            }

            let choice = block.chosen.unwrap_or(MergeChoice::Both);
            match choice {
                MergeChoice::Local => extend_range(&mut merged, &local_lines, &block.local_lines),
                MergeChoice::Remote => {
                    extend_range(&mut merged, &remote_lines, &block.remote_lines)
                }
                MergeChoice::Both => {
                    extend_range(&mut merged, &local_lines, &block.local_lines);
                    extend_range(&mut merged, &remote_lines, &block.remote_lines);
                }
            }
            base_cursor = base_cursor.max(block.base_lines.end.min(base_lines.len()));
        }

        if base_cursor < base_lines.len() {
            merged.extend(base_lines[base_cursor..].iter().cloned());
        }

        TextBuffer::new(&merged.join("\n"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum MergeSide {
    Local,
    Remote,
}

#[derive(Clone, Debug)]
struct SideChange {
    base_lines: Range<usize>,
    side_lines: Range<usize>,
}

#[derive(Clone, Debug)]
struct PendingChange {
    side: MergeSide,
    base_lines: Range<usize>,
    side_lines: Range<usize>,
}

#[derive(Clone, Debug)]
struct ChangeGroup {
    base_lines: Range<usize>,
    local_lines: Range<usize>,
    remote_lines: Range<usize>,
    has_local: bool,
    has_remote: bool,
}

impl ChangeGroup {
    fn from_change(change: PendingChange) -> Self {
        match change.side {
            MergeSide::Local => Self {
                base_lines: change.base_lines,
                local_lines: change.side_lines,
                remote_lines: 0..0,
                has_local: true,
                has_remote: false,
            },
            MergeSide::Remote => Self {
                base_lines: change.base_lines,
                local_lines: 0..0,
                remote_lines: change.side_lines,
                has_local: false,
                has_remote: true,
            },
        }
    }

    fn absorb(&mut self, change: PendingChange) {
        self.base_lines = union_ranges(&self.base_lines, &change.base_lines);
        match change.side {
            MergeSide::Local => {
                self.local_lines = if self.has_local {
                    union_ranges(&self.local_lines, &change.side_lines)
                } else {
                    change.side_lines
                };
                self.has_local = true;
            }
            MergeSide::Remote => {
                self.remote_lines = if self.has_remote {
                    union_ranges(&self.remote_lines, &change.side_lines)
                } else {
                    change.side_lines
                };
                self.has_remote = true;
            }
        }
    }
}

fn side_changes_from_base(base_text: &str, side_text: &str) -> Vec<SideChange> {
    DiffEngine::diff_text(base_text, side_text)
        .into_iter()
        .filter(|block| block.status != DiffStatus::Equal)
        .map(|block| SideChange {
            base_lines: block.left_lines,
            side_lines: block.right_lines,
        })
        .collect()
}

fn collect_change_groups(
    local_changes: Vec<SideChange>,
    remote_changes: Vec<SideChange>,
) -> Vec<ChangeGroup> {
    let mut pending = Vec::with_capacity(local_changes.len() + remote_changes.len());
    pending.extend(local_changes.into_iter().map(|change| PendingChange {
        side: MergeSide::Local,
        base_lines: change.base_lines,
        side_lines: change.side_lines,
    }));
    pending.extend(remote_changes.into_iter().map(|change| PendingChange {
        side: MergeSide::Remote,
        base_lines: change.base_lines,
        side_lines: change.side_lines,
    }));
    pending.sort_by_key(|change| {
        (
            change.base_lines.start,
            change.base_lines.end,
            change.side,
            change.side_lines.start,
            change.side_lines.end,
        )
    });

    let mut groups: Vec<ChangeGroup> = Vec::new();
    for change in pending {
        if let Some(group) = groups.last_mut() {
            if ranges_interact(&group.base_lines, &change.base_lines) {
                group.absorb(change);
                continue;
            }
        }
        groups.push(ChangeGroup::from_change(change));
    }
    groups
}

fn ranges_interact(a: &Range<usize>, b: &Range<usize>) -> bool {
    let a_empty = a.start == a.end;
    let b_empty = b.start == b.end;
    match (a_empty, b_empty) {
        (true, true) => a.start == b.start,
        (true, false) => a.start > b.start && a.start < b.end,
        (false, true) => b.start > a.start && b.start < a.end,
        (false, false) => a.start < b.end && b.start < a.end,
    }
}

fn union_ranges(a: &Range<usize>, b: &Range<usize>) -> Range<usize> {
    a.start.min(b.start)..a.end.max(b.end)
}

fn line_slice(lines: &[String], range: &Range<usize>) -> Vec<String> {
    lines
        .get(range.start.min(lines.len())..range.end.min(lines.len()))
        .unwrap_or(&[])
        .to_vec()
}

fn extend_range(out: &mut Vec<String>, lines: &[String], range: &Range<usize>) {
    if let Some(slice) = lines.get(range.start.min(lines.len())..range.end.min(lines.len())) {
        out.extend(slice.iter().cloned());
    }
}

/// Split a [`TextBuffer`] into its logical lines (without trailing newlines), for the line-aligned
/// three-way merge. A buffer ending in `\n` does NOT yield a trailing empty line here (the merge
/// aligns content lines), matching the React port which aligns `content` array entries.
fn buffer_lines(buffer: &TextBuffer) -> Vec<String> {
    let text = buffer.to_string();
    lines_from_text(&text)
}

fn lines_from_text(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.lines().map(|l| l.to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── MergeEngine three-way (AC-002: ported from document_diff_merge.test.ts) ───────────────────

    #[test]
    fn non_overlapping_edits_are_local_only_and_remote_only() {
        // local adds to line 2, remote adds to line 5 of the same base -> two non-conflict blocks.
        let base = TextBuffer::new("l0\nl1\nl2\nl3\nl4\nl5");
        let local = TextBuffer::new("l0\nl1\nLOCAL\nl3\nl4\nl5");
        let remote = TextBuffer::new("l0\nl1\nl2\nl3\nl4\nREMOTE");
        let blocks = MergeEngine::three_way(&base, &local, &remote);

        let local_only = blocks
            .iter()
            .filter(|b| b.status == MergeStatus::LocalOnly)
            .count();
        let remote_only = blocks
            .iter()
            .filter(|b| b.status == MergeStatus::RemoteOnly)
            .count();
        let conflicts = blocks
            .iter()
            .filter(|b| b.status == MergeStatus::Conflict)
            .count();
        assert_eq!(local_only, 1, "one LocalOnly block; got {blocks:?}");
        assert_eq!(remote_only, 1, "one RemoteOnly block; got {blocks:?}");
        assert_eq!(
            conflicts, 0,
            "no conflicts for non-overlapping edits; got {blocks:?}"
        );
    }

    #[test]
    fn both_modify_same_line_differently_is_a_conflict() {
        let base = TextBuffer::new("l0\nl1\nl2");
        let local = TextBuffer::new("l0\nLOCAL_EDIT\nl2");
        let remote = TextBuffer::new("l0\nREMOTE_EDIT\nl2");
        let blocks = MergeEngine::three_way(&base, &local, &remote);
        let conflicts: Vec<_> = blocks
            .iter()
            .filter(|b| b.status == MergeStatus::Conflict)
            .collect();
        assert_eq!(conflicts.len(), 1, "one Conflict block; got {blocks:?}");
        assert_eq!(
            conflicts[0].chosen, None,
            "an unresolved conflict has no chosen side"
        );
    }

    #[test]
    fn both_modify_same_line_identically_is_both_same() {
        let base = TextBuffer::new("l0\nl1\nl2");
        let local = TextBuffer::new("l0\nSAME\nl2");
        let remote = TextBuffer::new("l0\nSAME\nl2");
        let blocks = MergeEngine::three_way(&base, &local, &remote);
        assert_eq!(
            blocks.len(),
            1,
            "one block for the single changed line; got {blocks:?}"
        );
        assert_eq!(
            blocks[0].status,
            MergeStatus::BothSame,
            "identical edits -> BothSame"
        );
    }

    #[test]
    fn apply_accept_local_emits_local_version_of_conflict() {
        // AC-005 basis (engine half): a conflict resolved to Local must put the local line in the
        // merged output.
        let base = TextBuffer::new("l0\nl1\nl2");
        let local = TextBuffer::new("l0\nLOCAL_EDIT\nl2");
        let remote = TextBuffer::new("l0\nREMOTE_EDIT\nl2");
        let mut blocks = MergeEngine::three_way(&base, &local, &remote);
        // Resolve the single conflict to Local.
        for b in &mut blocks {
            if b.status == MergeStatus::Conflict {
                b.chosen = Some(MergeChoice::Local);
            }
        }
        let merged = MergeEngine::apply(&base, &local, &remote, &blocks);
        let merged_text = merged.to_string();
        assert!(
            merged_text.contains("LOCAL_EDIT"),
            "Accept Local -> merged buffer contains the local version; got {merged_text:?}"
        );
        assert!(
            !merged_text.contains("REMOTE_EDIT"),
            "Accept Local -> merged buffer does NOT contain the remote version; got {merged_text:?}"
        );
    }

    #[test]
    fn apply_accept_remote_emits_remote_version_of_conflict() {
        let base = TextBuffer::new("l0\nl1\nl2");
        let local = TextBuffer::new("l0\nLOCAL_EDIT\nl2");
        let remote = TextBuffer::new("l0\nREMOTE_EDIT\nl2");
        let mut blocks = MergeEngine::three_way(&base, &local, &remote);
        for b in &mut blocks {
            if b.status == MergeStatus::Conflict {
                b.chosen = Some(MergeChoice::Remote);
            }
        }
        let merged = MergeEngine::apply(&base, &local, &remote, &blocks);
        let merged_text = merged.to_string();
        assert!(
            merged_text.contains("REMOTE_EDIT"),
            "Accept Remote -> remote version; {merged_text:?}"
        );
        assert!(
            !merged_text.contains("LOCAL_EDIT"),
            "Accept Remote -> not local; {merged_text:?}"
        );
    }

    #[test]
    fn apply_accept_both_emits_local_then_remote() {
        let base = TextBuffer::new("l0\nl1\nl2");
        let local = TextBuffer::new("l0\nLOCAL_EDIT\nl2");
        let remote = TextBuffer::new("l0\nREMOTE_EDIT\nl2");
        let mut blocks = MergeEngine::three_way(&base, &local, &remote);
        for b in &mut blocks {
            if b.status == MergeStatus::Conflict {
                b.chosen = Some(MergeChoice::Both);
            }
        }
        let merged = MergeEngine::apply(&base, &local, &remote, &blocks);
        let merged_text = merged.to_string();
        let local_pos = merged_text.find("LOCAL_EDIT").expect("local present");
        let remote_pos = merged_text.find("REMOTE_EDIT").expect("remote present");
        assert!(
            local_pos < remote_pos,
            "Accept Both -> local line before remote line; {merged_text:?}"
        );
    }

    #[test]
    fn apply_unchanged_lines_pass_through() {
        let base = TextBuffer::new("keep0\nl1\nkeep2");
        let local = TextBuffer::new("keep0\nLOCAL\nkeep2");
        let remote = TextBuffer::new("keep0\nl1\nkeep2");
        let blocks = MergeEngine::three_way(&base, &local, &remote);
        let merged = MergeEngine::apply(&base, &local, &remote, &blocks).to_string();
        assert!(
            merged.contains("keep0") && merged.contains("keep2"),
            "unchanged lines survive: {merged:?}"
        );
        assert!(
            merged.contains("LOCAL"),
            "the local-only edit is applied: {merged:?}"
        );
    }
}
