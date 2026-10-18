//! In-file replacement operations (WP-KERNEL-012 MT-004/MT-108).

use handshake_text_mechanics::buffer::TextBuffer;

use crate::search::{FindEngine, Match};

/// Maximum number of matches a single Replace All click applies before yielding control back to the UI
/// (MT-108 residual hardening for MT-004). A Replace All over a pathologically large match set (tens of
/// thousands of hits in a huge document) would otherwise mutate the buffer in one unbounded synchronous
/// pass and block the UI thread. The panel caps each click at this many replacements — taking the
/// lowest-offset matches first so repeated clicks walk the document top-to-bottom — and surfaces the
/// remaining count as a progress hint so the operator can finish the set with another click. Kept as a
/// bounded per-click cap (rather than an off-thread worker) because the replace is a synchronous buffer
/// mutation and a bounded cap is the minimal, deterministic, deadlock-free control.
pub const REPLACE_ALL_CAP: usize = 1000;

impl FindEngine {
    /// Replace a single match `m` in `buffer` with `replacement`. Returns `true` on success. The
    /// caller MUST re-run [`search`](Self::search) afterward — the remaining match offsets are now
    /// stale (RISK-003). A no-op (and `false`) if the range is invalid for the current buffer.
    pub fn replace_one(buffer: &mut TextBuffer, m: &Match, replacement: &str) -> bool {
        if m.byte_range.end > buffer.len_bytes() || m.byte_range.start > m.byte_range.end {
            return false;
        }
        // Delete the matched bytes then insert the replacement at the (now-vacated) start.
        if buffer.delete(m.byte_range.clone()).is_err() {
            return false;
        }
        buffer.insert(m.byte_range.start, replacement).is_ok()
    }

    /// Replace ALL of `matches` in `buffer` with `replacement`, processing in REVERSE byte order so an
    /// earlier replacement never shifts a later match's stored offset out from under it (RISK-003 /
    /// MC-002). Returns the number of replacements actually applied. The caller MUST re-run
    /// [`search`](Self::search) afterward; the input `matches` are fully consumed/invalidated here.
    pub fn replace_all(buffer: &mut TextBuffer, matches: &[Match], replacement: &str) -> usize {
        // Sort a copy by descending start so we mutate high offsets first; the caller's list is already
        // ascending, but never assume — a defensive sort keeps the reverse-order invariant true even if
        // a caller passes an unsorted list.
        let mut ordered: Vec<&Match> = matches.iter().collect();
        ordered.sort_by_key(|item| std::cmp::Reverse(item.byte_range.start));
        let mut applied = 0usize;
        let mut last_start = usize::MAX;
        for m in ordered {
            // Skip a match that overlaps one we just replaced (defensive: overlapping matches in the
            // same batch would corrupt offsets). Ascending-sorted non-overlapping input never hits this.
            if m.byte_range.end > last_start {
                continue;
            }
            if Self::replace_one(buffer, m, replacement) {
                applied += 1;
                last_start = m.byte_range.start;
            }
        }
        applied
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::FindQuery;

    #[test]
    fn replace_one_replaces_correct_match_and_adjusts_buffer() {
        let mut buf = TextBuffer::new("foo bar foo");
        let q = FindQuery::literal("foo");
        let matches = FindEngine::search(&q, &buf);
        assert_eq!(matches.len(), 2);
        // Replace the SECOND match (8..11) with "X"; the first is untouched.
        assert!(FindEngine::replace_one(&mut buf, &matches[1], "X"));
        assert_eq!(buf.to_string(), "foo bar X");
    }

    #[test]
    fn replace_all_replaces_all_occurrences_offset_correct() {
        // MC-002: replace_all must replace every occurrence with correct offsets (reverse order so a
        // length-changing replacement does not corrupt later offsets).
        let mut buf = TextBuffer::new("foo foo foo");
        let q = FindQuery::literal("foo");
        let matches = FindEngine::search(&q, &buf);
        assert_eq!(matches.len(), 3);
        // Replace with a LONGER string so a forward (wrong-order) walk would corrupt offsets.
        let applied = FindEngine::replace_all(&mut buf, &matches, "LONGER");
        assert_eq!(applied, 3);
        assert_eq!(buf.to_string(), "LONGER LONGER LONGER");
    }

    #[test]
    fn replace_all_with_shorter_replacement_offset_correct() {
        let mut buf = TextBuffer::new("aaaa aaaa aaaa");
        let q = FindQuery::literal("aaaa");
        let matches = FindEngine::search(&q, &buf);
        let applied = FindEngine::replace_all(&mut buf, &matches, "b");
        assert_eq!(applied, 3);
        assert_eq!(buf.to_string(), "b b b");
    }
}
