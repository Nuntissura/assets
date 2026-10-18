//! Document-wide find scanner (WP-KERNEL-012 MT-018).
//!
//! Ports `app/src/lib/editor/find_replace.ts` (`buildFindMatches`, the segment scan, the
//! whole-word boundary post-filter, `EMPTY_SCAN`, the invalid-regex typed error) onto the
//! MT-011 [`BlockNode`] document model. The scan walks the typed block tree and collects, for
//! every text run AND every `code_block`'s rope, the CHAR ranges where the query matches —
//! **relative to each [`crate::node::TextLeaf`] rope**, not absolute document offsets, because the MT-018
//! replace operations target a specific rope position inside a leaf (MT impl note 1).
//!
//! ## Why per-leaf char ranges (not absolute document positions)
//!
//! The React port uses absolute ProseMirror positions because ProseMirror replace ops address
//! the whole-doc position space. The native model's [`crate::transform::Step::DeleteText`]/[`crate::transform::Step::InsertText`]
//! instead address a `(node_path, char_offset)` INSIDE one [`crate::node::TextLeaf`] rope. So a [`FindMatch`]
//! carries the leaf's `node_path` (the child-index path to that text leaf) plus the
//! `[char_start, char_end)` range WITHIN that leaf. This is the address Replace feeds straight to
//! a transaction step with no remapping (RISK fix vs. an absolute-offset design).
//!
//! ## Engine: Rust `regex` (linear-time), NOT a thread-timeout (KERNEL_BUILDER gate)
//!
//! The contract's RISK-003/MC-003 worried about catastrophic backtracking (`(a+)+$`). The Rust
//! `regex` crate is a finite-automaton engine with NO backtracking, so that input does NOT hang
//! it — the MT contract's KERNEL_BUILDER gate explicitly corrects RISK-003. We therefore drop the
//! thread/channel-timeout machinery and instead use [`regex::RegexBuilder::size_limit`] /
//! `dfa_size_limit` (a compile-time memory guard) plus the [`MAX_MATCHES`] cap (mirroring the React
//! `EMPTY_SCAN`/cap pattern). An invalid pattern (or a pattern past the size limit) returns
//! [`FindScanResult::invalid_regex`] so the panel shows a red `find-error` and clears highlights.
//!
//! ## Pure in-memory CPU, synchronous (KERNEL_BUILDER gate — MT-015 spinner lesson)
//!
//! `scan` is a pure tree walk with no backend, no tokio runtime, and no I/O; the widget calls it
//! synchronously while the panel is open. There is deliberately NO idle "Scanning…" spinner in the
//! shared `rich_editor_widget.rs` (an idle auto-repainting spinner re-broke the MT-015 tests); the
//! panel renders only when open (default closed), so the common path adds no repaint.

use regex::{Regex, RegexBuilder};

use crate::node::{BlockNode, Child, NodeKind};

/// The hard cap on the number of matches a single scan collects, mirroring the React
/// `find_replace.ts` cap (the React file uses 2000; the MT-018 contract pins the native cap at
/// 1000 to bound the highlight + replace work on a pathological document). When the cap is hit the
/// scan stops and [`FindScanResult::truncated`] is `true` (the panel shows `1000+ matches`).
pub const MAX_MATCHES: usize = 1000;

/// The compile-time size limit (bytes) for the compiled regex program + its DFA, the
/// finite-automaton memory guard that replaces the (unnecessary) backtracking timeout. A pattern
/// whose compiled program would exceed this is rejected as an invalid-regex error rather than
/// allocating unbounded memory. 1 MiB is comfortably larger than any realistic find pattern yet
/// small enough that a deliberately huge pattern (`a{1000000}`) fails fast.
pub const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// The kind of node a [`FindMatch`] was found in: ordinary prose (a paragraph/heading/quote text
/// run) or the text inside a `code_block`. Mirrors the React `find_decorations.ts` split of matches
/// into prose vs. code-block payloads so the highlight layer / a later code-aware consumer can treat
/// them differently. `block_index` is the index of the `code_block` among the document's top-level
/// blocks (for a code-aware reveal); it is informational here — Replace addresses the leaf by
/// `node_path` regardless of kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    /// A match inside ordinary prose text (paragraph/heading/quote/list/table run).
    Prose,
    /// A match inside a `code_block`'s text rope. `block_index` is the code block's top-level index.
    CodeBlock { block_index: usize },
}

/// One match: the text leaf it lives in (`node_path` = the child-index path to that [`crate::node::TextLeaf`]),
/// the `[char_start, char_end)` CHAR range WITHIN that leaf's rope, and the [`MatchKind`].
///
/// The range is per-leaf (NOT absolute) so a Replace step (`DeleteText { path: node_path, start:
/// char_start, end: char_end }`) addresses exactly the matched chars in the right rope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindMatch {
    /// Whether the match is in prose or a code block.
    pub kind: MatchKind,
    /// The child-index path from the doc root to the text leaf holding this match (its last element
    /// is the leaf's index within its parent block). This is the `path` a replace `Step` uses.
    pub node_path: Vec<usize>,
    /// The match start CHAR offset within the leaf's rope.
    pub char_start: usize,
    /// The match end CHAR offset (exclusive) within the leaf's rope.
    pub char_end: usize,
}

impl FindMatch {
    /// The number of chars this match spans (`char_end - char_start`).
    pub fn len_chars(&self) -> usize {
        self.char_end.saturating_sub(self.char_start)
    }
}

/// A find query: the literal-or-regex pattern plus the three VS Code option toggles. Mirrors the
/// React `FindQuery` (`term`/`caseSensitive`/`wholeWord`/`isRegex`); named `pattern` here to match
/// the MT-018 scope's type.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FindQuery {
    /// The search text. In literal mode it is matched verbatim (regex-escaped); in regex mode it is
    /// compiled as a `regex` pattern.
    pub pattern: String,
    /// Case-sensitive matching when `true` (the React `Aa` toggle).
    pub case_sensitive: bool,
    /// Whole-word matching when `true`: a match is kept only when the chars on each side of the
    /// range are NOT word chars (the React `W` toggle / `isWordBoundary`).
    pub whole_word: bool,
    /// Regex mode when `true`: `pattern` is a `regex` pattern (the React `.*` toggle). When `false`
    /// the pattern is matched literally (regex-escaped).
    pub is_regex: bool,
}

impl FindQuery {
    /// A literal (non-regex) query with all toggles off.
    pub fn literal(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            ..Self::default()
        }
    }

    /// True when the query has no pattern (an empty query matches nothing — `EMPTY_SCAN`).
    pub fn is_empty(&self) -> bool {
        self.pattern.is_empty()
    }
}

/// The result of a [`scan`]: the ordered matches, whether the scan was truncated at [`MAX_MATCHES`],
/// and an optional typed regex error. Mirrors the React `FindScanResult` (`matches`/`truncated`/
/// `error`). When `error` is `Some`, `matches` is empty (an invalid query highlights nothing) and
/// the panel renders the red `find-error` text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FindScanResult {
    /// The matches in document order (depth-first over the block tree).
    pub matches: Vec<FindMatch>,
    /// True when the scan stopped early at [`MAX_MATCHES`] (the panel shows `{MAX_MATCHES}+ matches`).
    pub truncated: bool,
    /// `Some(message)` when the regex failed to compile (or exceeded the size limit). `matches` is
    /// then empty. `None` for a valid (or empty) query.
    pub error: Option<String>,
}

impl FindScanResult {
    /// The empty scan (no matches, not truncated, no error) — the result for an empty query, mirroring
    /// the React `EMPTY_SCAN`.
    pub fn empty() -> Self {
        Self::default()
    }

    /// A typed invalid-regex result: no matches, no truncation, and the compile error message. The
    /// panel renders `message` as the red `find-error` text and clears all highlights (AC-6).
    pub fn invalid_regex(message: impl Into<String>) -> Self {
        Self {
            matches: Vec::new(),
            truncated: false,
            error: Some(message.into()),
        }
    }

    /// The number of matches found.
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// True when there are no matches.
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

/// Scan `doc` for every occurrence of `query`. Walks the block tree depth-first; for every text run
/// (prose) and every `code_block` text rope, collects the per-leaf char ranges where the pattern
/// matches. Capped at [`MAX_MATCHES`] (sets `truncated`). An empty query returns
/// [`FindScanResult::empty`]; an invalid regex returns [`FindScanResult::invalid_regex`].
///
/// This is a PURE function over the in-memory model — no backend, no async, no allocation per frame
/// beyond the result vec. The widget re-runs it on every document change while the panel is open
/// (mirroring the React "recompute on every document change"), synchronously.
pub fn scan(doc: &BlockNode, query: &FindQuery) -> FindScanResult {
    if query.is_empty() {
        return FindScanResult::empty();
    }
    let regex = match compile_query(query) {
        Ok(re) => re,
        Err(message) => return FindScanResult::invalid_regex(message),
    };

    let mut matches: Vec<FindMatch> = Vec::new();
    let mut truncated = false;
    // The top-level block index, threaded so a code_block match can carry its document position.
    let mut path = Vec::new();
    walk(
        doc,
        &mut path,
        &regex,
        query.whole_word,
        None,
        &mut matches,
        &mut truncated,
    );

    FindScanResult {
        matches,
        truncated,
        error: None,
    }
}

/// Compile a [`FindQuery`] to a `Regex`, or return the typed error message. In literal mode the
/// pattern is regex-escaped (so `a.b` matches the literal `a.b`, not `a<any>b`). Case-insensitivity
/// is the `(?i)` flag via the builder. The compiled-program size is bounded by [`REGEX_SIZE_LIMIT`]
/// (the finite-automaton memory guard that replaces the unnecessary backtracking timeout).
fn compile_query(query: &FindQuery) -> Result<Regex, String> {
    let source = if query.is_regex {
        query.pattern.clone()
    } else {
        regex::escape(&query.pattern)
    };
    RegexBuilder::new(&source)
        .case_insensitive(!query.case_sensitive)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| e.to_string())
}

/// Depth-first walk of the block tree. `path` is the running child-index path to `node`. For a
/// `code_block` node, `code_block_index` is set to its top-level block index so its matches carry
/// the document position. Appends matches into `out`; sets `truncated` when [`MAX_MATCHES`] is hit.
fn walk(
    node: &BlockNode,
    path: &mut Vec<usize>,
    regex: &Regex,
    whole_word: bool,
    code_block_index: Option<usize>,
    out: &mut Vec<FindMatch>,
    truncated: &mut bool,
) {
    for (i, child) in node.children.iter().enumerate() {
        if *truncated {
            return;
        }
        path.push(i);
        match child {
            Child::Text(leaf) => {
                let kind = match code_block_index {
                    Some(block_index) => MatchKind::CodeBlock { block_index },
                    None => MatchKind::Prose,
                };
                scan_leaf(
                    &leaf.text.to_string(),
                    path,
                    regex,
                    whole_word,
                    kind,
                    out,
                    truncated,
                );
            }
            Child::Block(block) => {
                // A code_block's text matches are tagged with the code-block kind so the highlight
                // layer / a later code-aware reveal can distinguish them. The block_index is the
                // TOP-LEVEL index (the position the document-order reveal uses); for a code_block
                // nested deeper we use its top-level ancestor index, which is `path[0]` here since
                // a code_block holds inline text directly (it never nests another block).
                let next_code_index = if matches!(block.kind, NodeKind::CodeBlock) {
                    Some(path.first().copied().unwrap_or(i))
                } else {
                    code_block_index
                };
                walk(
                    block,
                    path,
                    regex,
                    whole_word,
                    next_code_index,
                    out,
                    truncated,
                );
            }
            // Inline atoms (hsLink / loomTransclusion) carry no searchable rope text of their own
            // (their visible label is a render-time projection, not editable rope content), so they
            // are not scanned — a Replace could not target an atom's label as a rope range anyway.
            Child::HsLink(_) | Child::Transclusion(_) => {}
        }
        path.pop();
    }
}

/// Scan one leaf's `text` for every non-overlapping regex match, applying the whole-word boundary
/// post-filter, and append a [`FindMatch`] (with the leaf's `node_path` = `path`) for each kept
/// match. Stops + sets `truncated` when the global [`MAX_MATCHES`] cap is reached.
///
/// Char-vs-byte discipline (RISK-1 family): `regex` returns BYTE offsets into the `&str`; a
/// [`FindMatch`] must carry CHAR offsets into the rope (the unit `Step::DeleteText` uses). We convert
/// each byte offset to a char offset via `text[..byte].chars().count()`, so a multi-byte char
/// (CJK/emoji) never produces an off-by-one rope range.
fn scan_leaf(
    text: &str,
    path: &[usize],
    regex: &Regex,
    whole_word: bool,
    kind: MatchKind,
    out: &mut Vec<FindMatch>,
    truncated: &mut bool,
) {
    // Precompute char boundaries once so byte->char conversion is O(1) per match rather than
    // re-scanning the prefix each time (a leaf can hold many matches).
    let char_starts: Vec<usize> = text.char_indices().map(|(b, _)| b).collect();
    let byte_to_char = |byte: usize| -> usize {
        // The number of chars strictly before `byte` == the count of char-start byte offsets < byte.
        char_starts.partition_point(|&b| b < byte)
    };

    for m in regex.find_iter(text) {
        if out.len() >= MAX_MATCHES {
            *truncated = true;
            return;
        }
        // A zero-length match (e.g. `a*` on "") cannot be replaced and would not advance a visible
        // highlight; skip it (the React scanner also advances past zero-length matches).
        if m.start() == m.end() {
            continue;
        }
        if whole_word && !is_word_boundary(text, m.start(), m.end()) {
            continue;
        }
        out.push(FindMatch {
            kind,
            node_path: path.to_vec(),
            char_start: byte_to_char(m.start()),
            char_end: byte_to_char(m.end()),
        });
    }
}

/// True when the BYTE range `[start, end)` of `text` is a whole-word match: the char immediately
/// before `start` and the char immediately after `end` are NOT word chars (or are absent). Ports the
/// React `isWordBoundary`: a word char is a Unicode letter, number, or `_`.
fn is_word_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    let before_is_word = before.map(is_word_char).unwrap_or(false);
    let after_is_word = after.map(is_word_char).unwrap_or(false);
    !before_is_word && !after_is_word
}

/// True when `c` is a word char (Unicode letter, number, or `_`) — the React `WORD_CHAR` class
/// `[\p{L}\p{N}_]`.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{Child, NodeKind, TextLeaf};

    fn multi_para_doc() -> BlockNode {
        // doc > [ "foo bar foo", "a foo here", code_block("let foo = foo;") ]
        BlockNode::doc(vec![
            BlockNode::paragraph("foo bar foo"),
            BlockNode::paragraph("a foo here"),
            BlockNode::with_children(
                NodeKind::CodeBlock,
                vec![Child::Text(TextLeaf::new("let foo = foo;"))],
            ),
        ])
    }

    #[test]
    fn empty_query_is_empty_scan() {
        let doc = multi_para_doc();
        let result = scan(&doc, &FindQuery::default());
        assert!(result.is_empty());
        assert!(result.error.is_none());
        assert!(!result.truncated);
    }

    #[test]
    fn finds_all_occurrences_of_foo_across_paragraphs_and_code() {
        // AC-3 / scanner contract: scan() finds all 'foo' in a multi-paragraph doc (incl. the code
        // block). "foo bar foo" -> 2, "a foo here" -> 1, "let foo = foo;" -> 2 == 5.
        let doc = multi_para_doc();
        let result = scan(&doc, &FindQuery::literal("foo"));
        assert_eq!(
            result.len(),
            5,
            "five 'foo' occurrences across prose + code"
        );
        // The first two are in the first paragraph's leaf [0,0] at char 0 and char 8.
        assert_eq!(result.matches[0].node_path, vec![0, 0]);
        assert_eq!(
            (result.matches[0].char_start, result.matches[0].char_end),
            (0, 3)
        );
        assert_eq!(
            (result.matches[1].char_start, result.matches[1].char_end),
            (8, 11)
        );
        // The code-block matches carry the CodeBlock kind with the block index 2.
        let code_matches: Vec<_> = result
            .matches
            .iter()
            .filter(|m| matches!(m.kind, MatchKind::CodeBlock { .. }))
            .collect();
        assert_eq!(code_matches.len(), 2, "two 'foo' inside the code block");
        assert!(matches!(
            code_matches[0].kind,
            MatchKind::CodeBlock { block_index: 2 }
        ));
    }

    #[test]
    fn case_insensitive_matches_mixed_case() {
        // AC-4: case-insensitive (default) matches 'Foo', 'FOO', 'foo'.
        let doc = BlockNode::doc(vec![BlockNode::paragraph("Foo FOO foo fOo")]);
        let result = scan(&doc, &FindQuery::literal("foo"));
        assert_eq!(
            result.len(),
            4,
            "all four case variants match when case-insensitive"
        );
        // Case-SENSITIVE only matches the exact "foo".
        let sensitive = FindQuery {
            pattern: "foo".into(),
            case_sensitive: true,
            ..Default::default()
        };
        let result = scan(&doc, &sensitive);
        assert_eq!(
            result.len(),
            1,
            "case-sensitive matches only the exact 'foo'"
        );
    }

    #[test]
    fn whole_word_skips_foobar() {
        // AC-5: whole-word mode does not match 'foo' inside 'foobar' / 'unfoo'.
        let doc = BlockNode::doc(vec![BlockNode::paragraph("foo foobar unfoo foo!")]);
        let query = FindQuery {
            pattern: "foo".into(),
            whole_word: true,
            ..Default::default()
        };
        let result = scan(&doc, &query);
        // "foo" (word) yes; "foobar" no; "unfoo" no; "foo!" yes (`!` is not a word char).
        assert_eq!(result.len(), 2, "only the two standalone 'foo' words match");
        assert_eq!(
            (result.matches[0].char_start, result.matches[0].char_end),
            (0, 3)
        );
        // The second standalone "foo" is the one before "!" at char 17.
        assert_eq!(result.matches[1].char_start, 17);
    }

    #[test]
    fn regex_mode_matches_pattern() {
        // Regex mode: `f.o` matches "foo" and "fao".
        let doc = BlockNode::doc(vec![BlockNode::paragraph("foo fao fxxo")]);
        let query = FindQuery {
            pattern: "f.o".into(),
            is_regex: true,
            ..Default::default()
        };
        let result = scan(&doc, &query);
        assert_eq!(result.len(), 2, "f.o matches foo and fao, not fxxo");
    }

    #[test]
    fn literal_mode_escapes_regex_metachars() {
        // Literal mode treats `.` as a literal dot, not "any char".
        let doc = BlockNode::doc(vec![BlockNode::paragraph("a.b axb a.b")]);
        let result = scan(&doc, &FindQuery::literal("a.b"));
        assert_eq!(
            result.len(),
            2,
            "literal 'a.b' matches only the two real 'a.b', not 'axb'"
        );
    }

    #[test]
    fn invalid_regex_returns_typed_error() {
        // AC-6: an invalid regex returns a typed error message + zero matches.
        let doc = BlockNode::doc(vec![BlockNode::paragraph("foo")]);
        let query = FindQuery {
            pattern: "(unclosed".into(),
            is_regex: true,
            ..Default::default()
        };
        let result = scan(&doc, &query);
        assert!(
            result.error.is_some(),
            "an invalid regex sets the error message"
        );
        assert!(result.is_empty(), "an invalid regex clears all matches");
    }

    #[test]
    fn catastrophic_backtracking_pattern_does_not_hang() {
        // KERNEL_BUILDER gate (corrects RISK-003): the classic catastrophic-backtracking pattern
        // `(a+)+$` does NOT hang the Rust regex engine (it is finite-automaton, linear time). This
        // test would TIME OUT under a backtracking engine; it returns quickly here.
        let doc = BlockNode::doc(vec![BlockNode::paragraph(&"a".repeat(40))]);
        let query = FindQuery {
            pattern: "(a+)+$".into(),
            is_regex: true,
            ..Default::default()
        };
        let result = scan(&doc, &query);
        // It compiles and runs in linear time; the exact match count is not the point — the point
        // is it returns without hanging and without a timeout thread.
        assert!(result.error.is_none(), "the pattern compiles (no error)");
    }

    #[test]
    fn cap_truncates_at_max_matches() {
        // A document with > MAX_MATCHES occurrences truncates the scan and sets `truncated`.
        let text = "x".repeat(MAX_MATCHES + 50);
        let doc = BlockNode::doc(vec![BlockNode::paragraph(&text)]);
        let result = scan(&doc, &FindQuery::literal("x"));
        assert_eq!(result.len(), MAX_MATCHES, "the scan caps at MAX_MATCHES");
        assert!(result.truncated, "hitting the cap sets truncated");
    }

    #[test]
    fn char_ranges_are_char_not_byte_for_multibyte_text() {
        // RISK-1 family: a leaf with a multi-byte char before the match must yield CHAR offsets, not
        // byte offsets. "héllo foo" — 'é' is 2 bytes; "foo" starts at char 6 (byte 7).
        let doc = BlockNode::doc(vec![BlockNode::paragraph("héllo foo")]);
        let result = scan(&doc, &FindQuery::literal("foo"));
        assert_eq!(result.len(), 1);
        assert_eq!(
            (result.matches[0].char_start, result.matches[0].char_end),
            (6, 9),
            "the match range is in CHARS (6..9), not bytes (7..10)"
        );
    }

    #[test]
    fn matches_carry_the_leaf_node_path() {
        // The node_path must address the text LEAF (its last element is the leaf index), so a
        // Replace step can target it directly. The first paragraph's leaf is at [0,0].
        let doc = multi_para_doc();
        let result = scan(&doc, &FindQuery::literal("bar"));
        assert_eq!(result.len(), 1);
        assert_eq!(result.matches[0].node_path, vec![0, 0]);
        assert_eq!(result.matches[0].kind, MatchKind::Prose);
    }
}
