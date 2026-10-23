//! Pattern (GREP) styles (lane SMB-LAYOUT-UI; STU-LAY-038): every regular-expression match inside
//! a paragraph whose paragraph style (or a `based_on` ancestor, parent rules first) carries
//! `grep_styles` gets the rule's character style, resolved BEFORE composition.
//!
//! Engine: the `regex` crate (finite automata; search time linear in the paragraph length,
//! no backtracking), compiled with bounded program and cache sizes and nesting depth. Patterns the
//! engine cannot run in linear time (look-around, back-references) do not compile and are refused
//! as `grep_pattern_unsupported` (InDesign GREP supports them; parity gap, UNVERIFIED D5).
//! Matches run per paragraph (they never cross a paragraph boundary), empty matches are skipped,
//! later rules win where matches overlap (InDesign order semantics UNVERIFIED), and the total number
//! of styled matches per story is bounded (`grep_budget_exceeded`).
#![deny(clippy::arithmetic_side_effects)]
use crate::folio_input::story_text;
use crate::LayoutError;
use hsk_studio_folio::{StoryContent, StoryRange, StudioTextStory, StudioTypeStyleRecords, TypeStyleScope};

/// Compiled program bytes per pattern.
pub const GREP_SIZE_LIMIT: usize = 1 << 20;
/// Lazy-DFA cache bytes per pattern.
pub const GREP_DFA_LIMIT: usize = 1 << 20;
/// Pattern nesting depth.
pub const GREP_NEST_LIMIT: u32 = 64;
/// Styled matches per story.
pub const MAX_GREP_MATCHES: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GrepError {
    Layout(LayoutError),
    /// The pattern is not valid syntax (style id, rule index).
    PatternInvalid(String, usize),
    /// The pattern needs look-around/back-references or exceeds the size limits.
    PatternUnsupported(String, usize),
    /// A style or rule names a style that is missing or of the wrong scope.
    StyleMissing(String),
    BudgetExceeded,
}
impl GrepError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Layout(e) => e.code(),
            Self::PatternInvalid(..) => "grep_pattern_invalid",
            Self::PatternUnsupported(..) => "grep_pattern_unsupported",
            Self::StyleMissing(_) => "grep_style_missing",
            Self::BudgetExceeded => "grep_budget_exceeded",
        }
    }
}
impl From<LayoutError> for GrepError {
    fn from(e: LayoutError) -> Self {
        Self::Layout(e)
    }
}
impl std::fmt::Display for GrepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for GrepError {}

/// One styled match: story bytes and the character style applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrepMatch {
    pub start: u64,
    pub end: u64,
    pub character_style: String,
}

fn compile(pattern: &str, style: &str, rule: usize) -> Result<regex::Regex, GrepError> {
    regex::RegexBuilder::new(pattern)
        .size_limit(GREP_SIZE_LIMIT)
        .dfa_size_limit(GREP_DFA_LIMIT)
        .nest_limit(GREP_NEST_LIMIT)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(_) => GrepError::PatternUnsupported(style.to_owned(), rule),
            // Look-around and back-references are syntax errors to this engine.
            regex::Error::Syntax(s)
                if s.contains("look-around") || s.contains("backreferences") || s.contains("nested parentheses") =>
            {
                GrepError::PatternUnsupported(style.to_owned(), rule)
            }
            _ => GrepError::PatternInvalid(style.to_owned(), rule),
        })
}

/// The rules of a paragraph style, ancestors first (`based_on`, bounded by the set size).
fn rules_of<'a>(
    styles: &'a StudioTypeStyleRecords,
    style_id: &str,
) -> Result<Vec<(&'a str, usize, &'a hsk_studio_folio::StudioGrepStyle)>, GrepError> {
    let mut chain = Vec::new();
    let mut at = Some(style_id);
    while let Some(id) = at {
        if chain.len() > styles.styles.len() {
            return Err(GrepError::Layout(LayoutError::InvalidInput));
        }
        let s = styles.style(id).ok_or_else(|| GrepError::StyleMissing(id.to_owned()))?;
        if s.scope != TypeStyleScope::Paragraph {
            return Err(GrepError::StyleMissing(id.to_owned()));
        }
        chain.push(s);
        at = s.based_on.as_deref();
    }
    Ok(chain
        .into_iter()
        .rev()
        .flat_map(|s| s.grep_styles.iter().enumerate().map(move |(i, g)| (s.style_id.as_str(), i, g)))
        .collect())
}

/// Every styled match of the story (paragraph order, then rule order).
pub fn grep_matches(
    story: &StudioTextStory,
    resolved_text: Option<&str>,
    styles: &StudioTypeStyleRecords,
) -> Result<Vec<GrepMatch>, GrepError> {
    let text = story_text(story, resolved_text)?;
    let mut out = Vec::new();
    let mut compiled: Vec<(String, usize, regex::Regex)> = Vec::new();
    for p in &story.paragraphs {
        let Some(sid) = p.style_ref.as_deref() else { continue };
        let (s, e) = (
            usize::try_from(p.start).map_err(|_| LayoutError::InvalidInput)?,
            usize::try_from(p.end).map_err(|_| LayoutError::InvalidInput)?,
        );
        let body = text.get(s..e).ok_or(LayoutError::InvalidInput)?;
        for (owner, i, g) in rules_of(styles, sid)? {
            let cs = styles
                .style(&g.character_style)
                .filter(|c| c.scope == TypeStyleScope::Character)
                .ok_or_else(|| GrepError::StyleMissing(g.character_style.clone()))?;
            let re = match compiled.iter().find(|(o, k, _)| o == owner && *k == i) {
                Some((_, _, re)) => re.clone(),
                None => {
                    let re = compile(&g.pattern, owner, i)?;
                    compiled.push((owner.to_owned(), i, re.clone()));
                    re
                }
            };
            for m in re.find_iter(body) {
                if m.start() == m.end() {
                    continue;
                }
                if out.len() >= MAX_GREP_MATCHES {
                    return Err(GrepError::BudgetExceeded);
                }
                let at = |o: usize| p.start.checked_add(o as u64).ok_or(GrepError::BudgetExceeded);
                out.push(GrepMatch { start: at(m.start())?, end: at(m.end())?, character_style: cs.style_id.clone() });
            }
        }
    }
    Ok(out)
}

/// The story with GREP-styled character ranges: every original range is split at match
/// boundaries and the matched parts take the match's character style (later matches win); the
/// partition still tiles the text.
pub fn apply_grep_styles(
    story: &StudioTextStory,
    resolved_text: Option<&str>,
    styles: &StudioTypeStyleRecords,
) -> Result<(StudioTextStory, Vec<GrepMatch>), GrepError> {
    let text = story_text(story, resolved_text)?;
    let matches = grep_matches(story, resolved_text, styles)?;
    // Style per boundary segment: collect cut points, then assign.
    let mut cuts: Vec<u64> = story
        .character_ranges
        .iter()
        .flat_map(|r| [r.start, r.end])
        .chain(matches.iter().flat_map(|m| [m.start, m.end]))
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut ranges: Vec<StoryRange> = Vec::with_capacity(cuts.len());
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let Some(base) = story.character_ranges.iter().find(|r| r.start <= a && b <= r.end) else {
            continue;
        };
        let styled = matches.iter().rev().find(|m| m.start <= a && b <= m.end);
        let style_ref = styled.map_or_else(|| base.style_ref.clone(), |m| Some(m.character_style.clone()));
        match ranges.last_mut() {
            Some(last) if last.end == a && last.style_ref == style_ref && last.applied_language == base.applied_language => last.end = b,
            _ => ranges.push(StoryRange { start: a, end: b, style_ref, applied_language: base.applied_language.clone() }),
        }
    }
    Ok((
        StudioTextStory {
            content: StoryContent::Inline { text_utf8: text.to_owned() },
            character_ranges: ranges,
            ..story.clone()
        },
        matches,
    ))
}
