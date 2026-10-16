//! Find/change over story text (lane SMB-LAYOUT-UI-2; STU-LAY-027/129, modes `find.text` and
//! `find.grep`). One command family with a mode discriminator: `Text` searches a literal (escaped
//! before compilation, so metacharacters are literal) with case and whole-word toggles; `Grep`
//! searches a regular expression with capture groups whose replacement expands `$0`, `$1`, `${name}`
//! (`$$` is a literal dollar). Both compile through the same bounded linear-time engine as GREP
//! styles ([`crate::grep`]: program size, DFA cache and nesting limits; look-around and
//! back-references are typed `grep_pattern_unsupported`, a parity gap against InDesign GREP,
//! UNVERIFIED D5). Matches are byte ranges of the story text, in order, non-overlapping, empty
//! matches skipped, at most `MAX_GREP_MATCHES` per call (`grep_budget_exceeded`).
//!
//! A change is computed, never applied here: [`find_in_story`] returns each hit with its
//! replacement text; callers apply hits back to front through `replace_story_text` (one edit).
//! Declared gaps: `find.glyph`, `find.object`, `find.transliterate`, `find.colour`, formatting
//! criteria, saved queries and location tokens.
#![deny(clippy::arithmetic_side_effects)]
use crate::folio_input::story_text;
use crate::grep::{GREP_DFA_LIMIT, GREP_NEST_LIMIT, GREP_SIZE_LIMIT, GrepError, MAX_GREP_MATCHES};
use hsk_studio_folio::StudioTextStory;

/// Pattern bytes accepted by find/change (the Folio GREP-rule ceiling).
pub const MAX_FIND_PATTERN_BYTES: usize = hsk_studio_folio::MAX_GREP_PATTERN_BYTES;

/// Find mode (STU-LAY-129 discriminator; the two text modes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindMode {
    /// Literal text.
    Text { case_sensitive: bool, whole_word: bool },
    /// Regular expression.
    Grep { case_sensitive: bool },
}

/// One hit: story bytes and, when a change was requested, the replacement text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindHit {
    pub start: u64,
    pub end: u64,
    pub replacement: Option<String>,
}

/// Compiles a find pattern with the bounded engine (the same checks find/change and GREP styles use).
pub fn compile_find(pattern: &str, mode: FindMode) -> Result<regex::Regex, GrepError> {
    if pattern.is_empty() || pattern.len() > MAX_FIND_PATTERN_BYTES {
        return Err(GrepError::PatternInvalid(String::new(), 0));
    }
    let (source, case_sensitive) = match mode {
        FindMode::Text { case_sensitive, whole_word } => {
            let lit = regex::escape(pattern);
            (if whole_word { format!(r"\b(?:{lit})\b") } else { lit }, case_sensitive)
        }
        FindMode::Grep { case_sensitive } => (pattern.to_owned(), case_sensitive),
    };
    regex::RegexBuilder::new(&source)
        .case_insensitive(!case_sensitive)
        .size_limit(GREP_SIZE_LIMIT)
        .dfa_size_limit(GREP_DFA_LIMIT)
        .nest_limit(GREP_NEST_LIMIT)
        .build()
        .map_err(|e| match e {
            regex::Error::CompiledTooBig(_) => GrepError::PatternUnsupported(String::new(), 0),
            regex::Error::Syntax(s)
                if s.contains("look-around") || s.contains("backreferences") || s.contains("nested parentheses") =>
            {
                GrepError::PatternUnsupported(String::new(), 0)
            }
            _ => GrepError::PatternInvalid(String::new(), 0),
        })
}

/// Every hit of `pattern` in the story bytes `[from, to)` (`None` = the whole story). With
/// `change`, each hit carries its replacement (`Text`: the literal; `Grep`: capture expansion).
pub fn find_in_story(
    story: &StudioTextStory,
    resolved_text: Option<&str>,
    pattern: &str,
    mode: FindMode,
    range: Option<(u64, u64)>,
    change: Option<&str>,
) -> Result<Vec<FindHit>, GrepError> {
    let text = story_text(story, resolved_text)?;
    let re = compile_find(pattern, mode)?;
    let (from, to) = match range {
        None => (0usize, text.len()),
        Some((a, b)) => {
            let (a, b) = (usize::try_from(a).map_err(|_| crate::LayoutError::InvalidInput)?, usize::try_from(b).map_err(|_| crate::LayoutError::InvalidInput)?);
            if a > b || b > text.len() || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
                return Err(crate::LayoutError::InvalidInput.into());
            }
            (a, b)
        }
    };
    let hay = &text[..to];
    let mut out = Vec::new();
    for caps in re.captures_iter_at_from(hay, from) {
        let Some(m) = caps.get(0) else { continue };
        if m.start() == m.end() {
            continue;
        }
        if out.len() >= MAX_GREP_MATCHES {
            return Err(GrepError::BudgetExceeded);
        }
        let replacement = change.map(|c| match mode {
            FindMode::Text { .. } => c.to_owned(),
            FindMode::Grep { .. } => {
                let mut s = String::new();
                caps.expand(c, &mut s);
                s
            }
        });
        out.push(FindHit { start: m.start() as u64, end: m.end() as u64, replacement });
    }
    Ok(out)
}

/// `captures_iter` starting at a byte offset (the regex crate exposes `captures_at` only).
trait CapturesFrom {
    fn captures_iter_at_from<'h>(&self, hay: &'h str, from: usize) -> CapturesAt<'_, 'h>;
}
impl CapturesFrom for regex::Regex {
    fn captures_iter_at_from<'h>(&self, hay: &'h str, from: usize) -> CapturesAt<'_, 'h> {
        CapturesAt { re: self, hay, at: Some(from) }
    }
}
struct CapturesAt<'r, 'h> {
    re: &'r regex::Regex,
    hay: &'h str,
    at: Option<usize>,
}
impl<'h> Iterator for CapturesAt<'_, 'h> {
    type Item = regex::Captures<'h>;
    fn next(&mut self) -> Option<Self::Item> {
        let at = self.at?;
        let caps = self.re.captures_at(self.hay, at)?;
        let m = caps.get(0)?;
        // After an empty match the search advances one char (no infinite loop).
        let next = if m.start() == m.end() {
            let step = self.hay[m.end()..].chars().next().map_or(1, char::len_utf8);
            m.end().checked_add(step)
        } else {
            Some(m.end())
        };
        self.at = next.filter(|n| *n <= self.hay.len());
        Some(caps)
    }
}
