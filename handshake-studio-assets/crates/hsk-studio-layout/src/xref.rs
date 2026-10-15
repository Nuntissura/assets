//! Cross-references (lane SMB-LAYOUT-UI; STU-LAY-047, STU-LAY-151).
//!
//! A format definition is a building-block string, tokenized here (never substring-matched):
//! literal text with the entities `&lt; &gt; &amp; &quot; &apos;` (a bare `&` or `<` is invalid
//! syntax), and the tags `<fullPara/>`, `<fullPara delim="x" includeDelim="true|false"/>` (partial
//! paragraph up to the first delimiter), `<paraText/>`, `<paraNum/>`, `<txtAnchrName/>`,
//! `<pageNum/>`, `<chapNum/>`, `<fileName/>` and one level of `<cs name="Style">...</cs>`. Tag and
//! attribute names are case-sensitive; whitespace is allowed between attributes and before `/>`.
//! The tag vocabulary follows InDesign's building blocks by name; the syntax and its equivalence
//! with InDesign are UNVERIFIED (D5).
//!
//! Studio carries no autonumbered paragraphs, so `paraText` equals `fullPara` and `paraNum` is a
//! typed refusal (`xref_block_unsupported`). Resolution reads the destination paragraph text from
//! the destination story, the page from its placement (`PageNumber::label` of the page holding the
//! line with the destination byte) and reports per reference `current`, `stale` (the cached
//! `resolved_text` differs), `broken` (destination story, paragraph or byte missing) or `unplaced`
//! (destination overset or not composed: no page).
#![deny(clippy::arithmetic_side_effects)]
use crate::numbering::PageNumber;
use crate::threaded_reflow::LayoutProposal;
use hsk_studio_folio::{
    StudioLayoutFlowRecords, StudioTextStory, StudioTypeStyleRecords, StudioXrefDestination,
    TypeStyleScope,
};

/// Tokens of one definition (checked before the push).
pub const MAX_XREF_TOKENS: usize = 1024;

/// Typed cross-reference refusals (STU-LAY-151 constraint vocabulary).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XrefError {
    /// The format string has invalid syntax (byte offset).
    InvalidSyntax(usize),
    /// A `delim` attribute is not exactly one character.
    DelimiterNotSingleChar,
    /// `includeDelim` is not exactly `true` or `false`.
    DelimiterInclusionNotBool,
    /// A named character style does not exist in this document.
    CharacterStyleMissing(String),
    /// A building block Studio cannot resolve (`paraNum`).
    BlockUnsupported(&'static str),
    /// The format named by a reference does not exist.
    FormatMissing(String),
    BudgetExceeded,
}
impl XrefError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidSyntax(_) => "xref_format_invalid_syntax",
            Self::DelimiterNotSingleChar => "xref_delimiter_not_single_char",
            Self::DelimiterInclusionNotBool => "xref_delimiter_inclusion_not_bool",
            Self::CharacterStyleMissing(_) => "character_style_missing",
            Self::BlockUnsupported(_) => "xref_block_unsupported",
            Self::FormatMissing(_) => "xref_format_missing",
            Self::BudgetExceeded => "budget_exceeded",
        }
    }
}
impl std::fmt::Display for XrefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for XrefError {}

/// One building block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum XrefBlock {
    Text(String),
    FullPara,
    PartialPara { delimiter: char, include: bool },
    ParaText,
    ParaNum,
    TextAnchorName,
    PageNum,
    ChapterNum,
    FileName,
    /// Start of a character-style run (style NAME as written).
    StyleStart(String),
    StyleEnd,
}

fn entity(s: &str, at: usize) -> Result<(char, usize), XrefError> {
    for (name, c) in [("&lt;", '<'), ("&gt;", '>'), ("&amp;", '&'), ("&quot;", '"'), ("&apos;", '\'')] {
        if s[at..].starts_with(name) {
            return Ok((c, name.len()));
        }
    }
    Err(XrefError::InvalidSyntax(at))
}

/// Decodes an attribute value (between quotes) starting after the opening quote; returns the value
/// and the index after the closing quote.
fn attr_value(s: &str, mut at: usize) -> Result<(String, usize), XrefError> {
    let mut out = String::new();
    loop {
        let c = s[at..].chars().next().ok_or(XrefError::InvalidSyntax(at))?;
        match c {
            '"' => return Ok((out, at.saturating_add(1))),
            '&' => {
                let (d, n) = entity(s, at)?;
                out.push(d);
                at = at.saturating_add(n);
            }
            '<' => return Err(XrefError::InvalidSyntax(at)),
            _ => {
                out.push(c);
                at = at.saturating_add(c.len_utf8());
            }
        }
    }
}

fn skip_ws(s: &str, mut at: usize) -> usize {
    while let Some(c) = s[at..].chars().next() {
        if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
            at = at.saturating_add(1);
        } else {
            break;
        }
    }
    at
}

/// Tokenizes a format definition (module docs).
pub fn parse_xref_format(def: &str) -> Result<Vec<XrefBlock>, XrefError> {
    if def.len() > hsk_studio_folio::MAX_XREF_FORMAT_BYTES {
        return Err(XrefError::BudgetExceeded);
    }
    let mut out: Vec<XrefBlock> = Vec::new();
    let mut text = String::new();
    let mut in_style = false;
    let mut at = 0usize;
    let push = |out: &mut Vec<XrefBlock>, b: XrefBlock| -> Result<(), XrefError> {
        if out.len() >= MAX_XREF_TOKENS {
            return Err(XrefError::BudgetExceeded);
        }
        out.push(b);
        Ok(())
    };
    while at < def.len() {
        let c = def[at..].chars().next().ok_or(XrefError::InvalidSyntax(at))?;
        match c {
            '&' => {
                let (d, n) = entity(def, at)?;
                text.push(d);
                at = at.saturating_add(n);
            }
            '>' => return Err(XrefError::InvalidSyntax(at)),
            '<' => {
                if !text.is_empty() {
                    push(&mut out, XrefBlock::Text(std::mem::take(&mut text)))?;
                }
                let start = at;
                at = at.saturating_add(1);
                if def[at..].starts_with("/cs>") {
                    if !in_style {
                        return Err(XrefError::InvalidSyntax(start));
                    }
                    in_style = false;
                    push(&mut out, XrefBlock::StyleEnd)?;
                    at = at.saturating_add(4);
                    continue;
                }
                let name_end = def[at..]
                    .find(|ch: char| !ch.is_ascii_alphabetic())
                    .map_or(def.len(), |i| at.saturating_add(i));
                let name = &def[at..name_end];
                at = name_end;
                // Attributes.
                let mut attrs: Vec<(String, String)> = Vec::new();
                loop {
                    at = skip_ws(def, at);
                    if def[at..].starts_with("/>") || def[at..].starts_with('>') {
                        break;
                    }
                    let an_end = def[at..]
                        .find(|ch: char| !ch.is_ascii_alphabetic())
                        .map_or(def.len(), |i| at.saturating_add(i));
                    if an_end == at || !def[an_end..].starts_with("=\"") {
                        return Err(XrefError::InvalidSyntax(at));
                    }
                    let an = def[at..an_end].to_owned();
                    let (v, next) = attr_value(def, an_end.saturating_add(2))?;
                    if attrs.iter().any(|(k, _)| *k == an) || attrs.len() >= 4 {
                        return Err(XrefError::InvalidSyntax(at));
                    }
                    attrs.push((an, v));
                    at = next;
                }
                let self_closing = def[at..].starts_with("/>");
                at = at.saturating_add(if self_closing { 2 } else { 1 });
                let get = |k: &str| attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
                let block = match (name, self_closing) {
                    ("cs", false) => {
                        if in_style || attrs.len() != 1 {
                            return Err(XrefError::InvalidSyntax(start));
                        }
                        let n = get("name").ok_or(XrefError::InvalidSyntax(start))?;
                        in_style = true;
                        XrefBlock::StyleStart(n.to_owned())
                    }
                    ("fullPara", true) => match (get("delim"), get("includeDelim")) {
                        (None, None) if attrs.is_empty() => XrefBlock::FullPara,
                        (Some(d), inc) if attrs.len() == if inc.is_some() { 2 } else { 1 } => {
                            let mut cs = d.chars();
                            let delimiter = match (cs.next(), cs.next()) {
                                (Some(ch), None) => ch,
                                _ => return Err(XrefError::DelimiterNotSingleChar),
                            };
                            let include = match inc {
                                None | Some("false") => false,
                                Some("true") => true,
                                Some(_) => return Err(XrefError::DelimiterInclusionNotBool),
                            };
                            XrefBlock::PartialPara { delimiter, include }
                        }
                        _ => return Err(XrefError::InvalidSyntax(start)),
                    },
                    (n, true) if attrs.is_empty() => match n {
                        "paraText" => XrefBlock::ParaText,
                        "paraNum" => XrefBlock::ParaNum,
                        "txtAnchrName" => XrefBlock::TextAnchorName,
                        "pageNum" => XrefBlock::PageNum,
                        "chapNum" => XrefBlock::ChapterNum,
                        "fileName" => XrefBlock::FileName,
                        _ => return Err(XrefError::InvalidSyntax(start)),
                    },
                    _ => return Err(XrefError::InvalidSyntax(start)),
                };
                push(&mut out, block)?;
            }
            _ => {
                text.push(c);
                at = at.saturating_add(c.len_utf8());
            }
        }
    }
    if in_style {
        return Err(XrefError::InvalidSyntax(def.len()));
    }
    if !text.is_empty() {
        push(&mut out, XrefBlock::Text(text))?;
    }
    Ok(out)
}

/// A story a reference may point into (destination) or live in (source).
#[derive(Clone, Copy, Debug)]
pub struct XrefStory<'a> {
    pub story: &'a StudioTextStory,
    pub text: &'a str,
    /// Its composition; `None` = not composed (no page numbers).
    pub placement: Option<&'a LayoutProposal>,
}

/// Document facts the blocks read.
#[derive(Clone, Copy, Debug)]
pub struct XrefContext<'a> {
    pub file_name: &'a str,
    pub chapter_number: Option<&'a str>,
    pub pages: &'a [PageNumber],
    pub type_styles: Option<&'a StudioTypeStyleRecords>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XrefStatus {
    Current,
    Stale,
    Broken,
    Unplaced,
}

/// Resolved text in runs with their character style (`STYS` id; the format's style when a run has
/// none of its own).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XrefRun {
    pub text: String,
    pub character_style: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct XrefResolution {
    pub xref_id: String,
    pub status: XrefStatus,
    /// `None` when broken or unplaced.
    pub text: Option<String>,
    pub runs: Vec<XrefRun>,
}

fn char_style_id(ctx: &XrefContext<'_>, name: &str) -> Result<String, XrefError> {
    ctx.type_styles
        .and_then(|t| {
            t.styles
                .iter()
                .find(|s| s.name == name && s.scope == TypeStyleScope::Character)
        })
        .map(|s| s.style_id.clone())
        .ok_or_else(|| XrefError::CharacterStyleMissing(name.to_owned()))
}

/// The page label of the page holding `byte` of a composed story.
fn page_of(p: &LayoutProposal, byte: u64, pages: &[PageNumber]) -> Option<String> {
    let f = p
        .frames
        .iter()
        .find(|f| f.lines.iter().any(|l| l.source.start <= byte && byte < l.source.end.max(l.source.start.saturating_add(1))))?;
    pages
        .iter()
        .find(|n| n.spread == f.spread && n.page == f.page)
        .map(|n| n.label.clone())
}

/// Resolves every cross-reference of the record set (module docs). Formats are parsed and their
/// character styles checked first, so a bad format refuses the whole call.
pub fn resolve_cross_references(
    records: &StudioLayoutFlowRecords,
    stories: &[XrefStory<'_>],
    ctx: &XrefContext<'_>,
) -> Result<Vec<XrefResolution>, XrefError> {
    let mut formats = Vec::with_capacity(records.xref_formats.len());
    for f in &records.xref_formats {
        let blocks = parse_xref_format(&f.definition)?;
        for b in &blocks {
            if let XrefBlock::StyleStart(n) = b {
                char_style_id(ctx, n)?;
            }
        }
        if let Some(cs) = &f.character_style {
            let ok = ctx
                .type_styles
                .and_then(|t| t.style(cs))
                .is_some_and(|s| s.scope == TypeStyleScope::Character);
            if !ok {
                return Err(XrefError::CharacterStyleMissing(cs.clone()));
            }
        }
        formats.push((f, blocks));
    }
    let mut out = Vec::with_capacity(records.cross_references.len());
    for x in &records.cross_references {
        let (format, blocks) = formats
            .iter()
            .find(|(f, _)| f.format_id == x.format_id)
            .ok_or_else(|| XrefError::FormatMissing(x.format_id.clone()))?;
        let broken = XrefResolution { xref_id: x.xref_id.clone(), status: XrefStatus::Broken, text: None, runs: vec![] };
        let (story_id, byte, anchor_name) = match &x.destination {
            StudioXrefDestination::Paragraph { story_id, paragraph_start } => (story_id.as_str(), *paragraph_start, None),
            StudioXrefDestination::TextAnchor { anchor_id } => match records.text_anchors.iter().find(|a| a.anchor_id == *anchor_id) {
                Some(a) => (a.story_id.as_str(), a.byte, Some(a.name.as_str())),
                None => {
                    out.push(broken);
                    continue;
                }
            },
        };
        let Some(dest) = stories.iter().find(|s| s.story.story_id == story_id) else {
            out.push(broken);
            continue;
        };
        let para = dest.story.paragraphs.iter().find(|p| {
            if anchor_name.is_some() { p.start <= byte && byte < p.end } else { p.start == byte }
        });
        let Some(para) = para else {
            out.push(broken);
            continue;
        };
        let (Ok(ps), Ok(pe)) = (usize::try_from(para.start), usize::try_from(para.end)) else {
            out.push(broken);
            continue;
        };
        let Some(full) = dest.text.get(ps..pe) else {
            out.push(broken);
            continue;
        };
        let full = full.trim_end_matches(['\n', '\r']);
        let page = dest.placement.and_then(|p| page_of(p, byte, ctx.pages));
        let mut runs: Vec<XrefRun> = Vec::new();
        let mut style: Option<String> = None;
        let mut unplaced = false;
        for b in blocks {
            let piece = match b {
                XrefBlock::Text(t) => t.clone(),
                XrefBlock::FullPara | XrefBlock::ParaText => full.to_owned(),
                XrefBlock::PartialPara { delimiter, include } => match full.find(*delimiter) {
                    Some(i) => {
                        let end = if *include { i.saturating_add(delimiter.len_utf8()) } else { i };
                        full[..end].to_owned()
                    }
                    None => full.to_owned(),
                },
                XrefBlock::ParaNum => return Err(XrefError::BlockUnsupported("paraNum")),
                XrefBlock::TextAnchorName => anchor_name.unwrap_or_default().to_owned(),
                XrefBlock::PageNum => match &page {
                    Some(l) => l.clone(),
                    None => {
                        unplaced = true;
                        String::new()
                    }
                },
                XrefBlock::ChapterNum => ctx.chapter_number.unwrap_or_default().to_owned(),
                XrefBlock::FileName => ctx.file_name.to_owned(),
                XrefBlock::StyleStart(n) => {
                    style = Some(char_style_id(ctx, n)?);
                    continue;
                }
                XrefBlock::StyleEnd => {
                    style = None;
                    continue;
                }
            };
            let cs = style.clone().or_else(|| format.character_style.clone());
            match runs.last_mut() {
                Some(r) if r.character_style == cs => r.text.push_str(&piece),
                _ if piece.is_empty() => {}
                _ => runs.push(XrefRun { text: piece, character_style: cs }),
            }
        }
        if unplaced {
            out.push(XrefResolution { xref_id: x.xref_id.clone(), status: XrefStatus::Unplaced, text: None, runs: vec![] });
            continue;
        }
        let text: String = runs.iter().map(|r| r.text.as_str()).collect();
        let status = if x.resolved_text.as_deref() == Some(text.as_str()) { XrefStatus::Current } else { XrefStatus::Stale };
        out.push(XrefResolution { xref_id: x.xref_id.clone(), status, text: Some(text), runs });
    }
    Ok(out)
}

/// Writes a resolved reference into its source story: the source range is replaced by `text`
/// (`replace_story_text`), every flow-record byte of that story is shifted, and the reference's
/// cached `resolved_text` becomes `text` (so it reads `current` afterwards).
pub fn write_cross_reference(
    records: &mut StudioLayoutFlowRecords,
    source: &StudioTextStory,
    resolved_text: Option<&str>,
    xref_id: &str,
    text: &str,
) -> Result<StudioTextStory, crate::LayoutError> {
    let x = records
        .cross_references
        .iter()
        .find(|x| x.xref_id == xref_id && x.source_story_id == source.story_id)
        .ok_or(crate::LayoutError::InvalidInput)?;
    let (start, end) = (x.source_start, x.source_end);
    let edited = crate::textflow::replace_story_text(source, resolved_text, start, end, text)?;
    crate::textflow::shift_flow_records(records, &source.story_id, start, end, text.len() as u64)?;
    if let Some(x) = records.cross_references.iter_mut().find(|x| x.xref_id == xref_id) {
        x.resolved_text = Some(text.to_owned());
    }
    Ok(edited)
}
