//! Whiteboard records (lane SMB-BOARD-1; [STU-WB-001], [STU-WB-002], [STU-WB-102] to [STU-WB-109],
//! [STU-WB-115], [STU-WB-120]): the boards of a document and the additional fields of its board nodes, as
//! two members of `StudioVectorRecords` (`boards`, `board_nodes`), each skipped when empty, so record sets
//! written before them are byte-identical.
//!
//! [STU-WB-102]: a board node is a `StudioLayer` whose `kind` is its board node kind token. The kinds this
//! build draws ([`BOARD_VECTOR_KINDS`]: sticky, shape_with_text, connector, section) are vector-backed:
//! their payload is `Payload::Vector` (the drawn geometry, `SVPT` paths) and `validate_document` accepts
//! them exactly as a `vector` layer (no new `Payload` variant). The other eight kinds of the closed
//! [STU-WB-103] set ([`BOARD_NODE_KINDS`]) have no binding yet: a layer of such a kind is refused as
//! before (`PayloadMismatch`). A node's additional fields are its [`StudioBoardNode`] record, keyed by
//! the layer id. A node's text ([STU-WB-104] `text`, [STU-WB-105] `text`) is an ordinary `StudioTextStory`
//! bound as area text (`area_texts`) inside the node's own path; Studio reading: the spec's "text
//! sublayer" is that bound story, not a separate layer (declared, UNVERIFIED against the source, D5).
//!
//! [STU-WB-120]: a board is a page-kind container, an artboard (`SART`). A document whose `boards` lists
//! one is in whiteboard mode, and then every artboard of it is a board (a node names its own in
//! `board_id`).
//!
//! History: a record whose layer the document no longer holds belongs to a revision the history can
//! return to and is not checked against the document; a connector endpoint naming a node the document
//! no longer holds is the broken state of [STU-WB-108] (kept, reported by the reader, never deleted).
#![deny(clippy::arithmetic_side_effects)]
use super::{Code, Payload, Result, StudioLayer, err, id};
use crate::values::Ctx;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// [STU-WB-103] board node kinds (normative, closed, twelve members).
pub const BOARD_NODE_KINDS: [&str; 12] = [
    "sticky",
    "shape_with_text",
    "connector",
    "stamp",
    "highlight",
    "washi_tape",
    "table",
    "code_block",
    "section",
    "media_embed",
    "link_unfurl",
    "widget",
];
/// The board node kinds this build binds to a layer: vector-backed (`Payload::Vector`).
pub const BOARD_VECTOR_KINDS: [&str; 4] = ["sticky", "shape_with_text", "connector", "section"];
/// Most boards of one record set (the artboard ceiling of New Document).
pub const MAX_BOARDS: usize = 1000;
/// Most board node records of one record set (a Folio hard ceiling; the spec declares none).
pub const MAX_BOARD_NODES: usize = 65_536;
/// Longest `author_name`, bytes (Studio-declared).
pub const MAX_AUTHOR_NAME_BYTES: usize = 256;
/// Largest coordinate magnitude of a connector position, document units (New Document's position bound).
pub const BOARD_COORDINATE_LIMIT: f64 = 10_000_000.0;
/// Most points of a connector's kept route (Studio-declared).
pub const MAX_CONNECTOR_ROUTE_POINTS: usize = 64;
/// Largest corner radius, document units (Studio-declared).
pub const MAX_BOARD_CORNER_RADIUS: f64 = 100_000.0;
/// [STU-WB-105] the twelve native shapes of the `studio_declared` vocabulary (Studio spellings).
pub const STUDIO_SHAPE_TOKENS: [&str; 12] = [
    "rectangle",
    "rounded_rectangle",
    "ellipse",
    "diamond",
    "triangle",
    "parallelogram",
    "hexagon",
    "cylinder",
    "cloud",
    "document",
    "predefined_process",
    "terminator",
];
/// [STU-WB-105] / [STU-WB-129] the pinned Plugin API `shapeType` union (thirty tokens).
pub const FIGMA_SHAPE_TOKENS: [&str; 30] = [
    "SQUARE",
    "ELLIPSE",
    "ROUNDED_RECTANGLE",
    "DIAMOND",
    "TRIANGLE_UP",
    "TRIANGLE_DOWN",
    "PARALLELOGRAM_RIGHT",
    "PARALLELOGRAM_LEFT",
    "ENG_DATABASE",
    "ENG_QUEUE",
    "ENG_FILE",
    "ENG_FOLDER",
    "TRAPEZOID",
    "PREDEFINED_PROCESS",
    "SHIELD",
    "DOCUMENT_SINGLE",
    "DOCUMENT_MULTIPLE",
    "MANUAL_INPUT",
    "HEXAGON",
    "CHEVRON",
    "PENTAGON",
    "OCTAGON",
    "STAR",
    "PLUS",
    "ARROW_LEFT",
    "ARROW_RIGHT",
    "SUMMING_JUNCTION",
    "OR",
    "SPEECH_BUBBLE",
    "INTERNAL_STORAGE",
];
/// The version the `figma_plugin_api` vocabulary is pinned to ([STU-WB-129]).
pub const FIGMA_SHAPE_VOCABULARY_VERSION: &str = "1.141.0";

/// True for a kind this build binds to a vector-backed board layer ([`BOARD_VECTOR_KINDS`]).
pub fn is_board_vector_kind(kind: &str) -> bool {
    BOARD_VECTOR_KINDS.contains(&kind)
}

/// [STU-WB-120]: one board, a page-kind container.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioBoard {
    /// `SART` artboard of the document.
    pub artboard_id: String,
}

/// [STU-WB-107] connector line types (normative, closed, three members; default `ELBOWED`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StudioConnectorLineType {
    Straight,
    #[default]
    Elbowed,
    Curved,
}

/// [STU-WB-108] magnet tokens (the minimum set).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StudioConnectorMagnet {
    #[default]
    Auto,
    Top,
    Right,
    Bottom,
    Left,
    Center,
}

/// [STU-WB-109] connector stroke caps (normative, closed, twelve members).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StudioConnectorCap {
    #[default]
    None,
    ArrowEquilateral,
    ArrowLines,
    TriangleFilled,
    CircleFilled,
    DiamondFilled,
    ErdOne,
    ErdMany,
    ErdOneOrMore,
    ErdZeroOrOne,
    ErdZeroOrMore,
    ErdExactlyOne,
}

/// [STU-DS-159] dev status (normative, closed, two members).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum StudioDevStatus {
    ReadyForDev,
    Completed,
}

/// A canvas coordinate, document units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioBoardPoint {
    pub x: f64,
    pub y: f64,
}

/// [STU-WB-108] position endpoint: pinned to a canvas coordinate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEndpointPosition {
    pub position: StudioBoardPoint,
}

/// [STU-WB-108] magnet endpoint: attached to a node's anchor magnet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEndpointMagnet {
    /// `SLYR` board node.
    pub endpoint_node_id: String,
    pub magnet: StudioConnectorMagnet,
}

/// [STU-WB-108] position-and-node endpoint: attached to a node at a point relative to its bounds'
/// top-left corner, document units (Studio reading of "relative", UNVERIFIED, D5).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEndpointPositionAndNode {
    pub position: StudioBoardPoint,
    /// `SLYR` board node.
    pub endpoint_node_id: String,
}

/// [STU-WB-108] connector endpoint (exactly one of three closed variants; told apart by their fields).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum StudioConnectorEndpoint {
    PositionAndNode(StudioEndpointPositionAndNode),
    Magnet(StudioEndpointMagnet),
    Position(StudioEndpointPosition),
}

impl StudioConnectorEndpoint {
    /// The node the endpoint is attached to, if any.
    pub fn node(&self) -> Option<&str> {
        match self {
            Self::PositionAndNode(e) => Some(&e.endpoint_node_id),
            Self::Magnet(e) => Some(&e.endpoint_node_id),
            Self::Position(_) => None,
        }
    }

    /// The variant token (`position`, `magnet`, `position_and_node`).
    pub const fn variant(&self) -> &'static str {
        match self {
            Self::PositionAndNode(_) => "position_and_node",
            Self::Magnet(_) => "magnet",
            Self::Position(_) => "position",
        }
    }
}

/// [STU-WB-105] the vocabulary a `shape_type` token comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioShapeVocabulary {
    StudioDeclared,
    FigmaPluginApi,
}

/// [STU-WB-105] a provenance-qualified diagram-shape token: the vocabulary, its pinned version
/// (`figma_plugin_api` only) and the literal token, never relabelled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioShapeType {
    pub vocabulary: StudioShapeVocabulary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vocabulary_version: Option<String>,
    pub token: String,
}

impl StudioShapeType {
    /// The native shape `token` of the `studio_declared` vocabulary.
    pub fn studio(token: &str) -> Self {
        Self { vocabulary: StudioShapeVocabulary::StudioDeclared, vocabulary_version: None, token: token.to_owned() }
    }

    /// The text form `<vocabulary>:<token>` (`figma_plugin_api@1.141.0:SQUARE`).
    pub fn text(&self) -> String {
        match (self.vocabulary, &self.vocabulary_version) {
            (StudioShapeVocabulary::StudioDeclared, _) => format!("studio_declared:{}", self.token),
            (StudioShapeVocabulary::FigmaPluginApi, Some(v)) => format!("figma_plugin_api@{v}:{}", self.token),
            (StudioShapeVocabulary::FigmaPluginApi, None) => format!("figma_plugin_api:{}", self.token),
        }
    }

    /// Parses the text form; `None` when the vocabulary or token is outside the closed lists.
    pub fn parse(text: &str) -> Option<Self> {
        let (vocabulary, token) = text.split_once(':')?;
        let parsed = match vocabulary {
            "studio_declared" => Self::studio(token),
            v => {
                let version = v.strip_prefix("figma_plugin_api@")?;
                Self { vocabulary: StudioShapeVocabulary::FigmaPluginApi, vocabulary_version: Some(version.to_owned()), token: token.to_owned() }
            }
        };
        parsed.is_valid().then_some(parsed)
    }

    /// The token is a member of its vocabulary, with the pinned version for `figma_plugin_api`.
    pub fn is_valid(&self) -> bool {
        match self.vocabulary {
            StudioShapeVocabulary::StudioDeclared => self.vocabulary_version.is_none() && STUDIO_SHAPE_TOKENS.contains(&self.token.as_str()),
            StudioShapeVocabulary::FigmaPluginApi => {
                self.vocabulary_version.as_deref() == Some(FIGMA_SHAPE_VOCABULARY_VERSION) && FIGMA_SHAPE_TOKENS.contains(&self.token.as_str())
            }
        }
    }
}

/// The additional fields of one board node kind ([STU-WB-104], [STU-WB-105], [STU-WB-106],
/// [STU-WB-115]); `node_kind` is the layer's `kind`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "node_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StudioBoardNodeFields {
    Sticky {
        author_visible: bool,
        /// A display string captured at creation; the creating event's actor stays the authority.
        author_name: String,
        is_wide_width: bool,
    },
    ShapeWithText {
        shape_type: StudioShapeType,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corner_radius: Option<f64>,
    },
    Connector {
        connector_line_type: StudioConnectorLineType,
        connector_start: StudioConnectorEndpoint,
        connector_end: StudioConnectorEndpoint,
        connector_start_stroke_cap: StudioConnectorCap,
        connector_end_stroke_cap: StudioConnectorCap,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corner_radius: Option<f64>,
        /// Degrees.
        #[serde(default)]
        rotation: f64,
        /// The route last drawn (document units): the last known position a broken connector keeps
        /// ([STU-WB-108]); empty when never drawn.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        last_route: Vec<StudioBoardPoint>,
    },
    Section {
        section_contents_hidden: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        corner_radius: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dev_status: Option<StudioDevStatus>,
    },
}

impl StudioBoardNodeFields {
    /// The [STU-WB-103] kind token.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Sticky { .. } => "sticky",
            Self::ShapeWithText { .. } => "shape_with_text",
            Self::Connector { .. } => "connector",
            Self::Section { .. } => "section",
        }
    }
}

/// One board node: its layer, its board and its additional fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioBoardNode {
    /// `SLYR` layer of kind [`StudioBoardNodeFields::kind`].
    pub layer_id: String,
    /// `SART` artboard of the board ([`StudioBoard`]).
    pub board_id: String,
    pub node: StudioBoardNodeFields,
}

fn invalid(target: &str, field: &str) -> crate::Diagnostic {
    err(Code::InvalidField, format!("{target}/{field}"))
}

fn finite_in(v: f64, limit: f64) -> bool {
    v.is_finite() && v.abs() <= limit
}

fn radius_ok(r: Option<f64>) -> bool {
    r.is_none_or(|r| r.is_finite() && (0.0..=MAX_BOARD_CORNER_RADIUS).contains(&r))
}

fn endpoint(e: &StudioConnectorEndpoint, own: &str, target: &str) -> Result<()> {
    let point = |p: &StudioBoardPoint| finite_in(p.x, BOARD_COORDINATE_LIMIT) && finite_in(p.y, BOARD_COORDINATE_LIMIT);
    match e {
        StudioConnectorEndpoint::Position(p) if !point(&p.position) => return Err(invalid(target, "position")),
        StudioConnectorEndpoint::PositionAndNode(p) if !point(&p.position) => return Err(invalid(target, "position")),
        _ => {}
    }
    if let Some(node) = e.node() {
        id(node, "SLYR", &format!("{target}/endpoint_node_id"))?;
        if node == own {
            return Err(invalid(target, "endpoint_node_id"));
        }
    }
    Ok(())
}

/// Checks `boards` and `board_nodes` (module docs): ids, uniqueness, closed tokens, finite bounded
/// numbers, and a node whose layer the document holds is a vector-backed
/// layer of its own kind.
pub(crate) fn validate_board_records(boards: &[StudioBoard], nodes: &[StudioBoardNode], layers: &BTreeMap<&str, &StudioLayer>, cx: &mut Ctx<'_>) -> Result<()> {
    if boards.len() > MAX_BOARDS {
        return Err(err(Code::BudgetExceeded, "boards"));
    }
    if nodes.len() > MAX_BOARD_NODES {
        return Err(err(Code::BudgetExceeded, "board_nodes"));
    }
    cx.nodes(boards.len(), "boards")?;
    cx.nodes(nodes.len(), "board_nodes")?;
    let mut listed = BTreeSet::new();
    for b in boards {
        cx.tick()?;
        id(&b.artboard_id, "SART", "boards/artboard_id")?;
        if !listed.insert(b.artboard_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, b.artboard_id.as_str()));
        }
    }
    let mut seen = BTreeSet::new();
    for n in nodes {
        cx.tick()?;
        let t = n.layer_id.as_str();
        id(t, "SLYR", "board_nodes/layer_id")?;
        if !seen.insert(t) {
            return Err(err(Code::DuplicateIdentity, t));
        }
        id(&n.board_id, "SART", &format!("{t}/board_id"))?;
        if let Some(l) = layers.get(t)
            && (l.kind != n.node.kind() || !matches!(l.payload, Payload::Vector { .. }))
        {
            return Err(err(Code::PayloadMismatch, t));
        }
        match &n.node {
            StudioBoardNodeFields::Sticky { author_name, .. } => {
                cx.text(author_name, t)?;
                if author_name.len() > MAX_AUTHOR_NAME_BYTES || author_name.chars().any(char::is_control) {
                    return Err(invalid(t, "author_name"));
                }
            }
            StudioBoardNodeFields::ShapeWithText { shape_type, corner_radius } => {
                cx.text(&shape_type.token, t)?;
                if !shape_type.is_valid() {
                    return Err(invalid(t, "shape_type"));
                }
                if !radius_ok(*corner_radius) {
                    return Err(invalid(t, "corner_radius"));
                }
            }
            StudioBoardNodeFields::Connector { connector_start, connector_end, corner_radius, rotation, last_route, .. } => {
                endpoint(connector_start, t, &format!("{t}/connector_start"))?;
                endpoint(connector_end, t, &format!("{t}/connector_end"))?;
                if last_route.len() > MAX_CONNECTOR_ROUTE_POINTS || !last_route.iter().all(|p| finite_in(p.x, BOARD_COORDINATE_LIMIT) && finite_in(p.y, BOARD_COORDINATE_LIMIT)) {
                    return Err(invalid(t, "last_route"));
                }
                if !radius_ok(*corner_radius) {
                    return Err(invalid(t, "corner_radius"));
                }
                if !finite_in(*rotation, 360.0) {
                    return Err(invalid(t, "rotation"));
                }
            }
            StudioBoardNodeFields::Section { corner_radius, .. } => {
                if !radius_ok(*corner_radius) {
                    return Err(invalid(t, "corner_radius"));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::*;
    use hsk_studio_accord::CancellationToken;
    use serde_json::{Value, json};

    fn identity(prefix: &str, n: u32) -> String {
        format!("{prefix}-019abcde-0000-7000-8000-{n:012x}")
    }

    fn layer(n: u32, kind: &str) -> Value {
        json!({"schema_id":"hsk.studio.layer@1","layer_id":identity("SLYR",n),"name":"n","parent":null,
            "kind":kind,"payload":{"payload_kind":"vector","path_ids":[identity("SVPT",n)]},"visible":true})
    }

    fn document(layers: Vec<Value>) -> Value {
        let ops: Vec<Value> = layers
            .iter()
            .map(|l| json!({"layer_id": l["layer_id"], "operation_key":"source","operation_kind":"source","inputs":[],
                "outputs":[{"port_key":"out","value_type":"image","cardinality":"one","semantic":"source"}]}))
            .collect();
        let outputs: Vec<Value> = layers.iter().map(|l| json!({"layer_id": l["layer_id"], "operation_key":"source","port_key":"out"})).collect();
        json!({
            "schema_id":"hsk.studio.document@1","document_id":identity("SDOC",1),"revision":1,"geometry_unit":"px",
            "colour_mode":2,"bits_per_channel":8,"working_space_rgb":null,"working_space_cmyk":null,
            "working_space_gray":null,"working_space_spot":null,"blending_space":null,"artboards":[],"page_spreads":[],
            "layers":layers,"graph":{"schema_id":"hsk.studio.layer_graph@1","operations":ops,"edges":[],"outputs":outputs}
        })
    }

    fn doc() -> StudioDocument {
        serde_json::from_value(document(vec![layer(1, "sticky"), layer(2, "shape_with_text"), layer(3, "connector")])).unwrap()
    }

    fn records() -> Value {
        json!({"schema_id":"hsk.studio.vector_records@1","document_id":identity("SDOC",1),
            "boards":[{"artboard_id":identity("SART",1)}],
            "board_nodes":[
                {"layer_id":identity("SLYR",1),"board_id":identity("SART",1),"node":{"node_kind":"sticky","author_visible":true,"author_name":"Ilse","is_wide_width":false}},
                {"layer_id":identity("SLYR",2),"board_id":identity("SART",1),"node":{"node_kind":"shape_with_text","shape_type":{"vocabulary":"figma_plugin_api","vocabulary_version":"1.141.0","token":"ENG_DATABASE"}}},
                {"layer_id":identity("SLYR",3),"board_id":identity("SART",1),"node":{"node_kind":"connector","connector_line_type":"ELBOWED",
                    "connector_start":{"endpoint_node_id":identity("SLYR",1),"magnet":"RIGHT"},
                    "connector_end":{"position":{"x":10.0,"y":5.0},"endpoint_node_id":identity("SLYR",2)},
                    "connector_start_stroke_cap":"NONE","connector_end_stroke_cap":"ERD_ONE_OR_MORE","rotation":0.0,"last_route":[{"x":0.0,"y":0.0},{"x":4.0,"y":0.0}]}}
            ]})
    }

    fn check(v: &Value) -> std::result::Result<(), Code> {
        let r: StudioVectorRecords = serde_json::from_value(v.clone()).map_err(|_| Code::InvalidJson)?;
        validate_vector_records(&r, &doc(), None, Budget::default(), &CancellationToken::default()).map_err(|d| d.code)
    }

    /// `records()` with the member at `path` set (inserted when absent).
    fn with(path: &str, v: Value) -> std::result::Result<(), Code> {
        let mut x = records();
        let (parent, key) = path.rsplit_once('/').expect("a pointer");
        match x.pointer_mut(parent).unwrap_or_else(|| panic!("{parent}")) {
            Value::Object(m) => {
                m.insert(key.to_owned(), v);
            }
            Value::Array(a) => a[key.parse::<usize>().expect("an index")] = v,
            other => panic!("{other}"),
        }
        check(&x)
    }

    #[test]
    fn board_records_round_trip_and_a_set_without_them_is_byte_identical() {
        assert_eq!(check(&records()), Ok(()));
        let r: StudioVectorRecords = serde_json::from_value(records()).unwrap();
        assert_eq!(serde_json::to_value(&r).unwrap(), records());
        let StudioBoardNodeFields::Connector { connector_start, connector_end, .. } = &r.board_nodes[2].node else { panic!("connector") };
        assert_eq!((connector_start.variant(), connector_end.variant()), ("magnet", "position_and_node"));
        let empty = format!(r#"{{"schema_id":"hsk.studio.vector_records@1","document_id":"{}"}}"#, identity("SDOC", 1));
        let r: StudioVectorRecords = serde_json::from_str(&empty).unwrap();
        assert_eq!(serde_json::to_string(&r).unwrap(), empty);
        assert!(vector_records_schema().as_value().to_string().contains("board_nodes"));
    }

    #[test]
    fn board_kinds_bind_only_vector_payloads_and_the_other_eight_stay_refused() {
        assert_eq!(BOARD_NODE_KINDS.len(), 12);
        assert!(BOARD_VECTOR_KINDS.iter().all(|k| BOARD_NODE_KINDS.contains(k)));
        let ok = serde_json::from_value::<StudioDocument>(document(vec![layer(1, "sticky"), layer(2, "section")])).unwrap();
        assert!(validate_document(ok, Budget::default(), &CancellationToken::default(), &Unresolved).is_ok());
        let stamp = serde_json::from_value::<StudioDocument>(document(vec![layer(1, "stamp")])).unwrap();
        assert_eq!(validate_document(stamp, Budget::default(), &CancellationToken::default(), &Unresolved).err().map(|d| d.code), Some(Code::PayloadMismatch));
        // A record whose layer is another kind is a mismatch.
        assert_eq!(with("/board_nodes/0/node", json!({"node_kind":"section","section_contents_hidden":false})), Err(Code::PayloadMismatch));
    }

    #[test]
    fn hostile_board_records_are_refused_with_typed_codes() {
        assert_eq!(with("/board_nodes/0/node/author_name", json!("x".repeat(MAX_AUTHOR_NAME_BYTES.saturating_add(1)))), Err(Code::InvalidField));
        assert_eq!(with("/board_nodes/0/node/author_name", json!("a\u{7}b")), Err(Code::InvalidField));
        assert_eq!(with("/board_nodes/1/node/shape_type/token", json!("SHIELDS")), Err(Code::InvalidField));
        assert_eq!(with("/board_nodes/1/node/shape_type/vocabulary_version", json!("1.142.0")), Err(Code::InvalidField));
        assert_eq!(with("/board_nodes/1/node/corner_radius", json!(-1.0)), Err(Code::InvalidField));
        assert_eq!(with("/board_nodes/2/node/connector_start/magnet", json!("NORTH")), Err(Code::InvalidJson));
        assert_eq!(with("/board_nodes/2/node/connector_line_type", json!("WAVY")), Err(Code::InvalidJson));
        assert_eq!(with("/board_nodes/2/node/connector_end_stroke_cap", json!("ARROW_SIMPLE")), Err(Code::InvalidJson));
        assert_eq!(with("/board_nodes/2/node/connector_start/endpoint_node_id", json!(identity("SLYR", 3))), Err(Code::InvalidField), "self reference");
        assert_eq!(with("/board_nodes/2/node/connector_start/endpoint_node_id", json!("not-an-id")), Err(Code::InvalidId));
        assert_eq!(with("/board_nodes/2/node/connector_end/position/x", json!(1.0e12)), Err(Code::InvalidField));
        assert_eq!(with("/board_nodes/2/node/rotation", json!(720.0)), Err(Code::InvalidField));
        assert_eq!(with("/board_nodes/2/node/last_route", json!(vec![json!({"x":0.0,"y":0.0}); MAX_CONNECTOR_ROUTE_POINTS.saturating_add(1)])), Err(Code::InvalidField));
        assert_eq!(with("/board_nodes/1/layer_id", json!(identity("SLYR", 1))), Err(Code::DuplicateIdentity));
        assert_eq!(with("/boards/0/artboard_id", json!(identity("SLYR", 1))), Err(Code::InvalidId));
        assert_eq!(with("/board_nodes/0/node/surprise", json!(1)), Err(Code::InvalidJson), "unknown fields are refused");
        // A record whose layer is gone is history, not an error; an endpoint naming a gone node is the broken state.
        assert_eq!(with("/board_nodes/0/layer_id", json!(identity("SLYR", 9))), Ok(()));
        assert_eq!(with("/board_nodes/2/node/connector_start/endpoint_node_id", json!(identity("SLYR", 9))), Ok(()));
    }

    #[test]
    fn board_record_counts_are_charged_before_the_records_are_walked() {
        let mut x = records();
        let node = x["board_nodes"][0].clone();
        let many: Vec<Value> = (0..=u32::try_from(MAX_BOARD_NODES).unwrap())
            .map(|n| {
                let mut v = node.clone();
                v["layer_id"] = json!(identity("SLYR", n.saturating_add(100)));
                v
            })
            .collect();
        x["board_nodes"] = json!(many);
        assert_eq!(check(&x), Err(Code::BudgetExceeded), "aggregate over budget");
        let mut y = records();
        y["boards"] = json!((0..=u32::try_from(MAX_BOARDS).unwrap()).map(|n| json!({"artboard_id": identity("SART", n)})).collect::<Vec<_>>());
        assert_eq!(check(&y), Err(Code::BudgetExceeded));
    }

    #[test]
    fn shape_type_text_form_keeps_vocabulary_and_token_apart() {
        let s = StudioShapeType::parse("figma_plugin_api@1.141.0:TRIANGLE_UP").unwrap();
        assert_eq!((s.vocabulary, s.vocabulary_version.as_deref(), s.token.as_str()), (StudioShapeVocabulary::FigmaPluginApi, Some("1.141.0"), "TRIANGLE_UP"));
        assert_eq!(s.text(), "figma_plugin_api@1.141.0:TRIANGLE_UP");
        assert_eq!(StudioShapeType::parse("studio_declared:cloud").unwrap().text(), "studio_declared:cloud");
        for bad in ["cloud", "studio_declared:CLOUD", "figma_plugin_api:SQUARE", "figma_plugin_api@1.141.0:cloud", ""] {
            assert!(StudioShapeType::parse(bad).is_none(), "{bad}");
        }
        assert!(STUDIO_SHAPE_TOKENS.iter().all(|t| StudioShapeType::parse(&format!("studio_declared:{t}")).is_some()));
        assert!(FIGMA_SHAPE_TOKENS.iter().all(|t| StudioShapeType::parse(&format!("figma_plugin_api@1.141.0:{t}")).is_some()));
    }
}
