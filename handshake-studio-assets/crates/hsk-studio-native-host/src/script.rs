//! Scripted input: a typed queue of [`HostEvent`]s and invoke-by-author_id actions, the only input tests and agents
//! feed (STU-QUIET-002, HBR-QUIET-002: never OS input; STU-MDL-109: the command API plus the tree by author_id).
//!
//! Script file, one step per line, lines starting with `#` are comments:
//! `move X Y` (physical px), `leave`, `down [left|right|middle]`, `up [...]`, `click X Y` (move, down, up),
//! `wheel N` (N notches, positive reveals rows above), `wheelpx N`, `key [ctrl+][shift+][alt+]NAME` (press and
//! release), `resize W H` and `scale MILLI` (CPU frame only), `invoke AUTHOR_ID`.
use crate::error::HostError;
use crate::event::{Button, HostEvent, Key, Mods};
use std::path::Path;

/// Most steps one script may hold; a longer list is refused (`script_too_long`).
pub const MAX_STEPS: usize = 4096;
/// Largest script file read.
pub const MAX_SCRIPT_BYTES: u64 = 1 << 20;
/// Longest author_id an invoke step carries.
pub const MAX_ID_BYTES: usize = 256;

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

fn bad() -> HostError {
    HostError::Usage("script")
}

fn button(name: Option<&&str>) -> Result<Button, HostError> {
    Ok(match name.copied() {
        None | Some("left") => Button::Left,
        Some("right") => Button::Right,
        Some("middle") => Button::Middle,
        _ => return Err(bad()),
    })
}

fn key_chord(text: &str) -> Result<(Key, Mods), HostError> {
    let mut parts: Vec<&str> = text.split('+').collect();
    let key = parts.pop().and_then(Key::from_name).ok_or_else(bad)?;
    let mut mods = Mods::default();
    for m in parts {
        match m {
            "ctrl" => mods.ctrl = true,
            "shift" => mods.shift = true,
            "alt" => mods.alt = true,
            _ => return Err(bad()),
        }
    }
    Ok((key, mods))
}

/// Parses a script; any unknown line is `script` (exit 2), more than [`MAX_STEPS`] steps is `script_too_long`.
pub fn parse(text: &str) -> Result<Vec<Step>, HostError> {
    let mut steps = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_ascii_whitespace();
        let verb = words.next().unwrap_or_default();
        let args: Vec<&str> = words.collect();
        let int = |i: usize| -> Result<i32, HostError> { args.get(i).and_then(|v| v.parse().ok()).ok_or_else(bad) };
        let uint = |i: usize| -> Result<u32, HostError> { args.get(i).and_then(|v| v.parse().ok()).ok_or_else(bad) };
        let ev = |e: HostEvent| vec![Step::Event(e)];
        let more = match (verb, args.len()) {
            ("move", 2) => ev(HostEvent::PointerMoved { x: int(0)?, y: int(1)? }),
            ("leave", 0) => ev(HostEvent::PointerLeft),
            ("down" | "up", 0 | 1) => ev(HostEvent::Button { button: button(args.first())?, pressed: verb == "down" }),
            ("click", 2) => click(int(0)?, int(1)?).to_vec(),
            ("wheel", 1) => ev(HostEvent::Wheel { x: 0, y: int(0)?.checked_mul(1000).ok_or_else(bad)?, pixels: false }),
            ("wheelpx", 1) => ev(HostEvent::Wheel { x: 0, y: int(0)?, pixels: true }),
            ("key", 1) => {
                let (key, mods) = key_chord(args[0])?;
                chord(key, mods).to_vec()
            }
            ("resize", 2) => ev(HostEvent::Resized { width: uint(0)?, height: uint(1)? }),
            ("scale", 1) => ev(HostEvent::ScaleFactor { milli: uint(0)? }),
            ("invoke", 1) if !args[0].is_empty() && args[0].len() <= MAX_ID_BYTES => vec![Step::Invoke(args[0].to_owned())],
            _ => return Err(bad()),
        };
        extend(&mut steps, more)?;
    }
    Ok(steps)
}

/// Reads and parses a script file (bounded by [`MAX_SCRIPT_BYTES`]).
pub fn read(path: &Path) -> Result<Vec<Step>, HostError> {
    let len = std::fs::metadata(path).map_err(|_| HostError::Usage("script_read"))?.len();
    if len > MAX_SCRIPT_BYTES {
        return Err(HostError::Usage("script_too_large"));
    }
    let text = std::fs::read_to_string(path).map_err(|_| HostError::Usage("script_read"))?;
    parse(&text)
}

/// Appends `more` to `steps`, refusing a total over [`MAX_STEPS`].
pub fn extend(steps: &mut Vec<Step>, more: impl IntoIterator<Item = Step>) -> Result<(), HostError> {
    for s in more {
        if steps.len() >= MAX_STEPS {
            return Err(HostError::Usage("script_too_long"));
        }
        steps.push(s);
    }
    Ok(())
}

/// Steps a window or an offscreen pane cannot take (their pane follows the window, not a script): resize and scale.
pub fn frame_only(steps: &[Step]) -> bool {
    steps.iter().any(|s| matches!(s, Step::Event(HostEvent::Resized { .. } | HostEvent::ScaleFactor { .. })))
}
