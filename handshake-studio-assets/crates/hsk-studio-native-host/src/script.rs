//! Scripted input: a typed queue of [`HostEvent`]s and invoke-by-author_id actions, the only input tests and agents
//! feed (STU-QUIET-002, HBR-QUIET-002: never OS input; STU-MDL-109: the command API plus the tree by author_id).
//!
//! Script file, one step per line, `#` comments:
//! `move X Y` (physical px), `leave`, `down [left|right|middle]`, `up [...]`, `click X Y` (move, down, up),
//! `wheel N` (N notches, positive reveals rows above), `wheelpx N`, `key [ctrl+][shift+][alt+]NAME` (press and
//! release), `resize W H`, `scale MILLI`, `invoke AUTHOR_ID`.
use crate::error::HostError;
use crate::event::{Button, HostEvent, Key, Mods};
use std::path::Path;

/// Most steps one script may hold; a longer list is refused (`script_too_long`).
pub const MAX_STEPS: usize = 4096;
/// Largest script file read.
pub const MAX_SCRIPT_BYTES: u64 = 1 << 20;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Event(HostEvent),
    /// Activate the control with this author_id as a release inside it would (no pointer involved).
    Invoke(String),
}

/// The three events of a left click at physical (x, y).
pub fn click(x: i32, y: i32) -> [Step; 3] {
    [
        Step::Event(HostEvent::PointerMoved { x, y }),
        Step::Event(HostEvent::Button { button: Button::Left, pressed: true }),
        Step::Event(HostEvent::Button { button: Button::Left, pressed: false }),
    ]
}

/// Press and release of `key` with `mods`.
pub fn chord(key: Key, mods: Mods) -> [Step; 2] {
    [Step::Event(HostEvent::Key { key, pressed: true, mods }), Step::Event(HostEvent::Key { key, pressed: false, mods })]
}

/// Parses a script; any unknown line is `script` (exit 2), more than [`MAX_STEPS`] steps is `script_too_long`.
pub fn parse(text: &str) -> Result<Vec<Step>, HostError> {
    let _ = text;
    Err(HostError::Usage("not_built"))
}

/// Reads and parses a script file (bounded by [`MAX_SCRIPT_BYTES`]).
pub fn read(path: &Path) -> Result<Vec<Step>, HostError> {
    let _ = path;
    Err(HostError::Usage("not_built"))
}

/// Appends `more` to `steps`, refusing a total over [`MAX_STEPS`].
pub fn extend(steps: &mut Vec<Step>, more: impl IntoIterator<Item = Step>) -> Result<(), HostError> {
    let _ = (steps, more.into_iter());
    Err(HostError::Usage("not_built"))
}
