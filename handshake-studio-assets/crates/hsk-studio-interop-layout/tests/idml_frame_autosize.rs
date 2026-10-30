//! Lane SMB-ID-FRAME-AUTOSIZE-2: Type-shaped first baselines (Type's OS/2 run metrics through
//! `ShapedParagraph::from_type`), and the IDML text-frame auto-size attributes mapped to the Folio
//! settings layout's auto-size lays out (lane SMB-ID-FRAME-AUTOSIZE-1), with typed losses for what
//! Folio does not store. Attribute names and the default values below are the ones InDesign 21.6
//! wrote in the oracle corpus exports (`adobe-session/indesign/ID-03__*.idml`, Resources/Styles.xml
//! and Resources/Preferences.xml `TextFramePreference`): `AutoSizingType="Off"
//! AutoSizingReferencePoint="CenterPoint" UseMinimumHeightForAutoSizing="false"
//! MinimumHeightForAutoSizing="0" UseMinimumWidthForAutoSizing="false" MinimumWidthForAutoSizing="0"
//! UseNoLineBreaksForAutoSizing="false"`. Packages are synthetic; InDesign behaviour is UNVERIFIED
//! (D5): no InDesign has opened anything written here.
mod common;

use common::text_shaping::{FontFile, Seg, shape};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{AutoSizing, StudioDocument};
use hsk_studio_interop_layout::layout::{FirstBaselineUnavailable, LayoutError, ShapedParagraph};
use hsk_studio_interop_layout::*;

const NS: &str = "http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging";
const DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";
const SPREAD: &str = "Spreads/Spread_us1.xml";
const TEXT: &str = "aaaa bbbb cccc";
/// The oracle exports' default auto-size attributes (see the module comment).
const DEFAULTS: &str = r#"AutoSizingReferencePoint="CenterPoint" UseMinimumHeightForAutoSizing="false" MinimumHeightForAutoSizing="0" UseMinimumWidthForAutoSizing="false" MinimumWidthForAutoSizing="0" UseNoLineBreaksForAutoSizing="false""#;

/// One 100 x 100 pt frame (no insets) holding the one-paragraph story `TEXT`; `prefs` is written
/// inside its `TextFramePreference` element.
fn package(prefs: &str) -> Vec<u8> {
    let pt = |x: i32, y: i32| format!(r#"<PathPointType Anchor="{x} {y}" LeftDirection="{x} {y}" RightDirection="{x} {y}"/>"#);
    let spread = format!(
        r#"{DECL}<idPkg:Spread xmlns:idPkg="{NS}" DOMVersion="20.0"><Spread Self="us1" ItemTransform="1 0 0 1 0 0">
<Page Self="up1" Name="1" GeometricBounds="0 0 400 300" ItemTransform="1 0 0 1 -150 -200"/>
<TextFrame Self="uf1" ParentStory="ua1" PreviousTextFrame="n" NextTextFrame="n" ItemTransform="1 0 0 1 0 0"><Properties><PathGeometry><GeometryPathType PathOpen="false"><PathPointArray>{}{}{}{}</PathPointArray></GeometryPathType></PathGeometry></Properties><TextFramePreference TextColumnCount="1" TextColumnGutter="12" {prefs}/></TextFrame>
</Spread></idPkg:Spread>"#,
        pt(-130, -180),
        pt(-130, -80),
        pt(-30, -80),
        pt(-30, -180)
    );
    let story = format!(
        "{DECL}<idPkg:Story xmlns:idPkg=\"{NS}\" DOMVersion=\"20.0\"><Story Self=\"ua1\"><StoryPreference StoryDirection=\"LeftToRightDirection\"/><ParagraphStyleRange AppliedParagraphStyle=\"ParagraphStyle/Body\"><CharacterStyleRange><Content>{TEXT}</Content></CharacterStyleRange></ParagraphStyleRange></Story></idPkg:Story>"
    );
    let map = format!(
        r#"{DECL}<Document xmlns:idPkg="{NS}" DOMVersion="20.0" Self="d" StoryList="ua1"><idPkg:Spread src="{SPREAD}"/><idPkg:Story src="Stories/Story_ua1.xml"/></Document>"#
    );
    let parts = [
        ("mimetype", IDML_MIMETYPE.to_owned()),
        ("designmap.xml", map),
        (SPREAD, spread),
        ("Stories/Story_ua1.xml", story),
    ];
    write_idml(&IdmlPackage {
        parts: parts.into_iter().map(|(n, c)| IdmlPart::new(n.into(), c.into_bytes())).collect(),
    })
    .expect("fixture container")
}
fn import(bytes: &[u8]) -> IdmlImport {
    import_idml(bytes, &Limits::default(), &mut SequentialIdMint::new(1), &CancellationToken::default()).expect("import")
}
fn document(i: &IdmlImport) -> &StudioDocument {
    i.proposal.snapshot.document()
}
fn auto_sizing(i: &IdmlImport) -> AutoSizing {
    document(i).layers.iter().find_map(|l| l.text_frame.as_ref()).expect("frame").auto_sizing_type
}
/// The frame-preference loss codes of an import (`TextFramePreference` element), sorted.
fn pref_loss(loss: &[LossEntry]) -> Vec<(&str, LossKind, &'static str, u32)> {
    loss.iter()
        .filter(|l| l.element == "TextFramePreference")
        .map(|l| (l.part.as_str(), l.kind, l.code, l.count))
        .collect()
}

// ---- Type run metrics through from_type (STU-LAY-120) ------------------------------------------------

/// Test-side reader (independent of Type's parser): `head.unitsPerEm`, OS/2 `sCapHeight` and
/// `sxHeight` (OS/2 version 2 and later).
fn os2(bytes: &[u8]) -> (u16, Option<i16>, Option<i16>) {
    let u16_at = |o: usize| u16::from_be_bytes([bytes[o], bytes[o + 1]]);
    let u32_at = |o: usize| u32::from_be_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
    let table = |tag: &[u8; 4]| (0..usize::from(u16_at(4))).map(|i| 12 + 16 * i).find(|&r| &bytes[r..r + 4] == tag).map(|r| u32_at(r + 8) as usize);
    let upm = u16_at(table(b"head").expect("head") + 18);
    match table(b"OS/2") {
        Some(o) if u16_at(o) >= 2 => (upm, Some(u16_at(o + 88) as i16), Some(u16_at(o + 86) as i16)),
        _ => (upm, None, None),
    }
}

/// Host shaper over Type: Inter Regular at 11 pt, line height 12, ascent 10, `from_type` only (no
/// `with_face_metrics`).
struct InterShaper(FontFile);
impl ParagraphShaper for InterShaper {
    fn shape(&self, ctx: &ShapeContext<'_>) -> std::result::Result<ShapedParagraph, LayoutError> {
        let seg = Seg { start: ctx.source.start, end: ctx.source.end, font: 0, script: *b"Latn", language: "en", axes: Vec::new() };
        Ok(shape(std::slice::from_ref(&self.0), ctx.text, &[seg], false, 11.0, |p| {
            ShapedParagraph::from_type(&p.paragraphs()[0], 12.0, 10.0)
        }))
    }
}

/// Inter (unitsPerEm 2816, sCapHeight 2048, sxHeight 1536, read from the bytes here) at 11 pt is
/// 1/256 pt per unit: `from_type` carries cap height 8, x height 6 and em box 11 from Type's run, so
/// an imported cap-height / x-height / em-box frame composes its first baseline at inset 0 + that
/// metric without `with_face_metrics`. The fixture shaper (no face) is refused with the typed code.
#[test]
fn type_shaped_paragraphs_carry_first_baseline_metrics() {
    let inter = FontFile::inter();
    assert_eq!(os2(&inter.bytes), (2816, Some(2048), Some(1536)));
    let seg = Seg { start: 0, end: TEXT.len() as u64, font: 0, script: *b"Latn", language: "en", axes: Vec::new() };
    let p = shape(std::slice::from_ref(&inter), TEXT, &[seg], false, 11.0, |p| {
        let r = &p.paragraphs()[0].runs()[0];
        assert_eq!((r.cap_height_pt(), r.x_height_pt()), (Some(8.0), Some(6.0)));
        ShapedParagraph::from_type(&p.paragraphs()[0], 12.0, 10.0)
    });
    assert_eq!((p.cap_height_pt, p.x_height_pt, p.em_box_pt), (Some(8.0), Some(6.0), Some(11.0)));

    let shaper = InterShaper(inter);
    let cancel = CancellationToken::default();
    for (kind, first) in [("CapHeight", 8.0), ("XHeight", 6.0), ("EmBoxHeight", 11.0)] {
        let i = import(&package(&format!(r#"FirstBaselineOffset="{kind}""#)));
        let t = reflow_import(&i, &shaper, &ReflowOptions::default(), &cancel).expect("reflow");
        let placed = t[0].outcome.as_ref().unwrap_or_else(|e| panic!("{kind}: {e}"));
        assert_eq!(placed.frames[0].lines[0].baseline_y_pt, first, "{kind}");
        assert!(t[0].notes.is_empty(), "{kind}: {:?}", t[0].notes);
        let toy = FixedAdvanceShaper { advance_pt: 5.0, line_height_pt: 12.0, ascent_pt: 10.0 };
        let refused = reflow_import(&i, &toy, &ReflowOptions::default(), &cancel).expect("reflow");
        let code = refused[0].refusal().expect("refused");
        assert!(code.starts_with("first_baseline_metric_unavailable:"), "{kind}: {code}");
        assert!(matches!(refused[0].outcome, Err(LayoutError::FirstBaselineMetricUnavailable(FirstBaselineUnavailable { .. }))));
    }
}

// ---- IDML auto-size attributes (STU-LAY-118 / STU-LAY-119) -------------------------------------------

/// Off: the six auto-size attributes are moot, whatever they say (even unknown values), and the
/// document imports identically to one without them; so does an auto-sizing frame without them.
#[test]
fn absent_or_moot_auto_size_attributes_import_identically() {
    let bare = import(&package(r#"AutoSizingType="Off""#));
    for extra in [
        DEFAULTS,
        r#"AutoSizingReferencePoint="Elsewhere" UseMinimumHeightForAutoSizing="maybe" MinimumHeightForAutoSizing="x" UseMinimumWidthForAutoSizing="true" MinimumWidthForAutoSizing="40" UseNoLineBreaksForAutoSizing="true""#,
    ] {
        let i = import(&package(&format!(r#"AutoSizingType="Off" {extra}"#)));
        assert_eq!(document(&i), document(&bare), "{extra}");
        assert_eq!(i.loss, bare.loss, "{extra}");
    }
    assert!(pref_loss(&bare.loss).is_empty());
    let plain = import(&package(""));
    assert_eq!((document(&plain), &plain.loss), (document(&bare), &bare.loss));
    for kind in ["HeightOnly", "WidthOnly", "HeightAndWidth", "HeightAndWidthProportionally"] {
        let i = import(&package(&format!(r#"AutoSizingType="{kind}""#)));
        assert!(pref_loss(&i.loss).is_empty(), "{kind}: {:?}", i.loss);
    }
}

/// Each auto-sizing kind keeps its Folio `auto_sizing_type`. Studio's readings (layout DESCRIPTOR
/// frame_autosize.readings): the top or left edge stays fixed, no minimum height or width, and the
/// kinds whose width floats unscaled (width only, height and width) set every line unwrapped while
/// proportional re-breaks lines at the scaled width. An attribute that matches the reading maps with
/// no loss; one that differs is a typed `Approximated` loss naming what Folio does not store.
#[test]
fn auto_size_attributes_map_to_studio_readings_or_typed_loss() {
    let a = |code: &'static str| vec![(SPREAD, LossKind::Approximated, code, 1)];
    let ref_point = "auto_sizing_reference_point_not_stored";
    let minimum = "auto_sizing_minimum_not_stored";
    let breaks = "auto_sizing_line_breaks_not_stored";
    let cases: [(&str, AutoSizing, &str, Vec<_>); 16] = [
        // InDesign's defaults: only the centre reference point differs from Studio's fixed edge.
        ("HeightOnly", AutoSizing::HeightOnly, DEFAULTS, a(ref_point)),
        ("HeightAndWidthProportionally", AutoSizing::HeightAndWidthProportionally, DEFAULTS, a(ref_point)),
        // Minimum gates: a loss only on an axis the kind sizes.
        ("HeightOnly", AutoSizing::HeightOnly, r#"UseMinimumHeightForAutoSizing="true" MinimumHeightForAutoSizing="40""#, a(minimum)),
        ("HeightOnly", AutoSizing::HeightOnly, r#"UseMinimumWidthForAutoSizing="true" MinimumWidthForAutoSizing="40""#, vec![]),
        ("WidthOnly", AutoSizing::WidthOnly, r#"UseMinimumHeightForAutoSizing="true""#, vec![]),
        ("WidthOnly", AutoSizing::WidthOnly, r#"UseMinimumWidthForAutoSizing="true""#, a(minimum)),
        ("HeightAndWidth", AutoSizing::HeightAndWidth, r#"UseMinimumHeightForAutoSizing="true" UseMinimumWidthForAutoSizing="true""#, vec![(SPREAD, LossKind::Approximated, minimum, 2)]),
        // No-line-breaks flag against each kind's reading; moot for height only.
        ("WidthOnly", AutoSizing::WidthOnly, r#"UseNoLineBreaksForAutoSizing="false""#, a(breaks)),
        ("HeightAndWidth", AutoSizing::HeightAndWidth, r#"UseNoLineBreaksForAutoSizing="true""#, vec![]),
        ("HeightAndWidth", AutoSizing::HeightAndWidth, r#"UseNoLineBreaksForAutoSizing="false""#, a(breaks)),
        ("HeightAndWidthProportionally", AutoSizing::HeightAndWidthProportionally, r#"UseNoLineBreaksForAutoSizing="true""#, a(breaks)),
        ("HeightAndWidthProportionally", AutoSizing::HeightAndWidthProportionally, r#"UseNoLineBreaksForAutoSizing="false""#, vec![]),
        ("HeightOnly", AutoSizing::HeightOnly, r#"UseNoLineBreaksForAutoSizing="true""#, vec![]),
        // Hostile text (V4-08): a character reference is decoded before the comparison ("true" here,
        // the reading of height and width: no loss); a look-alike attribute name is not read.
        ("HeightAndWidth", AutoSizing::HeightAndWidth, r#"UseNoLineBreaksForAutoSizing="&#116;rue""#, vec![]),
        ("HeightAndWidth", AutoSizing::HeightAndWidth, r#"UseNoLineBreaksForAutoSizingX="false" xUseMinimumHeightForAutoSizing="true""#, vec![]),
        // Unknown values (attribute text compared exactly, no case folding or trimming).
        ("HeightAndWidth", AutoSizing::HeightAndWidth, r#"AutoSizingReferencePoint="centerpoint" UseMinimumHeightForAutoSizing=" false" UseNoLineBreaksForAutoSizing="TRUE""#, vec![(SPREAD, LossKind::Approximated, "frame_preference_value_unknown", 3)]),
    ];
    for (kind, folio, attrs, expected) in cases {
        let i = import(&package(&format!(r#"AutoSizingType="{kind}" {attrs}"#)));
        assert_eq!(auto_sizing(&i), folio, "{kind} {attrs}");
        assert_eq!(pref_loss(&i.loss), expected, "{kind} {attrs}");
    }
    // An unknown auto-sizing type stays a typed loss and keeps Off (unchanged behaviour).
    let i = import(&package(r#"AutoSizingType="Sometimes""#));
    assert_eq!(auto_sizing(&i), AutoSizing::Off);
    assert_eq!(pref_loss(&i.loss), a("frame_preference_value_unknown"));
}

fn exported_prefs(bytes: &[u8]) -> String {
    let package = open_idml(bytes, &Limits::default(), &CancellationToken::default()).expect("open");
    let spread = package.parts.iter().find(|p| p.name.starts_with("Spreads/")).expect("spread");
    let xml = String::from_utf8(spread.bytes.clone()).expect("utf-8");
    let start = xml.find("<TextFramePreference").expect("prefs");
    xml[start..start + xml[start..].find('>').expect("end")].to_owned()
}

/// Folio export writes the attributes that state Studio's readings for an auto-sizing frame (no
/// minimum gates, the no-line-breaks flag of the kind) and reports the reference point it cannot
/// state as a typed loss; the export imports back with the same kind and no auto-size loss. An Off
/// frame's preference element is unchanged (no auto-size attribute beyond `AutoSizingType`).
#[test]
fn folio_export_states_the_auto_size_readings() {
    let cancel = CancellationToken::default();
    for (kind, folio, no_breaks) in [
        ("HeightOnly", AutoSizing::HeightOnly, "false"),
        ("WidthOnly", AutoSizing::WidthOnly, "true"),
        ("HeightAndWidth", AutoSizing::HeightAndWidth, "true"),
        ("HeightAndWidthProportionally", AutoSizing::HeightAndWidthProportionally, "false"),
    ] {
        let i = import(&package(&format!(r#"AutoSizingType="{kind}" {DEFAULTS}"#)));
        let stories: Vec<_> = i.proposal.stories.iter().map(|s| StoryInput { story: &s.story, resolved_text: None }).collect();
        let names = i.export_names();
        let out = export_folio(&FolioExportInput { document: document(&i), stories: &stories, names: &names }, &Limits::default(), &cancel).expect("export");
        let prefs = exported_prefs(&out.bytes);
        assert!(
            prefs.contains(&format!(
                r#"AutoSizingType="{kind}" UseMinimumHeightForAutoSizing="false" UseMinimumWidthForAutoSizing="false" UseNoLineBreaksForAutoSizing="{no_breaks}""#
            )),
            "{prefs}"
        );
        assert!(!prefs.contains("AutoSizingReferencePoint"), "{prefs}");
        assert!(
            out.loss.iter().any(|l| l.part == "document"
                && l.element == "TextFramePreference"
                && l.kind == LossKind::Approximated
                && l.code == "auto_sizing_reference_point_not_exported"
                && l.count == 1),
            "{kind}: {:?}",
            out.loss
        );
        let back = import(&out.bytes);
        assert_eq!(auto_sizing(&back), folio, "{kind}");
        assert!(pref_loss(&back.loss).is_empty(), "{kind}: {:?}", back.loss);
    }
    let off = import(&package(r#"AutoSizingType="Off""#));
    let stories: Vec<_> = off.proposal.stories.iter().map(|s| StoryInput { story: &s.story, resolved_text: None }).collect();
    let names = off.export_names();
    let out = export_folio(&FolioExportInput { document: document(&off), stories: &stories, names: &names }, &Limits::default(), &cancel).expect("export");
    let prefs = exported_prefs(&out.bytes);
    assert!(prefs.ends_with(r#"AutoSizingType="Off""#), "{prefs}");
    assert!(!out.loss.iter().any(|l| l.code.starts_with("auto_sizing")), "{:?}", out.loss);
}
