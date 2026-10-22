//! IDML paragraph composition -> Folio type-style records (lane SMB-LAYOUT-COMPOSER-WIRING).
//!
//! [`import_type_styles`] reads `Resources/Styles.xml` of an [`IdmlImport`] and the
//! `ParagraphStyleRange` attributes of its stories, and returns Folio `StudioTypeStyleRecords`
//! for the proposal's document: one paragraph `StudioTypeStyle` per applied paragraph style and per
//! `BasedOn` ancestor (ancestors no story applies get freshly minted `STYS` ids, listed in
//! `new_styles`), the composition settings each `ParagraphStyle` element states
//! (`style_composition`), and the composition attributes a `ParagraphStyleRange` sets locally
//! (`paragraph_composition`, STU-TYP-201 step 4). hsk-studio-layout composes from these records
//! (`TypesetOptions::styles`); [`crate::reflow_import_typeset`] passes them to the IDML reflow.
//!
//! Attribute names and values, verified on the InDesign-written oracle packages
//! (`oracle-corpus/adobe-session/indesign/ID-03__*.idml`, InDesign 21.6: `[No paragraph style]`
//! carries every attribute, `NormalParagraphStyle` has `Properties/BasedOn` = `$ID/[No paragraph
//! style]`, local `ParagraphStyleRange@Composer="HL Single"` / `@Hyphenation="false"`):
//! `Composer` (`HL Composer` = Adobe Paragraph Composer -> `every_line`, `HL Single` = Adobe
//! Single-line Composer -> `single_line`, both `latin_cjk`), `Hyphenation`,
//! `HyphenateWordsLongerThan`, `HyphenateAfterFirst`, `HyphenateBeforeLast`,
//! `HyphenateLadderLimit`, `HyphenWeight`, `HyphenationZone` (points), `HyphenateCapitalizedWords`,
//! `HyphenateLastWord`, `HyphenateAcrossColumns`, `Minimum/Desired/MaximumWordSpacing`,
//! `Minimum/Desired/MaximumLetterSpacing`, `Minimum/Desired/MaximumGlyphScaling`, `AutoLeading`
//! and `SingleWordJustification` (`FullyJustified` seen). UNVERIFIED (no oracle file shows them):
//! the `SingleWordJustification` values `LeftAlign`, `CenterAlign`, `RightAlign` (the
//! `Justification` enumeration's names), the World-Ready / Japanese composer values (not mapped:
//! typed loss), and `BasedOn` written as `ParagraphStyle/...` (accepted in both forms).
//!
//! Every value that does not map is a typed loss and the field stays unset (it then inherits),
//! never a silent default: an unknown composer (`composer_not_mapped`), a value outside the
//! STU-TYP-147/148/150 hard bounds or a non-integral / negative count
//! (`composition_value_out_of_range`), an unparseable boolean, number or enumeration
//! (`composition_value_invalid`), a minimum / desired / maximum triple out of order in one element
//! (`composition_spacing_order`, the triple dropped). Values are read from decoded attributes
//! (roxmltree resolves character references; DTDs and entities are refused by the package parser),
//! never by substring matching; enumeration values must match exactly, numbers are trimmed.
//! Paragraphs set in `[No paragraph style]` have no Folio style, so that style's settings are
//! stored with their local attributes as a paragraph override (`root_style_settings_as_paragraph_override`,
//! approximated). Style attributes other than composition (fonts, size, alignment, indents, ...)
//! are not mapped here (`paragraph_style_attributes_not_mapped`). The result is validated by
//! Folio `validate_type_style_records`.
#![deny(clippy::arithmetic_side_effects)]
use crate::idml_decode::{LossBook, canceled, parse_xml, text_of};
use crate::model::*;
use crate::proposal::{Known, folio_err};
use crate::{IdMint, IdmlError, Result};
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{
    AUTO_LEADING_PERCENT, GLYPH_SCALING_PERCENT, GeometryUnit, LETTER_SPACING_PERCENT, Length, MAX_HYPHEN_WEIGHT,
    MAX_PARAGRAPH_COMPOSITION, MAX_TYPE_STYLES, StudioCompositionEngine, StudioCompositionScope, StudioCompositionSettings,
    StudioParagraphComposition, StudioSingleWordJustification, StudioStyleComposition, StudioTypeStyle, StudioTypeStyleRecords,
    TypeStyleSchema, TypeStyleScope, WORD_SPACING_PERCENT, validate_type_style_records,
};
use roxmltree::Node;
use std::collections::{BTreeMap, BTreeSet};

const STYLES_PART: &str = "Resources/Styles.xml";
const STYLE_PREFIX: &str = "ParagraphStyle/";
const ROOT_STYLE: &str = "ParagraphStyle/$ID/[No paragraph style]";
/// Folio name bytes kept from an IDML style name (longer names are replaced, typed loss).
const MAX_NAME_BYTES: usize = 256;

/// Result of [`import_type_styles`]. Nothing is committed.
#[derive(Clone, Debug)]
pub struct TypeStyleImport {
    pub records: StudioTypeStyleRecords,
    /// Paragraph styles minted here (ancestors no story applies), with their IDML names.
    pub new_styles: Vec<StyleRef>,
    pub loss: Vec<LossEntry>,
}

/// The IDML attribute names this module maps (others are left to `paragraph_style_attributes_not_mapped`).
pub const COMPOSITION_ATTRIBUTES: [&str; 22] = [
    "Composer",
    "Hyphenation",
    "HyphenateWordsLongerThan",
    "HyphenateAfterFirst",
    "HyphenateBeforeLast",
    "HyphenateLadderLimit",
    "HyphenWeight",
    "HyphenationZone",
    "HyphenateCapitalizedWords",
    "HyphenateLastWord",
    "HyphenateAcrossColumns",
    "MinimumWordSpacing",
    "DesiredWordSpacing",
    "MaximumWordSpacing",
    "MinimumLetterSpacing",
    "DesiredLetterSpacing",
    "MaximumLetterSpacing",
    "MinimumGlyphScaling",
    "DesiredGlyphScaling",
    "MaximumGlyphScaling",
    "AutoLeading",
    "SingleWordJustification",
];
/// Attributes of a `ParagraphStyle` element that are identity or structure, not formatting.
const STRUCTURAL: [&str; 6] = ["Self", "Name", "Imported", "NextStyle", "StyleUniqueId", "KeyboardShortcut"];

fn el_attr<'a>(n: Node<'a, '_>, k: &str) -> Option<&'a str> {
    n.attributes().find(|a| a.name() == k && a.namespace().is_none()).map(|a| a.value())
}
/// `Properties/<name>` text (IDML writes `BasedOn` there).
fn prop_text<'a>(n: Node<'a, '_>, k: &str) -> Option<&'a str> {
    let props = n.children().find(|c| c.is_element() && c.tag_name().name() == "Properties")?;
    props.children().find(|c| c.is_element() && c.tag_name().name() == k)?.text().map(str::trim)
}
fn style_key(v: &str) -> String {
    if v.starts_with(STYLE_PREFIX) { v.to_owned() } else { format!("{STYLE_PREFIX}{v}") }
}
/// A Folio slash-path name (no control character, no empty segment) of bounded length.
fn folio_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_NAME_BYTES && !s.chars().any(char::is_control) && s.split('/').all(|seg| !seg.trim().is_empty())
}

/// Parse context for one element's attributes.
struct Reader<'a> {
    part: &'a str,
    element: &'a str,
    loss: &'a mut LossBook,
}
impl Reader<'_> {
    fn lose(&mut self, attr: &str, kind: LossKind, code: &'static str) {
        let element = format!("{}@{attr}", self.element);
        self.loss.note(self.part, &element, kind, code);
    }
    fn boolean(&mut self, attr: &str, v: &str) -> Option<bool> {
        match v {
            "true" => Some(true),
            "false" => Some(false),
            _ => {
                self.lose(attr, LossKind::Unsupported, "composition_value_invalid");
                None
            }
        }
    }
    fn number(&mut self, attr: &str, v: &str) -> Option<f64> {
        match v.trim().parse::<f64>() {
            Ok(x) if x.is_finite() => Some(x),
            _ => {
                self.lose(attr, LossKind::Unsupported, "composition_value_invalid");
                None
            }
        }
    }
    fn percent(&mut self, attr: &str, v: &str, (lo, hi): (f64, f64)) -> Option<f64> {
        let x = self.number(attr, v)?;
        if (lo..=hi).contains(&x) {
            Some(x)
        } else {
            self.lose(attr, LossKind::Unsupported, "composition_value_out_of_range");
            None
        }
    }
    fn count(&mut self, attr: &str, v: &str, max: u32) -> Option<u32> {
        let x = self.number(attr, v)?;
        if x >= 0.0 && x <= f64::from(max) && x.fract() == 0.0 {
            // In range and integral: the cast is exact.
            Some(x as u32)
        } else {
            self.lose(attr, LossKind::Unsupported, "composition_value_out_of_range");
            None
        }
    }
}

/// The composition settings `attrs` state (IDML attribute name, decoded value). Unknown attribute
/// names are not composition and are ignored here.
fn settings_of<'v>(attrs: impl Iterator<Item = (&'v str, &'v str)>, r: &mut Reader<'_>) -> StudioCompositionSettings {
    let mut s = StudioCompositionSettings::default();
    for (k, v) in attrs {
        match k {
            "Composer" => match v.strip_prefix("$ID/").unwrap_or(v) {
                "HL Composer" => {
                    s.composition_scope = Some(StudioCompositionScope::EveryLine);
                    s.composition_engine = Some(StudioCompositionEngine::LatinCjk);
                }
                "HL Single" => {
                    s.composition_scope = Some(StudioCompositionScope::SingleLine);
                    s.composition_engine = Some(StudioCompositionEngine::LatinCjk);
                }
                _ => r.lose(k, LossKind::Unsupported, "composer_not_mapped"),
            },
            "Hyphenation" => s.hyphenation = r.boolean(k, v),
            "HyphenateCapitalizedWords" => s.hyphenate_capitalized_words = r.boolean(k, v),
            "HyphenateLastWord" => s.hyphenate_last_word = r.boolean(k, v),
            "HyphenateAcrossColumns" => s.hyphenate_across_columns = r.boolean(k, v),
            "HyphenateWordsLongerThan" => s.hyphenate_words_longer_than = r.count(k, v, u32::MAX),
            "HyphenateAfterFirst" => s.hyphenate_after_first = r.count(k, v, u32::MAX),
            "HyphenateBeforeLast" => s.hyphenate_before_last = r.count(k, v, u32::MAX),
            "HyphenateLadderLimit" => s.hyphenate_ladder_limit = r.count(k, v, u32::MAX),
            "HyphenWeight" => s.hyphen_weight = r.count(k, v, MAX_HYPHEN_WEIGHT),
            "HyphenationZone" => {
                s.hyphenation_zone = r.percent(k, v, (0.0, f64::MAX)).map(|value| Length { value, unit: GeometryUnit::Pt });
            }
            "MinimumWordSpacing" => s.word_spacing_minimum = r.percent(k, v, WORD_SPACING_PERCENT),
            "DesiredWordSpacing" => s.word_spacing_desired = r.percent(k, v, WORD_SPACING_PERCENT),
            "MaximumWordSpacing" => s.word_spacing_maximum = r.percent(k, v, WORD_SPACING_PERCENT),
            "MinimumLetterSpacing" => s.letter_spacing_minimum = r.percent(k, v, LETTER_SPACING_PERCENT),
            "DesiredLetterSpacing" => s.letter_spacing_desired = r.percent(k, v, LETTER_SPACING_PERCENT),
            "MaximumLetterSpacing" => s.letter_spacing_maximum = r.percent(k, v, LETTER_SPACING_PERCENT),
            "MinimumGlyphScaling" => s.glyph_scaling_minimum = r.percent(k, v, GLYPH_SCALING_PERCENT),
            "DesiredGlyphScaling" => s.glyph_scaling_desired = r.percent(k, v, GLYPH_SCALING_PERCENT),
            "MaximumGlyphScaling" => s.glyph_scaling_maximum = r.percent(k, v, GLYPH_SCALING_PERCENT),
            "AutoLeading" => s.auto_leading_amount = r.percent(k, v, AUTO_LEADING_PERCENT),
            "SingleWordJustification" => {
                s.single_word_justification = match v {
                    "LeftAlign" => Some(StudioSingleWordJustification::LeftAlign),
                    "CenterAlign" => Some(StudioSingleWordJustification::CenterAlign),
                    "RightAlign" => Some(StudioSingleWordJustification::RightAlign),
                    "FullyJustified" => Some(StudioSingleWordJustification::FullyJustified),
                    _ => {
                        r.lose(k, LossKind::Unsupported, "composition_value_invalid");
                        None
                    }
                }
            }
            _ => {}
        }
    }
    // A triple out of order inside one element is dropped whole (typed loss).
    let ordered = |a: Option<f64>, b: Option<f64>, c: Option<f64>| {
        let le = |x: Option<f64>, y: Option<f64>| x.zip(y).is_none_or(|(x, y)| x <= y);
        le(a, b) && le(b, c) && le(a, c)
    };
    if !ordered(s.word_spacing_minimum, s.word_spacing_desired, s.word_spacing_maximum) {
        (s.word_spacing_minimum, s.word_spacing_desired, s.word_spacing_maximum) = (None, None, None);
        r.lose("WordSpacing", LossKind::Unsupported, "composition_spacing_order");
    }
    if !ordered(s.letter_spacing_minimum, s.letter_spacing_desired, s.letter_spacing_maximum) {
        (s.letter_spacing_minimum, s.letter_spacing_desired, s.letter_spacing_maximum) = (None, None, None);
        r.lose("LetterSpacing", LossKind::Unsupported, "composition_spacing_order");
    }
    if !ordered(s.glyph_scaling_minimum, s.glyph_scaling_desired, s.glyph_scaling_maximum) {
        (s.glyph_scaling_minimum, s.glyph_scaling_desired, s.glyph_scaling_maximum) = (None, None, None);
        r.lose("GlyphScaling", LossKind::Unsupported, "composition_spacing_order");
    }
    s
}

/// Maps the paragraph composition of `import` into Folio type-style records (module docs).
pub fn import_type_styles(
    import: &IdmlImport,
    limits: &Limits,
    mint: &mut dyn IdMint,
    cancel: &CancellationToken,
) -> Result<TypeStyleImport> {
    canceled(cancel)?;
    let document = import.proposal.snapshot.document();
    let mut loss = LossBook::default();
    let mut records = StudioTypeStyleRecords::new(document.document_id.clone());
    let applied: BTreeMap<&str, &str> =
        import.proposal.style_refs.iter().filter(|s| s.paragraph).map(|s| (s.idml_name.as_str(), s.folio.as_str())).collect();

    // `ParagraphStyle` elements of Styles.xml by `Self`.
    let text = match import.package.part(STYLES_PART) {
        Some(part) => Some(text_of(part)?),
        None => None,
    };
    let parsed = text.map(|t| parse_xml(t, limits)).transpose()?;
    let mut defs: BTreeMap<&str, Node> = BTreeMap::new();
    if let Some(doc) = &parsed {
        for n in doc.root().descendants().filter(|n| n.is_element() && n.tag_name().name() == "ParagraphStyle") {
            canceled(cancel)?;
            let Some(key) = el_attr(n, "Self") else {
                loss.note(STYLES_PART, "ParagraphStyle@Self", LossKind::Unsupported, "composition_value_invalid");
                continue;
            };
            if defs.len() >= MAX_TYPE_STYLES {
                return Err(IdmlError::BudgetExceeded("paragraph_styles"));
            }
            if defs.insert(key, n).is_some() {
                loss.note(STYLES_PART, "ParagraphStyle@Self", LossKind::Unsupported, "duplicate_style_definition");
            }
        }
    } else if !applied.is_empty() {
        loss.note(STYLES_PART, "ParagraphStyle", LossKind::Unsupported, "styles_part_missing");
    }

    // Applied styles and their `BasedOn` ancestors, ancestors minted on first sight.
    let mut ids: BTreeMap<String, String> = applied.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    let mut new_styles = Vec::new();
    let mut parent_of: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut pending: Vec<String> = applied.keys().map(|k| (*k).to_owned()).collect();
    while let Some(key) = pending.pop() {
        canceled(cancel)?;
        if parent_of.contains_key(&key) {
            continue;
        }
        let parent = defs
            .get(key.as_str())
            .and_then(|n| prop_text(*n, "BasedOn").or_else(|| el_attr(*n, "BasedOn")))
            .filter(|b| !b.is_empty())
            .map(style_key);
        let parent = match parent {
            Some(p) if defs.contains_key(p.as_str()) && p != key => {
                if !ids.contains_key(&p) {
                    let id = mint.mint("STYS");
                    new_styles.push(StyleRef { folio: id.clone(), idml_name: p.clone(), paragraph: true });
                    ids.insert(p.clone(), id);
                    pending.push(p.clone());
                }
                Some(p)
            }
            Some(_) => {
                loss.note(STYLES_PART, "ParagraphStyle@BasedOn", LossKind::Unsupported, "based_on_missing");
                None
            }
            None => None,
        };
        parent_of.insert(key, parent);
    }
    // Break `BasedOn` cycles (typed loss): walk each chain, bounded by the set size.
    let keys: Vec<String> = parent_of.keys().cloned().collect();
    for start in &keys {
        let mut seen = BTreeSet::new();
        let mut at = start.clone();
        while let Some(Some(p)) = parent_of.get(&at) {
            if !seen.insert(at.clone()) || seen.len() > keys.len() {
                break;
            }
            if p == start {
                loss.note(STYLES_PART, "ParagraphStyle@BasedOn", LossKind::Unsupported, "based_on_cycle");
                parent_of.insert(at.clone(), None);
                break;
            }
            at = p.clone();
        }
    }

    // Style records and their composition settings, in IDML name order (deterministic).
    for (key, parent) in &parent_of {
        canceled(cancel)?;
        let folio = ids[key].clone();
        let node = defs.get(key.as_str());
        let raw_name = node.and_then(|n| el_attr(*n, "Name")).unwrap_or_else(|| key.strip_prefix(STYLE_PREFIX).unwrap_or(key));
        let name = if folio_name(raw_name) {
            raw_name.to_owned()
        } else {
            loss.note(STYLES_PART, "ParagraphStyle@Name", LossKind::Approximated, "style_name_replaced");
            format!("idml/paragraph/{}", records.styles.len())
        };
        records.styles.push(StudioTypeStyle {
            schema_id: TypeStyleSchema::V1,
            style_id: folio.clone(),
            scope: TypeStyleScope::Paragraph,
            name,
            based_on: parent.as_ref().map(|p| ids[p].clone()),
            next_style: None,
            font_family: None,
            font_style: None,
            font_size: None,
            fill: None,
            line_height: None,
            letter_spacing: None,
            align: None,
            grep_styles: Vec::new(),
        });
        let Some(n) = node else {
            loss.note(STYLES_PART, "ParagraphStyle", LossKind::Unsupported, "style_definition_missing");
            continue;
        };
        let attrs = n.attributes().filter(|a| a.namespace().is_none()).map(|a| (a.name(), a.value()));
        let settings = settings_of(attrs, &mut Reader { part: STYLES_PART, element: "ParagraphStyle", loss: &mut loss });
        if n.attributes().any(|a| !STRUCTURAL.contains(&a.name()) && !COMPOSITION_ATTRIBUTES.contains(&a.name())) {
            loss.note(STYLES_PART, "ParagraphStyle", LossKind::Unsupported, "paragraph_style_attributes_not_mapped");
        }
        if !settings.is_empty() {
            records.style_composition.push(StudioStyleComposition { style_id: folio, settings });
        }
    }

    // Local paragraph attributes, at the proposal's paragraph start bytes (proposal text rule: run
    // text plus one newline per `Br`; paragraphs adding no text yield no Folio paragraph).
    let root_settings = defs.get(ROOT_STYLE).map(|n| {
        let attrs = n.attributes().filter(|a| a.namespace().is_none()).map(|a| (a.name(), a.value()));
        // Losses of the root's own values were already noted when (if) it was mapped as a style.
        let mut scratch = LossBook::default();
        settings_of(attrs, &mut Reader { part: STYLES_PART, element: "ParagraphStyle", loss: &mut scratch })
    });
    for st in &import.model.stories {
        canceled(cancel)?;
        let Some(link) = import.proposal.ids.iter().find(|l| l.kind == "story" && l.idml == st.id) else { continue };
        let Some(story) = import.proposal.stories.iter().find(|p| p.story.story_id == link.folio) else { continue };
        let mut at = 0u64;
        for para in &st.paragraphs {
            let start = at;
            for run in &para.runs {
                at = at.saturating_add(run.text.len() as u64).saturating_add(u64::from(run.br));
            }
            if at == start {
                continue;
            }
            let element = "ParagraphStyleRange";
            let local = settings_of(para.attrs.iter().map(|(k, v)| (k.as_str(), v.as_str())), &mut Reader { part: &st.part, element, loss: &mut loss });
            let unstyled = para.style.is_empty() || para.style.contains("[No paragraph style]");
            let base = if unstyled { root_settings.clone().unwrap_or_default() } else { StudioCompositionSettings::default() };
            if unstyled && !base.is_empty() {
                loss.note(&st.part, element, LossKind::Approximated, "root_style_settings_as_paragraph_override");
            }
            let settings = base.overlaid(&local);
            if settings.is_empty() {
                continue;
            }
            if !story.story.paragraphs.iter().any(|p| p.start == start) {
                loss.note(&st.part, element, LossKind::Unsupported, "paragraph_not_in_proposal");
                continue;
            }
            if records.paragraph_composition.len() >= MAX_PARAGRAPH_COMPOSITION {
                return Err(IdmlError::BudgetExceeded("paragraph_composition"));
            }
            records.paragraph_composition.push(StudioParagraphComposition { text_id: link.folio.clone(), paragraph_start: start, settings });
        }
    }

    let none = BTreeSet::new();
    validate_type_style_records(&records, document, limits.folio, cancel, &Known(&none)).map_err(folio_err)?;
    Ok(TypeStyleImport { records, new_styles, loss: loss.finish() })
}
