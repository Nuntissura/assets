//! Lane SMB-LAYOUT-SENTINELS-1 (R52 F-R52-1): layout resolves the STU-TYP-207 stream sentinels
//! (Master Spec v02.229, the v02.228 SD-29 assignment) at composition.
//!
//! Hand-computed oracles. Toy shaper: one glyph per character of the composition text, every glyph
//! advances 6 pt, line height 12 pt, ascent 10 pt, fixed first baseline 10 pt, no insets; a 12 pt
//! high frame holds exactly one line. Like Type, the toy shaper refuses a marker or flush sentinel
//! (here `missing_font`), so every passing composition proves layout never handed one to shaping.
//! The sentinel code points and the paragraph terminators below are typed in from the spec table
//! and from UAX #9 (bidi class B: U+000A, U+000D, U+001C..U+001E, U+0085, U+2029), not read from
//! the crates. Vendor parity UNVERIFIED (D5).
mod support_layout;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use hsk_studio_observe::{Budget, DeliveryClass, DeliveryError, FailureCode, Observe, Outcome, SinkPort};
use support_layout::*;

const CUR: char = '\u{FDD0}';
const NEXT: char = '\u{FDD1}';
const PREV: char = '\u{FDD2}';
const SECTION: char = '\u{FDD3}';
const FOOTNOTE: char = '\u{FDD4}';
const ENDNOTE: char = '\u{FDD5}';
const XREF: char = '\u{FDD6}';
const INDEX: char = '\u{FDD7}';
const TERMINATORS: [char; 12] = [
    '\u{000A}', '\u{000D}', '\u{001C}', '\u{001D}', '\u{001E}', '\u{0085}', '\u{2029}', '\u{FDD8}', '\u{FDD9}',
    '\u{FDDA}', '\u{FDDB}', '\u{FDDC}',
];

fn is_terminator(c: char) -> bool {
    TERMINATORS.contains(&c)
}

/// Toy shaper (module docs): glyphs for the paragraph without its terminator, source = the whole
/// requested span (Type's convention).
fn toy(input: &ShapeInput<'_>) -> Result<ShapedParagraph, LayoutError> {
    let (s, e) = (input.paragraph.start as usize, input.paragraph.end as usize);
    let slice = input.text.get(s..e).ok_or(LayoutError::InvalidInput)?;
    if slice.chars().any(|c| matches!(c, '\u{FDD0}'..='\u{FDD7}' | '\u{FDDD}'..='\u{FDEF}')) {
        return Err(LayoutError::MissingFont);
    }
    let content = slice.strip_suffix(is_terminator).unwrap_or(slice);
    let mut p = ShapedParagraph::from_fixed_advance(
        input.text,
        SourceSpan { start: s as u64, end: (s + content.len()) as u64 },
        6.0,
        12.0,
        10.0,
    )
    .ok_or(LayoutError::InvalidInput)?;
    p.source = input.paragraph;
    Ok(p)
}

/// `n` pages of 200 x 200 pt, two per spread, keys page-1..page-n.
fn pages_doc(n: usize, layers: Vec<StudioLayer>) -> StudioDocument {
    let mut d = document(layers, [PageSide::LeftHand, PageSide::RightHand]);
    let template = d.page_spreads[0].clone();
    d.page_spreads = (0..n.div_ceil(2))
        .map(|s| StudioPageSpread {
            page_spread_id: id("SPGS", s as u32 + 1),
            name: format!("spread {}", s + 1),
            pages: (2 * s..(2 * s + 2).min(n))
                .map(|i| page(&format!("page-{}", i + 1), if i % 2 == 0 { PageSide::LeftHand } else { PageSide::RightHand }))
                .collect(),
            ..template.clone()
        })
        .collect();
    d
}
/// Frames 1..=n of story 1, threaded in order; frame k sits on page `on[k]` (0-based).
fn chain(on: &[usize], h: f64, columns: u32) -> Vec<StudioLayer> {
    let n = on.len() as u32;
    on.iter()
        .enumerate()
        .map(|(i, &pg)| {
            let k = i as u32 + 1;
            let s = Spec { page: pg, x: 10.0, y: 10.0, w: 180.0, h, columns, gutter: 0.0 };
            frame_layer(k, 1, s, (k > 1).then(|| k - 1), (k < n).then_some(k + 1))
        })
        .collect()
}
fn story_of(text: &str) -> StudioTextStory {
    let spans: Vec<(u64, u64)> = {
        let mut out = Vec::new();
        let mut start = 0usize;
        for (i, c) in text.char_indices() {
            if is_terminator(c) {
                out.push((start as u64, (i + c.len_utf8()) as u64));
                start = i + c.len_utf8();
            }
        }
        if start < text.len() || out.is_empty() {
            out.push((start as u64, text.len() as u64));
        }
        out
    };
    story(1, text, &spans).0
}
#[allow(clippy::too_many_arguments)]
fn section(key: &str, start: &str, number_start: u32, style: StudioNumberStyle, prefix: Option<&str>, marker: Option<&str>) -> StudioSection {
    StudioSection {
        section_id: key.into(),
        name: format!("section {key}"),
        page_start: start.into(),
        continue_numbering: false,
        page_number_start: Some(number_start),
        page_number_style: style,
        include_section_prefix: prefix.is_some(),
        section_prefix: prefix.map(Into::into),
        marker: marker.map(Into::into),
    }
}
/// Section "Intro" (arabic from 1) on page-1, section "Body" (upper roman from 1, prefix "B-") on page-3.
fn two_sections() -> Vec<StudioSection> {
    vec![
        section("s1", "page-1", 1, StudioNumberStyle::Arabic, None, Some("Intro")),
        section("s2", "page-3", 1, StudioNumberStyle::UpperRoman, Some("B-"), Some("Body")),
    ]
}
fn request<'a>(
    doc: &'a StudioDocument,
    st: &'a StudioTextStory,
    sections: &'a [StudioSection],
    markers: &'a [MarkerRecord],
    budget: SentinelBudget,
) -> SentinelRequest<'a> {
    SentinelRequest {
        document: doc,
        story: st,
        resolved_text: None,
        obstacles: &[],
        composer: Composer::FirstFit,
        space: SpaceParams::default(),
        expected_revision: 2,
        limits: Limits::default(),
        sections,
        markers,
        budget,
    }
}
fn compose(req: &SentinelRequest<'_>, opts: &TypesetOptions<'_>) -> Result<SentinelComposition, SentinelError> {
    compose_with_sentinels(req, opts, &records(), &mut toy, &CancellationToken::default())
}
fn resolved(s: &str) -> MarkerState {
    MarkerState::Resolved(s.into())
}
fn unresolved(u: UnresolvedMarker) -> MarkerState {
    MarkerState::Unresolved(u)
}
fn outcome(story_byte: u64, kind: MarkerKind, page: Option<&str>, state: MarkerState) -> MarkerOutcome {
    MarkerOutcome { story_byte, kind, page_id: page.map(Into::into), state }
}
/// Paragraph k (17 bytes with its newline): "A" CUR "|" NEXT "|" PREV "|" SECTION; marker bytes
/// 17k+1, +5, +9, +13.
fn four_marker_paragraphs(n: usize) -> String {
    let p = format!("A{CUR}|{NEXT}|{PREV}|{SECTION}");
    assert_eq!(p.len(), 16);
    vec![p; n].join("\n")
}
fn page_markers(k: u64, page: Option<&str>, states: [MarkerState; 4]) -> Vec<MarkerOutcome> {
    let kinds = [MarkerKind::CurrentPageNumber, MarkerKind::NextPageNumber, MarkerKind::PreviousPageNumber, MarkerKind::SectionMarker];
    let [a, b, c, d] = states;
    kinds.into_iter().zip([1, 5, 9, 13]).zip([a, b, c, d]).map(|((kind, off), s)| outcome(17 * k + off, kind, page, s)).collect()
}

#[test]
fn mt_31005_marker_kinds_follow_the_spec_assignment() {
    let table = [
        (MarkerKind::CurrentPageNumber, CUR),
        (MarkerKind::NextPageNumber, NEXT),
        (MarkerKind::PreviousPageNumber, PREV),
        (MarkerKind::SectionMarker, SECTION),
        (MarkerKind::FootnoteReference, FOOTNOTE),
        (MarkerKind::EndnoteReference, ENDNOTE),
        (MarkerKind::CrossReference, XREF),
        (MarkerKind::Index, INDEX),
    ];
    assert_eq!(MarkerKind::ALL.len(), table.len());
    for (kind, c) in table {
        assert_eq!(kind.sentinel(), Some(c), "{kind:?}");
        assert_eq!(MarkerKind::from_sentinel(c), Some(kind));
    }
    // Breaks, flush space, U+FFFC (anchored-object marker) and reserved code points are no markers.
    for c in ['\u{FDD8}', '\u{FDDC}', '\u{FDDD}', '\u{FFFC}', '\u{FDDE}', '\u{FDEF}', 'a'] {
        assert_eq!(MarkerKind::from_sentinel(c), None, "U+{:04X}", c as u32);
    }
    assert_eq!(FLUSH_SPACE, '\u{FDDD}');
}

#[test]
fn mt_31005_paragraph_partition_ends_after_every_terminator_as_type_does() {
    // Every UAX #9 class-B character and every break sentinel ends its paragraph, terminator included.
    for c in TERMINATORS {
        assert!(ends_paragraph(c), "U+{:04X}", c as u32);
        let text = format!("ab{c}cd");
        let cut = (2 + c.len_utf8()) as u64;
        assert_eq!(
            paragraph_partition(&text),
            vec![SourceSpan { start: 0, end: cut }, SourceSpan { start: cut, end: text.len() as u64 }],
            "U+{:04X}",
            c as u32
        );
    }
    // Not terminators: forced line break, markers, flush space, U+FFFC, tab, NEL look-alikes.
    for c in ['\u{2028}', CUR, INDEX, '\u{FDDD}', '\u{FFFC}', '\t', '\u{000B}', '\u{000C}', '\u{001F}', ' '] {
        assert!(!ends_paragraph(c), "U+{:04X}", c as u32);
    }
    // A trailing terminator adds no empty paragraph; consecutive terminators make empty ones.
    let text = "a\u{FDDA}\u{FDD8}b\u{2029}";
    assert_eq!(
        paragraph_partition(text),
        vec![SourceSpan { start: 0, end: 4 }, SourceSpan { start: 4, end: 7 }, SourceSpan { start: 7, end: 11 }]
    );
    assert!(paragraph_partition("").is_empty());
    assert_eq!(paragraph_partition("x"), vec![SourceSpan { start: 0, end: 1 }]);
}

#[test]
fn mt_31007_page_and_section_markers_resolve_where_their_line_lands() {
    let text = four_marker_paragraphs(4);
    let st = story_of(&text);
    let doc = pages_doc(4, chain(&[0, 1, 2, 3], 12.0, 1));
    let sections = two_sections();
    let out = compose(&request(&doc, &st, &sections, &[], SentinelBudget::default()), &TypesetOptions::default()).unwrap();
    use UnresolvedMarker as U;
    let mut expected = Vec::new();
    expected.extend(page_markers(0, Some("page-1"), [resolved("1"), resolved("2"), unresolved(U::NoPreviousPage), resolved("Intro")]));
    expected.extend(page_markers(1, Some("page-2"), [resolved("2"), resolved("B-I"), resolved("1"), resolved("Intro")]));
    expected.extend(page_markers(2, Some("page-3"), [resolved("B-I"), resolved("B-II"), resolved("2"), resolved("Body")]));
    expected.extend(page_markers(3, Some("page-4"), [resolved("B-II"), unresolved(U::NoNextPage), resolved("B-I"), resolved("Body")]));
    assert_eq!(out.markers, expected);
    assert_eq!(out.passes, 2, "pass 1 unresolved, pass 2 confirms the resolved values");
    assert_eq!(out.layout.overset, None);
    // The lines are shaped with the resolved text: glyph counts
    // A 1 | 2 | _ | Intro = 11, A 2 | B-I | 1 | Intro = 14, A B-I | B-II | 2 | Body = 16,
    // A B-II | _ | B-I | Body = 15 glyphs of 6 pt.
    let widths: Vec<f64> = out.layout.frames.iter().map(|f| f.lines[0].width_pt).collect();
    assert_eq!(widths, [66.0, 84.0, 96.0, 90.0]);
    // Source spans stay story bytes: each line runs from "A" to the end of its section marker.
    let spans: Vec<SourceSpan> = out.layout.frames.iter().map(|f| f.lines[0].source).collect();
    assert_eq!(spans, (0..4u64).map(|k| SourceSpan { start: 17 * k, end: 17 * k + 16 }).collect::<Vec<_>>());
    // The glyphs of a resolved marker carry the sentinel's 3-byte cluster: "Intro" on page-1.
    let p0 = &out.paragraphs[0];
    let intro: Vec<SourceSpan> = p0.glyphs[6..11].iter().map(|g| g.cluster).collect();
    assert_eq!(intro, vec![SourceSpan { start: 13, end: 16 }; 5]);
    assert_eq!(p0.glyphs[1].cluster, SourceSpan { start: 1, end: 4 });
    assert_eq!(p0.source, SourceSpan { start: 0, end: 17 });
}

#[test]
fn mt_31007_recomposition_after_a_page_move_updates_the_resolved_text() {
    let text = four_marker_paragraphs(2);
    let st = story_of(&text);
    let sections = two_sections();
    let before_doc = pages_doc(4, chain(&[0, 1], 12.0, 1));
    let before = compose(&request(&before_doc, &st, &sections, &[], SentinelBudget::default()), &TypesetOptions::default()).unwrap();
    use UnresolvedMarker as U;
    let mut expected = page_markers(0, Some("page-1"), [resolved("1"), resolved("2"), unresolved(U::NoPreviousPage), resolved("Intro")]);
    expected.extend(page_markers(1, Some("page-2"), [resolved("2"), resolved("B-I"), resolved("1"), resolved("Intro")]));
    assert_eq!(before.markers, expected);
    // The thread's frames move to page-3 and page-4: the same story re-composes with the new pages.
    let moved_doc = pages_doc(4, chain(&[2, 3], 12.0, 1));
    let moved = compose(&request(&moved_doc, &st, &sections, &[], SentinelBudget::default()), &TypesetOptions::default()).unwrap();
    let mut expected = page_markers(0, Some("page-3"), [resolved("B-I"), resolved("B-II"), resolved("2"), resolved("Body")]);
    expected.extend(page_markers(1, Some("page-4"), [resolved("B-II"), unresolved(U::NoNextPage), resolved("B-I"), resolved("Body")]));
    assert_eq!(moved.markers, expected);
    let widths: Vec<f64> = moved.layout.frames.iter().map(|f| f.lines[0].width_pt).collect();
    assert_eq!(widths, [96.0, 90.0]);
    // Without sections the whole document is one implicit arabic section: no section marker.
    let plain = compose(&request(&moved_doc, &st, &[], &[], SentinelBudget::default()), &TypesetOptions::default()).unwrap();
    let mut expected = page_markers(0, Some("page-3"), [resolved("3"), resolved("4"), resolved("2"), unresolved(U::NoSection)]);
    expected.extend(page_markers(1, Some("page-4"), [resolved("4"), unresolved(U::NoNextPage), resolved("3"), unresolved(U::NoSection)]));
    assert_eq!(plain.markers, expected);
}

#[test]
fn mt_31007_a_marker_in_overset_text_is_unresolved_and_the_story_bytes_never_change() {
    let text = four_marker_paragraphs(3);
    let st = story_of(&text);
    let stored_before = text_of(&st).as_bytes().to_vec();
    // Two one-line frames for three paragraphs: the third is overset.
    let doc = pages_doc(4, chain(&[0, 1], 12.0, 1));
    let mut sections = two_sections();
    sections[1].marker = None;
    sections[1].page_start = "page-2".into();
    let out = compose(&request(&doc, &st, &sections, &[], SentinelBudget::default()), &TypesetOptions::default()).unwrap();
    use UnresolvedMarker as U;
    let mut expected = page_markers(0, Some("page-1"), [resolved("1"), resolved("B-I"), unresolved(U::NoPreviousPage), resolved("Intro")]);
    expected.extend(page_markers(1, Some("page-2"), [resolved("B-I"), resolved("B-II"), resolved("1"), unresolved(U::NoSectionMarker)]));
    expected.extend(page_markers(2, None, std::array::from_fn(|_| unresolved(U::NotPlaced))));
    assert_eq!(out.markers, expected);
    assert_eq!(out.layout.overset, Some(SourceSpan { start: 34, end: 50 }));
    // The stored story keeps one sentinel per marker; composition never rewrites it.
    assert_eq!(text_of(&st).as_bytes(), stored_before.as_slice());
    assert_eq!(text_of(&st).chars().filter(|c| ('\u{FDD0}'..='\u{FDD3}').contains(c)).count(), 12);
    assert_eq!(&text_of(&st)[1..4], CUR.to_string());
}

#[test]
fn mt_31007_note_and_cross_reference_markers_take_their_record_and_index_is_unresolved() {
    // a FOOTNOTE b ENDNOTE c XREF d INDEX e: markers at bytes 1, 5, 9, 13.
    let text = format!("a{FOOTNOTE}b{ENDNOTE}c{XREF}d{INDEX}e");
    let st = story_of(&text);
    let doc = pages_doc(2, chain(&[0], 12.0, 1));
    let gen_ = |at, kind, rid: &str, text: &str| MarkerRecord {
        story_byte: at,
        kind,
        target: MarkerTarget::Generated { record_id: rid.into(), text: text.into() },
    };
    let marks = [gen_(1, MarkerKind::FootnoteReference, "fn-1", "7"), gen_(9, MarkerKind::CrossReference, "xr-1", "see p. 3")];
    let out = compose(&request(&doc, &st, &[], &marks, SentinelBudget::default()), &TypesetOptions::default()).unwrap();
    use UnresolvedMarker as U;
    assert_eq!(
        out.markers,
        vec![
            outcome(1, MarkerKind::FootnoteReference, Some("page-1"), resolved("7")),
            outcome(5, MarkerKind::EndnoteReference, Some("page-1"), unresolved(U::RecordMissing)),
            outcome(9, MarkerKind::CrossReference, Some("page-1"), resolved("see p. 3")),
            outcome(13, MarkerKind::Index, Some("page-1"), unresolved(U::RuleUnstated)),
        ]
    );
    // a 7 b c "see p. 3" d e = 14 glyphs; the resolved spaces stay inside the sentinel's cluster.
    assert_eq!(out.layout.frames[0].lines[0].width_pt, 84.0);
    // Records are checked: a byte without a marker, a kind mismatch, a target the kind does not take,
    // a duplicate.
    let bad: [Vec<MarkerRecord>; 4] = [
        vec![gen_(0, MarkerKind::FootnoteReference, "fn-1", "7")],
        vec![gen_(5, MarkerKind::FootnoteReference, "fn-1", "7")],
        vec![MarkerRecord { story_byte: 13, kind: MarkerKind::Index, target: MarkerTarget::Generated { record_id: "ix".into(), text: "x".into() } }],
        vec![marks[0].clone(), marks[0].clone()],
    ];
    for (i, b) in bad.iter().enumerate() {
        let r = compose(&request(&doc, &st, &[], b, SentinelBudget::default()), &TypesetOptions::default());
        assert_eq!(r, Err(SentinelError::MarkerRecordInvalid), "case {i}");
    }
    // A page-number marker may carry a Placement record; it resolves the same way.
    let cur = format!("x{CUR}");
    let st2 = story_of(&cur);
    let rec = [MarkerRecord { story_byte: 1, kind: MarkerKind::CurrentPageNumber, target: MarkerTarget::Placement }];
    let out = compose(&request(&doc, &st2, &[], &rec, SentinelBudget::default()), &TypesetOptions::default()).unwrap();
    assert_eq!(out.markers, vec![outcome(1, MarkerKind::CurrentPageNumber, Some("page-1"), resolved("1"))]);
}

#[test]
fn mt_31005_marker_counts_texts_and_passes_are_budgeted() {
    let doc = pages_doc(2, chain(&[0], 12.0, 1));
    let text = format!("{CUR}{CUR}{CUR}{CUR}");
    let st = story_of(&text);
    let small = SentinelBudget { max_markers: 3, ..SentinelBudget::default() };
    let r = compose(&request(&doc, &st, &[], &[], small), &TypesetOptions::default());
    assert_eq!(r, Err(SentinelError::Layout(LayoutError::BudgetExceeded)));
    // One pass cannot confirm a resolved value: the composition is refused, never half-resolved.
    let one = SentinelBudget { max_passes: 1, ..SentinelBudget::default() };
    assert_eq!(compose(&request(&doc, &st, &[], &[], one), &TypesetOptions::default()), Err(SentinelError::MarkersUnstable));
    let zero = SentinelBudget { max_passes: 0, ..SentinelBudget::default() };
    assert_eq!(
        compose(&request(&doc, &st, &[], &[], zero), &TypesetOptions::default()),
        Err(SentinelError::Layout(LayoutError::InvalidInput))
    );
    // Resolved text over the byte budget, or holding a terminator / sentinel / U+FFFC, is unresolved.
    let text = format!("{FOOTNOTE}{ENDNOTE}{XREF}");
    let st = story_of(&text);
    let gen_ = |at, kind, text: String| MarkerRecord { story_byte: at, kind, target: MarkerTarget::Generated { record_id: "r".into(), text } };
    let marks = [
        gen_(0, MarkerKind::FootnoteReference, "9".repeat(MAX_RESOLVED_MARKER_BYTES + 1)),
        gen_(3, MarkerKind::EndnoteReference, "x\ny".into()),
        gen_(6, MarkerKind::CrossReference, format!("p{CUR}")),
    ];
    let out = compose(&request(&doc, &st, &[], &marks, SentinelBudget::default()), &TypesetOptions::default()).unwrap();
    use UnresolvedMarker as U;
    let states: Vec<MarkerState> = out.markers.into_iter().map(|m| m.state).collect();
    assert_eq!(states, [unresolved(U::TextOverBudget), unresolved(U::TextNotInline), unresolved(U::TextNotInline)]);
    let exact = [gen_(0, MarkerKind::FootnoteReference, "9".repeat(MAX_RESOLVED_MARKER_BYTES))];
    let wide = pages_doc(2, vec![frame_layer(1, 1, Spec { page: 0, x: 10.0, y: 10.0, w: 180.0, h: 180.0, columns: 1, gutter: 0.0 }, None, None)]);
    let out = compose(&request(&wide, &st, &[], &exact, SentinelBudget::default()), &TypesetOptions::default()).unwrap();
    assert!(matches!(&out.markers[0].state, MarkerState::Resolved(t) if t.len() == MAX_RESOLVED_MARKER_BYTES));
    // Refusals: stale revision, cancellation, a refusing shaper.
    let req = request(&doc, &st, &[], &[], SentinelBudget::default());
    let stale = SentinelRequest { expected_revision: 1, ..req };
    assert_eq!(compose(&stale, &TypesetOptions::default()), Err(SentinelError::Layout(LayoutError::StaleRevision { expected: 1, current: 2 })));
    let token = CancellationToken::default();
    token.cancel();
    assert_eq!(
        compose_with_sentinels(&req, &TypesetOptions::default(), &records(), &mut toy, &token),
        Err(SentinelError::Layout(LayoutError::Canceled))
    );
    let mut refuse = |_: &ShapeInput<'_>| -> Result<ShapedParagraph, LayoutError> { Err(LayoutError::MissingFont) };
    assert_eq!(
        compose_with_sentinels(&req, &TypesetOptions::default(), &records(), &mut refuse, &CancellationToken::default()),
        Err(SentinelError::Layout(LayoutError::MissingFont))
    );
}

#[test]
fn mt_31005_the_shaper_sees_resolved_text_and_never_a_sentinel() {
    let text = format!("A{CUR}\u{FDDD}B\nC{SECTION}");
    let st = story_of(&text);
    let doc = pages_doc(2, chain(&[0, 1], 12.0, 1));
    let sections = [section("s1", "page-1", 5, StudioNumberStyle::LowerRoman, None, Some("Sec"))];
    let mut seen: Vec<(String, SourceSpan)> = Vec::new();
    let mut spy = |input: &ShapeInput<'_>| {
        seen.push((input.text.to_owned(), input.paragraph));
        toy(input)
    };
    let req = request(&doc, &st, &sections, &[], SentinelBudget::default());
    let out = compose_with_sentinels(&req, &TypesetOptions::default(), &records(), &mut spy, &CancellationToken::default()).unwrap();
    // Pass 1: markers unresolved (nothing), flush space removed. Pass 2: "v" (page 5, lower roman),
    // "Sec"; the flush space is never part of the shaped text.
    assert_eq!(
        seen,
        vec![
            ("AB\nC".to_owned(), SourceSpan { start: 0, end: 3 }),
            ("AB\nC".to_owned(), SourceSpan { start: 3, end: 4 }),
            ("AvB\nCSec".to_owned(), SourceSpan { start: 0, end: 4 }),
            ("AvB\nCSec".to_owned(), SourceSpan { start: 4, end: 8 }),
        ]
    );
    assert_eq!(out.passes, 2);
    // Story coordinates: A [0,1), "v" [1,4), the flush space as one zero-advance glyph [4,7), B [7,8).
    let p0: Vec<(SourceSpan, f64)> = out.paragraphs[0].glyphs.iter().map(|g| (g.cluster, g.advance_pt)).collect();
    assert_eq!(
        p0,
        vec![
            (SourceSpan { start: 0, end: 1 }, 6.0),
            (SourceSpan { start: 1, end: 4 }, 6.0),
            (SourceSpan { start: 4, end: 7 }, 0.0),
            (SourceSpan { start: 7, end: 8 }, 6.0),
        ]
    );
    assert_eq!(out.paragraphs[0].source, SourceSpan { start: 0, end: 9 });
    assert_eq!(out.paragraphs[1].source, SourceSpan { start: 9, end: 13 });
}

struct Collect(Vec<Vec<u8>>);
impl SinkPort for Collect {
    fn try_send(&mut self, _: DeliveryClass, bytes: &[u8]) -> Result<(), DeliveryError> {
        self.0.push(bytes.to_vec());
        Ok(())
    }
}
fn observe() -> Observe {
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let resource = DomainId::parse("SDOC-019abcde-0000-7000-8000-000000000001").unwrap();
    Observe::new(1, 3, resource, actor, Budget::new(0, 0).unwrap())
}

#[test]
fn mt_31005_terminal_outcomes_are_reported_through_observe() {
    let token = CancellationToken::default();
    let cases: [(Result<(), SentinelError>, Outcome); 4] = [
        (Ok(()), Outcome::Success),
        (Err(SentinelError::MarkerRecordInvalid), Outcome::Failure(FailureCode::Validation)),
        (Err(SentinelError::MarkersUnstable), Outcome::Failure(FailureCode::Unsupported)),
        (Err(SentinelError::Layout(LayoutError::Canceled)), Outcome::Canceled),
    ];
    for (result, expected) in cases {
        let mut sink = Collect(Vec::new());
        let receipt = report_sentinels(&result, 1, 3, &token, &mut observe(), &mut sink).unwrap();
        assert_eq!(receipt.outcome, expected);
        assert_eq!(sink.0.len(), 1);
    }
    assert_eq!(SentinelError::MarkerRecordInvalid.code(), "marker_record_invalid");
    assert_eq!(SentinelError::MarkersUnstable.code(), "markers_unstable");
    assert_eq!(UnresolvedMarker::NoNextPage.code(), "marker_no_next_page");
}

// ---- Item 2: break sentinels direct the flow (MT-31008) ----

const COLUMN: char = '\u{FDD8}';
const FRAME: char = '\u{FDD9}';
const PAGE: char = '\u{FDDA}';
const ODD: char = '\u{FDDB}';
const EVEN: char = '\u{FDDC}';

/// (thread position, column, baseline) of the non-empty line that starts at story byte `start`.
fn line_at(layout: &LayoutProposal, start: u64) -> (usize, usize, f64) {
    for (pos, f) in layout.frames.iter().enumerate() {
        for l in &f.lines {
            if l.source.start == start && l.source.end > start {
                return (pos, l.column, l.baseline_y_pt);
            }
        }
    }
    panic!("no line starts at {start}")
}
fn breaks_case(on: &[usize], h: f64, columns: u32, sections: &[StudioSection], text: &str) -> SentinelComposition {
    let st = story_of(text);
    let doc = pages_doc(4, chain(on, h, columns));
    compose(&request(&doc, &st, sections, &[], SentinelBudget::default()), &TypesetOptions::default()).unwrap()
}
fn applied(story_byte: u64, target: hsk_studio_type::BreakTarget, frame: usize, column: usize, skipped_frames: usize) -> BreakOutcome {
    BreakOutcome { story_byte, target, result: BreakResult::Applied { frame, column, skipped_frames } }
}

#[test]
fn mt_31008_each_break_sentinel_directs_the_following_text_to_its_flow_target() {
    use hsk_studio_type::BreakTarget as T;
    // "ab" + break (bytes 2..5) + "cd" (from byte 5). Frames are 24 pt high (two lines).
    // Column break: two 90 pt columns; "cd" starts column 1 at the top (else column 0, line 2).
    let out = breaks_case(&[0], 24.0, 2, &[], &format!("ab{COLUMN}cd"));
    assert_eq!(line_at(&out.layout, 0), (0, 0, 10.0));
    assert_eq!(line_at(&out.layout, 5), (0, 1, 10.0));
    assert_eq!(out.layout.frames[0].lines[1].x_start_pt, 90.0);
    assert_eq!(out.breaks, vec![applied(2, T::Column, 0, 1, 0)]);
    // Column break in the last column: the next frame.
    let out = breaks_case(&[0, 0], 24.0, 1, &[], &format!("ab{COLUMN}cd"));
    assert_eq!(line_at(&out.layout, 5), (1, 0, 10.0));
    assert_eq!(out.breaks, vec![applied(2, T::Column, 1, 0, 0)]);
    // Frame break: the next frame of the thread, even on the same page.
    let out = breaks_case(&[0, 0], 24.0, 2, &[], &format!("ab{FRAME}cd"));
    assert_eq!(line_at(&out.layout, 5), (1, 0, 10.0));
    assert_eq!(out.breaks, vec![applied(2, T::Frame, 1, 0, 0)]);
    // Page break: frames 0 and 1 are on page-1, frame 2 on page-2: frame 1 is passed over.
    let out = breaks_case(&[0, 0, 1], 24.0, 1, &[], &format!("ab{PAGE}cd"));
    assert_eq!(line_at(&out.layout, 5), (2, 0, 10.0));
    assert_eq!(out.breaks, vec![applied(2, T::Page, 2, 0, 1)]);
    assert!(out.layout.frames[1].lines.is_empty());
    // Odd / even-page breaks from page-1 over frames on pages 1, 2, 3 (implicit numbering 1, 2, 3).
    let out = breaks_case(&[0, 1, 2], 24.0, 1, &[], &format!("ab{ODD}cd"));
    assert_eq!(line_at(&out.layout, 5), (2, 0, 10.0));
    assert_eq!(out.breaks, vec![applied(2, T::OddPage, 2, 0, 1)]);
    let out = breaks_case(&[0, 1, 2], 24.0, 1, &[], &format!("ab{EVEN}cd"));
    assert_eq!(line_at(&out.layout, 5), (1, 0, 10.0));
    assert_eq!(out.breaks, vec![applied(2, T::EvenPage, 1, 0, 0)]);
    // Parity is the page NUMBER: a section restarting at 1 on page-2 makes page-2 odd.
    let restart = [
        section("s1", "page-1", 1, StudioNumberStyle::Arabic, None, None),
        section("s2", "page-2", 1, StudioNumberStyle::Arabic, None, None),
    ];
    let out = breaks_case(&[0, 1, 2], 24.0, 1, &restart, &format!("ab{ODD}cd"));
    assert_eq!(out.breaks, vec![applied(2, T::OddPage, 1, 0, 0)]);
    let out = breaks_case(&[0, 1, 2], 24.0, 1, &restart, &format!("ab{EVEN}cd"));
    assert_eq!(out.breaks, vec![applied(2, T::EvenPage, 2, 0, 1)]);
    // Successive breaks each apply; an empty paragraph ended by a break still moves the flow.
    let out = breaks_case(&[0, 1, 2, 3], 24.0, 1, &[], &format!("a{FRAME}{FRAME}b{PAGE}c"));
    assert_eq!(out.breaks, vec![applied(1, T::Frame, 1, 0, 0), applied(4, T::Frame, 2, 0, 0), applied(8, T::Page, 3, 0, 0)]);
    assert_eq!(line_at(&out.layout, 7), (2, 0, 10.0));
    assert_eq!(line_at(&out.layout, 11), (3, 0, 10.0));
    assert_eq!(out.layout.overset, None);
}

#[test]
fn mt_31008_a_break_without_a_target_oversets_from_the_break_byte() {
    use hsk_studio_type::BreakTarget as T;
    let overset = |story_byte, target, why| BreakOutcome { story_byte, target, result: BreakResult::Overset(why) };
    // Frame break in the last frame: "cd" is overset with the break, never dropped.
    let out = breaks_case(&[0], 24.0, 1, &[], &format!("ab{FRAME}cd"));
    assert_eq!(out.layout.overset, Some(SourceSpan { start: 2, end: 7 }));
    assert_eq!(out.breaks, vec![overset(2, T::Frame, BreakOverset::NoTarget)]);
    assert_eq!(out.layout.frames[0].lines.len(), 1);
    // Page, odd-page and even-page break with no later page in the thread (two frames on page-1);
    // the column break there still has the next frame.
    for c in [PAGE, ODD, EVEN, COLUMN] {
        let out = breaks_case(&[0, 0], 24.0, 1, &[], &format!("ab{c}cd"));
        let target = hsk_studio_type::catalog_member_for_scalar(c).and_then(|m| m.break_target).unwrap();
        if c == COLUMN {
            assert_eq!(out.breaks, vec![applied(2, T::Column, 1, 0, 0)]);
            continue;
        }
        assert_eq!(out.layout.overset, Some(SourceSpan { start: 2, end: 7 }), "U+{:04X}", c as u32);
        assert_eq!(out.breaks, vec![overset(2, target, BreakOverset::NoTarget)]);
    }
    let out = breaks_case(&[0], 24.0, 1, &[], &format!("ab{COLUMN}cd"));
    assert_eq!(out.layout.overset, Some(SourceSpan { start: 2, end: 7 }));
    // A break that ends the story in the last frame still oversets (the break itself).
    let out = breaks_case(&[0], 24.0, 1, &[], &format!("ab{PAGE}"));
    assert_eq!(out.layout.overset, Some(SourceSpan { start: 2, end: 5 }));
    assert_eq!(out.breaks, vec![overset(2, T::Page, BreakOverset::NoTarget)]);
}

#[test]
fn mt_31008_plain_reflow_honours_breaks_and_oversets_parity_breaks_it_cannot_number() {
    // The plain entry points read the same story bytes: a frame break moves "cd" to frame 2.
    let text = format!("ab{FRAME}cd");
    let (st, _) = story(1, &text, &[(0, 5), (5, 7)]);
    let paras = shape(&text, &[(0, 2), (5, 7)]);
    let doc = pages_doc(4, chain(&[0, 1], 24.0, 1));
    let p = run(&doc, &st, &paras, &records()).unwrap();
    assert_eq!(line_at(&p, 5), (1, 0, 10.0));
    assert_eq!(p.overset, None);
    // Odd/even parity needs the section numbering (compose_with_sentinels): overset, not guessed.
    let text = format!("ab{ODD}cd");
    let (st, _) = story(1, &text, &[(0, 5), (5, 7)]);
    let p = run(&doc, &st, &shape(&text, &[(0, 2), (5, 7)]), &records()).unwrap();
    assert_eq!(p.overset, Some(SourceSpan { start: 2, end: 7 }));
    assert!(p.frames[1].lines.is_empty());
}

#[test]
fn mt_31008_story_edits_split_paragraphs_after_every_terminator() {
    let st = story_of("abcd");
    for c in [COLUMN, FRAME, PAGE, ODD, EVEN, '\u{2029}', '\n', '\r', '\u{0085}'] {
        let edited = replace_story_text(&st, None, 2, 2, &c.to_string()).unwrap();
        let cut = 2 + c.len_utf8() as u64;
        let spans: Vec<(u64, u64)> = edited.paragraphs.iter().map(|p| (p.start, p.end)).collect();
        assert_eq!(spans, vec![(0, cut), (cut, cut + 2)], "U+{:04X}", c as u32);
        // The edited partition is the Type partition of the edited text.
        let type_like: Vec<(u64, u64)> = paragraph_partition(text_of(&edited)).iter().map(|s| (s.start, s.end)).collect();
        assert_eq!(spans, type_like);
    }
    // A forced line break, a marker or a flush space does not split.
    for c in ['\u{2028}', CUR, '\u{FDDD}'] {
        let edited = replace_story_text(&st, None, 2, 2, &c.to_string()).unwrap();
        assert_eq!(edited.paragraphs.len(), 1, "U+{:04X}", c as u32);
    }
}

// ---- Item 3: flush space (MT-31009) ----

const FLUSH: char = '\u{FDDD}';

/// One 48 pt high frame of width `w` on page-1 (four lines).
fn flush_case(text: &str, w: f64, alignment: Alignment) -> SentinelComposition {
    let st = story_of(text);
    let frame = frame_layer(1, 1, Spec { page: 0, x: 10.0, y: 10.0, w, h: 48.0, columns: 1, gutter: 0.0 }, None, None);
    let doc = pages_doc(2, vec![frame]);
    let opts = TypesetOptions { alignment, ..TypesetOptions::default() };
    compose(&request(&doc, &st, &[], &[], SentinelBudget::default()), &opts).unwrap()
}

#[test]
fn mt_31009_flush_space_absorbs_the_slack_of_a_fully_justified_last_line_only() {
    // "ab cd" FLUSH "e": a b ' ' c d e = 6 glyphs of 6 pt (36 pt) plus the flush space (0 pt) in 180 pt.
    let text = format!("ab cd{FLUSH}e");
    let full = flush_case(&text, 180.0, Alignment::JustifiedAll);
    let line = &full.layout.frames[0].lines[0];
    assert_eq!(line.width_pt, 36.0);
    assert_eq!(line.x_start_pt, 0.0);
    assert_eq!(line.justification, None, "the word space keeps its natural width");
    assert_eq!(full.flush_spaces, vec![FlushOutcome { story_byte: 5, frame: 0, line: 0, absorbed_pt: Some(144.0) }]);
    // Control: the same line without the flush space is justified through its word space.
    let plain = flush_case("ab cde", 180.0, Alignment::JustifiedAll);
    assert!(plain.layout.frames[0].lines[0].justification.is_some());
    assert!(plain.flush_spaces.is_empty());
    // Not fully justified (last line aligned left; left; centred): no width, nothing absorbed.
    for alignment in [Alignment::Justified, Alignment::Left, Alignment::Center] {
        let out = flush_case(&text, 180.0, alignment);
        assert_eq!(
            out.flush_spaces,
            vec![FlushOutcome { story_byte: 5, frame: 0, line: 0, absorbed_pt: None }],
            "{alignment:?}"
        );
        assert_eq!(out.layout.frames[0].lines[0].width_pt, 36.0);
    }
    // Two flush spaces share the slack equally: a FLUSH b FLUSH c = 18 pt in 180 pt.
    let two = flush_case(&format!("a{FLUSH}b{FLUSH}c"), 180.0, Alignment::JustifiedAll);
    let shares: Vec<Option<f64>> = two.flush_spaces.iter().map(|f| f.absorbed_pt).collect();
    assert_eq!(shares, vec![Some(81.0), Some(81.0)]);
}

#[test]
fn mt_31009_flush_space_on_a_line_that_is_not_the_last_has_no_width() {
    // 60 pt frame (10 glyphs): "aaaa" FLUSH "bbbb cccc" breaks after the space; line 0 is not the last.
    let text = format!("aaaa{FLUSH}bbbb cccc");
    let out = flush_case(&text, 60.0, Alignment::JustifiedAll);
    let lines = &out.layout.frames[0].lines;
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].width_pt, 48.0);
    assert_eq!(out.flush_spaces, vec![FlushOutcome { story_byte: 4, frame: 0, line: 0, absorbed_pt: None }]);
    // The flush space is one zero-advance glyph of its own 3-byte cluster, never a shaped glyph.
    let g = &out.paragraphs[0].glyphs[4];
    assert_eq!((g.cluster, g.advance_pt), (SourceSpan { start: 4, end: 7 }, 0.0));
}

// ---- SMB-LAYOUT-SENTINELS-1 remediation (R53: break after a glyph-less marker; V4-06) ----

#[test]
fn mt_31008_a_break_after_a_marker_without_glyphs_still_directs_the_flow() {
    // A marker shaped as nothing (the index marker, whose value the spec does not state, or any
    // unresolved marker) right before a break sentinel left the break unseen: the following text
    // stayed in the frame and no BreakOutcome was reported. Cases of the R53 validator probe plus
    // its control; byte offsets: "a"/"b" 1 byte, every sentinel 3 bytes.
    use hsk_studio_type::BreakTarget as T;
    // Control: "ab" PAGE "cd" over frames on page-1, page-1, page-2: frame 1 is passed over.
    let out = breaks_case(&[0, 0, 1], 24.0, 1, &[], &format!("ab{PAGE}cd"));
    assert_eq!(out.breaks, vec![applied(2, T::Page, 2, 0, 1)]);
    assert_eq!(line_at(&out.layout, 5), (2, 0, 10.0));
    // "ab" INDEX PAGE "cd": the page break at byte 5, "cd" from byte 8 on page-2.
    let out = breaks_case(&[0, 0, 1], 24.0, 1, &[], &format!("ab{INDEX}{PAGE}cd"));
    assert_eq!(out.breaks, vec![applied(5, T::Page, 2, 0, 1)]);
    assert_eq!(line_at(&out.layout, 8), (2, 0, 10.0));
    // INDEX PAGE "cd": a paragraph holding only the glyph-less marker and the break.
    let out = breaks_case(&[0, 0, 1], 24.0, 1, &[], &format!("{INDEX}{PAGE}cd"));
    assert_eq!(out.breaks, vec![applied(3, T::Page, 2, 0, 1)]);
    assert_eq!(line_at(&out.layout, 6), (2, 0, 10.0));
    // "ab" XREF FRAME "cd" with no cross-reference record (unresolved, record missing).
    let out = breaks_case(&[0, 0], 24.0, 1, &[], &format!("ab{XREF}{FRAME}cd"));
    assert_eq!(out.markers[0].state, MarkerState::Unresolved(UnresolvedMarker::RecordMissing));
    assert_eq!(out.breaks, vec![applied(5, T::Frame, 1, 0, 0)]);
    assert_eq!(line_at(&out.layout, 8), (1, 0, 10.0));
    // Previous page number on page-1 (unresolved, no previous page) before a frame break.
    let out = breaks_case(&[0, 0], 24.0, 1, &[], &format!("a{PREV}{FRAME}b"));
    assert_eq!(out.breaks, vec![applied(4, T::Frame, 1, 0, 0)]);
    assert_eq!(line_at(&out.layout, 7), (1, 0, 10.0));
    // The plain entry point (no marker resolution) sees the same break after the marker.
    let text = format!("ab{INDEX}{FRAME}cd");
    let (st, _) = story(1, &text, &[(0, 8), (8, 10)]);
    let doc = pages_doc(4, chain(&[0, 1], 24.0, 1));
    let p = run(&doc, &st, &shape(&text, &[(0, 2), (8, 10)]), &records()).unwrap();
    assert_eq!(line_at(&p, 8), (1, 0, 10.0));
    assert_eq!(p.overset, None);
    // A break without a target after a glyph-less marker oversets from the break byte.
    let out = breaks_case(&[0], 24.0, 1, &[], &format!("ab{INDEX}{FRAME}cd"));
    assert_eq!(out.layout.overset, Some(SourceSpan { start: 5, end: 10 }));
    assert_eq!(out.breaks, vec![BreakOutcome { story_byte: 5, target: T::Frame, result: BreakResult::Overset(BreakOverset::NoTarget) }]);
}
