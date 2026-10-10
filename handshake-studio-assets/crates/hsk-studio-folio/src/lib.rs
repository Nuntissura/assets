//! Pure Studio document wire contracts and immutable, caller-revision-checked snapshots.
//! Catalog, grants, byte resolution, rendering and host promotion remain caller-owned.
use hsk_studio_accord::{
    CancellationToken, DomainId, Length as AccordLength, SchemaId, Unit as AccordUnit,
};
use schemars::{JsonSchema, generate::SchemaSettings};
use serde::{
    Deserialize, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::Arc,
};

macro_rules! schema_token {
    ($name:ident, $wire:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        pub enum $name {
            #[serde(rename=$wire)]
            V1,
        }
    };
}
schema_token!(DocumentSchema, "hsk.studio.document@1");
schema_token!(LayerSchema, "hsk.studio.layer@1");
schema_token!(ArtboardSchema, "hsk.studio.artboard@1");
schema_token!(PageSpreadSchema, "hsk.studio.page_spread@1");
schema_token!(GraphSchema, "hsk.studio.layer_graph@1");
schema_token!(TileSchema, "hsk.studio.raster_tile@1");
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum GeometryUnit {
    #[serde(rename = "mm")]
    Mm,
    #[serde(rename = "in")]
    In,
    #[serde(rename = "px")]
    Px,
    #[serde(rename = "pt")]
    Pt,
}
impl GeometryUnit {
    fn accord(self) -> AccordUnit {
        match self {
            Self::Mm => AccordUnit::Millimetres,
            Self::In => AccordUnit::Inches,
            Self::Px => AccordUnit::Pixels,
            Self::Pt => AccordUnit::Points,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Length {
    pub value: f64,
    pub unit: GeometryUnit,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    pub x: Length,
    pub y: Length,
    pub width: Length,
    pub height: Length,
}
/// Non-Option wire field: null is a required explicit value, never a decoder default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ProfileBinding {
    Unassigned(()),
    Assigned(String),
}
fn required<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<T, D::Error> {
    T::deserialize(d)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    Layer,
    Artboard,
    PageSpread,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeRef {
    pub node_kind: NodeKind,
    pub node_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Parent {
    Root(()),
    Node(NodeRef),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioDocument {
    pub schema_id: DocumentSchema,
    pub document_id: String,
    pub revision: u64,
    pub geometry_unit: GeometryUnit,
    #[schemars(range(min = 1, max = 8))]
    pub colour_mode: u8,
    #[schemars(extend("enum"=[1,8,16,32]))]
    pub bits_per_channel: u8,
    #[serde(deserialize_with = "required")]
    pub working_space_rgb: ProfileBinding,
    #[serde(deserialize_with = "required")]
    pub working_space_cmyk: ProfileBinding,
    #[serde(deserialize_with = "required")]
    pub working_space_gray: ProfileBinding,
    #[serde(deserialize_with = "required")]
    pub working_space_spot: ProfileBinding,
    #[serde(deserialize_with = "required")]
    pub blending_space: ProfileBinding,
    pub artboards: Vec<StudioArtboard>,
    pub page_spreads: Vec<StudioPageSpread>,
    pub layers: Vec<StudioLayer>,
    pub graph: StudioLayerGraph,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioArtboard {
    pub schema_id: ArtboardSchema,
    pub artboard_id: String,
    pub name: String,
    #[serde(deserialize_with = "required")]
    pub parent: Parent,
    pub bounds: Bounds,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioPageSpread {
    pub schema_id: PageSpreadSchema,
    pub page_spread_id: String,
    pub name: String,
    #[serde(deserialize_with = "required")]
    pub parent: Parent,
    pub bounds: Bounds,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLayer {
    pub schema_id: LayerSchema,
    pub layer_id: String,
    pub name: String,
    #[serde(deserialize_with = "required")]
    pub parent: Parent,
    /// Open domain vocabulary; unsupported preservation is explicit, never a generic editable bag.
    pub kind: String,
    pub payload: Payload,
    pub visible: bool,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "payload_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Payload {
    Group,
    Raster {
        tiles: Vec<TileRef>,
    },
    Vector {
        path_ids: Vec<String>,
    },
    Text {
        story_id: String,
    },
    PrimitiveReference {
        schema_id: String,
        object_id: String,
    },
    Unsupported {
        kind: String,
        schema_id: String,
        encoded_bytes: Vec<u8>,
        reason: String,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContentDigest {
    pub algorithm: String,
    pub digest: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TileRef {
    pub object_key: String,
    pub layer_id: String,
    pub column: i64,
    pub row: i64,
    pub width: Length,
    pub height: Length,
    pub format: String,
    pub colour_profile_id: String,
    pub schema_id: TileSchema,
    pub content_digest: ContentDigest,
    pub artifact_manifest_id: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLayerGraph {
    pub schema_id: GraphSchema,
    pub operations: Vec<Operation>,
    pub edges: Vec<Edge>,
    pub outputs: Vec<PortAddress>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Cardinality {
    One,
    Optional,
    Many,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Image,
    Vector,
    Text,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Semantic {
    Source,
    OrderedComposite,
    CompositeResult,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Source,
    Composite,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Data,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Port {
    #[schemars(regex(pattern = "^[A-Za-z0-9_-]{1,128}$"))]
    pub port_key: String,
    pub value_type: ValueType,
    pub cardinality: Cardinality,
    pub semantic: Semantic,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub layer_id: String,
    #[schemars(regex(pattern = "^[A-Za-z0-9_-]{1,128}$"))]
    pub operation_key: String,
    pub operation_kind: OperationKind,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PortAddress {
    pub layer_id: String,
    pub operation_key: String,
    pub port_key: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Edge {
    pub source: PortAddress,
    pub target: PortAddress,
    pub dependency_kind: DependencyKind,
    pub input_ordinal: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resolution {
    Available,
    Absent,
    HashMismatch,
    Unauthorized,
    Unsupported,
}
/// Existing handle grammar and byte/grant authority are supplied by their actual owner.
/// Folio does not mint a PRIM or ArtifactManifest grammar or cache a catalog.
pub trait Resolver {
    fn primitive(&self, schema_id: &str, object_id: &str) -> Resolution;
    fn profile(&self, profile_id: &str) -> Resolution;
    fn tile(&self, tile: &TileRef) -> Resolution;
    fn format_supported(&self, format: &str) -> bool;
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool;
}
/// No resolver is falsely treated as an available provider.
pub struct Unresolved;
impl Resolver for Unresolved {
    fn primitive(&self, _: &str, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn profile(&self, _: &str) -> Resolution {
        Resolution::Absent
    }
    fn tile(&self, _: &TileRef) -> Resolution {
        Resolution::Absent
    }
    fn format_supported(&self, _: &str) -> bool {
        false
    }
    fn primitive_layer_binding(&self, _: &str, _: &str) -> bool {
        false
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolutionIssue {
    pub target: String,
    pub disposition: Resolution,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    InvalidJson,
    MissingField,
    InvalidField,
    DuplicateField,
    InvalidId,
    InvalidUnit,
    InvalidColourDepth,
    DuplicateIdentity,
    DanglingReference,
    IllegalParent,
    ContainmentCycle,
    PayloadMismatch,
    UnsupportedSchema,
    UnknownField,
    UnsupportedDescriptor,
    InvalidPort,
    InvalidEdge,
    Cardinality,
    RenderCycle,
    Unavailable,
    StaleRevision,
    RevisionOverflow,
    InvalidSuccessor,
    Canceled,
    BudgetExceeded,
    UnsupportedMutation,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub target: String,
}
fn err(code: Code, target: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code,
        target: target.into(),
    }
}
type Result<T> = std::result::Result<T, Diagnostic>;
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub input_bytes: usize,
    pub nodes: usize,
    pub edges: usize,
    pub payload_bytes: usize,
    pub depth: usize,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            input_bytes: 1_048_576,
            nodes: 4096,
            edges: 8192,
            payload_bytes: 262_144,
            depth: 32,
        }
    }
}
fn canceled(token: &CancellationToken) -> Result<()> {
    token.check().map_err(|_| err(Code::Canceled, "document"))
}
fn key(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn id(s: &str, prefix: &str, target: &str) -> Result<()> {
    let d = DomainId::parse(s).map_err(|_| err(Code::InvalidId, target))?;
    if d.prefix() != prefix {
        return Err(err(Code::InvalidId, target));
    }
    Ok(())
}
fn finite_length(l: &Length, unit: GeometryUnit, size: bool, target: &str) -> Result<()> {
    let a =
        AccordLength::new(l.value, l.unit.accord()).map_err(|_| err(Code::InvalidUnit, target))?;
    if l.unit != unit || (size && a.nonnegative().is_err()) {
        return Err(err(Code::InvalidUnit, target));
    }
    Ok(())
}
fn bounds(b: &Bounds, unit: GeometryUnit, target: &str) -> Result<()> {
    finite_length(&b.x, unit, false, target)?;
    finite_length(&b.y, unit, false, target)?;
    finite_length(&b.width, unit, true, target)?;
    finite_length(&b.height, unit, true, target)
}
fn cycle_check(
    nodes: &BTreeSet<String>,
    links: &[(String, String)],
    token: &CancellationToken,
    code: Code,
) -> Result<()> {
    let mut degrees: BTreeMap<String, usize> = nodes.iter().map(|n| (n.clone(), 0)).collect();
    let mut next: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (a, b) in links {
        canceled(token)?;
        *degrees
            .get_mut(b)
            .ok_or_else(|| err(Code::DanglingReference, b))? += 1;
        next.entry(a).or_default().push(b);
    }
    let mut ready: Vec<String> = degrees
        .iter()
        .filter(|(_, d)| **d == 0)
        .map(|(n, _)| n.clone())
        .collect();
    let mut seen = 0;
    while let Some(n) = ready.pop() {
        canceled(token)?;
        seen += 1;
        if let Some(children) = next.get(n.as_str()) {
            for child in children {
                let d = degrees
                    .get_mut(*child)
                    .ok_or_else(|| err(Code::DanglingReference, *child))?;
                *d -= 1;
                if *d == 0 {
                    ready.push((*child).to_owned());
                }
            }
        }
    }
    if seen != nodes.len() {
        return Err(err(code, "graph"));
    }
    Ok(())
}
fn validate(
    d: &StudioDocument,
    b: Budget,
    token: &CancellationToken,
    r: &dyn Resolver,
) -> Result<Vec<ResolutionIssue>> {
    canceled(token)?;
    if d.artboards
        .len()
        .checked_add(d.page_spreads.len())
        .and_then(|n| n.checked_add(d.layers.len()))
        .and_then(|n| n.checked_add(d.graph.operations.len()))
        .is_none_or(|n| n > b.nodes)
        || d.graph.edges.len() > b.edges
    {
        return Err(err(Code::BudgetExceeded, "document"));
    }
    id(&d.document_id, "SDOC", "document_id")?;
    if !(1..=8).contains(&d.colour_mode)
        || ![1, 8, 16, 32].contains(&d.bits_per_channel)
        || (d.bits_per_channel == 1 && d.colour_mode != 5)
    {
        return Err(err(
            Code::InvalidColourDepth,
            "colour_mode/bits_per_channel",
        ));
    }
    let mut issues = Vec::new();
    for (name, p) in [
        ("working_space_rgb", &d.working_space_rgb),
        ("working_space_cmyk", &d.working_space_cmyk),
        ("working_space_gray", &d.working_space_gray),
        ("working_space_spot", &d.working_space_spot),
        ("blending_space", &d.blending_space),
    ] {
        canceled(token)?;
        if let ProfileBinding::Assigned(v) = p {
            id(v, "SCPF", name)?;
            let disposition = r.profile(v);
            if disposition != Resolution::Available {
                issues.push(ResolutionIssue {
                    target: name.into(),
                    disposition,
                });
            }
        }
    }
    let mut nodes = BTreeMap::<String, (NodeKind, bool, &Parent)>::new();
    for a in &d.artboards {
        canceled(token)?;
        id(&a.artboard_id, "SART", &a.artboard_id)?;
        bounds(&a.bounds, d.geometry_unit, &a.artboard_id)?;
        if nodes
            .insert(a.artboard_id.clone(), (NodeKind::Artboard, true, &a.parent))
            .is_some()
        {
            return Err(err(Code::DuplicateIdentity, &a.artboard_id));
        }
    }
    for a in &d.page_spreads {
        canceled(token)?;
        id(&a.page_spread_id, "SPGS", &a.page_spread_id)?;
        bounds(&a.bounds, d.geometry_unit, &a.page_spread_id)?;
        if nodes
            .insert(
                a.page_spread_id.clone(),
                (NodeKind::PageSpread, true, &a.parent),
            )
            .is_some()
        {
            return Err(err(Code::DuplicateIdentity, &a.page_spread_id));
        }
    }
    let mut payload_size = 0usize;
    for l in &d.layers {
        canceled(token)?;
        id(&l.layer_id, "SLYR", &l.layer_id)?;
        let initial_binding = match l.kind.as_str() {
            "group" => matches!(l.payload, Payload::Group),
            "raster" => matches!(l.payload, Payload::Raster { .. }),
            "vector" => matches!(l.payload, Payload::Vector { .. }),
            "text" => matches!(l.payload, Payload::Text { .. }),
            _ => true,
        };
        if !initial_binding {
            return Err(err(Code::PayloadMismatch, &l.layer_id));
        }
        let group = l.kind == "group" && matches!(l.payload, Payload::Group);
        if nodes
            .insert(l.layer_id.clone(), (NodeKind::Layer, group, &l.parent))
            .is_some()
        {
            return Err(err(Code::DuplicateIdentity, &l.layer_id));
        }
        let owned_bytes = match &l.payload {
            Payload::Group => 0,
            Payload::Raster { tiles } => tiles
                .iter()
                .try_fold(0usize, |n, t| {
                    n.checked_add(t.object_key.len())
                        .and_then(|n| n.checked_add(t.layer_id.len()))
                        .and_then(|n| n.checked_add(t.format.len()))
                        .and_then(|n| n.checked_add(t.colour_profile_id.len()))
                        .and_then(|n| n.checked_add(t.content_digest.algorithm.len()))
                        .and_then(|n| n.checked_add(t.content_digest.digest.len()))
                        .and_then(|n| n.checked_add(t.artifact_manifest_id.len()))
                        .and_then(|n| n.checked_add(128))
                })
                .ok_or_else(|| err(Code::BudgetExceeded, &l.layer_id))?,
            Payload::Vector { path_ids } => path_ids
                .iter()
                .try_fold(0usize, |n, v| n.checked_add(v.len()))
                .ok_or_else(|| err(Code::BudgetExceeded, &l.layer_id))?,
            Payload::Text { story_id } => story_id.len(),
            Payload::PrimitiveReference {
                schema_id,
                object_id,
            } => schema_id
                .len()
                .checked_add(object_id.len())
                .ok_or_else(|| err(Code::BudgetExceeded, &l.layer_id))?,
            Payload::Unsupported {
                kind,
                schema_id,
                encoded_bytes,
                reason,
            } => kind
                .len()
                .checked_add(schema_id.len())
                .and_then(|n| n.checked_add(encoded_bytes.len()))
                .and_then(|n| n.checked_add(reason.len()))
                .ok_or_else(|| err(Code::BudgetExceeded, &l.layer_id))?,
        };
        payload_size = payload_size
            .checked_add(owned_bytes)
            .ok_or_else(|| err(Code::BudgetExceeded, &l.layer_id))?;
        match (&*l.kind, &l.payload) {
            ("group", Payload::Group) => {}
            ("raster", Payload::Raster { tiles }) => {
                let mut keys = BTreeSet::new();
                for t in tiles {
                    canceled(token)?;
                    if t.layer_id != l.layer_id
                        || !key(&t.object_key)
                        || !keys.insert(&t.object_key)
                    {
                        return Err(err(Code::PayloadMismatch, &l.layer_id));
                    }
                    finite_length(&t.width, GeometryUnit::Px, true, &l.layer_id)?;
                    finite_length(&t.height, GeometryUnit::Px, true, &l.layer_id)?;
                    if t.width.value <= 0.0
                        || t.height.value <= 0.0
                        || t.width.value.fract() != 0.0
                        || t.height.value.fract() != 0.0
                    {
                        return Err(err(Code::InvalidUnit, &l.layer_id));
                    }
                    id(&t.colour_profile_id, "SCPF", &l.layer_id)?;
                    if t.artifact_manifest_id.is_empty()
                        || t.content_digest.algorithm.is_empty()
                        || t.content_digest.digest.is_empty()
                    {
                        return Err(err(Code::InvalidField, &l.layer_id));
                    }
                    let disposition = if !r.format_supported(&t.format) {
                        Resolution::Unsupported
                    } else {
                        r.tile(t)
                    };
                    if disposition != Resolution::Available {
                        issues.push(ResolutionIssue {
                            target: format!("{}/tile/{}", l.layer_id, t.object_key),
                            disposition,
                        });
                    }
                }
            }
            ("vector", Payload::Vector { path_ids }) => {
                for v in path_ids {
                    canceled(token)?;
                    id(v, "SVPT", &l.layer_id)?;
                    let disposition = r.primitive(hsk_studio_accord::SCHEMA_STUDIO_VECTOR_PATH, v);
                    if disposition != Resolution::Available {
                        issues.push(ResolutionIssue {
                            target: format!("{}/path/{}", l.layer_id, v),
                            disposition,
                        });
                    }
                }
            }
            ("text", Payload::Text { story_id }) => {
                id(story_id, "STXT", &l.layer_id)?;
                let disposition =
                    r.primitive(hsk_studio_accord::SCHEMA_STUDIO_TEXT_STORY, story_id);
                if disposition != Resolution::Available {
                    issues.push(ResolutionIssue {
                        target: format!("{}/story", l.layer_id),
                        disposition,
                    });
                }
            }
            (
                _,
                Payload::Unsupported {
                    kind,
                    schema_id,
                    encoded_bytes,
                    reason,
                },
            ) => {
                if kind != &l.kind || schema_id.is_empty() || reason.is_empty() {
                    return Err(err(Code::PayloadMismatch, &l.layer_id));
                }
                let _ = encoded_bytes;
                issues.push(ResolutionIssue {
                    target: l.layer_id.clone(),
                    disposition: Resolution::Unsupported,
                });
            }
            (
                kind,
                Payload::PrimitiveReference {
                    schema_id,
                    object_id,
                },
            ) => {
                if !r.primitive_layer_binding(kind, schema_id) {
                    return Err(err(Code::UnsupportedDescriptor, &l.layer_id));
                }
                SchemaId::parse(schema_id)
                    .map_err(|_| err(Code::UnsupportedSchema, &l.layer_id))?;
                let disposition = r.primitive(schema_id, object_id);
                if disposition != Resolution::Available {
                    issues.push(ResolutionIssue {
                        target: l.layer_id.clone(),
                        disposition,
                    });
                }
            }
            _ => return Err(err(Code::PayloadMismatch, &l.layer_id)),
        }
        if payload_size > b.payload_bytes {
            return Err(err(Code::BudgetExceeded, &l.layer_id));
        }
    }
    let mut containment = Vec::new();
    for (n, (_, _, parent)) in &nodes {
        canceled(token)?;
        if let Parent::Node(p) = parent {
            let (kind, capable, _) = nodes
                .get(&p.node_id)
                .ok_or_else(|| err(Code::DanglingReference, n))?;
            if &p.node_kind != kind || !*capable {
                return Err(err(Code::IllegalParent, n));
            }
            containment.push((p.node_id.clone(), n.clone()));
        }
    }
    cycle_check(
        &nodes.keys().cloned().collect(),
        &containment,
        token,
        Code::ContainmentCycle,
    )?;
    let layers: BTreeMap<&str, &StudioLayer> =
        d.layers.iter().map(|l| (l.layer_id.as_str(), l)).collect();
    let mut ops = BTreeSet::new();
    let mut ports = BTreeMap::<PortAddress, (bool, &Port)>::new();
    let mut input_counts = BTreeMap::<PortAddress, BTreeSet<u64>>::new();
    for op in &d.graph.operations {
        canceled(token)?;
        let l = layers
            .get(op.layer_id.as_str())
            .ok_or_else(|| err(Code::DanglingReference, &op.layer_id))?;
        if !key(&op.operation_key) || !ops.insert(format!("{}/{}", op.layer_id, op.operation_key)) {
            return Err(err(Code::DuplicateIdentity, &op.layer_id));
        }
        let expected = match op.operation_kind {
            OperationKind::Source => {
                let ty = match l.kind.as_str() {
                    "raster" => ValueType::Image,
                    "vector" => ValueType::Vector,
                    "text" => ValueType::Text,
                    _ => return Err(err(Code::UnsupportedDescriptor, &op.layer_id)),
                };
                (
                    Vec::new(),
                    vec![Port {
                        port_key: "out".into(),
                        value_type: ty,
                        cardinality: Cardinality::One,
                        semantic: Semantic::Source,
                    }],
                )
            }
            OperationKind::Composite
                if l.kind == "group" && matches!(l.payload, Payload::Group) =>
            {
                (
                    vec![Port {
                        port_key: "inputs".into(),
                        value_type: ValueType::Image,
                        cardinality: Cardinality::Many,
                        semantic: Semantic::OrderedComposite,
                    }],
                    vec![Port {
                        port_key: "out".into(),
                        value_type: ValueType::Image,
                        cardinality: Cardinality::One,
                        semantic: Semantic::CompositeResult,
                    }],
                )
            }
            _ => return Err(err(Code::UnsupportedDescriptor, &op.layer_id)),
        };
        if op.inputs != expected.0 || op.outputs != expected.1 {
            return Err(err(
                Code::InvalidPort,
                format!("{}/{}", op.layer_id, op.operation_key),
            ));
        }
        for (output, list) in [(false, &op.inputs), (true, &op.outputs)] {
            for port in list {
                canceled(token)?;
                let address = PortAddress {
                    layer_id: op.layer_id.clone(),
                    operation_key: op.operation_key.clone(),
                    port_key: port.port_key.clone(),
                };
                if !key(&port.port_key) || ports.insert(address.clone(), (output, port)).is_some() {
                    return Err(err(Code::InvalidPort, &op.layer_id));
                }
                if !output {
                    input_counts.insert(address, BTreeSet::new());
                }
            }
        }
    }
    let mut links = Vec::new();
    let mut edge_keys = BTreeSet::new();
    for edge in &d.graph.edges {
        canceled(token)?;
        let (source_output, source) = ports
            .get(&edge.source)
            .ok_or_else(|| err(Code::DanglingReference, address_text(&edge.source)))?;
        let (target_output, target) = ports
            .get(&edge.target)
            .ok_or_else(|| err(Code::DanglingReference, address_text(&edge.target)))?;
        if !*source_output
            || *target_output
            || source.value_type != target.value_type
            || edge.dependency_kind != DependencyKind::Data
        {
            return Err(err(Code::InvalidEdge, address_text(&edge.target)));
        }
        if !edge_keys.insert((edge.source.clone(), edge.target.clone()))
            || !input_counts
                .get_mut(&edge.target)
                .ok_or_else(|| err(Code::InvalidEdge, address_text(&edge.target)))?
                .insert(edge.input_ordinal)
        {
            return Err(err(Code::InvalidEdge, address_text(&edge.target)));
        }
        links.push((
            format!("{}/{}", edge.source.layer_id, edge.source.operation_key),
            format!("{}/{}", edge.target.layer_id, edge.target.operation_key),
        ));
    }
    for (address, ordinals) in input_counts {
        canceled(token)?;
        let (_, port) = ports
            .get(&address)
            .ok_or_else(|| err(Code::InvalidPort, address_text(&address)))?;
        if ordinals
            .iter()
            .enumerate()
            .any(|(n, v)| u64::try_from(n).ok() != Some(*v))
            || match port.cardinality {
                Cardinality::One => ordinals.len() != 1,
                Cardinality::Optional => ordinals.len() > 1,
                Cardinality::Many => false,
            }
        {
            return Err(err(Code::Cardinality, address_text(&address)));
        }
    }
    for address in &d.graph.outputs {
        canceled(token)?;
        if !ports.get(address).is_some_and(|(output, _)| *output) {
            return Err(err(Code::DanglingReference, address_text(address)));
        }
    }
    cycle_check(&ops, &links, token, Code::RenderCycle)?;
    canceled(token)?;
    Ok(issues)
}
fn address_text(a: &PortAddress) -> String {
    format!("{}/{}/{}", a.layer_id, a.operation_key, a.port_key)
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    document: Arc<StudioDocument>,
    encoded: Arc<[u8]>,
    issues: Vec<ResolutionIssue>,
}
impl Snapshot {
    pub fn document(&self) -> &StudioDocument {
        &self.document
    }
    pub fn encoded_bytes(&self) -> &[u8] {
        &self.encoded
    }
    pub fn resolution_issues(&self) -> &[ResolutionIssue] {
        &self.issues
    }
    pub fn require_available(&self) -> Result<()> {
        if let Some(i) = self.issues.first() {
            return Err(err(Code::Unavailable, &i.target));
        }
        if [
            &self.document.working_space_rgb,
            &self.document.working_space_cmyk,
            &self.document.working_space_gray,
            &self.document.working_space_spot,
            &self.document.blending_space,
        ]
        .iter()
        .any(|p| matches!(p, ProfileBinding::Unassigned(())))
        {
            return Err(err(Code::Unavailable, "unassigned_profile"));
        }
        Ok(())
    }
    /// A real prospective-document edit. Caller revision checks are source-local only.
    // Explicit caller preconditions and bounded execution context stay separate from authored fields.
    #[allow(clippy::too_many_arguments)]
    pub fn rename(
        &self,
        target: &str,
        name: String,
        expected_revision: u64,
        successor: u64,
        b: Budget,
        token: &CancellationToken,
        r: &dyn Resolver,
    ) -> Result<Self> {
        canceled(token)?;
        if expected_revision != self.document.revision {
            return Err(err(Code::StaleRevision, "revision"));
        }
        let next = expected_revision
            .checked_add(1)
            .ok_or_else(|| err(Code::RevisionOverflow, "revision"))?;
        if successor != next {
            return Err(err(Code::InvalidSuccessor, "revision"));
        }
        if name.len() > b.payload_bytes {
            return Err(err(Code::BudgetExceeded, target));
        }
        if self
            .document
            .layers
            .iter()
            .any(|l| l.layer_id == target && matches!(l.payload, Payload::Unsupported { .. }))
        {
            return Err(err(Code::UnsupportedMutation, target));
        }
        let mut d = (*self.document).clone();
        let mut found = false;
        for l in &mut d.layers {
            canceled(token)?;
            if l.layer_id == target {
                l.name = name.clone();
                found = true;
            }
        }
        for l in &mut d.artboards {
            canceled(token)?;
            if l.artboard_id == target {
                l.name = name.clone();
                found = true;
            }
        }
        for l in &mut d.page_spreads {
            canceled(token)?;
            if l.page_spread_id == target {
                l.name = name.clone();
                found = true;
            }
        }
        if !found {
            return Err(err(Code::DanglingReference, target));
        }
        d.revision = successor;
        validate_document(d, b, token, r)
    }
    pub fn ordered_inputs(&self, address: &PortAddress) -> Vec<&PortAddress> {
        let mut e: Vec<_> = self
            .document
            .graph
            .edges
            .iter()
            .filter(|e| &e.target == address)
            .collect();
        e.sort_by_key(|e| e.input_ordinal);
        e.into_iter().map(|e| &e.source).collect()
    }
}
#[derive(Clone, Debug)]
pub struct ReadOnlyInspection {
    encoded: Arc<[u8]>,
    diagnostic: Diagnostic,
}
impl ReadOnlyInspection {
    pub fn encoded_bytes(&self) -> &[u8] {
        &self.encoded
    }
    pub fn diagnostic(&self) -> &Diagnostic {
        &self.diagnostic
    }
    pub fn refuse_mutation(&self) -> Result<()> {
        Err(err(Code::UnsupportedMutation, &self.diagnostic.target))
    }
}
#[derive(Clone, Debug)]
pub enum Inspection {
    Editable(Snapshot),
    ReadOnly(ReadOnlyInspection),
}
struct BoundedOutput<'a> {
    bytes: Vec<u8>,
    limit: usize,
    token: &'a CancellationToken,
    failure: Option<Code>,
}
impl std::io::Write for BoundedOutput<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.token.check().is_err() {
            self.failure = Some(Code::Canceled);
            return Err(std::io::Error::other("canceled"));
        }
        if self
            .bytes
            .len()
            .checked_add(bytes.len())
            .is_none_or(|n| n > self.limit)
        {
            self.failure = Some(Code::BudgetExceeded);
            return Err(std::io::Error::other("budget"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub fn validate_document(
    d: StudioDocument,
    b: Budget,
    t: &CancellationToken,
    r: &dyn Resolver,
) -> Result<Snapshot> {
    let issues = validate(&d, b, t, r)?;
    let mut output = BoundedOutput {
        bytes: Vec::new(),
        limit: b.input_bytes,
        token: t,
        failure: None,
    };
    serde_json::to_writer(&mut output, &d)
        .map_err(|_| err(output.failure.unwrap_or(Code::InvalidField), "document"))?;
    canceled(t)?;
    Ok(Snapshot {
        document: Arc::new(d),
        encoded: output.bytes.into(),
        issues,
    })
}
/// Duplicate JSON object fields are rejected rather than silently overwritten by Value.
struct UniqueValue(Value);
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = UniqueValue;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON value with unique fields")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| UniqueValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(v.into()))
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(v) = a.next_element::<UniqueValue>()? {
                    values.push(v.0);
                }
                Ok(UniqueValue(Value::Array(values)))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = serde_json::Map::new();
                while let Some(k) = a.next_key::<String>()? {
                    if values.contains_key(&k) {
                        return Err(de::Error::custom("duplicate_field"));
                    }
                    let v = a.next_value::<UniqueValue>()?;
                    values.insert(k, v.0);
                }
                Ok(UniqueValue(Value::Object(values)))
            }
        }
        d.deserialize_any(V)
    }
}
fn preflight(input: &[u8], b: Budget, t: &CancellationToken) -> Result<()> {
    if input.len() > b.input_bytes || b.depth == 0 || b.depth > 64 {
        return Err(err(Code::BudgetExceeded, "input"));
    }
    let (mut depth, mut quoted, mut escape) = (0usize, false, false);
    for byte in input {
        canceled(t)?;
        if quoted {
            if escape {
                escape = false;
            } else if *byte == b'\\' {
                escape = true;
            } else if *byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > b.depth {
                        return Err(err(Code::BudgetExceeded, "input_depth"));
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    Ok(())
}
/// Known typed objects are closed; schema extensions preserve the entire original input.
fn extension(v: &Value, target: &str, t: &CancellationToken) -> Result<Option<Diagnostic>> {
    canceled(t)?;
    if let Value::Object(o) = v {
        for (field, known) in [
            ("operation_kind", &["source", "composite"][..]),
            ("value_type", &["image", "vector", "text"][..]),
            (
                "semantic",
                &["source", "ordered_composite", "composite_result"][..],
            ),
            ("dependency_kind", &["data"][..]),
        ] {
            if o.get(field)
                .and_then(Value::as_str)
                .is_some_and(|v| !known.contains(&v))
            {
                return Ok(Some(err(
                    Code::UnsupportedDescriptor,
                    format!("{target}/{field}"),
                )));
            }
        }
        if let Some(Value::String(schema)) = o.get("schema_id") {
            let expected = if o.contains_key("document_id") {
                Some("hsk.studio.document@1")
            } else if o.contains_key("artboard_id") {
                Some("hsk.studio.artboard@1")
            } else if o.contains_key("page_spread_id") {
                Some("hsk.studio.page_spread@1")
            } else if o.contains_key("layer_id") && o.contains_key("payload") {
                Some("hsk.studio.layer@1")
            } else if o.contains_key("operations") {
                Some("hsk.studio.layer_graph@1")
            } else if o.contains_key("object_key") {
                Some("hsk.studio.raster_tile@1")
            } else {
                None
            };
            if expected.is_some_and(|e| schema != e) {
                return Ok(Some(err(Code::UnsupportedSchema, target)));
            }
        }
        let allowed: Option<&[&str]> = if o.contains_key("document_id") {
            Some(&[
                "schema_id",
                "document_id",
                "revision",
                "geometry_unit",
                "colour_mode",
                "bits_per_channel",
                "working_space_rgb",
                "working_space_cmyk",
                "working_space_gray",
                "working_space_spot",
                "blending_space",
                "artboards",
                "page_spreads",
                "layers",
                "graph",
            ])
        } else if o.contains_key("artboard_id") {
            Some(&["schema_id", "artboard_id", "name", "parent", "bounds"])
        } else if o.contains_key("page_spread_id") {
            Some(&["schema_id", "page_spread_id", "name", "parent", "bounds"])
        } else if o.contains_key("payload") {
            Some(&[
                "schema_id",
                "layer_id",
                "name",
                "parent",
                "kind",
                "payload",
                "visible",
            ])
        } else if o.contains_key("payload_kind") {
            match o.get("payload_kind").and_then(Value::as_str) {
                Some("group") => Some(&["payload_kind"]),
                Some("raster") => Some(&["payload_kind", "tiles"]),
                Some("vector") => Some(&["payload_kind", "path_ids"]),
                Some("text") => Some(&["payload_kind", "story_id"]),
                Some("primitive_reference") => Some(&["payload_kind", "schema_id", "object_id"]),
                Some("unsupported") => Some(&[
                    "payload_kind",
                    "kind",
                    "schema_id",
                    "encoded_bytes",
                    "reason",
                ]),
                _ => return Ok(Some(err(Code::UnsupportedDescriptor, target))),
            }
        } else if o.contains_key("operations") {
            Some(&["schema_id", "operations", "edges", "outputs"])
        } else if o.contains_key("operation_kind") {
            Some(&[
                "layer_id",
                "operation_key",
                "operation_kind",
                "inputs",
                "outputs",
            ])
        } else if o.contains_key("cardinality") {
            Some(&["port_key", "value_type", "cardinality", "semantic"])
        } else if o.contains_key("operation_key") {
            Some(&["layer_id", "operation_key", "port_key"])
        } else if o.contains_key("source") {
            Some(&["source", "target", "dependency_kind", "input_ordinal"])
        } else if o.contains_key("node_kind") {
            Some(&["node_kind", "node_id"])
        } else if o.contains_key("object_key") {
            Some(&[
                "object_key",
                "layer_id",
                "column",
                "row",
                "width",
                "height",
                "format",
                "colour_profile_id",
                "schema_id",
                "content_digest",
                "artifact_manifest_id",
            ])
        } else if o.contains_key("algorithm") {
            Some(&["algorithm", "digest"])
        } else if o.contains_key("value") {
            Some(&["value", "unit"])
        } else if o.contains_key("x") {
            Some(&["x", "y", "width", "height"])
        } else {
            None
        };
        if let Some(allowed) = allowed {
            if o.keys().any(|k| !allowed.contains(&k.as_str())) {
                return Ok(Some(err(
                    Code::UnknownField,
                    format!("{target}/unknown_field"),
                )));
            }
        }
        for (k, v) in o {
            if let Some(d) = extension(v, &format!("{target}/{k}"), t)? {
                return Ok(Some(d));
            }
        }
    } else if let Value::Array(a) = v {
        for (n, v) in a.iter().enumerate() {
            if let Some(d) = extension(v, &format!("{target}/{n}"), t)? {
                return Ok(Some(d));
            }
        }
    }
    Ok(None)
}
pub fn inspect_bytes(
    input: &[u8],
    b: Budget,
    t: &CancellationToken,
    r: &dyn Resolver,
) -> Result<Inspection> {
    preflight(input, b, t)?;
    let value = serde_json::from_slice::<UniqueValue>(input)
        .map_err(|e| {
            err(
                if e.to_string().contains("duplicate_field") {
                    Code::DuplicateField
                } else {
                    Code::InvalidJson
                },
                "input",
            )
        })?
        .0;
    if let Some(diagnostic) = extension(&value, "document", t)? {
        return Ok(Inspection::ReadOnly(ReadOnlyInspection {
            encoded: Arc::from(input),
            diagnostic,
        }));
    }
    let d: StudioDocument = match serde_json::from_value(value) {
        Ok(d) => d,
        Err(e) if e.to_string().contains("unknown field") => {
            return Ok(Inspection::ReadOnly(ReadOnlyInspection {
                encoded: Arc::from(input),
                diagnostic: err(Code::UnknownField, "document"),
            }));
        }
        Err(e) => {
            return Err(err(
                if e.to_string().contains("missing field") {
                    Code::MissingField
                } else {
                    Code::InvalidField
                },
                "document",
            ));
        }
    };
    let issues = validate(&d, b, t, r)?;
    canceled(t)?;
    Ok(Inspection::Editable(Snapshot {
        document: Arc::new(d),
        encoded: Arc::from(input),
        issues,
    }))
}
pub fn document_schema() -> schemars::Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<StudioDocument>()
}
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-FOLIO","version":1,"api":"inspect_bytes / validate_document / Snapshot::rename","consumer":"folio-consumer --input FILE --expected-revision U64 [--rename ID --name TEXT --successor U64] [--cancel] [--report] [--max-bytes N --max-nodes N --max-edges N --max-payload-bytes N]; --schema; --descriptor","input":"Published CON015-021 typed document; all nullable profiles and parents explicitly present; caller revision and limits required at mutation","output":"Immutable source-local document or typed rejection; unknown schema/fields preserve complete original bytes read-only; ordered composition edge projection","recovery":"Correct invalid fields; refresh caller revision; obtain typed domain/profile/ArtifactManifest resolver from existing owner; unavailable meaning blocks dependent operations; canceled or rejected edits preserve original bytes","manual":"Same pure wire contract and generated draft2020-12 schema for human/model/API consumers; no hidden defaults or private catalog","argus":{"inspect":"snapshot bytes, revision, resolution dispositions, stable diagnostic address","steer":"same bounded rename API","state":"immutable graph and ordered input projection","capture":"caller-owned granted capture of actual output"},"diagnostics":"Pure typed codes and target addresses adapted by caller through Observe/FlightRecorder/internal diagnostics/Palmistry; private document names/content are not diagnostic text","authority":"Caller revision is not host authority; grants, CKC/ArtifactService, Chronicle, Package, native GUI, color/render, DB, CRDT and EventLedger acceptance remain pending"}"#;
