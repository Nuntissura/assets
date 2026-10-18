//! Right-to-left + bidirectional text handling for BOTH native editors (WP-KERNEL-012 MT-078, E13 i18n).
//!
//! This module is the SINGLE owner of the Unicode Bidirectional Algorithm (UAX#9) pass the rich-text and
//! code editors share, so the base-direction detection + logical→visual reordering logic lives in exactly
//! ONE place (the MT-078 steer: "REUSE the MT-077 text_intl/ module … do NOT create a parallel i18n
//! module"). It is a pure-logic module: no egui, no GPU, no backend, no `Color32`. The rope/model stays in
//! LOGICAL order everywhere; this module is consulted ONLY at render/caret time, so the DocJson backend
//! round-trip and the MT-011 model invariants are unaffected (RISK-3 / MC-2).
//!
//! ## The honest tiered scope (the load-bearing design — NO faking)
//!
//! egui's immediate-mode epaint layout does line-based LTR layout. It does NOT natively perform either
//! (a) the Unicode Bidirectional Algorithm (visual reordering of mixed LTR/RTL runs), nor (b) complex-script
//! SHAPING (Arabic cursive joining — letters change form by position; Indic reordering/conjuncts). Getting
//! shaping fully correct needs a real shaping engine (HarfBuzz / rustybuzz). This module delivers the
//! achievable tiers HONESTLY and surfaces a TYPED LIMITATION for the rest:
//!
//! - **TIER 1 (DELIVERED here):** [`base_direction`] detects the paragraph base direction from the first
//!   strong character (UAX#9 rule P2/P3 via [`unicode_bidi`]), and [`reorder_line`] reorders a logical-order
//!   line into VISUAL order via the UAX#9 algorithm so a mixed `abc ABC 123` line (UPPER = an RTL run)
//!   displays in correct visual order. Pure-Rust, exact, no shaping engine needed.
//! - **TIER 2 (DELIVERED — Hebrew):** Hebrew is NON-JOINING (its letters do not change form by position),
//!   so once Hebrew glyphs are in the font chain (MT-078 added Noto Sans Hebrew) and the line is reordered +
//!   right-aligned, Hebrew renders AND edits correctly end-to-end. Hebrew is therefore the honest RTL proof
//!   case (AC1 render, AC4 edit). The rope stays logical-order; the caret moves in logical order with the
//!   documented arrow semantics ([`RTL_CARET_ARROW_SEMANTICS`]).
//! - **TIER 3 (TYPED LIMITATION — Arabic cursive joining + Indic conjuncts):** egui does not execute the
//!   font's GSUB/GPOS tables, so Arabic renders in ISOLATED letter forms (disconnected) rather than joined
//!   cursive forms, and Indic conjuncts/reordering are not applied. The glyphs are PRESENT (not tofu — the
//!   font chain has them), but UNSHAPED. Rather than present this as "done", [`shaping_limitation`] returns
//!   a VISIBLE, documented capability boundary ([`ShapingLimitation`]) the editors surface as a note, plus a
//!   future-MT pointer ([`SHAPING_FOLLOW_ON_POINTER`]). NEVER silently-broken Arabic (RISK-1 / MC-1 / AC5).
//!
//! ## Why `unicode-bidi` (RESEARCH BASIS, verified 2026-06-26)
//!
//! `unicode-bidi` is the canonical pure-Rust UAX#9 implementation (the Servo/`idna`/`url` ecosystem crate).
//! It exposes `BidiInfo` (paragraph analysis: embedding levels + the paragraph base level) and
//! `Level::reorder_visual` (the L2 reorder of a level run into visual order). It is already in the locked
//! dependency graph transitively, so it is not a new dependency family — see Cargo.toml. Rejected: a
//! hand-rolled bidi pass (UAX#9 is subtle — neutral resolution, isolates, mirrored brackets — and getting it
//! wrong corrupts text), and storing visual-order text in the rope (breaks the model + backend round-trip).

use unicode_bidi::BidiInfo;

/// The base writing direction of a paragraph/line, resolved from its first strong directional character
/// (UAX#9 rules P2/P3). A line with no strong character (digits/punctuation/whitespace only, or empty) is
/// [`Direction::Ltr`] — the UAX#9 default base direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Left-to-right base direction (Latin, Cyrillic, CJK, digits-only, empty).
    Ltr,
    /// Right-to-left base direction (the first strong character is RTL — Hebrew, Arabic).
    Rtl,
}

impl Direction {
    /// True for [`Direction::Rtl`]. Convenience for renderers choosing right-alignment.
    pub fn is_rtl(self) -> bool {
        matches!(self, Direction::Rtl)
    }
}

/// The documented caret arrow-key semantics for RTL text (AC4 / RISK-5). The native editors keep the
/// caret in LOGICAL order (the rope is logical-order, direction-agnostic): a LEFT/RIGHT arrow moves the
/// caret to the previous/next LOGICAL grapheme boundary (delegating to `text_intl::grapheme`), exactly as
/// for LTR text. This is the "logical-order caret" convention (the same one used by terminals and by
/// many code editors for RTL): the model offset is what moves, and the renderer maps that logical offset
/// to its visual position via [`reorder_line`]. We deliberately do NOT implement visual-order caret motion
/// (where LEFT means "visually left", which flips meaning inside an RTL run) in this MT — that is a
/// follow-on once full shaping lands; documenting the chosen convention is the AC4 requirement.
pub const RTL_CARET_ARROW_SEMANTICS: &str =
    "Caret moves in LOGICAL order: ArrowRight advances to the next logical grapheme, ArrowLeft to the \
     previous logical grapheme, regardless of the run's visual direction. The rope stays logical-order; \
     the renderer maps the logical caret offset to its visual x via the bidi reorder.";

/// The future-MT pointer recorded in code + handoff for the Tier-3 complex-script shaping work egui cannot
/// do (Arabic cursive joining + Indic conjunct reordering need a HarfBuzz/rustybuzz shaping engine). Surfaced
/// alongside the visible [`ShapingLimitation`] note so the capability boundary is discoverable, not hidden.
pub const SHAPING_FOLLOW_ON_POINTER: &str =
    "WP-KERNEL-012 follow-on or WP-KERNEL-014: integrate a complex-script shaping engine \
     (rustybuzz/cosmic-text) for Arabic cursive joining + Indic conjunct reordering.";

/// One contiguous slice of the original logical string that maps to one visual run, carrying the byte range
/// in the ORIGINAL (logical-order) text and whether that run is reversed (RTL) on screen. Returned by
/// [`reorder_line`] so a caller can either (a) read [`ReorderedLine::visual_text`] directly (the visual-order
/// string ready to lay out LTR), or (b) walk the runs to keep its own logical↔visual mapping for caret math.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualRun {
    /// The byte range `[start, end)` of this run in the ORIGINAL logical-order text.
    pub logical_range: std::ops::Range<usize>,
    /// Whether this run is right-to-left (its characters are emitted reversed in the visual string).
    pub rtl: bool,
}

/// The result of reordering one logical-order line into visual order (UAX#9 L1/L2). `visual_text` is the
/// string in VISUAL order — appending it to an `egui` LayoutJob and laying it out LTR yields the correct
/// on-screen order for a mixed bidi line. `runs` carries the logical→visual run mapping for callers that
/// need it (caret positioning). For a pure-LTR line this is the IDENTITY (visual_text == input, one
/// non-rtl run) — proven by [`is_identity`] (AC6 / MC-3 / RISK-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReorderedLine {
    /// The line content in VISUAL order, ready to lay out LTR.
    pub visual_text: String,
    /// The base paragraph direction resolved for this line (UAX#9 P2/P3).
    pub base: Direction,
    /// The visual-order sequence of runs, each pointing back to its logical byte range.
    pub runs: Vec<VisualRun>,
}

impl ReorderedLine {
    /// True when this reorder is the IDENTITY: the visual text equals the original input and the line is a
    /// single LTR run. For any pure-LTR line (Latin/Cyrillic/CJK/digits) the bidi pass MUST be a no-op
    /// (AC6 / MC-3 / RISK-2) — `reorder_line(ltr)` returns an identity, so the existing LTR + CJK render
    /// path (MT-075/077) is byte-for-byte unchanged. `original` is the input that produced this result.
    pub fn is_identity(&self, original: &str) -> bool {
        self.base == Direction::Ltr
            && self.visual_text == original
            && self.runs.len() <= 1
            && self.runs.first().map(|r| !r.rtl).unwrap_or(true)
    }
}

/// Detect the base writing direction of `text` from its first strong directional character (UAX#9 P2/P3).
/// Hebrew/Arabic (and other RTL scripts) → [`Direction::Rtl`]; Latin/Cyrillic/CJK/digits-only/empty →
/// [`Direction::Ltr`]. This is the AC3 paragraph-direction detection: a Hebrew or Arabic paragraph reports
/// RTL so the renderer right-aligns it; everything else stays LTR (the unchanged default).
///
/// Implemented via [`BidiInfo::new`] with `None` (auto base level): UAX#9 P2/P3 set the paragraph's base
/// level to 1 (odd = RTL) when the first strong char is RTL, else 0 (even = LTR). We read that resolved
/// base level, so this is the standard algorithm, not a hand-rolled first-RTL-char scan.
pub fn base_direction(text: &str) -> Direction {
    let info = BidiInfo::new(text, None);
    // `BidiInfo::new` analyses one-or-more paragraphs; for a single line there is one paragraph. An empty
    // string yields one empty paragraph with the default LTR base level (0).
    match info.paragraphs.first() {
        Some(p) if p.level.is_rtl() => Direction::Rtl,
        _ => Direction::Ltr,
    }
}

/// Reorder one logical-order line into VISUAL order via the Unicode Bidirectional Algorithm (UAX#9 L1/L2).
///
/// The rope/model is ALWAYS logical-order (direction-agnostic); this is applied ONLY at render/caret time
/// (RISK-3 / MC-2). For a pure-LTR line the result is the IDENTITY (visual == input, one LTR run — AC6).
/// For an RTL or mixed line, RTL runs are emitted reversed and the run sequence is visually reordered, so
/// laying out [`ReorderedLine::visual_text`] LTR produces the correct on-screen order.
///
/// ## Algorithm (standard UAX#9, via `unicode-bidi`)
///
/// 1. [`BidiInfo::new`] resolves per-character embedding levels for the paragraph (auto base level).
/// 2. For the single line `0..text.len()`, `BidiInfo::visual_runs` returns the per-level run ranges already
///    REORDERED into visual order (UAX#9 L2), plus each run's level (odd = RTL).
/// 3. For each visual run we copy its logical bytes; an RTL run (odd level) is emitted with its GRAPHEME
///    CLUSTERS reversed (not raw bytes/chars — reversing raw chars would split a combining sequence; we
///    reverse by grapheme via `unicode-segmentation` so an accented Hebrew/combining cluster stays intact).
///
/// Note (Tier-3 honesty): this reorders correctly, but it does NOT SHAPE. Arabic letters in an RTL run are
/// reordered to the correct visual position yet remain in ISOLATED forms (egui will not join them); that is
/// the documented limitation surfaced by [`shaping_limitation`], not a bug in this reorder.
pub fn reorder_line(text: &str) -> ReorderedLine {
    let base = base_direction(text);
    if text.is_empty() {
        return ReorderedLine {
            visual_text: String::new(),
            base,
            runs: Vec::new(),
        };
    }

    let info = BidiInfo::new(text, None);
    // Single line spanning the whole text. `paragraphs[0]` exists because the text is non-empty.
    let Some(para) = info.paragraphs.first() else {
        // Degenerate (never expected for non-empty text): fall back to identity.
        return ReorderedLine {
            visual_text: text.to_string(),
            base,
            runs: vec![VisualRun {
                logical_range: 0..text.len(),
                rtl: false,
            }],
        };
    };
    let line = para.range.clone();
    let (levels, runs_logical) = info.visual_runs(para, line);

    let mut visual_text = String::with_capacity(text.len());
    let mut runs: Vec<VisualRun> = Vec::with_capacity(runs_logical.len());
    for run in runs_logical {
        // `run` is already in VISUAL order (visual_runs returns runs left-to-right on screen). The run's
        // level decides whether its content is reversed. `levels[run.start]` is the run's embedding level.
        let level = levels[run.start];
        let slice = &text[run.clone()];
        let rtl = level.is_rtl();
        if rtl {
            // Reverse by GRAPHEME CLUSTER so a base+combining mark (or any multi-scalar cluster) is not
            // torn — reversing raw chars would put a combining mark before its base.
            for g in graphemes_rev(slice) {
                visual_text.push_str(g);
            }
        } else {
            visual_text.push_str(slice);
        }
        runs.push(VisualRun {
            logical_range: run,
            rtl,
        });
    }

    ReorderedLine {
        visual_text,
        base,
        runs,
    }
}

/// Reverse the grapheme clusters of `s`, yielding them last-to-first. Used for emitting an RTL run in
/// visual order without splitting a cluster (UAX#9 L4 mirrors single chars; cluster integrity is preserved
/// here by reversing whole clusters). Pure helper over `unicode-segmentation`.
fn graphemes_rev(s: &str) -> impl Iterator<Item = &str> {
    use unicode_segmentation::UnicodeSegmentation;
    let mut v: Vec<&str> = s.graphemes(true).collect();
    v.reverse();
    v.into_iter()
}

/// The script class this module reasons about for the Tier-3 shaping limitation. Hebrew is non-joining
/// (fully handled — no limitation); Arabic is cursive-joining and Indic uses conjuncts/reordering (both
/// need a shaping engine egui lacks → a typed limitation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplexScript {
    /// No complex-script content needing shaping (Latin/Cyrillic/CJK/Hebrew/digits) — fully handled.
    None,
    /// Arabic (cursive joining — contextual isolated/initial/medial/final forms). egui does not join.
    Arabic,
    /// Indic / Devanagari (conjuncts + reordering). egui does not reorder/conjoin.
    Indic,
}

/// A VISIBLE, documented capability boundary for content egui cannot shape (Tier-3 / RISK-1 / MC-1 / AC5).
/// Returned by [`shaping_limitation`] when a string contains Arabic or Indic. The editors render this
/// [`note`](ShapingLimitation::note) next to the affected content so the user is told the text is present
/// but UNSHAPED (isolated forms), rather than seeing silently-broken Arabic presented as correct. Carries
/// the future-MT [`pointer`](ShapingLimitation::pointer) ([`SHAPING_FOLLOW_ON_POINTER`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShapingLimitation {
    /// Which complex script triggered the limitation (Arabic or Indic).
    pub script: ComplexScript,
    /// The short, human-readable note the editor displays (e.g. "Arabic cursive shaping limited …").
    pub note: String,
    /// The future-MT pointer recorded so the boundary is discoverable ([`SHAPING_FOLLOW_ON_POINTER`]).
    pub pointer: String,
}

/// Whether `text` contains Arabic cursive-joining content (Arabic block U+0600..=U+06FF, plus the Arabic
/// Supplement / Extended-A and Presentation Forms ranges). Cheap O(n) scan. True ⇒ egui will render
/// isolated (unjoined) forms ⇒ the typed limitation applies.
pub fn contains_arabic(text: &str) -> bool {
    text.chars().any(is_arabic_char)
}

/// Whether `text` contains Devanagari/Indic content (Devanagari block U+0900..=U+097F). Cheap O(n) scan.
/// True ⇒ egui will not apply conjuncts/reordering ⇒ the typed limitation applies.
pub fn contains_indic(text: &str) -> bool {
    text.chars().any(is_indic_char)
}

/// Whether `text` contains Hebrew (U+0590..=U+05FF). Hebrew is non-joining, so it is FULLY handled once in
/// the font chain — this is exposed so a renderer can confirm it does NOT raise a shaping limitation for
/// Hebrew (Hebrew is the honest RTL proof case, AC1/AC4).
pub fn contains_hebrew(text: &str) -> bool {
    text.chars().any(|c| ('\u{0590}'..='\u{05FF}').contains(&c))
}

fn is_arabic_char(c: char) -> bool {
    matches!(c,
        '\u{0600}'..='\u{06FF}'   // Arabic
        | '\u{0750}'..='\u{077F}' // Arabic Supplement
        | '\u{08A0}'..='\u{08FF}' // Arabic Extended-A
        | '\u{FB50}'..='\u{FDFF}' // Arabic Presentation Forms-A
        | '\u{FE70}'..='\u{FEFF}' // Arabic Presentation Forms-B
    )
}

fn is_indic_char(c: char) -> bool {
    ('\u{0900}'..='\u{097F}').contains(&c) // Devanagari
}

/// The typed Tier-3 limitation for `text`, or `None` when there is nothing egui cannot shape.
///
/// Returns `Some(ShapingLimitation)` when `text` contains Arabic or Indic content (which egui renders
/// UNSHAPED — isolated Arabic letter forms, un-reordered Indic conjuncts). Returns `None` for everything
/// egui handles correctly: Latin, Cyrillic, CJK, digits, and HEBREW (Hebrew is non-joining, so it is the
/// fully-handled honest RTL case and raises NO limitation). The editors call this to decide whether to show
/// the visible "shaping limited" note (AC5 / PROOF3 / MC-1 — never silently-broken Arabic). Arabic takes
/// precedence over Indic when both are present (the more common case to flag).
pub fn shaping_limitation(text: &str) -> Option<ShapingLimitation> {
    if contains_arabic(text) {
        Some(ShapingLimitation {
            script: ComplexScript::Arabic,
            note: "Arabic cursive shaping limited: glyphs are shown in isolated (unjoined) forms. Full \
                   cursive joining is pending a shaping-engine update."
                .to_string(),
            pointer: SHAPING_FOLLOW_ON_POINTER.to_string(),
        })
    } else if contains_indic(text) {
        Some(ShapingLimitation {
            script: ComplexScript::Indic,
            note: "Indic shaping limited: conjuncts and reordering are not applied (glyphs shown \
                   unshaped). Full shaping is pending a shaping-engine update."
                .to_string(),
            pointer: SHAPING_FOLLOW_ON_POINTER.to_string(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Hebrew "שלום עולם" (shalom olam = "hello world"), logical order. Non-joining RTL — the honest case.
    const HEBREW_HELLO: &str = "שלום עולם";
    // Arabic "العربية" (al-arabiyya = "Arabic"), logical order. Cursive-joining — the typed-limitation case.
    const ARABIC: &str = "العربية";
    // Devanagari "नमस्ते" (namaste). Indic conjunct — the typed-limitation case.
    const DEVANAGARI: &str = "नमस्ते";

    #[test]
    fn base_direction_first_strong() {
        // AC3: base direction is the first STRONG character's direction (UAX#9 P2/P3).
        assert_eq!(base_direction("hello"), Direction::Ltr, "Latin -> LTR");
        assert_eq!(base_direction("Привет"), Direction::Ltr, "Cyrillic -> LTR");
        assert_eq!(base_direction("中文"), Direction::Ltr, "CJK -> LTR");
        assert_eq!(
            base_direction(HEBREW_HELLO),
            Direction::Rtl,
            "Hebrew first-strong -> RTL"
        );
        assert_eq!(
            base_direction(ARABIC),
            Direction::Rtl,
            "Arabic first-strong -> RTL"
        );
        // A line starting with digits/punctuation (no strong char) is LTR by default; a digit then Hebrew
        // is still RTL only if the FIRST STRONG char is RTL — "123" has no strong char -> LTR.
        assert_eq!(
            base_direction("123"),
            Direction::Ltr,
            "digits-only -> LTR default"
        );
        assert_eq!(base_direction(""), Direction::Ltr, "empty -> LTR default");
        // Leading LTR word makes the whole paragraph LTR even if Hebrew follows (first strong = Latin).
        assert_eq!(
            base_direction("abc שלום"),
            Direction::Ltr,
            "first strong Latin -> LTR base"
        );
        // Leading Hebrew then Latin -> RTL base (first strong = Hebrew).
        assert_eq!(
            base_direction("שלום abc"),
            Direction::Rtl,
            "first strong Hebrew -> RTL base"
        );
    }

    #[test]
    fn ltr_reorder_is_identity_no_regression() {
        // AC6 / MC-3 / RISK-2: bidi MUST be a NO-OP (identity) for pure-LTR text so MT-075/077 LTR + CJK
        // rendering is byte-for-byte unchanged. Prove it for Latin, Cyrillic, CJK, digits, mixed-with-spaces.
        for s in [
            "hello world",
            "Привет мир",
            "这是中文段落",
            "abc 123 def",
            "",
            "a",
        ] {
            let r = reorder_line(s);
            assert!(
                r.is_identity(s),
                "LTR identity required for {s:?}: got base={:?} visual={:?} runs={:?}",
                r.base,
                r.visual_text,
                r.runs
            );
            // The visual text equals the input exactly (the strongest identity statement).
            assert_eq!(
                r.visual_text, s,
                "LTR reorder must not change the text for {s:?}"
            );
        }
    }

    #[test]
    fn hebrew_line_is_rtl_and_reordered() {
        // AC1/AC2: a Hebrew line is RTL base and its visual order is the REVERSE of logical order (Hebrew
        // is a single RTL run, so the whole line reverses for display).
        let r = reorder_line(HEBREW_HELLO);
        assert_eq!(r.base, Direction::Rtl, "Hebrew line base is RTL");
        assert!(
            !r.is_identity(HEBREW_HELLO),
            "an RTL line must NOT be identity"
        );
        // The visual string is the grapheme-reversed logical string (one RTL run).
        let expected_visual: String = {
            use unicode_segmentation::UnicodeSegmentation;
            HEBREW_HELLO.graphemes(true).rev().collect()
        };
        assert_eq!(
            r.visual_text, expected_visual,
            "a pure-Hebrew line reverses to visual order (logical {HEBREW_HELLO:?})"
        );
        // Reordering is reversible at the run level: the run still points at the whole logical range.
        assert_eq!(r.runs.len(), 1, "pure Hebrew is one RTL run");
        assert!(r.runs[0].rtl, "the Hebrew run is RTL");
        assert_eq!(r.runs[0].logical_range, 0..HEBREW_HELLO.len());
    }

    #[test]
    fn mixed_line_reorders_runs_to_visual_order() {
        // AC2: a mixed line "abc <HEBREW> 123" — the RTL Hebrew run is visually reordered relative to the
        // LTR Latin/number runs. The LOGICAL order is preserved (we never mutate the input); only the
        // VISUAL string changes. Concretely, the Hebrew run is reversed and placed per UAX#9 L2.
        let logical = format!("abc {HEBREW_HELLO} 123");
        let r = reorder_line(&logical);
        // The base is LTR (first strong char is Latin 'a').
        assert_eq!(r.base, Direction::Ltr, "first strong is Latin -> LTR base");
        // It is NOT identity: a mixed line with an RTL run must reorder.
        assert!(
            !r.is_identity(&logical),
            "a mixed bidi line must reorder (not identity)"
        );
        // The visual text must contain the Hebrew run REVERSED (visual order) — assert the reversed Hebrew
        // grapheme sequence appears, and the original logical Hebrew substring does NOT appear verbatim
        // (because it was reversed for display).
        let hebrew_visual: String = {
            use unicode_segmentation::UnicodeSegmentation;
            HEBREW_HELLO.graphemes(true).rev().collect()
        };
        assert!(
            r.visual_text.contains(&hebrew_visual),
            "visual text must carry the Hebrew run in reversed (visual) order: {:?}",
            r.visual_text
        );
        // Every run maps back to a real byte range in the ORIGINAL logical text (logical order preserved).
        for run in &r.runs {
            assert!(run.logical_range.end <= logical.len());
            assert!(logical.is_char_boundary(run.logical_range.start));
            assert!(logical.is_char_boundary(run.logical_range.end));
        }
        // At least one RTL run (the Hebrew) and at least one LTR run (the Latin/digits).
        assert!(r.runs.iter().any(|x| x.rtl), "must have an RTL run");
        assert!(r.runs.iter().any(|x| !x.rtl), "must have an LTR run");
    }

    #[test]
    fn reorder_preserves_all_logical_content() {
        // Integrity: the set of grapheme clusters in the visual text equals the set in the logical text
        // (reorder rearranges; it never drops or duplicates content). Checked for a mixed line.
        use unicode_segmentation::UnicodeSegmentation;
        let logical = format!("abc {HEBREW_HELLO} 123");
        let r = reorder_line(&logical);
        let mut logical_gr: Vec<&str> = logical.graphemes(true).collect();
        let mut visual_gr: Vec<&str> = r.visual_text.graphemes(true).collect();
        logical_gr.sort_unstable();
        visual_gr.sort_unstable();
        assert_eq!(
            logical_gr, visual_gr,
            "reorder must preserve the multiset of grapheme clusters"
        );
    }

    #[test]
    fn arabic_raises_typed_limitation_not_silent() {
        // AC5 / PROOF3 / MC-1 / RISK-1: Arabic content MUST raise a VISIBLE typed limitation (never silently
        // broken). The glyphs are present (font chain has them) but unshaped (isolated forms).
        let lim = shaping_limitation(ARABIC).expect("Arabic must raise a shaping limitation");
        assert_eq!(lim.script, ComplexScript::Arabic);
        assert!(
            lim.note.to_lowercase().contains("arabic"),
            "note names Arabic: {:?}",
            lim.note
        );
        assert!(
            lim.note.to_lowercase().contains("limit"),
            "note says 'limited': {:?}",
            lim.note
        );
        assert!(
            !lim.pointer.is_empty(),
            "a future-MT pointer must be recorded"
        );
        assert_eq!(lim.pointer, SHAPING_FOLLOW_ON_POINTER);
        assert!(contains_arabic(ARABIC));
    }

    #[test]
    fn indic_raises_typed_limitation() {
        // AC5: Indic (Devanagari) also raises the typed limitation (conjuncts/reordering unshaped).
        let lim =
            shaping_limitation(DEVANAGARI).expect("Devanagari must raise a shaping limitation");
        assert_eq!(lim.script, ComplexScript::Indic);
        assert!(
            lim.note.to_lowercase().contains("indic"),
            "note names Indic: {:?}",
            lim.note
        );
        assert!(contains_indic(DEVANAGARI));
    }

    #[test]
    fn hebrew_and_ltr_raise_no_limitation() {
        // Hebrew is NON-JOINING (the honest RTL case) — it must NOT raise a shaping limitation. Latin/CJK
        // must not either. Only Arabic/Indic do. This is what makes Hebrew the honest end-to-end RTL proof.
        assert!(
            shaping_limitation(HEBREW_HELLO).is_none(),
            "Hebrew is fully handled — no limitation"
        );
        assert!(
            shaping_limitation("hello world").is_none(),
            "Latin — no limitation"
        );
        assert!(
            shaping_limitation("中文 段落").is_none(),
            "CJK — no limitation"
        );
        assert!(contains_hebrew(HEBREW_HELLO));
        assert!(!contains_arabic(HEBREW_HELLO), "Hebrew is not Arabic");
    }

    #[test]
    fn empty_line_reorder_is_safe() {
        // Defensive: empty input never panics and is identity.
        let r = reorder_line("");
        assert!(r.visual_text.is_empty());
        assert!(r.runs.is_empty());
        assert_eq!(r.base, Direction::Ltr);
        assert!(r.is_identity(""));
    }

    #[test]
    fn caret_semantics_are_documented() {
        // AC4 (documentation half): the chosen RTL caret arrow semantics are a non-empty, explicit
        // contract string (logical-order motion). The behavioral half is proven in the rich-editor test.
        assert!(RTL_CARET_ARROW_SEMANTICS.contains("LOGICAL"));
        assert!(RTL_CARET_ARROW_SEMANTICS.contains("ArrowLeft"));
        assert!(RTL_CARET_ARROW_SEMANTICS.contains("ArrowRight"));
    }
}
