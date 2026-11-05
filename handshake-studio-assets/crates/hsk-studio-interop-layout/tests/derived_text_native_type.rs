//! Lane SMB-LAYOUT-ENTRY-TEXT-1, items 1 and 2 through the real Type engine (R55 F-R55-5 and F-R55-9,
//! validator probes n7a-n7e; Master Spec v02.229 STU-TYP-207). The source story is composed by
//! `compose_with_sentinels` with a Type-backed shaper; the TOC, running and cross-reference text
//! derived from it is then handed to Type directly and, for the TOC, composed like any other story.
//! Before the lane direct Type refused the text (MissingGlyph, n7a-c), a current-page marker resolved
//! against the TOC story's own page (n7c), and a heading ended by a break sentinel lost its page label
//! (n7d). Composition and derived text share one value rule (`sentinels::marker_value`) but find a
//! marker's page separately (`place_markers`, `toc::line_of`): derived text must show the value
//! `compose_with_sentinels` resolves at the same source byte.
//!
//! The Type ports, fixtures and font are the ones of `tests/sentinels_native_type.rs` (test-only copy:
//! one test target cannot import another). Inter Regular 8 pt, hash-pinned. U+FFFC is out of scope
//! (F-R55-13: no fixture font maps it, so a source holding it does not compose natively).
//! InDesign's handling of markers in a TOC entry UNVERIFIED (D5).
mod common;
use common::text_shaping::FontFile;
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::*;
use hsk_studio_layout::*;
use hsk_studio_type as ty;
use hsk_studio_type::AdmissionPort as _;
use sha2::{Digest, Sha256};
use std::sync::Mutex;

const CUR: char = '\u{FDD0}';
const NEXT: char = '\u{FDD1}';
const PREV: char = '\u{FDD2}';
const SECTION: char = '\u{FDD3}';
const FOOTNOTE: char = '\u{FDD4}';
const ENDNOTE: char = '\u{FDD5}';
const INDEX: char = '\u{FDD7}';
const PAGE: char = '\u{FDDA}';
const FLUSH: char = '\u{FDDD}';
const SIZE_PT: f64 = 8.0;
const LINE_PT: f64 = 12.0;
const ASCENT_PT: f64 = 10.0;

// ---- Type ports (caller side, as tests/common/text_shaping.rs, for multi-paragraph requests) ----

#[derive(Default)]
struct LedgerState {
    current: u64,
    peak: u64,
    retained: u64,
    next: u64,
}
struct Ledger(Mutex<LedgerState>);
impl ty::RetirementPort for Ledger {
    fn retire(&self, _: u64, n: u64) {
        self.0.lock().unwrap().current -= n;
    }
}
impl ty::GenerationRetirement for Ledger {
    fn retire_generation(&self, _: u64) {
        self.0.lock().unwrap().retained -= 1;
    }
}
impl ty::AdmissionPort for Ledger {
    fn reserve(&self, n: u64, limit: u64) -> Result<ty::Reservation<'_>, ty::Error> {
        let mut s = self.0.lock().unwrap();
        let next = s.current.checked_add(n).ok_or(ty::Error::Overflow)?;
        if n == 0 || next > limit {
            return Err(ty::Error::Budget);
        }
        s.current = next;
        s.peak = s.peak.max(next);
        s.next += 1;
        let handle = s.next;
        drop(s);
        ty::Reservation::new(self, handle, n)
    }
    fn snapshot(&self) -> Result<ty::AllocationSnapshot, ty::Error> {
        let s = self.0.lock().unwrap();
        Ok(ty::AllocationSnapshot {
            current_requested_bytes: s.current,
            operation_peak_requested_bytes: s.peak,
            historical_peak_requested_bytes: s.peak,
            retained_generations: s.retained,
        })
    }
    fn retain_generation(&self, limit: u64) -> Result<ty::Generation<'_>, ty::Error> {
        let mut s = self.0.lock().unwrap();
        if s.retained >= limit {
            return Err(ty::Error::Budget);
        }
        s.retained += 1;
        s.next += 1;
        let handle = s.next;
        drop(s);
        ty::Generation::new(self, handle)
    }
}
struct Fonts<'a> {
    file: &'a FontFile,
    ledger: &'a Ledger,
}
impl ty::FontResolver for Fonts<'_> {
    fn revision(&self) -> Result<u64, ty::Error> {
        Ok(1)
    }
    fn resolve(&self, r: &ty::FontResource<'_>) -> Result<ty::FontRead<'_>, ty::Error> {
        if r.identity != self.file.identity {
            return Err(ty::Error::MissingFont);
        }
        Ok(ty::FontRead {
            bytes: &self.file.bytes,
            resource_revision: 1,
            grant_revision: 1,
            lease: self.ledger.reserve(self.file.bytes.len() as u64, u64::MAX)?,
        })
    }
    fn verify_lifetime(&self, _: &ty::FontResource<'_>, read: &ty::FontRead<'_>) -> Result<(), ty::Error> {
        if read.lease.requested_bytes() < read.bytes.len() as u64 {
            return Err(ty::Error::LeaseUnavailable);
        }
        Ok(())
    }
}
struct Input(u64);
impl ty::InputLifetime for Input {
    fn verify(&self, _: &ty::Request<'_>) -> Result<ty::InputCharge, ty::Error> {
        Ok(ty::InputCharge { lifetime_id: 1, requested_bytes: self.0 })
    }
}
struct Scene<'a> {
    text: &'a str,
    styles: &'a [ty::Style<'a>],
}
impl ty::EpochPort for Scene<'_> {
    fn current_epoch(&self) -> Result<u64, ty::Error> {
        Ok(1)
    }
}
impl ty::SourceContext for Scene<'_> {
    fn member(&self, _: &ty::Request<'_>) -> Result<(), ty::Error> {
        Ok(())
    }
    fn read(&self, a: ty::ReadAddress<'_>) -> Result<ty::CurrentRead<'_>, ty::Error> {
        let bytes: &[u8] = match a.property {
            "text" => self.text.as_bytes(),
            "result" => b"original-preimage",
            _ => b"resolved-style",
        };
        Ok(ty::CurrentRead { revision: 1, fingerprint: hsk_studio_prism::profile_hash(bytes), bytes })
    }
    fn resolved_style(&self, a: ty::ReadAddress<'_>) -> Result<ty::Style<'_>, ty::Error> {
        let index = a.property.strip_prefix("style").and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
        self.styles.iter().find(|s| s.read_index == index).copied().ok_or(ty::Error::AbsentRead)
    }
}
fn type_limits() -> ty::Limits {
    ty::Limits {
        input_bytes: 4096,
        input_scalars: 4096,
        styles: 32,
        features: 64,
        axes: 16,
        font_resources: 32,
        font_bytes: 16 << 20,
        parser_tables: 128,
        lookups: 65536,
        runs: 128,
        glyphs: 4096,
        fallback_attempts: 64,
        recursion: 64,
        work_units: 50_000_000,
        requested_owned_bytes: 1 << 40,
        retained_generations: 8,
    }
}
fn domain(prefix: &str) -> DomainId {
    DomainId::parse(&format!("{prefix}-019abcde-0000-7000-8000-000000000001")).unwrap()
}

/// One Type request over the whole composition `text`: one Latin style (Inter, 8 pt) and the
/// paragraphs `partition` (left to right, explicit). Type refusals come back as the error.
fn type_shape<R>(
    font: &FontFile,
    text: &str,
    partition: &[SourceSpan],
    body: impl for<'a> FnOnce(&ty::Prepared<'a>) -> R,
) -> Result<R, ty::Error> {
    let (document, story, layer, style_id) = (domain("SDOC"), domain("STXT"), domain("SLYR"), domain("STYS"));
    let actor = ActorContext::new("account", "principal", "owner", "owner-principal", "space", "session").unwrap();
    let cancel = CancellationToken::default();
    let ledger = Ledger(Mutex::new(LedgerState::default()));
    let resolver = Fonts { file: font, ledger: &ledger };
    let candidates = [font.resource()];
    let axes: [ty::Axis; 0] = [];
    let styles = [ty::Style {
        read_index: 1,
        range: ty::SourceRange { start: 0, end: text.len() as u64 },
        font_size_pt: SIZE_PT,
        requested_identity: font.identity,
        candidates: &candidates,
        substitution: None,
        script: *b"Latn",
        language: "en",
        character_override: ty::CharacterOverride::Default,
        features: &[],
        axes: &axes,
    }];
    let reads = [
        ty::ReadAddress {
            owner: &story,
            property: "text",
            expected_revision: 1,
            fingerprint: hsk_studio_prism::profile_hash(text.as_bytes()),
        },
        ty::ReadAddress {
            owner: &style_id,
            property: "style1",
            expected_revision: 1,
            fingerprint: hsk_studio_prism::profile_hash(b"resolved-style"),
        },
    ];
    let paragraphs: Vec<ty::ParagraphInput> = partition
        .iter()
        .map(|p| ty::ParagraphInput {
            range: ty::SourceRange { start: p.start, end: p.end },
            direction: ty::Direction::LeftToRight,
            provenance: ty::DirectionProvenance::Explicit,
        })
        .collect();
    let input_lease = ledger.reserve(1 << 20, u64::MAX).unwrap();
    let input = Input(1 << 20);
    let scene = Scene { text, styles: &styles };
    let ports = ty::Ports { context: &scene, resolver: &resolver, input_lifetime: &input, admission: &ledger };
    let request = ty::Request {
        document_id: &document,
        document_revision: 1,
        story_id: &story,
        layer_id: &layer,
        text_utf8: text,
        text_sha256: hsk_studio_prism::profile_hash(text.as_bytes()),
        text_read_index: 0,
        reads: &reads,
        result_target: ty::ResultTarget {
            address: ty::ReadAddress {
                owner: &story,
                property: "result",
                expected_revision: 1,
                fingerprint: hsk_studio_prism::profile_hash(b"original-preimage"),
            },
            preimage: b"original-preimage",
        },
        composition_version: ty::COMPOSITION_VERSION,
        normalization: ty::Normalization::Preserve,
        story_direction: ty::StoryDirection::LeftToRight,
        paragraphs: &paragraphs,
        styles: &styles,
        resolver_revision: 1,
        cancel_epoch: 1,
        cancellation: &cancel,
        actor: &actor,
        correlation_id: "interop-layout-sentinels",
        observation_correlation: 1,
        limits: type_limits(),
    };
    let out = match ty::TextEngine.shaped_runs(request, &ports) {
        Ok(prepared) => Ok(body(&prepared)),
        Err(rejected) => Err(rejected.error),
    };
    drop(input_lease);
    out
}

/// The Type fixture font, checked against the type crate's provenance pin before use.
fn inter() -> FontFile {
    let font = FontFile::inter();
    let provenance: serde_json::Value = serde_json::from_slice(
        &std::fs::read(format!("{}/../hsk-studio-type/tests/fixtures/FIXTURES_PROVENANCE.json", env!("CARGO_MANIFEST_DIR"))).unwrap(),
    )
    .unwrap();
    let pinned = provenance["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["file"] == "fonts/Inter-Regular.ttf")
        .and_then(|f| f["sha256"].as_str())
        .unwrap()
        .to_owned();
    let actual: String = Sha256::digest(&font.bytes).iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(actual, pinned, "Inter-Regular.ttf is the pinned Type fixture");
    font
}

// ---- Folio fixtures ----

fn id(prefix: &str, n: u32) -> String {
    format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
}
fn len(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
fn edges(v: f64) -> LayoutEdges {
    LayoutEdges { top: len(v), right: len(v), bottom: len(v), left: len(v) }
}
fn page(key: &str, side: PageSide) -> StudioPage {
    StudioPage { page_id: key.into(), width: len(200.0), height: len(200.0), side, margins: edges(10.0), bleed: None, applied_trap_preset: None }
}
/// Text frame `n` of story 1 on page index `page`, threaded to `n - 1` / `n + 1` of `count`.
fn frame(n: u32, count: u32, page: usize, y: f64, h: f64, columns: u32) -> StudioLayer {
    StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: id("SLYR", n),
        name: format!("frame {n}"),
        parent: Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: id("SPGS", 1 + page as u32 / 2) }),
        kind: "text".into(),
        payload: Payload::Text { story_id: id("STXT", 1) },
        visible: true,
        appearance: None,
        text_frame: Some(StudioTextFrame {
            page_id: format!("page-{}", page + 1),
            bounds: Bounds { x: len(10.0), y: len(y), width: len(180.0), height: len(h) },
            inset_spacing: edges(0.0),
            text_column_count: columns,
            text_column_gutter: len(0.0),
            first_baseline_offset: FirstBaseline::Fixed,
            minimum_first_baseline_offset: len(10.0),
            vertical_justification: VerticalJustification::Top,
            auto_sizing_type: AutoSizing::Off,
            previous_text_frame: (n > 1).then(|| id("SLYR", n - 1)),
            next_text_frame: (n < count).then(|| id("SLYR", n + 1)),
        }),
    }
}
/// Four 200 x 200 pt pages, two per spread (page-1 .. page-4).
fn document(layers: Vec<StudioLayer>) -> StudioDocument {
    let unassigned = || ProfileBinding::Unassigned(());
    let spread = |s: u32| StudioPageSpread {
        schema_id: PageSpreadSchema::V1,
        page_spread_id: id("SPGS", s),
        name: format!("spread {s}"),
        parent: Parent::Root(()),
        bounds: Bounds { x: len(0.0), y: len(0.0), width: len(400.0), height: len(200.0) },
        pages: vec![
            page(&format!("page-{}", 2 * s - 1), PageSide::LeftHand),
            page(&format!("page-{}", 2 * s), PageSide::RightHand),
        ],
    };
    StudioDocument {
        schema_id: DocumentSchema::V1,
        document_id: id("SDOC", 1),
        revision: 2,
        geometry_unit: GeometryUnit::Pt,
        colour_mode: 3,
        bits_per_channel: 8,
        working_space_rgb: unassigned(),
        working_space_cmyk: unassigned(),
        working_space_gray: unassigned(),
        working_space_spot: unassigned(),
        blending_space: unassigned(),
        artboards: vec![],
        page_spreads: vec![spread(1), spread(2)],
        layers,
        graph: StudioLayerGraph { schema_id: GraphSchema::V1, operations: vec![], edges: vec![], outputs: vec![] },
        sequences: vec![],
    }
}
fn story_of(text: &str) -> StudioTextStory {
    StudioTextStory {
        schema_id: TextStorySchema::V1,
        story_id: id("STXT", 1),
        text_kind: StoryTextKind::Area,
        story_direction: StoryDirection::LeftToRight,
        content: StoryContent::Inline { text_utf8: text.into() },
        paragraphs: paragraph_partition(text)
            .iter()
            .map(|p| StoryParagraph { start: p.start, end: p.end, style_ref: None, paragraph_direction: None })
            .collect(),
        character_ranges: vec![StoryRange { start: 0, end: text.len() as u64, style_ref: None, applied_language: None }],
    }
}

/// Host `SentinelShaper` over the real Type engine (as `sentinels_native_type.rs`).
struct Native<'f> {
    font: &'f FontFile,
    refusals: Vec<(usize, ty::Error)>,
}
impl SentinelShaper for Native<'_> {
    fn shape(&mut self, input: &ShapeInput<'_>) -> Result<ShapedParagraph, LayoutError> {
        let partition = paragraph_partition(input.text);
        assert_eq!(partition.get(input.index), Some(&input.paragraph), "layout hands Type its own partition");
        let shaped = type_shape(self.font, input.text, &partition, |prepared| {
            assert_eq!(prepared.paragraphs().len(), partition.len(), "Type keeps every paragraph of layout's partition");
            ShapedParagraph::from_type(&prepared.paragraphs()[input.index], LINE_PT, ASCENT_PT)
        });
        shaped.map_err(|e| {
            self.refusals.push((input.index, e));
            LayoutError::InvalidInput
        })
    }
}
fn compose(doc: &StudioDocument, st: &StudioTextStory, sections: &[StudioSection], font: &FontFile) -> SentinelComposition {
    let req = SentinelRequest {
        document: doc,
        story: st,
        resolved_text: None,
        obstacles: &[],
        composer: Composer::FirstFit,
        space: SpaceParams::default(),
        expected_revision: 2,
        limits: Limits::default(),
        sections,
        markers: &[],
        budget: SentinelBudget::default(),
    };
    let mut native = Native { font, refusals: Vec::new() };
    let out = compose_with_sentinels(&req, &TypesetOptions::default(), &StudioLayoutFlowRecords::new(id("SDOC", 1)), &mut native, &CancellationToken::default());
    assert_eq!(native.refusals, vec![], "Type refused a paragraph");
    out.expect("composes")
}
/// Frame 1 on page-1 holds one line, frame 2 on page-2 ten.
fn two_pages() -> StudioDocument {
    document(vec![frame(1, 2, 0, 10.0, 12.0, 1), frame(2, 2, 1, 10.0, 120.0, 1)])
}
fn toc_style() -> StudioTocStyle {
    StudioTocStyle {
        toc_id: "toc".into(),
        name: "Contents".into(),
        title: String::new(),
        title_style: None,
        include_overset: false,
        entries: vec![StudioTocEntry {
            paragraph_style: id("STYS", 1),
            format_style: Some(id("STYS", 11)),
            level: 1,
            page_number_position: TocPageNumberPosition::After,
            separator: " ".into(),
            sort_alphabet: false,
            page_number_style: None,
            separator_style: None,
        }],
        include_book_documents: None,
        include_hidden: None,
        run_in: None,
        create_bookmarks: None,
        make_anchor: None,
        remove_forced_line_break: None,
        numbered_paragraphs: None,
        set_story_direction: None,
        default_styled: false,
    }
}
struct Derived {
    entry: String,
    gaps: Vec<DerivedMarkerGap>,
    toc: GeneratedStory,
    running: String,
    xref: String,
    /// The source composition's marker outcomes.
    markers: Vec<MarkerOutcome>,
}
/// Paragraph `styled` of `text`, composed natively, with the text derived from it at every site.
/// The running text is shown in the frame of the page the paragraph starts on.
fn derive(text: &str, styled: usize, sections: &[StudioSection], font: &FontFile) -> Derived {
    let doc = two_pages();
    let mut st = story_of(text);
    st.paragraphs[styled].style_ref = Some(id("STYS", 1));
    let start = st.paragraphs[styled].start;
    let out = compose(&doc, &st, sections, font);
    let pages = number_pages(&doc, sections).unwrap();
    let source = [TocSource { story: &st, text, placement: Some(&out.layout) }];
    let cancel = CancellationToken::default();
    let toc = generate_toc(&doc, &toc_style(), &source, &pages, &id("STXT", 9), TocLimits::default(), &cancel).expect("toc");
    let vars = [StudioTextVariable {
        variable_id: "h1".into(),
        kind: StudioVariableKind::MatchParagraphStyle { style: id("STYS", 1), use_on_page: RunningUse::FirstOnPage, delete_end_punctuation: false, change_case: ChangeCase::None },
    }];
    let running = [StudioRunningText { running_id: "r".into(), frame_ids: vec![id("SLYR", 2)], parts: vec![StudioRunningPart::Variable { variable_id: "h1".into() }] }];
    let r = resolve_running_texts(&doc, &vars, &running, sections, &pages, &source, &VariableContext::default()).expect("running");
    let mut records = StudioLayoutFlowRecords::new(id("SDOC", 1));
    records.xref_formats.push(StudioXrefFormat { format_id: "f".into(), name: "f".into(), definition: "<fullPara/>".into(), character_style: None });
    records.cross_references.push(StudioCrossReference {
        xref_id: "x".into(),
        source_story_id: id("STXT", 2),
        source_start: 0,
        source_end: 0,
        destination: StudioXrefDestination::Paragraph { story_id: id("STXT", 1), paragraph_start: start },
        format_id: "f".into(),
        resolved_text: None,
    });
    let stories = [XrefStory { story: &st, text, placement: Some(&out.layout) }];
    let c = XrefContext { file_name: "doc", chapter_number: None, pages: &pages, type_styles: None };
    let x = resolve_cross_references(&records, &stories, &c).expect("xref").remove(0);
    let e = toc.entries.first().expect("one entry");
    Derived { entry: e.text.clone(), gaps: e.marker_gaps.clone(), running: r[0].text.clone(), xref: x.text.expect("text"), markers: out.markers, toc }
}
fn direct_type(font: &FontFile, text: &str) -> Result<usize, ty::Error> {
    type_shape(font, text, &paragraph_partition(text), |p| p.paragraphs().len())
}
/// The derived text goes through Type directly, and the TOC story composes as one placed story whose
/// partition is its own paragraphs, every byte on a line, with no marker of its own.
fn holds_through_type(d: &Derived, font: &FontFile) {
    for t in [&d.entry, &d.running, &d.xref, &d.toc.text] {
        assert_eq!(direct_type(font, t), Ok(paragraph_partition(t).len()), "{}", t.escape_unicode());
    }
    let spans: Vec<(u64, u64)> = d.toc.paragraphs.iter().map(|p| (p.start, p.end)).collect();
    let partition: Vec<(u64, u64)> = paragraph_partition(&d.toc.text).iter().map(|p| (p.start, p.end)).collect();
    assert_eq!(spans, partition);
    let doc = document(vec![frame(1, 1, 0, 10.0, 120.0, 1)]);
    let out = compose(&doc, &d.toc.to_story(&id("STXT", 1)), &[], font);
    assert_eq!(out.markers, vec![], "derived text carries no marker");
    assert_eq!(out.layout.overset, None);
    let lines: Vec<(u64, u64)> = out.layout.frames.iter().flat_map(|f| f.lines.iter().map(|l| (l.source.start, l.source.end))).collect();
    assert_eq!(lines, vec![(0, d.toc.text.len() as u64)], "the entry and its page label are placed");
}

#[test]
fn n7a_n7b_a_heading_with_a_marker_of_no_value_through_native_type() {
    let font = inter();
    for m in [INDEX, FOOTNOTE, ENDNOTE] {
        for heading in [format!("{m}Chapter\n"), format!("Chap{m}ter\n"), format!("Chapter{m}\n")] {
            let d = derive(&format!("{heading}Body\n"), 0, &[], &font);
            assert_eq!((d.entry.as_str(), d.toc.text.as_str(), d.gaps.as_slice()), ("Chapter", "Chapter 1", &[][..]));
            assert_eq!((d.running.as_str(), d.xref.as_str()), ("Chapter", "Chapter"));
            holds_through_type(&d, &font);
        }
    }
}

#[test]
fn n7c_a_current_page_marker_shows_the_page_composition_resolves_at_the_source() {
    let font = inter();
    let d = derive(&format!("Intro\nChapter {CUR}\n"), 1, &[], &font);
    // The heading's page, never page 1 where the generated story lands.
    assert_eq!(d.markers[0].state, MarkerState::Resolved("2".into()));
    assert_eq!((d.entry.as_str(), d.toc.text.as_str()), ("Chapter 2", "Chapter 2 2"));
    assert_eq!((d.running.as_str(), d.xref.as_str()), ("Chapter 2", "Chapter 2"));
    holds_through_type(&d, &font);
}

#[test]
fn derived_values_equal_the_values_composition_resolves() {
    let font = inter();
    let sections = [
        StudioSection {
            section_id: "s1".into(),
            name: "s1".into(),
            page_start: "page-1".into(),
            continue_numbering: false,
            page_number_start: Some(1),
            page_number_style: StudioNumberStyle::LowerRoman,
            include_section_prefix: false,
            section_prefix: None,
            marker: Some("Front".into()),
        },
        StudioSection {
            section_id: "s2".into(),
            name: "s2".into(),
            page_start: "page-2".into(),
            continue_numbering: false,
            page_number_start: Some(7),
            page_number_style: StudioNumberStyle::UpperRoman,
            include_section_prefix: true,
            section_prefix: Some("B-".into()),
            marker: Some("Body".into()),
        },
    ];
    for (m, want) in [(CUR, "B-VII"), (NEXT, "B-VIII"), (PREV, "i")] {
        let d = derive(&format!("Intro\nAt {m}\n"), 1, &sections, &font);
        assert_eq!(d.markers[0].state, MarkerState::Resolved(want.into()), "composition, U+{:04X}", m as u32);
        assert_eq!(d.running, format!("At {want}"), "derived, U+{:04X}", m as u32);
        assert_eq!(d.entry, format!("At {want}"));
        holds_through_type(&d, &font);
    }
    let d = derive(&format!("Intro\nAt {SECTION}\n"), 1, &sections, &font);
    assert_eq!(d.markers[0].state, MarkerState::Resolved("Body".into()));
    assert_eq!(d.running, "At Body");
    // The TOC call has no sections: typed, no value.
    assert_eq!(d.entry, "At ");
    assert_eq!(d.gaps[0].reason, DerivedGapReason::SectionsUnavailable);
    holds_through_type(&d, &font);
}

#[test]
fn a_marker_with_no_value_at_the_source_is_typed_as_composition_states_it() {
    let font = inter();
    let d = derive(&format!("A{PREV}\nB\n"), 0, &[], &font);
    assert_eq!(d.markers[0].state, MarkerState::Unresolved(UnresolvedMarker::NoPreviousPage));
    assert_eq!(d.gaps, vec![DerivedMarkerGap { story_byte: 1, kind: Some(MarkerKind::PreviousPageNumber), reason: DerivedGapReason::Marker(UnresolvedMarker::NoPreviousPage) }]);
    assert_eq!(d.entry, "A");
    holds_through_type(&d, &font);
}

#[test]
fn a_flush_space_in_a_heading_becomes_a_space_through_native_type() {
    let font = inter();
    let d = derive(&format!("A{FLUSH}B\nC\n"), 0, &[], &font);
    assert_eq!((d.entry.as_str(), d.toc.text.as_str()), ("A B", "A B 1"));
    holds_through_type(&d, &font);
}

#[test]
fn n7d_a_heading_ended_by_a_page_break_keeps_its_label_through_native_type() {
    let font = inter();
    let d = derive(&format!("Chapter{PAGE}Body\n"), 0, &[], &font);
    assert_eq!((d.entry.as_str(), d.toc.text.as_str()), ("Chapter", "Chapter 1"));
    holds_through_type(&d, &font);
}

#[test]
fn every_terminator_ends_the_entry_through_native_type() {
    let font = inter();
    let all: Vec<char> = (0..=0x10FFFFu32).filter_map(char::from_u32).filter(|c| ends_paragraph(*c)).collect();
    assert_eq!(all.len(), 12);
    for t in all {
        let d = derive(&format!("Chapter{t}Body\n"), 0, &[], &font);
        assert_eq!((d.entry.as_str(), d.toc.text.as_str()), ("Chapter", "Chapter 1"), "U+{:04X}", t as u32);
        assert_eq!((d.running.as_str(), d.xref.as_str()), ("Chapter", "Chapter"), "U+{:04X}", t as u32);
        holds_through_type(&d, &font);
    }
}

#[test]
fn a_heading_of_markers_only_gives_no_entry_through_native_type() {
    let font = inter();
    let toc_of = |text: &str, styled: usize| {
        let doc = two_pages();
        let mut st = story_of(text);
        st.paragraphs[styled].style_ref = Some(id("STYS", 1));
        let out = compose(&doc, &st, &[], &font);
        let pages = number_pages(&doc, &[]).unwrap();
        let source = [TocSource { story: &st, text, placement: Some(&out.layout) }];
        generate_toc(&doc, &toc_style(), &source, &pages, &id("STXT", 9), TocLimits::default(), &CancellationToken::default()).expect("toc")
    };
    for m in [INDEX, FOOTNOTE, ENDNOTE] {
        let g = toc_of(&format!("Intro\n{m}\nBody\n"), 1);
        assert_eq!((g.entries.len(), g.text.as_str(), g.omitted_marker_gaps.len()), (0, "", 0), "U+{:04X}", m as u32);
    }
    // A gap is still reported; a value is text and keeps the entry.
    let g = toc_of(&format!("A\n{PREV}\n"), 1);
    assert_eq!((g.entries[0].text.as_str(), g.text.as_str()), ("1", "1 2"));
    let g = toc_of(&format!("{PREV}\nB\n"), 0);
    assert_eq!(g.entries.len(), 0);
    let reasons: Vec<DerivedGapReason> = g.omitted_marker_gaps.iter().map(|(_, gap)| gap.reason).collect();
    assert_eq!(reasons, [DerivedGapReason::Marker(UnresolvedMarker::NoPreviousPage)]);
}

// ---- Lane SMB-LAYOUT-BLANK-LINE-LOOKUPS-1 (R56 F-R56-5, F-R56-9) through the real Type engine: a
// paragraph with no glyph has one placed line at its end. A mark or destination in it answers the page
// composition places it on; a heading with no text gives no entry and is counted. ----

const LS: char = '\u{2028}';

/// (index text, unplaced, `<pageNum/>` text) of a mark and a destination at byte `at`.
fn blank_line_answers(doc: &StudioDocument, st: &StudioTextStory, text: &str, layout: &LayoutProposal, at: u64) -> (String, usize, Option<String>) {
    let pages = number_pages(doc, &[]).unwrap();
    let source = [TocSource { story: st, text, placement: Some(layout) }];
    let options = StudioIndexOptions {
        title: "Index".into(),
        title_style: None,
        index_format: IndexFormat::Nested,
        include_section_headings: false,
        include_empty_index_sections: false,
        level_styles: vec![],
        section_heading_style: None,
        following_topic_separator: ", ".into(),
        between_entries_separator: "; ".into(),
        page_range_separator: "-".into(),
        between_page_numbers_separator: ", ".into(),
        before_cross_reference_separator: ". ".into(),
        entry_end_separator: String::new(),
        replace_existing_index: None,
        include_book_documents: None,
        include_hidden_entries: None,
        page_number_style: None,
        cross_reference_style: None,
        cross_reference_topic_style: None,
        section_header_set: None,
    };
    let marks = [StudioIndexMark { mark_id: "m".into(), story_id: id("STXT", 1), at, topics: vec!["t".into()], sort_by: vec![], xref: None, page_range_scope: None }];
    let index = generate_index(doc, &options, &marks, &source, &pages, None, IndexLimits::default(), &CancellationToken::default()).expect("index");
    let mut records = StudioLayoutFlowRecords::new(id("SDOC", 1));
    records.xref_formats.push(StudioXrefFormat { format_id: "f".into(), name: "f".into(), definition: "<pageNum/>".into(), character_style: None });
    records.cross_references.push(StudioCrossReference {
        xref_id: "x".into(),
        source_story_id: id("STXT", 2),
        source_start: 0,
        source_end: 0,
        destination: StudioXrefDestination::Paragraph { story_id: id("STXT", 1), paragraph_start: at },
        format_id: "f".into(),
        resolved_text: None,
    });
    let stories = [XrefStory { story: st, text, placement: Some(layout) }];
    let c = XrefContext { file_name: "doc", chapter_number: None, pages: &pages, type_styles: None };
    let x = resolve_cross_references(&records, &stories, &c).expect("xref").remove(0);
    (index.story.text, index.unplaced, x.text)
}

#[test]
fn a_mark_and_a_destination_on_a_blank_line_answer_the_page_of_composition_through_native_type() {
    let font = inter();
    let doc = two_pages();
    // The paragraph's first byte is at 6; the second member is the marker's byte when it holds one.
    let cases = [
        (String::new(), None),
        (format!("{INDEX}"), Some(6)),
        (format!("{INDEX}{LS}"), Some(6)),
        (format!("{LS}{INDEX}"), Some(9)),
        (format!("{LS}"), None),
        ("  ".to_owned(), None),
    ];
    for (heading, marker) in cases {
        let text = format!("Intro\n{heading}\nBody\n");
        let st = story_of(&text);
        let out = compose(&doc, &st, &[], &font);
        if let Some(byte) = marker {
            let m = out.markers.iter().find(|m| m.story_byte == byte).expect("the marker");
            assert_eq!(m.page_id.as_deref(), Some("page-2"), "{}", heading.escape_unicode());
        }
        let at = marker.unwrap_or(6);
        let got = blank_line_answers(&doc, &st, &text, &out.layout, at);
        assert_eq!(got, ("Index\nt, 2".to_owned(), 0, Some("2".to_owned())), "{}", heading.escape_unicode());
    }
}

#[test]
fn a_heading_with_no_text_is_counted_and_a_heading_after_a_blank_line_keeps_its_page_through_native_type() {
    let font = inter();
    let doc = two_pages();
    let pages = number_pages(&doc, &[]).unwrap();
    let toc_of = |text: &str, styled: usize| {
        let mut st = story_of(text);
        st.paragraphs[styled].style_ref = Some(id("STYS", 1));
        let out = compose(&doc, &st, &[], &font);
        let source = [TocSource { story: &st, text, placement: Some(&out.layout) }];
        generate_toc(&doc, &toc_style(), &source, &pages, &id("STXT", 9), TocLimits::default(), &CancellationToken::default()).expect("toc")
    };
    for heading in [String::new(), format!("{INDEX}"), format!("{LS}"), "  ".to_owned(), format!(" {INDEX}")] {
        let g = toc_of(&format!("Intro\n{heading}\nBody\n"), 1);
        assert_eq!((g.entries.len(), g.text.as_str(), g.omitted.no_text, g.omitted.overset), (0, "", 1, 0), "{}", heading.escape_unicode());
    }
    // The blank line on page-1 and "Head" on page-2 both start at byte 1.
    let g = toc_of("\nHead\n", 1);
    assert_eq!(g.text, "Head 2");
}
