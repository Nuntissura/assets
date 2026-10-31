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
mod appearance;
pub use appearance::*;
mod timeline;
pub use timeline::*;
mod layout;
pub use layout::*;
mod story;
pub use story::*;
mod values;
pub use values::{
    CubicBezier, SpringEasing, StudioColor, StudioDocumentationLink, StudioEasing, StudioPublishing,
    StudioRgba, StudioValue, VariableType,
};
mod paint;
pub use paint::*;
mod autolayout;
pub use autolayout::*;
mod reactions;
pub use reactions::*;
mod components;
pub use components::*;
mod records;
pub use records::*;
mod prototype;
pub use prototype::*;
mod identity;
pub use identity::{check_identity_uniqueness, check_variable_identity_uniqueness};
mod raster;
pub use raster::*;
mod channel;
pub use channel::*;
mod fill_layer;
pub use fill_layer::*;
mod smart_filter;
pub use smart_filter::*;
mod audio_preset;
pub use audio_preset::*;
mod motion_preset;
pub use motion_preset::*;
mod audio_routing;
pub use audio_routing::*;
mod vector;
pub use vector::*;
mod vector_markers;
pub use vector_markers::*;
mod live_shape;
pub use live_shape::*;
mod vector_network;
pub use vector_network::*;
mod width_profile;
pub use width_profile::*;
mod typestyle;
pub use typestyle::*;
mod composition;
pub use composition::*;
mod motion_records;
pub use motion_records::*;
mod clip_finishing;
pub use clip_finishing::*;
mod multicam;
pub use multicam::*;
mod proof_state;
pub use proof_state::*;
mod design_state;
pub use design_state::*;
mod parentpages;
pub use parentpages::*;
mod longdoc;
pub use longdoc::*;
mod layoutflow;
pub use layoutflow::*;
mod layout_grids;
pub use layout_grids::*;
mod layout_rules;
pub use layout_rules::*;
mod book;
pub use book::*;
mod cjk_composition;
pub use cjk_composition::*;
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
    /// Timeline members (STU-VID-010). Absent and empty are the same document; empty is not encoded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sequences: Vec<StudioSequence>,
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
    /// Pages of the spread, left to right (STU-LAY-001). Absent and empty are the same spread; empty
    /// is not encoded.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<StudioPage>,
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
    /// Opacity, fill opacity, blend mode, clipping, group isolation and mask reference. Absent is
    /// the kind's identity default and is not encoded; see [`StudioLayer::effective_appearance`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub appearance: Option<LayerAppearance>,
    /// Text-frame geometry and thread links of a `text` layer (STU-LAY-017/122). Absent is not a
    /// frame and is not encoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_frame: Option<StudioTextFrame>,
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
    /// Timeline clip media (`ClipSource::Media`). Defaults to Absent so no resolver is falsely
    /// treated as an available media provider.
    fn media(&self, _artifact_manifest_id: &str, _content_digest: &ContentDigest) -> Resolution {
        Resolution::Absent
    }
    /// Stored story content (`StoryContent::Stored`). Defaults to Absent for the same reason.
    fn story_content(
        &self,
        _artifact_manifest_id: &str,
        _content_digest: &ContentDigest,
    ) -> Resolution {
        Resolution::Absent
    }
    /// A font face named by a type style's `font_family` and `font_style` (empty when the style
    /// names none). Lane SMB-PACKAGE-COMPLETENESS-2; defaults to Absent like `media`.
    fn font(&self, _family: &str, _style: &str) -> Resolution {
        Resolution::Absent
    }
    /// A composition audio artifact (motion records `StudioAudioLayer.audio_id`). Defaults to
    /// Absent.
    fn audio(&self, _audio_id: &str) -> Resolution {
        Resolution::Absent
    }
    /// A colour lookup table artifact: `StudioAdjustment::ColourLookup.lut` without a carried
    /// `table` (no digest), or a clip-finishing `StudioLutRef` (manifest handle + content digest).
    /// Defaults to Absent.
    fn lut(&self, _lut: &str, _content_digest: Option<&ContentDigest>) -> Resolution {
        Resolution::Absent
    }
    /// A linked placed source (`StudioPlacedMode::Linked`): the file at the relative `uri`
    /// exists and its bytes match `content_digest` (Available), differ (HashMismatch), are gone
    /// (Absent) or cannot be checked (Unsupported). Defaults to Absent.
    fn placed_link(&self, _uri: &str, _content_digest: &str) -> Resolution {
        Resolution::Absent
    }
}

/// The catalog asset-id grammar of the CKC port (`Asset.asset_id: UUID`, kernel-bindings
/// `AssetId::new`): canonical lowercase UUID text, 36 bytes, hyphens at 8/13/18/23. Optional
/// `asset_id` members of media clips and placed sources must match it (STU-ASSET-005/006).
pub fn is_catalog_asset_id(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_digit() || (b'a'..=b'f').contains(c),
        })
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
    /// Appearance values out of range or inconsistent (opacity percent, isolation, mask tiles).
    InvalidAppearance,
    /// Blend discriminant outside the known `StudioBlendMode` set `1..=35`.
    UnknownBlendMode,
    /// Known blend mode that a layer cannot take (tool-only, or group-only on a leaf).
    InapplicableBlendMode,
    /// Text-frame thread links that are not reciprocal, or a story that is not one chain.
    InvalidThread,
    /// Text-frame thread links that loop back on themselves.
    ThreadCycle,
    /// A component-set member omits a declared variant property (STU-DS-173).
    VariantAssignmentIncomplete,
    /// Two members of one component set carry identical variant assignments (STU-DS-174).
    VariantAssignmentDuplicate,
    /// Variable aliases that return to a variable already on the chain (STU-DS-175).
    VariableAliasCycle,
    /// A layer clipped (`LayerAppearance::clipped`) to an `adjustment` layer in the composition
    /// graph; the target is the clipped layer. The spec defines a clipping base through its
    /// content and alpha (STU-RAS-015) and does not name an adjustment layer as one, so Folio
    /// refuses the arrangement (rule and test: `raster::validate_adjustment_clip_bases`,
    /// `adjustment_clip_base_rule`).
    AdjustmentClipBase,
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
        .and_then(|n| n.checked_add(timeline::node_count(d)))
        .and_then(|n| n.checked_add(layout::page_count(d)))
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
    let mut ids = identity::Namespaces::default();
    for a in &d.artboards {
        canceled(token)?;
        id(&a.artboard_id, "SART", &a.artboard_id)?;
        bounds(&a.bounds, d.geometry_unit, &a.artboard_id)?;
        ids.node(&a.artboard_id)?;
        nodes.insert(a.artboard_id.clone(), (NodeKind::Artboard, true, &a.parent));
    }
    for a in &d.page_spreads {
        canceled(token)?;
        id(&a.page_spread_id, "SPGS", &a.page_spread_id)?;
        bounds(&a.bounds, d.geometry_unit, &a.page_spread_id)?;
        ids.node(&a.page_spread_id)?;
        nodes.insert(
            a.page_spread_id.clone(),
            (NodeKind::PageSpread, true, &a.parent),
        );
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
        ids.node(&l.layer_id)?;
        nodes.insert(l.layer_id.clone(), (NodeKind::Layer, group, &l.parent));
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
        appearance::validate_layer(l, group, b, token, r, &mut payload_size, &mut issues)?;
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
    layout::validate_layout(d, token)?;
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
                    // SMB-FOLIO-VECTOR-IMAGE-PORT: a vector source may declare its single `out`
                    // port `image` (rasterisable vector) so a vector layer can be a group input.
                    // The vector-typed form is unchanged; edges still require equal value types.
                    "vector" => match op.outputs.as_slice() {
                        [
                            Port {
                                value_type: ValueType::Image,
                                ..
                            },
                        ] => ValueType::Image,
                        _ => ValueType::Vector,
                    },
                    // Lead decision G1 (SMB-FOLIO-PAINT-BLUR-STYLE): a text source may declare its
                    // single `out` port `image` instead of `text` (rasterised text). Edges require
                    // equal value types and group `inputs` are image-typed, so this is the one
                    // way text is authored over other layers inside a group. The text-typed form
                    // is unchanged; renderers that do not rasterise text refuse the image form.
                    "text" => match op.outputs.as_slice() {
                        [
                            Port {
                                value_type: ValueType::Image,
                                ..
                            },
                        ] => ValueType::Image,
                        _ => ValueType::Text,
                    },
                    // STU-RAS-007 lists `adjustment` as a layer kind; STU-CMP-092 makes the
                    // graph input order the stack order, so an adjustment layer is an image
                    // source like any other layer. The payload must name its own record.
                    "adjustment" => match &l.payload {
                        Payload::PrimitiveReference {
                            schema_id,
                            object_id,
                        } if schema_id == ADJUSTMENT_SCHEMA && *object_id == l.layer_id => {
                            ValueType::Image
                        }
                        Payload::PrimitiveReference { .. } => {
                            return Err(err(Code::PayloadMismatch, &op.layer_id));
                        }
                        _ => return Err(err(Code::UnsupportedDescriptor, &op.layer_id)),
                    },
                    // Lane SMB-PS-FILL-LAYERS (STU-RAS-007 `fill`): an image source whose payload
                    // names its own fill record under one of the three fill schemas.
                    "fill" if raster::hosts_fill_layer(l) => ValueType::Image,
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
    raster::validate_adjustment_clip_bases(d, token)?;
    for address in &d.graph.outputs {
        canceled(token)?;
        if !ports.get(address).is_some_and(|(output, _)| *output) {
            return Err(err(Code::DanglingReference, address_text(address)));
        }
    }
    cycle_check(&ops, &links, token, Code::RenderCycle)?;
    timeline::validate_sequences(d, b, token, r, payload_size, &mut issues)?;
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
            ("side", &["right_hand", "left_hand", "single_sided"][..]),
            (
                "first_baseline_offset",
                &[
                    "ascent",
                    "cap_height",
                    "leading",
                    "em_box_height",
                    "x_height",
                    "fixed",
                ][..],
            ),
            (
                "vertical_justification",
                &["top", "center", "bottom", "justify"][..],
            ),
            (
                "auto_sizing_type",
                &[
                    "off",
                    "height_only",
                    "width_only",
                    "height_and_width",
                    "height_and_width_proportionally",
                ][..],
            ),
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
            } else if o.contains_key("clip_id") {
                Some("hsk.studio.clip@1")
            } else if o.contains_key("track_id") {
                Some("hsk.studio.track@1")
            } else if o.contains_key("sequence_id") && o.contains_key("tracks") {
                Some("hsk.studio.sequence@1")
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
                "sequences",
            ])
        } else if o.contains_key("artboard_id") {
            Some(&["schema_id", "artboard_id", "name", "parent", "bounds"])
        } else if o.contains_key("page_spread_id") {
            Some(&[
                "schema_id",
                "page_spread_id",
                "name",
                "parent",
                "bounds",
                "pages",
            ])
        } else if o.contains_key("payload") {
            Some(&[
                "schema_id",
                "layer_id",
                "name",
                "parent",
                "kind",
                "payload",
                "visible",
                "appearance",
                "text_frame",
            ])
        } else if o.contains_key("fill_opacity") {
            Some(&[
                "opacity",
                "fill_opacity",
                "blend_mode",
                "clipped",
                "isolation",
                "mask",
            ])
        } else if o.contains_key("tiles") && o.contains_key("linked") {
            Some(&["tiles", "enabled", "linked"])
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
        } else if o.contains_key("text_column_count") {
            Some(&[
                "page_id",
                "bounds",
                "inset_spacing",
                "text_column_count",
                "text_column_gutter",
                "first_baseline_offset",
                "minimum_first_baseline_offset",
                "vertical_justification",
                "auto_sizing_type",
                "previous_text_frame",
                "next_text_frame",
            ])
        } else if o.contains_key("page_id") {
            Some(&["page_id", "width", "height", "side", "margins", "bleed"])
        } else if o.contains_key("inside_or_left") {
            Some(&["top", "bottom", "inside_or_left", "outside_or_right"])
        } else if o.contains_key("top") {
            Some(&["top", "right", "bottom", "left"])
        } else if o.contains_key("value") {
            Some(&["value", "unit"])
        } else if o.contains_key("x") {
            Some(&["x", "y", "width", "height"])
        } else {
            None
        };
        if let Some(allowed) = allowed
            && o.keys().any(|k| !allowed.contains(&k.as_str()))
        {
            return Ok(Some(err(
                Code::UnknownField,
                format!("{target}/unknown_field"),
            )));
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
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-FOLIO","version":1,"api":"inspect_bytes / validate_document / Snapshot::rename / validate_text_story / StudioDocument::text_threads / StudioDocument::locate_page / validate_design_records / validate_paint_style / design_records_schema / validate_raster_records / raster_records_schema / validate_channels / channel_list_budget / channel_tile_format / validate_vector_records / vector_records_schema / validate_multicam_records / parse_multicam_records / multicam_records_schema / validate_proof_records / parse_proof_records / proof_records_schema / validate_design_state_records / parse_design_state_records / design_state_records_schema / validate_layout_rule_records / layout_rule_records_schema / resolve_liquid_rule / validate_cjk_composition_records / cjk_composition_records_schema","consumer":"folio-consumer --input FILE --expected-revision U64 [--rename ID --name TEXT --successor U64] [--cancel] [--report] [--max-bytes N --max-nodes N --max-edges N --max-payload-bytes N]; --schema; --descriptor","input":"Published CON015-021 typed document; all nullable profiles and parents explicitly present; caller revision and limits required at mutation; optional timeline members sequences/tracks/clips (STU-VID-010/020/021, integer Ticks, ordered non-overlapping clips, content-addressed media) counted against Budget.nodes, with sequence markers and clip markers (STU-VID-034 StudioMarker: document-unique key id, name, comment, LabelColor, start and duration ticks, explicit-null role chapter|web_link|cue_point|caption; clip markers in source ticks; at most 65536 per sequence, checked before any is examined) and clip head/tail transitions (STU-VID-032 hsk.studio.transition@1, Rust TimelineTransition: document-unique key id, transition_kind key, duration > 0, alignment, center_offset only with custom and within the duration; one transition per cut, held as the outgoing clip tail; optional parameter values keyed by param_key (STU-FX-103, at most 256, finite numbers) and the repeat_end_frame choice), an optional sequence work_area [start, end) (STU-VID-035), all counted against Budget.nodes and omitted from the wire when empty so earlier documents stay byte-identical (tests timeline_records.rs); optional layout members: pages inside page_spreads (per-page size, margins and bleed in pt, STU-LAY-107/113; document-local page_id) and text frames on text layers (bounds, inset, columns, first baseline, vertical justification, auto-size in pt; previous/next thread links per STU-LAY-122 that must be reciprocal, acyclic and one story per chain; pages counted against Budget.nodes); standalone text story hsk.studio.text_story@1 validated by validate_text_story (content reference, paragraph and character-range partitions with style and language refs); optional standalone design records hsk.studio.design_records@1 (StudioDesignRecords, one per document_id, never a StudioDocument member) validated by validate_design_records: per-node auto layout (STU-DS-144; GRID since lane SMB-FIG-GRID-1: the eight STU-DS-146 fields in auto_layout.grid StudioGridLayout, present exactly when layout_mode is GRID, counts 1..=1000 and gaps 0..=100000 studio_declared, grid_row_sizes / grid_column_sizes matching the counts with STU-DS-147 track records FIXED|FLEX value 0..=100000 studio_declared and HUG without value, grid_auto_tracks NONE|ROWS, grid_items_positioning MANUAL|ROW_AUTO_FLOW; absent grid MissingField, other failures InvalidField at <node>/auto_layout/grid/<field>), child layout and size clamps (STU-DS-148/149; STU-DS-150 child_layout.grid_child anchors and spans of an AUTO child of a GRID parent, anchor + span within the declared counts, grid_child_index derived and never stored; both grid members skipped when absent so earlier record sets stay byte-identical, tests auto_layout_grid_records.rs), constraints (STU-DS-152), ordered reactions (STU-PRO-103), component_property_references (STU-DS-113), explicit_variable_modes (STU-DS-132) and style_refs (STU-DS-136), components, component sets, instances, variable collections and variables (STU-DS-103..132, cross-record variant completeness, mode coverage and alias cycles) and PAINT style entries (solid or gradient fills, strokes, ordered appearance rows; validate_paint_style needs no document), counted against Budget.nodes; optional per-layer appearance (opacity and fill_opacity percent 0..=100, blend_mode StudioBlendMode discriminant 1..=35, clipped, isolation, StudioMask coverage tiles with enabled/linked; absent = identity default, not encoded); optional standalone raster records hsk.studio.raster_records@1 (StudioRasterRecords, one per document_id, not a StudioDocument member) validated by validate_raster_records: per-layer StudioAdjustment first set (levels, curves, hue_saturation, brightness_contrast, exposure, invert, threshold, posterize; STU-RAS-151) on an adjustment layer whose PrimitiveReference payload names hsk.studio.adjustment@1 and its own layer id (that layer is a valid source operation of the composition graph with an image output and takes its stack position from input_ordinal, STU-CMP-092; a layer clipped to an adjustment layer is refused as AdjustmentClipBase), StudioMaskParameters (density percent 0..=100, feather px 0..=10000, inverted; STU-RAS-115/124; needs appearance.mask), StudioAdvancedBlending (Blend-If eight levels_0_255 values per channel STU-RAS-159, STU-VEC-153 knockout unknown/disabled/enabled/inherited, channel blend, interior/clipped-effects and transparency-shapes toggles), StudioEffectStack (SEFX id, ordered multi-instance effects, common block, ten kind blocks, StudioContour; STU-RAS-156..158) and document Global Light; effect instances counted against Budget.nodes; rendering is not Folio's; persistent channels StudioRasterRecords.channels (lane SMB-FOLIO-CHANNEL-RECORD, STU-RAS-022/031/144, channel.rs, tests channel_records.rs): StudioChannelRecord per alpha or spot channel in panel order (set-local channel_id key, name 1..=256 bytes without control characters, kind masked_area_alpha|selected_area_alpha|spot_colour, the component kind derived and never stored, visible, display colour rgb|hsb|cmyk|lab|gray, opacity or spot solidity percent 0..=100, depth 8|16|32 with tile format coverage_u8|coverage_u16le|coverage_f32le, width and height of the document raster 1..=300000 shared by every channel, tiles = every cell of one uniform grid over the extent exactly once, tile layer_id = channel_id), at most 56 channels, tiles counted against Budget.nodes, validate_channels checks a list without a document, empty list not encoded so earlier record sets stay byte-identical; optional standalone vector records hsk.studio.vector_records@1 (StudioVectorRecords, one per document_id, not a StudioDocument member, StudioPaint gains no variant) validated by validate_vector_records with the document and optional design records: StudioPattern (hsk.studio.pattern@1: tile_type, spacing and the tile-artwork or live-source model, STU-VEC-017/138, STU-COL-195), StudioPatternPaint (the paint.pattern kind bound to a row of a PAINT style, with the STU-COL-196 placement parameters or an affine matrix), StudioVectorClip (geometry layer and NONZERO|EVENODD), StudioVectorMask (ALPHA|VECTOR|LUMINANCE, mask_clip, invert_mask, region; STU-VEC-053/153), StudioVectorMaskParameters (mask_parameters sidecar keyed by mask_id: density percent 0..=100 default 100, feather px 0..=10000 default 0, linked default true, reveal_outside default false, each skipped at its default so earlier record sets stay byte-identical; STU-RAS-016) and StudioMaskAttachment (the clip/mask reference of a layer), StudioWidthProfile (width_profiles, lane SMB-VECTOR-STYLES, STU-VEC-122: one per SVPT path, at least 2 and at most 256 points, position 0..1 strictly increasing, width and optional width_out finite 0..=1e6 in the document unit, the editing booleans adjust_adjoining_width_points / single_width_only_incoming / single_width_only_outgoing skipped at false; empty set not encoded, so earlier record sets stay byte-identical; tests width_profile.rs), lane SMB-ILLUSTRATOR-TOOLS-2 (vector_tools.rs, tests vector_tools.rs): StudioSwatch hsk.studio.swatch@1 (SSWT; kinds process|global|spot|tint|gradient; color_model RGB|CMYK|LAB|GRAY with ranged components and optional SCPF profile_ref; spot ink with measured Lab and alternate space/value; tint of a colour swatch with tint_value percent; gradient swatch), StudioSwatchPaint (a PAINT row bound to a swatch, optional per-use tint, one binding per row, not on pattern/image rows), StudioVectorBrush (set-local id; calligraphic|scatter|art|pattern|bristle with STU-VEC-152 ranges, StudioBrushParam value + max under non-fixed STU-VEC-150 dynamics, STU-VEC-163 colorization, SVPT art with colours, five named pattern tile slots, side required), StudioBrushStroke (stroke row -> brush, optional scale_override percent), StudioEnvelope (vector layer; warp of 15 STU-VEC-168 styles with bend/distortions as ratios -1..1, mesh 1..=64 rows/columns with (rows+1)(columns+1) points, or an unwrapped top-object vector layer; fidelity 0..100 and the STU-VEC-169 booleans); each member skipped when empty (byte-identical), at most 4096 entries; StudioIccProfile (icc_profiles, lane SMB-ILLUSTRATOR-INTEROP: SCPF id, name, colour_space RGB|CMYK|GRAY, sha256 content_digest of bytes kept outside Folio, working_space flag at most once per model; a listed swatch profile_ref must match the swatch model); StudioStrokeMarkers (stroke_markers, lane SMB-AI-ARROWHEADS-1, vector_markers.rs, STU-VEC-146: one entry per Stroke row of a PAINT style (style_id SSTY + row; a fill row is InvalidField, a dangling style or row DanglingReference, a second entry DuplicateIdentity), start and end tokens stored verbatim from the 26-token union catalogue StudioArrowhead (layout suite arrow.none|arrow.simple|arrow.simple_wide|arrow.triangle|arrow.triangle_wide|arrow.barbed|arrow.curved|arrow.circle|arrow.circle_solid|arrow.square|arrow.square_solid|arrow.bar, collaborative suite NONE|ROUND|SQUARE|ARROW_LINES|ARROW_EQUILATERAL|TRIANGLE_FILLED|DIAMOND_FILLED|CIRCLE_FILLED, connector ERD_ONE|ERD_MANY|ERD_ONE_OR_MORE|ERD_ZERO_OR_MORE|ERD_ZERO_OR_ONE|ERD_EXACTLY_ONE; arrow.none and NONE = no marker; an unknown token is InvalidField), start_scale and end_scale percent of the stroke weight hard 1..=1000 (InvalidField outside; soft bounds and precision unknown; DEFAULT_MARKER_SCALE 100 is studio_declared, used by StudioStrokeMarkers::new only), alignment arrow_align.inside_path|arrow_align.outside_path; swap_markers exchanges the two tokens and scales and keeps the alignment; at most 65536 entries; the member is skipped when empty so earlier record sets stay byte-identical; tests vector_markers.rs; the marker shapes, proportions, default scale and align semantics are Studio readings, UNVERIFIED (D5): the proportion table is hsk_studio_nib::markers; the live-effect head_arrow/tail_arrow indices are not mapped); lengths in the document unit, set-local ids, reference cycles refused as RenderCycle; records counted against Budget.nodes; optional standalone multicam records hsk.studio.multicam_records@1 (StudioMulticamRecords, one per document_id, not a StudioDocument member) validated by validate_multicam_records / parse_multicam_records: multicam_sequences (document sequence id, selection_order top_down|source_order, angle_order = the sequence's video tracks in that numbering, validated) and clips (SCLP clip whose source_ref is ClipSource::Sequence of a declared source other than its own sequence; active_angle and audio_angle 1..=angle count, audio_angle == active_angle while audio_follows_video; follows_nest_setting; enabled); counted against Budget.nodes, hard ceilings 64 sequences / 1024 angles / 262144 clips; tests multicam_records.rs; optional standalone proof records hsk.studio.proof_records@1 (StudioProofRecords, lane SMB-PS-SOFT-PROOF-2, one per document_id, not a StudioDocument member, STU-COL-240..244) validated by validate_proof_records / parse_proof_records: saved conditions (unique name of at most 128 bytes without control characters that is not a shipped preset name, SHIPPED_PROOF_PRESETS working_cmyk|working_rgb|legacy_uncompensated_rgb|protanopia|deuteranopia|tritanopia; the six STU-COL-241 fields proof_profile shipped preset|icc SCPF profile_id + sha256 content_digest of bytes kept outside Folio|colour_vision protanopia|deuteranopia|tritanopia|operator_matrix 9 finite coefficients |c| <= 4 with row sums 0..=2, proof_intent, display_intent, black_point_compensation, simulate_paper_color, simulate_black_ink), active_condition (a saved or shipped name), proof_colours toggle and gamut_warning (enabled, colour display RGB 0..=1 default 0.5 grey, opacity percent 0..=100 default 100 per the STU-COL-244 parameter table); a toggle on without an active condition is refused; at most 256 conditions counted against Budget.nodes; tests proof_records.rs; optional standalone design state records hsk.studio.design_state@1 (StudioDesignStateRecords, lane SMB-FIG-EDIT-2, one per document_id, not a StudioDocument or StudioDesignRecords member, STU-IO-006 row 35 edit-preserve, STU-DS-003/008/016/017) validated by validate_design_state_records / parse_design_state_records against the document and optional design records: instances = StudioInstanceState per (instance group layer, content_signature = its child SLYR ids in document order, shape-checked only since an undone content's children may be gone), unique per (layer, content), one SCIN instance per layer and one layer per instance; component_id (the record's main_component_id as stored, SCMP component or set; a variant switch keeps it and adds a VARIANT property), variant (the drawn member's assignment = the main member's, or the set default variant's, with every VARIANT property applied; each property and value declared by the set and a set member carrying exactly it), properties (component property values typed as StudioInstanceProperty: VARIANT ones declared by the set, others defined by the drawn member or its set with the same type), overrides (node_id a layer under the drawn member's definition, field in the STU-DS-115 closed set OVERRIDABLE_FIELDS, characters string, visible boolean, opacity float 0..=1 or a variable alias of that type, one row per node and field), swap_target (the component or set a swap chose; the main component or the drawn member is it or shares its set); without design records only the document is checked; hard ceilings 1024 states, 4096 signature ids, 4096 overrides and 256 properties per state, counted against Budget.nodes; tests design_state_records.rs; optional standalone layout-rule records hsk.studio.layout_rules@1 (StudioLayoutRuleRecords, lane SMB-ID-LIQUID-LAYOUT-1, one per document_id, not a StudioDocument member, STU-LAY-011/019/111) validated by validate_layout_rule_records with the document and optional parent-page records: page_rules and parent_rules (the seven liquid members off|scale|recenter|object_based|guide_based|use_parent|preserve_existing; preserve_existing is an apply-range directive (apply_page_rules) and never stored; resolve_liquid_rule resolves use_parent through the applied parent and based_on, absent = off), object_constraints (SLYR layer, four pins, width/height fixed|relative; both pins of an axis with a fixed size refused), alternate_layouts (sections: section_id, alternate_layout name not blank, unique, not ending with * or :, alternate_layout_length pages from page_start in document page order, sections never overlap), linked_stories (child/parent STXT shown by text frames, StudioLinkedStoryOptions update_policy auto_update|warn_on_change + remove_forced_line_breaks) and linked_page_items (child/parent SLYR, StudioLinkedPageItemOptions update_policy), one link per child, no self or looping links, synced_parent_fingerprint (SHA-256 of the parent content JSON, 64 lowercase hex; a serialization failure is the typed InvalidField diagnostic at <id>/fingerprint, never a fingerprint; a legacy 16-hex FNV-1a 64 value from earlier records still validates but is never compared and reads stale_fingerprint until a sync rewrites it; tests link_fingerprints_are_sha256_and_legacy_fnv_values_read_stale, fingerprint_serialization_failure_is_a_typed_error) from which the link state up_to_date|out_of_date|stale_fingerprint is derived; at most 65536 per list, counted against Budget.nodes; empty lists not encoded; tests layout_rule_records.rs; optional standalone CJK composition records hsk.studio.cjk_composition@1 (StudioCjkCompositionRecords, lane SMB-ID-CJK-KINSOKU-1, one per document_id, not a StudioDocument member, STU-TYP-136/137/138) validated by validate_cjk_composition_records with the document and optional type-style records: kinsoku_tables (unique name not a shipped name hard_kinsoku|soft_kinsoku, four character sets cant_begin_line_chars / cant_end_line_chars / cant_be_separated_chars / hanging_punctuation_chars, each character once, no control characters, at most 4096 characters), mojikumi_tables (unique name not a base-set name, based_on one of the seventeen StudioMojikumiBaseSet members, aki_override_list of class pairs over the JLREQ classes plus line_start / line_end with minimum <= desired <= maximum em in 0..=2, one per pair, at most 1024; use_cid_mojikumi), composers (unique composer_name, composition_scope, composition_engine, kinsoku_table and mojikumi_table names that resolve to a table of the set, a shipped kinsoku table or a base-set name; an unresolvable name is DanglingReference, never a fallback) and bindings (one per STYS paragraph style: composer_name of the set plus kinsoku_type, kinsoku_hang_type, bunri_kinshi, rensuuji, burasagari_type, kinsoku_order); at most 256 tables or composers per kind and 4096 bindings; empty lists not encoded; tests cjk_composition_records.rs","output":"Immutable source-local document or typed rejection; unknown schema/fields preserve complete original bytes read-only; ordered composition edge projection","ports":"Resolver ports with Absent defaults (no resolver is falsely a provider): media, story_content, and (lane SMB-PACKAGE-COMPLETENESS-2) font(family, style), audio(audio_id), lut(lut, Option<content_digest>) for adjustment LUT references and clip-finishing StudioLutRef, placed_link(uri, content_digest); optional CKC catalog asset_id (is_catalog_asset_id: canonical lowercase UUID, the kernel-bindings AssetId grammar, test catalog_asset_id_contract) on ClipSource::Media and StudioPlacedSource, serde-default and not encoded when absent (tests catalog_asset_id.rs)","recovery":"Correct invalid fields; refresh caller revision; obtain typed domain/profile/ArtifactManifest resolver from existing owner; unavailable meaning blocks dependent operations; canceled or rejected edits preserve original bytes","manual":"Same pure wire contract and generated draft2020-12 schema for human/model/API consumers; no hidden defaults or private catalog","argus":{"inspect":"snapshot bytes, revision, resolution dispositions, stable diagnostic address","steer":"same bounded rename API","state":"immutable graph and ordered input projection","capture":"caller-owned granted capture of actual output"},"diagnostics":"Pure typed codes and target addresses adapted by caller through Observe/FlightRecorder/internal diagnostics/Palmistry; private document names/content are not diagnostic text","authority":"Caller revision is not host authority; grants, CKC/ArtifactService, Chronicle, Package, native GUI, color/render, DB, CRDT and EventLedger acceptance remain pending"}"#;
