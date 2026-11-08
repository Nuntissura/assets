//! SMB-LAYOUT-COMPOSER-WIRING: IDML paragraph composition (style definitions and local
//! `ParagraphStyleRange` attributes) mapped into Folio type-style records, typed losses for every
//! value that does not map, hostile XML (escapes, comments and decoys, case and whitespace, DTDs),
//! and the IDML reflow composing from the mapped records. Attribute names as InDesign 21.6 writes
//! them in the D5 oracle packages (module docs of `idml_composition.rs`); the fixture is generated.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::*;
use hsk_studio_interop_layout::layout::{ShapedParagraph, SourceSpan, StyleSource, TypesetOptions, resolve_style_composition};
use hsk_studio_interop_layout::*;
mod common;

const NS: &str = "http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging";
const CSR: &str = "<CharacterStyleRange AppliedCharacterStyle=\"CharacterStyle/$ID/[No character style]\">";
const ROOT_ATTRS: &str = "Composer=\"HL Composer\" Hyphenation=\"true\" HyphenateWordsLongerThan=\"5\" HyphenateAfterFirst=\"2\" HyphenateBeforeLast=\"2\" \
HyphenateLadderLimit=\"3\" HyphenWeight=\"5\" HyphenationZone=\"36\" HyphenateCapitalizedWords=\"true\" HyphenateLastWord=\"true\" \
HyphenateAcrossColumns=\"true\" MinimumWordSpacing=\"80\" DesiredWordSpacing=\"100\" MaximumWordSpacing=\"133\" MinimumLetterSpacing=\"0\" \
DesiredLetterSpacing=\"0\" MaximumLetterSpacing=\"0\" MinimumGlyphScaling=\"100\" DesiredGlyphScaling=\"100\" MaximumGlyphScaling=\"100\" \
AutoLeading=\"120\" SingleWordJustification=\"FullyJustified\" PointSize=\"12\"";

fn wrap(element: &str, body: &str) -> String {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<idPkg:{element} xmlns:idPkg=\"{NS}\" DOMVersion=\"16.0\">\n{body}\n</idPkg:{element}>\n")
}
fn rect_path(x0: f64, y0: f64, x1: f64, y1: f64) -> String {
    let pts: String = [(x0, y0), (x0, y1), (x1, y1), (x1, y0)]
        .iter()
        .map(|(x, y)| format!("<PathPointType Anchor=\"{x} {y}\" LeftDirection=\"{x} {y}\" RightDirection=\"{x} {y}\"/>"))
        .collect();
    format!("<Properties><PathGeometry><GeometryPathType PathOpen=\"false\"><PathPointArray>{pts}</PathPointArray></GeometryPathType></PathGeometry></Properties>")
}
fn para(style: &str, local: &str, text: &str, br: bool) -> String {
    let br = if br { "<Br/>" } else { "" };
    format!("<ParagraphStyleRange AppliedParagraphStyle=\"ParagraphStyle/{style}\"{local}>{CSR}<Content>{text}</Content>{br}</CharacterStyleRange></ParagraphStyleRange>")
}
fn based_on(parent: &str) -> String {
    format!("<Properties><BasedOn type=\"string\">{parent}</BasedOn></Properties>")
}
/// Story text "aa typographic\naaaa bbbb\ncc dd\nee\nff\ngg": paragraph starts 0, 15, 25, 31, 34, 37.
fn styles_xml() -> String {
    wrap(
        "Styles",
        &format!(
            "<RootParagraphStyleGroup Self=\"u81\">\
<ParagraphStyle Self=\"ParagraphStyle/$ID/[No paragraph style]\" Name=\"$ID/[No paragraph style]\" {ROOT_ATTRS}/>\
<ParagraphStyle Self=\"ParagraphStyle/$ID/NormalParagraphStyle\" Name=\"$ID/NormalParagraphStyle\" NextStyle=\"ParagraphStyle/$ID/NormalParagraphStyle\">{}</ParagraphStyle>\
<!-- <ParagraphStyle Self=\"ParagraphStyle/Body\" Composer=\"HL Composer\"/> -->\
<ParagraphStyleGroup Self=\"g1\" Name=\"Text\">\
<ParagraphStyle Self=\"ParagraphStyle/Body\" Name=\"Body\" Composer=\"HL&#32;Single\" MaximumWordSpacing=\" 150 \" HyphenationZone=\"18\">\
<Properties><BasedOn type=\"object\">ParagraphStyle/$ID/NormalParagraphStyle</BasedOn></Properties></ParagraphStyle>\
</ParagraphStyleGroup>\
<ParagraphStyle Self=\"ParagraphStyle/Hostile\" Name=\"Hostile\" xmlns:x=\"urn:decoy\" x:Composer=\"HL Single\" composer=\"HL Single\" \
Composer=\"Adobe World-Ready Paragraph Composer\" MinimumWordSpacing=\"2000\" HyphenateWordsLongerThan=\"2.5\" HyphenateAfterFirst=\"-1\" \
Hyphenation=\"yes\" SingleWordJustification=\"LeftJustified\" AutoLeading=\"NaN\" HyphenWeight=\"101\" MinimumGlyphScaling=\"120\" \
DesiredGlyphScaling=\"100\" HyphenateLastWord=\" false\"><Decoy Composer=\"HL Single\"/></ParagraphStyle>\
<ParagraphStyle Self=\"ParagraphStyle/Loop A\" Name=\"Loop A\">{}</ParagraphStyle>\
<ParagraphStyle Self=\"ParagraphStyle/Loop B\" Name=\"Loop B\" Composer=\"HL Single\">{}</ParagraphStyle>\
<ParagraphStyle Self=\"ParagraphStyle/Orphan\" Name=\"Orphan\">{}</ParagraphStyle>\
<ParagraphStyle Self=\"ParagraphStyle/Unused\" Name=\"Unused\" Composer=\"HL Single\"/>\
</RootParagraphStyleGroup>",
            based_on("$ID/[No paragraph style]"),
            based_on("Loop B"),
            based_on("Loop A"),
            based_on("Nowhere"),
        ),
    )
}
fn package(styles: &str) -> Vec<u8> {
    let paras = [
        para("Body", "", "aa typographic", true),
        para("Body", " Composer=\"HL Composer\" Hyphenation=\"false\"", "aaaa bbbb", true),
        para("$ID/[No paragraph style]", " MinimumWordSpacing=\"85\"", "cc dd", true),
        para("Hostile", "", "ee", true),
        para("Loop A", "", "ff", true),
        para("Orphan", "", "gg", false),
    ];
    package_with(styles, &paras.concat())
}
/// A one-story package: `paragraphs` (`para` elements) in a 60 x 390 frame on a 300 x 400 page.
fn package_with(styles: &str, paragraphs: &str) -> Vec<u8> {
    let designmap = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<Document xmlns:idPkg=\"{NS}\" DOMVersion=\"16.0\" Self=\"d\">\n\
<idPkg:Styles src=\"Resources/Styles.xml\"/>\n<idPkg:Preferences src=\"Resources/Preferences.xml\"/>\n\
<idPkg:Spread src=\"Spreads/Spread_us1.xml\"/>\n<idPkg:Story src=\"Stories/Story_ua1.xml\"/>\n</Document>\n"
    );
    let prefs = wrap(
        "Preferences",
        "<DocumentPreference PageWidth=\"300\" PageHeight=\"400\" FacingPages=\"false\"/>\n<MarginPreference Top=\"0\" Bottom=\"0\" Left=\"0\" Right=\"0\"/>",
    );
    let spread = wrap(
        "Spread",
        &format!(
            "<Spread Self=\"us1\" PageCount=\"1\" ItemTransform=\"1 0 0 1 0 0\">\
<Page Self=\"up1\" Name=\"1\" GeometricBounds=\"0 0 400 300\" ItemTransform=\"1 0 0 1 0 0\"/>\
<TextFrame Self=\"uf1\" ParentStory=\"ua1\" PreviousTextFrame=\"n\" NextTextFrame=\"n\" ItemTransform=\"1 0 0 1 0 0\">{}</TextFrame></Spread>",
            rect_path(0.0, 0.0, 60.0, 390.0)
        ),
    );
    let story = wrap(
        "Story",
        &format!(
            "<Story Self=\"ua1\"><StoryPreference StoryDirection=\"LeftToRightDirection\"/>{paragraphs}</Story>"
        ),
    );
    let parts = [
        ("mimetype", IDML_MIMETYPE.to_owned()),
        ("META-INF/container.xml", "<?xml version=\"1.0\" encoding=\"UTF-8\"?><container/>".to_owned()),
        ("designmap.xml", designmap),
        ("Resources/Preferences.xml", prefs),
        ("Resources/Styles.xml", styles.to_owned()),
        ("Spreads/Spread_us1.xml", spread),
        ("Stories/Story_ua1.xml", story),
    ];
    write_idml(&IdmlPackage { parts: parts.into_iter().map(|(n, b)| IdmlPart::new(n.into(), b.into_bytes())).collect() }).expect("write fixture")
}
fn import(bytes: &[u8]) -> (IdmlImport, hsk_studio_interop_layout::Result<TypeStyleImport>) {
    let (limits, cancel) = (Limits::default(), CancellationToken::default());
    let mut mint = SequentialIdMint::new(3);
    let i = import_idml(bytes, &limits, &mut mint, &cancel).expect("import");
    let t = import_type_styles(&i, &limits, &mut mint, &cancel);
    (i, t)
}
/// Folio id of the paragraph style with this IDML `Self`.
fn style_id(i: &IdmlImport, t: &TypeStyleImport, idml: &str) -> String {
    i.proposal.style_refs.iter().chain(&t.new_styles).find(|s| s.idml_name == idml).map(|s| s.folio.clone()).unwrap_or_else(|| panic!("{idml}"))
}
fn has_loss(t: &TypeStyleImport, element: &str, code: &str) -> bool {
    t.loss.iter().any(|e| e.element == element && e.code == code)
}
fn root_settings() -> StudioCompositionSettings {
    StudioCompositionSettings {
        composition_scope: Some(StudioCompositionScope::EveryLine),
        composition_engine: Some(StudioCompositionEngine::LatinCjk),
        hyphenation: Some(true),
        hyphenate_words_longer_than: Some(5),
        hyphenate_after_first: Some(2),
        hyphenate_before_last: Some(2),
        hyphenate_ladder_limit: Some(3),
        hyphen_weight: Some(5),
        hyphenation_zone: Some(Length { value: 36.0, unit: GeometryUnit::Pt }),
        hyphenate_capitalized_words: Some(true),
        hyphenate_last_word: Some(true),
        hyphenate_across_columns: Some(true),
        word_spacing_minimum: Some(80.0),
        word_spacing_desired: Some(100.0),
        word_spacing_maximum: Some(133.0),
        letter_spacing_minimum: Some(0.0),
        letter_spacing_desired: Some(0.0),
        letter_spacing_maximum: Some(0.0),
        glyph_scaling_minimum: Some(100.0),
        glyph_scaling_desired: Some(100.0),
        glyph_scaling_maximum: Some(100.0),
        single_word_justification: Some(StudioSingleWordJustification::FullyJustified),
        auto_leading_amount: Some(120.0),
        ..Default::default()
    }
}

/// Styles, `BasedOn` ancestry, style composition and local overrides arrive in the records;
/// escaped values decode before matching, comments and decoys (a commented-out definition, a
/// namespaced or lower-case attribute, a child element) never map.
#[test]
fn idml_paragraph_composition_maps_into_type_style_records() {
    let (i, t) = import(&package(&styles_xml()));
    let t = t.expect("type styles");
    let r = &t.records;
    // Applied: Body, Hostile, Loop A, Orphan; minted ancestors: Normal, the root, Loop B. Unused
    // and the commented-out definition do not appear.
    let mut names: Vec<&str> = r.styles.iter().map(|s| s.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["$ID/NormalParagraphStyle", "$ID/[No paragraph style]", "Body", "Hostile", "Loop A", "Loop B", "Orphan"]);
    let mut minted: Vec<&str> = t.new_styles.iter().map(|s| s.idml_name.as_str()).collect();
    minted.sort_unstable();
    assert_eq!(minted, ["ParagraphStyle/$ID/NormalParagraphStyle", "ParagraphStyle/$ID/[No paragraph style]", "ParagraphStyle/Loop B"]);
    let (body, normal, root) = (
        style_id(&i, &t, "ParagraphStyle/Body"),
        style_id(&i, &t, "ParagraphStyle/$ID/NormalParagraphStyle"),
        style_id(&i, &t, "ParagraphStyle/$ID/[No paragraph style]"),
    );
    assert_eq!(r.style(&body).unwrap().based_on.as_deref(), Some(normal.as_str()), "BasedOn as ParagraphStyle/... object");
    assert_eq!(r.style(&normal).unwrap().based_on.as_deref(), Some(root.as_str()), "BasedOn as a bare name");
    // Body: the escaped "HL&#32;Single" decodes to the Single-line Composer; " 150 " is a number.
    let own = r.style_composition(&body).expect("body settings");
    assert_eq!(
        own,
        &StudioCompositionSettings {
            composition_scope: Some(StudioCompositionScope::SingleLine),
            composition_engine: Some(StudioCompositionEngine::LatinCjk),
            hyphenation_zone: Some(Length { value: 18.0, unit: GeometryUnit::Pt }),
            word_spacing_maximum: Some(150.0),
            ..Default::default()
        }
    );
    assert_eq!(r.style_composition(&root), Some(&root_settings()));
    assert!(r.style_composition(&normal).is_none(), "Normal states nothing");
    // The chain resolves root first (layout's resolver).
    let resolved = resolve_style_composition(r, &body).unwrap();
    assert_eq!(resolved, root_settings().overlaid(own));
    // Local overrides: paragraph 1 (Composer + Hyphenation), paragraph 2 in [No paragraph style]
    // (the root's settings plus its own MinimumWordSpacing, approximated); paragraph 0 has none.
    let story = i.proposal.stories[0].story.story_id.clone();
    assert!(r.paragraph_composition(&story, 0).is_none());
    assert_eq!(
        r.paragraph_composition(&story, 15),
        Some(&StudioCompositionSettings {
            composition_scope: Some(StudioCompositionScope::EveryLine),
            composition_engine: Some(StudioCompositionEngine::LatinCjk),
            hyphenation: Some(false),
            ..Default::default()
        })
    );
    let mut cc = root_settings();
    cc.word_spacing_minimum = Some(85.0);
    assert_eq!(r.paragraph_composition(&story, 25), Some(&cc));
    assert!(has_loss(&t, "ParagraphStyleRange", "root_style_settings_as_paragraph_override"));
    assert_eq!(r.paragraph_composition.len(), 2);
    // Loops and missing parents are typed losses with the link dropped; the set validates.
    assert!(has_loss(&t, "ParagraphStyle@BasedOn", "based_on_cycle"));
    assert!(has_loss(&t, "ParagraphStyle@BasedOn", "based_on_missing"));
    assert_eq!(r.style(&style_id(&i, &t, "ParagraphStyle/Orphan")).unwrap().based_on, None);
    let loop_links =
        ["ParagraphStyle/Loop A", "ParagraphStyle/Loop B"].map(|k| r.style(&style_id(&i, &t, k)).unwrap().based_on.is_some());
    assert_eq!(loop_links.iter().filter(|b| **b).count(), 1, "one link of the cycle kept");
    assert!(has_loss(&t, "ParagraphStyle", "paragraph_style_attributes_not_mapped"), "PointSize is not composition");
    assert!(
        validate_type_style_records(r, i.proposal.snapshot.document(), Budget::default(), &CancellationToken::default(), &Unresolved).is_ok()
    );
    // Wire: byte-identical round trip.
    let bytes = serde_json::to_vec(r).unwrap();
    let back: StudioTypeStyleRecords = serde_json::from_slice(&bytes).unwrap();
    assert_eq!((&back, serde_json::to_vec(&back).unwrap()), (r, bytes));
    // Deterministic: a second import yields the same records.
    assert_eq!(import(&package(&styles_xml())).1.unwrap().records, t.records);
}

/// Every value that does not map is a typed loss and the field stays unset: never a clamp, never a
/// default, never matched by case or by surrounding whitespace.
#[test]
fn unmappable_composition_values_are_typed_losses() {
    let (i, t) = import(&package(&styles_xml()));
    let t = t.expect("type styles");
    let hostile = style_id(&i, &t, "ParagraphStyle/Hostile");
    assert!(t.records.style_composition(&hostile).is_none(), "nothing of Hostile maps: {:?}", t.records.style_composition(&hostile));
    for (attr, code) in [
        ("Composer", "composer_not_mapped"),
        ("MinimumWordSpacing", "composition_value_out_of_range"),
        ("HyphenateWordsLongerThan", "composition_value_out_of_range"),
        ("HyphenateAfterFirst", "composition_value_out_of_range"),
        ("HyphenWeight", "composition_value_out_of_range"),
        ("Hyphenation", "composition_value_invalid"),
        ("SingleWordJustification", "composition_value_invalid"),
        ("AutoLeading", "composition_value_invalid"),
        ("HyphenateLastWord", "composition_value_invalid"),
        ("GlyphScaling", "composition_spacing_order"),
    ] {
        assert!(has_loss(&t, &format!("ParagraphStyle@{attr}"), code), "{attr} {code}: {:#?}", t.loss);
    }
    // A styles part without the applied definitions: typed loss per style, no settings invented.
    let (_, t2) = import(&package(&wrap("Styles", "<RootParagraphStyleGroup Self=\"u81\"/>")));
    let t2 = t2.expect("empty styles part");
    assert!(has_loss(&t2, "ParagraphStyle", "style_definition_missing"));
    assert!(t2.records.style_composition.is_empty());
}

/// Hostile packages: a DTD (entity expansion) in Styles.xml is refused before any read; more
/// paragraph styles than Folio holds is a budget refusal, not a truncation.
#[test]
fn hostile_styles_parts_are_refused() {
    let dtd = format!(
        "<?xml version=\"1.0\"?><!DOCTYPE x [<!ENTITY c \"HL Single\">]><idPkg:Styles xmlns:idPkg=\"{NS}\"><RootParagraphStyleGroup>\
<ParagraphStyle Self=\"ParagraphStyle/Body\" Composer=\"&c;\"/></RootParagraphStyleGroup></idPkg:Styles>"
    );
    let (_, t) = import(&package(&dtd));
    assert_eq!(t.unwrap_err(), IdmlError::EntityDenied);
    let many: String = (0..=MAX_TYPE_STYLES).map(|n| format!("<ParagraphStyle Self=\"ParagraphStyle/S{n}\" Name=\"S{n}\"/>")).collect();
    let (_, t) = import(&package(&wrap("Styles", &format!("<RootParagraphStyleGroup Self=\"u81\">{many}</RootParagraphStyleGroup>"))));
    assert_eq!(t.unwrap_err(), IdmlError::BudgetExceeded("paragraph_styles"));
}

/// The IDML reflow composes from the mapped records: "aa typographic" in 60 pt with Body's
/// Single-line Composer, hyphenation on (root) and zone 18 is hyphenated; the plain reflow
/// (FirstFit, no styles) leaves it whole and overfull. Paragraph 1's local `Hyphenation="false"`
/// keeps it unhyphenated.
#[test]
fn idml_reflow_composes_with_the_mapped_composer() {
    let (i, t) = import(&package(&styles_xml()));
    let t = t.expect("type styles");
    let shaper = FixedAdvanceShaper { advance_pt: 6.0, line_height_pt: 12.0, ascent_pt: 10.0 };
    let cancel = CancellationToken::default();
    let plain = reflow_import(&i, &shaper, &ReflowOptions::default(), &cancel).unwrap();
    let opts = TypesetOptions {
        styles: Some(StyleSource { records: &t.records, default_font_size_pt: 12.0 }),
        hyphen_advance_pt: Some(6.0),
        ..TypesetOptions::default()
    };
    let styled = reflow_import_typeset(&i, &shaper, &ReflowOptions::default(), &opts, &cancel).unwrap();
    let lines = |r: &[ThreadReflow]| r[0].outcome.as_ref().expect("placed").frames[0].lines.clone();
    let (p, s) = (lines(&plain), lines(&styled));
    assert!(!p[0].hyphenated && p.iter().any(|l| l.overfull), "plain: {:?}", p[0]);
    assert!(s[0].hyphenated && !s[0].overfull, "styled: {:?}", s[0]);
    assert!(s.iter().filter(|l| l.paragraph == 1).all(|l| !l.hyphenated));
    // Auto leading 120 % of the 12 pt default size: 14.4 pt between baselines.
    assert!((s[1].baseline_y_pt - s[0].baseline_y_pt - 14.4).abs() < 1e-9, "{} {}", s[0].baseline_y_pt, s[1].baseline_y_pt);
    // Same input, same placement.
    assert_eq!(reflow_import_typeset(&i, &shaper, &ReflowOptions::default(), &opts, &cancel).unwrap(), styled);
}

/// Lane SMB-TYPE-INDENT-SPACING-1 fixture: the root states the six STU-TYP-185 attributes at 0 (as
/// every InDesign-written root does, ID-03), "Ind" states them set, "Wild" states hostile values; a
/// lower-case decoy and a `TableStyle@SpaceBefore` (not a paragraph attribute) never map. Story
/// "aaaa bbbb cccc dddd\neeee\nff" (paragraph starts 0, 20, 25); paragraph 1 overrides
/// `LeftIndent` locally.
const INDENT_ZEROS: &str = "LeftIndent=\"0\" RightIndent=\"0\" FirstLineIndent=\"0\" LastLineIndent=\"0\" SpaceBefore=\"0\" SpaceAfter=\"0\"";
fn indent_styles_xml() -> String {
    wrap(
        "Styles",
        &format!(
            "<RootParagraphStyleGroup Self=\"u81\">\
<ParagraphStyle Self=\"ParagraphStyle/$ID/[No paragraph style]\" Name=\"$ID/[No paragraph style]\" {INDENT_ZEROS}/>\
<ParagraphStyle Self=\"ParagraphStyle/Ind\" Name=\"Ind\" LeftIndent=\" 12 \" RightIndent=\"6\" FirstLineIndent=\"6\" LastLineIndent=\"0\" \
SpaceBefore=\"5\" SpaceAfter=\"3\" leftindent=\"99\"/>\
<ParagraphStyle Self=\"ParagraphStyle/Wild\" Name=\"Wild\" LeftIndent=\"1e308\" RightIndent=\"-1e308\" FirstLineIndent=\"NaN\" \
LastLineIndent=\"inf\" SpaceBefore=\"1e999\" SpaceAfter=\"-INF\"/>\
</RootParagraphStyleGroup><RootTableStyleGroup Self=\"u90\"><TableStyle Self=\"TableStyle/x\" Name=\"x\" SpaceBefore=\"4\"/></RootTableStyleGroup>"
        ),
    )
}
fn indent_package() -> Vec<u8> {
    let paras = [para("Ind", "", "aaaa bbbb cccc dddd", true), para("Ind", " LeftIndent=\"20\"", "eeee", true), para("Wild", "", "ff", false)];
    package_with(&indent_styles_xml(), &paras.concat())
}
fn pt(v: f64) -> Option<Length> {
    Some(Length { value: v, unit: GeometryUnit::Pt })
}

/// `LeftIndent`, `RightIndent`, `FirstLineIndent`, `LastLineIndent`, `SpaceBefore` and
/// `SpaceAfter` map into the Folio settings as points (signed, finite, trimmed; exact names only),
/// the root's zeros are stored as zeros, a local attribute overrides its style, and non-finite or
/// unparseable values are `composition_value_invalid` with the field unset; `1e308` is stored.
#[test]
fn idml_indents_and_spacing_map_as_points() {
    let (i, t) = import(&indent_package());
    let t = t.expect("type styles");
    let r = &t.records;
    let ind = r.style_composition(&style_id(&i, &t, "ParagraphStyle/Ind")).expect("Ind settings");
    let set = StudioCompositionSettings { left_indent: pt(12.0), right_indent: pt(6.0), first_line_indent: pt(6.0), last_line_indent: pt(0.0), space_before: pt(5.0), space_after: pt(3.0), ..Default::default() };
    assert_eq!(ind, &set);
    let wild = r.style_composition(&style_id(&i, &t, "ParagraphStyle/Wild")).expect("Wild settings");
    assert_eq!(wild, &StudioCompositionSettings { left_indent: pt(1e308), right_indent: pt(-1e308), ..Default::default() });
    for attr in ["FirstLineIndent", "LastLineIndent", "SpaceBefore", "SpaceAfter"] {
        assert!(has_loss(&t, &format!("ParagraphStyle@{attr}"), "composition_value_invalid"), "{attr}: {:#?}", t.loss);
    }
    let story = i.proposal.stories[0].story.story_id.clone();
    assert_eq!(r.paragraph_composition(&story, 20), Some(&StudioCompositionSettings { left_indent: pt(20.0), ..Default::default() }));
    assert!(r.paragraph_composition(&story, 0).is_none());
    // Exactly these names; no attribute is mapped for space_between_same_style.
    for name in ["LeftIndent", "RightIndent", "FirstLineIndent", "LastLineIndent", "SpaceBefore", "SpaceAfter"] {
        assert!(COMPOSITION_ATTRIBUTES.contains(&name), "{name}");
    }
    assert!(!COMPOSITION_ATTRIBUTES.iter().any(|a| a.to_ascii_lowercase().contains("same")));
    // The root's zeros are stored as zeros (ID-03 writes them on every root style).
    let zeros = StudioCompositionSettings { left_indent: pt(0.0), right_indent: pt(0.0), first_line_indent: pt(0.0), last_line_indent: pt(0.0), space_before: pt(0.0), space_after: pt(0.0), ..Default::default() };
    let root_id = i.proposal.style_refs.iter().chain(&t.new_styles).find(|s| s.idml_name == "ParagraphStyle/$ID/[No paragraph style]").map(|s| s.folio.clone());
    if let Some(root) = root_id {
        assert_eq!(r.style_composition(&root), Some(&zeros));
    }
    assert!(validate_type_style_records(r, i.proposal.snapshot.document(), Budget::default(), &CancellationToken::default(), &Unresolved).is_ok());
}

/// End to end: the IDML reflow composes the mapped indents and spacing (narrowed lines, the gap
/// between paragraphs, the hostile 1e308 indent overset), and PDF export draws every line's first
/// glyph at the indented x.
#[test]
fn idml_indents_compose_and_export_to_pdf_at_the_moved_x() {
    let (i, t) = import(&indent_package());
    let t = t.expect("type styles");
    let shaper = FixedAdvanceShaper { advance_pt: 6.0, line_height_pt: 12.0, ascent_pt: 10.0 };
    let cancel = CancellationToken::default();
    let opts = TypesetOptions { styles: Some(StyleSource { records: &t.records, default_font_size_pt: 12.0 }), ..TypesetOptions::default() };
    let r = reflow_import_typeset(&i, &shaper, &ReflowOptions::default(), &opts, &cancel).unwrap();
    let placement = r[0].outcome.as_ref().expect("placed").clone();
    let lines = &placement.frames[0].lines;
    let got: Vec<(f64, f64, u64)> = lines.iter().map(|l| (l.x_start_pt, l.available_pt, l.source.start)).collect();
    // 60 pt frame: first line 12 + 6 in, 36 wide; then 12 in, 42 wide; "eeee" 20 + 6 in, 28 wide.
    assert_eq!(got, vec![(18.0, 36.0, 0), (12.0, 42.0, 5), (12.0, 42.0, 10), (12.0, 42.0, 15), (26.0, 28.0, 20)]);
    let b0 = lines[0].baseline_y_pt;
    let gaps: Vec<f64> = lines.iter().map(|l| l.baseline_y_pt - b0).collect();
    assert_eq!(gaps, vec![0.0, 12.0, 24.0, 36.0, 56.0], "3 after + 5 before between the paragraphs");
    assert_eq!(placement.overset.map(|o| o.start), Some(25), "the 1e308 indent leaves no width");

    // PDF: one glyph id for letters, one for the space; every line's first glyph at frame x + x_start.
    let ps = &i.proposal.stories[0];
    let text = ps.story.inline_text().unwrap();
    let paragraphs: Vec<ShapedParagraph> = ps
        .story
        .paragraphs
        .iter()
        .map(|p| {
            let end = if text.as_bytes().get(p.end as usize - 1) == Some(&b'\n') { p.end - 1 } else { p.end };
            ShapedParagraph::from_fixed_advance(text, SourceSpan { start: p.start, end }, 6.0, 12.0, 10.0).unwrap()
        })
        .collect();
    let space_gid = 1682u16;
    let glyphs: Vec<Vec<PdfGlyph>> = paragraphs
        .iter()
        .map(|p| {
            p.glyphs
                .iter()
                .map(|g| {
                    let gid = if text.as_bytes()[g.cluster.start as usize] == b' ' { space_gid } else { 161 };
                    PdfGlyph { glyph_id: gid, font: 0, size_pt: 12.0, x_offset_pt: 0.0, y_offset_pt: 0.0 }
                })
                .collect()
        })
        .collect();
    let stories = [PdfStory { story: &ps.story, text, placement: &placement, paragraphs: &paragraphs, glyphs: &glyphs, default_fill: press::Colour::DeviceGray(0.0) }];
    let fonts = [PdfFont { bytes: INTER, face_index: 0, hyphen: None }];
    let input = PdfExportInput {
        snapshot: &i.proposal.snapshot,
        design: None,
        type_styles: None,
        stories: &stories,
        fonts: &fonts,
        assets: &NoAssets,
        options: press::ExportOptions::default(),
        limits: press::Limits::default(),
    };
    let out = export_pdf(&input, &cancel).unwrap();
    let shown = common::pdf_scan::shown_runs(&pdf_streams(&out.bytes), &press::font_advance_widths(INTER, 0).unwrap());
    let origin = placement.frames[0].origin_x_pt;
    // Leftmost letter per baseline (PDF y up: the first line is the highest).
    let mut first_x: Vec<(i64, f64)> = Vec::new();
    for (gid, x, y) in shown.into_iter().filter(|g| g.0 != space_gid) {
        assert_eq!(gid, 161);
        let key = (y * 100.0).round() as i64;
        match first_x.iter_mut().find(|(k, _)| *k == key) {
            Some(e) => e.1 = e.1.min(x),
            None => first_x.push((key, x)),
        }
    }
    first_x.sort_by_key(|b| std::cmp::Reverse(b.0));
    let want: Vec<f64> = lines.iter().map(|l| origin + l.x_start_pt).collect();
    assert_eq!(first_x.len(), want.len(), "{first_x:?}");
    for ((_, x), w) in first_x.iter().zip(&want) {
        assert!((x - w).abs() < 0.01, "{first_x:?} vs {want:?}");
    }
}

const INTER: &[u8] = include_bytes!("../../hsk-studio-type/tests/fixtures/fonts/Inter-Regular.ttf");
struct NoAssets;
impl PdfAssets for NoAssets {
    fn path(&self, _: &str) -> Option<hsk_studio_nib::OwnedPath> {
        None
    }
    fn raster(&self, _: &str) -> Option<PlacedRaster<'_>> {
        None
    }
    fn profile(&self, _: &str) -> Option<&[u8]> {
        None
    }
}
fn pdf_streams(pdf: &[u8]) -> Vec<Vec<u8>> {
    let find = |h: &[u8], n: &[u8]| h.windows(n.len()).position(|w| w == n);
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(s) = find(&pdf[at..], b">>\nstream\n").map(|i| at + i + 10) {
        let Some(e) = find(&pdf[s..], b"\nendstream").map(|i| s + i) else { break };
        if let Ok(d) = miniz_oxide::inflate::decompress_to_vec_zlib(&pdf[s..e]) {
            out.push(d);
        }
        at = e + 10;
    }
    out
}

/// STU-IO-012: IDML export writes no Folio composition settings yet, so a record set that states
/// any (indents and spacing included) yields one typed `Unsupported` entry counting the style
/// entries and overrides that state settings; a set without settings yields none.
#[test]
fn idml_export_reports_the_composition_settings_it_does_not_write() {
    let (i, t) = import(&indent_package());
    let t = t.expect("type styles");
    let loss = type_style_composition_loss(&t.records).expect("a loss entry");
    assert_eq!(
        (loss.part.as_str(), loss.element.as_str(), loss.kind, loss.code),
        ("Resources/Styles.xml", "ParagraphStyle", LossKind::Unsupported, "paragraph_composition_not_exported")
    );
    assert_eq!(loss.count as usize, t.records.style_composition.len() + t.records.paragraph_composition.len());
    assert!(loss.count >= 3, "root, Ind, Wild and the local override: {loss:?}");
    let empty = StudioTypeStyleRecords::new(i.proposal.snapshot.document().document_id.clone());
    assert_eq!(type_style_composition_loss(&empty), None);
}
