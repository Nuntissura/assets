//! Whiteboard mode on the viewer (lane SMB-BOARD-1; [STU-WB-001], [STU-WB-002], [STU-WB-102] to
//! [STU-WB-108], [STU-WB-115], [STU-WB-120], [STU-WB-127], [STU-WB-134]): the typed board commands
//! ([`BoardCommand`], ids `board.*`), egui-free and drawing nothing on screen. Every mutating command is ONE
//! undo step; reads change nothing.
//!
//! Model (Folio `board.rs`): whiteboard mode is written once, when `document.new` or `new --profile board`
//! creates the document (harness-core, the artboard options entry: `Run::whiteboard_mode`; review F4 of
//! e4d033aa, lead ruling option B); a document without it never becomes a whiteboard, and a board command on it
//! is refused `not_whiteboard_document`. A board is an artboard of a whiteboard document (`board.add` is
//! `artboard.add`). A board node is a layer of kind `sticky`, `shape_with_text`, `section` or `connector` whose
//! `Payload::Vector` paths are its drawing (composite `board`): the outline first (a shape's interior lines
//! second), with its additional fields in the vector records' `board_nodes` and its text an area-text story bound
//! inside its outline. A redraw, a move and a reroute replace the node's path records in place (review F2):
//! the step's entry snapshot keeps the old geometry, so nothing grows with edits.
//!
//! Connectors ([STU-WB-108]): endpoints resolve against the current bounds of their nodes; a board move, an
//! arrange transform (`Viewer::board_after_transform`) and a resize reroute every attached connector in the same
//! step. A read reports the route resolved now (`route_current`: the drawing matches it). A connector whose
//! endpoint node is gone is `broken` (it keeps its last route and drawing; `connector_endpoint_node_unresolvable`,
//! warning, [STU-WB-142]); one whose ends meet is `unroutable` (`connector_route_degenerate`, warning; it keeps
//! its drawing and never blocks a move, review F5) until its nodes separate.
//!
//! Generic layer operations (review F1, F6): a layer delete drops a node's record and text binding in its own
//! step (`Viewer::board_records_after_delete`); pathfinder, Create Outlines and Outline Stroke refuse a board node
//! `board_node_not_vector_editable`; align, distribute and transform keep the node's text on its outline and
//! reroute its connectors.
//!
//! Studio readings, declared (FigJam behaviour UNVERIFIED, D5): sticky sizes (240 square, 480 wide, taller for
//! more lines), shape sizes from the text width, shape text centred, sticky and section text at the top, a
//! section's members are the stickies and shapes whose centre lies in it and move with it (nested sections stay),
//! a section's name is its label story (the layer keeps its creation name), `section_contents_hidden` is stored
//! and read but not drawn, `CURVED` and six Plugin API shapes refused typed, a connector does not move by itself
//! and keeps its creation colour and width, no resize command (a shape's size follows its text upward),
//! `author_name` defaults to the actor's account id. Out of scope: stamps, highlights, washi tape, tables, code
//! blocks, media, link unfurls, widgets, facilitation, templates, the board GUI (OD-4), PDF and CSV export.
#![deny(clippy::arithmetic_side_effects)]
use super::Viewer;
use super::text;
use super::vector::{VectorEntry, VectorStyle};
use hsk_studio_composite::board::{self as geo, BoardEnd, BoardRect};
use hsk_studio_folio::{
    self as folio, Payload, StudioBoardNode, StudioBoardNodeFields, StudioBoardPoint, StudioConnectorCap, StudioConnectorEndpoint, StudioConnectorLineType, StudioDevStatus, StudioDocument,
    StudioShapeType,
};
use hsk_studio_nib::anchor_path::AnchorPath;
use hsk_studio_nib::{OwnedPath, PathPoint};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Creating, editing, moving, deleting and exporting.
mod write;

/// Side of a square sticky and of the wide variant's height (document units).
pub const STICKY_SIDE: f64 = 240.0;
/// Width of a wide sticky.
pub const STICKY_WIDE: f64 = 480.0;
/// Text inset of a sticky (points = document units).
pub const TEXT_INSET: f64 = 16.0;
/// Text inset of a shape: small, because an inset is taken square to a slanted edge too.
pub const SHAPE_TEXT_INSET: f64 = 8.0;
/// Text size of sticky and shape text (points).
pub const TEXT_PT: f64 = 18.0;
/// Text size of a section label (points).
pub const LABEL_PT: f64 = 14.0;
/// Default shape size when its text is short.
pub const SHAPE_MIN: [f64; 2] = [160.0, 80.0];
/// Largest node side (document units).
pub const MAX_NODE_SIDE: f64 = 100_000.0;
/// Margin around the content of a board export (document units).
pub const EXPORT_MARGIN: f64 = 32.0;
/// Default size of a new board (document units).
pub const BOARD_DEFAULT_SIZE: [f64; 2] = [1920.0, 1080.0];
/// Most lines of one sticky or shape text (Studio-declared).
pub const MAX_TEXT_LINES: usize = 64;
const STICKY_FILL: [u8; 3] = [255, 235, 153];
const SHAPE_FILL: [u8; 3] = [196, 224, 255];
const SHAPE_STROKE: [u8; 3] = [51, 51, 51];
const SECTION_FILL: [u8; 3] = [242, 242, 242];
const SECTION_STROKE: [u8; 3] = [170, 170, 170];
const CONNECTOR_COLOUR: [u8; 3] = [30, 30, 30];
const CONNECTOR_WIDTH: f64 = 2.0;
/// Longest route a connector record keeps.
const MAX_ROUTE_POINTS: usize = 64;

/// A typed board command ([STU-WB-134]).
#[derive(Clone, Debug, PartialEq)]
pub enum BoardCommand {
    /// `board.add`: a new board (an artboard of a whiteboard document).
    Add { name: Option<String>, size: Option<[f64; 2]>, at: Option<[f64; 2]> },
    /// `board.read`: the boards and the nodes (of `board`, or all).
    Read { board: Option<String> },
    /// `board.node.read`.
    NodeRead { node: String },
    /// `board.sticky.create` ([STU-WB-104]).
    CreateSticky { board: Option<String>, at: [f64; 2], text: String, author_visible: bool, author_name: Option<String>, is_wide_width: bool, fill: Option<[u8; 3]> },
    /// `board.sticky.set`.
    SetSticky { node: String, author_visible: Option<bool>, author_name: Option<String>, is_wide_width: Option<bool> },
    /// `board.shape_with_text.create` ([STU-WB-105]).
    CreateShape { board: Option<String>, at: [f64; 2], shape_type: StudioShapeType, text: String, size: [Option<f64>; 2], corner_radius: Option<f64>, fill: Option<[u8; 3]> },
    /// `board.shape_with_text.set`.
    SetShape { node: String, shape_type: Option<StudioShapeType>, corner_radius: Option<f64> },
    /// `board.section.create` ([STU-WB-115]).
    CreateSection { board: Option<String>, rect: [f64; 4], name: String, corner_radius: Option<f64>, fill: Option<[u8; 3]> },
    /// `board.section.set` (`dev_status` `Some(None)` clears it).
    SetSection { node: String, name: Option<String>, contents_hidden: Option<bool>, dev_status: Option<Option<StudioDevStatus>> },
    /// `board.connector.create` ([STU-WB-106] to [STU-WB-109]).
    CreateConnector { board: Option<String>, start: StudioConnectorEndpoint, end: StudioConnectorEndpoint, line_type: StudioConnectorLineType, caps: [StudioConnectorCap; 2], stroke: Option<[u8; 3]>, width: Option<f64> },
    /// `board.connector.set`.
    SetConnector { node: String, start: Option<StudioConnectorEndpoint>, end: Option<StudioConnectorEndpoint>, line_type: Option<StudioConnectorLineType>, caps: [Option<StudioConnectorCap>; 2] },
    /// `board.node.set_text`.
    SetText { node: String, text: String },
    /// `board.node.set_fill`.
    SetFill { node: String, fill: [u8; 3] },
    /// `board.node.move`: the node's top-left to `to`.
    Move { node: String, to: [f64; 2] },
    /// `board.node.delete`.
    Delete { node: String },
    /// `board.export_png` ([STU-WB-127]): the board region to PNG bytes on disk.
    ExportPng { board: Option<String>, path: PathBuf, overwrite: bool, content_bounds: bool },
}

impl BoardCommand {
    /// The command id of the row.
    pub const fn id(&self) -> &'static str {
        match self {
            Self::Add { .. } => "board.add",
            Self::Read { .. } => "board.read",
            Self::NodeRead { .. } => "board.node.read",
            Self::CreateSticky { .. } => "board.sticky.create",
            Self::SetSticky { .. } => "board.sticky.set",
            Self::CreateShape { .. } => "board.shape_with_text.create",
            Self::SetShape { .. } => "board.shape_with_text.set",
            Self::CreateSection { .. } => "board.section.create",
            Self::SetSection { .. } => "board.section.set",
            Self::CreateConnector { .. } => "board.connector.create",
            Self::SetConnector { .. } => "board.connector.set",
            Self::SetText { .. } => "board.node.set_text",
            Self::SetFill { .. } => "board.node.set_fill",
            Self::Move { .. } => "board.node.move",
            Self::Delete { .. } => "board.node.delete",
            Self::ExportPng { .. } => "board.export_png",
        }
    }
}

fn refused(code: &str, why: impl std::fmt::Display) -> String {
    format!("board: {code} ({why})")
}

fn rect_json(r: &BoardRect) -> Value {
    json!([r.x, r.y, r.w, r.h])
}

fn rgb_json(c: Option<[u8; 3]>) -> Value {
    c.map_or(Value::Null, |c| json!(c))
}

fn finite_coordinate(v: f64) -> bool {
    v.is_finite() && v.abs() <= folio::BOARD_COORDINATE_LIMIT
}

fn side_ok(v: f64) -> bool {
    v.is_finite() && v > 0.0 && v <= MAX_NODE_SIDE
}

/// The stored form of a board text: each `\n` becomes U+2028, Type's forced line break, which the live
/// area-text path lays out as a new line of the same paragraph (a line feed is a paragraph separator there, which
/// the viewer's shaping refuses; Studio reading of review F12). Commands and reads speak `\n`.
pub(super) fn story_form(t: &str) -> String {
    t.replace('\n', "\u{2028}")
}

/// The command form of a stored board text ([`story_form`] reversed).
pub(super) fn command_form(s: &str) -> String {
    s.replace('\u{2028}', "\n")
}

/// 1..=4096 stored bytes of story text: one line, or with `lines` at most [`MAX_TEXT_LINES`] lines split at
/// `\n` (a sticky's or a shape's text, review F12); no other control character.
fn text_ok(t: &str, lines: bool) -> Result<(), String> {
    let breaks = t.matches('\n').count();
    let other = t.chars().any(|c| c.is_control() && c != '\n') || t.contains(['\u{2028}', '\u{2029}']);
    // U+2028 takes three bytes where `\n` takes one.
    let stored = t.len().saturating_add(breaks.saturating_mul(2));
    if t.is_empty() || stored > text::MAX_STORY_BYTES || other || (breaks > 0 && !lines) || breaks >= MAX_TEXT_LINES {
        let shape = if lines { format!("at most {MAX_TEXT_LINES} lines") } else { "one line".to_owned() };
        return Err(refused("board_text_invalid", format!("{shape} of 1..={} bytes", text::MAX_STORY_BYTES)));
    }
    Ok(())
}

fn uuid_parts(document_id: &str) -> (String, String) {
    let uuid = document_id.split_once('-').map_or("", |(_, u)| u);
    (uuid.get(0..8).unwrap_or("00000000").to_owned(), uuid.get(9..13).unwrap_or("0000").to_owned())
}

/// A fresh `SVPT` id the entry does not hold (the vector tools' scheme).
fn mint_path(doc: &StudioDocument, entry: &VectorEntry) -> String {
    let (high, low) = uuid_parts(&doc.document_id);
    let mut n = u64::try_from(entry.paths.len()).unwrap_or(0);
    loop {
        let id = format!("SVPT-{high}-{low}-7ede-a000-{n:012x}");
        if entry.path(&id).is_none() {
            return id;
        }
        n = n.wrapping_add(1);
    }
}

/// The union of the exact bounds of `paths`.
fn bounds_of_paths(entry: &VectorEntry, paths: &[String]) -> Option<BoardRect> {
    let mut acc: Option<[f64; 4]> = None;
    for id in paths {
        let owned = entry.path(id)?.path.to_owned_path().ok()?;
        let b = hsk_studio_nib::arrange::path_bounds(&owned)?;
        acc = Some(match acc {
            None => [b.min[0], b.min[1], b.max[0], b.max[1]],
            Some(a) => [a[0].min(b.min[0]), a[1].min(b.min[1]), a[2].max(b.max[0]), a[3].max(b.max[1])],
        });
    }
    acc.map(|a| BoardRect { x: a[0], y: a[1], w: a[2] - a[0], h: a[3] - a[1] })
}

fn anchor_path(p: &OwnedPath) -> Result<AnchorPath, String> {
    AnchorPath::from_owned_path(p).map_err(|e| refused("board_geometry_invalid", e.code()))
}

fn translated(p: &AnchorPath, d: [f64; 2]) -> AnchorPath {
    let mut out = p.clone();
    for c in &mut out.contours {
        for a in &mut c.anchors {
            a.position = PathPoint::new(a.position.x + d[0], a.position.y + d[1]);
        }
    }
    out
}

fn geo_refusal(e: geo::BoardGeometryError) -> String {
    refused(e.code(), "board drawing")
}

/// The document's vector layers' paths, with `changes` applied (layer -> path ids).
fn project(doc: &StudioDocument, changes: &[(String, Vec<String>)], created: &[folio::StudioLayer], deleted: &[String]) -> StudioDocument {
    let mut d = doc.clone();
    d.layers.retain(|l| !deleted.contains(&l.layer_id));
    d.layers.extend(created.iter().cloned());
    for (id, paths) in changes {
        if let Some(l) = d.layers.iter_mut().find(|l| &l.layer_id == id) {
            l.payload = Payload::Vector { path_ids: paths.clone() };
        }
    }
    d
}

/// A route as Folio points.
fn route_points(r: &[[f64; 2]]) -> Vec<StudioBoardPoint> {
    r.iter().map(|p| StudioBoardPoint { x: p[0], y: p[1] }).collect()
}

/// Why a connector does not resolve now.
enum Unresolved {
    /// Endpoints whose node the document no longer holds: `(endpoint, node id)`.
    Broken(Vec<(&'static str, String)>),
    /// Both ends meet: the route is one point (review F5).
    Degenerate,
    Geometry(String),
}

/// The editing context: the document, the vector entry and its board records, and whiteboard mode.
struct Ctx {
    doc: StudioDocument,
    entry: VectorEntry,
    whiteboard: bool,
}

/// Bounds of every vector layer of `doc` (computed once per command; review F9), with `moved` offsets.
fn rects_of(doc: &StudioDocument, entry: &VectorEntry, moved: &BTreeMap<String, [f64; 2]>) -> BTreeMap<String, BoardRect> {
    let mut out = BTreeMap::new();
    for l in &doc.layers {
        if let Payload::Vector { path_ids } = &l.payload
            && let Some(mut r) = bounds_of_paths(entry, path_ids)
        {
            if let Some(d) = moved.get(&l.layer_id) {
                r.x += d[0];
                r.y += d[1];
            }
            out.insert(l.layer_id.clone(), r);
        }
    }
    out
}

impl Ctx {
    fn record(&self, node: &str) -> Option<&StudioBoardNode> {
        self.entry.records.board_nodes.iter().find(|n| n.layer_id == node)
    }

    fn layer_paths(&self, id: &str) -> Option<Vec<String>> {
        match &self.doc.layers.iter().find(|l| l.layer_id == id)?.payload {
            Payload::Vector { path_ids } => Some(path_ids.clone()),
            _ => None,
        }
    }

    /// A live board node: its record (the layer is present and of the record's kind).
    fn node(&self, node: &str) -> Result<StudioBoardNode, String> {
        let present = self.doc.layers.iter().any(|l| l.layer_id == node);
        match self.record(node) {
            Some(r) if present => Ok(r.clone()),
            _ if present => Err(refused("not_a_board_node", node)),
            _ => Err(refused("board_node_unknown", node)),
        }
    }

    fn rect(&self, id: &str) -> Option<BoardRect> {
        bounds_of_paths(&self.entry, &self.layer_paths(id)?)
    }

    fn style(&self, id: &str) -> Option<VectorStyle> {
        let first = self.layer_paths(id)?.into_iter().next()?;
        Some(self.entry.path(&first)?.style)
    }

    fn text_id(&self, id: &str) -> Option<String> {
        self.entry.records.area_texts.iter().find(|a| a.container_layer_id == id).map(|a| a.text_id.clone())
    }

    fn rects(&self, moved: &BTreeMap<String, [f64; 2]>) -> BTreeMap<String, BoardRect> {
        rects_of(&self.doc, &self.entry, moved)
    }

    /// `not_whiteboard_document` unless the document is a whiteboard (review F4).
    fn whiteboard_only(&self) -> Result<(), String> {
        if self.whiteboard { Ok(()) } else { Err(refused("not_whiteboard_document", "create the document with profile board")) }
    }
}

/// The reference point of an endpoint (AUTO aims at the other end's).
fn reference(e: &StudioConnectorEndpoint, rects: &BTreeMap<String, BoardRect>) -> Option<[f64; 2]> {
    match e {
        StudioConnectorEndpoint::Position(p) => Some([p.position.x, p.position.y]),
        StudioConnectorEndpoint::Magnet(m) => rects.get(&m.endpoint_node_id).map(BoardRect::center),
        StudioConnectorEndpoint::PositionAndNode(p) => rects.get(&p.endpoint_node_id).map(|r| [r.x + p.position.x, r.y + p.position.y]),
    }
}

fn resolve(e: &StudioConnectorEndpoint, toward: [f64; 2], rects: &BTreeMap<String, BoardRect>) -> Option<BoardEnd> {
    match e {
        StudioConnectorEndpoint::Position(p) => Some(BoardEnd { point: [p.position.x, p.position.y], side: None }),
        StudioConnectorEndpoint::Magnet(m) => rects.get(&m.endpoint_node_id).map(|r| geo::anchor(r, m.magnet, toward)),
        StudioConnectorEndpoint::PositionAndNode(p) => rects.get(&p.endpoint_node_id).map(|r| BoardEnd { point: [r.x + p.position.x, r.y + p.position.y], side: None }),
    }
}

/// The route of a connector now.
fn connector_route(start: &StudioConnectorEndpoint, end: &StudioConnectorEndpoint, line: StudioConnectorLineType, rects: &BTreeMap<String, BoardRect>) -> Result<Vec<[f64; 2]>, Unresolved> {
    let mut broken = Vec::new();
    for (name, e) in [("start", start), ("end", end)] {
        if let Some(node) = e.node()
            && !rects.contains_key(node)
        {
            broken.push((name, node.to_owned()));
        }
    }
    if !broken.is_empty() {
        return Err(Unresolved::Broken(broken));
    }
    let (ra, rb) = (reference(start, rects), reference(end, rects));
    let (Some(ra), Some(rb)) = (ra, rb) else {
        return Err(Unresolved::Geometry("endpoint".into()));
    };
    let (Some(a), Some(b)) = (resolve(start, rb, rects), resolve(end, ra, rects)) else {
        return Err(Unresolved::Geometry("endpoint".into()));
    };
    let route = geo::route(a, b, line).map_err(|e| Unresolved::Geometry(e.code().into()))?;
    if route.len() < 2 {
        return Err(Unresolved::Degenerate);
    }
    if route.len() > MAX_ROUTE_POINTS {
        return Err(Unresolved::Geometry("route_too_long".into()));
    }
    Ok(route)
}

/// The size a node of `fields` takes for its text, given each line's advance (points).
fn fitted_size(fields: &StudioBoardNodeFields, line_widths: &[f64], asked: [Option<f64>; 2]) -> [f64; 2] {
    let line = TEXT_PT * 1.3;
    let widest = line_widths.iter().copied().fold(0.0, f64::max);
    let count = f64::from(u32::try_from(line_widths.len().max(1)).unwrap_or(u32::MAX));
    match fields {
        StudioBoardNodeFields::Sticky { is_wide_width, .. } => {
            let w = if *is_wide_width { STICKY_WIDE } else { STICKY_SIDE };
            let lines: f64 = line_widths.iter().map(|lw| (lw / (w - 2.0 * TEXT_INSET)).ceil().max(1.0)).sum::<f64>().max(1.0);
            [w, (lines * line + 2.0 * TEXT_INSET).max(STICKY_SIDE)]
        }
        StudioBoardNodeFields::ShapeWithText { shape_type, .. } => {
            // 5 % and 4 points of slack over the widest line's advance: the area layout breaks a line that
            // fits only exactly.
            let fit = geo::text_fit(shape_type);
            [asked[0].unwrap_or(SHAPE_MIN[0]).max((widest * 1.05 + 4.0 + 2.0 * SHAPE_TEXT_INSET) * fit), asked[1].unwrap_or(SHAPE_MIN[1]).max((count * line + 2.0 * SHAPE_TEXT_INSET) * fit)]
        }
        _ => [asked[0].unwrap_or(SHAPE_MIN[0]), asked[1].unwrap_or(SHAPE_MIN[1])],
    }
}

/// The outline of a node of `fields` in `rect`.
fn outline(fields: &StudioBoardNodeFields, rect: &BoardRect) -> Result<OwnedPath, String> {
    match fields {
        StudioBoardNodeFields::Sticky { .. } => geo::card_outline(rect, 2.0),
        StudioBoardNodeFields::ShapeWithText { shape_type, corner_radius } => geo::shape_outline(shape_type, rect, *corner_radius),
        StudioBoardNodeFields::Section { corner_radius, .. } => geo::card_outline(rect, corner_radius.unwrap_or(8.0)),
        StudioBoardNodeFields::Connector { .. } => Err(geo::BoardGeometryError::Invalid),
    }
    .map_err(geo_refusal)
}

/// Where a node's text starts: a shape centres its lines vertically (its outline is widest near the middle:
/// a diamond's or a triangle's top holds no line), a sticky and a section start at the top (Studio reading).
fn text_baseline(fields: &StudioBoardNodeFields, rect: &BoardRect, lines: usize) -> (folio::StudioFirstBaseline, f64) {
    match fields {
        StudioBoardNodeFields::ShapeWithText { .. } => {
            let block = TEXT_PT * 1.3 * f64::from(u32::try_from(lines.max(1)).unwrap_or(u32::MAX));
            (folio::StudioFirstBaseline::Fixed, ((rect.h - 2.0 * SHAPE_TEXT_INSET - block) / 2.0).max(0.0) + TEXT_PT * 0.8)
        }
        _ => (folio::StudioFirstBaseline::Ascent, 0.0),
    }
}

/// The interior lines a node of `fields` draws over its outline (a shape's details), if any.
fn details(fields: &StudioBoardNodeFields, rect: &BoardRect) -> Result<Option<AnchorPath>, String> {
    match fields {
        StudioBoardNodeFields::ShapeWithText { shape_type, .. } => geo::shape_details(shape_type, rect).as_ref().map(anchor_path).transpose(),
        _ => Ok(None),
    }
}

impl Viewer {
    /// Runs one board command (module docs). Returns the command's result.
    pub fn board_command(&mut self, command: BoardCommand) -> Result<Value, String> {
        let id = command.id();
        let r = self.board_inner(command);
        match &r {
            Ok(_) => self.say(format!("{id} done")),
            Err(e) => self.say(format!("{id} refused: {e}")),
        }
        r
    }

    fn board_ctx(&self) -> Result<Ctx, String> {
        let doc = self.run.snapshot().ok_or_else(|| refused("nothing_open", "no document"))?.document().clone();
        let entry = self.vector_entry()?;
        let whiteboard = self.run.whiteboard_mode().map_err(|code| refused(code, "artboard options"))?;
        Ok(Ctx { doc, entry, whiteboard })
    }

    /// `canceled` when the run's token is (review F9).
    fn board_cancel_check(&self) -> Result<(), String> {
        self.cancel.check().map_err(|_| refused("canceled", "board"))
    }

    fn board_inner(&mut self, command: BoardCommand) -> Result<Value, String> {
        let op: &'static str = command.id();
        match command {
            BoardCommand::Add { name, size, at } => self.board_add(name, size, at),
            BoardCommand::Read { board } => self.board_read(board.as_deref()),
            BoardCommand::NodeRead { node } => {
                self.board_cancel_check()?;
                let c = self.board_ctx()?;
                let rec = c.node(&node)?;
                let rects = c.rects(&BTreeMap::new());
                let te = self.text_entry().ok();
                Ok(self.node_json(&c, &rec, &rects, &self.text_overset(), te.as_ref()))
            }
            BoardCommand::CreateSticky { board, at, text, author_visible, author_name, is_wide_width, fill } => {
                let author_name = author_name.unwrap_or_else(|| self.run.actor().account_id().to_owned());
                if author_name.len() > folio::MAX_AUTHOR_NAME_BYTES || author_name.chars().any(char::is_control) {
                    return Err(refused("author_name_invalid", format!("at most {} bytes, no control character", folio::MAX_AUTHOR_NAME_BYTES)));
                }
                let fields = StudioBoardNodeFields::Sticky { author_visible, author_name, is_wide_width };
                let style = VectorStyle { fill: Some(fill.unwrap_or(STICKY_FILL)), stroke: None, stroke_width: 0.0 };
                self.create_text_node(op, board.as_deref(), at, fields, &text, [None, None], style)
            }
            BoardCommand::CreateShape { board, at, shape_type, text, size, corner_radius, fill } => {
                if !size.iter().flatten().all(|v| side_ok(*v)) || corner_radius.is_some_and(|r| !(r.is_finite() && (0.0..=folio::MAX_BOARD_CORNER_RADIUS).contains(&r))) {
                    return Err(refused("board_size_invalid", "sides in (0, 100000], corner radius in [0, 100000]"));
                }
                let fields = StudioBoardNodeFields::ShapeWithText { shape_type, corner_radius };
                let style = VectorStyle { fill: Some(fill.unwrap_or(SHAPE_FILL)), stroke: Some(SHAPE_STROKE), stroke_width: 2.0 };
                self.create_text_node(op, board.as_deref(), at, fields, &text, size, style)
            }
            BoardCommand::CreateSection { board, rect, name, corner_radius, fill } => {
                if !(side_ok(rect[2]) && side_ok(rect[3])) || corner_radius.is_some_and(|r| !(r.is_finite() && (0.0..=folio::MAX_BOARD_CORNER_RADIUS).contains(&r))) {
                    return Err(refused("board_size_invalid", "sides in (0, 100000], corner radius in [0, 100000]"));
                }
                let fields = StudioBoardNodeFields::Section { section_contents_hidden: false, corner_radius, dev_status: None };
                let style = VectorStyle { fill: Some(fill.unwrap_or(SECTION_FILL)), stroke: Some(SECTION_STROKE), stroke_width: 1.0 };
                self.create_text_node(op, board.as_deref(), [rect[0], rect[1]], fields, &name, [Some(rect[2]), Some(rect[3])], style)
            }
            BoardCommand::CreateConnector { board, start, end, line_type, caps, stroke, width } => self.create_connector(op, board.as_deref(), start, end, line_type, caps, stroke, width),
            BoardCommand::SetSticky { node, author_visible, author_name, is_wide_width } => {
                if author_name.as_ref().is_some_and(|n| n.len() > folio::MAX_AUTHOR_NAME_BYTES || n.chars().any(char::is_control)) {
                    return Err(refused("author_name_invalid", format!("at most {} bytes, no control character", folio::MAX_AUTHOR_NAME_BYTES)));
                }
                self.edit_node(op, &node, |fields| match fields {
                    StudioBoardNodeFields::Sticky { author_visible: v, author_name: n, is_wide_width: w } => {
                        if let Some(x) = author_visible {
                            *v = x;
                        }
                        if let Some(x) = author_name {
                            *n = x;
                        }
                        if let Some(x) = is_wide_width {
                            *w = x;
                        }
                        Ok(())
                    }
                    _ => Err(refused("board_node_kind_mismatch", "not a sticky")),
                })
            }
            BoardCommand::SetShape { node, shape_type, corner_radius } => {
                if corner_radius.is_some_and(|r| !(r.is_finite() && (0.0..=folio::MAX_BOARD_CORNER_RADIUS).contains(&r))) {
                    return Err(refused("board_size_invalid", "corner radius in [0, 100000]"));
                }
                self.edit_node(op, &node, |fields| match fields {
                    StudioBoardNodeFields::ShapeWithText { shape_type: s, corner_radius: r } => {
                        if let Some(x) = shape_type {
                            *s = x;
                        }
                        if corner_radius.is_some() {
                            *r = corner_radius;
                        }
                        Ok(())
                    }
                    _ => Err(refused("board_node_kind_mismatch", "not a shape with text")),
                })
            }
            BoardCommand::SetSection { node, name, contents_hidden, dev_status } => {
                if let Some(n) = &name {
                    text_ok(n, false)?;
                }
                if !matches!(self.board_ctx()?.node(&node)?.node, StudioBoardNodeFields::Section { .. }) {
                    return Err(refused("board_node_kind_mismatch", "not a section"));
                }
                self.edit_node_text(op, &node, name.as_deref(), |fields| {
                    if let StudioBoardNodeFields::Section { section_contents_hidden, dev_status: d, .. } = fields {
                        if let Some(x) = contents_hidden {
                            *section_contents_hidden = x;
                        }
                        if let Some(x) = dev_status {
                            *d = x;
                        }
                    }
                    Ok(())
                })
            }
            BoardCommand::SetConnector { node, start, end, line_type, caps } => self.set_connector(op, &node, start, end, line_type, caps),
            BoardCommand::SetText { node, text } => {
                let section = match self.board_ctx()?.node(&node)?.node {
                    StudioBoardNodeFields::Connector { .. } => return Err(refused("board_node_kind_mismatch", "a connector has no text in this build")),
                    StudioBoardNodeFields::Section { .. } => true,
                    _ => false,
                };
                text_ok(&text, !section)?;
                self.edit_node_text(op, &node, Some(&text), |_| Ok(()))
            }
            BoardCommand::SetFill { node, fill } => {
                if matches!(self.board_ctx()?.node(&node)?.node, StudioBoardNodeFields::Connector { .. }) {
                    return Err(refused("board_node_kind_mismatch", "a connector's colour is its stroke"));
                }
                self.board_restyle(op, &node, fill)
            }
            BoardCommand::Move { node, to } => self.move_node(op, &node, to),
            BoardCommand::Delete { node } => self.delete_node(op, &node),
            BoardCommand::ExportPng { board, path, overwrite, content_bounds } => self.board_export_png(board.as_deref(), &path, overwrite, content_bounds),
        }
    }

    // ----- boards -------------------------------------------------------------------------------

    /// The board a create names, or the active (else first) artboard: `board_unknown`.
    fn target_board(&self, c: &Ctx, board: Option<&str>) -> Result<String, String> {
        match board {
            Some(b) if c.doc.artboards.iter().any(|a| a.artboard_id == b) => Ok(b.to_owned()),
            Some(b) => Err(refused("board_unknown", b)),
            None => self.active_artboard(&c.doc).map(|a| a.artboard_id.clone()).ok_or_else(|| refused("board_unknown", "the document holds no artboard")),
        }
    }

    fn board_add(&mut self, name: Option<String>, size: Option<[f64; 2]>, at: Option<[f64; 2]>) -> Result<Value, String> {
        let c = self.board_ctx()?;
        c.whiteboard_only()?;
        let name = name.unwrap_or_else(|| format!("Board {}", c.doc.artboards.len().saturating_add(1)));
        let [w, h] = size.or_else(|| c.doc.artboards.first().map(|a| [a.bounds.width.value, a.bounds.height.value])).unwrap_or(BOARD_DEFAULT_SIZE);
        let ok = self.run.add_artboard(&name, w, h, at);
        let revision = self.after_edit(ok, format!("board.add {name}"))?;
        let id = self.last_op()["detail"]["artboard_id"].as_str().unwrap_or_default().to_owned();
        Ok(json!({"board_id": id, "name": name, "revision": revision}))
    }

    /// `board.read` (review F9): the bounds of every vector layer and the overset of every story are taken
    /// once, and the token is checked before the read and per node.
    fn board_read(&self, board: Option<&str>) -> Result<Value, String> {
        self.board_cancel_check()?;
        let c = self.board_ctx()?;
        if let Some(b) = board
            && !c.doc.artboards.iter().any(|a| a.artboard_id == b)
        {
            return Err(refused("board_unknown", b));
        }
        let active = self.active_artboard(&c.doc).map(|a| a.artboard_id.clone());
        let present: std::collections::BTreeSet<&str> = c.doc.layers.iter().map(|l| l.layer_id.as_str()).collect();
        let boards: Vec<Value> = c
            .doc
            .artboards
            .iter()
            .filter(|_| c.whiteboard)
            .map(|a| {
                let count = c.entry.records.board_nodes.iter().filter(|n| n.board_id == a.artboard_id && present.contains(n.layer_id.as_str())).count();
                json!({"board_id": a.artboard_id, "name": a.name, "bounds": [a.bounds.x.value, a.bounds.y.value, a.bounds.width.value, a.bounds.height.value], "active": active.as_deref() == Some(a.artboard_id.as_str()), "nodes": count})
            })
            .collect();
        let rects = c.rects(&BTreeMap::new());
        let overset = self.text_overset();
        let te = self.text_entry().ok();
        let mut nodes: Vec<Value> = Vec::new();
        for n in c.entry.records.board_nodes.iter().filter(|n| board.is_none_or(|b| n.board_id == b) && present.contains(n.layer_id.as_str())) {
            self.board_cancel_check()?;
            nodes.push(self.node_json(&c, n, &rects, &overset, te.as_ref()));
        }
        let issues: Vec<Value> = nodes.iter().flat_map(|n| n["issues"].as_array().cloned().unwrap_or_default()).collect();
        Ok(json!({"whiteboard_mode": c.whiteboard, "boards": boards, "nodes": nodes, "issues": issues}))
    }

    /// Overset glyphs of every live story as the window draws it (story id -> count); a story the
    /// renderer could not lay out reads `None` (its code is in the vector text reports).
    fn text_overset(&self) -> BTreeMap<String, Option<usize>> {
        self.vector_text_reports().unwrap_or_default().into_iter().map(|s| (s.report.text_id.clone(), s.report.outcome.as_ref().ok().map(|d| d.overset_count))).collect()
    }

    /// The reading of one node (module docs); `rects` are the bounds of every vector layer now, `overset` the
    /// glyphs each story does not fit and `te` the text entry. A connector reads its stroke and no fill
    /// ([STU-WB-106]; its caps are filled in its stroke colour).
    fn node_json(&self, c: &Ctx, n: &StudioBoardNode, rects: &BTreeMap<String, BoardRect>, overset: &BTreeMap<String, Option<usize>>, te: Option<&text::TextEntry>) -> Value {
        let layer = c.doc.layers.iter().find(|l| l.layer_id == n.layer_id);
        let rect = rects.get(&n.layer_id);
        let style = c.style(&n.layer_id);
        let connector = matches!(n.node, StudioBoardNodeFields::Connector { .. });
        let text_id = c.text_id(&n.layer_id);
        let text = text_id.as_ref().and_then(|id| te?.stories.iter().find(|s| &s.story_id == id).and_then(text::story_text).map(command_form));
        let mut v = json!({
            "node_id": n.layer_id, "kind": n.node.kind(), "board_id": n.board_id, "name": layer.map(|l| l.name.clone()),
            "visible": layer.map(|l| l.visible), "bounds": rect.map(rect_json), "fill": if connector { Value::Null } else { rgb_json(style.and_then(|s| s.fill)) },
            "stroke": rgb_json(style.and_then(|s| s.stroke)), "stroke_width": style.map(|s| s.stroke_width), "text": text,
            "text_overset": text_id.as_ref().and_then(|id| overset.get(id).copied().flatten()), "text_id": text_id,
            "fields": serde_json::to_value(&n.node).unwrap_or(Value::Null), "issues": [],
        });
        match &n.node {
            StudioBoardNodeFields::Connector { connector_line_type, connector_start, connector_end, last_route, .. } => {
                let last: Vec<[f64; 2]> = last_route.iter().map(|p| [p.x, p.y]).collect();
                match connector_route(connector_start, connector_end, *connector_line_type, rects) {
                    Ok(route) => {
                        v["state"] = json!("attached");
                        v["route_current"] = json!(route == last);
                        v["route"] = json!(route);
                    }
                    Err(Unresolved::Broken(ends)) => {
                        v["state"] = json!("broken");
                        v["route"] = json!(last);
                        v["issues"] = ends
                            .iter()
                            .map(|(end, node)| json!({"code": "connector_endpoint_node_unresolvable", "severity": "warning", "node_id": n.layer_id, "endpoint": end, "endpoint_node_id": node}))
                            .collect();
                    }
                    Err(Unresolved::Degenerate) => {
                        v["state"] = json!("unroutable");
                        v["route"] = json!(last);
                        v["issues"] = json!([{"code": "connector_route_degenerate", "severity": "warning", "node_id": n.layer_id}]);
                    }
                    Err(Unresolved::Geometry(code)) => {
                        v["state"] = json!("unroutable");
                        v["route"] = json!(last);
                        v["issues"] = json!([{"code": code, "severity": "error", "node_id": n.layer_id}]);
                    }
                }
            }
            StudioBoardNodeFields::Section { .. } => {
                v["members"] = json!(Self::section_members(c, &n.layer_id, rects));
            }
            _ => {}
        }
        v
    }

    /// Stickies and shapes of the section's board whose centre lies in the section (on `rects`).
    fn section_members(c: &Ctx, section: &str, rects: &BTreeMap<String, BoardRect>) -> Vec<String> {
        let (Some(rec), Some(r)) = (c.record(section), rects.get(section)) else { return Vec::new() };
        c.entry
            .records
            .board_nodes
            .iter()
            .filter(|n| n.board_id == rec.board_id && n.layer_id != section && matches!(n.node, StudioBoardNodeFields::Sticky { .. } | StudioBoardNodeFields::ShapeWithText { .. }))
            .filter_map(|n| {
                let m = rects.get(&n.layer_id)?.center();
                (m[0] >= r.x && m[0] <= r.x + r.w && m[1] >= r.y && m[1] <= r.y + r.h).then(|| n.layer_id.clone())
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{BoardCommand, Viewer};
    use hsk_studio_harness_core::document_new::{DocumentProfile, NewDocument};

    fn board_viewer() -> Viewer {
        use hsk_studio_accord::{ActorContext, ClientSessionId, TransientCorrelation};
        let actor = ActorContext::new("board-unit", "principal", "owner", "owner-principal", "space", "session").unwrap();
        let context = hsk_studio_harness_core::RunContext::new(actor, ClientSessionId::parse("board-unit").unwrap(), std::sync::Arc::new(TransientCorrelation::new()));
        let mut v = Viewer::with_context(super::super::render::Prefer::Cpu, hsk_studio_render_gpu::DeviceOptions::default(), context);
        v.new_document(&NewDocument::new(DocumentProfile::Board, Some((640.0, 480.0))), None).unwrap();
        for (i, t) in ["A", "B", "C"].iter().enumerate() {
            let x = 260.0 * f64::from(u8::try_from(i).unwrap());
            v.board_command(BoardCommand::CreateSticky { board: None, at: [x, 0.0], text: (*t).to_owned(), author_visible: true, author_name: None, is_wide_width: false, fill: None }).unwrap();
        }
        v
    }

    /// Review F9: a board read checks the token per node and refuses `canceled`.
    #[test]
    fn a_board_read_refuses_canceled() {
        let v = &mut board_viewer();
        v.cancel.cancel();
        let e = v.board_command(BoardCommand::Read { board: None }).unwrap_err();
        assert!(e.starts_with("board: canceled"), "{e}");
    }

    /// Review F1 (lead ruling 2026-11-13, revised): a board record naming a layer the opened document does not
    /// hold is dropped at open with a loss note, so a layer that later takes that id never inherits it.
    #[test]
    fn a_board_record_without_its_layer_is_dropped_at_open_with_a_loss_note() {
        let v = &mut board_viewer();
        let mut entry = v.vector_entry().unwrap();
        let mut stale = entry.records.board_nodes[0].clone();
        stale.layer_id = "SLYR-019abcde-0000-7000-8000-00000000dead".to_owned();
        entry.records.board_nodes.push(stale);
        let document_id = v.run.snapshot().unwrap().document().document_id.clone();
        v.run.set_document_entry(super::super::vector::ENTRY, Some(entry.to_bytes(&document_id).unwrap())).unwrap();
        let dir = std::env::temp_dir().join(format!("hsk-board-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stale.handshake");
        v.save(&path, true).unwrap();
        let mut w = {
            use hsk_studio_accord::{ActorContext, ClientSessionId, TransientCorrelation};
            let actor = ActorContext::new("board-unit", "principal", "owner", "owner-principal", "space", "session").unwrap();
            let context = hsk_studio_harness_core::RunContext::new(actor, ClientSessionId::parse("board-unit").unwrap(), std::sync::Arc::new(TransientCorrelation::new()));
            Viewer::with_context(super::super::render::Prefer::Cpu, hsk_studio_render_gpu::DeviceOptions::default(), context)
        };
        w.open(&path).unwrap();
        let records = w.vector_entry().unwrap().records.board_nodes;
        assert_eq!(records.len(), 3, "{records:?}");
        assert!(records.iter().all(|n| n.layer_id != "SLYR-019abcde-0000-7000-8000-00000000dead"));
        let status = w.state_json()["status"].to_string();
        assert!(status.contains("board_record_dropped") && status.contains("00000000dead"), "{status}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Review F13: an export canceled after its admission settles its job `canceled`, not `failed`, and writes
    /// nothing.
    #[test]
    fn a_canceled_board_export_settles_its_job_canceled() {
        let v = &mut board_viewer();
        let dir = std::env::temp_dir().join(format!("hsk-board-unit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("canceled.png");
        let job = v.admit_export_job().unwrap();
        v.cancel.cancel();
        let r = v.board_export_admitted(job, None, &path, true);
        assert!(r.is_err(), "{r:?}");
        let counts = v.job_counts();
        assert_eq!((counts.canceled, counts.failed), (1, 0), "{counts:?}");
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
