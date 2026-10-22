//! Lane SMB-LAYOUT-COMPOSER-WIRING: Folio paragraph-style composition settings select the
//! Adobe-parity composers and carry their H&J and hyphenation parameters into composition. Toy font
//! (every glyph 6 pt, line height 12, ascent 10, first baseline 10 pt below the frame top), as in
//! tests/typeset.rs; hand-computed expectations.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    AutoSizing, Bounds, DocumentSchema, FirstBaseline, GeometryUnit, GraphSchema, LayerSchema, LayoutEdges, Length, NodeKind, NodeRef,
    PageSide, PageSpreadSchema, Parent, Payload, ProfileBinding, StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind,
    StudioCompositionEngine, StudioCompositionScope, StudioCompositionSettings, StudioDocument, StudioLayer, StudioLayerGraph,
    StudioPage, StudioPageSpread, StudioParagraphComposition, StudioSingleWordJustification, StudioStyleComposition, StudioTextAlign,
    StudioTextFrame, StudioTextStory, StudioTypeStyle, StudioTypeStyleRecords, TextStorySchema, TypeStyleSchema, TypeStyleScope,
    VerticalJustification,
};
use hsk_studio_layout::*;

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
/// One paragraph per `\n`-terminated piece of `text`, every paragraph bound to style 1.
fn story(text: &str) -> StudioTextStory {
    let mut paragraphs = Vec::new();
    let mut start = 0u64;
    for piece in text.split_inclusive('\n') {
        let end = start + piece.len() as u64;
        paragraphs.push(StoryParagraph { start, end, style_ref: Some(id("STYS", 1)), paragraph_direction: None });
        start = end;
    }
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", 1),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs,
        character_ranges: vec![StoryRange { start: 0, end: text.len() as u64, style_ref: None, applied_language: Some("en-US".into()) }],
    }
}
fn shaped(text: &str) -> Vec<ShapedParagraph> {
    let mut out = Vec::new();
    let mut start = 0u64;
    for piece in text.split_inclusive('\n') {
        let body = piece.strip_suffix('\n').unwrap_or(piece);
        let source = SourceSpan { start, end: start + body.len() as u64 };
        out.push(ShapedParagraph::from_fixed_advance(text, source, 6.0, 12.0, 10.0).unwrap());
        start += piece.len() as u64;
    }
    out
}
fn style(n: u32, based_on: Option<u32>, align: Option<StudioTextAlign>) -> StudioTypeStyle {
    StudioTypeStyle {
        schema_id: TypeStyleSchema::V1,
        style_id: id("STYS", n),
        scope: TypeStyleScope::Paragraph,
        name: format!("style/{n}"),
        based_on: based_on.map(|b| id("STYS", b)),
        next_style: None,
        font_family: None,
        font_style: None,
        font_size: Some(len(12.0)),
        fill: None,
        line_height: None,
        letter_spacing: None,
        align,
        grep_styles: Vec::new(),
    }
}
/// Style 1 (aligned `align`) with `settings` (none when empty).
fn records(align: StudioTextAlign, settings: StudioCompositionSettings) -> StudioTypeStyleRecords {
    let mut r = StudioTypeStyleRecords::new(id("SDOC", 1));
    r.styles = vec![style(1, None, Some(align))];
    if !settings.is_empty() {
        r.style_composition.push(StudioStyleComposition { style_id: id("STYS", 1), settings });
    }
    r
}
fn run(text: &str, width: f64, composer: Composer, opts: &TypesetOptions<'_>) -> Result<LayoutProposal, LayoutError> {
    let doc = document(width, 300.0);
    let st = story(text);
    let paragraphs = shaped(text);
    reflow_typeset(
        &ReflowRequest {
            document: &doc,
            story: &st,
            resolved_text: None,
            paragraphs: &paragraphs,
            obstacles: &[],
            composer,
            space: SpaceParams::default(),
            expected_revision: 3,
            limits: Limits::default(),
        },
        opts,
        &CancellationToken::default(),
    )
}
fn styled(set: &StudioTypeStyleRecords) -> TypesetOptions<'_> {
    TypesetOptions { styles: Some(StyleSource { records: set, default_font_size_pt: 12.0 }), hyphen_advance_pt: Some(6.0), ..Default::default() }
}
fn scope(s: StudioCompositionScope) -> StudioCompositionSettings {
    StudioCompositionSettings { composition_scope: Some(s), ..Default::default() }
}
fn hy() -> HyphenationParams {
    HyphenationParams { hyphen_advance_pt: 6.0, ..HyphenationParams::default() }
}
const TEXT: &str = "aaaa bbbb cccc dddd\naa typographic\n";

/// Without composition settings the styled story composes exactly as before the settings existed
/// (request composer FirstFit and the plain options), whatever other styles or stories state.
#[test]
fn absent_settings_compose_identically() {
    for align in [StudioTextAlign::Left, StudioTextAlign::Justified] {
        let plain = records(align, StudioCompositionSettings::default());
        let before = run(TEXT, 82.0, Composer::FirstFit, &styled(&plain)).unwrap();
        // Settings on a style no paragraph uses, and an override of another story, change nothing.
        let mut other = plain.clone();
        other.styles.push(style(2, None, None));
        other.style_composition.push(StudioStyleComposition { style_id: id("STYS", 2), settings: scope(StudioCompositionScope::SingleLine) });
        other.paragraph_composition.push(StudioParagraphComposition { text_id: id("STXT", 9), paragraph_start: 0, settings: scope(StudioCompositionScope::EveryLine) });
        assert_eq!(run(TEXT, 82.0, Composer::FirstFit, &styled(&other)).unwrap(), before, "{align:?}");
        // paragraph_params without composition is the request itself.
        let o = styled(&plain);
        let p = paragraph_params(None, 12.0, Composer::FirstFit, SpaceParams::default(), &o).unwrap();
        assert_eq!((p.composer, p.space, p.hyphenation, p.hyphenate_last_word, p.single_word), (Composer::FirstFit, SpaceParams::default(), None, true, None));
    }
}

/// `composition_scope` selects the Adobe-parity Single-line / Paragraph composers with the style's
/// H&J values: identical to requesting those composers explicitly with the same parameters, and
/// different from the FirstFit default (the 82 pt Single-line tight break, tests/typeset.rs).
#[test]
fn composition_scope_selects_the_adobe_composers() {
    let hj = StudioCompositionSettings {
        word_spacing_minimum: Some(80.0),
        word_spacing_desired: Some(100.0),
        word_spacing_maximum: Some(133.0),
        hyphenation: Some(false),
        ..Default::default()
    };
    let explicit = TypesetOptions { justify: Some(JustifyParams::default()), alignment: Alignment::Justified, ..Default::default() };
    for (s, composer) in [
        (StudioCompositionScope::SingleLine, Composer::AdobeSingleLine(AdobeParams::default())),
        (StudioCompositionScope::EveryLine, Composer::AdobeParagraph(AdobeParams::default())),
    ] {
        let set = records(StudioTextAlign::Justified, StudioCompositionSettings { composition_scope: Some(s), ..hj.clone() });
        let wired = run(TEXT, 82.0, Composer::FirstFit, &styled(&set)).unwrap();
        assert_eq!(wired, run(TEXT, 82.0, composer, &explicit).unwrap(), "{s:?}");
    }
    let single = records(StudioTextAlign::Justified, StudioCompositionSettings { composition_scope: Some(StudioCompositionScope::SingleLine), ..hj });
    assert_eq!(run(TEXT, 82.0, Composer::FirstFit, &styled(&single)).unwrap().frames[0].lines[0].glyph_range, 0..15, "tight break");
    let plain = records(StudioTextAlign::Justified, StudioCompositionSettings::default());
    assert_eq!(run(TEXT, 82.0, Composer::FirstFit, &styled(&plain)).unwrap().frames[0].lines[0].glyph_range, 0..10, "first fit");
}

/// The `based_on` chain overlays root first and the local paragraph override wins (STU-TYP-201):
/// root every_line, child silent, override of paragraph 2 single_line with zone 50 -> paragraph 2
/// is set by the Single-line Composer and keeps "typographic" whole (zone 36 hyphenates it).
#[test]
fn inheritance_and_local_override_resolve_in_order() {
    let mut set = StudioTypeStyleRecords::new(id("SDOC", 1));
    set.styles = vec![style(1, Some(2), None), style(2, None, Some(StudioTextAlign::Left))];
    set.style_composition.push(StudioStyleComposition {
        style_id: id("STYS", 2),
        settings: StudioCompositionSettings { composition_scope: Some(StudioCompositionScope::EveryLine), hyphenation: Some(true), ..Default::default() },
    });
    let chain = resolve_style_composition(&set, &id("STYS", 1)).unwrap();
    assert_eq!((chain.composition_scope, chain.hyphenation), (Some(StudioCompositionScope::EveryLine), Some(true)));
    let second = TEXT.find('\n').unwrap() as u64 + 1;
    let zone = |z: f64| {
        let mut s = set.clone();
        s.paragraph_composition = vec![StudioParagraphComposition {
            text_id: id("STXT", 1),
            paragraph_start: second,
            settings: StudioCompositionSettings {
                composition_scope: Some(StudioCompositionScope::SingleLine),
                hyphenation_zone: Some(len(z)),
                ..Default::default()
            },
        }];
        let p = run(TEXT, 60.0, Composer::FirstFit, &styled(&s)).unwrap();
        p.frames[0].lines.iter().find(|l| l.paragraph == 1).unwrap().clone()
    };
    assert!(zone(36.0).hyphenated, "zone 36 < 48 pt left: hyphenated");
    assert!(!zone(50.0).hyphenated, "zone 50 >= 48 pt left: whole word moves");
    // Explicit Single-line request with the same values: identical paragraph 2 line.
    let explicit = TypesetOptions { justify: Some(JustifyParams::default()), hyphenation: Some(hy()), ..Default::default() };
    let e = run("aa typographic\n", 60.0, Composer::AdobeSingleLine(AdobeParams { hyphenation_zone_pt: 36.0 }), &explicit).unwrap();
    let w = zone(36.0);
    assert_eq!((e.frames[0].lines[0].glyph_range.len(), e.frames[0].lines[0].hyphenated, e.frames[0].lines[0].width_pt), (w.glyph_range.len(), w.hyphenated, w.width_pt));
    // An override naming no paragraph start is refused, never ignored.
    let mut stray = set.clone();
    stray.paragraph_composition = vec![StudioParagraphComposition { text_id: id("STXT", 1), paragraph_start: 3, settings: scope(StudioCompositionScope::SingleLine) }];
    assert_eq!(run(TEXT, 60.0, Composer::FirstFit, &styled(&stray)).unwrap_err(), LayoutError::InvalidInput);
}

/// Hyphenation settings: on/off against the request, last word, limits no word can meet.
#[test]
fn hyphenation_settings_drive_the_breakers() {
    let text = "aa typographic\n";
    let first = |settings: StudioCompositionSettings, request: Option<HyphenationParams>| {
        let set = records(StudioTextAlign::Left, settings);
        let o = TypesetOptions { hyphenation: request, ..styled(&set) };
        run(text, 60.0, Composer::FirstFit, &o).unwrap().frames[0].lines[0].clone()
    };
    let on = StudioCompositionSettings { hyphenation: Some(true), ..Default::default() };
    assert!(first(on.clone(), None).hyphenated, "style turns hyphenation on");
    assert!(!first(StudioCompositionSettings { hyphenation: Some(false), ..Default::default() }, Some(hy())).hyphenated, "style turns it off");
    assert!(!first(StudioCompositionSettings { hyphenate_last_word: Some(false), ..on.clone() }, None).hyphenated, "last word kept whole");
    assert!(!first(StudioCompositionSettings { hyphenate_words_longer_than: Some(100), ..on.clone() }, None).hyphenated, "minimum no word meets");
    assert!(!first(StudioCompositionSettings { hyphenate_after_first: Some(40), hyphenate_before_last: Some(40), ..on.clone() }, None).hyphenated);
    // The hyphen advance comes from the request when it states one (8 pt here).
    let line = first(on.clone(), Some(HyphenationParams { hyphen_advance_pt: 8.0, ..hy() }));
    assert!(line.hyphenated);
    assert_eq!((line.glyph_range.clone(), line.width_pt), (0..7, 6.0 * 7.0 + 8.0), "aa typo- with an 8 pt hyphen");
}

/// Exact mapping of every setting onto the composer parameters (percent / 100, counts, the zone,
/// the shaped space as letter-spacing reference, ladder saturation, the hyphen estimate).
#[test]
fn paragraph_params_map_every_setting() {
    let settings = StudioCompositionSettings {
        composition_scope: Some(StudioCompositionScope::SingleLine),
        composition_engine: Some(StudioCompositionEngine::LatinCjk),
        hyphenation: Some(true),
        hyphenate_words_longer_than: Some(7),
        hyphenate_after_first: Some(3),
        hyphenate_before_last: Some(4),
        hyphenate_ladder_limit: Some(300),
        hyphen_weight: Some(5),
        hyphenation_zone: Some(Length { value: 0.5, unit: GeometryUnit::In }),
        hyphenate_capitalized_words: Some(false),
        hyphenate_last_word: Some(false),
        hyphenate_across_columns: Some(false),
        word_spacing_minimum: Some(50.0),
        word_spacing_desired: Some(100.0),
        word_spacing_maximum: Some(200.0),
        letter_spacing_minimum: Some(-5.0),
        letter_spacing_desired: Some(0.0),
        letter_spacing_maximum: Some(5.0),
        glyph_scaling_minimum: Some(90.0),
        glyph_scaling_desired: Some(100.0),
        glyph_scaling_maximum: Some(110.0),
        single_word_justification: Some(StudioSingleWordJustification::CenterAlign),
        auto_leading_amount: Some(120.0),
    };
    let c = ParagraphComposition { settings, space_advance_pt: Some(3.0), hyphen_advance_pt: None };
    let o = TypesetOptions::default();
    let p = paragraph_params(Some(&c), 12.0, Composer::FirstFit, SpaceParams::default(), &o).unwrap();
    assert_eq!(p.composer, Composer::AdobeSingleLine(AdobeParams { hyphenation_zone_pt: 36.0 }));
    assert_eq!(p.justify.word_spacing, Spacing { min: 0.5, desired: 1.0, max: 2.0 });
    assert_eq!(p.justify.letter_spacing, Spacing { min: -0.05, desired: 0.0, max: 0.05 });
    assert_eq!(p.justify.reference_space_pt, 3.0);
    assert_eq!(p.justify.glyph_scaling, Spacing { min: 0.9, desired: 1.0, max: 1.1 });
    assert_eq!(p.space, SpaceParams { stretch: 1.0, shrink: 0.5 });
    let h = p.hyphenation.unwrap();
    assert_eq!((h.min_word_len, h.min_before, h.min_after, h.consecutive_limit, h.hyphenate_capitalized), (7, 3, 4, 255, false));
    assert_eq!(h.hyphen_advance_pt, 4.0, "estimate: font size / 3");
    assert_eq!((p.hyphenate_last_word, p.single_word), (false, Some(StudioSingleWordJustification::CenterAlign)));
    // The caller's hyphen advance wins over the paragraph glyph and the estimate.
    let c2 = ParagraphComposition { hyphen_advance_pt: Some(5.0), ..c.clone() };
    assert_eq!(paragraph_params(Some(&c2), 12.0, Composer::FirstFit, SpaceParams::default(), &o).unwrap().hyphenation.unwrap().hyphen_advance_pt, 5.0);
    let with = TypesetOptions { hyphen_advance_pt: Some(7.0), ..Default::default() };
    assert_eq!(paragraph_params(Some(&c2), 12.0, Composer::FirstFit, SpaceParams::default(), &with).unwrap().hyphenation.unwrap().hyphen_advance_pt, 7.0);
    // every_line keeps the zone; no scope keeps a non-Adobe request composer.
    let every = ParagraphComposition { settings: StudioCompositionSettings { composition_scope: Some(StudioCompositionScope::EveryLine), ..c.settings.clone() }, ..c.clone() };
    assert_eq!(paragraph_params(Some(&every), 12.0, Composer::FirstFit, SpaceParams::default(), &o).unwrap().composer, Composer::AdobeParagraph(AdobeParams { hyphenation_zone_pt: 36.0 }));
    let kp = Composer::KnuthPlass(KnuthPlassParams::default());
    let silent = ParagraphComposition { settings: StudioCompositionSettings { hyphenation: Some(true), ..Default::default() }, ..c.clone() };
    assert_eq!(paragraph_params(Some(&silent), 12.0, kp, SpaceParams::default(), &o).unwrap().composer, kp);
}

/// Hostile and unsupported settings are typed refusals: engines layout does not compose, triples
/// disordered after inheritance, a NaN or negative zone, a NaN request hyphen advance.
#[test]
fn unsupported_and_out_of_range_settings_are_refused() {
    let refuse = |settings: StudioCompositionSettings| {
        let set = records(StudioTextAlign::Justified, settings);
        run(TEXT, 82.0, Composer::FirstFit, &styled(&set)).unwrap_err()
    };
    for engine in [StudioCompositionEngine::WorldReady, StudioCompositionEngine::Adornment] {
        assert_eq!(refuse(StudioCompositionSettings { composition_engine: Some(engine), ..Default::default() }), LayoutError::UnsupportedComposition);
    }
    assert_eq!(refuse(StudioCompositionSettings { word_spacing_desired: Some(70.0), ..Default::default() }), LayoutError::InvalidInput, "desired below the default minimum 80");
    assert_eq!(refuse(StudioCompositionSettings { glyph_scaling_maximum: Some(300.0), ..Default::default() }), LayoutError::InvalidInput);
    assert_eq!(refuse(StudioCompositionSettings { hyphenation_zone: Some(len(f64::NAN)), ..Default::default() }), LayoutError::InvalidGeometry);
    assert_eq!(refuse(StudioCompositionSettings { hyphenation_zone: Some(len(-1.0)), ..Default::default() }), LayoutError::InvalidInput);
    assert_eq!(refuse(StudioCompositionSettings { auto_leading_amount: Some(0.0), ..Default::default() }), LayoutError::InvalidGeometry);
    let set = records(StudioTextAlign::Left, scope(StudioCompositionScope::SingleLine));
    let o = TypesetOptions { hyphen_advance_pt: Some(f64::NAN), ..styled(&set) };
    assert_eq!(run(TEXT, 82.0, Composer::FirstFit, &o).unwrap_err(), LayoutError::InvalidInput);
}

/// Single-word justification (STU-TYP-146) and auto leading (STU-TYP-148).
#[test]
fn single_word_justification_and_auto_leading() {
    // 50 pt: "aaaaaaa " (42 pt) is a one-word line of a justified paragraph, "bb" the last line.
    let text = "aaaaaaa bb\n";
    let first = |swj: Option<StudioSingleWordJustification>| {
        let set = records(StudioTextAlign::Justified, StudioCompositionSettings { single_word_justification: swj, hyphenation: Some(false), ..Default::default() });
        run(text, 50.0, Composer::FirstFit, &styled(&set)).unwrap().frames[0].lines[0].clone()
    };
    let full = first(Some(StudioSingleWordJustification::FullyJustified));
    assert!(full.justification.is_some() && full.x_start_pt == 0.0, "{full:?}");
    for (swj, x) in [
        (StudioSingleWordJustification::LeftAlign, 0.0),
        (StudioSingleWordJustification::CenterAlign, 4.0),
        (StudioSingleWordJustification::RightAlign, 8.0),
    ] {
        let l = first(Some(swj));
        assert_eq!((l.x_start_pt, l.justification), (x, None), "{swj:?}");
    }
    // Auto leading 150 % of 12 pt = 18 pt between baselines; without it the shaped 12 pt.
    let lead = |amount: Option<f64>| {
        let set = records(StudioTextAlign::Left, StudioCompositionSettings { auto_leading_amount: amount, ..Default::default() });
        let p = run("aaaa bbbb cccc\n", 30.0, Composer::FirstFit, &styled(&set)).unwrap();
        (p.frames[0].lines[0].baseline_y_pt, p.frames[0].lines[1].baseline_y_pt)
    };
    assert_eq!(lead(Some(150.0)), (10.0, 28.0));
    assert_eq!(lead(None), (10.0, 22.0));
}
