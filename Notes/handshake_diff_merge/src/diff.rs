//! Line-level and JSON-block diff mechanics (WP-KERNEL-012 MT-009).

use std::ops::Range;

use handshake_text_mechanics::buffer::TextBuffer;

/// The change classification of a [`DiffBlock`]. Mirrors the React
/// `RichDocumentDiffStatus` union (`unchanged | modified | added | removed`); `Equal` is the native
/// name for `unchanged` so the enum reads naturally next to `similar::ChangeTag::Equal`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffStatus {
    /// The lines are identical on both sides.
    Equal,
    /// The lines exist only on the right (an insertion).
    Added,
    /// The lines exist only on the left (a deletion).
    Removed,
    /// The lines differ on both sides (a delete run fused with the insert run that replaced it).
    Modified,
}

/// One contiguous run of the diff: the half-open LINE range it occupies on each side plus its
/// [`DiffStatus`]. A `Removed` block has an EMPTY `right_lines` (nothing on the right); an `Added`
/// block has an EMPTY `left_lines`. `Equal`/`Modified` blocks carry both ranges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffBlock {
    /// Half-open line range on the LEFT (old) buffer this block covers. Empty for `Added`.
    pub left_lines: Range<usize>,
    /// Half-open line range on the RIGHT (new) buffer this block covers. Empty for `Removed`.
    pub right_lines: Range<usize>,
    /// The change classification.
    pub status: DiffStatus,
}

/// Stateless line-level diff engine. Computes a [`Vec<DiffBlock>`] for two [`TextBuffer`]s using
/// Myers' diff via the `similar` crate.
pub struct DiffEngine;

impl DiffEngine {
    /// Diff two buffers at the LINE level, returning one [`DiffBlock`] per contiguous run of equal /
    /// added / removed / modified lines (MT step 1). The blocks are returned in document order and
    /// their left/right ranges tile the two buffers without gaps or overlaps.
    pub fn diff(left: &TextBuffer, right: &TextBuffer) -> Vec<DiffBlock> {
        let left_text = left.to_string();
        let right_text = right.to_string();
        Self::diff_text(&left_text, &right_text)
    }

    /// Diff two `&str`s at the line level. Split out from [`diff`](Self::diff) so the unit tests and
    /// the prose entry point share one implementation without forcing a `TextBuffer` allocation.
    pub fn diff_text(left_text: &str, right_text: &str) -> Vec<DiffBlock> {
        let diff = similar::TextDiff::from_lines(left_text, right_text);

        // First, flatten `similar`'s changes into per-line (tag, side-index) tuples in document order.
        // `similar` already groups by tag in its iterator, but we re-walk so we can FUSE an adjacent
        // Delete-run + Insert-run into a single Modified block (MT step 1 — the "changed line" case).
        let mut blocks: Vec<DiffBlock> = Vec::new();

        // Track the next unallocated line index on each side so each block gets contiguous ranges.
        let mut left_cursor = 0usize;
        let mut right_cursor = 0usize;

        // Collect the change tags into runs of the same tag (a "run" = maximal consecutive same-tag).
        let mut runs: Vec<(similar::ChangeTag, usize)> = Vec::new();
        for change in diff.iter_all_changes() {
            let tag = change.tag();
            match runs.last_mut() {
                Some((last_tag, count)) if *last_tag == tag => *count += 1,
                _ => runs.push((tag, 1)),
            }
        }

        let mut i = 0usize;
        while i < runs.len() {
            let (tag, count) = runs[i];
            match tag {
                similar::ChangeTag::Equal => {
                    let l = left_cursor..left_cursor + count;
                    let r = right_cursor..right_cursor + count;
                    left_cursor += count;
                    right_cursor += count;
                    blocks.push(DiffBlock {
                        left_lines: l,
                        right_lines: r,
                        status: DiffStatus::Equal,
                    });
                    i += 1;
                }
                similar::ChangeTag::Delete => {
                    // A Delete run immediately followed by an Insert run is a MODIFIED block (lines
                    // replaced in place), not a separate Removed + Added (MT step 1).
                    if let Some((similar::ChangeTag::Insert, ins_count)) = runs.get(i + 1).copied()
                    {
                        let l = left_cursor..left_cursor + count;
                        let r = right_cursor..right_cursor + ins_count;
                        left_cursor += count;
                        right_cursor += ins_count;
                        blocks.push(DiffBlock {
                            left_lines: l,
                            right_lines: r,
                            status: DiffStatus::Modified,
                        });
                        i += 2; // consumed both the Delete and the Insert run.
                    } else {
                        let l = left_cursor..left_cursor + count;
                        let r = right_cursor..right_cursor; // empty on the right.
                        left_cursor += count;
                        blocks.push(DiffBlock {
                            left_lines: l,
                            right_lines: r,
                            status: DiffStatus::Removed,
                        });
                        i += 1;
                    }
                }
                similar::ChangeTag::Insert => {
                    let l = left_cursor..left_cursor; // empty on the left.
                    let r = right_cursor..right_cursor + count;
                    right_cursor += count;
                    blocks.push(DiffBlock {
                        left_lines: l,
                        right_lines: r,
                        status: DiffStatus::Added,
                    });
                    i += 1;
                }
            }
        }

        blocks
    }
}

// ── Deferred prose/JSON block-level diff seam (E2, MT-011+) ──────────────────────────────────────

/// DEFERRED prose/JSON-block diff entry point (MT-009 contract: the JSON-block-level prose diff
/// targets the E2 native rich-document format that does not exist yet). Provided as the design seam
/// (and exercised by a unit test) so the block-level path can be added later without reshaping this
/// module. It aligns two arrays of JSON block nodes by index and classifies each position the same
/// way the line diff does (`Added` / `Removed` / `Modified` / `Equal`), returning [`DiffBlock`]s
/// whose ranges are block INDICES rather than line indices.
///
/// This is intentionally a simple index alignment (not the React LCS block alignment) because the
/// native rich-document format is not yet defined; when E2 lands, this is the hook to replace with
/// the format-aware alignment ported from `buildRichDocumentDiff`.
pub fn diff_json_blocks(left: &[serde_json::Value], right: &[serde_json::Value]) -> Vec<DiffBlock> {
    let count = left.len().max(right.len());
    let mut blocks = Vec::new();
    for index in 0..count {
        let l = left.get(index);
        let r = right.get(index);
        let status = match (l, r) {
            (None, Some(_)) => DiffStatus::Added,
            (Some(_), None) => DiffStatus::Removed,
            (Some(a), Some(b)) if a == b => DiffStatus::Equal,
            (Some(_), Some(_)) => DiffStatus::Modified,
            (None, None) => continue,
        };
        let l_range = if l.is_some() {
            index..index + 1
        } else {
            index..index
        };
        let r_range = if r.is_some() {
            index..index + 1
        } else {
            index..index
        };
        blocks.push(DiffBlock {
            left_lines: l_range,
            right_lines: r_range,
            status,
        });
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── DiffEngine line-level (AC-001: ported from document_diff_merge.test.ts) ──────────────────

    #[test]
    fn identical_texts_are_all_equal() {
        let left = TextBuffer::new("line0\nline1\nline2");
        let right = TextBuffer::new("line0\nline1\nline2");
        let blocks = DiffEngine::diff(&left, &right);
        assert!(
            blocks.iter().all(|b| b.status == DiffStatus::Equal),
            "identical texts -> all Equal blocks; got {blocks:?}"
        );
        // And the Equal block(s) cover every line.
        let covered: usize = blocks
            .iter()
            .filter(|b| b.status == DiffStatus::Equal)
            .map(|b| b.left_lines.len())
            .sum();
        assert_eq!(covered, 3, "all 3 lines covered by Equal blocks");
    }

    #[test]
    fn added_line_yields_one_added_block() {
        let left = TextBuffer::new("a\nb\nc");
        let right = TextBuffer::new("a\nb\nNEW\nc");
        let blocks = DiffEngine::diff(&left, &right);
        let added: Vec<_> = blocks
            .iter()
            .filter(|b| b.status == DiffStatus::Added)
            .collect();
        assert_eq!(added.len(), 1, "exactly one Added block; got {blocks:?}");
        assert_eq!(
            added[0].left_lines.len(),
            0,
            "Added block is empty on the left"
        );
        assert_eq!(
            added[0].right_lines.len(),
            1,
            "Added block covers one right line"
        );
    }

    #[test]
    fn removed_line_yields_one_removed_block() {
        let left = TextBuffer::new("a\nb\nc\nd");
        let right = TextBuffer::new("a\nb\nd");
        let blocks = DiffEngine::diff(&left, &right);
        let removed: Vec<_> = blocks
            .iter()
            .filter(|b| b.status == DiffStatus::Removed)
            .collect();
        assert_eq!(
            removed.len(),
            1,
            "exactly one Removed block; got {blocks:?}"
        );
        assert_eq!(
            removed[0].right_lines.len(),
            0,
            "Removed block is empty on the right"
        );
        assert_eq!(
            removed[0].left_lines.len(),
            1,
            "Removed block covers one left line"
        );
    }

    #[test]
    fn modified_line_yields_one_modified_block() {
        let left = TextBuffer::new("a\nMID\nc");
        let right = TextBuffer::new("a\nCHANGED\nc");
        let blocks = DiffEngine::diff(&left, &right);
        let modified: Vec<_> = blocks
            .iter()
            .filter(|b| b.status == DiffStatus::Modified)
            .collect();
        assert_eq!(
            modified.len(),
            1,
            "a delete+insert at the same position is ONE Modified block; got {blocks:?}"
        );
        assert_eq!(
            modified[0].left_lines.len(),
            1,
            "Modified covers one left line"
        );
        assert_eq!(
            modified[0].right_lines.len(),
            1,
            "Modified covers one right line"
        );
    }

    #[test]
    fn diff_blocks_tile_both_buffers_without_gaps() {
        // RISK-002 basis: the left/right ranges must tile each buffer contiguously so the sync-scroll
        // line map and the background rects line up. Use a mix of add/remove/modify/equal.
        let left = TextBuffer::new("keep1\nremoveme\nmod_old\nkeep2");
        let right = TextBuffer::new("keep1\nmod_new\nkeep2\nadded");
        let blocks = DiffEngine::diff(&left, &right);

        let mut next_left = 0usize;
        let mut next_right = 0usize;
        for b in &blocks {
            assert_eq!(
                b.left_lines.start, next_left,
                "left ranges are contiguous: {blocks:?}"
            );
            assert_eq!(
                b.right_lines.start, next_right,
                "right ranges are contiguous: {blocks:?}"
            );
            next_left = b.left_lines.end;
            next_right = b.right_lines.end;
        }
        assert_eq!(
            next_left,
            left.to_string().lines().count(),
            "left fully tiled"
        );
        assert_eq!(
            next_right,
            right.to_string().lines().count(),
            "right fully tiled"
        );
    }

    // ── Deferred prose/JSON block seam ────────────────────────────────────────────────────────────

    #[test]
    fn json_block_diff_classifies_by_index() {
        let left = vec![
            serde_json::json!({"type":"p","text":"a"}),
            serde_json::json!({"type":"p","text":"b"}),
        ];
        let right = vec![
            serde_json::json!({"type":"p","text":"a"}),
            serde_json::json!({"type":"p","text":"CHANGED"}),
            serde_json::json!({"type":"p","text":"NEW"}),
        ];
        let blocks = diff_json_blocks(&left, &right);
        assert_eq!(blocks[0].status, DiffStatus::Equal, "block 0 identical");
        assert_eq!(blocks[1].status, DiffStatus::Modified, "block 1 changed");
        assert_eq!(
            blocks[2].status,
            DiffStatus::Added,
            "block 2 only on the right"
        );
    }
}
