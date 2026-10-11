//! Typed view of the supported IDML subset, the Folio proposal built from it, and the loss report.
//!
//! The model is the decoder's typed view of what it understood. Everything it does not interpret is
//! carried verbatim (`Opaque`, `Attrs`) so that `export_idml` can re-serialise it canonically and the
//! byte-exact path (`replace_content` + `write_idml`) never depends on the model at all. Fields marked
//! `derived` are read from verbatim children for the Folio projection and are not written back.
use hsk_studio_folio::{
    AutoSizing, Budget, FirstBaseline, ResolutionIssue, Snapshot, StudioTextStory,
    VerticalJustification,
};
use sha2::{Digest, Sha256};
use std::ops::Range;
use std::sync::Arc;

/// IDML / PostScript affine `[a b c d tx ty]` (point = (a*x + c*y + tx, b*x + d*y + ty)).
pub type Affine = [f64; 6];
pub const IDENTITY: Affine = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
/// XML attributes in source order, values unescaped.
pub type Attrs = Vec<(String, String)>;

/// Every bound the reader enforces before it trusts a size or a count.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Container size in bytes.
    pub max_bytes: usize,
    pub max_entries: usize,
    pub max_entry_bytes: u64,
    pub max_inflated_bytes: u64,
    /// Inflated / compressed ratio, judged for entries above 1 MiB inflated.
    pub max_ratio: u64,
    /// roxmltree node limit per XML part.
    pub xml_nodes: u32,
    /// Logical size of the decoded text model: for every paragraph and run, the bytes its
    /// canonical export writes (the shared range data it references, counted once per use, plus
    /// its text and a fixed per-record overhead). Bounds what `export_idml` can write for the
    /// supported subset, so a package that repeats one large `Properties` block over many
    /// `Content` elements is refused with `BudgetExceeded("model_bytes")`.
    pub max_model_bytes: u64,
    /// Budget handed to Folio when it validates the proposal.
    pub folio: Budget,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 128 << 20,
            max_entries: 4096,
            max_entry_bytes: 64 << 20,
            max_inflated_bytes: 256 << 20,
            max_ratio: 100,
            xml_nodes: 1_000_000,
            max_model_bytes: 256 << 20,
            folio: Budget {
                input_bytes: 32 << 20,
                nodes: 100_000,
                edges: 8192,
                payload_bytes: 32 << 20,
                depth: 32,
            },
        }
    }
}

/// One container entry. Every part is kept (unknown ones quarantined verbatim, STU-IO-005).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdmlPart {
    pub name: String,
    pub bytes: Vec<u8>,
    pub sha256: [u8; 32],
}
impl IdmlPart {
    pub fn new(name: String, bytes: Vec<u8>) -> Self {
        let sha256 = Sha256::digest(&bytes).into();
        Self {
            name,
            bytes,
            sha256,
        }
    }
}

/// A safely opened IDML container: all parts, in container order, `mimetype` first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdmlPackage {
    pub parts: Vec<IdmlPart>,
}
impl IdmlPackage {
    pub fn part(&self, name: &str) -> Option<&IdmlPart> {
        self.parts.iter().find(|p| p.name == name)
    }
    /// SHA-256 over every `(name, part sha256)` pair in order: the package content identity.
    pub fn fingerprint(&self) -> [u8; 32] {
        let mut h = Sha256::new();
        for p in &self.parts {
            h.update((p.name.len() as u64).to_le_bytes());
            h.update(p.name.as_bytes());
            h.update(p.sha256);
        }
        h.finalize().into()
    }
}

/// A child element kept as its exact source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opaque {
    /// Local element name.
    pub element: String,
    pub xml: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum DesignmapItem {
    Opaque(Opaque),
    /// Index into `IdmlModel::spreads`.
    Spread(usize),
    /// Index into `IdmlModel::stories`.
    Story(usize),
}

/// `derived` from `DocumentPreference` / `MarginPreference` in the preferences part.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocumentSettings {
    pub page_width_pt: Option<f64>,
    pub page_height_pt: Option<f64>,
    pub facing_pages: bool,
    /// top, bottom, inside_or_left, outside_or_right.
    pub bleed_pt: Option<[f64; 4]>,
    /// top, bottom, left, right (left/right are inside/outside on facing pages).
    pub margins_pt: Option<[f64; 4]>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IdmlModel {
    pub dom_version: String,
    /// Attributes of the designmap `Document` element except `StoryList` (regenerated).
    pub document_attrs: Attrs,
    pub designmap: Vec<DesignmapItem>,
    pub document: DocumentSettings,
    pub spreads: Vec<SpreadModel>,
    pub stories: Vec<StoryModel>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum SpreadItem {
    /// Index into `SpreadModel::pages`.
    Page(usize),
    /// Index into `SpreadModel::frames`.
    Frame(usize),
    /// Graphics, groups, guides, preferences, anything not interpreted: verbatim, in document order.
    Opaque(Opaque),
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpreadModel {
    pub id: String,
    pub part: String,
    pub item_transform: Affine,
    /// Attributes except `Self`, `PageCount`, `ItemTransform`.
    pub attrs: Attrs,
    pub pages: Vec<PageModel>,
    pub frames: Vec<FrameModel>,
    pub items: Vec<SpreadItem>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PageModel {
    pub id: String,
    pub name: String,
    /// `GeometricBounds` in page coordinates: top, left, bottom, right (points).
    pub bounds_pt: [f64; 4],
    /// Page coordinates -> spread coordinates.
    pub item_transform: Affine,
    /// Attributes except `Self`, `Name`, `GeometricBounds`, `ItemTransform`.
    pub attrs: Attrs,
    /// Child elements (margin and grid preferences, properties), verbatim.
    pub children: Vec<Opaque>,
    /// `derived` from the `MarginPreference` child: top, bottom, left, right.
    pub margins_pt: Option<[f64; 4]>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PathPoint {
    pub anchor: [f64; 2],
    pub left: [f64; 2],
    pub right: [f64; 2],
}

/// `derived` from the `TextFramePreference` child (IDML defaults when absent).
#[derive(Clone, Debug, PartialEq)]
pub struct FramePrefs {
    pub column_count: u32,
    pub column_gutter_pt: f64,
    /// top, left, bottom, right (IDML list order).
    pub inset_pt: [f64; 4],
    pub vertical_justification: VerticalJustification,
    pub first_baseline: FirstBaseline,
    pub min_first_baseline_pt: f64,
    pub auto_size: AutoSizing,
}
impl Default for FramePrefs {
    fn default() -> Self {
        Self {
            column_count: 1,
            column_gutter_pt: 12.0,
            inset_pt: [0.0; 4],
            vertical_justification: VerticalJustification::Top,
            first_baseline: FirstBaseline::Ascent,
            min_first_baseline_pt: 0.0,
            auto_size: AutoSizing::Off,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FrameModel {
    pub id: String,
    pub story_id: String,
    /// `None` is the IDML value `n`.
    pub previous: Option<String>,
    pub next: Option<String>,
    /// Frame path coordinates -> spread coordinates.
    pub item_transform: Affine,
    /// Attributes except `Self`, `ParentStory`, `PreviousTextFrame`, `NextTextFrame`, `ItemTransform`.
    pub attrs: Attrs,
    /// Child elements (`Properties` with the path, `TextFramePreference`, wrap, ...), verbatim.
    pub children: Vec<Opaque>,
    /// `derived` from `Properties/PathGeometry` (first subpath).
    pub path: Vec<PathPoint>,
    pub path_open: bool,
    pub prefs: FramePrefs,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoryModel {
    pub id: String,
    pub part: String,
    /// Identity of the part bytes the spans of `runs` address.
    pub part_sha256: [u8; 32],
    /// Attributes except `Self`.
    pub attrs: Attrs,
    /// Children before the first paragraph range (`StoryPreference`, ...), verbatim.
    pub head: Vec<Opaque>,
    /// `derived` from `StoryPreference@StoryDirection`.
    pub right_to_left: bool,
    pub paragraphs: Vec<ParaModel>,
    /// `derived`: codes of composition this slice does not lay out (RTL, CJK, vertical, ...).
    pub composition: Vec<&'static str>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParaModel {
    /// `AppliedParagraphStyle` as written, for example `ParagraphStyle/$ID/NormalParagraphStyle`.
    /// Shared (`Arc`) by every paragraph the same `ParagraphStyleRange` yields.
    pub style: Arc<str>,
    /// `ParagraphStyleRange` attributes except `AppliedParagraphStyle`; shared like `style`.
    pub attrs: Arc<Attrs>,
    /// The range's `Properties` element, verbatim; shared like `style`.
    pub props: Option<Arc<Opaque>>,
    pub runs: Vec<RunModel>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunModel {
    /// `AppliedCharacterStyle` as written. `char_style`, `attrs`, `props` and `language` are shared
    /// (`Arc`) by every run the same `CharacterStyleRange` yields: one `Content` or `Br` costs a
    /// pointer, never a copy of the range's attributes or `Properties` XML.
    pub char_style: Arc<str>,
    pub attrs: Arc<Attrs>,
    pub props: Option<Arc<Opaque>>,
    pub text: String,
    /// A `Br` ends the paragraph after this run's text.
    pub br: bool,
    /// The `Content` held special-character processing instructions that are not in `text`.
    pub special: bool,
    /// `derived`: `AppliedLanguage` as written.
    pub language: Option<Arc<str>>,
    /// Byte range of the `Content` element in the story part (`None` for a bare `Br`).
    pub span: Option<Range<usize>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LossKind {
    /// Not interpreted into the proposal; kept verbatim by both write paths.
    PreservedOpaque,
    /// Not interpreted and dropped by `export_idml`; only the byte-exact path keeps it.
    Unsupported,
    /// Interpreted with reduced precision in the proposal.
    Approximated,
    /// Composition this slice cannot lay out; Latin proof never certifies it.
    UnsupportedComposition,
}
impl LossKind {
    pub const fn wire(self) -> &'static str {
        match self {
            Self::PreservedOpaque => "preserved_opaque",
            Self::Unsupported => "unsupported",
            Self::Approximated => "approximated",
            Self::UnsupportedComposition => "unsupported_composition",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct LossEntry {
    pub part: String,
    pub element: String,
    pub kind: LossKind,
    pub code: &'static str,
    pub count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdLink {
    /// `spread`, `page`, `frame`, `story`.
    pub kind: &'static str,
    pub idml: String,
    pub folio: String,
}

/// A style id Folio holds by reference. The definition is not imported (it stays in
/// `Resources/Styles.xml`), so Folio reports it unresolved until a Type style owner defines it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyleRef {
    pub folio: String,
    pub idml_name: String,
    pub paragraph: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProposedStory {
    pub story: StudioTextStory,
    pub idml_id: String,
    pub part: String,
    /// False when `composition` is non-empty: a Latin reflow must not be run on this story.
    pub reflow_certified: bool,
    pub composition: Vec<&'static str>,
}

#[derive(Clone, Debug)]
pub struct LayoutProposal {
    /// Folio document: page spreads with pages, text-frame layers with thread links.
    pub snapshot: Snapshot,
    pub stories: Vec<ProposedStory>,
    pub style_refs: Vec<StyleRef>,
    /// Unresolved style/content references reported while validating the stories.
    pub unresolved: Vec<ResolutionIssue>,
    pub ids: Vec<IdLink>,
}

#[derive(Clone, Debug)]
pub struct IdmlImport {
    pub package: IdmlPackage,
    pub model: IdmlModel,
    pub proposal: LayoutProposal,
    pub loss: Vec<LossEntry>,
}
