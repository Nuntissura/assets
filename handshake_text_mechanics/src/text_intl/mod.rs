//! Shared Unicode-correct text segmentation for BOTH native editors (WP-KERNEL-012 MT-077, E13 i18n).
//!
//! This module is the SINGLE owner of the international text-mechanics primitives the rich-text and
//! code editors share, so the grapheme / line-break / word+char-count logic lives in exactly ONE place
//! and is never duplicated per editor (the MT-077 KEY STEER #2: "shared text_intl/ module reused by
//! BOTH editors, NOT duplicated"). It is a pure-logic module: no egui, no GPU, no backend, no
//! `Color32`. The rope stays in LOGICAL order; the bidi pass ([`bidi`], MT-078) reorders ONLY at
//! render/caret time and never mutates the model, so the backend round-trip is unaffected.
//!
//! ## The three corrections this module makes
//!
//! 1. **Grapheme-cluster caret movement** ([`grapheme`]). Before MT-077, caret RIGHT/LEFT and Backspace
//!    moved by a single Unicode SCALAR (`char`): a family ZWJ emoji (man+woman+girl, ~7 codepoints), a
//!    combining accent (`e` + U+0301), a flag (two regional indicators), or a Hangul syllable could be
//!    torn in half — the caret would land INSIDE a user-perceived character and a Backspace would delete
//!    only one of its codepoints. [`grapheme::next_grapheme_boundary`] /
//!    [`grapheme::prev_grapheme_boundary`] move by the WHOLE extended grapheme cluster (UAX#29) so one
//!    keypress crosses the entire cluster. The segmentation is LOCAL to the caret neighbourhood (a
//!    bounded window), never the whole document, so a huge line keeps O(1)-ish keypress cost (RISK-1).
//!
//! 2. **CJK line-breaking** ([`linebreak`]). CJK has no spaces, so naive whitespace wrapping either
//!    overflows or never wraps a long ideograph run. egui's `Galley` ALREADY wraps Han + Kana natively
//!    (verified — see [`linebreak`] module docs), so this module's job is the UAX#14 break-opportunity
//!    table that SUPPLEMENTS egui where it falls short: Korean Hangul (egui has an explicit
//!    `TODO: Add support for Korean Hangul`) and the "no break AFTER an opening bracket" kinsoku rule
//!    egui does not encode. [`linebreak::break_opportunities`] returns the UAX#14 opportunities;
//!    [`linebreak::is_break_before`] answers the kinsoku question the renderer asks.
//!
//! 3. **Unicode-correct counts** ([`counts`]). The MT-019 footer split on whitespace, which is
//!    meaningless for CJK (a Chinese sentence has no spaces but many words). [`counts::word_count`] uses
//!    UAX#29 word boundaries (`unicode_words`), which counts CJK per-ideograph/per-run sensibly, and
//!    [`counts::char_count`] counts GRAPHEME CLUSTERS (the family emoji = 1 character, not 7 codepoints).
//!
//! ## Field-standard crates (RESEARCH BASIS, versions verified 2026-06-26)
//!
//! - `unicode-segmentation` 1.13 (UAX#29 graphemes + words) — already in the locked graph transitively;
//!   declared as a direct dep by MT-077. Mature, pure-Rust, the canonical Rust segmentation crate.
//! - `unicode-linebreak` 0.1.5 (UAX#14 line-break opportunities incl. CJK + kinsoku classes) — the ONE
//!   new dependency family MT-077 adds. ~159 LOC + a generated Unicode table; Apache-2.0; pure-Rust;
//!   this is the same crate egui's own ecosystem uses for UAX#14. Reject hand-rolled char-class tables
//!   (incomplete/incorrect for the long tail — the MT impl note).

pub mod bidi;
pub mod counts;
pub mod grapheme;
pub mod linebreak;

pub use bidi::{
    base_direction, reorder_line, shaping_limitation, ComplexScript, Direction, ReorderedLine,
    ShapingLimitation, VisualRun, RTL_CARET_ARROW_SEMANTICS, SHAPING_FOLLOW_ON_POINTER,
};
pub use counts::{char_count, word_count};
pub use grapheme::{
    next_grapheme_boundary, prev_grapheme_boundary, GraphemeCursor, GRAPHEME_LOCAL_WINDOW_BYTES,
};
pub use linebreak::{break_opportunities, is_break_before, BreakClass, BreakOpportunity};
