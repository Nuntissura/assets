//! Slice 2 proof: reflow and overset of imported threaded stories through hsk-studio-layout, typed
//! refusals, text frames inside groups, and Folio-to-IDML export of created and edited documents
//! with import round trips. Packages are synthetic; no InDesign has opened anything written here.
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    Code, Payload, StoryContent, StoryTextKind, StudioDocument, StudioTextStory,
};
use hsk_studio_interop_layout::layout::{LayoutError, SourceSpan};
use hsk_studio_interop_layout::*;
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::BTreeMap;

const NS: &str = "http://ns.adobe.com/AdobeInDesign/idml/1.0/packaging";
const DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n";

// ---- synthetic IDML for the reflow test ----------------------------------------------------------

fn frame(
    id: &str,
    story: &str,
    prev: &str,
    next: &str,
    y: (i32, i32),
    columns: u32,
    baseline: &str,
) -> String {
    let pt = |x: i32, y: i32| {
        format!(
            r#"<PathPointType Anchor="{x} {y}" LeftDirection="{x} {y}" RightDirection="{x} {y}"/>"#
        )
    };
    format!(
        r#"<TextFrame Self="{id}" ParentStory="{story}" PreviousTextFrame="{prev}" NextTextFrame="{next}" ItemTransform="1 0 0 1 0 0"><Properties><PathGeometry><GeometryPathType PathOpen="false"><PathPointArray>{}{}{}{}</PathPointArray></GeometryPathType></PathGeometry></Properties><TextFramePreference TextColumnCount="{columns}" TextColumnGutter="12" FirstBaselineOffset="{baseline}"/></TextFrame>"#,
        pt(-130, y.0),
        pt(-130, y.1),
        pt(-30, y.1),
        pt(-30, y.0)
    )
}

fn story(id: &str, direction: &str, paragraphs: &[&str]) -> String {
    let last = paragraphs.len() - 1;
    let body: String = paragraphs
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let br = if i < last { "<Br/>" } else { "" };
            format!("<ParagraphStyleRange AppliedParagraphStyle=\"ParagraphStyle/Body\"><CharacterStyleRange><Content>{t}</Content>{br}</CharacterStyleRange></ParagraphStyleRange>")
        })
        .collect();
    format!(
        "{DECL}<idPkg:Story xmlns:idPkg=\"{NS}\" DOMVersion=\"20.0\"><Story Self=\"{id}\"><StoryPreference StoryDirection=\"{direction}\"/>{body}</Story></idPkg:Story>"
    )
}

struct Fixture {
    columns: u32,
    baseline: &'static str,
    direction: &'static str,
}
impl Fixture {
    const BASE: Fixture = Fixture {
        columns: 1,
        baseline: "AscentOffset",
        direction: "LeftToRightDirection",
    };

    fn bytes(&self) -> Vec<u8> {
        let spread = format!(
            r#"{DECL}<idPkg:Spread xmlns:idPkg="{NS}" DOMVersion="20.0"><Spread Self="us1" ItemTransform="1 0 0 1 0 0">
<Page Self="up1" Name="1" GeometricBounds="0 0 400 300" ItemTransform="1 0 0 1 -150 -200"/>
{}
{}
{}
<Group Self="ug1"><TextFrame Self="ug1a" ParentStory="ux" PreviousTextFrame="n" NextTextFrame="n" ItemTransform="1 0 0 1 0 0"/><Group Self="ug2"><TextFrame Self="ug2a" ParentStory="ux" PreviousTextFrame="n" NextTextFrame="n" ItemTransform="1 0 0 1 0 0"/></Group></Group>
</Spread></idPkg:Spread>"#,
            frame("uf1", "ua1", "n", "uf2", (-180, -150), 1, self.baseline),
            frame(
                "uf2",
                "ua1",
                "uf1",
                "n",
                (-140, -120),
                self.columns,
                self.baseline
            ),
            frame("uf3", "ub1", "n", "n", (-100, -60), 1, self.baseline),
        );
        let map = format!(
            r#"{DECL}<Document xmlns:idPkg="{NS}" DOMVersion="20.0" Self="d" StoryList="ua1 ub1"><idPkg:Spread src="Spreads/Spread_us1.xml"/><idPkg:Story src="Stories/Story_ua1.xml"/><idPkg:Story src="Stories/Story_ub1.xml"/></Document>"#
        );
        let parts = [
            ("mimetype", IDML_MIMETYPE.to_owned()),
            ("designmap.xml", map),
            ("Spreads/Spread_us1.xml", spread),
            (
                "Stories/Story_ua1.xml",
                story(
                    "ua1",
                    self.direction,
                    &[
                        "aaaa bbbb cccc dddd eeee",
                        "ffff",
                        "gggg hhhh",
                        "iiii",
                        "jjjj kkkk",
                    ],
                ),
            ),
            (
                "Stories/Story_ub1.xml",
                story("ub1", "LeftToRightDirection", &["hello"]),
            ),
        ];
        let package = IdmlPackage {
            parts: parts
                .into_iter()
                .map(|(n, c)| IdmlPart::new(n.into(), c.into_bytes()))
                .collect(),
        };
        write_idml(&package).expect("fixture container")
    }
}

fn import(bytes: &[u8]) -> IdmlImport {
    import_idml(
        bytes,
        &Limits::default(),
        &mut SequentialIdMint::new(1),
        &CancellationToken::default(),
    )
    .expect("import")
}
fn has(
    loss: &[LossEntry],
    part: &str,
    element: &str,
    kind: LossKind,
    code: &str,
    count: u32,
) -> bool {
    loss.iter().any(|l| {
        l.part == part
            && l.element == element
            && l.kind == kind
            && l.code == code
            && l.count == count
    })
}

const TOY: FixedAdvanceShaper = FixedAdvanceShaper {
    advance_pt: 5.0,
    line_height_pt: 10.0,
    ascent_pt: 8.0,
};

struct Counting(Cell<usize>, Option<LayoutError>);
impl ParagraphShaper for Counting {
    fn shape(
        &self,
        ctx: &ShapeContext<'_>,
    ) -> std::result::Result<layout::ShapedParagraph, LayoutError> {
        self.0.set(self.0.get() + 1);
        match self.1 {
            Some(e) => Err(e),
            None => TOY.shape(ctx),
        }
    }
}

fn flow(import: &IdmlImport, shaper: &dyn ParagraphShaper) -> Vec<ThreadReflow> {
    reflow_import(
        import,
        shaper,
        &ReflowOptions::default(),
        &CancellationToken::default(),
    )
    .expect("reflow")
}

// ---- tests ---------------------------------------------------------------------------------------

#[test]
fn reflow_overset_per_thread_and_typed_refusals() {
    let a = import(&Fixture::BASE.bytes());
    let threads = flow(&a, &TOY);
    assert_eq!(
        threads.len(),
        2,
        "one thread per story that has a frame chain"
    );

    // ua1: frame 1 holds 3 lines (baselines 8, 18, 28 in a 30 pt frame), frame 2 holds 2 (20 pt);
    // 5 + 1 lines do not fit, so paragraph 5 (bytes 45..54) is overset.
    let t = &threads[0];
    assert_eq!((t.idml_story_id.as_str(), t.frame_ids.len()), ("ua1", 2));
    assert_eq!(t.notes, ["first_baseline_mapped_to_fixed"]);
    let placed = t.outcome.as_ref().expect("placed");
    assert_eq!(t.overset(), Some(SourceSpan { start: 45, end: 54 }));
    let lines = |f: usize| {
        placed.frames[f]
            .lines
            .iter()
            .map(|l| {
                (
                    l.paragraph,
                    l.baseline_y_pt,
                    l.width_pt,
                    l.hang_pt,
                    l.source.start,
                    l.source.end,
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        lines(0),
        [
            (0, 8.0, 95.0, 5.0, 0, 20),
            (0, 18.0, 20.0, 0.0, 20, 24),
            (1, 28.0, 20.0, 0.0, 25, 29)
        ]
    );
    assert_eq!(
        lines(1),
        [(2, 8.0, 45.0, 0.0, 30, 39), (3, 18.0, 20.0, 0.0, 40, 44)]
    );
    assert_eq!(placed.frames[0].frame_id, t.frame_ids[0]);

    // ub1 fits entirely.
    assert_eq!(threads[1].idml_story_id, "ub1");
    assert!(threads[1].outcome.is_ok() && threads[1].overset().is_none());

    // Text frames inside groups (two, nested) are counted, not decoded and not dropped.
    assert!(has(
        &a.loss,
        "Spreads/Spread_us1.xml",
        "TextFrame",
        LossKind::PreservedOpaque,
        "text_frame_in_group_not_decoded",
        2
    ));
    assert_eq!(a.proposal.snapshot.document().layers.len(), 3);

    // A refusal on one thread passes through from layout and does not hide the other thread.
    let multi = import(
        &Fixture {
            columns: 2,
            ..Fixture::BASE
        }
        .bytes(),
    );
    // Multi-column frames compose since SMB-LAYOUT-UI slice 1 (layout tests/layout_columns.rs).
    let t = flow(&multi, &TOY);
    assert!(t[0].outcome.is_ok());
    assert!(t[1].outcome.is_ok());
    // Baselines layout cannot map (cap height) are refused by layout, not guessed, and the refusal
    // passes through per thread (both threads use the fixture baseline here).
    let cap = import(
        &Fixture {
            baseline: "CapHeight",
            ..Fixture::BASE
        }
        .bytes(),
    );
    let c = flow(&cap, &TOY);
    assert_eq!(c[0].outcome, Err(LayoutError::UnsupportedComposition));
    assert_eq!(c[0].refusal(), Some("unsupported_composition"));
    assert!(c[0].overset().is_none());
    assert_eq!(c[1].refusal(), Some("unsupported_composition"));

    // An RTL story is refused before any shaping; a missing font is a typed layout refusal.
    let rtl = import(
        &Fixture {
            direction: "RightToLeftDirection",
            ..Fixture::BASE
        }
        .bytes(),
    );
    let counter = Counting(Cell::new(0), None);
    let t = flow(&rtl, &counter);
    assert!(!rtl.proposal.stories[0].reflow_certified);
    assert_eq!(t[0].outcome, Err(LayoutError::UnsupportedComposition));
    assert_eq!(
        counter.0.get(),
        1,
        "only the Latin story ub1 was shaped (1 paragraph)"
    );
    let missing = Counting(Cell::new(0), Some(LayoutError::MissingFont));
    assert_eq!(flow(&a, &missing)[0].refusal(), Some("missing_font"));

    // Cancellation aborts the whole call.
    let stop = CancellationToken::default();
    stop.cancel();
    assert_eq!(
        reflow_import(&a, &TOY, &ReflowOptions::default(), &stop).unwrap_err(),
        IdmlError::Canceled
    );
}

// ---- Folio documents -----------------------------------------------------------------------------

fn lp(v: f64) -> Value {
    json!({"value": v, "unit": "pt"})
}
fn page(key: &str, side: &str, margins: [f64; 4]) -> Value {
    json!({
        "page_id": key, "width": lp(200.0), "height": lp(300.0), "side": side,
        "margins": {"top": lp(margins[0]), "right": lp(margins[1]), "bottom": lp(margins[2]), "left": lp(margins[3])},
        "bleed": {"top": lp(6.0), "bottom": lp(6.0), "inside_or_left": lp(6.0), "outside_or_right": lp(6.0)},
    })
}
#[allow(clippy::too_many_arguments)]
fn text_layer(
    id: &str,
    name: &str,
    spread: &str,
    story: &str,
    page: &str,
    b: [f64; 4],
    prev: Option<&str>,
    next: Option<&str>,
    frame: Value,
) -> Value {
    let mut f = json!({
        "page_id": page,
        "bounds": {"x": lp(b[0]), "y": lp(b[1]), "width": lp(b[2]), "height": lp(b[3])},
        "inset_spacing": {"top": lp(0.0), "right": lp(0.0), "bottom": lp(0.0), "left": lp(0.0)},
        "text_column_count": 1, "text_column_gutter": lp(12.0),
        "first_baseline_offset": "ascent", "minimum_first_baseline_offset": lp(0.0),
        "vertical_justification": "top", "auto_sizing_type": "off",
        "previous_text_frame": prev, "next_text_frame": next,
    });
    for (k, v) in frame.as_object().expect("overrides") {
        f[k] = v.clone();
    }
    json!({
        "schema_id": "hsk.studio.layer@1", "layer_id": id, "name": name,
        "parent": {"node_kind": "page_spread", "node_id": spread},
        "kind": "text", "payload": {"payload_kind": "text", "story_id": story},
        "visible": true, "text_frame": f,
    })
}
fn story_json(
    id: &str,
    direction: &str,
    text: &str,
    paragraphs: Value,
    ranges: Value,
) -> StudioTextStory {
    serde_json::from_value(json!({
        "schema_id": "hsk.studio.text_story@1", "story_id": id, "text_kind": "area",
        "story_direction": direction, "content": {"content_kind": "inline", "text_utf8": text},
        "paragraphs": paragraphs, "character_ranges": ranges,
    }))
    .expect("story")
}

struct Created {
    doc: StudioDocument,
    stories: Vec<StudioTextStory>,
    names: ExportNames,
}

/// Two spreads (facing pair with a two-frame thread, one single right-hand page), a group layer
/// Folio can hold but IDML export does not write, two stories with styles, languages and an
/// empty paragraph; the second story is right-to-left.
fn created() -> Created {
    let mut m = SequentialIdMint::new(7);
    let (doc_id, s1, s2) = (m.mint("SDOC"), m.mint("SPGS"), m.mint("SPGS"));
    let (l1, l2, l3, l4) = (
        m.mint("SLYR"),
        m.mint("SLYR"),
        m.mint("SLYR"),
        m.mint("SLYR"),
    );
    let (st1, st2) = (m.mint("STXT"), m.mint("STXT"));
    let (heading, body, emphasis) = (m.mint("STYS"), m.mint("STYS"), m.mint("STYS"));
    let wire = json!({
        "schema_id": "hsk.studio.document@1", "document_id": doc_id, "revision": 4,
        "geometry_unit": "pt", "colour_mode": 3, "bits_per_channel": 8,
        "working_space_rgb": null, "working_space_cmyk": null, "working_space_gray": null,
        "working_space_spot": null, "blending_space": null, "artboards": [],
        "page_spreads": [
            {"schema_id": "hsk.studio.page_spread@1", "page_spread_id": s1, "name": "A", "parent": null,
             "bounds": {"x": lp(0.0), "y": lp(0.0), "width": lp(400.0), "height": lp(300.0)},
             "pages": [page("pl", "left_hand", [10.0, 20.0, 30.0, 40.0]), page("pr", "right_hand", [11.0, 21.0, 31.0, 41.0])]},
            {"schema_id": "hsk.studio.page_spread@1", "page_spread_id": s2, "name": "B", "parent": null,
             "bounds": {"x": lp(0.0), "y": lp(0.0), "width": lp(200.0), "height": lp(300.0)},
             "pages": [page("pr2", "right_hand", [12.0, 22.0, 32.0, 42.0])]},
        ],
        "layers": [
            text_layer(&l1, "Left frame", &s1, &st1, "pl", [10.0, 12.0, 150.0, 100.0], None, Some(&l2),
                json!({"inset_spacing": {"top": lp(1.0), "right": lp(2.0), "bottom": lp(3.0), "left": lp(4.0)},
                       "text_column_count": 2, "text_column_gutter": lp(8.0), "vertical_justification": "center",
                       "first_baseline_offset": "leading", "minimum_first_baseline_offset": lp(5.0), "auto_sizing_type": "height_only"})),
            text_layer(&l2, "Right frame", &s1, &st1, "pr", [10.0, 12.0, 150.0, 100.0], Some(&l1), None, json!({})),
            text_layer(&l3, "Solo", &s2, &st2, "pr2", [20.0, 30.0, 100.0, 60.0], None, None, json!({})),
            {"schema_id": "hsk.studio.layer@1", "layer_id": l4, "name": "A group", "parent": null, "kind": "group",
             "payload": {"payload_kind": "group"}, "visible": true},
        ],
        "graph": {"schema_id": "hsk.studio.layer_graph@1", "operations": [], "edges": [], "outputs": []},
    });
    let doc: StudioDocument = serde_json::from_value(wire).expect("document");
    let text1 = "Created in Folio\nsecond paragraph here\n\nlast";
    let stories = vec![
        story_json(
            &st1,
            "left_to_right",
            text1,
            json!([
                {"start": 0, "end": 17, "style_ref": heading, "paragraph_direction": null},
                {"start": 17, "end": 39, "style_ref": body, "paragraph_direction": null},
                {"start": 39, "end": 40, "style_ref": null, "paragraph_direction": null},
                {"start": 40, "end": 44, "style_ref": body, "paragraph_direction": null},
            ]),
            json!([
                {"start": 0, "end": 8, "style_ref": emphasis, "applied_language": "en_US"},
                {"start": 8, "end": 17, "style_ref": null, "applied_language": "en_US"},
                {"start": 17, "end": 28, "style_ref": emphasis, "applied_language": null},
                {"start": 28, "end": 44, "style_ref": null, "applied_language": null},
            ]),
        ),
        story_json(
            &st2,
            "right_to_left",
            "solo",
            json!([{"start": 0, "end": 4, "style_ref": null, "paragraph_direction": null}]),
            json!([{"start": 0, "end": 4, "style_ref": null, "applied_language": null}]),
        ),
    ];
    let names = ExportNames {
        paragraph_styles: BTreeMap::from([
            (heading, "ParagraphStyle/Heading".into()),
            (body, "ParagraphStyle/Body".into()),
        ]),
        character_styles: BTreeMap::from([(emphasis, "CharacterStyle/Emphasis".into())]),
        languages: BTreeMap::from([("en_US".into(), "$ID/English: USA".into())]),
    };
    Created {
        doc,
        stories,
        names,
    }
}

fn inputs(stories: &[StudioTextStory]) -> Vec<StoryInput<'_>> {
    stories
        .iter()
        .map(|story| StoryInput {
            story,
            resolved_text: None,
        })
        .collect()
}
fn export(doc: &StudioDocument, stories: &[StudioTextStory], names: &ExportNames) -> FolioExport {
    export_folio(
        &FolioExportInput {
            document: doc,
            stories: &inputs(stories),
            names,
        },
        &Limits::default(),
        &CancellationToken::default(),
    )
    .expect("export")
}

/// Id-free structural summary of a Folio document and its stories; style and language ids are
/// replaced by their IDML names (`names`: style id -> name, language key -> name).
fn summary(
    doc: &StudioDocument,
    stories: &[&StudioTextStory],
    styles: &BTreeMap<String, String>,
    langs: &BTreeMap<String, String>,
) -> Vec<String> {
    let pages: Vec<&str> = doc
        .page_spreads
        .iter()
        .flat_map(|s| &s.pages)
        .map(|p| p.page_id.as_str())
        .collect();
    let layers: Vec<&str> = doc.layers.iter().map(|l| l.layer_id.as_str()).collect();
    let story_ids: Vec<&str> = stories.iter().map(|s| s.story_id.as_str()).collect();
    let pos = |list: &[&str], id: &str| list.iter().position(|x| *x == id);
    let mut out = Vec::new();
    for s in &doc.page_spreads {
        for p in &s.pages {
            let m = &p.margins;
            out.push(format!(
                "page {}x{} {:?} margins {:?} bleed {:?}",
                p.width.value,
                p.height.value,
                p.side,
                [m.top.value, m.right.value, m.bottom.value, m.left.value],
                p.bleed.as_ref().map(|b| [
                    b.top.value,
                    b.bottom.value,
                    b.inside_or_left.value,
                    b.outside_or_right.value
                ]),
            ));
        }
    }
    for l in &doc.layers {
        let (Some(f), Payload::Text { story_id }) = (&l.text_frame, &l.payload) else {
            continue;
        };
        let (b, i) = (&f.bounds, &f.inset_spacing);
        out.push(format!(
            "frame {:?} page {:?} bounds {:?} inset {:?} cols {} gutter {} vj {:?} fb {:?} min {} auto {:?} prev {:?} next {:?} story {:?}",
            l.name,
            pos(&pages, &f.page_id),
            [b.x.value, b.y.value, b.width.value, b.height.value],
            [i.top.value, i.right.value, i.bottom.value, i.left.value],
            f.text_column_count,
            f.text_column_gutter.value,
            f.vertical_justification,
            f.first_baseline_offset,
            f.minimum_first_baseline_offset.value,
            f.auto_sizing_type,
            f.previous_text_frame.as_deref().and_then(|p| pos(&layers, p)),
            f.next_text_frame.as_deref().and_then(|p| pos(&layers, p)),
            pos(&story_ids, story_id),
        ));
    }
    let name = |m: &BTreeMap<String, String>, k: &Option<String>| {
        k.as_ref()
            .map(|k| m.get(k).cloned().unwrap_or_else(|| format!("?{k}")))
    };
    for s in stories {
        out.push(format!(
            "story {:?} {:?}",
            s.story_direction,
            s.inline_text()
        ));
        for p in &s.paragraphs {
            out.push(format!(
                "  para {}..{} {:?} {:?}",
                p.start,
                p.end,
                name(styles, &p.style_ref),
                p.paragraph_direction
            ));
        }
        for c in &s.character_ranges {
            out.push(format!(
                "  range {}..{} {:?} {:?}",
                c.start,
                c.end,
                name(styles, &c.style_ref),
                name(langs, &c.applied_language)
            ));
        }
    }
    out
}

fn reimport_summary(i: &IdmlImport) -> Vec<String> {
    let names = i.export_names();
    let styles: BTreeMap<String, String> = names
        .paragraph_styles
        .into_iter()
        .chain(names.character_styles)
        .collect();
    let langs = names.languages;
    let stories: Vec<&StudioTextStory> = i.proposal.stories.iter().map(|s| &s.story).collect();
    summary(i.proposal.snapshot.document(), &stories, &styles, &langs)
}

#[test]
fn folio_created_document_exports_idml_and_round_trips() {
    let c = created();
    let e = export(&c.doc, &c.stories, &c.names);
    assert_eq!(
        e.bytes,
        export(&c.doc, &c.stories, &c.names).bytes,
        "export is deterministic"
    );

    // A real, safe IDML container with the expected parts.
    let pkg = open_idml(&e.bytes, &Limits::default(), &CancellationToken::default())
        .expect("safe container");
    let names: Vec<&str> = pkg.parts.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "mimetype",
            "META-INF/container.xml",
            "designmap.xml",
            "Resources/Preferences.xml",
            "Spreads/Spread_us1.xml",
            "Spreads/Spread_us2.xml",
            "Stories/Story_ua1.xml",
            "Stories/Story_ua2.xml"
        ]
    );

    // The Folio group layer is reported, nothing else is.
    assert_eq!(e.loss.len(), 1);
    assert!(has(
        &e.loss,
        "document",
        "group",
        LossKind::Unsupported,
        "folio_layer_not_exported",
        1
    ));

    // Import the exported package: the same pages, frames, thread, text, styles and languages.
    let i = import_idml(
        &e.bytes,
        &Limits::default(),
        &mut SequentialIdMint::new(2),
        &CancellationToken::default(),
    )
    .expect("reimport");
    assert!(!i.loss.iter().any(|l| matches!(
        l.kind,
        LossKind::Unsupported | LossKind::UnsupportedComposition
    ) && l.element != "Story"));
    let styles: BTreeMap<String, String> = c
        .names
        .paragraph_styles
        .iter()
        .chain(&c.names.character_styles)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let stories: Vec<&StudioTextStory> = c.stories.iter().collect();
    let before = summary(&c.doc, &stories, &styles, &c.names.languages);
    let after = reimport_summary(&i);
    assert_eq!(before, after);
    assert_eq!(i.proposal.snapshot.document().text_threads().len(), 2);
    // The right-to-left story is flagged for composition, the Latin one is certified.
    assert!(i.proposal.stories[0].reflow_certified);
    assert_eq!(i.proposal.stories[1].composition, ["rtl_direction"]);
    // Exporting the re-import (model path) reproduces the same bytes: the writer is a fixed point.
    assert_eq!(
        export_idml(&i, &CancellationToken::default()).expect("re-export"),
        e.bytes
    );
}

#[test]
fn folio_edited_import_round_trips_with_names_and_reflow() {
    let a = import(&Fixture::BASE.bytes());
    let names = a.export_names();
    assert_eq!(names.paragraph_styles.len(), 1);

    // Edit in Folio: move the single-frame story's frame and append text to the long story.
    let mut doc = a.proposal.snapshot.document().clone();
    let solo = a
        .proposal
        .stories
        .iter()
        .find(|s| s.idml_id == "ub1")
        .expect("ub1")
        .story
        .story_id
        .clone();
    for l in &mut doc.layers {
        if matches!(&l.payload, Payload::Text { story_id } if *story_id == solo) {
            l.text_frame.as_mut().expect("frame").bounds.x.value += 10.0;
        }
    }
    let mut stories: Vec<StudioTextStory> =
        a.proposal.stories.iter().map(|s| s.story.clone()).collect();
    let long = &mut stories[0];
    if let StoryContent::Inline { text_utf8 } = &mut long.content {
        text_utf8.push_str(" llll");
    }
    long.paragraphs.last_mut().expect("paragraph").end += 5;
    long.character_ranges.last_mut().expect("range").end += 5;

    let e = export(&doc, &stories, &names);
    assert!(e.loss.is_empty(), "{:?}", e.loss);
    let b = import(&e.bytes);
    let (long_b, solo_b) = (&b.proposal.stories[0], &b.proposal.stories[1]);
    assert_eq!(
        long_b.story.inline_text(),
        Some("aaaa bbbb cccc dddd eeee\nffff\ngggg hhhh\niiii\njjjj kkkk llll")
    );
    assert_eq!(
        b.proposal
            .style_refs
            .iter()
            .map(|s| s.idml_name.as_str())
            .collect::<Vec<_>>(),
        ["ParagraphStyle/Body"]
    );
    let moved = b.proposal.snapshot.document().layers.iter().find(|l| matches!(&l.payload, Payload::Text { story_id } if *story_id == solo_b.story.story_id)).expect("solo frame");
    let before = a.proposal.snapshot.document().layers[2]
        .text_frame
        .as_ref()
        .expect("frame")
        .bounds
        .x
        .value;
    assert_eq!(
        moved.text_frame.as_ref().expect("frame").bounds.x.value,
        before + 10.0
    );
    assert_eq!(b.proposal.snapshot.document().text_threads().len(), 2);

    // Reflow after the edit: the overset grew with the text (bytes 45..59).
    let threads = flow(&b, &TOY);
    assert_eq!(
        threads[0].overset(),
        Some(SourceSpan { start: 45, end: 59 })
    );

    // Without the names the style references cannot be written: reported, not silently dropped.
    let anon = export(&doc, &stories, &ExportNames::default());
    assert!(has(
        &anon.loss,
        "Stories/Story_ua1.xml",
        "ParagraphStyleRange",
        LossKind::Approximated,
        "style_name_unmapped",
        5
    ));
    assert!(import(&anon.bytes).proposal.style_refs.is_empty());
}

#[test]
fn folio_export_refusals_and_cancel() {
    let c = created();
    let go = |doc: &StudioDocument, stories: &[StoryInput<'_>], cancel: &CancellationToken| {
        export_folio(
            &FolioExportInput {
                document: doc,
                stories,
                names: &c.names,
            },
            &Limits::default(),
            cancel,
        )
    };
    let ok = CancellationToken::default();
    let all = inputs(&c.stories);

    // A frame whose story was not supplied.
    assert_eq!(
        go(&c.doc, &all[..1], &ok).unwrap_err(),
        IdmlError::DanglingStory
    );
    // A document Folio itself refuses (thread link to a layer that does not exist).
    let mut broken = c.doc.clone();
    broken.layers[0]
        .text_frame
        .as_mut()
        .expect("frame")
        .next_text_frame = Some("SLYR-00000000-0000-7000-8000-0000000000ff".into());
    assert_eq!(
        go(&broken, &all, &ok).unwrap_err(),
        IdmlError::Folio(Code::DanglingReference)
    );
    // Stored content needs resolved text; non-area stories are not exported.
    let mut stored = c.stories[1].clone();
    stored.content = StoryContent::Stored {
        artifact_manifest_id: "manifest-1".into(),
        content_digest: hsk_studio_folio::ContentDigest {
            algorithm: "sha256".into(),
            digest: "00".into(),
        },
        byte_len: 4,
    };
    let with_stored = [
        all[0],
        StoryInput {
            story: &stored,
            resolved_text: None,
        },
    ];
    assert_eq!(
        go(&c.doc, &with_stored, &ok).unwrap_err(),
        IdmlError::Unsupported("story_content_not_resolved")
    );
    let resolved = [
        all[0],
        StoryInput {
            story: &stored,
            resolved_text: Some("solo"),
        },
    ];
    assert!(go(&c.doc, &resolved, &ok).is_ok());
    let mut point = c.stories[1].clone();
    point.text_kind = StoryTextKind::Point;
    let with_point = [
        all[0],
        StoryInput {
            story: &point,
            resolved_text: None,
        },
    ];
    assert_eq!(
        go(&c.doc, &with_point, &ok).unwrap_err(),
        IdmlError::Unsupported("story_text_kind_not_area")
    );
    // Pre-cancelled.
    let stop = CancellationToken::default();
    stop.cancel();
    assert_eq!(go(&c.doc, &all, &stop).unwrap_err(), IdmlError::Canceled);
}
