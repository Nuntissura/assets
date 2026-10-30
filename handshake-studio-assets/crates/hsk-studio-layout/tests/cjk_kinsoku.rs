//! Lane SMB-ID-CJK-KINSOKU-1, slices 2 and 4: kinsoku line breaking in the item model and the
//! composer wiring through Folio `hsk.studio.cjk_composition@1`. Synthetic advances (every glyph a
//! 10 pt em box, `ShapedParagraph::from_fixed_advance`), so breaks are closed-form. Equality with
//! InDesign's Japanese composer is NOT_PROVEN (no InDesign CJK oracle case, D5).
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    AutoSizing, Bounds, DocumentSchema, FirstBaseline, GeometryUnit, GraphSchema, LayerSchema, LayoutEdges, Length, NodeKind, NodeRef,
    PageSide, PageSpreadSchema, Parent, Payload, ProfileBinding, StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind,
    StudioBurasagariType, StudioCjkCompositionRecords, StudioCjkParagraphBinding, StudioCompositionEngine, StudioCompositionScope,
    StudioDocument, StudioKinsokuHangType, StudioKinsokuOrder, StudioKinsokuType, StudioLayer, StudioLayerGraph, StudioLayoutFlowRecords,
    StudioMojikumiBaseSet, StudioNamedComposer, StudioPage, StudioPageSpread, StudioTextFrame, StudioTextStory, TextStorySchema,
    VerticalJustification,
};
use hsk_studio_layout::*;

const EM: f64 = 10.0;

fn para(text: &str) -> ShapedParagraph {
    ShapedParagraph::from_fixed_advance(text, SourceSpan { start: 0, end: text.len() as u64 }, EM, 12.0, 10.0).unwrap()
}
fn chars_of(text: &str) -> Vec<char> {
    text.chars().collect()
}
fn single(kinsoku: Option<KinsokuSet>, set: StudioMojikumiBaseSet) -> CjkParagraph {
    CjkParagraph::new(StudioCompositionScope::SingleLine, kinsoku, MojikumiRules::base(set))
}
fn hard() -> Option<KinsokuSet> {
    KinsokuSet::shipped("hard_kinsoku")
}
fn lines(text: &str, c: &CjkParagraph, measure: f64) -> Vec<CjkComposedLine> {
    compose_cjk_lines(&para(text), text, c, EM, measure, false, &CancellationToken::default()).unwrap()
}
fn starts(ls: &[CjkComposedLine]) -> Vec<usize> {
    ls.iter().map(|l| l.glyph_range.start).collect()
}

/// STU-TYP-137 `cant_begin_line_chars`: over several measures, texts and both scopes no line
/// starts with a character of the table's line-start set (JLREQ hard and soft tables).
#[test]
fn cant_begin_chars_never_start_a_line() {
    let texts = ["漢字。漢字、漢字」漢字ー漢字ッ漢字", "「漢字」、漢字。漢字！漢字・漢字ァ漢字", "漢漢漢漢。漢漢漢、漢漢」。漢"];
    for name in ["hard_kinsoku", "soft_kinsoku"] {
        let k = KinsokuSet::shipped(name).unwrap();
        for scope in [StudioCompositionScope::SingleLine, StudioCompositionScope::EveryLine] {
            for set in [StudioMojikumiBaseSet::Nothing, StudioMojikumiBaseSet::LineEndAllOneHalfEm, StudioMojikumiBaseSet::SimplifiedChineseDefault] {
                let c = CjkParagraph::new(scope, Some(k.clone()), MojikumiRules::base(set));
                for text in texts {
                    let cs = chars_of(text);
                    for m in (3..=9).map(|n| f64::from(n) * EM) {
                        let ls = lines(text, &c, m);
                        assert_eq!(ls.last().unwrap().glyph_range.end, cs.len());
                        for l in &ls[1..] {
                            let first = cs[l.glyph_range.start];
                            assert!(!k.cant_begin.contains(&first), "{name} {scope:?} {set:?} {text} m={m}: line starts with {first}");
                            let prev = cs[l.glyph_range.start - 1];
                            assert!(!k.cant_end.contains(&prev), "{name} {text} m={m}: line ends with {prev}");
                        }
                    }
                }
            }
        }
    }
    // The soft table lets the prolonged sound mark and small kana start a line; hard does not.
    let soft = KinsokuSet::shipped("soft_kinsoku").unwrap();
    assert!(!soft.cant_begin.contains(&'ー') && !soft.cant_begin.contains(&'ッ'));
    let strict = hard().unwrap();
    assert!(strict.cant_begin.contains(&'ー') && strict.cant_begin.contains(&'ッ'));
    let c = single(Some(soft), StudioMojikumiBaseSet::Nothing);
    assert_eq!(starts(&lines("漢漢漢ー漢", &c, 30.0)), vec![0, 3]);
    let c = single(hard(), StudioMojikumiBaseSet::Nothing);
    assert_eq!(starts(&lines("漢漢漢ー漢", &c, 30.0)), vec![0, 2], "pushed out: ー may not begin a line");
    // cant_be_separated: the two-dot leader pair stays together.
    assert_eq!(starts(&lines("漢漢‥‥漢", &c, 30.0)), vec![0, 2]);
    // bunri_kinshi adds the double period, ellipsis and double hyphen to the active set (a
    // composer with no kinsoku table has none otherwise).
    let mut c = single(None, StudioMojikumiBaseSet::Nothing);
    assert_eq!(starts(&lines("漢漢‥‥漢", &c, 30.0)), vec![0, 3]);
    c.bunri_kinshi = true;
    assert_eq!(starts(&lines("漢漢‥‥漢", &c, 30.0)), vec![0, 2]);
    assert_eq!(starts(&lines("漢漢……漢", &c, 30.0)), vec![0, 2]);
}

/// STU-TYP-137 `kinsoku_type`: the natural line end (40 pt) falls before a full stop that may not
/// begin a line. Push-in compresses the comma's aki (5 pt shrink) to take the full stop in; push-out
/// breaks before the forbidden run. Single-line scope (the explicit rule); prioritize_adjustment
/// takes the smaller adjustment (5 pt shrink against 10 pt stretch).
#[test]
fn push_in_and_push_out_choose_differently() {
    let text = "漢、漢漢。漢漢";
    let mut c = single(hard(), StudioMojikumiBaseSet::LineEndAllOneHalfEm);
    let push_in = lines(text, &c, 40.0);
    assert_eq!(push_in[0].glyph_range, 0..5);
    assert!((push_in[0].width_pt - 40.0).abs() < 1e-9, "{push_in:?}");
    assert!((push_in[0].ratio + 1.0).abs() < 1e-9, "compressed to the comma aki minimum");
    // The comma's 5 pt aki is gone: the third glyph sits 5 pt closer than the shaped advances say.
    assert_eq!(push_in[0].gaps, vec![CjkGap { glyph: 1, delta_pt: -5.0 }]);
    c.kinsoku_type = StudioKinsokuType::PushOutFirst;
    let push_out = lines(text, &c, 40.0);
    assert_eq!(push_out[0].glyph_range, 0..3);
    assert_eq!(push_out[1].glyph_range.start, 3);
    c.kinsoku_type = StudioKinsokuType::PushOutOnly;
    assert_eq!(lines(text, &c, 40.0)[0].glyph_range, 0..3);
    c.kinsoku_type = StudioKinsokuType::PrioritizeAdjustment;
    assert_eq!(lines(text, &c, 40.0)[0].glyph_range, 0..5);
    assert_ne!(push_in, push_out);
    // Every-line scope composes the same paragraph without a forbidden line start either way.
    for k in [StudioKinsokuType::PushInFirst, StudioKinsokuType::PushOutFirst, StudioKinsokuType::PushOutOnly] {
        let mut c = CjkParagraph::new(StudioCompositionScope::EveryLine, hard(), MojikumiRules::base(StudioMojikumiBaseSet::LineEndAllOneHalfEm));
        c.kinsoku_type = k;
        let ls = lines(text, &c, 40.0);
        assert!(ls.iter().all(|l| l.glyph_range.start != 4), "{k:?}");
    }
}

/// STU-TYP-137 `rensuuji`: a run of full-width digits is never broken when set, and breaks like
/// any CJK run when not.
#[test]
fn rensuuji_keeps_digit_runs_together() {
    let text = "漢漢１２３漢";
    let mut c = single(None, StudioMojikumiBaseSet::Nothing);
    assert_eq!(starts(&lines(text, &c, 30.0)), vec![0, 3]);
    c.rensuuji = true;
    let ls = lines(text, &c, 30.0);
    assert_eq!(starts(&ls), vec![0, 2, 5]);
    for l in &ls {
        assert!(!(3..5).contains(&l.glyph_range.start), "break inside the digit run: {ls:?}");
    }
    // Every-line scope too.
    c.scope = StudioCompositionScope::EveryLine;
    assert!(lines(text, &c, 30.0).iter().all(|l| !(3..5).contains(&l.glyph_range.start)));
}

/// STU-TYP-137 `burasagari_type` / `kinsoku_hang_type`: the full stop that may not begin a line
/// hangs past the measure (standard: only when it does not fit; forced: always at a line end).
#[test]
fn burasagari_hangs_the_period() {
    let c = single(hard(), StudioMojikumiBaseSet::Nothing);
    assert_eq!(starts(&lines("漢漢漢。漢", &c, 30.0)), vec![0, 2], "no hanging: pushed out");
    let mut c = c;
    c.hang = HangMode::of(StudioBurasagariType::Standard, StudioKinsokuHangType::None);
    assert_eq!(c.hang, HangMode::Optional);
    let ls = lines("漢漢漢。漢", &c, 30.0);
    assert_eq!(ls[0].glyph_range, 0..4);
    assert_eq!((ls[0].width_pt, ls[0].hang_pt), (30.0, 10.0));
    assert!(!ls[0].overfull);
    // Standard does not hang a period that fits; forced does.
    let ls = lines("漢漢。漢漢", &c, 30.0);
    assert_eq!((ls[0].glyph_range.clone(), ls[0].width_pt, ls[0].hang_pt), (0..3, 30.0, 0.0));
    c.hang = HangMode::of(StudioBurasagariType::None, StudioKinsokuHangType::Force);
    assert_eq!(c.hang, HangMode::Forced);
    let ls = lines("漢漢。漢漢", &c, 30.0);
    assert_eq!((ls[0].glyph_range.clone(), ls[0].width_pt, ls[0].hang_pt), (0..3, 20.0, 10.0));
    // At the paragraph end an optional hang applies only when the period does not fit.
    c.hang = HangMode::Optional;
    let ls = lines("漢漢漢。", &c, 30.0);
    assert_eq!((ls.len(), ls[0].width_pt, ls[0].hang_pt), (1, 30.0, 10.0));
    let ls = lines("漢漢。", &c, 30.0);
    assert_eq!((ls.len(), ls[0].width_pt, ls[0].hang_pt), (1, 30.0, 0.0));
}

// ---- slice 4: composer wiring through Folio records ----

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
fn edges(v: f64) -> LayoutEdges {
    LayoutEdges { top: len(v), right: len(v), bottom: len(v), left: len(v) }
}
fn document(w: f64, h: f64) -> StudioDocument {
    let unassigned = || ProfileBinding::Unassigned(());
    let page = StudioPage { page_id: "page-1".into(), width: len(400.0), height: len(400.0), side: PageSide::RightHand, margins: edges(5.0), bleed: None };
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 3,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![StudioPageSpread {
            schema_id: PageSpreadSchema::V1,
            page_spread_id: id("SPGS", 1),
            name: "spread".into(),
            parent: Parent::Root(()),
            bounds: Bounds { x: len(0.0), y: len(0.0), width: len(400.0), height: len(400.0) },
            pages: vec![page],
        }],
        layers: vec![StudioLayer {
            schema_id: LayerSchema::V1,
            layer_id: id("SLYR", 1),
            name: "frame".into(),
            parent: Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", 1) }),
            kind: "text".into(),
            payload: Payload::Text { story_id: id("STXT", 1) },
            visible: true,
            appearance: None,
            text_frame: Some(StudioTextFrame {
                page_id: "page-1".into(),
                bounds: Bounds { x: len(0.0), y: len(0.0), width: len(w), height: len(h) },
                inset_spacing: edges(0.0),
                text_column_count: 1,
                text_column_gutter: len(0.0),
                first_baseline_offset: FirstBaseline::Fixed,
                minimum_first_baseline_offset: len(10.0),
                vertical_justification: VerticalJustification::Top,
                auto_sizing_type: AutoSizing::Off,
                previous_text_frame: None,
                next_text_frame: None,
            }),
        }],
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}
/// One paragraph per `\n`-terminated piece; paragraph `i` bound to style `styles[i]`.
fn story(text: &str, styles: &[u32]) -> StudioTextStory {
    let mut paragraphs = Vec::new();
    let mut start = 0u64;
    for (i, piece) in text.split_inclusive('\n').enumerate() {
        let end = start + piece.len() as u64;
        paragraphs.push(StoryParagraph { start, end, style_ref: Some(id("STYS", styles[i])), paragraph_direction: None });
        start = end;
    }
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", 1),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs,
        character_ranges: vec![StoryRange { start: 0, end: text.len() as u64, style_ref: None, applied_language: Some("ja-JP".into()) }],
    }
}
/// Shaped paragraphs; CJK pieces are marked non-Latin as Type would mark Hani/Hira runs.
fn shaped(text: &str, advance: f64) -> Vec<ShapedParagraph> {
    let mut out = Vec::new();
    let mut start = 0u64;
    for piece in text.split_inclusive('\n') {
        let body = piece.strip_suffix('\n').unwrap_or(piece);
        let source = SourceSpan { start, end: start + body.len() as u64 };
        let mut p = ShapedParagraph::from_fixed_advance(text, source, advance, 12.0, 10.0).unwrap();
        p.latin_only = body.is_ascii();
        out.push(p);
        start += piece.len() as u64;
    }
    out
}
fn records(scope: StudioCompositionScope, bound_style: u32) -> StudioCjkCompositionRecords {
    let mut r = StudioCjkCompositionRecords::new(id("SDOC", 1));
    r.composers.push(StudioNamedComposer {
        composer_name: "Japanese".into(),
        composition_scope: scope,
        composition_engine: StudioCompositionEngine::LatinCjk,
        kinsoku_table: Some("hard_kinsoku".into()),
        mojikumi_table: Some("line_end_all_one_half_em".into()),
    });
    r.bindings.push(StudioCjkParagraphBinding {
        style_id: id("STYS", bound_style),
        composer_name: "Japanese".into(),
        kinsoku_type: StudioKinsokuType::PushInFirst,
        kinsoku_hang_type: StudioKinsokuHangType::None,
        bunri_kinshi: false,
        rensuuji: true,
        burasagari_type: StudioBurasagariType::None,
        kinsoku_order: StudioKinsokuOrder::PushIn,
    });
    r
}
fn request<'a>(doc: &'a StudioDocument, st: &'a StudioTextStory, paragraphs: &'a [ShapedParagraph], composer: Composer) -> ReflowRequest<'a> {
    ReflowRequest {
        document: doc,
        story: st,
        resolved_text: None,
        paragraphs,
        obstacles: &[],
        composer,
        space: SpaceParams::default(),
        expected_revision: 3,
        limits: Limits::default(),
    }
}

/// STU-TYP-135/136: a paragraph whose style binds the named CJK composer composes through kinsoku
/// and mojikumi in both scopes (a CJK paragraph without a binding stays `unsupported_composition`);
/// an unresolvable composer name is refused, never a fallback.
#[test]
fn cjk_paragraph_composes_in_both_scopes() {
    let text = "漢字「漢字」、漢字。漢字漢字漢字ー漢字、漢字漢字。";
    let cs = chars_of(text);
    let doc = document(45.0, 300.0);
    let st = story(text, &[1]);
    let ps = shaped(text, EM);
    let flow = StudioLayoutFlowRecords::new(id("SDOC", 1));
    let token = CancellationToken::default();
    let req = request(&doc, &st, &ps, Composer::FirstFit);
    assert_eq!(reflow_typeset(&req, &TypesetOptions::default(), &token).unwrap_err(), LayoutError::UnsupportedComposition);
    let k = KinsokuSet::shipped("hard_kinsoku").unwrap();
    let mut results = Vec::new();
    for scope in [StudioCompositionScope::SingleLine, StudioCompositionScope::EveryLine] {
        let out = reflow_cjk(&req, &TypesetOptions::default(), &flow, &records(scope, 1), &token).unwrap();
        let lines = &out.proposal.frames[0].lines;
        assert!(lines.len() > 1 && out.proposal.overset.is_none(), "{scope:?}");
        assert_eq!(out.spacing.len(), lines.len(), "every line is CJK-composed");
        assert_eq!(lines.last().unwrap().glyph_range.end, cs.len());
        for (i, l) in lines.iter().enumerate() {
            assert!(l.width_pt <= l.available_pt + 1e-9, "{scope:?} line {i} {l:?}");
            assert!(l.justification.is_none() && !l.hyphenated);
            if i > 0 {
                assert!(!k.cant_begin.contains(&cs[l.glyph_range.start]), "{scope:?} line {i} starts with {}", cs[l.glyph_range.start]);
                assert_eq!(l.glyph_range.start, lines[i - 1].glyph_range.end, "lines tile the paragraph");
            }
        }
        // The opening bracket glyph starts half-width at a line start: its origin is 5 pt left of
        // the frame edge when it begins a line; otherwise lines start at the frame edge.
        for l in lines {
            let expect = if cs[l.glyph_range.start] == '「' { -5.0 } else { 0.0 };
            assert!((l.x_start_pt - expect).abs() < 1e-9, "{l:?}");
        }
        results.push(lines.iter().map(|l| l.glyph_range.clone()).collect::<Vec<_>>());
    }
    // Unresolvable composer name: refused.
    let mut bad = records(StudioCompositionScope::SingleLine, 1);
    bad.bindings[0].composer_name = "Korean".into();
    assert_eq!(reflow_cjk(&req, &TypesetOptions::default(), &flow, &bad, &token).unwrap_err(), LayoutError::InvalidInput);
    let mut bad = records(StudioCompositionScope::SingleLine, 1);
    bad.composers[0].mojikumi_table = Some("missing".into());
    assert_eq!(reflow_cjk(&req, &TypesetOptions::default(), &flow, &bad, &token).unwrap_err(), LayoutError::InvalidInput);
    let mut bad = records(StudioCompositionScope::SingleLine, 1);
    bad.composers[0].composition_engine = StudioCompositionEngine::WorldReady;
    assert_eq!(reflow_cjk(&req, &TypesetOptions::default(), &flow, &bad, &token).unwrap_err(), LayoutError::UnsupportedComposition);
    let mut other = records(StudioCompositionScope::SingleLine, 1);
    other.document_id = id("SDOC", 2);
    assert_eq!(reflow_cjk(&req, &TypesetOptions::default(), &flow, &other, &token).unwrap_err(), LayoutError::InvalidInput);
}

/// Paragraphs without a CJK binding compose byte-for-byte as before: `reflow_cjk` equals
/// `reflow_flow` over Latin fixtures, widths, composers and alignments, with no spacing entries;
/// in a mixed story the Latin paragraph's lines equal its lines in a Latin-only story.
#[test]
fn latin_paragraphs_are_unchanged() {
    let flow = StudioLayoutFlowRecords::new(id("SDOC", 1));
    let token = CancellationToken::default();
    let texts = [
        "The quick brown fox jumps over the lazy dog and keeps running far away.\nSecond paragraph, short.",
        "Hyphen-ated words and well-known compounds break after hyphens only.",
    ];
    for text in texts {
        let pieces = text.split_inclusive('\n').count();
        let st = story(text, &vec![2; pieces]);
        let ps = shaped(text, 6.0);
        for width in [60.0, 97.0, 150.0, 233.0] {
            let doc = document(width, 300.0);
            for composer in [Composer::FirstFit, Composer::KnuthPlass(KnuthPlassParams::default())] {
                for alignment in [Alignment::Left, Alignment::Justified, Alignment::Center] {
                    let opts = TypesetOptions { alignment, ..TypesetOptions::default() };
                    let req = request(&doc, &st, &ps, composer);
                    let plain = reflow_flow(&req, &opts, &flow, &token).unwrap();
                    for r in [StudioCjkCompositionRecords::new(id("SDOC", 1)), records(StudioCompositionScope::EveryLine, 1)] {
                        let cjk = reflow_cjk(&req, &opts, &flow, &r, &token).unwrap();
                        assert_eq!(cjk.proposal, plain, "{text} {width} {composer:?} {alignment:?}");
                        assert!(cjk.spacing.is_empty());
                    }
                }
            }
        }
    }
    // Mixed story: Latin paragraph (style 2) then a CJK paragraph (style 1).
    let latin = "Plain Latin text that wraps over lines.";
    let mixed = format!("{latin}\n漢字漢字、漢字。");
    let doc = document(97.0, 300.0);
    let (st_l, ps_l) = (story(latin, &[2]), shaped(latin, 6.0));
    let (st_m, ps_m) = (story(&mixed, &[2, 1]), shaped(&mixed, 6.0));
    let alone = reflow_flow(&request(&doc, &st_l, &ps_l, Composer::FirstFit), &TypesetOptions::default(), &flow, &token).unwrap();
    let both = reflow_cjk(&request(&doc, &st_m, &ps_m, Composer::FirstFit), &TypesetOptions::default(), &flow, &records(StudioCompositionScope::SingleLine, 1), &token)
        .unwrap();
    let latin_lines: Vec<_> = both.proposal.frames[0].lines.iter().filter(|l| l.paragraph == 0).cloned().collect();
    assert_eq!(latin_lines, alone.frames[0].lines);
    let n_latin = latin_lines.len();
    assert!(both.spacing.iter().all(|s| s.frame == 0 && s.line >= n_latin));
    assert_eq!(both.spacing.len(), both.proposal.frames[0].lines.len() - n_latin);
}
