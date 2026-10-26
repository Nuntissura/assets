//! Optional whole-literal comparison. Hosts own authorized candidates and retrieval policy.
use serde::Serialize;

/// Maximum Unicode scalar counts accepted before allocating distance work.
pub const MAX_NEEDLE_CHARS: usize = 64;
pub const MAX_CANDIDATE_CHARS: usize = 256;
pub const MAX_LITERAL_EDITS: u8 = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct LiteralMatch {
    pub edit_distance: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiteralMatchError {
    EmptyNeedle,
    NeedleLimit,
    CandidateLimit,
    ThresholdLimit,
}

/// Case-sensitive optimal string alignment distance over entire literals, including
/// adjacent transpositions. This does not parse queries, normalize text, tokenize,
/// find substrings, rank results or establish authorization.
pub fn match_literal(
    needle: &str,
    candidate: &str,
    max_edits: u8,
) -> Result<Option<LiteralMatch>, LiteralMatchError> {
    if max_edits > MAX_LITERAL_EDITS {
        return Err(LiteralMatchError::ThresholdLimit);
    }
    if needle.is_empty() {
        return Err(LiteralMatchError::EmptyNeedle);
    }
    let needle_chars = needle.chars().take(MAX_NEEDLE_CHARS + 1).count();
    if needle_chars > MAX_NEEDLE_CHARS {
        return Err(LiteralMatchError::NeedleLimit);
    }
    let candidate_chars = candidate.chars().take(MAX_CANDIDATE_CHARS + 1).count();
    if candidate_chars > MAX_CANDIDATE_CHARS {
        return Err(LiteralMatchError::CandidateLimit);
    }
    if needle_chars.abs_diff(candidate_chars) > usize::from(max_edits) {
        return Ok(None);
    }
    let distance = strsim::osa_distance(needle, candidate);
    Ok(u8::try_from(distance)
        .ok()
        .filter(|distance| *distance <= max_edits)
        .map(|edit_distance| LiteralMatch { edit_distance }))
}
