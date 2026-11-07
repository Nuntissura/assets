//! Layout output from the host (lane SMB-LAYOUT-PDF-HOST-1; STU-LAY-054, STU-LAY-061, STU-LAY-154,
//! STU-LAY-158): the live [`LayoutDoc`] of the Layout Editor written as PDF through
//! hsk-studio-interop-layout's `export_pdf_with` and as IDML through its `export_folio_longdoc`, a
//! preflight run before every PDF export, the page size of a new layout, and a page of any PDF
//! rendered to PNG (press `render_page`) so a model can look at what it wrote. No draw code: every
//! entry point is a [`Viewer`] method, driven as `viewer.<method>` and named by the `layout.*`
//! rows of the command corpus.
//!
//! # Manual (no-context operator or model)
//!
//! - [`Viewer::layout_open`]: opens the Layout Editor (WORKSPACE > Panels > Layout Editor). With a
//!   page size or a page count it starts a new blank layout of `pages` single-sided pages of that
//!   size in points, each on its own spread (a deck of 1920 x 1080 pt slides, an A4 landscape
//!   guide), only while the open document has no pages and the editor holds no edits:
//!   `layout_document_has_pages`, `layout_unsaved_edits`. With a document open the pages become its
//!   pages as one committed layout step `layout.open` (lane SMB-LAYOUT-PERSIST-1; refused
//!   `layout_commit_refused` with the session's code); with nothing open the layout stays the editor's. Size bounds: 1 to 14400 pt per side
//!   (`page_size_out_of_range`; 14400 pt = 200 in, the classic PDF page limit, UNVERIFIED against
//!   Acrobat), 1 to 1000 pages (`page_count_out_of_range`).
//! - [`Viewer::layout_preflight`]: the editor's live preflight (layout's engine, basic profile)
//!   as JSON: `ok`, error, warning and informational counts, findings.
//! - [`Viewer::export_layout_pdf`]: preflight first; any error-severity finding refuses
//!   `preflight_errors` with the counts and the first findings, unless `ignore_preflight`. Then
//!   every story placed in a text frame is shaped by hsk-studio-type with the viewer's fonts (the
//!   character style's family and size, else the paragraph style's, else the bundled
//!   `Ubuntu-Light` 12 pt; a family the viewer does not have is drawn in the bundled font and
//!   reported `font_substituted`; a paragraph terminator inside one Folio paragraph splits the
//!   shaping only: layout sets it as a line break, the paragraph boundary is lost (the text after
//!   it keeps the first paragraph's style), reported `paragraph_break_inside_paragraph_set_as_line_break`), composed by layout (`reflow_flow`, first-fit, line height 1.25
//!   em and ascent 0.75 em of the paragraph's largest size: the editor's convention), and lowered by
//!   `export_pdf_with` with Type's glyph ids, page labels from the Studio sections
//!   (`number_pages`, arabic from 1 without sections), TrimBox and BleedBox from the pages. Options:
//!   `pdfx` (`pdfx.none` or `pdfx.x4_2010`, which needs `created`, a PDF date, and an output
//!   profile), an output intent from an ICC file (`output_profile`; components from its header),
//!   `title`, `subset_fonts`. Unstyled text is black in the intent's colour space (DeviceCMYK for a
//!   CMYK intent, DeviceGray for gray, else DeviceRGB). PDF/X-4 output is self-checked (press
//!   `check_pdfx4`): a failed rule refuses `pdfx_self_check_failed` before anything is written, a
//!   passing report is returned; conformance stays `not_proven` (STU-PDF-002). The result lists the engine's loss
//!   entries and this host's own (`host_loss`), page ids and sizes, glyph, path and tile counts,
//!   sha256 and the job settlement.
//! - [`Viewer::export_layout_idml`]: the same live document as an IDML package; style names are
//!   the Folio type styles' own (`ParagraphStyle/<name>`, `CharacterStyle/<name>`); losses listed.
//! - [`Viewer::render_pdf_page`]: page `page` (0-based) of the PDF file `source` at `px_per_pt`
//!   pixels per point, written as an 8-bit RGB PNG; returns size, non-white pixel count and the
//!   paint device's notes.
//! - Writes: atomic through `export::write_atomic` (a created-exclusive sibling, then rename);
//!   an existing file needs `overwrite` (`overwrite_required`). Exports are derived deliverables and
//!   change no document state, so they are not undo steps.
//! - Admission and diagnostics: each export or render is one job admitted through the viewer's
//!   session `JobPort` (`overloaded`, `canceled`, `stale_revision` refuse before any work) and
//!   settled once, so its terminal Observe frame (codes and counters only) goes to the job sink.
//! - Not done here (declared gaps): placed graphics, vector paths and raster tiles of the layout
//!   (no asset port: the engine reports them as losses), tables (`table_not_exported` per table),
//!   design records, outlines, tagged PDF, CMYK conversion of RGB fills, script and language
//!   detection (every span shapes as Latin, English), saving layout edits with the package
//!   (SMB-LAYOUT-PERSIST-1). The PDF's line breaks can differ from the editor's preview, whose
//!   shaping is a fixed-advance placeholder.
//!
//! Hostile input (V4-01/02): page sizes and counts are bounded before allocation; text over
//! [`MAX_EXPORT_TEXT_BYTES`] in all stories, a paragraph over Type's input budget, an ICC file over
//! [`MAX_PROFILE_BYTES`] and a PDF source over the import bound refuse typed before they are
//! shaped or read; every PDF bound past that is press's.
#![deny(clippy::arithmetic_side_effects)]
use super::Viewer;
use super::export::write_atomic;
use super::job_admission::{refusal_json, settlement_json};
use super::shell::{Panel, blank_layout_doc};
use super::text::{self, DEFAULT_FONT, FontFile};
use crate::layout_ui::{LayoutDoc, LayoutEditor, LayoutRecords};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::{self as folio, Bounds, GeometryUnit, Length, PageSide, PageSpreadSchema, Parent, Payload, StudioPage, StudioPageSpread, StudioTextStory, StudioTypeStyleRecords, TypeStyleScope};
use hsk_studio_interop_layout::{
    self as il, ExportNames, FolioExportInput, LongDocExportInput, PdfAssets, PdfExportInput, PdfFont, PdfGlyph, PdfLongDoc, PdfStory, PlacedRaster,
    StoryInput, export_folio_longdoc, export_pdf_with, page_labels_from_sections, pdf_glyphs_from_type,
};
use hsk_studio_layout::{Composer, LayoutProposal, ReflowRequest, ShapedParagraph, SourceSpan, SpaceParams, StyleSource, TypesetOptions, number_pages, reflow_flow, resolve_style};
use hsk_studio_nib::OwnedPath;
use hsk_studio_press::{Colour, ExportOptions, OutputIntent, OutputIntentSubtype, PdfX4, check_pdfx4, render_page};
/// The PDF/X standard member of [`LayoutPdfSettings::pdfx`] (press's).
pub use hsk_studio_press::PdfXStandard;
use hsk_studio_session::{Action, JobOutcome};
use hsk_studio_type as ty;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

/// Most pages [`Viewer::layout_open`] creates.
pub const MAX_LAYOUT_PAGES: u32 = 1000;
/// Smallest and largest page side (pt) [`Viewer::layout_open`] accepts.
pub const PAGE_SIDE_PT: (f64, f64) = (1.0, 14_400.0);
/// Most story text (bytes, all stories) one PDF export shapes.
pub const MAX_EXPORT_TEXT_BYTES: usize = 1 << 20;
/// Largest output-intent ICC file read.
pub const MAX_PROFILE_BYTES: u64 = 4 << 20;
/// Most preflight findings a `preflight_errors` refusal lists.
const MAX_LISTED_FINDINGS: usize = 50;
/// Size of a span no style sizes (pt).
const DEFAULT_SIZE_PT: f64 = 12.0;
/// Paragraph terminators trimmed from a paragraph before it is shaped.
const TERMINATORS: &[char] = &['\n', '\r', '\u{000B}', '\u{000C}', '\u{0085}', '\u{2029}'];

/// A typed refusal: a stable code and a detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutExportRefusal {
    pub code: &'static str,
    pub detail: String,
}

impl LayoutExportRefusal {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self { code, detail: detail.into() }
    }
}

impl std::fmt::Display for LayoutExportRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.code, self.detail)
    }
}

type Result<T> = std::result::Result<T, LayoutExportRefusal>;

/// PDF export settings (STU-LAY-158 subset; module docs).
#[derive(Clone, Copy, Debug)]
pub struct LayoutPdfSettings<'a> {
    pub pdfx: PdfXStandard,
    /// PDF date (`D:20261107000000Z`) of the PDF/X-4 identification (creation = modification).
    pub created: Option<&'a str>,
    /// Output-intent destination profile (ICC bytes).
    pub output_profile: Option<&'a [u8]>,
    pub output_condition_identifier: Option<&'a str>,
    pub output_condition: Option<&'a str>,
    pub registry: Option<&'a str>,
    pub title: Option<&'a str>,
    pub subset_fonts: bool,
    /// Export even when preflight reports error-severity findings.
    pub ignore_preflight: bool,
}

impl Default for LayoutPdfSettings<'_> {
    fn default() -> Self {
        Self { pdfx: PdfXStandard::None, created: None, output_profile: None, output_condition_identifier: None, output_condition: None, registry: None, title: None, subset_fonts: true, ignore_preflight: false }
    }
}

fn pt(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}

/// A blank layout of `pages` single-sided pages of `w` x `h` pt, one spread each (module docs).
pub fn sized_layout_doc(w: f64, h: f64, pages: u32) -> Result<LayoutDoc> {
    let ok = |v: f64| v.is_finite() && (PAGE_SIDE_PT.0..=PAGE_SIDE_PT.1).contains(&v);
    if !(ok(w) && ok(h)) {
        return Err(LayoutExportRefusal::new("page_size_out_of_range", format!("{w} x {h} pt; each side {} to {} pt", PAGE_SIDE_PT.0, PAGE_SIDE_PT.1)));
    }
    if !(1..=MAX_LAYOUT_PAGES).contains(&pages) {
        return Err(LayoutExportRefusal::new("page_count_out_of_range", format!("{pages}; 1 to {MAX_LAYOUT_PAGES}")));
    }
    let mut doc = blank_layout_doc();
    let template_page = doc.document.page_spreads.first().and_then(|s| s.pages.first()).cloned().ok_or_else(|| LayoutExportRefusal::new("layout_internal", "blank layout has no page"))?;
    doc.document.page_spreads = (1..=pages)
        .map(|n| StudioPageSpread {
            schema_id: PageSpreadSchema::V1,
            page_spread_id: format!("SPGS-019abcde-0000-7000-8000-{:012x}", 0xb000_u64.saturating_add(u64::from(n))),
            name: format!("spread {n}"),
            parent: Parent::Root(()),
            bounds: Bounds { x: pt(0.0), y: pt(0.0), width: pt(w), height: pt(h) },
            pages: vec![StudioPage { page_id: format!("page-{n}"), width: pt(w), height: pt(h), side: PageSide::SingleSided, ..template_page.clone() }],
        })
        .collect();
    Ok(doc)
}

fn pages_json(doc: &LayoutDoc) -> Value {
    json!(doc
        .document
        .page_spreads
        .iter()
        .flat_map(|s| s.pages.iter().map(|p| json!({"page_id": p.page_id, "width_pt": p.width.value, "height_pt": p.height.value})))
        .collect::<Vec<_>>())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The engine's loss entries as JSON (kind, code, element, count).
fn loss_json(loss: &[il::LossEntry]) -> Vec<Value> {
    loss.iter().map(|l| json!({"kind": l.kind.wire(), "code": l.code, "element": l.element, "count": l.count})).collect()
}

/// Lowering resolves no Folio asset (module docs): the engine reports each one it needed.
struct NoAssets;
impl PdfAssets for NoAssets {
    fn path(&self, _: &str) -> Option<OwnedPath> {
        None
    }
    fn raster(&self, _: &str) -> Option<PlacedRaster<'_>> {
        None
    }
    fn profile(&self, _: &str) -> Option<&[u8]> {
        None
    }
}

/// One style span of a paragraph: bytes `start..end` of the paragraph's visible text, the index of
/// its font in the viewer's list, its size.
#[derive(Clone, Copy, Debug)]
struct Span {
    start: u64,
    end: u64,
    font: usize,
    size_pt: f64,
}

/// One paragraph before shaping: its story byte range, the visible range inside it (paragraph
/// terminators trimmed: Type shapes one paragraph without them) and spans of the visible range.
struct Planned {
    start: u64,
    end: u64,
    visible_start: u64,
    visible_end: u64,
    spans: Vec<Span>,
}

/// One composed story (owned parts of a [`PdfStory`]).
struct Composed {
    story: usize,
    placement: LayoutProposal,
    paragraphs: Vec<ShapedParagraph>,
    glyphs: Vec<Vec<PdfGlyph>>,
}

/// Family and size of a style, folded along `based_on`.
fn style_font(styles: &StudioTypeStyleRecords, id: Option<&str>, scope: TypeStyleScope) -> (Option<String>, Option<f64>) {
    match id.and_then(|s| resolve_style(styles, s, scope).ok()) {
        Some(r) => (r.font_family, r.font_size_pt.filter(|v| v.is_finite() && *v > 0.0)),
        None => (None, None),
    }
}

fn story_text(s: &StudioTextStory) -> &str {
    text::story_text(s).unwrap_or("")
}

/// What the PDF export reads from the editor (cloned: the export never holds the editor).
struct Live {
    doc: LayoutDoc,
    rec: LayoutRecords,
    preflight: std::result::Result<hsk_studio_layout::PreflightReport, &'static str>,
}

const PROBE_DOC: &str = "SDOC-019a5a20-0000-7000-8000-000000000001";
const PROBE_STORY: &str = "STXT-019a5a20-0000-7000-8000-000000000002";
const PROBE_LAYER: &str = "SLYR-019a5a20-0000-7000-8000-000000000003";
const PROBE_STYLE: &str = "STYS-019a5a20-0000-7000-8000-000000000004";

/// Shapes one paragraph's visible text with Type (one style per span) and gives layout's paragraph
/// (clusters moved to `offset`, the story position) and the PDF glyphs.
#[allow(clippy::too_many_arguments)]
fn shape_paragraph(
    fonts: &[FontFile],
    text: &str,
    spans: &[Span],
    offset: u64,
    line: (f64, f64),
    pdf_fonts: &[PdfFont<'_>],
    composition: &str,
    actor: &ActorContext,
    cancel: &CancellationToken,
) -> Result<(ShapedParagraph, Vec<PdfGlyph>)> {
    let internal = |what: &str| LayoutExportRefusal::new("layout_internal", what.to_string());
    let (document, story, layer, style_owner) = (
        DomainId::parse(PROBE_DOC).map_err(|_| internal("probe id"))?,
        DomainId::parse(PROBE_STORY).map_err(|_| internal("probe id"))?,
        DomainId::parse(PROBE_LAYER).map_err(|_| internal("probe id"))?,
        DomainId::parse(PROBE_STYLE).map_err(|_| internal("probe id"))?,
    );
    let ledger = text::Ledger(Mutex::new(text::LedgerState::default()));
    let resolver = text::Fonts { files: fonts, ledger: &ledger };
    let candidates: Vec<[ty::FontResource<'_>; 1]> = spans
        .iter()
        .map(|s| {
            let f = fonts.get(s.font).ok_or_else(|| internal("span font"))?;
            Ok([ty::FontResource { identity: &f.family, location: &f.family, content_hash: f.hash, face_index: 0, resource_revision: 1, grant_revision: 1 }])
        })
        .collect::<Result<_>>()?;
    let styles: Vec<ty::Style<'_>> = spans
        .iter()
        .zip(&candidates)
        .enumerate()
        .map(|(i, (s, c))| {
            let read_index = u32::try_from(i).ok().and_then(|v| v.checked_add(1)).ok_or_else(|| internal("span count"))?;
            Ok(ty::Style {
                read_index,
                range: ty::SourceRange { start: s.start, end: s.end },
                font_size_pt: s.size_pt,
                requested_identity: c[0].identity,
                candidates: c,
                substitution: None,
                script: *b"Latn",
                language: "en",
                character_override: ty::CharacterOverride::Default,
                features: &[],
                axes: &[],
            })
        })
        .collect::<Result<_>>()?;
    let names: Vec<String> = styles.iter().map(|s| format!("style{}", s.read_index)).collect();
    let mut reads = vec![ty::ReadAddress { owner: &story, property: "text", expected_revision: 1, fingerprint: hsk_studio_prism::profile_hash(text.as_bytes()) }];
    for name in &names {
        reads.push(ty::ReadAddress { owner: &style_owner, property: name, expected_revision: 1, fingerprint: hsk_studio_prism::profile_hash(b"resolved-style") });
    }
    let paragraphs = [ty::ParagraphInput { range: ty::SourceRange { start: 0, end: text.len() as u64 }, direction: ty::Direction::LeftToRight, provenance: ty::DirectionProvenance::Explicit }];
    let input_lease = ty::AdmissionPort::reserve(&ledger, 1 << 20, u64::MAX).map_err(|e| LayoutExportRefusal::new("text_admission", format!("{e:?}")))?;
    let input = text::Input(1 << 20);
    let scene = text::Scene { text, styles: &styles };
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
            address: ty::ReadAddress { owner: &story, property: "result", expected_revision: 1, fingerprint: hsk_studio_prism::profile_hash(b"harness-text-result") },
            preimage: b"harness-text-result",
        },
        composition_version: composition,
        normalization: ty::Normalization::Preserve,
        story_direction: ty::StoryDirection::LeftToRight,
        paragraphs: &paragraphs,
        styles: &styles,
        resolver_revision: 1,
        cancel_epoch: 1,
        cancellation: cancel,
        actor,
        correlation_id: "harness-layout-export",
        observation_correlation: 1,
        limits: text::limits(),
    };
    let out = {
        let prepared = ty::TextEngine.shaped_runs(request, &ports).map_err(|r| LayoutExportRefusal::new("text_shaping_refused", format!("{:?}", r.error)))?;
        let p = prepared.paragraphs().first().ok_or_else(|| internal("no shaped paragraph"))?;
        let mut shaped = ShapedParagraph::from_type(p, line.0, line.1);
        let glyphs = pdf_glyphs_from_type(p, pdf_fonts).map_err(|e| LayoutExportRefusal::new(e.code(), e.to_string()))?;
        for g in &mut shaped.glyphs {
            g.cluster = SourceSpan { start: g.cluster.start.saturating_add(offset), end: g.cluster.end.saturating_add(offset) };
        }
        for l in &mut shaped.ligature_parts {
            l.source = SourceSpan { start: l.source.start.saturating_add(offset), end: l.source.end.saturating_add(offset) };
        }
        (shaped, glyphs)
    };
    drop(input_lease);
    Ok(out)
}

/// The paragraphs of `story` with their spans (module docs: character style, else paragraph
/// style, else the bundled font); `used` collects the font indices, `host_loss` the substitutions.
fn plan_story(st: &StudioTextStory, styles: &StudioTypeStyleRecords, fonts: &[FontFile], fallback: usize, used: &mut BTreeSet<usize>, substituted: &mut BTreeMap<String, u32>) -> Result<Vec<Planned>> {
    let text = story_text(st);
    let total = text.len() as u64;
    let mut out = Vec::with_capacity(st.paragraphs.len());
    for p in &st.paragraphs {
        if p.start > p.end || p.end > total {
            return Err(LayoutExportRefusal::new("story_paragraph_range", st.story_id.clone()));
        }
        let body = text.get(usize::try_from(p.start).unwrap_or(usize::MAX)..usize::try_from(p.end).unwrap_or(usize::MAX)).ok_or_else(|| LayoutExportRefusal::new("story_paragraph_range", st.story_id.clone()))?;
        let lead = body.len().saturating_sub(body.trim_start_matches(TERMINATORS).len()) as u64;
        let trail = body.len().saturating_sub(body.trim_end_matches(TERMINATORS).len()) as u64;
        let visible_start = p.start.saturating_add(lead).min(p.end);
        let visible_end = p.end.saturating_sub(trail).max(visible_start);
        let (para_family, para_size) = style_font(styles, p.style_ref.as_deref(), TypeStyleScope::Paragraph);
        let mut cuts: BTreeSet<u64> = [visible_start, visible_end].into_iter().collect();
        for r in &st.character_ranges {
            for at in [r.start, r.end] {
                if at > visible_start && at < visible_end {
                    cuts.insert(at);
                }
            }
        }
        let cuts: Vec<u64> = cuts.into_iter().collect();
        let mut spans = Vec::new();
        for w in cuts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let range = st.character_ranges.iter().find(|r| r.start <= a && r.end >= b && r.style_ref.is_some());
            let (cf, cs) = style_font(styles, range.and_then(|r| r.style_ref.as_deref()), TypeStyleScope::Character);
            let family = cf.or_else(|| para_family.clone());
            let size_pt = cs.or(para_size).unwrap_or(DEFAULT_SIZE_PT);
            let font = match family.as_deref() {
                None => fallback,
                Some(name) => match fonts.iter().position(|f| f.family == name) {
                    Some(i) => i,
                    None => {
                        let n = substituted.entry(name.to_string()).or_insert(0);
                        *n = n.saturating_add(1);
                        fallback
                    }
                },
            };
            used.insert(font);
            spans.push(Span { start: a.saturating_sub(visible_start), end: b.saturating_sub(visible_start), font, size_pt });
        }
        out.push(Planned { start: p.start, end: p.end, visible_start, visible_end, spans });
    }
    Ok(out)
}

/// Non-empty byte ranges of `visible` between paragraph terminators.
fn pieces_of(visible: &str) -> Vec<(u64, u64)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    for (k, c) in visible.char_indices() {
        if TERMINATORS.contains(&c) {
            if k > at {
                out.push((at as u64, k as u64));
            }
            at = k.saturating_add(c.len_utf8());
        }
    }
    if visible.len() > at {
        out.push((at as u64, visible.len() as u64));
    }
    out
}

/// Joins a later shaped piece onto one paragraph (glyph-indexed members move by the glyphs before).
fn append_paragraph(into: &mut ShapedParagraph, more: ShapedParagraph) {
    let base = into.glyphs.len();
    into.line_end_advances.extend(more.line_end_advances.into_iter().map(|mut a| {
        a.glyph = a.glyph.saturating_add(base);
        a
    }));
    into.ligature_parts.extend(more.ligature_parts.into_iter().map(|mut l| {
        l.glyph = l.glyph.saturating_add(base);
        l
    }));
    into.glyphs.extend(more.glyphs);
    into.right_to_left |= more.right_to_left;
    into.latin_only &= more.latin_only;
}

/// The `/W` advance (1/1000 em) and glyph id of a font's hyphen, shaped once at 1000 pt.
fn hyphen_of(font: &FontFile, composition: &str, actor: &ActorContext, cancel: &CancellationToken) -> Option<(u16, u16)> {
    let g = text::shaped_glyphs(font, "-", 1000.0, composition, actor, cancel).ok()?;
    let g = g.first()?;
    let advance = g.x_advance_pt.round();
    (advance.is_finite() && (0.0..=f64::from(u16::MAX)).contains(&advance)).then_some(())?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((u16::try_from(g.glyph_id).ok()?, advance as u16))
}

/// The ICC data colour space's PDF component count (header bytes 16..20).
fn icc_components(icc: &[u8]) -> Result<u8> {
    if icc.len() < 128 || icc.get(36..40) != Some(b"acsp".as_slice()) {
        return Err(LayoutExportRefusal::new("output_profile_invalid", "not an ICC profile (no acsp signature)"));
    }
    match icc.get(16..20) {
        Some(b"CMYK") => Ok(4),
        Some(b"RGB ") => Ok(3),
        Some(b"GRAY") => Ok(1),
        _ => Err(LayoutExportRefusal::new("output_profile_colour_space", "the profile's data colour space is not gray, RGB or CMYK")),
    }
}

/// Reads at most `limit` bytes of `path`, typed.
pub fn read_bounded(path: &Path, limit: u64, code: &'static str) -> Result<Vec<u8>> {
    let len = std::fs::metadata(path).map_err(|e| LayoutExportRefusal::new(code, format!("{}: {e}", path.display())))?.len();
    if len > limit {
        return Err(LayoutExportRefusal::new("source_over_budget", format!("{}: {len} bytes; at most {limit}", path.display())));
    }
    std::fs::read(path).map_err(|e| LayoutExportRefusal::new(code, format!("{}: {e}", path.display())))
}

fn report_json(r: &hsk_studio_layout::PreflightReport, findings: usize) -> Value {
    json!({
        "profile": r.profile, "ok": r.ok(), "errors": r.errors, "warnings": r.warnings, "informational": r.informational,
        "findings": r.findings.iter().take(findings).map(|f| json!({"rule_id": f.rule_id, "severity": f.severity.code(), "object": f.object, "page": f.page})).collect::<Vec<_>>(),
    })
}

impl Viewer {
    /// Opens the Layout Editor; a page size or count starts a new blank layout (module docs).
    pub fn layout_open(&mut self, page_size_pt: Option<(f64, f64)>, pages: Option<u32>) -> Result<Value> {
        let sized = match (page_size_pt, pages) {
            (None, None) => None,
            (size, pages) => {
                let (w, h) = size.unwrap_or((595.0, 842.0));
                let doc = sized_layout_doc(w, h, pages.unwrap_or(1))?;
                if self.run.snapshot().is_some_and(|s| !s.document().page_spreads.is_empty()) {
                    return Err(LayoutExportRefusal::new("layout_document_has_pages", "the open document has pages: the layout edits them (resize a page with its own command)"));
                }
                if self.layout_editor().is_some_and(|e| e.history_len().0 > 0) {
                    return Err(LayoutExportRefusal::new("layout_unsaved_edits", "the layout editor holds edits; a new blank layout would drop them"));
                }
                Some(doc)
            }
        };
        if !self.panel_open(Panel::LayoutEditor) {
            self.toggle_panel(Panel::LayoutEditor).map_err(|m| LayoutExportRefusal::new("layout_unavailable", m))?;
        }
        // With a document open, the sized pages become its pages: one committed layout step (lane
        // SMB-LAYOUT-PERSIST-1), so they are saved and every later layout command keeps them.
        match sized {
            Some(doc) if self.run.snapshot().is_some() => {
                self.layout_sync();
                self.layout_commit("layout.open", |d| d.page_spreads = doc.document.page_spreads)
                    .map_err(|code| LayoutExportRefusal::new("layout_commit_refused", format!("the sized pages were not committed: {code}")))?;
                self.layout_sync();
            }
            Some(doc) => {
                if let Some(editor) = self.layout_editor() {
                    *editor = LayoutEditor::new(doc);
                }
            }
            None => {}
        }
        let editor = self.layout_editor().ok_or_else(|| LayoutExportRefusal::new("layout_unavailable", "the layout editor did not open"))?;
        let doc = editor.doc();
        Ok(json!({"document_id": doc.document.document_id, "pages": pages_json(doc), "stories": doc.stories.len(), "frames": doc.document.layers.iter().filter(|l| l.text_frame.is_some()).count()}))
    }

    /// The live preflight of the Layout Editor (module docs).
    pub fn layout_preflight(&mut self) -> Result<Value> {
        self.layout_sync();
        let editor = self.layout_editor().ok_or_else(|| LayoutExportRefusal::new("layout_not_open", "open the layout editor first (layout_open)"))?;
        let report = editor.preflight(None).map_err(|e| LayoutExportRefusal::new(e.code(), "preflight"))?;
        Ok(report_json(&report, usize::MAX))
    }

    fn live_layout(&mut self) -> Result<Live> {
        self.layout_sync();
        let editor = self.layout_editor().ok_or_else(|| LayoutExportRefusal::new("layout_not_open", "open the layout editor first (layout_open)"))?;
        Ok(Live { doc: editor.doc().clone(), rec: editor.records().clone(), preflight: editor.preflight(None).map_err(|e| e.code()) })
    }

    /// One admitted job around `work` (module docs): settled once with its outcome.
    fn as_export_job(&mut self, work: impl FnOnce(&mut Self) -> Result<Value>) -> Result<Value> {
        let admission = self.job_admission();
        let head = self.document_head();
        let correlation = admission.next_correlation();
        let job = admission
            .admit(admission.request(Action::Export, 0, head, correlation), head, self.cancel)
            .map_err(|r| LayoutExportRefusal::new(r.code(), refusal_json(r).to_string()))?;
        match work(self) {
            Ok(mut v) => {
                v["job"] = settlement_json(&job.settle(JobOutcome::Completed));
                Ok(v)
            }
            Err(e) => {
                let outcome = if e.code == "canceled" { JobOutcome::Canceled } else { JobOutcome::Failed(e.code) };
                job.settle(outcome);
                Err(e)
            }
        }
    }

    /// The PDF of the live layout and its receipt JSON, preflight first (module docs).
    pub fn layout_pdf_bytes(&mut self, settings: &LayoutPdfSettings<'_>) -> Result<(Vec<u8>, Value)> {
        let live = self.live_layout()?;
        let report = live.preflight.map_err(|code| LayoutExportRefusal::new(code, "preflight"))?;
        if report.errors > 0 && !settings.ignore_preflight {
            return Err(LayoutExportRefusal::new("preflight_errors", report_json(&report, MAX_LISTED_FINDINGS).to_string()));
        }
        let (doc, rec) = (&live.doc, &live.rec);
        let cancel = self.cancel;
        let actor = self.run.actor().clone();
        let composition = if self.run.snapshot().is_some() { self.composition_token().map_err(|m| LayoutExportRefusal::new("text_composition_token", m))? } else { ty::COMPOSITION_VERSION };
        let fonts: &[FontFile] = &self.fonts;
        let fallback = fonts.iter().position(|f| f.family == DEFAULT_FONT).or(if fonts.is_empty() { None } else { Some(0) }).ok_or_else(|| LayoutExportRefusal::new("layout_font_unavailable", "the viewer has no font"))?;
        let framed: Vec<usize> = doc
            .stories
            .iter()
            .enumerate()
            .filter(|(_, s)| doc.document.layers.iter().any(|l| l.text_frame.is_some() && matches!(&l.payload, Payload::Text { story_id } if *story_id == s.story_id)))
            .map(|(i, _)| i)
            .collect();
        let text_bytes = framed.iter().filter_map(|i| doc.stories.get(*i)).fold(0usize, |n, s| n.saturating_add(story_text(s).len()));
        if text_bytes > MAX_EXPORT_TEXT_BYTES {
            return Err(LayoutExportRefusal::new("layout_text_over_budget", format!("{text_bytes} bytes of story text; at most {MAX_EXPORT_TEXT_BYTES}")));
        }
        let mut used = BTreeSet::new();
        let mut substituted = BTreeMap::new();
        let mut plans = Vec::with_capacity(framed.len());
        for i in &framed {
            let st = doc.stories.get(*i).ok_or_else(|| LayoutExportRefusal::new("layout_internal", "story index"))?;
            plans.push((*i, plan_story(st, &doc.styles, fonts, fallback, &mut used, &mut substituted)?));
        }
        let used: Vec<usize> = used.into_iter().collect();
        let pdf_fonts: Vec<PdfFont<'_>> = used
            .iter()
            .filter_map(|i| fonts.get(*i))
            .map(|f| PdfFont { bytes: &f.bytes, face_index: 0, hyphen: hyphen_of(f, composition, &actor, cancel) })
            .collect();
        let mut composed = Vec::with_capacity(plans.len());
        let mut inner_breaks = 0usize;
        for (i, planned) in &plans {
            cancel.check().map_err(|_| LayoutExportRefusal::new("canceled", "layout export"))?;
            let st = doc.stories.get(*i).ok_or_else(|| LayoutExportRefusal::new("layout_internal", "story index"))?;
            let text = story_text(st);
            let mut paragraphs = Vec::with_capacity(planned.len());
            let mut glyphs = Vec::with_capacity(planned.len());
            for p in planned {
                let size = if p.spans.is_empty() { DEFAULT_SIZE_PT } else { p.spans.iter().map(|s| s.size_pt).fold(0.0, f64::max) };
                let line = (size * 1.25, size * 0.75);
                let visible = text.get(usize::try_from(p.visible_start).unwrap_or(usize::MAX)..usize::try_from(p.visible_end).unwrap_or(usize::MAX)).unwrap_or("");
                let mut shaped = ShapedParagraph::from_fixed_advance(text, SourceSpan { start: p.start, end: p.start }, 0.0, line.0, line.1).ok_or_else(|| LayoutExportRefusal::new("story_paragraph_range", st.story_id.clone()))?;
                let mut g = Vec::new();
                // A terminator inside one Folio paragraph (the editor's insert into an empty story
                // keeps one paragraph): Type shapes each piece on its own, the pieces join into the
                // one paragraph layout composes, whose line breaking (UAX #14) ends the line at the
                // terminator. The paragraph boundary is what is lost, and it is reported.
                let pieces = pieces_of(visible);
                inner_breaks = inner_breaks.saturating_add(pieces.len().saturating_sub(1));
                for (k, (a, b)) in pieces.into_iter().enumerate() {
                    let spans: Vec<Span> = p.spans.iter().filter(|s| s.end > a && s.start < b).map(|s| Span { start: s.start.max(a).saturating_sub(a), end: s.end.min(b).saturating_sub(a), ..*s }).collect();
                    let piece = visible.get(usize::try_from(a).unwrap_or(usize::MAX)..usize::try_from(b).unwrap_or(usize::MAX)).unwrap_or("");
                    let (part, part_glyphs) = shape_paragraph(fonts, piece, &spans, p.visible_start.saturating_add(a), line, &pdf_fonts, composition, &actor, cancel)?;
                    if k == 0 {
                        shaped = part;
                    } else {
                        append_paragraph(&mut shaped, part);
                    }
                    g.extend(part_glyphs);
                }
                shaped.source = SourceSpan { start: p.start, end: p.end };
                paragraphs.push(shaped);
                glyphs.push(g);
            }
            let request = ReflowRequest {
                document: &doc.document,
                story: st,
                resolved_text: None,
                paragraphs: &paragraphs,
                obstacles: &[],
                composer: Composer::FirstFit,
                space: SpaceParams::default(),
                expected_revision: doc.document.revision,
                limits: hsk_studio_layout::Limits::default(),
            };
            let options = TypesetOptions { styles: Some(StyleSource { records: &doc.styles, default_font_size_pt: DEFAULT_SIZE_PT }), ..TypesetOptions::default() };
            let placement = reflow_flow(&request, &options, &doc.flow, cancel).map_err(|e| LayoutExportRefusal::new(e.code(), st.story_id.clone()))?;
            composed.push(Composed { story: *i, placement, paragraphs, glyphs });
        }
        // Unstyled text is black in the output intent's colour space (a CMYK or gray intent
        // takes no device RGB); style fills stay the engine's (RGB ones are its losses).
        let intent_components = settings.output_profile.map(icc_components).transpose()?;
        let default_fill = match intent_components {
            Some(4) => Colour::DeviceCmyk([0.0, 0.0, 0.0, 1.0]),
            Some(1) => Colour::DeviceGray(0.0),
            _ => Colour::DeviceRgb([0.0, 0.0, 0.0]),
        };
        let stories: Vec<PdfStory<'_>> = composed
            .iter()
            .filter_map(|c| {
                let st = doc.stories.get(c.story)?;
                Some(PdfStory { story: st, text: story_text(st), placement: &c.placement, paragraphs: &c.paragraphs, glyphs: &c.glyphs, default_fill })
            })
            .collect();
        let snapshot = match self.run.snapshot().filter(|s| *s.document() == doc.document) {
            Some(s) => s.clone(),
            None => folio::validate_document(doc.document.clone(), folio::Budget::default(), cancel, &folio::Unresolved)
                .map_err(|d| LayoutExportRefusal::new("layout_document_invalid", format!("{:?} {}", d.code, d.target)))?,
        };
        let numbers = number_pages(&doc.document, &rec.longdoc.sections).map_err(|e| LayoutExportRefusal::new(e.code(), "page numbering"))?;
        let labels = page_labels_from_sections(&rec.longdoc.sections, &numbers);
        let intent = match (settings.output_profile, intent_components) {
            (Some(icc), Some(components)) => Some(OutputIntent {
                icc,
                components,
                output_condition_identifier: settings.output_condition_identifier.unwrap_or("Custom"),
                output_condition: settings.output_condition,
                registry: settings.registry,
                subtype: OutputIntentSubtype::GtsPdfx,
            }),
            _ => None,
        };
        let identification = match (settings.pdfx, settings.created) {
            (PdfXStandard::X4_2010, Some(created)) => Some(PdfX4 { created, modified: created, trapped: false }),
            (PdfXStandard::X4_2010, None) => return Err(LayoutExportRefusal::new("pdfx_created_required", "pdfx.x4_2010 needs `created`, a PDF date such as D:20261107000000Z")),
            _ => None,
        };
        let options = ExportOptions { output_intent: intent, info_title: settings.title, subset_fonts: settings.subset_fonts, ..ExportOptions::default() }
            .with_pdfx_standard(settings.pdfx, identification)
            .map_err(|e| LayoutExportRefusal::new(e.code(), "pdfx"))?;
        let longdoc = PdfLongDoc { page_labels: &labels, ..PdfLongDoc::default() };
        let input = PdfExportInput {
            snapshot: &snapshot,
            design: None,
            type_styles: Some(&doc.styles),
            stories: &stories,
            fonts: &pdf_fonts,
            assets: &NoAssets,
            options,
            limits: hsk_studio_press::Limits::default(),
        };
        let out = export_pdf_with(&input, &longdoc, cancel).map_err(|e| LayoutExportRefusal::new(e.code(), e.to_string()))?;
        let pdfx = match settings.pdfx {
            PdfXStandard::X4_2010 => {
                let r = check_pdfx4(&out.bytes, &hsk_studio_press::Limits::default(), cancel).map_err(|e| LayoutExportRefusal::new(e.code(), "pdfx self-check"))?;
                if !r.all_passed() {
                    return Err(LayoutExportRefusal::new("pdfx_self_check_failed", json!({"failed": r.failed()}).to_string()));
                }
                json!({"standard": settings.pdfx.code(), "self_check": {"conformance": r.conformance, "all_passed": r.all_passed(), "failed": r.failed(), "rules": r.rules.iter().map(|x| json!({"id": x.id, "passed": x.passed})).collect::<Vec<_>>()}})
            }
            other => json!({"standard": other.code()}),
        };
        let mut host_loss: Vec<Value> = substituted.iter().map(|(family, n)| json!({"code": "font_substituted", "element": family, "count": n, "drawn_with": fonts.get(fallback).map(|f| f.family.as_str())})).collect();
        if inner_breaks > 0 {
            host_loss.push(json!({"code": "paragraph_break_inside_paragraph_set_as_line_break", "element": "stories", "count": inner_breaks}));
        }
        let tables = rec.longdoc.tables.len();
        if tables > 0 {
            host_loss.push(json!({"code": "table_not_exported", "element": "tables", "count": tables}));
        }
        let receipt = &out.receipt;
        let lines: usize = composed.iter().flat_map(|c| c.placement.frames.iter()).map(|f| f.lines.len()).sum();
        let overset: Vec<&str> = composed.iter().filter(|c| c.placement.overset.is_some()).filter_map(|c| doc.stories.get(c.story)).map(|s| s.story_id.as_str()).collect();
        let summary = json!({
            "bytes": out.bytes.len(), "sha256": hex(&hsk_studio_prism::profile_hash(&out.bytes)), "pages": out.pages, "page_sizes": pages_json(doc),
            "default_fill": match default_fill { Colour::DeviceCmyk(_) => "device_cmyk", Colour::DeviceGray(_) => "device_gray", _ => "device_rgb" },
            "glyphs_drawn": out.glyphs_drawn, "paths_drawn": out.paths_drawn, "tiles_drawn": out.tiles_drawn, "lines": lines, "overset_stories": overset,
            "fonts": used.iter().filter_map(|i| fonts.get(*i)).map(|f| f.family.as_str()).collect::<Vec<_>>(), "page_labels": labels.len(),
            "loss": loss_json(&out.loss), "host_loss": host_loss, "preflight": report_json(&report, MAX_LISTED_FINDINGS), "pdfx": pdfx,
            "receipt": {"version": receipt.version, "pages": receipt.pages, "fonts_embedded": receipt.fonts_embedded, "conformance": receipt.conformance,
                        "effective_output_condition": receipt.effective_output_condition, "effective_destination_profile_sha256": receipt.effective_destination_profile_sha256,
                        "effective_pdfx_profile": receipt.effective_pdfx_profile},
        });
        Ok((out.bytes, summary))
    }

    /// Writes [`Viewer::layout_pdf_bytes`] to `path` (module docs).
    pub fn export_layout_pdf(&mut self, path: &Path, overwrite: bool, settings: &LayoutPdfSettings<'_>) -> Result<Value> {
        self.as_export_job(|v| {
            refuse_existing(path, overwrite)?;
            let (bytes, mut summary) = v.layout_pdf_bytes(settings)?;
            write_atomic(path, &bytes).map_err(|code| LayoutExportRefusal::new(code, path.display().to_string()))?;
            summary["path"] = json!(path.to_string_lossy());
            Ok(summary)
        })
    }

    /// Writes the live layout as an IDML package to `path` (module docs).
    pub fn export_layout_idml(&mut self, path: &Path, overwrite: bool) -> Result<Value> {
        self.as_export_job(|v| {
            refuse_existing(path, overwrite)?;
            let live = v.live_layout()?;
            let (doc, rec) = (&live.doc, &live.rec);
            let names_of = |scope: TypeStyleScope, prefix: &str| -> BTreeMap<String, String> {
                doc.styles.styles.iter().filter(|s| s.scope == scope).map(|s| (s.style_id.clone(), format!("{prefix}/{}", s.name))).collect()
            };
            let names = ExportNames { paragraph_styles: names_of(TypeStyleScope::Paragraph, "ParagraphStyle"), character_styles: names_of(TypeStyleScope::Character, "CharacterStyle"), languages: BTreeMap::new() };
            // Table cell, footnote, endnote and sidenote stories are the long-document records'
            // (export_folio_longdoc looks them up there); every other story is a host story.
            let notes: BTreeSet<&str> = rec
                .longdoc
                .tables
                .iter()
                .filter_map(|t| t.cell_story_id.as_deref())
                .chain(rec.longdoc.footnotes.iter().map(|f| f.note_story_id.as_str()))
                .chain(rec.longdoc.endnotes.iter().chain(&rec.longdoc.sidenotes).map(|e| e.note_story_id.as_str()))
                .collect();
            let (note_stories, stories): (Vec<StoryInput<'_>>, Vec<StoryInput<'_>>) =
                doc.stories.iter().map(|s| StoryInput { story: s, resolved_text: None }).partition(|s| notes.contains(s.story.story_id.as_str()));
            let input = FolioExportInput { document: &doc.document, stories: &stories, names: &names };
            let out = export_folio_longdoc(&input, &LongDocExportInput { records: &rec.longdoc, stories: &note_stories }, &il::Limits::default(), v.cancel)
                .map_err(|e| LayoutExportRefusal::new(e.code(), "idml export"))?;
            write_atomic(path, &out.bytes).map_err(|code| LayoutExportRefusal::new(code, path.display().to_string()))?;
            Ok(json!({"path": path.to_string_lossy(), "bytes": out.bytes.len(), "sha256": hex(&hsk_studio_prism::profile_hash(&out.bytes)), "stories": stories.len(), "note_stories": note_stories.len(), "pages": pages_json(doc), "loss": loss_json(&out.loss)}))
        })
    }

    /// Renders page `page` of the PDF at `source` to an RGB PNG at `out` (module docs).
    pub fn render_pdf_page(&mut self, source: &Path, page: usize, px_per_pt: f64, out: &Path, overwrite: bool) -> Result<Value> {
        self.as_export_job(|v| {
            refuse_existing(out, overwrite)?;
            let bytes = read_bounded(source, crate::import::MAX_SOURCE_BYTES, "source_unreadable")?;
            let page_image = render_page(&bytes, page, px_per_pt, &hsk_studio_press::Limits::default(), v.cancel).map_err(|e| LayoutExportRefusal::new(e.code(), "render_page"))?;
            let samples = hsk_studio_interop_raster::Samples::U8(page_image.rgb.clone());
            let request = hsk_studio_interop_raster::PngEncodeRequest {
                width: page_image.width,
                height: page_image.height,
                colour: hsk_studio_interop_raster::Colour::Rgb,
                samples: &samples,
                icc: None,
                compression: hsk_studio_interop_raster::PngCompression::Balanced,
            };
            let png = hsk_studio_interop_raster::encode_png(&request).map_err(|e| LayoutExportRefusal::new(e.code(), "png"))?;
            write_atomic(out, &png).map_err(|code| LayoutExportRefusal::new(code, out.display().to_string()))?;
            let ink = page_image.rgb.chunks_exact(3).filter(|p| *p != [255, 255, 255]).count();
            Ok(json!({"path": out.to_string_lossy(), "page": page, "width": page_image.width, "height": page_image.height, "non_white_pixels": ink,
                      "notes": page_image.notes, "font_warnings": page_image.font_warnings, "image_warnings": page_image.image_warnings}))
        })
    }
}

/// An existing destination needs `overwrite`.
fn refuse_existing(path: &Path, overwrite: bool) -> Result<()> {
    if path.file_name().is_none() {
        return Err(LayoutExportRefusal::new("export_path", path.display().to_string()));
    }
    if path.exists() && !overwrite {
        return Err(LayoutExportRefusal::new("overwrite_required", path.display().to_string()));
    }
    Ok(())
}
