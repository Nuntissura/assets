//! Lane SMB-SENTINELS-NATIVE-1, item 2 (R53 finding F-R53-3): the STU-TYP-207 stream sentinels
//! (Master Spec v02.229 with the v02.228 SD-29 assignment) composed through the real Type engine,
//! not a closure shaper. Host composition: `hsk_studio_layout::compose_with_sentinels` with a
//! `SentinelShaper` that hands every paragraph to `hsk_studio_type::TextEngine` as one request
//! over the whole composition text whose paragraphs are layout's partition, so Type's request
//! validation (the R52 InvalidBidi refusal) judges layout's paragraph partition on every call.
//!
//! Font: Inter Regular 8 pt, the hash-pinned OFL fixture of hsk-studio-type (its
//! tests/fixtures/FIXTURES_PROVENANCE.json, resolved from this repository, never a machine path);
//! line height 12 pt, ascent 10 pt, fixed first baseline. Glyph advances are Type's, so the
//! assertions are on placement (frame, column, page), marker states, break outcomes, flush-space
//! slack and story bytes, never on a hand-computed width. The sentinel code points are typed in
//! from the spec table. The anchored-object marker (U+FFFC, a real character) is out of scope as
//! in SMB-LAYOUT-SENTINELS-1. Vendor parity UNVERIFIED (D5).
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
const XREF: char = '\u{FDD6}';
const INDEX: char = '\u{FDD7}';
const COLUMN: char = '\u{FDD8}';
const FRAME: char = '\u{FDD9}';
const PAGE: char = '\u{FDDA}';
const ODD: char = '\u{FDDB}';
const EVEN: char = '\u{FDDC}';
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
fn stored(st: &StudioTextStory) -> &str {
    match &st.content {
        StoryContent::Inline { text_utf8 } => text_utf8,
        StoryContent::Stored { .. } => panic!("stored"),
    }
}
#[allow(clippy::too_many_arguments)]
fn section(key: &str, start: &str, style: StudioNumberStyle, prefix: Option<&str>, marker: &str) -> StudioSection {
    StudioSection {
        section_id: key.into(),
        name: format!("section {key}"),
        page_start: start.into(),
        continue_numbering: false,
        page_number_start: Some(1),
        page_number_style: style,
        include_section_prefix: prefix.is_some(),
        section_prefix: prefix.map(Into::into),
        marker: Some(marker.into()),
    }
}
/// (thread position, column, page id) of the non-empty line starting at story byte `start`.
fn line_at(out: &SentinelComposition, doc: &StudioDocument, start: u64) -> (usize, usize, String) {
    for (pos, f) in out.layout.frames.iter().enumerate() {
        for l in &f.lines {
            if l.source.start == start && l.source.end > start {
                return (pos, l.column, doc.page_spreads[f.spread].pages[f.page].page_id.clone());
            }
        }
    }
    panic!("no line starts at {start}")
}
fn byte_of(text: &str, c: char, nth: usize) -> u64 {
    text.char_indices().filter(|(_, x)| *x == c).nth(nth).map(|(i, _)| i as u64).unwrap_or_else(|| panic!("U+{:04X} #{nth}", c as u32))
}

/// Host `SentinelShaper` over the real Type engine: every call is one Type request over the whole
/// composition text whose paragraphs are layout's partition, so Type's request validation judges
/// that partition each time. A refusal (InvalidBidi or any other) is recorded and refuses the
/// composition.
struct Native<'f> {
    font: &'f FontFile,
    calls: usize,
    refusals: Vec<(usize, ty::Error)>,
    texts: Vec<String>,
}
impl<'f> Native<'f> {
    fn new(font: &'f FontFile) -> Self {
        Self { font, calls: 0, refusals: Vec::new(), texts: Vec::new() }
    }
}
impl SentinelShaper for Native<'_> {
    fn shape(&mut self, input: &ShapeInput<'_>) -> Result<ShapedParagraph, LayoutError> {
        let partition = paragraph_partition(input.text);
        assert_eq!(partition.get(input.index), Some(&input.paragraph), "layout hands Type its own partition");
        self.calls += 1;
        if !self.texts.iter().any(|t| t == input.text) {
            self.texts.push(input.text.to_owned());
        }
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

#[test]
fn mt_31005_a_multi_page_multi_section_story_composes_through_native_type() {
    let font = inter();
    // Seven paragraphs; every marker member, the break members column, frame, page, odd-page,
    // even-page (sentinels) and paragraph return (U+2029), and flush space. The forced line break
    // has its own test (`mt_31008_a_forced_line_break_through_native_type`).
    let paragraphs = [
        format!("A{CUR}|{NEXT}|{PREV}|{SECTION}{COLUMN}"),
        format!("B{FOOTNOTE}|{ENDNOTE}|{XREF}|{INDEX}{FRAME}"),
        format!("C{FLUSH}D{PAGE}"),
        format!("E{CUR}{ODD}"),
        format!("F{CUR}{EVEN}"),
        format!("G{SECTION}\u{2029}"),
        "HI".to_owned(),
    ];
    let text: String = paragraphs.concat();
    let st = story_of(&text);
    let before = stored(&st).as_bytes().to_vec();
    // Thread: frame 1 (page-1, two 90 pt columns), frame 2 (page-1), frame 3 (page-2), frame 4
    // (page-3), each two lines high, so every break moves text that would otherwise still fit;
    // frame 5 (page-4, three lines).
    let layers = vec![
        frame(1, 5, 0, 10.0, 24.0, 2),
        frame(2, 5, 0, 100.0, 24.0, 1),
        frame(3, 5, 1, 10.0, 24.0, 1),
        frame(4, 5, 2, 10.0, 24.0, 1),
        frame(5, 5, 3, 10.0, 36.0, 1),
    ];
    let doc = document(layers);
    // Section "Intro" arabic from 1 on page-1; "Body" upper roman from 1, prefix "B-", on page-3.
    let sections = [
        section("s1", "page-1", StudioNumberStyle::Arabic, None, "Intro"),
        section("s2", "page-3", StudioNumberStyle::UpperRoman, Some("B-"), "Body"),
    ];
    let generated = |c: char, kind, rid: &str, t: &str| MarkerRecord {
        story_byte: byte_of(&text, c, 0),
        kind,
        target: MarkerTarget::Generated { record_id: rid.into(), text: t.into() },
    };
    let records = [
        generated(FOOTNOTE, MarkerKind::FootnoteReference, "fn-1", "7"),
        generated(ENDNOTE, MarkerKind::EndnoteReference, "en-1", "iv"),
        generated(XREF, MarkerKind::CrossReference, "xr-1", "see p. 3"),
    ];
    let req = SentinelRequest {
        document: &doc,
        story: &st,
        resolved_text: None,
        obstacles: &[],
        composer: Composer::FirstFit,
        space: SpaceParams::default(),
        expected_revision: 2,
        limits: Limits::default(),
        sections: &sections,
        markers: &records,
        budget: SentinelBudget::default(),
    };
    let opts = TypesetOptions { alignment: Alignment::JustifiedAll, ..TypesetOptions::default() };
    let mut native = Native::new(&font);
    let out = compose_with_sentinels(&req, &opts, &StudioLayoutFlowRecords::new(id("SDOC", 1)), &mut native, &CancellationToken::default());
    assert_eq!(native.refusals, vec![], "Type accepted layout's paragraph partition on every call (no InvalidBidi)");
    let out = out.expect("composed through native Type");
    assert_eq!(native.calls, paragraphs.len() * out.passes, "one Type request per paragraph and pass");
    assert!(native.texts.iter().all(|t| !t.chars().any(|c| ('\u{FDD0}'..='\u{FDD7}').contains(&c) || c == FLUSH)),
        "Type never received a marker or flush sentinel");

    // The stored story is unchanged; the composed paragraphs are the story's partition.
    assert_eq!(stored(&st).as_bytes(), before.as_slice());
    let partition = paragraph_partition(&text);
    assert_eq!(partition.len(), paragraphs.len());
    assert_eq!(out.paragraphs.iter().map(|p| p.source).collect::<Vec<_>>(), partition);
    assert_eq!(out.layout.overset, None);

    // Markers resolve per page.
    use UnresolvedMarker as U;
    let r = |s: &str| MarkerState::Resolved(s.into());
    let u = MarkerState::Unresolved;
    let m = |c: char, nth: usize, kind: MarkerKind, page: &str, state: MarkerState| MarkerOutcome {
        story_byte: byte_of(&text, c, nth),
        kind,
        page_id: Some(page.into()),
        state,
    };
    assert_eq!(
        out.markers,
        vec![
            m(CUR, 0, MarkerKind::CurrentPageNumber, "page-1", r("1")),
            m(NEXT, 0, MarkerKind::NextPageNumber, "page-1", r("2")),
            m(PREV, 0, MarkerKind::PreviousPageNumber, "page-1", u(U::NoPreviousPage)),
            m(SECTION, 0, MarkerKind::SectionMarker, "page-1", r("Intro")),
            m(FOOTNOTE, 0, MarkerKind::FootnoteReference, "page-1", r("7")),
            m(ENDNOTE, 0, MarkerKind::EndnoteReference, "page-1", r("iv")),
            m(XREF, 0, MarkerKind::CrossReference, "page-1", r("see p. 3")),
            m(INDEX, 0, MarkerKind::Index, "page-1", u(U::RuleUnstated)),
            m(CUR, 1, MarkerKind::CurrentPageNumber, "page-2", r("2")),
            m(CUR, 2, MarkerKind::CurrentPageNumber, "page-3", r("B-I")),
            m(SECTION, 1, MarkerKind::SectionMarker, "page-4", r("Body")),
        ]
    );
    // A resolved marker is shaped by Type and its glyphs carry the sentinel's 3-byte cluster; an
    // unresolved one has no glyph.
    for o in &out.markers {
        let cluster = SourceSpan { start: o.story_byte, end: o.story_byte + 3 };
        let glyphs = out.paragraphs.iter().flat_map(|p| &p.glyphs).filter(|g| g.cluster == cluster).count();
        match &o.state {
            MarkerState::Resolved(_) => assert!(glyphs >= 1, "{:?} at {}", o.kind, o.story_byte),
            MarkerState::Unresolved(_) => assert_eq!(glyphs, 0, "{:?} at {}", o.kind, o.story_byte),
        }
    }

    // Breaks reach their targets: column 1 of frame 1, frame 2, the page-2 frame, the first
    // odd-numbered page (page-3 = B-I), the first even-numbered page (page-4 = B-II).
    let applied = |c: char, target, frame, column| BreakOutcome {
        story_byte: byte_of(&text, c, 0),
        target,
        result: BreakResult::Applied { frame, column, skipped_frames: 0 },
    };
    use ty::BreakTarget as T;
    assert_eq!(
        out.breaks,
        vec![
            applied(COLUMN, T::Column, 0, 1),
            applied(FRAME, T::Frame, 1, 0),
            applied(PAGE, T::Page, 2, 0),
            applied(ODD, T::OddPage, 3, 0),
            applied(EVEN, T::EvenPage, 4, 0),
        ]
    );
    let starts: Vec<u64> = partition.iter().map(|p| p.start).collect();
    // (thread position, column, page) of each paragraph's first line; frame 2 is on page-1.
    let expected = [(0, 0, "page-1"), (0, 1, "page-1"), (1, 0, "page-1"), (2, 0, "page-2"), (3, 0, "page-3"), (4, 0, "page-4"), (4, 0, "page-4")];
    for (i, (start, (pos, column, page))) in starts.iter().zip(expected).enumerate() {
        let (got_pos, got_column, got_page) = line_at(&out, &doc, *start);
        assert_eq!((got_pos, got_column, got_page.as_str()), (pos, column, page), "paragraph {i}");
    }

    // Flush space: a zero-advance glyph that absorbs the slack of the fully justified last line.
    let flush_byte = byte_of(&text, FLUSH, 0);
    assert_eq!(out.flush_spaces.len(), 1);
    let flush = out.flush_spaces[0];
    assert_eq!((flush.story_byte, flush.frame, flush.line), (flush_byte, 1, 0));
    let line = &out.layout.frames[1].lines[0];
    let absorbed = flush.absorbed_pt.expect("fully justified last line");
    assert!(absorbed > 0.0 && (absorbed + line.width_pt - 180.0).abs() < 1e-6, "absorbed {absorbed} + width {} = slot", line.width_pt);
    let g = out.paragraphs[2].glyphs.iter().find(|g| g.cluster.start == flush_byte).unwrap();
    assert_eq!((g.cluster.end, g.advance_pt), (flush_byte + 3, 0.0));
}

#[test]
fn mt_31008_a_forced_line_break_through_native_type() {
    // "H" U+2028 "I" in one three-line frame: the forced line break ends the line, not the paragraph.
    let font = inter();
    let text = "H\u{2028}I";
    let st = story_of(text);
    assert_eq!(st.paragraphs.len(), 1, "U+2028 is no paragraph terminator");
    let doc = document(vec![frame(1, 1, 0, 10.0, 36.0, 1)]);
    let req = SentinelRequest {
        document: &doc,
        story: &st,
        resolved_text: None,
        obstacles: &[],
        composer: Composer::FirstFit,
        space: SpaceParams::default(),
        expected_revision: 2,
        limits: Limits::default(),
        sections: &[],
        markers: &[],
        budget: SentinelBudget::default(),
    };
    let mut native = Native::new(&font);
    let out = compose_with_sentinels(&req, &TypesetOptions::default(), &StudioLayoutFlowRecords::new(id("SDOC", 1)), &mut native, &CancellationToken::default());
    // Type's request validation accepts the one-paragraph partition; shaping may still refuse.
    assert!(native.refusals.iter().all(|(_, e)| *e != ty::Error::InvalidBidi), "{:?}", native.refusals);
    if let Some((_, e)) = native.refusals.first() {
        println!(
            "NOT_PROVEN: mt_31008_a_forced_line_break_through_native_type forced_line_break reason=type_refuses_u2028_{e:?} (Type shapes U+2028 as content; the Inter fixture maps no glyph to it; lane SMB-SENTINELS-NATIVE-1 notes)"
        );
        return;
    }
    let out = out.expect("composed through native Type");
    assert_eq!(out.layout.frames[0].lines.len(), 2, "the forced line break ends the first line");
    assert_eq!(line_at(&out, &doc, 4).0, 0);
}
