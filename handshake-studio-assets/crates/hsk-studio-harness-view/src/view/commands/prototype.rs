//! `prototype.*` (lane SMB-DRIVE-TYPED-2, STU-PRO-138/139): the in-editor player over a Figma `.fig`: the
//! spec's start_session, dispatch_trigger (a named trigger on a named node: no hit test, no pointer),
//! read_runtime_variables, capture_frame (an RGB PNG when a path is given, its size and channel means
//! always) and end_session, plus the play panel's frame-local click and hover, the virtual clock and
//! restart. Every reply carries the player's state and the events since the last row. Play mode changes
//! no document authority; one session per viewer (no session id yet).
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::prototype::{PlayFail, PrototypePlay, Screen};
use super::decode::{Fields, invalid};
use super::{CommandRefusal, read_bounded};
use hsk_studio_components::PlayerInput;
use serde_json::{Value, json};

/// Longest clock step of one `prototype.advance` (one hour).
pub const MAX_ADVANCE_MS: u64 = 3_600_000;

fn refusal(e: PlayFail) -> CommandRefusal {
    CommandRefusal::new(e.code, json!({"message": e.detail}))
}

fn state(play: &mut PrototypePlay) -> Value {
    let events: Vec<String> = play.take_events().iter().map(|e| format!("{e:?}")).collect();
    json!({"state": play.state_json(), "events": events})
}

/// Size and the mean of each channel: what the reply carries of a frame.
fn screen_json(screen: &Screen) -> Value {
    let mut sums = [0u64; 3];
    for px in screen.rgb.chunks_exact(3) {
        for (sum, c) in sums.iter_mut().zip(px) {
            *sum = sum.saturating_add(u64::from(*c));
        }
    }
    let pixels = u64::from(screen.width).saturating_mul(u64::from(screen.height)).max(1);
    json!({"width": screen.width, "height": screen.height, "mean_rgb": sums.map(|s| s.checked_div(pixels).unwrap_or(0))})
}

/// The frame as an 8-bit RGB PNG at `path`, never over a file without `overwrite`.
fn write_png(screen: &Screen, path: &std::path::Path, overwrite: bool) -> Result<(), CommandRefusal> {
    if path.exists() && !overwrite {
        return Err(CommandRefusal::new("overwrite_required", json!({"path": path.to_string_lossy()})));
    }
    let samples = hsk_studio_interop_raster::Samples::U8(screen.rgb.clone());
    let request = hsk_studio_interop_raster::PngEncodeRequest {
        width: screen.width,
        height: screen.height,
        colour: hsk_studio_interop_raster::Colour::Rgb,
        samples: &samples,
        icc: None,
        compression: hsk_studio_interop_raster::PngCompression::Balanced,
    };
    let png = hsk_studio_interop_raster::encode_png(&request).map_err(|e| CommandRefusal::new(e.code(), json!({"stage": "png"})))?;
    super::super::export::write_atomic(path, &png).map_err(|code| CommandRefusal::new(code, json!({"path": path.to_string_lossy()})))
}

fn trigger(f: &Fields<'_>) -> Result<PlayerInput, CommandRefusal> {
    let node = f.text("node")?;
    Ok(match f.text("trigger")?.as_str() {
        "click" => PlayerInput::Click(node),
        "hover_enter" => PlayerInput::HoverEnter(node),
        "hover_leave" => PlayerInput::HoverLeave(node),
        "mouse_down" => PlayerInput::MouseDown(node),
        "mouse_up" => PlayerInput::MouseUp(node),
        "press" => PlayerInput::Press(node),
        _ => return Err(invalid("trigger", "click|hover_enter|hover_leave|mouse_down|mouse_up|press")),
    })
}

/// One `prototype.*` row (module docs).
pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>) -> Result<Value, CommandRefusal> {
    if id == "prototype.start_session" {
        let bytes = read_bounded(&f.path("path")?, crate::import::MAX_SOURCE_BYTES)?;
        v.open_prototype(&bytes).map_err(refusal)?;
    }
    if v.prototype.is_none() {
        return Err(CommandRefusal::new("prototype_closed", json!({"hint": "prototype.start_session first"})));
    }
    if id == "prototype.end_session" {
        v.prototype = None;
        return Ok(json!({"open": false}));
    }
    let play = v.prototype().ok_or_else(|| CommandRefusal::new("prototype_closed", Value::Null))?;
    let mut reply = match id {
        "prototype.start_session" | "prototype.state" => state(play),
        "prototype.dispatch_trigger" => {
            let input = trigger(&f)?;
            play.dispatch_trigger(&input);
            state(play)
        }
        "prototype.click" => {
            let hit = play.click(f.number("x")?, f.number("y")?);
            let mut r = state(play);
            r["hit"] = json!(hit);
            r
        }
        "prototype.hover" => {
            play.hover(f.number("x")?, f.number("y")?);
            state(play)
        }
        "prototype.advance" => {
            let ms = f.unsigned("ms")?;
            if ms > MAX_ADVANCE_MS {
                return Err(invalid("ms", "0..=3600000"));
            }
            play.advance(ms);
            state(play)
        }
        "prototype.restart" => {
            play.restart();
            state(play)
        }
        "prototype.read_runtime_variables" => {
            let s = play.session().state();
            let mut r = state(play);
            r["variables"] = serde_json::to_value(&s.variables).unwrap_or(Value::Null);
            r["variants"] = json!(s.variants);
            r
        }
        "prototype.capture_frame" => {
            // `overwrite` applies to a written file only: refused without a path (review F7 on 2bbe2207).
            if f.has("overwrite") && !f.has("path") {
                return Err(CommandRefusal::new("parameter_conflict", json!({"keys": ["overwrite", "path"]})));
            }
            let screen = play.render().map_err(refusal)?;
            if f.has("path") {
                write_png(&screen, &f.path("path")?, f.flag_or("overwrite", false)?)?;
            }
            let mut r = state(play);
            r["screen"] = screen_json(&screen);
            r
        }
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    };
    reply["open"] = json!(true);
    Ok(reply)
}
