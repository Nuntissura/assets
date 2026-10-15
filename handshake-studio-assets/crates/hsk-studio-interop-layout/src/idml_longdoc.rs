//! IDML <-> Studio long-document records (lane SMB-INTEROP-IDML): tables, footnotes, endnotes,
//! sections (page labels), TOC styles, placed graphics with fitting and text variables, mapped into
//! and out of Folio `StudioLongDocRecords` (hsk-studio-folio `longdoc.rs`).
//!
//! Sources of the IDML names: the Adobe IDML File Format Specification (CS6, 2012, the last one
//! Adobe published), cross-checked against DesignCraft's IDML importer (`crates/idml/src/import.rs`,
//! MIT OR Apache-2.0; element and attribute names only, no code copied) and Okapi issue 856
//! (endnote stories: `Story@IsEndnoteStory`, `EndnoteRange@SourceEndnote`). Confirmed there:
//! `Table` (`HeaderRowCount`, `FooterRowCount`, `BodyRowCount`, `ColumnCount`), `Row`
//! (`AutoGrow`, `MinimumHeight`, `MaximumHeight`, `SingleRowHeight`), `Column@SingleColumnWidth`,
//! `Cell` (`Name` = "column:row", spans, insets, `FillColor`, `FillTint`, `VerticalJustification`,
//! `*EdgeStroke{Weight,Color,Tint,Priority}`), `Section` (`PageStart`, `ContinueNumbering`,
//! `PageNumberStart`, `IncludeSectionPrefix`, `SectionPrefix`, `Marker`, `PageNumberStyle`),
//! `Footnote`, `FrameFittingOption` (`*Crop`, `FittingOnEmptyFrame`, `FittingAlignment`, `AutoFit`),
//! `Image` with `GraphicBounds`. UNVERIFIED (D5 InDesign oracle cases ID-05 / ID-06): the body
//! endnote reference `Endnote@Self`; `TOCStyle` / `TOCStyleEntry` attributes; `TextVariable` /
//! `*VariablePreference` attributes; `FootnoteOption` / `EndnoteOption` / `ChapterNumberPreference`
//! attributes; `Table@BreakHeaders` / `BreakFooters` / `SkipFirstHeader` / `SkipLastFooter` /
//! `StrokeOrder` and `Row@StartRow` / `KeepWithNextRow` values; whether InDesign writes the cells a
//! span covers (both are read; covered slots are skipped); `GraphicBounds` as points at 100 %.
//!
//! Import ([`import_longdoc`]) reads an [`IdmlImport`]. The decoder keeps character-range objects in
//! place (`RunModel::inline`), so tables, footnotes and endnote references get the host story byte
//! of the proposal text rule (run text, plus one newline per `Br`). Cell text becomes one story per
//! table (each cell's paragraphs joined by newlines, cells back to back), note text one story per
//! note (the leading number-marker processing instruction and one separator tab are dropped).
//! `designmap.xml` gives `Section` and `TextVariable`, `Resources/Styles.xml` gives `TOCStyle`,
//! `Resources/Preferences.xml` gives `FootnoteOption`, `EndnoteOption` and the chapter number,
//! `Resources/Graphic.xml` the RGB / CMYK swatches. Spread `Rectangle` / `Oval` / `Polygon` items
//! holding an `Image` become graphic frames on new raster layers without tiles (the linked pixels
//! are not imported: typed loss). The applied fit is recovered from geometry: every fit mode is
//! recomputed with hsk-studio-layout `fit_content` and the first whose transform matches (1e-3 pt)
//! is taken (order: cleared, content-to-frame, proportionally, fill-proportionally, centre,
//! preserve sizes), so geometry-equivalent fits normalise to the earlier one; no match is
//! `graphic_placement_not_a_fit_mode`. Everything that does not map is a typed [`LossEntry`].
//!
//! Export ([`export_folio_longdoc`]) is `export_folio` plus the same features: inline `Table` /
//! `Footnote` / `Endnote` at their host bytes (runs are split there), one `IsEndnoteStory` story,
//! `Section` / `TextVariable` and the `Styles` / `Graphic` part references in the designmap,
//! `TOCStyle`s, RGB swatches, footnote / endnote / chapter-number preferences, and graphic frames
//! as `Rectangle` / `Oval` with `FrameFittingOption` and an `Image` whose transform is the fit.
//! Import -> export -> import is stable on every modelled field (tests/idml_longdoc.rs).
//!
//! Hostile input: table rows, columns and cells are counted against Folio's table bounds before
//! any grid exists; the occupancy grid (at most `MAX_TABLE_CELLS` slots) refuses a span past the
//! grid edge or an overlap at the first offending slot, and Folio's `check_table_grid` has the last
//! word (typed loss, the table is dropped, never clamped); tables in cells and notes in notes or
//! cells are losses, never recursion; every fragment is parsed with the package XML limits; the
//! result is re-validated by Folio (`validate_document`, `validate_text_story`,
//! `validate_longdoc_records`).
use crate::folio_export::{ExportNames, FolioExport, FolioExportInput, StoryInput, finish, folio_model, opaque};
use crate::idml_decode::{IDPKG_NS, LossBook, canceled, parse_xml, text_of};
use crate::idml_encode::{esc, num};
use crate::model::*;
use crate::proposal::{Known, folio_err};
use crate::{IdMint, IdmlError, Result};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    ChangeCase, Code, EndnoteRestart, EndnoteScope, FootnoteRestart, GeometryUnit, LayerSchema, LayoutEdges, Length, MAX_TABLE_CELLS,
    MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, MAX_TABLES, NodeKind, NodeRef, Parent, Payload, ProfileBinding, ResolutionIssue, RunningUse, Snapshot,
    StoryContent, StoryDirection, StoryParagraph, StoryRange, StoryTextKind, StudioByteRange, StudioCellEdges, StudioCellStroke, StudioColor,
    StudioEdgePriorities, StudioEndnote, StudioEndnoteOptions, StudioFit, StudioFitAnchor, StudioFootnote, StudioFootnoteOptions,
    StudioFootnoteRule, StudioFrameShape, StudioGraphicFrame, StudioLayer, StudioLongDocRecords, StudioNumberStyle, StudioSection, StudioTable,
    StudioTableCell, StudioTableRow, StudioTextStory, StudioTextVariable, StudioTocEntry, StudioTocStyle, StudioVariableKind, TableHeaderBreak,
    TableRowType, TableStartRow, TableStrokeOrder, TextStorySchema, TocPageNumberPosition, VariableScope, VerticalJustification,
    check_footnote_options, check_running, check_sections, check_table_grid, check_toc_style, validate_document, validate_longdoc_records,
    validate_text_story,
};
use hsk_studio_layout::{Crop, Rect, fit_content};
use roxmltree::Node;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

const DESIGNMAP: &str = "designmap.xml";
const STYLES_PART: &str = "Resources/Styles.xml";
const GRAPHIC_PART: &str = "Resources/Graphic.xml";
const PREFERENCES_PART: &str = "Resources/Preferences.xml";
const NO_PARAGRAPH_STYLE: &str = "ParagraphStyle/$ID/[No paragraph style]";
const NO_CHARACTER_STYLE: &str = "CharacterStyle/$ID/[No character style]";
const FIT_TOLERANCE: f64 = 1e-3;

fn pt(v: f64) -> Length {
    Length { value: v, unit: GeometryUnit::Pt }
}
fn label_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control)
}
fn el_attr<'a>(n: Node<'a, '_>, k: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == k && a.namespace().is_none()).map(|a| a.value())
}
fn el_num(n: Node, k: &str) -> Option<f64> {
    el_attr(n, k).and_then(|v| v.trim().parse::<f64>().ok()).filter(|v| v.is_finite() && v.abs() <= 1e9)
}
fn el_u32(n: Node, k: &str) -> Option<u32> {
    el_attr(n, k).and_then(|v| v.trim().parse::<u32>().ok())
}
fn el_bool(n: Node, k: &str) -> Option<bool> {
    el_attr(n, k).map(|v| v == "true")
}
fn el_text(n: Node, k: &str) -> String {
    el_attr(n, k).unwrap_or_default().to_owned()
}
fn kids<'a, 'i>(n: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(|c| c.is_element())
}
fn kid<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    kids(n).find(|c| c.tag_name().name() == name)
}
fn tag<'a>(n: Node<'a, '_>) -> &'a str {
    n.tag_name().name()
}
/// A property written as an attribute or as a `Properties/<name>` child's text.
fn prop(n: Node, k: &str) -> Option<String> {
    el_attr(n, k).map(str::to_owned).or_else(|| kid(n, "Properties").and_then(|p| kid(p, k)).and_then(|c| c.text()).map(|t| t.trim().to_owned()))
}
fn affine_of(n: Node) -> Option<Affine> {
    let Some(s) = el_attr(n, "ItemTransform") else { return Some(IDENTITY) };
    let v: Vec<f64> = s.split_whitespace().map(|x| x.parse::<f64>().ok().filter(|v| v.is_finite() && v.abs() <= 1e9)).collect::<Option<_>>()?;
    <[f64; 6]>::try_from(v).ok()
}
/// `a` after `b`.
fn compose(a: &Affine, b: &Affine) -> Affine {
    [
        a[0] * b[0] + a[2] * b[1],
        a[1] * b[0] + a[3] * b[1],
        a[0] * b[2] + a[2] * b[3],
        a[1] * b[2] + a[3] * b[3],
        a[0] * b[4] + a[2] * b[5] + a[4],
        a[1] * b[4] + a[3] * b[5] + a[5],
    ]
}
fn apply(t: &Affine, x: f64, y: f64) -> (f64, f64) {
    (t[0] * x + t[2] * y + t[4], t[1] * x + t[3] * y + t[5])
}
fn bbox(points: impl Iterator<Item = (f64, f64)>, t: &Affine) -> [f64; 4] {
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for (x, y) in points {
        let (x, y) = apply(t, x, y);
        b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
    }
    b
}

// ---------------------------------------------------------------------------------------------
// Enumerations: IDML value <-> Studio member.

const NUMBER_STYLES: [(&str, StudioNumberStyle); 20] = [
    ("Arabic", StudioNumberStyle::Arabic),
    ("UpperRoman", StudioNumberStyle::UpperRoman),
    ("LowerRoman", StudioNumberStyle::LowerRoman),
    ("UpperLetters", StudioNumberStyle::UpperLetters),
    ("LowerLetters", StudioNumberStyle::LowerLetters),
    ("Kanji", StudioNumberStyle::Kanji),
    ("FullWidthArabic", StudioNumberStyle::FullWidthArabic),
    ("ArabicAlifBaTah", StudioNumberStyle::ArabicAlifBaTah),
    ("ArabicAbjad", StudioNumberStyle::ArabicAbjad),
    ("HebrewBiblical", StudioNumberStyle::HebrewBiblical),
    ("HebrewNonStandard", StudioNumberStyle::HebrewNonStandard),
    ("SingleLeadingZeros", StudioNumberStyle::SingleLeadingZeros),
    ("DoubleLeadingZeros", StudioNumberStyle::DoubleLeadingZeros),
    ("TripleLeadingZeros", StudioNumberStyle::TripleLeadingZeros),
    ("KatakanaModern", StudioNumberStyle::KatakanaModern),
    ("KatakanaTraditional", StudioNumberStyle::KatakanaTraditional),
    ("FormatNone", StudioNumberStyle::FormatNone),
    ("Symbols", StudioNumberStyle::Symbols),
    ("Asterisks", StudioNumberStyle::Asterisks),
    ("Current", StudioNumberStyle::Current),
];
const ANCHORS: [(&str, StudioFitAnchor); 9] = [
    ("TopLeftAnchor", StudioFitAnchor::TopLeft),
    ("TopCenterAnchor", StudioFitAnchor::TopCenter),
    ("TopRightAnchor", StudioFitAnchor::TopRight),
    ("LeftCenterAnchor", StudioFitAnchor::CenterLeft),
    ("CenterAnchor", StudioFitAnchor::Center),
    ("RightCenterAnchor", StudioFitAnchor::CenterRight),
    ("BottomLeftAnchor", StudioFitAnchor::BottomLeft),
    ("BottomCenterAnchor", StudioFitAnchor::BottomCenter),
    ("BottomRightAnchor", StudioFitAnchor::BottomRight),
];
const EMPTY_FITS: [(&str, StudioFit); 5] = [
    ("ContentToFrame", StudioFit::ContentToFrame),
    ("CenterContent", StudioFit::CenterContent),
    ("Proportionally", StudioFit::Proportionally),
    ("FillProportionally", StudioFit::FillProportionally),
    ("ContentAwareFit", StudioFit::ContentAware),
];
const FIT_ORDER: [Option<StudioFit>; 6] = [
    None,
    Some(StudioFit::ContentToFrame),
    Some(StudioFit::Proportionally),
    Some(StudioFit::FillProportionally),
    Some(StudioFit::CenterContent),
    Some(StudioFit::PreserveSizes),
];
const BREAKS: [(&str, TableHeaderBreak); 3] = [
    ("InAllTextColumns", TableHeaderBreak::InAllTextColumns),
    ("OncePerTextFrame", TableHeaderBreak::OncePerTextFrame),
    ("OncePerPage", TableHeaderBreak::OncePerPage),
];
const STROKE_ORDERS: [(&str, TableStrokeOrder); 4] = [
    ("RowOnTop", TableStrokeOrder::RowOnTop),
    ("ColumnOnTop", TableStrokeOrder::ColumnOnTop),
    ("BestJoins", TableStrokeOrder::BestJoins),
    ("InDesign2Compatibility", TableStrokeOrder::LegacyCompatibility),
];
const START_ROWS: [(&str, TableStartRow); 5] = [
    ("StartAnywhere", TableStartRow::Anywhere),
    ("StartNextFrame", TableStartRow::NextFrame),
    ("StartNextPage", TableStartRow::NextPage),
    ("StartNextOddPage", TableStartRow::NextOddPage),
    ("StartNextEvenPage", TableStartRow::NextEvenPage),
];
const VJS: [(&str, VerticalJustification); 4] = [
    ("TopAlign", VerticalJustification::Top),
    ("CenterAlign", VerticalJustification::Center),
    ("BottomAlign", VerticalJustification::Bottom),
    ("JustifyAlign", VerticalJustification::Justify),
];
const RESTARTS: [(&str, FootnoteRestart); 4] = [
    ("DontRestart", FootnoteRestart::DontRestart),
    ("PageRestart", FootnoteRestart::Page),
    ("SpreadRestart", FootnoteRestart::Spread),
    ("SectionRestart", FootnoteRestart::Section),
];
const ENDNOTE_RESTARTS: [(&str, EndnoteRestart); 2] =
    [("EndnoteContinuous", EndnoteRestart::Continuous), ("EndnoteRestartPerStory", EndnoteRestart::StoryRestart)];
const ENDNOTE_SCOPES: [(&str, EndnoteScope); 2] = [("EndnoteScopeDocument", EndnoteScope::Document), ("EndnoteScopeStory", EndnoteScope::Story)];
const CASES: [(&str, ChangeCase); 5] = [
    ("None", ChangeCase::None),
    ("Uppercase", ChangeCase::Uppercase),
    ("Lowercase", ChangeCase::Lowercase),
    ("Titlecase", ChangeCase::Titlecase),
    ("Sentencecase", ChangeCase::Sentencecase),
];
const POSITIONS: [(&str, TocPageNumberPosition); 3] = [
    ("AfterEntry", TocPageNumberPosition::After),
    ("BeforeEntry", TocPageNumberPosition::Before),
    ("NoPageNumber", TocPageNumberPosition::None),
];

fn from<T: Copy>(table: &[(&'static str, T)], v: &str) -> Option<T> {
    table.iter().find(|(k, _)| *k == v).map(|(_, t)| *t)
}
fn to<T: Copy + PartialEq>(table: &[(&'static str, T)], v: T) -> &'static str {
    table.iter().find(|(_, t)| *t == v).map_or("", |(k, _)| k)
}
/// IDML special-character escapes in separators: `^t` tab, `^>` en space, `^m` em space.
fn special_in(s: &str) -> String {
    s.replace("^t", "\t").replace("^>", "\u{2002}").replace("^m", "\u{2003}")
}
fn special_out(s: &str) -> String {
    s.replace('\t', "^t").replace('\u{2002}', "^>").replace('\u{2003}', "^m")
}

// ---------------------------------------------------------------------------------------------
// Import.

/// The long-document records of an IDML import.
#[derive(Clone, Debug)]
pub struct LongDocImport {
    /// The proposal document plus one raster layer (no tiles) per imported graphic frame.
    pub snapshot: Snapshot,
    pub records: StudioLongDocRecords,
    /// Cell, footnote and endnote stories the records name (in no frame).
    pub stories: Vec<StudioTextStory>,
    /// Proposal stories that only carried endnote ranges (`IsEndnoteStory`): they are the endnote
    /// records now and are not document stories.
    pub container_stories: Vec<String>,
    /// Styles first referenced here (cell and note paragraphs, TOC entries, variables, options).
    pub style_refs: Vec<StyleRef>,
    /// Graphic layers (`kind` "graphic") and the stories minted here.
    pub ids: Vec<IdLink>,
    pub unresolved: Vec<ResolutionIssue>,
    pub loss: Vec<LossEntry>,
}
impl LongDocImport {
    /// Adds the style names of `style_refs` to `names` (start from `IdmlImport::export_names`).
    pub fn extend_names(&self, names: &mut ExportNames) {
        for s in &self.style_refs {
            let map = if s.paragraph { &mut names.paragraph_styles } else { &mut names.character_styles };
            map.insert(s.folio.clone(), s.idml_name.clone());
        }
    }
}

/// Paragraphs of a cell or note fragment: (text without the break, applied paragraph style).
/// Processing instructions are not text; nested tables and notes are reported, never descended.
fn fragment_paragraphs(root: Node, part: &str, loss: &mut LossBook) -> Vec<(String, String)> {
    fn walk(n: Node, style: &str, out: &mut Vec<(String, String)>, open: &mut bool, part: &str, loss: &mut LossBook) {
        for c in kids(n) {
            match tag(c) {
                "ParagraphStyleRange" => walk(c, el_attr(c, "AppliedParagraphStyle").unwrap_or(style), out, open, part, loss),
                "CharacterStyleRange" => walk(c, style, out, open, part, loss),
                "Content" => {
                    if !*open {
                        out.push((String::new(), style.to_owned()));
                        *open = true;
                    }
                    if let Some(last) = out.last_mut() {
                        for t in c.children().filter(|t| t.is_text()) {
                            last.0.push_str(t.text().unwrap_or_default());
                        }
                    }
                }
                "Br" => {
                    if !*open {
                        out.push((String::new(), style.to_owned()));
                    }
                    *open = false;
                }
                "Properties" => {}
                "Table" => loss.note(part, "Table", LossKind::Unsupported, "nested_table_not_imported"),
                el @ ("Footnote" | "Endnote" | "EndnoteRange") => loss.note(part, el, LossKind::Unsupported, "note_in_note_or_cell_not_imported"),
                other => loss.note(part, other, LossKind::Unsupported, "fragment_element_not_interpreted"),
            }
        }
    }
    let mut out = Vec::new();
    walk(root, "", &mut out, &mut false, part, loss);
    out
}

struct Importer<'a> {
    import: &'a IdmlImport,
    limits: &'a Limits,
    mint: &'a mut dyn IdMint,
    loss: LossBook,
    styles: BTreeMap<(bool, String), String>,
    new_styles: Vec<StyleRef>,
    stories: Vec<StudioTextStory>,
    ids: Vec<IdLink>,
    colours: BTreeMap<String, Option<[f64; 3]>>,
}

impl Importer<'_> {
    fn style(&mut self, paragraph: bool, idml: &str) -> Option<String> {
        if idml.is_empty() || idml == "n" || idml.contains("[No paragraph style]") || idml.contains("[No character style]") {
            return None;
        }
        if let Some(s) = self.styles.get(&(paragraph, idml.to_owned())) {
            return Some(s.clone());
        }
        let id = self.mint.mint("STYS");
        self.styles.insert((paragraph, idml.to_owned()), id.clone());
        self.new_styles.push(StyleRef { folio: id.clone(), idml_name: idml.to_owned(), paragraph });
        Some(id)
    }
    fn folio_of(&self, kind: &str, idml: &str) -> Option<String> {
        self.import.proposal.ids.iter().find(|l| l.kind == kind && l.idml == idml).map(|l| l.folio.clone())
    }
    /// A story from (text, style) pieces laid back to back; each non-empty piece is a paragraph.
    fn story(&mut self, pieces: &[(String, String)], idml: String) -> StudioTextStory {
        let mut text = String::new();
        let mut paragraphs = Vec::new();
        for (t, style) in pieces {
            let start = text.len() as u64;
            text.push_str(t);
            if text.len() as u64 > start {
                paragraphs.push(StoryParagraph { start, end: text.len() as u64, style_ref: self.style(true, style), paragraph_direction: None });
            }
        }
        let story_id = self.mint.mint("STXT");
        self.ids.push(IdLink { kind: "story", idml, folio: story_id.clone() });
        let end = text.len() as u64;
        StudioTextStory {
            schema_id: TextStorySchema::V1,
            story_id,
            text_kind: StoryTextKind::Area,
            story_direction: StoryDirection::LeftToRight,
            content: StoryContent::Inline { text_utf8: text },
            paragraphs,
            character_ranges: if end == 0 { Vec::new() } else { vec![StoryRange { start: 0, end, style_ref: None, applied_language: None }] },
        }
    }
    /// A note story: paragraphs joined by newlines; the separator tab after the number marker
    /// (a processing instruction, already not text) is dropped.
    fn note_story(&mut self, mut paras: Vec<(String, String)>, idml: String) -> StudioTextStory {
        if let Some(first) = paras.first_mut()
            && first.0.starts_with('\t')
        {
            first.0.remove(0);
        }
        let n = paras.len();
        for (i, p) in paras.iter_mut().enumerate() {
            if i + 1 < n {
                p.0.push('\n');
            }
        }
        self.story(&paras, idml)
    }
    fn colour(&mut self, part: &str, swatch: Option<&str>) -> Option<StudioColor> {
        let s = swatch.filter(|s| !s.is_empty() && *s != "n" && *s != "Swatch/None")?;
        let rgb = match self.colours.get(s) {
            Some(c) => *c,
            None if s == "Color/Black" => Some([0.0; 3]),
            None if s == "Color/Paper" => Some([1.0; 3]),
            None => {
                self.loss.note(part, "Swatch", LossKind::Approximated, "swatch_unresolved_black");
                Some([0.0; 3])
            }
        };
        rgb.map(|[r, g, b]| StudioColor { r, g, b, colour_profile_id: ProfileBinding::Unassigned(()) })
    }
    fn read_colours(&mut self) -> Result<()> {
        let Some(p) = self.import.package.part(GRAPHIC_PART) else { return Ok(()) };
        let doc = parse_xml(text_of(p)?, self.limits)?;
        for c in doc.root_element().descendants().filter(|c| c.is_element() && tag(*c) == "Color") {
            let (Some(id), Some(space), Some(values)) = (el_attr(c, "Self"), el_attr(c, "Space"), el_attr(c, "ColorValue")) else { continue };
            let v: Vec<f64> = values.split_whitespace().filter_map(|x| x.parse::<f64>().ok()).filter(|x| x.is_finite()).collect();
            let rgb = match (space, v.as_slice()) {
                ("RGB", [r, g, b]) => Some([r / 255.0, g / 255.0, b / 255.0].map(|x| x.clamp(0.0, 1.0))),
                ("CMYK", [c, m, y, k]) => {
                    self.loss.note(GRAPHIC_PART, "Color", LossKind::Approximated, "cmyk_swatch_converted_without_profile");
                    let k = k / 100.0;
                    Some([c, m, y].map(|x| ((1.0 - x / 100.0) * (1.0 - k)).clamp(0.0, 1.0)))
                }
                _ => {
                    self.loss.note(GRAPHIC_PART, "Color", LossKind::Unsupported, "swatch_space_not_mapped");
                    None
                }
            };
            self.colours.insert(id.to_owned(), rgb);
        }
        Ok(())
    }
    fn number_style(&mut self, part: &str, element: &str, v: Option<&str>, default: StudioNumberStyle) -> StudioNumberStyle {
        match v {
            None => default,
            Some(v) => from(&NUMBER_STYLES, v).unwrap_or_else(|| {
                self.loss.note(part, element, LossKind::Approximated, "number_style_not_mapped");
                default
            }),
        }
    }
}

fn stroke_of(imp: &mut Importer<'_>, part: &str, c: Node, side: &str) -> Option<StudioCellStroke> {
    let weight = el_num(c, &format!("{side}EdgeStrokeWeight")).unwrap_or(0.0);
    let colour = el_attr(c, &format!("{side}EdgeStrokeColor"));
    if weight <= 0.0 || colour.is_none_or(|s| s == "Swatch/None") {
        return None;
    }
    let tint = el_num(c, &format!("{side}EdgeStrokeTint")).filter(|t| (0.0..=100.0).contains(t)).unwrap_or(100.0);
    let colour = imp.colour(part, colour)?;
    Some(StudioCellStroke { weight: pt(weight.min(1000.0)), colour, tint })
}

/// One `Table` object at `anchor` of host story `host`: the record and its cell story.
fn table_of(imp: &mut Importer<'_>, part: &str, xml: &str, host: &str, anchor: u64) -> Result<Option<(StudioTable, Option<StudioTextStory>)>> {
    let doc = parse_xml(xml, imp.limits)?;
    let t = doc.root_element();
    // Size gate on element counts before any grid or per-cell work.
    let (mut nrows, mut ncols, mut ncells) = (0usize, 0usize, 0usize);
    for c in kids(t) {
        match tag(c) {
            "Row" => nrows += 1,
            "Column" => ncols += 1,
            "Cell" => ncells += 1,
            _ => {}
        }
    }
    let too_large = nrows > MAX_TABLE_ROWS || ncols > MAX_TABLE_COLUMNS || nrows.saturating_mul(ncols) > MAX_TABLE_CELLS || ncells > MAX_TABLE_CELLS;
    if too_large || nrows == 0 || ncols == 0 {
        imp.loss.note(part, "Table", LossKind::Unsupported, if too_large { "table_too_large" } else { "table_empty" });
        return Ok(None);
    }
    let refuse = |imp: &mut Importer<'_>, why: &'static str| {
        imp.loss.note(part, "Table", LossKind::Unsupported, why);
        Ok(None)
    };
    let index = |n: Node| el_attr(n, "Name").and_then(|v| v.parse::<usize>().ok());
    let mut column_widths = vec![pt(72.0); ncols];
    for (i, c) in kids(t).filter(|c| tag(*c) == "Column").enumerate() {
        let slot = index(c).filter(|k| *k < ncols).unwrap_or(i);
        column_widths[slot] = pt(el_num(c, "SingleColumnWidth").filter(|w| *w > 0.0).unwrap_or(72.0));
    }
    let header = el_u32(t, "HeaderRowCount").unwrap_or(0) as usize;
    let footer = el_u32(t, "FooterRowCount").unwrap_or(0) as usize;
    let mut row_nodes: Vec<Option<Node>> = vec![None; nrows];
    for (i, r) in kids(t).filter(|c| tag(*c) == "Row").enumerate() {
        let slot = index(r).filter(|k| *k < nrows).unwrap_or(i);
        row_nodes[slot] = Some(r);
    }
    let mut rows = Vec::with_capacity(nrows);
    for (i, r) in row_nodes.iter().enumerate() {
        let (min, max) = match r {
            Some(r) if el_bool(*r, "AutoGrow") == Some(false) => {
                let h = el_num(*r, "SingleRowHeight").filter(|h| *h > 0.0).unwrap_or(12.0);
                (h, h)
            }
            Some(r) => {
                let min = el_num(*r, "MinimumHeight").filter(|h| *h >= 0.0).unwrap_or(3.0);
                (min, el_num(*r, "MaximumHeight").filter(|h| *h >= min && *h > 0.0).unwrap_or(min.max(1000.0)))
            }
            None => (3.0, 1000.0),
        };
        let row_type = if i < header {
            TableRowType::Header
        } else if i >= nrows.saturating_sub(footer) {
            TableRowType::Footer
        } else {
            TableRowType::Body
        };
        rows.push(StudioTableRow {
            row_type,
            minimum_height: pt(min),
            maximum_height: pt(max),
            keep_with_next_row: r.and_then(|r| el_bool(r, "KeepWithNextRow")).unwrap_or(false),
            start_row: r.and_then(|r| el_attr(r, "StartRow")).and_then(|v| from(&START_ROWS, v)).unwrap_or_default(),
            allow_split: false,
        });
    }
    // Anchor cells. A Cell element on a slot an earlier span took is the covered placeholder
    // (InDesign may or may not write it) and is skipped; a span past the grid edge or over another
    // anchor refuses the table at the first offending slot (at most nrows * ncols marks in total).
    let mut taken = vec![false; nrows * ncols];
    let mut cells = Vec::new();
    let mut texts: Vec<Option<Vec<(String, String)>>> = Vec::new();
    for c in kids(t).filter(|c| tag(*c) == "Cell") {
        let Some((col, row)) = el_attr(c, "Name").and_then(|n| n.split_once(':')).and_then(|(a, b)| Some((a.parse::<u32>().ok()?, b.parse::<u32>().ok()?)))
        else {
            imp.loss.note(part, "Cell", LossKind::Unsupported, "cell_name_invalid");
            continue;
        };
        if (row as usize) < nrows && (col as usize) < ncols && taken[row as usize * ncols + col as usize] {
            continue;
        }
        // A span that is present but not a positive u32 is refused, never read as 1.
        let span = |k: &str| el_attr(c, k).map_or(Some(1), |v| v.trim().parse::<u32>().ok().filter(|v| *v > 0));
        let (Some(row_span), Some(column_span)) = (span("RowSpan"), span("ColumnSpan")) else { return refuse(imp, "span_invalid") };
        let (Some(r1), Some(c1)) = (row.checked_add(row_span), col.checked_add(column_span)) else { return refuse(imp, "span_overflow") };
        if r1 as usize > nrows || c1 as usize > ncols {
            return refuse(imp, "span_overflow");
        }
        for r in row as usize..r1 as usize {
            for k in col as usize..c1 as usize {
                let slot = &mut taken[r * ncols + k];
                if *slot {
                    return refuse(imp, "cells_overlap");
                }
                *slot = true;
            }
        }
        let paras = fragment_paragraphs(c, part, &mut imp.loss);
        texts.push((!paras.is_empty()).then_some(paras));
        let inset = |k: &str| pt(el_num(c, k).filter(|v| *v >= 0.0).unwrap_or(0.0));
        let fill = imp.colour(part, el_attr(c, "FillColor"));
        cells.push(StudioTableCell {
            row,
            column: col,
            row_span,
            column_span,
            content: None,
            insets: LayoutEdges { top: inset("TopInset"), right: inset("RightInset"), bottom: inset("BottomInset"), left: inset("LeftInset") },
            fill_tint: fill.as_ref().and_then(|_| el_num(c, "FillTint")).filter(|t| (0.0..=100.0).contains(t)),
            fill,
            strokes: StudioCellEdges {
                top: stroke_of(imp, part, c, "Top"),
                left: stroke_of(imp, part, c, "Left"),
                bottom: stroke_of(imp, part, c, "Bottom"),
                right: stroke_of(imp, part, c, "Right"),
            },
            stroke_priorities: StudioEdgePriorities {
                top: el_num(c, "TopEdgeStrokePriority").unwrap_or(0.0) as i32,
                left: el_num(c, "LeftEdgeStrokePriority").unwrap_or(0.0) as i32,
                bottom: el_num(c, "BottomEdgeStrokePriority").unwrap_or(0.0) as i32,
                right: el_num(c, "RightEdgeStrokePriority").unwrap_or(0.0) as i32,
            },
            vertical_justification: el_attr(c, "VerticalJustification").and_then(|v| from(&VJS, v)).unwrap_or(VerticalJustification::Top),
        });
    }
    let table_id = el_attr(t, "Self").filter(|s| label_ok(s)).map_or_else(|| format!("table-{anchor}"), str::to_owned);
    let mut table = StudioTable {
        table_id: table_id.clone(),
        story_id: host.to_owned(),
        anchor,
        cell_story_id: None,
        column_widths,
        rows,
        cells,
        break_headers: el_attr(t, "BreakHeaders").and_then(|v| from(&BREAKS, v)).unwrap_or_default(),
        break_footers: el_attr(t, "BreakFooters").and_then(|v| from(&BREAKS, v)).unwrap_or_default(),
        skip_first_header: el_bool(t, "SkipFirstHeader").unwrap_or(false),
        skip_last_footer: el_bool(t, "SkipLastFooter").unwrap_or(false),
        stroke_order: el_attr(t, "StrokeOrder").and_then(|v| from(&STROKE_ORDERS, v)).unwrap_or_default(),
    };
    if let Err(why) = check_table_grid(&table) {
        return refuse(imp, why);
    }
    // The cell story: cell after cell, each cell's paragraphs joined by newlines.
    if texts.iter().all(Option::is_none) {
        return Ok(Some((table, None)));
    }
    let mut pieces: Vec<(String, String)> = Vec::new();
    let mut at = 0usize;
    for (cell, paras) in table.cells.iter_mut().zip(texts) {
        let Some(paras) = paras else { continue };
        let start = at;
        let n = paras.len();
        for (i, (mut text, style)) in paras.into_iter().enumerate() {
            if i + 1 < n {
                text.push('\n');
            }
            at += text.len();
            pieces.push((text, style));
        }
        cell.content = Some(StudioByteRange { start: start as u64, end: at as u64 });
    }
    let story = imp.story(&pieces, format!("{table_id}/cells"));
    table.cell_story_id = Some(story.story_id.clone());
    Ok(Some((table, Some(story))))
}

fn section_of(imp: &mut Importer<'_>, n: Node) -> Option<StudioSection> {
    let section_id = el_attr(n, "Self").filter(|s| label_ok(s))?.to_owned();
    let Some(page_start) = el_attr(n, "PageStart").and_then(|p| imp.folio_of("page", p)) else {
        imp.loss.note(DESIGNMAP, "Section", LossKind::Unsupported, "section_page_not_imported");
        return None;
    };
    let name = el_attr(n, "Name").filter(|s| !s.trim().is_empty() && label_ok(s)).map_or_else(|| section_id.clone(), str::to_owned);
    let continue_numbering = el_bool(n, "ContinueNumbering").unwrap_or(true);
    let style = prop(n, "PageNumberStyle");
    Some(StudioSection {
        name,
        page_start,
        continue_numbering,
        page_number_start: if continue_numbering { None } else { el_u32(n, "PageNumberStart") },
        page_number_style: imp.number_style(DESIGNMAP, "Section", style.as_deref(), StudioNumberStyle::Arabic),
        include_section_prefix: el_bool(n, "IncludeSectionPrefix").unwrap_or(false),
        section_prefix: el_attr(n, "SectionPrefix").filter(|s| !s.is_empty()).map(str::to_owned),
        marker: el_attr(n, "Marker").filter(|s| !s.is_empty()).map(str::to_owned),
        section_id,
    })
}

fn variable_of(imp: &mut Importer<'_>, n: Node, chapter: u32) -> Option<StudioTextVariable> {
    let variable_id = el_attr(n, "Name").or_else(|| el_attr(n, "Self")).filter(|s| label_ok(s))?.to_owned();
    let pref = kids(n).find(|c| tag(*c) != "Properties");
    let a = |k: &str| pref.and_then(|p| el_attr(p, k)).unwrap_or_default().to_owned();
    let vt = el_attr(n, "VariableType").unwrap_or_default();
    if vt != "ChapterNumberType" && (!a("TextBefore").is_empty() || !a("TextAfter").is_empty()) {
        imp.loss.note(DESIGNMAP, "TextVariable", LossKind::Unsupported, "variable_text_before_after_not_imported");
    }
    let format = a("Format");
    let number = |imp: &mut Importer<'_>, d: StudioNumberStyle| {
        let v = (!format.is_empty()).then_some(format.as_str());
        imp.number_style(DESIGNMAP, "TextVariable", v, d)
    };
    let kind = match vt {
        "CustomTextType" => StudioVariableKind::CustomText { text: a("Contents") },
        "FileNameType" => StudioVariableKind::FileName { include_path: a("IncludePath") == "true", include_extension: a("IncludeExtension") == "true" },
        "LastPageNumberType" => StudioVariableKind::LastPageNumber {
            scope: if a("Scope") == "SectionScope" { VariableScope::Section } else { VariableScope::Document },
            style: number(imp, StudioNumberStyle::Current),
        },
        "ChapterNumberType" => {
            StudioVariableKind::ChapterNumber { number: chapter, before: a("TextBefore"), after: a("TextAfter"), style: number(imp, StudioNumberStyle::Arabic) }
        }
        "OutputDateType" => StudioVariableKind::OutputDate { format: format.clone() },
        "CreationDateType" => StudioVariableKind::CreationDate { format: format.clone() },
        "ModificationDateType" => StudioVariableKind::ModificationDate { format: format.clone() },
        "MatchCharacterStyleType" | "MatchParagraphStyleType" => {
            let paragraph = vt == "MatchParagraphStyleType";
            let Some(style) = imp.style(paragraph, &a(if paragraph { "AppliedParagraphStyle" } else { "AppliedCharacterStyle" })) else {
                imp.loss.note(DESIGNMAP, "TextVariable", LossKind::Unsupported, "variable_style_missing");
                return None;
            };
            let use_on_page = if a("SearchStrategy") == "LastOnPage" { RunningUse::LastOnPage } else { RunningUse::FirstOnPage };
            let delete_end_punctuation = a("DeleteEndPunctuation") == "true";
            let change_case = from(&CASES, &a("ChangeCase")).unwrap_or_default();
            if paragraph {
                StudioVariableKind::MatchParagraphStyle { style, use_on_page, delete_end_punctuation, change_case }
            } else {
                StudioVariableKind::MatchCharacterStyle { style, use_on_page, delete_end_punctuation, change_case }
            }
        }
        "XrefPageNumberType" => StudioVariableKind::XrefPageNumber,
        "XrefChapterNumberType" => StudioVariableKind::XrefChapterNumber,
        "LiveCaptionType" => StudioVariableKind::LiveCaption,
        _ => {
            imp.loss.note(DESIGNMAP, "TextVariable", LossKind::Unsupported, "variable_type_not_mapped");
            return None;
        }
    };
    let v = StudioTextVariable { variable_id, kind };
    match check_running(std::slice::from_ref(&v), &[]) {
        Ok(()) => Some(v),
        Err(why) => {
            imp.loss.note(DESIGNMAP, "TextVariable", LossKind::Unsupported, why);
            None
        }
    }
}

fn toc_of(imp: &mut Importer<'_>, n: Node) -> Option<StudioTocStyle> {
    let raw = el_attr(n, "Self").unwrap_or_default();
    let toc_id = raw.strip_prefix("TOCStyle/").unwrap_or(raw);
    let mut entries = Vec::new();
    for e in kids(n).filter(|c| tag(*c) == "TOCStyleEntry") {
        let source = el_text(e, "Name");
        let source = if source.starts_with("ParagraphStyle/") { source } else { format!("ParagraphStyle/{source}") };
        let Some(paragraph_style) = imp.style(true, &source) else { continue };
        entries.push(StudioTocEntry {
            paragraph_style,
            format_style: el_attr(e, "FormatStyle").and_then(|s| imp.style(true, s)),
            level: el_u32(e, "Level").unwrap_or(1),
            page_number_position: el_attr(e, "PageNumberPosition").and_then(|v| from(&POSITIONS, v)).unwrap_or_default(),
            separator: special_in(el_attr(e, "Separator").unwrap_or("^t")),
            sort_alphabet: el_bool(e, "SortAlphabet").unwrap_or(false),
        });
    }
    let t = StudioTocStyle {
        toc_id: toc_id.to_owned(),
        name: el_attr(n, "Name").filter(|s| !s.is_empty()).unwrap_or(toc_id).to_owned(),
        title: el_text(n, "Title"),
        title_style: el_attr(n, "TitleStyle").and_then(|s| imp.style(true, s)),
        include_overset: el_bool(n, "IncludeOverset").unwrap_or(false),
        entries,
    };
    match check_toc_style(&t) {
        Ok(()) => Some(t),
        Err(why) => {
            imp.loss.note(STYLES_PART, "TOCStyle", LossKind::Unsupported, why);
            None
        }
    }
}

fn rule_of(imp: &mut Importer<'_>, n: Node, prefix: &str) -> Option<StudioFootnoteRule> {
    if el_bool(n, &format!("{prefix}On")) != Some(true) {
        return None;
    }
    let v = |k: &str| el_num(n, &format!("{prefix}{k}")).unwrap_or(0.0);
    let colour = imp.colour(PREFERENCES_PART, el_attr(n, &format!("{prefix}Color")).or(Some("Color/Black")))?;
    Some(StudioFootnoteRule {
        weight: pt(v("LineWeight")),
        colour,
        tint: el_num(n, &format!("{prefix}Tint")).unwrap_or(100.0),
        left_indent: pt(v("LeftIndent")),
        width: pt(v("Width")),
        offset: pt(v("Offset")),
    })
}

fn footnote_options_of(imp: &mut Importer<'_>, n: Node) -> Option<StudioFootnoteOptions> {
    let o = StudioFootnoteOptions {
        numbering_style: imp.number_style(PREFERENCES_PART, "FootnoteOption", el_attr(n, "FootnoteNumberingStyle"), StudioNumberStyle::Arabic),
        start_at: el_u32(n, "StartAt").unwrap_or(1),
        restart: el_attr(n, "RestartNumbering").and_then(|v| from(&RESTARTS, v)).unwrap_or_default(),
        prefix: el_text(n, "Prefix"),
        suffix: el_text(n, "Suffix"),
        space_between: pt(el_num(n, "SpaceBetween").unwrap_or(0.0)),
        spacer: pt(el_num(n, "Spacer").unwrap_or(0.0)),
        rule: rule_of(imp, n, "Rule"),
        continuing_rule: rule_of(imp, n, "ContinuingRule"),
        eos_placement: el_bool(n, "EosPlacement").unwrap_or(false),
        no_splitting: el_bool(n, "NoSplitting").unwrap_or(false),
    };
    match check_footnote_options(&o) {
        Ok(()) => Some(o),
        Err(why) => {
            imp.loss.note(PREFERENCES_PART, "FootnoteOption", LossKind::Unsupported, why);
            None
        }
    }
}

fn endnote_options_of(imp: &mut Importer<'_>, n: Node) -> StudioEndnoteOptions {
    StudioEndnoteOptions {
        title: el_text(n, "EndnoteTitle"),
        title_style: el_attr(n, "EndnoteTitleStyle").and_then(|s| imp.style(true, s)),
        text_style: el_attr(n, "EndnoteParagraphStyle").and_then(|s| imp.style(true, s)),
        numbering_style: imp.number_style(PREFERENCES_PART, "EndnoteOption", el_attr(n, "EndnoteNumberingStyle"), StudioNumberStyle::Arabic),
        start_at: el_u32(n, "StartEndnoteNumberAt").unwrap_or(1),
        restart: el_attr(n, "RestartEndnoteNumbering").and_then(|v| from(&ENDNOTE_RESTARTS, v)).unwrap_or_default(),
        scope: el_attr(n, "ScopeValue").and_then(|v| from(&ENDNOTE_SCOPES, v)).unwrap_or_default(),
        prefix: el_text(n, "EndnotePrefix"),
        suffix: el_text(n, "EndnoteSuffix"),
        separator_text: el_text(n, "EndnoteSeparatorText"),
    }
}

fn path_anchors(frame: Node) -> Vec<(f64, f64)> {
    let Some(array) = kid(frame, "Properties").and_then(|p| kid(p, "PathGeometry")).and_then(|g| kid(g, "GeometryPathType")).and_then(|g| kid(g, "PathPointArray"))
    else {
        return Vec::new();
    };
    kids(array)
        .filter_map(|p| {
            let mut it = el_attr(p, "Anchor")?.split_whitespace().map(|v| v.parse::<f64>().ok().filter(|v| v.is_finite() && v.abs() <= 1e9));
            Some((it.next()??, it.next()??))
        })
        .collect()
}

/// One spread graphic frame holding an `Image`: the record and its new raster layer.
fn graphic_of(imp: &mut Importer<'_>, si: usize, xml: &str) -> Result<Option<(StudioGraphicFrame, StudioLayer)>> {
    let import = imp.import;
    let spread = &import.model.spreads[si];
    let part = spread.part.as_str();
    let doc = parse_xml(xml, imp.limits)?;
    let r = doc.root_element();
    let Some(content) = kids(r).find(|c| matches!(tag(*c), "Image" | "PDF" | "EPS" | "ImportedPage" | "WMF" | "PICT" | "SVG")) else {
        return Ok(None);
    };
    if tag(content) != "Image" {
        imp.loss.note(part, tag(content), LossKind::Unsupported, "placed_content_not_raster");
        return Ok(None);
    }
    imp.loss.note(part, "Image", LossKind::Unsupported, "linked_pixels_not_imported");
    let (Some(tr), Some(ti)) = (affine_of(r), affine_of(content)) else {
        imp.loss.note(part, tag(r), LossKind::Unsupported, "graphic_transform_invalid");
        return Ok(None);
    };
    let anchors = path_anchors(r);
    let gb = kid(content, "Properties").and_then(|p| kid(p, "GraphicBounds"));
    let g = |k: &str| gb.and_then(|g| el_num(g, k));
    let (Some(l), Some(t), Some(right), Some(bottom)) = (g("Left"), g("Top"), g("Right"), g("Bottom")) else {
        imp.loss.note(part, "Image", LossKind::Unsupported, "graphic_bounds_missing");
        return Ok(None);
    };
    let (cw, ch) = (right - l, bottom - t);
    if anchors.is_empty() || cw <= 0.0 || ch <= 0.0 {
        imp.loss.note(part, tag(r), LossKind::Unsupported, "graphic_geometry_invalid");
        return Ok(None);
    }
    if tr[1].abs() > 1e-9 || tr[2].abs() > 1e-9 {
        imp.loss.note(part, tag(r), LossKind::Approximated, "graphic_frame_not_axis_aligned");
    }
    let fb = bbox(anchors.into_iter(), &tr);
    let rects: Vec<[f64; 4]> = spread
        .pages
        .iter()
        .map(|p| {
            let b = p.bounds_pt;
            bbox([(b[1], b[0]), (b[3], b[2])].into_iter(), &p.item_transform)
        })
        .collect();
    let overlap = |p: &[f64; 4]| (fb[2].min(p[2]) - fb[0].max(p[0])).max(0.0) * (fb[3].min(p[3]) - fb[1].max(p[1])).max(0.0);
    let Some(best) = (0..rects.len()).filter(|i| overlap(&rects[*i]) > 0.0).max_by(|a, c| overlap(&rects[*a]).total_cmp(&overlap(&rects[*c])).then(c.cmp(a)))
    else {
        imp.loss.note(part, tag(r), LossKind::Unsupported, "graphic_frame_off_page");
        return Ok(None);
    };
    let origin = rects[best];
    let (Some(page_id), Some(spread_id)) = (imp.folio_of("page", &spread.pages[best].id), imp.folio_of("spread", &spread.id)) else {
        return Ok(None);
    };
    let frame = Rect { x0_pt: fb[0] - origin[0], y0_pt: fb[1] - origin[1], x1_pt: fb[2] - origin[0], y1_pt: fb[3] - origin[1] };
    // Content (pt at 100 %, origin = content top-left) to page coordinates.
    let m = compose(&compose(&tr, &ti), &[1.0, 0.0, 0.0, 1.0, l, t]);
    let placed = [m[0], m[1], m[2], m[3], m[4] - origin[0], m[5] - origin[1]];
    let fo = kid(r, "FrameFittingOption");
    let opt = |k: &str| fo.and_then(|f| el_num(f, k)).unwrap_or(0.0);
    let crop = Crop { top: opt("TopCrop"), left: opt("LeftCrop"), bottom: opt("BottomCrop"), right: opt("RightCrop") };
    let frame_fitting = match fo.and_then(|f| el_attr(f, "FittingOnEmptyFrame")) {
        None | Some("None") => None,
        Some(v) => from(&EMPTY_FITS, v).or_else(|| {
            imp.loss.note(part, "FrameFittingOption", LossKind::Unsupported, "empty_frame_fitting_not_mapped");
            None
        }),
    };
    let anchor = fo.and_then(|f| el_attr(f, "FittingAlignment")).and_then(|v| from(&ANCHORS, v)).unwrap_or(StudioFitAnchor::Center);
    if fo.and_then(|f| el_bool(f, "AutoFit")) == Some(true) {
        imp.loss.note(part, "FrameFittingOption", LossKind::Unsupported, "auto_fit_not_imported");
    }
    let mut valid = false;
    let mut fit = None;
    for cand in FIT_ORDER {
        let Ok(f) = fit_content(frame, cw, ch, cand, frame_fitting, anchor, crop) else { continue };
        valid = true;
        if f.transform.iter().zip(placed).all(|(a, b)| (a - b).abs() <= FIT_TOLERANCE) {
            fit = Some(cand);
            break;
        }
    }
    if !valid {
        imp.loss.note(part, "FrameFittingOption", LossKind::Unsupported, "graphic_crop_invalid");
        return Ok(None);
    }
    let fit = fit.unwrap_or_else(|| {
        imp.loss.note(part, "Image", LossKind::Approximated, "graphic_placement_not_a_fit_mode");
        None
    });
    let shape = match tag(r) {
        "Oval" => StudioFrameShape::Ellipse,
        "Polygon" => {
            imp.loss.note(part, "Polygon", LossKind::Approximated, "polygon_frame_imported_as_rectangle");
            StudioFrameShape::Rectangle
        }
        _ => StudioFrameShape::Rectangle,
    };
    let any_crop = [crop.top, crop.left, crop.bottom, crop.right].iter().any(|v| *v != 0.0);
    let layer_id = imp.mint.mint("SLYR");
    imp.ids.push(IdLink { kind: "graphic", idml: el_text(r, "Self"), folio: layer_id.clone() });
    let name = el_attr(r, "Name").filter(|n| !n.starts_with("$ID/") && label_ok(n)).unwrap_or("Graphic frame");
    let layer = StudioLayer {
        schema_id: LayerSchema::V1,
        layer_id: layer_id.clone(),
        name: name.to_owned(),
        parent: Parent::Node(NodeRef { node_kind: NodeKind::PageSpread, node_id: spread_id }),
        kind: "raster".into(),
        payload: Payload::Raster { tiles: Vec::new() },
        visible: el_attr(r, "Visible") != Some("false"),
        appearance: None,
        text_frame: None,
    };
    let record = StudioGraphicFrame {
        layer_id,
        page_id,
        bounds: hsk_studio_folio::Bounds { x: pt(frame.x0_pt), y: pt(frame.y0_pt), width: pt(frame.x1_pt - frame.x0_pt), height: pt(frame.y1_pt - frame.y0_pt) },
        shape,
        content_width: pt(cw),
        content_height: pt(ch),
        fit,
        frame_fitting,
        anchor,
        crop: any_crop.then(|| LayoutEdges { top: pt(crop.top), right: pt(crop.right), bottom: pt(crop.bottom), left: pt(crop.left) }),
    };
    Ok(Some((record, layer)))
}

/// Maps the long-document features of an IDML import into Folio long-document records (module
/// docs). Nothing is committed; the proposal of `import` is extended, not changed.
pub fn import_longdoc(import: &IdmlImport, limits: &Limits, mint: &mut dyn IdMint, cancel: &CancellationToken) -> Result<LongDocImport> {
    canceled(cancel)?;
    let mut imp = Importer {
        import,
        limits,
        mint,
        loss: LossBook::default(),
        styles: import.proposal.style_refs.iter().map(|s| ((s.paragraph, s.idml_name.clone()), s.folio.clone())).collect(),
        new_styles: Vec::new(),
        stories: Vec::new(),
        ids: Vec::new(),
        colours: BTreeMap::new(),
    };
    imp.read_colours()?;
    let base = import.proposal.snapshot.document();
    let mut records = StudioLongDocRecords::new(base.document_id.clone());
    let mut container_stories = Vec::new();

    // Inline objects of the stories.
    let mut ranges: BTreeMap<String, (String, Arc<Opaque>)> = BTreeMap::new();
    let mut references: Vec<(String, String, String, u64)> = Vec::new();
    let (mut table_keys, mut note_keys) = (BTreeSet::new(), BTreeSet::new());
    for st in &import.model.stories {
        canceled(cancel)?;
        let Some(host) = imp.folio_of("story", &st.id) else { continue };
        let endnote_story = st.attrs.iter().any(|(k, v)| k == "IsEndnoteStory" && v == "true");
        if endnote_story {
            container_stories.push(host.clone());
        }
        let mut at = 0u64;
        for run in st.paragraphs.iter().flat_map(|p| &p.runs) {
            if let Some(obj) = &run.inline {
                match (obj.element.as_str(), endnote_story) {
                    ("EndnoteRange", true) => {
                        let doc = parse_xml(&obj.xml, limits)?;
                        match el_attr(doc.root_element(), "SourceEndnote") {
                            Some(src) if !ranges.contains_key(src) => {
                                ranges.insert(src.to_owned(), (st.part.clone(), obj.clone()));
                            }
                            _ => imp.loss.note(&st.part, "EndnoteRange", LossKind::Unsupported, "endnote_range_source_invalid"),
                        }
                    }
                    ("Table", false) if records.tables.len() >= MAX_TABLES => imp.loss.note(&st.part, "Table", LossKind::Unsupported, "too_many_tables"),
                    ("Table", false) => {
                        if let Some((mut t, cells)) = table_of(&mut imp, &st.part, &obj.xml, &host, at)? {
                            if !table_keys.insert(t.table_id.clone()) {
                                t.table_id = format!("table-{}", table_keys.len());
                                table_keys.insert(t.table_id.clone());
                            }
                            records.tables.push(t);
                            imp.stories.extend(cells);
                        }
                    }
                    ("Footnote", false) => {
                        let doc = parse_xml(&obj.xml, limits)?;
                        let root = doc.root_element();
                        let paras = fragment_paragraphs(root, &st.part, &mut imp.loss);
                        let mut note_id = el_attr(root, "Self").filter(|s| label_ok(s)).map_or_else(|| format!("footnote-{at}"), str::to_owned);
                        if !note_keys.insert(note_id.clone()) {
                            note_id = format!("footnote-{}", note_keys.len());
                            note_keys.insert(note_id.clone());
                        }
                        let story = imp.note_story(paras, format!("{note_id}/note"));
                        records.footnotes.push(StudioFootnote { note_id, story_id: host.clone(), anchor: at, note_story_id: story.story_id.clone() });
                        imp.stories.push(story);
                    }
                    ("Endnote", false) => {
                        let doc = parse_xml(&obj.xml, limits)?;
                        match el_attr(doc.root_element(), "Self").filter(|s| label_ok(s)) {
                            Some(id) => references.push((id.to_owned(), st.part.clone(), host.clone(), at)),
                            None => imp.loss.note(&st.part, "Endnote", LossKind::Unsupported, "endnote_reference_without_self"),
                        }
                    }
                    ("TextVariableInstance", _) => imp.loss.note(&st.part, "TextVariableInstance", LossKind::Unsupported, "variable_instance_text_not_in_story"),
                    (el, _) => imp.loss.note(&st.part, el, LossKind::Unsupported, "inline_object_out_of_place"),
                }
            }
            at += run.text.len() as u64 + u64::from(run.br);
        }
    }
    let mut endnote_keys = BTreeSet::new();
    for (note_id, part, host, anchor) in references {
        match ranges.remove(&note_id) {
            Some((_, obj)) if endnote_keys.insert(note_id.clone()) => {
                let doc = parse_xml(&obj.xml, limits)?;
                let paras = fragment_paragraphs(doc.root_element(), &part, &mut imp.loss);
                let story = imp.note_story(paras, format!("{note_id}/note"));
                records.endnotes.push(StudioEndnote { note_id, story_id: host, anchor, note_story_id: story.story_id.clone() });
                imp.stories.push(story);
            }
            _ => imp.loss.note(&part, "Endnote", LossKind::Unsupported, "endnote_range_missing"),
        }
    }
    for (part, _) in ranges.into_values() {
        imp.loss.note(&part, "EndnoteRange", LossKind::Unsupported, "endnote_range_without_reference");
    }

    // Preferences: chapter number, footnote and endnote options.
    let mut chapter = 1u32;
    if let Some(p) = import.package.part(PREFERENCES_PART) {
        let doc = parse_xml(text_of(p)?, limits)?;
        for e in kids(doc.root_element()) {
            match tag(e) {
                "ChapterNumberPreference" => chapter = el_u32(e, "ChapterNumber").unwrap_or(1),
                "FootnoteOption" => records.footnote_options = footnote_options_of(&mut imp, e),
                "EndnoteOption" => records.endnote_options = Some(endnote_options_of(&mut imp, e)),
                _ => {}
            }
        }
    }
    // Designmap: sections and text variables.
    let mut sections = Vec::new();
    let mut var_keys = BTreeSet::new();
    for item in &import.model.designmap {
        let DesignmapItem::Opaque(o) = item else { continue };
        if o.element != "Section" && o.element != "TextVariable" {
            continue;
        }
        let doc = parse_xml(&o.xml, limits)?;
        let n = doc.root_element();
        if o.element == "Section" {
            sections.extend(section_of(&mut imp, n));
        } else if let Some(v) = variable_of(&mut imp, n, chapter) {
            if var_keys.insert(v.variable_id.clone()) {
                records.text_variables.push(v);
            } else {
                imp.loss.note(DESIGNMAP, "TextVariable", LossKind::Unsupported, "variable_duplicate");
            }
        }
    }
    // TOC styles.
    if let Some(p) = import.package.part(STYLES_PART) {
        let doc = parse_xml(text_of(p)?, limits)?;
        let mut keys = BTreeSet::new();
        for n in doc.root_element().descendants().filter(|n| n.is_element() && tag(*n) == "TOCStyle") {
            if let Some(t) = toc_of(&mut imp, n) {
                if keys.insert(t.toc_id.clone()) {
                    records.toc_styles.push(t);
                } else {
                    imp.loss.note(STYLES_PART, "TOCStyle", LossKind::Unsupported, "toc_duplicate");
                }
            }
        }
    }
    // Graphic frames.
    let mut document = base.clone();
    for (si, s) in import.model.spreads.iter().enumerate() {
        for item in &s.items {
            canceled(cancel)?;
            let SpreadItem::Opaque(o) = item else { continue };
            if !matches!(o.element.as_str(), "Rectangle" | "Oval" | "Polygon") {
                continue;
            }
            if let Some((g, layer)) = graphic_of(&mut imp, si, &o.xml)? {
                records.graphic_frames.push(g);
                document.layers.push(layer);
            }
        }
    }
    let known: BTreeSet<String> = import.proposal.stories.iter().map(|s| s.story.story_id.clone()).collect();
    let snapshot = validate_document(document, limits.folio, cancel, &Known(&known)).map_err(folio_err)?;
    match check_sections(&sections, snapshot.document()) {
        Ok(()) => records.sections = sections,
        Err(why) => imp.loss.note(DESIGNMAP, "Section", LossKind::Unsupported, why),
    }

    let Importer { loss, new_styles, stories, ids, .. } = imp;
    let mut all = known;
    all.extend(stories.iter().map(|s| s.story_id.clone()));
    let resolver = Known(&all);
    let mut unresolved = Vec::new();
    for s in &stories {
        unresolved.extend(validate_text_story(s, limits.folio, cancel, &resolver).map_err(folio_err)?);
    }
    unresolved.extend(validate_longdoc_records(&records, snapshot.document(), limits.folio, cancel, &resolver).map_err(folio_err)?);
    Ok(LongDocImport { snapshot, records, stories, container_stories, style_refs: new_styles, ids, unresolved, loss: loss.finish() })
}

// ---------------------------------------------------------------------------------------------
// Export.

/// The long-document records to write and the stories they name that are in no frame (cell,
/// footnote and endnote stories).
#[derive(Clone, Copy, Debug)]
pub struct LongDocExportInput<'a> {
    pub records: &'a StudioLongDocRecords,
    pub stories: &'a [StoryInput<'a>],
}

struct Writer<'a> {
    names: &'a ExportNames,
    loss: &'a mut LossBook,
    swatches: BTreeMap<String, String>,
    swatch_xml: String,
}
impl Writer<'_> {
    fn style(&mut self, paragraph: bool, id: Option<&str>, part: &str, element: &str) -> String {
        let (map, none) = if paragraph { (&self.names.paragraph_styles, NO_PARAGRAPH_STYLE) } else { (&self.names.character_styles, NO_CHARACTER_STYLE) };
        let Some(id) = id else { return none.to_owned() };
        match map.get(id) {
            Some(n) => n.clone(),
            None => {
                self.loss.note(part, element, LossKind::Approximated, "style_name_unmapped");
                none.to_owned()
            }
        }
    }
    fn swatch(&mut self, c: &StudioColor, part: &str) -> String {
        if matches!(c.colour_profile_id, ProfileBinding::Assigned(_)) {
            self.loss.note(part, "Color", LossKind::Approximated, "colour_profile_not_exported");
        }
        let value = format!("{} {} {}", num(c.r * 255.0), num(c.g * 255.0), num(c.b * 255.0));
        if let Some(s) = self.swatches.get(&value) {
            return s.clone();
        }
        let n = self.swatches.len() + 1;
        let id = format!("Color/hsk{n}");
        let _ = writeln!(self.swatch_xml, "<Color Self=\"{id}\" Model=\"Process\" Space=\"RGB\" ColorValue=\"{value}\" Name=\"hsk{n}\"/>");
        self.swatches.insert(value, id.clone());
        id
    }
    /// `ParagraphStyleRange`s of `text` split at newlines; `base` is its byte in `story` (for the
    /// paragraph styles); `first_prefix` is raw XML put before the first paragraph's text.
    fn paragraphs_xml(&mut self, text: &str, story: &StudioTextStory, base: u64, first_prefix: &str, part: &str) -> String {
        let mut out = String::new();
        let mut pos = base;
        let pieces: Vec<&str> = text.split('\n').collect();
        let n = pieces.len();
        for (i, piece) in pieces.into_iter().enumerate() {
            let id = story.paragraphs.iter().find(|p| p.start <= pos && pos < p.end).and_then(|p| p.style_ref.as_deref());
            let style = self.style(true, id, part, "ParagraphStyleRange");
            let _ = write!(
                out,
                "<ParagraphStyleRange AppliedParagraphStyle=\"{}\"><CharacterStyleRange AppliedCharacterStyle=\"{}\"><Content>{}{}</Content>{}</CharacterStyleRange></ParagraphStyleRange>",
                esc(&style, true),
                esc(NO_CHARACTER_STYLE, true),
                if i == 0 { first_prefix } else { "" },
                esc(piece, false),
                if i + 1 < n { "<Br/>" } else { "" }
            );
            pos += piece.len() as u64 + 1;
        }
        out
    }
}

fn story_text<'a>(s: &StoryInput<'a>) -> Result<&'a str> {
    match (&s.story.content, s.resolved_text) {
        (StoryContent::Inline { text_utf8 }, None) => Ok(text_utf8.as_str()),
        (StoryContent::Stored { byte_len, .. }, Some(t)) if *byte_len == t.len() as u64 => Ok(t),
        _ => Err(IdmlError::Unsupported("story_content_not_resolved")),
    }
}

fn slice(text: &str, start: u64, end: u64) -> Result<&str> {
    match (usize::try_from(start).ok(), usize::try_from(end).ok()) {
        (Some(a), Some(b)) if a <= b && b <= text.len() && text.is_char_boundary(a) && text.is_char_boundary(b) => Ok(&text[a..b]),
        _ => Err(IdmlError::InvalidEdit("story_range_not_on_char_boundary")),
    }
}

fn edge_xml(out: &mut String, w: &mut Writer<'_>, side: &str, s: Option<&StudioCellStroke>, priority: i32, part: &str) {
    match s {
        None => {
            let _ = write!(out, " {side}EdgeStrokeWeight=\"0\"");
        }
        Some(s) => {
            let colour = w.swatch(&s.colour, part);
            let _ = write!(out, " {side}EdgeStrokeWeight=\"{}\" {side}EdgeStrokeColor=\"{colour}\" {side}EdgeStrokeTint=\"{}\"", num(s.weight.value), num(s.tint));
        }
    }
    if priority != 0 {
        let _ = write!(out, " {side}EdgeStrokePriority=\"{priority}\"");
    }
}

fn table_xml(t: &StudioTable, cells: Option<(&StudioTextStory, &str)>, w: &mut Writer<'_>, part: &str) -> Result<String> {
    let count = |k: TableRowType| t.rows.iter().filter(|r| r.row_type == k).count();
    let tid = esc(&t.table_id, true);
    let mut s = String::new();
    let _ = write!(
        s,
        "<Table Self=\"{tid}\" HeaderRowCount=\"{}\" BodyRowCount=\"{}\" FooterRowCount=\"{}\" ColumnCount=\"{}\" BreakHeaders=\"{}\" BreakFooters=\"{}\" SkipFirstHeader=\"{}\" SkipLastFooter=\"{}\" StrokeOrder=\"{}\">",
        count(TableRowType::Header),
        count(TableRowType::Body),
        count(TableRowType::Footer),
        t.column_widths.len(),
        to(&BREAKS, t.break_headers),
        to(&BREAKS, t.break_footers),
        t.skip_first_header,
        t.skip_last_footer,
        to(&STROKE_ORDERS, t.stroke_order),
    );
    for (i, r) in t.rows.iter().enumerate() {
        if r.allow_split {
            w.loss.note(part, "Row", LossKind::Unsupported, "row_allow_split_not_exported");
        }
        let _ = write!(s, "<Row Self=\"{tid}r{i}\" Name=\"{i}\"");
        if r.minimum_height.value == r.maximum_height.value {
            let _ = write!(s, " AutoGrow=\"false\" SingleRowHeight=\"{}\"", num(r.minimum_height.value));
        } else {
            let _ = write!(s, " AutoGrow=\"true\" MinimumHeight=\"{}\" MaximumHeight=\"{}\"", num(r.minimum_height.value), num(r.maximum_height.value));
        }
        let _ = write!(s, " KeepWithNextRow=\"{}\" StartRow=\"{}\"/>", r.keep_with_next_row, to(&START_ROWS, r.start_row));
    }
    for (i, c) in t.column_widths.iter().enumerate() {
        let _ = write!(s, "<Column Self=\"{tid}c{i}\" Name=\"{i}\" SingleColumnWidth=\"{}\"/>", num(c.value));
    }
    if let Some((story, _)) = cells
        && story.character_ranges.iter().any(|r| r.style_ref.is_some())
    {
        w.loss.note(part, "Cell", LossKind::Unsupported, "cell_character_styles_not_exported");
    }
    for c in &t.cells {
        let _ = write!(
            s,
            "<Cell Self=\"{tid}i{}x{}\" Name=\"{}:{}\" RowSpan=\"{}\" ColumnSpan=\"{}\" TopInset=\"{}\" LeftInset=\"{}\" BottomInset=\"{}\" RightInset=\"{}\" VerticalJustification=\"{}\"",
            c.column,
            c.row,
            c.column,
            c.row,
            c.row_span,
            c.column_span,
            num(c.insets.top.value),
            num(c.insets.left.value),
            num(c.insets.bottom.value),
            num(c.insets.right.value),
            to(&VJS, c.vertical_justification),
        );
        if let Some(f) = &c.fill {
            let id = w.swatch(f, part);
            let _ = write!(s, " FillColor=\"{id}\"");
            if let Some(tint) = c.fill_tint {
                let _ = write!(s, " FillTint=\"{}\"", num(tint));
            }
        }
        let p = &c.stroke_priorities;
        edge_xml(&mut s, w, "Top", c.strokes.top.as_ref(), p.top, part);
        edge_xml(&mut s, w, "Left", c.strokes.left.as_ref(), p.left, part);
        edge_xml(&mut s, w, "Bottom", c.strokes.bottom.as_ref(), p.bottom, part);
        edge_xml(&mut s, w, "Right", c.strokes.right.as_ref(), p.right, part);
        s.push('>');
        if let (Some(range), Some((story, text))) = (c.content, cells) {
            let piece = slice(text, range.start, range.end)?;
            s.push_str(&w.paragraphs_xml(piece, story, range.start, "", part));
        }
        s.push_str("</Cell>");
    }
    s.push_str("</Table>");
    Ok(s)
}

fn object_run(obj: Opaque) -> RunModel {
    RunModel {
        char_style: NO_CHARACTER_STYLE.into(),
        attrs: Arc::new(Attrs::new()),
        props: None,
        text: String::new(),
        br: false,
        special: false,
        language: None,
        span: None,
        inline: Some(Arc::new(obj)),
    }
}

/// Splits the runs of `st` at the host bytes of `items` and places each object there; an object at
/// a paragraph end goes before the break, one past the story end into a new last paragraph.
fn insert_inline(st: &mut StoryModel, mut items: Vec<(u64, Opaque)>, loss: &mut LossBook) -> Result<()> {
    let bad = IdmlError::InvalidEdit("story_range_not_on_char_boundary");
    items.sort_by_key(|(a, _)| *a);
    let mut items = items.into_iter().peekable();
    let mut off = 0u64;
    for para in &mut st.paragraphs {
        let runs = std::mem::take(&mut para.runs);
        let mut out = Vec::with_capacity(runs.len());
        for run in runs {
            let (len, br) = (run.text.len() as u64, run.br);
            let mut cut = 0usize;
            let mut split = false;
            while items.peek().is_some_and(|(a, _)| *a <= off + len) {
                let Some((a, obj)) = items.next() else { break };
                let k = usize::try_from(a.saturating_sub(off)).map_err(|_| bad.clone())?;
                if !run.text.is_char_boundary(k) {
                    return Err(bad);
                }
                if k > cut {
                    out.push(RunModel { text: run.text[cut..k].to_owned(), br: false, span: None, inline: None, ..run.clone() });
                }
                out.push(RunModel { text: String::new(), br: false, special: false, span: None, inline: Some(Arc::new(obj)), ..run.clone() });
                cut = k;
                split = true;
            }
            if !split {
                out.push(run);
            } else if cut < run.text.len() || br {
                out.push(RunModel { text: run.text[cut..].to_owned(), span: None, inline: None, ..run });
            }
            off += len + u64::from(br);
        }
        para.runs = out;
    }
    let mut rest = Vec::new();
    for (a, obj) in items {
        if a > off {
            loss.note(&st.part, &obj.element, LossKind::Approximated, "anchor_past_story_end");
        }
        rest.push(object_run(obj));
    }
    if !rest.is_empty() {
        st.paragraphs.push(ParaModel { style: NO_PARAGRAPH_STYLE.into(), attrs: Arc::new(Attrs::new()), props: None, runs: rest });
    }
    Ok(())
}

fn note_xml(element: &str, id: &str, extra: &str, story: &StoryInput<'_>, w: &mut Writer<'_>, part: &str) -> Result<String> {
    let text = story_text(story)?;
    let body = w.paragraphs_xml(text, story.story, 0, "<?ACE 4?>\t", part);
    Ok(format!("<{element} Self=\"{}\"{extra}>{body}</{element}>", esc(id, true)))
}

fn graphic_xml(g: &StudioGraphicFrame, name: &str, n: usize, page_at: (f64, f64), w: &mut Writer<'_>, part: &str) -> Result<Opaque> {
    let frame = Rect { x0_pt: g.bounds.x.value, y0_pt: g.bounds.y.value, x1_pt: g.bounds.x.value + g.bounds.width.value, y1_pt: g.bounds.y.value + g.bounds.height.value };
    let crop = g.crop.as_ref().map_or(Crop { top: 0.0, left: 0.0, bottom: 0.0, right: 0.0 }, |e| Crop {
        top: e.top.value,
        left: e.left.value,
        bottom: e.bottom.value,
        right: e.right.value,
    });
    let (cw, ch) = (g.content_width.value, g.content_height.value);
    let fitted = match fit_content(frame, cw, ch, g.fit, g.frame_fitting, g.anchor, crop) {
        Ok(f) => f,
        Err(_) => {
            w.loss.note(part, "Image", LossKind::Approximated, "fit_not_reproducible_exported_unfitted");
            fit_content(frame, cw, ch, None, None, g.anchor, crop).map_err(|_| IdmlError::InvalidGeometry)?
        }
    };
    let (px, py) = page_at;
    let f = fitted.frame;
    let element = match &g.shape {
        StudioFrameShape::Ellipse => "Oval",
        StudioFrameShape::Rectangle => "Rectangle",
        StudioFrameShape::Path { .. } => {
            w.loss.note(part, "Rectangle", LossKind::Approximated, "frame_path_shape_exported_as_rectangle");
            "Rectangle"
        }
    };
    let points: String = [(f.x0_pt, f.y0_pt), (f.x0_pt, f.y1_pt), (f.x1_pt, f.y1_pt), (f.x1_pt, f.y0_pt)]
        .iter()
        .map(|(x, y)| {
            let a = format!("{} {}", num(x + px), num(y + py));
            format!("<PathPointType Anchor=\"{a}\" LeftDirection=\"{a}\" RightDirection=\"{a}\"/>")
        })
        .collect();
    let empty_fit = match g.frame_fitting.map(|f| to(&EMPTY_FITS, f)) {
        None => "None",
        Some("") => {
            w.loss.note(part, "FrameFittingOption", LossKind::Approximated, "frame_fitting_not_an_empty_frame_option");
            "None"
        }
        Some(v) => v,
    };
    let t = fitted.transform;
    let xml = format!(
        "<{element} Self=\"ug{n:x}\" Name=\"{}\" ContentType=\"GraphicType\" ItemTransform=\"1 0 0 1 0 0\"><Properties><PathGeometry><GeometryPathType PathOpen=\"false\"><PathPointArray>{points}</PathPointArray></GeometryPathType></PathGeometry></Properties><FrameFittingOption LeftCrop=\"{}\" TopCrop=\"{}\" RightCrop=\"{}\" BottomCrop=\"{}\" FittingOnEmptyFrame=\"{empty_fit}\" FittingAlignment=\"{}\"/><Image Self=\"ug{n:x}i\" ItemTransform=\"{} {} {} {} {} {}\"><Properties><GraphicBounds Left=\"0\" Top=\"0\" Right=\"{}\" Bottom=\"{}\"/></Properties><Link Self=\"ug{n:x}l\" LinkResourceURI=\"hsk-studio-layer:{}\"/></Image></{element}>",
        esc(name, true),
        num(crop.left),
        num(crop.top),
        num(crop.right),
        num(crop.bottom),
        to(&ANCHORS, g.anchor),
        num(t[0]),
        num(t[1]),
        num(t[2]),
        num(t[3]),
        num(t[4] + px),
        num(t[5] + py),
        num(cw),
        num(ch),
        esc(&g.layer_id, true),
    );
    Ok(opaque(element, xml))
}

fn variable_xml(v: &StudioTextVariable, w: &mut Writer<'_>) -> String {
    let (vt, pref) = match &v.kind {
        StudioVariableKind::CustomText { text } => ("CustomTextType", format!("<CustomTextVariablePreference Contents=\"{}\"/>", esc(text, true))),
        StudioVariableKind::FileName { include_path, include_extension } => (
            "FileNameType",
            format!("<FileNameVariablePreference IncludePath=\"{include_path}\" IncludeExtension=\"{include_extension}\" TextBefore=\"\" TextAfter=\"\"/>"),
        ),
        StudioVariableKind::LastPageNumber { scope, style } => (
            "LastPageNumberType",
            format!(
                "<PageNumberVariablePreference Format=\"{}\" Scope=\"{}\" TextBefore=\"\" TextAfter=\"\"/>",
                to(&NUMBER_STYLES, *style),
                if *scope == VariableScope::Section { "SectionScope" } else { "EntireDocument" }
            ),
        ),
        StudioVariableKind::ChapterNumber { before, after, style, .. } => (
            "ChapterNumberType",
            format!("<ChapterNumberVariablePreference Format=\"{}\" TextBefore=\"{}\" TextAfter=\"{}\"/>", to(&NUMBER_STYLES, *style), esc(before, true), esc(after, true)),
        ),
        StudioVariableKind::OutputDate { format } => ("OutputDateType", format!("<DateVariablePreference Format=\"{}\"/>", esc(format, true))),
        StudioVariableKind::CreationDate { format } => ("CreationDateType", format!("<DateVariablePreference Format=\"{}\"/>", esc(format, true))),
        StudioVariableKind::ModificationDate { format } => ("ModificationDateType", format!("<DateVariablePreference Format=\"{}\"/>", esc(format, true))),
        StudioVariableKind::MatchCharacterStyle { style, use_on_page, delete_end_punctuation, change_case }
        | StudioVariableKind::MatchParagraphStyle { style, use_on_page, delete_end_punctuation, change_case } => {
            let paragraph = matches!(v.kind, StudioVariableKind::MatchParagraphStyle { .. });
            let name = w.style(paragraph, Some(style), DESIGNMAP, "TextVariable");
            let (vt, el, key) = if paragraph {
                ("MatchParagraphStyleType", "MatchParagraphStylePreference", "AppliedParagraphStyle")
            } else {
                ("MatchCharacterStyleType", "MatchCharacterStylePreference", "AppliedCharacterStyle")
            };
            (
                vt,
                format!(
                    "<{el} {key}=\"{}\" SearchStrategy=\"{}\" DeleteEndPunctuation=\"{delete_end_punctuation}\" ChangeCase=\"{}\"/>",
                    esc(&name, true),
                    if *use_on_page == RunningUse::LastOnPage { "LastOnPage" } else { "FirstOnPage" },
                    to(&CASES, *change_case)
                ),
            )
        }
        StudioVariableKind::XrefPageNumber => ("XrefPageNumberType", String::new()),
        StudioVariableKind::XrefChapterNumber => ("XrefChapterNumberType", String::new()),
        StudioVariableKind::LiveCaption => ("LiveCaptionType", String::new()),
    };
    let id = esc(&v.variable_id, true);
    format!("<TextVariable Self=\"TextVariable/{id}\" Name=\"{id}\" VariableType=\"{vt}\">{pref}</TextVariable>")
}

fn toc_xml(t: &StudioTocStyle, w: &mut Writer<'_>) -> String {
    let title_style = w.style(true, t.title_style.as_deref(), STYLES_PART, "TOCStyle");
    let mut s = format!(
        "<TOCStyle Self=\"TOCStyle/{}\" Name=\"{}\" Title=\"{}\" TitleStyle=\"{}\" IncludeOverset=\"{}\">",
        esc(&t.toc_id, true),
        esc(&t.name, true),
        esc(&t.title, true),
        esc(&title_style, true),
        t.include_overset
    );
    for e in &t.entries {
        let source = w.style(true, Some(&e.paragraph_style), STYLES_PART, "TOCStyleEntry");
        let format = w.style(true, e.format_style.as_deref(), STYLES_PART, "TOCStyleEntry");
        let _ = write!(
            s,
            "<TOCStyleEntry Name=\"{}\" FormatStyle=\"{}\" Level=\"{}\" PageNumberPosition=\"{}\" Separator=\"{}\" SortAlphabet=\"{}\"/>",
            esc(source.strip_prefix("ParagraphStyle/").unwrap_or(&source), true),
            esc(&format, true),
            e.level,
            to(&POSITIONS, e.page_number_position),
            esc(&special_out(&e.separator), true),
            e.sort_alphabet
        );
    }
    s.push_str("</TOCStyle>");
    s
}

fn rule_xml(out: &mut String, prefix: &str, r: Option<&StudioFootnoteRule>, w: &mut Writer<'_>) {
    let _ = write!(out, " {prefix}On=\"{}\"", r.is_some());
    if let Some(r) = r {
        let colour = w.swatch(&r.colour, PREFERENCES_PART);
        let _ = write!(
            out,
            " {prefix}LineWeight=\"{}\" {prefix}Color=\"{colour}\" {prefix}Tint=\"{}\" {prefix}LeftIndent=\"{}\" {prefix}Width=\"{}\" {prefix}Offset=\"{}\"",
            num(r.weight.value),
            num(r.tint),
            num(r.left_indent.value),
            num(r.width.value),
            num(r.offset.value)
        );
    }
}

fn wrapper(element: &str, dom: &str, body: &str) -> Vec<u8> {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<idPkg:{element} xmlns:idPkg=\"{IDPKG_NS}\" DOMVersion=\"{dom}\">\n{body}</idPkg:{element}>\n").into_bytes()
}

/// Exports a Folio document, its stories and its long-document records as one IDML package
/// (module docs). The records are validated against the document first.
pub fn export_folio_longdoc(input: &FolioExportInput, longdoc: &LongDocExportInput, limits: &Limits, cancel: &CancellationToken) -> Result<FolioExport> {
    canceled(cancel)?;
    let r = longdoc.records;
    let d = input.document;
    let known: BTreeSet<String> = input.stories.iter().chain(longdoc.stories).map(|s| s.story.story_id.clone()).collect();
    validate_longdoc_records(r, d, limits.folio, cancel, &Known(&known)).map_err(folio_err)?;
    let handled: BTreeSet<&str> = r.graphic_frames.iter().map(|g| g.layer_id.as_str()).collect();
    let mut built = folio_model(input, &handled, limits, cancel)?;
    let dom = built.model.dom_version.clone();
    let notes: BTreeMap<&str, &StoryInput> = longdoc.stories.iter().map(|s| (s.story.story_id.as_str(), s)).collect();
    let note = |id: &str| notes.get(id).copied().ok_or(IdmlError::DanglingStory);

    // Inline objects per host IDML story, and the endnote ranges.
    let mut w = Writer { names: input.names, loss: &mut built.loss, swatches: BTreeMap::new(), swatch_xml: String::new() };
    let story_ids = &built.story_ids;
    let host = |id: &str| story_ids.get(id).cloned().ok_or(IdmlError::DanglingStory);
    let mut inline: BTreeMap<String, Vec<(u64, Opaque)>> = BTreeMap::new();
    for t in &r.tables {
        canceled(cancel)?;
        let h = host(&t.story_id)?;
        let cells = match &t.cell_story_id {
            Some(id) => {
                let s = note(id)?;
                Some((s.story, story_text(s)?))
            }
            None => None,
        };
        let xml = table_xml(t, cells, &mut w, &format!("Stories/Story_{h}.xml"))?;
        inline.entry(h).or_default().push((t.anchor, opaque("Table", xml)));
    }
    for n in &r.footnotes {
        let h = host(&n.story_id)?;
        let xml = note_xml("Footnote", &n.note_id, "", note(&n.note_story_id)?, &mut w, &format!("Stories/Story_{h}.xml"))?;
        inline.entry(h).or_default().push((n.anchor, opaque("Footnote", xml)));
    }
    let mut endnote_ranges = Vec::new();
    for n in &r.endnotes {
        let h = host(&n.story_id)?;
        let id = esc(&n.note_id, true);
        inline.entry(h).or_default().push((n.anchor, opaque("Endnote", format!("<Endnote Self=\"{id}\"/>"))));
        let xml = note_xml("EndnoteRange", &format!("{}r", n.note_id), &format!(" SourceEndnote=\"{id}\""), note(&n.note_story_id)?, &mut w, "endnotes")?;
        endnote_ranges.push(opaque("EndnoteRange", xml));
    }
    let Writer { swatches, swatch_xml, .. } = w;
    let mut stories = std::mem::take(&mut built.model.stories);
    for st in &mut stories {
        if let Some(items) = inline.remove(&st.id) {
            insert_inline(st, items, &mut built.loss)?;
        }
    }
    if !endnote_ranges.is_empty() {
        let id = format!("ua{:x}", stories.len() + 1);
        let n = endnote_ranges.len();
        let paragraphs = endnote_ranges
            .into_iter()
            .enumerate()
            .map(|(i, obj)| {
                let mut runs = vec![object_run(obj)];
                if i + 1 < n {
                    runs.push(RunModel { br: true, inline: None, ..object_run(opaque("Br", String::new())) });
                }
                ParaModel { style: NO_PARAGRAPH_STYLE.into(), attrs: Arc::new(Attrs::new()), props: None, runs }
            })
            .collect();
        built.model.designmap.push(DesignmapItem::Story(stories.len()));
        stories.push(StoryModel {
            id: id.clone(),
            part: format!("Stories/Story_{id}.xml"),
            part_sha256: [0; 32],
            attrs: vec![("IsEndnoteStory".into(), "true".into())],
            head: vec![opaque("StoryPreference", "<StoryPreference StoryDirection=\"LeftToRightDirection\"/>".into())],
            right_to_left: false,
            paragraphs,
            composition: Vec::new(),
        });
    }
    built.model.stories = stories;

    let mut w = Writer { names: input.names, loss: &mut built.loss, swatches, swatch_xml };
    // Graphic frames on their spreads.
    for (n, g) in r.graphic_frames.iter().enumerate() {
        canceled(cancel)?;
        let (_, si, at) = built.pages.get(&g.page_id).cloned().ok_or(IdmlError::Folio(Code::DanglingReference))?;
        let name = d.layers.iter().find(|l| l.layer_id == g.layer_id).map_or("Graphic frame", |l| l.name.as_str());
        let part = built.model.spreads[si].part.clone();
        let item = graphic_xml(g, name, n + 1, at, &mut w, &part)?;
        built.model.spreads[si].items.push(SpreadItem::Opaque(item));
    }
    // Designmap: sections, text variables, resource parts; preferences.
    let mut items = Vec::new();
    for s in &r.sections {
        let page = built.pages.get(&s.page_start).map(|p| p.0.clone()).ok_or(IdmlError::Folio(Code::DanglingReference))?;
        let mut x = format!(
            "<Section Self=\"{}\" Name=\"{}\" PageStart=\"{page}\" ContinueNumbering=\"{}\" IncludeSectionPrefix=\"{}\" PageNumberStyle=\"{}\"",
            esc(&s.section_id, true),
            esc(&s.name, true),
            s.continue_numbering,
            s.include_section_prefix,
            to(&NUMBER_STYLES, s.page_number_style)
        );
        if let Some(n) = s.page_number_start {
            let _ = write!(x, " PageNumberStart=\"{n}\"");
        }
        if let Some(p) = &s.section_prefix {
            let _ = write!(x, " SectionPrefix=\"{}\"", esc(p, true));
        }
        if let Some(m) = &s.marker {
            let _ = write!(x, " Marker=\"{}\"", esc(m, true));
        }
        x.push_str("/>");
        items.push(DesignmapItem::Opaque(opaque("Section", x)));
    }
    let mut chapters = BTreeSet::new();
    for v in &r.text_variables {
        if let StudioVariableKind::ChapterNumber { number, .. } = &v.kind {
            chapters.insert(*number);
        }
        items.push(DesignmapItem::Opaque(opaque("TextVariable", variable_xml(v, &mut w))));
    }
    let mut prefs = String::new();
    if chapters.len() > 1 {
        w.loss.note(PREFERENCES_PART, "ChapterNumberPreference", LossKind::Approximated, "chapter_numbers_collapsed_to_first");
    }
    if let Some(n) = chapters.first() {
        let _ = writeln!(prefs, "<ChapterNumberPreference ChapterNumber=\"{n}\"/>");
    }
    if let Some(o) = &r.footnote_options {
        let _ = write!(
            prefs,
            "<FootnoteOption FootnoteNumberingStyle=\"{}\" StartAt=\"{}\" RestartNumbering=\"{}\" Prefix=\"{}\" Suffix=\"{}\" SpaceBetween=\"{}\" Spacer=\"{}\" EosPlacement=\"{}\" NoSplitting=\"{}\"",
            to(&NUMBER_STYLES, o.numbering_style),
            o.start_at,
            to(&RESTARTS, o.restart),
            esc(&o.prefix, true),
            esc(&o.suffix, true),
            num(o.space_between.value),
            num(o.spacer.value),
            o.eos_placement,
            o.no_splitting
        );
        rule_xml(&mut prefs, "Rule", o.rule.as_ref(), &mut w);
        rule_xml(&mut prefs, "ContinuingRule", o.continuing_rule.as_ref(), &mut w);
        prefs.push_str("/>\n");
    }
    if let Some(o) = &r.endnote_options {
        let title_style = w.style(true, o.title_style.as_deref(), PREFERENCES_PART, "EndnoteOption");
        let text_style = w.style(true, o.text_style.as_deref(), PREFERENCES_PART, "EndnoteOption");
        let _ = writeln!(
            prefs,
            "<EndnoteOption EndnoteTitle=\"{}\" EndnoteTitleStyle=\"{}\" EndnoteParagraphStyle=\"{}\" EndnoteNumberingStyle=\"{}\" StartEndnoteNumberAt=\"{}\" RestartEndnoteNumbering=\"{}\" ScopeValue=\"{}\" EndnotePrefix=\"{}\" EndnoteSuffix=\"{}\" EndnoteSeparatorText=\"{}\"/>",
            esc(&o.title, true),
            esc(&title_style, true),
            esc(&text_style, true),
            to(&NUMBER_STYLES, o.numbering_style),
            o.start_at,
            to(&ENDNOTE_RESTARTS, o.restart),
            to(&ENDNOTE_SCOPES, o.scope),
            esc(&o.prefix, true),
            esc(&o.suffix, true),
            esc(&o.separator_text, true)
        );
    }
    let mut parts = Vec::new();
    if !r.toc_styles.is_empty() {
        let mut body = String::from("<RootTOCStyleGroup Self=\"uts1\">\n");
        for t in &r.toc_styles {
            body.push_str(&toc_xml(t, &mut w));
            body.push('\n');
        }
        body.push_str("</RootTOCStyleGroup>\n");
        parts.push(IdmlPart::new(STYLES_PART.into(), wrapper("Styles", &dom, &body)));
        items.push(DesignmapItem::Opaque(opaque("Styles", format!("<idPkg:Styles src=\"{STYLES_PART}\"/>"))));
    }
    if !w.swatch_xml.is_empty() {
        parts.push(IdmlPart::new(GRAPHIC_PART.into(), wrapper("Graphic", &dom, &w.swatch_xml)));
        items.push(DesignmapItem::Opaque(opaque("Graphic", format!("<idPkg:Graphic src=\"{GRAPHIC_PART}\"/>"))));
    }
    for (absent, element, code) in [
        (r.index_marks.is_empty() && r.index_options.is_none(), "index", "index_not_exported"),
        (r.running_texts.is_empty(), "running_text", "running_text_not_exported"),
        (r.sidenotes.is_empty() && r.sidenote_options.is_none(), "sidenote", "sidenote_not_exported"),
    ] {
        if !absent {
            w.loss.note("document", element, LossKind::Unsupported, code);
        }
    }
    // After the preferences reference (designmap item 0), before spreads and stories.
    let at = built.model.designmap.len().min(1);
    built.model.designmap.splice(at..at, items);
    finish(built, &prefs, parts, cancel)
}
