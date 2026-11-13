//! Script itemization (lane SMB-TYPE-CHARACTER-ATTRIBUTES-1; STU-TYP-127 "script and language run
//! resolution"): a host splits each style span at the runs [`script_runs`] gives and sends each run
//! with its own ISO 15924 tag as `Style::script`, so a shaping request never declares `Latn` for
//! text of another script (the engine refuses a span whose strong characters disagree with its
//! declared script, `Error::UnsupportedProfile`). The language of a run is the host's (the range's
//! `applied_language` record); this module does not guess one.
//!
//! Rule (a Studio reading of UAX #24 script runs, UNVERIFIED against the source applications, D5):
//! Common and Inherited characters (spaces, digits, punctuation, combining marks) join the run they
//! stand in; those before the first strong character join the first run; a text with no strong
//! character is one `Zyyy` run. Paired-bracket matching is not done. Unassigned characters carry
//! `Zzzz`, which the shaping engine refuses typed.
//!
//! Hostile input: one table lookup per character, at most `max_runs` runs (`Error::Budget` before
//! the next push), no arithmetic beyond widening casts.
#![deny(clippy::arithmetic_side_effects)]
use crate::{Error, SourceRange};
use unicode_script::{Script, UnicodeScript};

/// One run of one script: bytes `range` of the itemized text, ISO 15924 `script`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptRun {
    pub range: SourceRange,
    pub script: [u8; 4],
}

fn tag_of(script: Script) -> [u8; 4] {
    <[u8; 4]>::try_from(script.short_name().as_bytes()).unwrap_or(*b"Zzzz")
}

/// The script runs of `text` (module docs), in text order, tiling it. Empty text has no run.
pub fn script_runs(text: &str, max_runs: usize) -> Result<Vec<ScriptRun>, Error> {
    let mut runs: Vec<ScriptRun> = Vec::new();
    let mut current: Option<[u8; 4]> = None;
    let mut start = 0usize;
    for (i, c) in text.char_indices() {
        let script = match c.script() {
            Script::Common | Script::Inherited => continue,
            other => tag_of(other),
        };
        match current {
            Some(cur) if cur != script => {
                if runs.len() >= max_runs {
                    return Err(Error::Budget);
                }
                runs.push(ScriptRun { range: SourceRange { start: start as u64, end: i as u64 }, script: cur });
                start = i;
                current = Some(script);
            }
            Some(_) => {}
            None => current = Some(script),
        }
    }
    if !text.is_empty() {
        if runs.len() >= max_runs {
            return Err(Error::Budget);
        }
        runs.push(ScriptRun { range: SourceRange { start: start as u64, end: text.len() as u64 }, script: current.unwrap_or(*b"Zyyy") });
    }
    Ok(runs)
}
