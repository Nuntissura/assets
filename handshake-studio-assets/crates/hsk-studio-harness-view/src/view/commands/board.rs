//! `board.*` (lane SMB-BOARD-1; [STU-WB-134]): decoders into `BoardCommand`, run by `Viewer::board_command`
//! (one undo step per mutating row). Closed tokens are decoded against their Folio lists; an endpoint is a
//! Folio `StudioConnectorEndpoint` record whose magnet token is checked first, so an unknown magnet is the
//! spec's `connector_endpoint_magnet_unknown` ([STU-WB-143]), not a generic invalid parameter.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::board::BoardCommand;
use super::decode::{Fields, invalid};
use super::{CommandRefusal, engine_refusal};
use hsk_studio_folio::{StudioConnectorCap, StudioConnectorEndpoint, StudioConnectorLineType, StudioDevStatus, StudioShapeType};
use serde_json::{Value, json};

const LINE_TYPES: &[(&str, StudioConnectorLineType)] = &[("STRAIGHT", StudioConnectorLineType::Straight), ("ELBOWED", StudioConnectorLineType::Elbowed), ("CURVED", StudioConnectorLineType::Curved)];
const CAPS: &[(&str, StudioConnectorCap)] = &[
    ("NONE", StudioConnectorCap::None),
    ("ARROW_EQUILATERAL", StudioConnectorCap::ArrowEquilateral),
    ("ARROW_LINES", StudioConnectorCap::ArrowLines),
    ("TRIANGLE_FILLED", StudioConnectorCap::TriangleFilled),
    ("CIRCLE_FILLED", StudioConnectorCap::CircleFilled),
    ("DIAMOND_FILLED", StudioConnectorCap::DiamondFilled),
    ("ERD_ONE", StudioConnectorCap::ErdOne),
    ("ERD_MANY", StudioConnectorCap::ErdMany),
    ("ERD_ONE_OR_MORE", StudioConnectorCap::ErdOneOrMore),
    ("ERD_ZERO_OR_ONE", StudioConnectorCap::ErdZeroOrOne),
    ("ERD_ZERO_OR_MORE", StudioConnectorCap::ErdZeroOrMore),
    ("ERD_EXACTLY_ONE", StudioConnectorCap::ErdExactlyOne),
];
const MAGNETS: [&str; 6] = ["AUTO", "TOP", "RIGHT", "BOTTOM", "LEFT", "CENTER"];
const ENDPOINT: &str = "{position: {x, y}} | {endpoint_node_id, magnet} | {position: {x, y}, endpoint_node_id}";

/// An endpoint record: an unknown magnet token is `connector_endpoint_magnet_unknown`.
fn endpoint(f: &Fields<'_>, key: &str) -> Result<StudioConnectorEndpoint, CommandRefusal> {
    let value = f.raw(key, ENDPOINT)?;
    if let Some(m) = value.get("magnet")
        && !m.as_str().is_some_and(|m| MAGNETS.contains(&m))
    {
        return Err(CommandRefusal::new("connector_endpoint_magnet_unknown", json!({"key": key, "magnet": m, "expected": MAGNETS})));
    }
    serde_json::from_value::<StudioConnectorEndpoint>(value).map_err(|_| invalid(key, ENDPOINT))
}

fn opt_endpoint(f: &Fields<'_>, key: &str) -> Result<Option<StudioConnectorEndpoint>, CommandRefusal> {
    if f.has(key) { endpoint(f, key).map(Some) } else { Ok(None) }
}

fn opt_token<T: Copy>(f: &Fields<'_>, key: &str, table: &[(&str, T)]) -> Result<Option<T>, CommandRefusal> {
    if f.has(key) { f.token(key, table).map(Some) } else { Ok(None) }
}

/// `<vocabulary>:<token>` ([STU-WB-105]): `shape_type_unknown` outside the closed vocabularies.
fn shape_type(f: &Fields<'_>, key: &str) -> Result<StudioShapeType, CommandRefusal> {
    let text = f.text(key)?;
    StudioShapeType::parse(&text).ok_or_else(|| CommandRefusal::new("shape_type_unknown", json!({"key": key, "value": text, "expected": "studio_declared:<one of 12 native shapes> | figma_plugin_api@1.141.0:<one of 30 shapeType tokens>"})))
}

fn opt_flag(f: &Fields<'_>, key: &str) -> Result<Option<bool>, CommandRefusal> {
    if f.has(key) { f.flag(key).map(Some) } else { Ok(None) }
}

fn opt_pair(f: &Fields<'_>, a: &str, b: &str) -> Result<Option<[f64; 2]>, CommandRefusal> {
    match (f.opt_number(a)?, f.opt_number(b)?) {
        (Some(x), Some(y)) => Ok(Some([x, y])),
        (None, None) => Ok(None),
        _ => Err(CommandRefusal::new("parameter_missing", json!({"key": if f.has(a) { b } else { a }}))),
    }
}

fn command_of(id: &str, f: &Fields<'_>) -> Result<BoardCommand, CommandRefusal> {
    Ok(match id {
        "board.add" => BoardCommand::Add { name: f.opt_text("name")?, size: opt_pair(f, "width", "height")?, at: opt_pair(f, "x", "y")? },
        "board.read" => BoardCommand::Read { board: f.opt_text("board")? },
        "board.node.read" => BoardCommand::NodeRead { node: f.text("node")? },
        "board.sticky.create" => BoardCommand::CreateSticky {
            board: f.opt_text("board")?,
            at: [f.number("x")?, f.number("y")?],
            text: f.text("text")?,
            author_visible: f.flag_or("author_visible", true)?,
            author_name: f.opt_text("author_name")?,
            is_wide_width: f.flag_or("is_wide_width", false)?,
            fill: f.opt_rgb8("fill")?,
        },
        "board.sticky.set" => BoardCommand::SetSticky { node: f.text("node")?, author_visible: opt_flag(f, "author_visible")?, author_name: f.opt_text("author_name")?, is_wide_width: opt_flag(f, "is_wide_width")? },
        "board.shape_with_text.create" => BoardCommand::CreateShape {
            board: f.opt_text("board")?,
            at: [f.number("x")?, f.number("y")?],
            shape_type: shape_type(f, "shape_type")?,
            text: f.text("text")?,
            size: [f.opt_number("width")?, f.opt_number("height")?],
            corner_radius: f.opt_number("corner_radius")?,
            fill: f.opt_rgb8("fill")?,
        },
        "board.shape_with_text.set" => BoardCommand::SetShape { node: f.text("node")?, shape_type: if f.has("shape_type") { Some(shape_type(f, "shape_type")?) } else { None }, corner_radius: f.opt_number("corner_radius")? },
        "board.section.create" => BoardCommand::CreateSection {
            board: f.opt_text("board")?,
            rect: [f.number("x")?, f.number("y")?, f.number("width")?, f.number("height")?],
            name: f.text("name")?,
            corner_radius: f.opt_number("corner_radius")?,
            fill: f.opt_rgb8("fill")?,
        },
        "board.section.set" => BoardCommand::SetSection {
            node: f.text("node")?,
            name: f.opt_text("name")?,
            contents_hidden: opt_flag(f, "section_contents_hidden")?,
            dev_status: opt_token(f, "dev_status", &[("READY_FOR_DEV", Some(StudioDevStatus::ReadyForDev)), ("COMPLETED", Some(StudioDevStatus::Completed)), ("NONE", None)])?,
        },
        "board.connector.create" => BoardCommand::CreateConnector {
            board: f.opt_text("board")?,
            start: endpoint(f, "start")?,
            end: endpoint(f, "end")?,
            line_type: opt_token(f, "connector_line_type", LINE_TYPES)?.unwrap_or_default(),
            caps: [opt_token(f, "connector_start_stroke_cap", CAPS)?.unwrap_or_default(), opt_token(f, "connector_end_stroke_cap", CAPS)?.unwrap_or(StudioConnectorCap::ArrowEquilateral)],
            stroke: f.opt_rgb8("stroke")?,
            width: f.opt_number("stroke_width")?,
        },
        "board.connector.set" => BoardCommand::SetConnector {
            node: f.text("node")?,
            start: opt_endpoint(f, "start")?,
            end: opt_endpoint(f, "end")?,
            line_type: opt_token(f, "connector_line_type", LINE_TYPES)?,
            caps: [opt_token(f, "connector_start_stroke_cap", CAPS)?, opt_token(f, "connector_end_stroke_cap", CAPS)?],
        },
        "board.node.set_text" => BoardCommand::SetText { node: f.text("node")?, text: f.text("text")? },
        "board.node.set_fill" => BoardCommand::SetFill { node: f.text("node")?, fill: f.rgb8("fill")? },
        "board.node.move" => BoardCommand::Move { node: f.text("node")?, to: [f.number("x")?, f.number("y")?] },
        "board.node.delete" => BoardCommand::Delete { node: f.text("node")? },
        "board.export_png" => BoardCommand::ExportPng {
            board: f.opt_text("board")?,
            path: f.path("path")?,
            overwrite: f.flag_or("overwrite", false)?,
            content_bounds: opt_token(f, "bounds", &[("content", true), ("board", false)])?.unwrap_or(true),
        },
        other => return Err(CommandRefusal::new("command_not_driveable", json!({"command_id": other}))),
    })
}

/// One row of this file (module docs).
pub(super) fn run(v: &mut Viewer, id: &str, f: Fields<'_>, before: usize) -> Result<Value, CommandRefusal> {
    let command = command_of(id, &f)?;
    v.board_command(command).map_err(|e| engine_refusal(v, before, e))
}
