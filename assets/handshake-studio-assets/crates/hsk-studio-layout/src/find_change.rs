//! Find/change command family (lane SMB-ID-PREPRESS; STU-LAY-027, STU-LAY-129), extending the find
//! engine of lane SMB-LAYOUT-UI-2 ([`crate::find`]: `find.text` / `find.grep` with the bounded regex
//! engine of GREP styles, `find_in_story`) with the remaining modes, scopes and saved queries. One
//! command with a mode discriminator ([`FindChangeMode`]) plus the separate document-scoped colour
//! operation; text and GREP searches are delegated to `find_in_story`, nothing is compiled here:
//!
//! - `find.text`: literal text with InDesign-style metacharacters (`^t` tab, `^p` paragraph end,
//!   `^n` forced line break U+2028, `^^` caret, `^?` any character, `^9` any digit, `^$` any letter,
//!   `^w` any white space), case and whole-word toggles. A literal without `^` goes to
//!   `find_in_story` as `FindMode::Text`; a literal with metacharacters is tokenized first (V4-08),
//!   each token an escaped regex atom, and searched as `FindMode::Grep` (the change stays literal:
//!   `$` is escaped); an unknown `^x` is `find_metacharacter_invalid`.
//! - `find.grep`: `find_in_story` with `FindMode::Grep` (capture groups, `$0`, `$1`, `${name}`;
//!   look-around and back-references `grep_pattern_unsupported`, parity gap UNVERIFIED D5).
//! - `find.glyph`: a character (code point) within a named font: the caller resolves the font of
//!   each character range (`FindContext::font_of`, from its type styles); only ranges in that font
//!   match. Glyph-id search needs the font's cmap and is a declared gap.
//! - `find.transliterate`: characters of one TYPE are changed to another: full-width <-> half-width
//!   Roman (U+FF01..U+FF5E <-> U+0021..U+007E, U+3000 <-> U+0020) and hiragana <-> katakana
//!   (U+3041..U+3096 <-> U+30A1..U+30F6). Other class pairs are `transliteration_unsupported`.
//! - `find.object`: text-frame and other layers by object formatting (kind, applied object style,
//!   column count, vertical justification); the change applies an object style (through
//!   [`crate::apply_object_style`], one operation per layer) and/or sets those frame attributes.
//! - `find.colour` ([`find_colour`] / [`change_colour`]): swatch usages in the Folio vector
//!   records (`swatch_paints`), replaced document-wide.
//!
//! Scope: document (every story some frame shows), story, to end of story, selection (a byte range
//! of a story, or a selected layer: a layer that holds no text is `object_contains_no_text`, a
//! distinct error, not "no matches"); include toggles for hidden layers (stories shown only by
//! hidden frames are skipped otherwise). Changes are computed per story and applied back to front
//! through `replace_story_text`; [`apply_text_changes`] returns the new stories and ONE undo record
//! holding the stories as they were (snapshot undo, like the story editor). Saved queries
//! ([`SavedQueries`]) save, load and delete named queries with the four captured typed errors
//! (invalid query name, query not found, query locked/read-only, invalid search mode). Matches are
//! bounded per call (`find_budget_exceeded`). InDesign parity of all modes is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use crate::LayoutError;
use crate::folio_input::story_text;
use crate::find::{FindMode, MAX_FIND_PATTERN_BYTES, find_in_story};
use crate::grep::GrepError;
use crate::textflow::replace_story_text;
use hsk_studio_folio::{
    Payload, StudioDocument, StudioLayoutFlowRecords, StudioTextStory, StudioVectorRecords, VerticalJustification,
};
use std::collections::BTreeMap;

/// Most matches one find returns.
pub const MAX_FIND_CHANGE_MATCHES: usize = 100_000;
/// Longest find or change string, bytes.
pub const MAX_FIND_CHANGE_QUERY_BYTES: usize = MAX_FIND_PATTERN_BYTES;
/// Most saved queries.
pub const MAX_SAVED_QUERIES: usize = 1024;

/// Typed find/change refusals.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindChangeError {
    Layout(LayoutError),
    QueryEmpty,
    QueryTooLong,
    MetacharacterInvalid(String),
    PatternInvalid,
    PatternUnsupported,
    TransliterationUnsupported,
    /// The selected object holds no text (distinct from "no matches").
    ObjectContainsNoText(String),
    UnknownStory(String),
    BudgetExceeded,
    InvalidQueryName,
    QueryNotFound(String),
    QueryLocked(String),
    InvalidSearchMode(String),
    TooManyQueries,
}
impl FindChangeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Layout(e) => e.code(),
            Self::QueryEmpty => "find_query_empty",
            Self::QueryTooLong => "find_query_too_long",
            Self::MetacharacterInvalid(_) => "find_metacharacter_invalid",
            Self::PatternInvalid => "grep_pattern_invalid",
            Self::PatternUnsupported => "grep_pattern_unsupported",
            Self::TransliterationUnsupported => "transliteration_unsupported",
            Self::ObjectContainsNoText(_) => "object_contains_no_text",
            Self::UnknownStory(_) => "find_unknown_story",
            Self::BudgetExceeded => "find_budget_exceeded",
            Self::InvalidQueryName => "invalid_query_name",
            Self::QueryNotFound(_) => "query_file_not_found",
            Self::QueryLocked(_) => "query_file_locked",
            Self::InvalidSearchMode(_) => "invalid_search_mode",
            Self::TooManyQueries => "too_many_queries",
        }
    }
}
impl From<LayoutError> for FindChangeError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}

/// A character type of `find.transliterate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CharType {
    FullWidthRoman,
    HalfWidthRoman,
    Hiragana,
    Katakana,
    HalfWidthKatakana,
}

/// The mode discriminator (STU-LAY-129).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindChangeMode {
    Text { find: String, case_sensitive: bool, whole_word: bool },
    Grep { find: String, case_sensitive: bool },
    Glyph { font: String, character: char },
    Transliterate { from: CharType, to: CharType },
}
impl FindChangeMode {
    /// The captured mode name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Text { .. } => "find.text",
            Self::Grep { .. } => "find.grep",
            Self::Glyph { .. } => "find.glyph",
            Self::Transliterate { .. } => "find.transliterate",
        }
    }
}

/// Where to search.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FindScope {
    /// Every story shown by a text frame of the document.
    Document,
    Story(String),
    /// From this byte to the end of the story.
    ToEndOfStory(String, u64),
    /// A byte range of a story.
    Selection(String, u64, u64),
    /// The text of a selected layer (its story).
    SelectedObject(String),
}

/// Context the modes need beyond the stories.
pub struct FindContext<'a> {
    pub document: &'a StudioDocument,
    pub stories: &'a [StudioTextStory],
    /// Font name of a character range's style (`find.glyph`); `None` = unknown font.
    pub font_of: &'a dyn Fn(Option<&str>) -> Option<String>,
    pub include_hidden_layers: bool,
}

/// One hit: story, bytes, and the replacement when a change was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindChangeHit {
    pub story_id: String,
    pub start: u64,
    pub end: u64,
    pub replacement: Option<String>,
}

/// Snapshot undo of one change operation: the stories as they were.
#[derive(Clone, Debug, PartialEq)]
pub struct ChangeUndo {
    pub stories_before: Vec<StudioTextStory>,
}

/// Tokenizes a `find.text` literal into a regex source (module docs).
fn text_source(find: &str, whole_word: bool) -> Result<String, FindChangeError> {
    let mut out = String::with_capacity(find.len().saturating_mul(2));
    let mut chars = find.chars();
    while let Some(c) = chars.next() {
        if c != '^' {
            out.push_str(&regex::escape(c.encode_utf8(&mut [0u8; 4])));
            continue;
        }
        let atom = match chars.next() {
            Some('t') => r"\t",
            Some('p') => r"\n",
            Some('n') => r"\x{2028}",
            Some('^') => r"\^",
            Some('?') => r"(?s:.)",
            Some('9') => r"\d",
            Some('$') => r"\p{L}",
            Some('w') => r"\s",
            Some(other) => return Err(FindChangeError::MetacharacterInvalid(format!("^{other}"))),
            None => return Err(FindChangeError::MetacharacterInvalid("^".into())),
        };
        out.push_str(atom);
    }
    Ok(if whole_word { format!(r"\b(?:{out})\b") } else { out })
}

/// A `find.text` change string: `^t ^p ^n ^^` expanded, any other `^x` refused.
fn text_change_literal(change: &str) -> Result<String, FindChangeError> {
    let mut out = String::with_capacity(change.len());
    let mut chars = change.chars();
    while let Some(c) = chars.next() {
        if c != '^' {
            out.push(c);
            continue;
        }
        out.push(match chars.next() {
            Some('t') => '\t',
            Some('p') => '\n',
            Some('n') => '\u{2028}',
            Some('^') => '^',
            Some(other) => return Err(FindChangeError::MetacharacterInvalid(format!("^{other}"))),
            None => return Err(FindChangeError::MetacharacterInvalid("^".into())),
        });
    }
    Ok(out)
}

fn grep_err(e: GrepError) -> FindChangeError {
    match e {
        GrepError::Layout(l) => FindChangeError::Layout(l),
        GrepError::PatternUnsupported(..) => FindChangeError::PatternUnsupported,
        GrepError::BudgetExceeded => FindChangeError::BudgetExceeded,
        GrepError::PatternInvalid(..) | GrepError::StyleMissing(_) => FindChangeError::PatternInvalid,
    }
}

fn check_len(s: &str, empty_ok: bool) -> Result<(), FindChangeError> {
    if s.is_empty() && !empty_ok {
        return Err(FindChangeError::QueryEmpty);
    }
    if s.len() > MAX_FIND_CHANGE_QUERY_BYTES {
        return Err(FindChangeError::QueryTooLong);
    }
    Ok(())
}

/// Transliteration of one character, `None` when it is not of type `from`.
fn transliterate(c: char, from: CharType, to: CharType) -> Result<Option<char>, FindChangeError> {
    let u = u32::from(c);
    let shifted = |delta: i64| -> Option<char> { i64::from(u).checked_add(delta).and_then(|v| u32::try_from(v).ok()).and_then(char::from_u32) };
    Ok(match (from, to) {
        (CharType::FullWidthRoman, CharType::HalfWidthRoman) => match u {
            0x3000 => Some(' '),
            0xFF01..=0xFF5E => shifted(-0xFEE0),
            _ => None,
        },
        (CharType::HalfWidthRoman, CharType::FullWidthRoman) => match u {
            0x20 => Some('\u{3000}'),
            0x21..=0x7E => shifted(0xFEE0),
            _ => None,
        },
        (CharType::Hiragana, CharType::Katakana) => match u {
            0x3041..=0x3096 => shifted(0x60),
            _ => None,
        },
        (CharType::Katakana, CharType::Hiragana) => match u {
            0x30A1..=0x30F6 => shifted(-0x60),
            _ => None,
        },
        (a, b) if a == b => None,
        _ => return Err(FindChangeError::TransliterationUnsupported),
    })
}

/// A story in scope with the byte range to search (`None` = to the end).
type Target<'s> = (&'s StudioTextStory, usize, Option<usize>);

/// Stories in scope.
fn targets<'s>(ctx: &FindContext<'s>, scope: &FindScope) -> Result<Vec<Target<'s>>, FindChangeError> {
    let story = |id: &str| ctx.stories.iter().find(|s| s.story_id == id).ok_or_else(|| FindChangeError::UnknownStory(id.into()));
    let byte = |v: u64| usize::try_from(v).map_err(|_| FindChangeError::Layout(LayoutError::InvalidInput));
    Ok(match scope {
        FindScope::Document => ctx
            .stories
            .iter()
            .filter(|s| {
                ctx.document.layers.iter().any(|l| {
                    l.text_frame.is_some()
                        && (l.visible || ctx.include_hidden_layers)
                        && matches!(&l.payload, Payload::Text { story_id } if *story_id == s.story_id)
                })
            })
            .map(|s| (s, 0, None))
            .collect(),
        FindScope::Story(id) => vec![(story(id)?, 0, None)],
        FindScope::ToEndOfStory(id, from) => vec![(story(id)?, byte(*from)?, None)],
        FindScope::Selection(id, a, b) => vec![(story(id)?, byte(*a)?, Some(byte(*b)?))],
        FindScope::SelectedObject(layer_id) => {
            let layer = ctx
                .document
                .layers
                .iter()
                .find(|l| l.layer_id == *layer_id)
                .ok_or(FindChangeError::Layout(LayoutError::InvalidInput))?;
            match &layer.payload {
                Payload::Text { story_id } => vec![(story(story_id)?, 0, None)],
                _ => return Err(FindChangeError::ObjectContainsNoText(layer_id.clone())),
            }
        }
    })
}

/// Every hit of `mode` in `scope` (in story order, then byte order). With `change`, each hit
/// carries its replacement (text: the literal with `^t ^p ^n ^^` expanded; grep: capture
/// expansion; glyph: the change string; transliterate: computed, `change` ignored).
pub fn find_change_text(ctx: &FindContext<'_>, mode: &FindChangeMode, scope: &FindScope, change: Option<&str>) -> Result<Vec<FindChangeHit>, FindChangeError> {
    if let Some(c) = change {
        check_len(c, true)?;
    }
    // Text / GREP: the pattern, the find-engine mode and the change handed to `find_in_story`.
    let delegated: Option<(String, FindMode, Option<String>)> = match mode {
        FindChangeMode::Text { find, case_sensitive, whole_word } => {
            check_len(find, false)?;
            let literal = change.map(text_change_literal).transpose()?;
            Some(if find.contains('^') {
                (text_source(find, *whole_word)?, FindMode::Grep { case_sensitive: *case_sensitive }, literal.map(|c| c.replace('$', "$$")))
            } else {
                (find.clone(), FindMode::Text { case_sensitive: *case_sensitive, whole_word: *whole_word }, literal)
            })
        }
        FindChangeMode::Grep { find, case_sensitive } => {
            check_len(find, false)?;
            Some((find.clone(), FindMode::Grep { case_sensitive: *case_sensitive }, change.map(str::to_owned)))
        }
        FindChangeMode::Transliterate { from, to } => {
            transliterate('a', *from, *to)?;
            None
        }
        FindChangeMode::Glyph { font, .. } => {
            check_len(font, false)?;
            None
        }
    };
    let mut out = Vec::new();
    let push = |out: &mut Vec<FindChangeHit>, hit: FindChangeHit| -> Result<(), FindChangeError> {
        if out.len() >= MAX_FIND_CHANGE_MATCHES {
            return Err(FindChangeError::BudgetExceeded);
        }
        out.push(hit);
        Ok(())
    };
    for (story, from, to) in targets(ctx, scope)? {
        let text = story_text(story, None)?;
        let to = to.unwrap_or(text.len());
        if from > to || to > text.len() || !text.is_char_boundary(from) || !text.is_char_boundary(to) {
            return Err(LayoutError::InvalidInput.into());
        }
        let hit = |s: usize, e: usize, r: Option<String>| FindChangeHit { story_id: story.story_id.clone(), start: s as u64, end: e as u64, replacement: r };
        match mode {
            FindChangeMode::Text { .. } | FindChangeMode::Grep { .. } => {
                let Some((pattern, fmode, ch)) = &delegated else { continue };
                let hits = find_in_story(story, None, pattern, *fmode, Some((from as u64, to as u64)), ch.as_deref()).map_err(grep_err)?;
                for h in hits {
                    if out.len() >= MAX_FIND_CHANGE_MATCHES {
                        return Err(FindChangeError::BudgetExceeded);
                    }
                    out.push(FindChangeHit { story_id: story.story_id.clone(), start: h.start, end: h.end, replacement: h.replacement });
                }
            }
            FindChangeMode::Glyph { font, character } => {
                for r in &story.character_ranges {
                    if (ctx.font_of)(r.style_ref.as_deref()).as_deref() != Some(font.as_str()) {
                        continue;
                    }
                    let (a, b) = (usize::try_from(r.start).unwrap_or(usize::MAX).max(from), usize::try_from(r.end).unwrap_or(usize::MAX).min(to));
                    if a >= b || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
                        continue;
                    }
                    for (i, c) in text[a..b].char_indices() {
                        if c == *character {
                            let s = a.checked_add(i).ok_or(FindChangeError::BudgetExceeded)?;
                            push(&mut out, hit(s, s.saturating_add(c.len_utf8()), change.map(str::to_owned)))?;
                        }
                    }
                }
            }
            FindChangeMode::Transliterate { from: f, to: t } => {
                for (i, c) in text[from..to].char_indices() {
                    if let Some(n) = transliterate(c, *f, *t)? {
                        let s = from.checked_add(i).ok_or(FindChangeError::BudgetExceeded)?;
                        push(&mut out, hit(s, s.saturating_add(c.len_utf8()), Some(n.to_string())))?;
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Applies the hits that carry a replacement, back to front per story, and returns the changed
/// stories (same order as `stories`, unchanged ones included) and one snapshot undo. Overlapping
/// hits are `invalid_input`.
pub fn apply_text_changes(stories: &[StudioTextStory], hits: &[FindChangeHit]) -> Result<(Vec<StudioTextStory>, ChangeUndo), FindChangeError> {
    let mut out = Vec::with_capacity(stories.len());
    for s in stories {
        let mut mine: Vec<&FindChangeHit> = hits.iter().filter(|h| h.story_id == s.story_id && h.replacement.is_some()).collect();
        mine.sort_by_key(|h| (h.start, h.end));
        if mine.windows(2).any(|w| w[1].start < w[0].end) {
            return Err(LayoutError::InvalidInput.into());
        }
        let mut cur = s.clone();
        for h in mine.iter().rev() {
            cur = replace_story_text(&cur, None, h.start, h.end, h.replacement.as_deref().unwrap_or(""))?;
        }
        out.push(cur);
    }
    Ok((out, ChangeUndo { stories_before: stories.to_vec() }))
}

/// Object formatting criteria of `find.object` (`None` = any).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectCriteria {
    pub kind: Option<String>,
    pub applied_object_style: Option<String>,
    pub text_column_count: Option<u32>,
    pub vertical_justification: Option<VerticalJustification>,
    pub include_hidden_layers: bool,
}

/// What `find.object` changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectChange {
    pub object_style: Option<String>,
    pub text_column_count: Option<u32>,
    pub vertical_justification: Option<VerticalJustification>,
}

/// Layer ids matching `criteria`, in layer order.
pub fn find_objects(document: &StudioDocument, flow: &StudioLayoutFlowRecords, c: &ObjectCriteria) -> Vec<String> {
    document
        .layers
        .iter()
        .filter(|l| l.visible || c.include_hidden_layers)
        .filter(|l| c.kind.as_ref().is_none_or(|k| *k == l.kind))
        .filter(|l| {
            c.applied_object_style.as_ref().is_none_or(|s| {
                flow.applied_object_styles.iter().any(|a| a.layer_id == l.layer_id && a.style_id == *s)
            })
        })
        .filter(|l| c.text_column_count.is_none_or(|n| l.text_frame.as_ref().is_some_and(|f| f.text_column_count == n)))
        .filter(|l| c.vertical_justification.is_none_or(|v| l.text_frame.as_ref().is_some_and(|f| f.vertical_justification == v)))
        .map(|l| l.layer_id.clone())
        .collect()
}

/// Applies `change` to every layer of `layers` (object style first, then the attributes) and
/// returns the new document and flow records; the inputs are the undo snapshot. A frame attribute
/// on a layer without a text frame is `invalid_input`.
pub fn change_objects(
    document: &StudioDocument,
    flow: &StudioLayoutFlowRecords,
    layers: &[String],
    change: &ObjectChange,
) -> Result<(StudioDocument, StudioLayoutFlowRecords), FindChangeError> {
    let (mut doc, mut fl) = (document.clone(), flow.clone());
    for id in layers {
        if let Some(style) = &change.object_style {
            let a = crate::apply_object_style(&doc, &fl, None, None, None, id, style)?;
            doc = a.document;
            fl = a.flow;
        }
        if change.text_column_count.is_some() || change.vertical_justification.is_some() {
            let f = doc
                .layers
                .iter_mut()
                .find(|l| l.layer_id == *id)
                .and_then(|l| l.text_frame.as_mut())
                .ok_or(FindChangeError::Layout(LayoutError::InvalidInput))?;
            if let Some(n) = change.text_column_count {
                if n == 0 {
                    return Err(LayoutError::InvalidInput.into());
                }
                f.text_column_count = n;
            }
            if let Some(v) = change.vertical_justification {
                f.vertical_justification = v;
            }
        }
    }
    Ok((doc, fl))
}

/// `find.colour`: the swatch paints (PAINT style id, row) that use `swatch_id`.
pub fn find_colour(records: &StudioVectorRecords, swatch_id: &str) -> Vec<(String, u32)> {
    records.swatch_paints.iter().filter(|p| p.swatch_id == swatch_id).map(|p| (p.style_id.clone(), p.row)).collect()
}

/// `find.colour` change: every usage of `from` becomes `to` (per-use tints kept). `to` must be a
/// swatch of the records. Returns the new records and the number of usages changed.
pub fn change_colour(records: &StudioVectorRecords, from: &str, to: &str) -> Result<(StudioVectorRecords, usize), FindChangeError> {
    if !records.swatches.iter().any(|s| s.swatch_id == to) {
        return Err(LayoutError::InvalidInput.into());
    }
    let mut r = records.clone();
    let mut n = 0usize;
    for p in r.swatch_paints.iter_mut().filter(|p| p.swatch_id == from) {
        p.swatch_id = to.to_owned();
        n = n.saturating_add(1);
    }
    Ok((r, n))
}

/// A saved query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedQuery {
    /// The captured mode name (`find.text`, `find.grep`, `find.glyph`, `find.transliterate`).
    pub mode: String,
    pub find: String,
    pub change: Option<String>,
    /// Shipped queries are read-only.
    pub locked: bool,
}

/// Named saved queries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SavedQueries {
    pub queries: BTreeMap<String, SavedQuery>,
}

const MODES: [&str; 4] = ["find.text", "find.grep", "find.glyph", "find.transliterate"];

fn query_name_ok(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= 255
        && !name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
}

impl SavedQueries {
    /// Saves (or replaces an unlocked) query.
    pub fn save(&mut self, name: &str, q: SavedQuery) -> Result<(), FindChangeError> {
        if !query_name_ok(name) {
            return Err(FindChangeError::InvalidQueryName);
        }
        if !MODES.contains(&q.mode.as_str()) {
            return Err(FindChangeError::InvalidSearchMode(q.mode));
        }
        check_len(&q.find, true)?;
        match self.queries.get(name) {
            Some(old) if old.locked => return Err(FindChangeError::QueryLocked(name.into())),
            None if self.queries.len() >= MAX_SAVED_QUERIES => return Err(FindChangeError::TooManyQueries),
            _ => {}
        }
        self.queries.insert(name.to_owned(), q);
        Ok(())
    }
    /// Loads a query; `mode` must be one of the captured modes.
    pub fn load(&self, name: &str) -> Result<&SavedQuery, FindChangeError> {
        if !query_name_ok(name) {
            return Err(FindChangeError::InvalidQueryName);
        }
        let q = self.queries.get(name).ok_or_else(|| FindChangeError::QueryNotFound(name.into()))?;
        if !MODES.contains(&q.mode.as_str()) {
            return Err(FindChangeError::InvalidSearchMode(q.mode.clone()));
        }
        Ok(q)
    }
    /// Deletes an unlocked query.
    pub fn delete(&mut self, name: &str) -> Result<(), FindChangeError> {
        if !query_name_ok(name) {
            return Err(FindChangeError::InvalidQueryName);
        }
        match self.queries.get(name) {
            None => Err(FindChangeError::QueryNotFound(name.into())),
            Some(q) if q.locked => Err(FindChangeError::QueryLocked(name.into())),
            Some(_) => {
                self.queries.remove(name);
                Ok(())
            }
        }
    }
}
