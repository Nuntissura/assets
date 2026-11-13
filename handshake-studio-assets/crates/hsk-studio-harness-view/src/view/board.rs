//! Whiteboard mode on the viewer (lane SMB-BOARD-1; [STU-WB-001], [STU-WB-002], [STU-WB-102] to
//! [STU-WB-108], [STU-WB-115], [STU-WB-120], [STU-WB-127], [STU-WB-134]): the typed board commands
//! ([`BoardCommand`], ids `board.*`), egui-free and drawing nothing on screen. Every mutating command is ONE
//! undo step; reads change nothing.
//!
//! Model (Folio `board.rs`): a board is an artboard; a document is in whiteboard mode when its vector
//! records list a board (`document.new` with profile `board` lists its first artboard; the first node
//! created on an artboard of any document lists it in the same step), and then every artboard is a board
//! (`board.add` is `artboard.add`). A board node is a layer of kind `sticky`, `shape_with_text`, `section`
//! or `connector` whose `Payload::Vector` path is its drawing (composite `board`), with its additional
//! fields in the vector records' `board_nodes` and its text an area-text story bound inside its own path.
//! Every write is the vector entry and the layers' paths in one step (`Run::restructure_and_entry` for a
//! create or delete, `Run::set_vector_paths_and_entry` for an edit); a story is written to the `text`
//! entry first under a fresh id, so an undo returns the binding to the old story and the old text.
//!
//! Connectors ([STU-WB-108]): endpoints resolve against the current bounds of their nodes; a move of a node
//! by `board.node.move` reroutes every connector attached to it in the same step, and a read reports the
//! route resolved now (`route_current` says whether the drawing matches it: a move by another tool redraws
//! the connector at the next board edit). A connector whose endpoint node is gone is `broken`: it keeps its
//! last route and drawing and reads report `connector_endpoint_node_unresolvable` (severity warning,
//! [STU-WB-142]); a command naming such a node is refused with that code.
//!
//! Studio readings, declared (FigJam behaviour UNVERIFIED, D5): sticky sizes (240 square, 480 wide, taller
//! for longer text), shape sizes from the text width, text at the top of its node, a section's members
//! are the nodes whose centre lies in it and move with it, a section's name is its label story,
//! `section_contents_hidden` is stored and read but not drawn, `CURVED` and the six unmapped Plugin API
//! shapes are refused typed, a connector does not move by itself, `author_name` defaults to the actor's
//! account id. Out of scope: stamps, highlights, washi tape, tables, code blocks, media, link unfurls,
//! widgets, facilitation, templates, the board GUI (OD-4), PDF and CSV export.
#![deny(clippy::arithmetic_side_effects)]
use super::Viewer;
use super::text;
use super::vector::{self, VectorEntry, VectorStyle};
use hsk_studio_composite::board::{self as geo, BoardEnd, BoardRect};
use hsk_studio_folio::{
    self as folio, Payload, StudioBoard, StudioBoardNode, StudioBoardNodeFields, StudioBoardPoint, StudioConnectorCap, StudioConnectorEndpoint, StudioConnectorLineType, StudioDevStatus,
    StudioDocument, StudioShapeType,
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
    /// `board.add`: a new board (an artboard of a whiteboard-mode document).
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

/// One line of 1..=4096 bytes (the area-text story rule of the viewer).
fn text_ok(t: &str) -> Result<(), String> {
    if t.is_empty() || t.len() > text::MAX_STORY_BYTES || t.contains(['\n', '\r', '\u{2028}']) || t.chars().any(char::is_control) {
        return Err(refused("board_text_invalid", format!("one line of 1..={} bytes", text::MAX_STORY_BYTES)));
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
    Geometry(String),
}

/// The editing context: the document, the vector entry and its board records.
struct Ctx {
    doc: StudioDocument,
    entry: VectorEntry,
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

    fn boards(&self) -> bool {
        !self.entry.records.boards.is_empty()
    }

    /// Bounds of every attachable node now (vector layers of the document), with `moved` offsets.
    fn rects(&self, moved: &BTreeMap<String, [f64; 2]>) -> BTreeMap<String, BoardRect> {
        let mut out = BTreeMap::new();
        for l in &self.doc.layers {
            if let Payload::Vector { path_ids } = &l.payload
                && let Some(mut r) = bounds_of_paths(&self.entry, path_ids)
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
    if route.len() > MAX_ROUTE_POINTS {
        return Err(Unresolved::Geometry("route_too_long".into()));
    }
    Ok(route)
}

/// The size a node of `kind` takes for text `width` (points).
fn fitted_size(fields: &StudioBoardNodeFields, text_width: f64, asked: [Option<f64>; 2]) -> [f64; 2] {
    let line = TEXT_PT * 1.3;
    match fields {
        StudioBoardNodeFields::Sticky { is_wide_width, .. } => {
            let w = if *is_wide_width { STICKY_WIDE } else { STICKY_SIDE };
            let lines = (text_width / (w - 2.0 * TEXT_INSET)).ceil().max(1.0);
            [w, (lines * line + 2.0 * TEXT_INSET).max(STICKY_SIDE)]
        }
        StudioBoardNodeFields::ShapeWithText { shape_type, .. } => {
            // 5 % and 4 points of slack over the one-line advance: the area layout breaks a line that
            // fits only exactly.
            let fit = geo::text_fit(shape_type);
            [asked[0].unwrap_or(SHAPE_MIN[0]).max((text_width * 1.05 + 4.0 + 2.0 * SHAPE_TEXT_INSET) * fit), asked[1].unwrap_or(SHAPE_MIN[1]).max((line + 2.0 * SHAPE_TEXT_INSET) * fit)]
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

/// Where a node's text starts: a shape centres one line vertically (its outline is widest near the middle:
/// a diamond's or a triangle's top holds no line), a sticky and a section start at the top (Studio reading).
fn text_baseline(fields: &StudioBoardNodeFields, rect: &BoardRect) -> (folio::StudioFirstBaseline, f64) {
    match fields {
        StudioBoardNodeFields::ShapeWithText { .. } => (folio::StudioFirstBaseline::Fixed, ((rect.h - 2.0 * SHAPE_TEXT_INSET - TEXT_PT * 1.3) / 2.0).max(0.0) + TEXT_PT * 0.8),
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
        Ok(Ctx { doc, entry })
    }

    fn board_inner(&mut self, command: BoardCommand) -> Result<Value, String> {
        let op = command.id().replace('.', "_");
        match command {
            BoardCommand::Add { name, size, at } => self.board_add(name, size, at),
            BoardCommand::Read { board } => self.board_read(board.as_deref()),
            BoardCommand::NodeRead { node } => {
                let c = self.board_ctx()?;
                let rec = c.node(&node)?;
                Ok(self.node_json(&c, &rec, &self.text_overset()))
            }
            BoardCommand::CreateSticky { board, at, text, author_visible, author_name, is_wide_width, fill } => {
                let author_name = author_name.unwrap_or_else(|| self.run.actor().account_id().to_owned());
                if author_name.len() > folio::MAX_AUTHOR_NAME_BYTES || author_name.chars().any(char::is_control) {
                    return Err(refused("author_name_invalid", format!("at most {} bytes, no control character", folio::MAX_AUTHOR_NAME_BYTES)));
                }
                let fields = StudioBoardNodeFields::Sticky { author_visible, author_name, is_wide_width };
                let style = VectorStyle { fill: Some(fill.unwrap_or(STICKY_FILL)), stroke: None, stroke_width: 0.0 };
                self.create_text_node(&op, board.as_deref(), at, fields, &text, [None, None], style)
            }
            BoardCommand::CreateShape { board, at, shape_type, text, size, corner_radius, fill } => {
                if !size.iter().flatten().all(|v| side_ok(*v)) || corner_radius.is_some_and(|r| !(r.is_finite() && (0.0..=folio::MAX_BOARD_CORNER_RADIUS).contains(&r))) {
                    return Err(refused("board_size_invalid", "sides in (0, 100000], corner radius in [0, 100000]"));
                }
                let fields = StudioBoardNodeFields::ShapeWithText { shape_type, corner_radius };
                let style = VectorStyle { fill: Some(fill.unwrap_or(SHAPE_FILL)), stroke: Some(SHAPE_STROKE), stroke_width: 2.0 };
                self.create_text_node(&op, board.as_deref(), at, fields, &text, size, style)
            }
            BoardCommand::CreateSection { board, rect, name, corner_radius, fill } => {
                if !(side_ok(rect[2]) && side_ok(rect[3])) || corner_radius.is_some_and(|r| !(r.is_finite() && (0.0..=folio::MAX_BOARD_CORNER_RADIUS).contains(&r))) {
                    return Err(refused("board_size_invalid", "sides in (0, 100000], corner radius in [0, 100000]"));
                }
                let fields = StudioBoardNodeFields::Section { section_contents_hidden: false, corner_radius, dev_status: None };
                let style = VectorStyle { fill: Some(fill.unwrap_or(SECTION_FILL)), stroke: Some(SECTION_STROKE), stroke_width: 1.0 };
                self.create_text_node(&op, board.as_deref(), [rect[0], rect[1]], fields, &name, [Some(rect[2]), Some(rect[3])], style)
            }
            BoardCommand::CreateConnector { board, start, end, line_type, caps, stroke, width } => self.create_connector(&op, board.as_deref(), start, end, line_type, caps, stroke, width),
            BoardCommand::SetSticky { node, author_visible, author_name, is_wide_width } => {
                if author_name.as_ref().is_some_and(|n| n.len() > folio::MAX_AUTHOR_NAME_BYTES || n.chars().any(char::is_control)) {
                    return Err(refused("author_name_invalid", format!("at most {} bytes, no control character", folio::MAX_AUTHOR_NAME_BYTES)));
                }
                self.edit_node(&op, &node, |fields| match fields {
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
                self.edit_node(&op, &node, |fields| match fields {
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
                    text_ok(n)?;
                }
                if !matches!(self.board_ctx()?.node(&node)?.node, StudioBoardNodeFields::Section { .. }) {
                    return Err(refused("board_node_kind_mismatch", "not a section"));
                }
                self.edit_node_text(&op, &node, name.as_deref(), |fields| {
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
            BoardCommand::SetConnector { node, start, end, line_type, caps } => self.set_connector(&op, &node, start, end, line_type, caps),
            BoardCommand::SetText { node, text } => {
                text_ok(&text)?;
                if matches!(self.board_ctx()?.node(&node)?.node, StudioBoardNodeFields::Connector { .. }) {
                    return Err(refused("board_node_kind_mismatch", "a connector has no text in this build"));
                }
                self.edit_node_text(&op, &node, Some(&text), |_| Ok(()))
            }
            BoardCommand::SetFill { node, fill } => {
                if matches!(self.board_ctx()?.node(&node)?.node, StudioBoardNodeFields::Connector { .. }) {
                    return Err(refused("board_node_kind_mismatch", "a connector's colour is its stroke"));
                }
                self.board_restyle(&op, &node, fill)
            }
            BoardCommand::Move { node, to } => self.move_node(&op, &node, to),
            BoardCommand::Delete { node } => self.delete_node(&op, &node),
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

    /// Lists `board` in the entry's boards (whiteboard mode, module docs).
    fn ensure_board(entry: &mut VectorEntry, board: &str) {
        if !entry.records.boards.iter().any(|b| b.artboard_id == board) {
            entry.records.boards.push(StudioBoard { artboard_id: board.to_owned() });
        }
    }

    /// `document.new` with profile `board`: lists the new document's first artboard as its board.
    /// Not an undo step (the new document has no history).
    pub(super) fn register_new_board(&mut self) -> Result<(), String> {
        let c = self.board_ctx()?;
        let first = c.doc.artboards.first().map(|a| a.artboard_id.clone()).ok_or_else(|| refused("board_unknown", "the new document holds no artboard"))?;
        if !vector::entry_persistable() {
            return Err(refused("vector_entry_not_persistable", "the session holds no vector entry token"));
        }
        let mut entry = c.entry;
        Self::ensure_board(&mut entry, &first);
        let bytes = entry.to_bytes(&c.doc.document_id)?;
        self.run.set_document_entry(vector::ENTRY, Some(bytes)).map_err(|e| refused("board_entry", e))
    }

    fn board_add(&mut self, name: Option<String>, size: Option<[f64; 2]>, at: Option<[f64; 2]>) -> Result<Value, String> {
        let c = self.board_ctx()?;
        if !c.boards() {
            return Err(refused("not_a_board_document", "create the document with profile board, or a board node first"));
        }
        let name = name.unwrap_or_else(|| format!("Board {}", c.doc.artboards.len().saturating_add(1)));
        let [w, h] = size.or_else(|| c.doc.artboards.first().map(|a| [a.bounds.width.value, a.bounds.height.value])).unwrap_or(BOARD_DEFAULT_SIZE);
        let ok = self.run.add_artboard(&name, w, h, at);
        let revision = self.after_edit(ok, format!("board.add {name}"))?;
        let id = self.last_op()["detail"]["artboard_id"].as_str().unwrap_or_default().to_owned();
        Ok(json!({"board_id": id, "name": name, "revision": revision}))
    }

    fn board_read(&self, board: Option<&str>) -> Result<Value, String> {
        let c = self.board_ctx()?;
        if let Some(b) = board
            && !c.doc.artboards.iter().any(|a| a.artboard_id == b)
        {
            return Err(refused("board_unknown", b));
        }
        let active = self.active_artboard(&c.doc).map(|a| a.artboard_id.clone());
        let mode = c.boards();
        let boards: Vec<Value> = c
            .doc
            .artboards
            .iter()
            .filter(|_| mode)
            .map(|a| {
                let count = c.entry.records.board_nodes.iter().filter(|n| n.board_id == a.artboard_id && c.doc.layers.iter().any(|l| l.layer_id == n.layer_id)).count();
                json!({"board_id": a.artboard_id, "name": a.name, "bounds": [a.bounds.x.value, a.bounds.y.value, a.bounds.width.value, a.bounds.height.value], "active": active.as_deref() == Some(a.artboard_id.as_str()), "nodes": count})
            })
            .collect();
        let overset = self.text_overset();
        let nodes: Vec<Value> = c
            .entry
            .records
            .board_nodes
            .iter()
            .filter(|n| board.is_none_or(|b| n.board_id == b) && c.doc.layers.iter().any(|l| l.layer_id == n.layer_id))
            .map(|n| self.node_json(&c, n, &overset))
            .collect();
        let issues: Vec<Value> = nodes.iter().flat_map(|n| n["issues"].as_array().cloned().unwrap_or_default()).collect();
        Ok(json!({"whiteboard_mode": mode, "boards": boards, "nodes": nodes, "issues": issues}))
    }

    /// Overset glyphs of every live story as the window draws it (story id -> count); a story the
    /// renderer could not lay out reads `None` (its code is in the vector text reports).
    fn text_overset(&self) -> BTreeMap<String, Option<usize>> {
        self.vector_text_reports().unwrap_or_default().into_iter().map(|s| (s.report.text_id.clone(), s.report.outcome.as_ref().ok().map(|d| d.overset_count))).collect()
    }

    /// The reading of one node (module docs); `text_overset` is the glyphs its text does not fit.
    fn node_json(&self, c: &Ctx, n: &StudioBoardNode, overset: &BTreeMap<String, Option<usize>>) -> Value {
        let layer = c.doc.layers.iter().find(|l| l.layer_id == n.layer_id);
        let rect = c.rect(&n.layer_id);
        let style = c.style(&n.layer_id);
        let text_id = c.text_id(&n.layer_id);
        let entry = self.text_entry().ok();
        let text = text_id.as_ref().and_then(|id| entry.as_ref()?.stories.iter().find(|s| &s.story_id == id).and_then(text::story_text).map(str::to_owned));
        let mut v = json!({
            "node_id": n.layer_id, "kind": n.node.kind(), "board_id": n.board_id, "name": layer.map(|l| l.name.clone()),
            "visible": layer.map(|l| l.visible), "bounds": rect.as_ref().map(rect_json), "fill": rgb_json(style.and_then(|s| s.fill)),
            "stroke": rgb_json(style.and_then(|s| s.stroke)), "text": text, "text_overset": text_id.as_ref().and_then(|id| overset.get(id).copied().flatten()), "text_id": text_id,
            "fields": serde_json::to_value(&n.node).unwrap_or(Value::Null), "issues": [],
        });
        match &n.node {
            StudioBoardNodeFields::Connector { connector_line_type, connector_start, connector_end, last_route, .. } => {
                let rects = c.rects(&BTreeMap::new());
                let last: Vec<[f64; 2]> = last_route.iter().map(|p| [p.x, p.y]).collect();
                match connector_route(connector_start, connector_end, *connector_line_type, &rects) {
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
                    Err(Unresolved::Geometry(code)) => {
                        v["state"] = json!("unroutable");
                        v["route"] = json!(last);
                        v["issues"] = json!([{"code": code, "severity": "error", "node_id": n.layer_id}]);
                    }
                }
            }
            StudioBoardNodeFields::Section { .. } => {
                v["members"] = json!(Self::section_members(c, &n.layer_id, &BTreeMap::new()));
            }
            _ => {}
        }
        v
    }

    /// Non-section, non-connector nodes of the section's board whose centre lies in the section.
    fn section_members(c: &Ctx, section: &str, moved: &BTreeMap<String, [f64; 2]>) -> Vec<String> {
        let rects = c.rects(moved);
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
