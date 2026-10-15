//! Pure source-local property patch preparation; no history stack or authority database.
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_folio::{
    BLEND_PASS_THROUGH, Isolation, LayerAppearance, NodeKind, NodeRef, Operation, Parent, Payload,
    Resolver, Snapshot, StudioDocument, StudioLayer, TileRef,
};
mod structure;
use structure::{capture, placement_of, remove_layer, resolve, restore_command, set_placement};
/// The current [`Property::Placement`] of a layer: its parent and stack index (0 = bottom-most),
/// `None` when the layer is absent or not stacked (no single graph operation filling exactly one
/// slot under the root or a group layer). The rule `prepare` reads; consumers need no copy of it.
pub fn placement(document: &StudioDocument, layer_id: &str) -> Option<(Parent, u64)> {
    placement_of(document, layer_id)
}
use hsk_studio_observe::{Observation, Observe, Outcome, SinkPort};
use schemars::{JsonSchema, generate::SchemaSettings};
use serde::{
    Deserialize, Serialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::Value;
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    fmt,
};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub account_id: String,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub principal_id: String,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub owner_account_id: String,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub owner_principal_id: String,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub access_space_id: String,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub session_id: String,
}
impl Actor {
    pub fn validated(&self) -> std::result::Result<ActorContext, Diagnostic> {
        ActorContext::new(
            &self.account_id,
            &self.principal_id,
            &self.owner_account_id,
            &self.owner_principal_id,
            &self.access_space_id,
            &self.session_id,
        )
        .map_err(|_| failure(Code::InvalidActor, None))
    }
}
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Property {
    Name,
    Visible,
    /// Layer opacity in percent (`LayerAppearance.opacity`, `0..=100`), value `Number`.
    Opacity,
    /// Layer blend mode (`LayerAppearance.blend_mode`, the `StudioBlendMode` discriminant), value
    /// an integer `Number`. Writing it on a group also sets the group's isolation (pass-through
    /// for discriminant 1, isolated otherwise), the one combination folio accepts.
    BlendMode,
    /// The tile references of a raster layer (`Payload::Raster.tiles`), value `Tiles`: a pixel
    /// edit is a new tile list whose new content digests the caller makes available (the session
    /// stages their bytes). Only raster layers have this property (`seed_revisions` seeds it for
    /// them alone; any other payload is `InvalidTarget`).
    Tiles,
    /// Where a layer sits in the layer stack (lane SMB-HARNESS-UI-3): its parent (the document
    /// root or a group layer) and its stack index there (0 = bottom-most: the composite input
    /// ordinal, or for the root the position in `graph.outputs`), or absent when the layer does not
    /// exist. Reads quote the state value `Placement`; writes are the command value `Place`, an
    /// enumerated placement ([STU-RAS-114]: `place_inside`, `place_at_beginning`, `place_at_end`,
    /// `place_before`, `place_after`; index-only placement writes are refused `InvalidInput`), or
    /// `Place(None)` to delete. A command on an absent layer creates it (it carries [`LayerCreate`]);
    /// on a present layer it moves it (reorder, group, ungroup). Writes of one patch apply in order,
    /// each command resolved on the stack as the previous writes left it; the inverse lists the
    /// restoring commands in reverse.
    /// Seeded only for a layer with exactly one graph operation whose single output fills exactly
    /// one stack slot under the root or a group layer. A layer that neither the vector nor the
    /// document holds reads as absent at revision 0 (a caller that deletes layers keeps their
    /// vector entries, as the session does). Tests `tests/structure.rs`.
    Placement,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyAddress {
    pub target: NodeRef,
    pub property: Property,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "value_kind",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum PropertyValue {
    String(String),
    Bool(bool),
    /// A finite JSON number (`opacity` percent, `blend_mode` discriminant). Compared by numeric
    /// value, so `50` and `50.0` are the same value.
    Number(serde_json::Number),
    /// A raster layer's complete tile list.
    Tiles(TileList),
    /// A layer's stack placement (state, for reads); `None` = the layer does not exist
    /// ([`Property::Placement`]).
    Placement(Option<Placement>),
    /// A placement command (writes of [`Property::Placement`]); `None` deletes the layer.
    Place(Option<PlaceCommand>),
}
/// Stack placement of a layer (state): parent and index (0 = bottom-most).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Placement {
    #[serde(deserialize_with = "required")]
    pub parent: Parent,
    pub index: u64,
}
/// The enumerated placement of [STU-RAS-114] (wire discriminants 0..=4). Studio reading of the
/// directions (the spec names the members only; this follows the Photoshop document model, where
/// a layer list runs top-first; UNVERIFIED, D5): `place_at_beginning` = top of the container,
/// `place_at_end` = bottom, `place_before` = directly above the anchor, `place_after` = directly
/// below it, `place_inside` = top of the anchor group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlaceMode {
    PlaceInside,
    PlaceAtBeginning,
    PlaceAtEnd,
    PlaceBefore,
    PlaceAfter,
}
/// A placement command. `anchor`: the group (`place_inside`; the container of
/// `place_at_beginning`/`place_at_end`, absent = the document root) or the sibling layer
/// (`place_before`/`place_after`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceCommand {
    pub mode: PlaceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<String>,
    /// Only on a command that creates the layer (absent -> present): its record and its graph
    /// operation; refused on a command that moves an existing layer (`InvalidInput`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<Box<LayerCreate>>,
}
/// Folio admits only finite numbers and JSON has no NaN, so equality is reflexive (`Eq`).
impl Eq for PlaceCommand {}
/// The record and graph operation of a layer that a placement write creates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayerCreate {
    pub layer: StudioLayer,
    pub operation: Operation,
}
fn required<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<T, D::Error> {
    T::deserialize(d)
}
/// A raster layer's tile references in document order. Equality is folio's field equality; folio
/// only admits finite tile lengths and JSON has no NaN, so the relation is reflexive (`Eq`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct TileList(pub Vec<TileRef>);
impl Eq for TileList {}
impl PropertyValue {
    /// A finite number; `None` for NaN or an infinity.
    pub fn number(v: f64) -> Option<Self> {
        serde_json::Number::from_f64(v).map(Self::Number)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyRead {
    pub address: PropertyAddress,
    pub expected_revision: u64,
    pub expected_value: PropertyValue,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropertyWrite {
    pub address: PropertyAddress,
    pub value: PropertyValue,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    #[schemars(extend("const"=1))]
    pub transport_version: u8,
    pub document_id: String,
    pub base_revision: u64,
    #[schemars(
        length(min = 1, max = 128),
        regex(pattern = r"^[^\x00-\x1f\x7f-\x9f]+$")
    )]
    pub command_id: String,
    pub correlation_id: u64,
    pub actor: Actor,
    pub reads: Vec<PropertyRead>,
    #[schemars(length(min = 1))]
    pub writes: Vec<PropertyWrite>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevisionEntry {
    pub address: PropertyAddress,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevisionUpdate {
    pub address: PropertyAddress,
    pub previous_revision: u64,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Inverse {
    #[schemars(extend("const"=1))]
    pub transport_version: u8,
    pub document_id: String,
    pub source_command_id: String,
    pub reads: Vec<PropertyRead>,
    pub writes: Vec<PropertyWrite>,
}
impl Inverse {
    /// The caller supplies fresh command, correlation and actor identity; the source command's
    /// identity is not reused (test: `actual_apply_inverse_and_typed_wire_roundtrip`).
    pub fn invocation(
        &self,
        command_id: String,
        correlation_id: u64,
        actor: Actor,
        base_revision: u64,
    ) -> Patch {
        Patch {
            transport_version: self.transport_version,
            document_id: self.document_id.clone(),
            base_revision,
            command_id,
            correlation_id,
            actor,
            reads: self.reads.clone(),
            writes: self.writes.clone(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "disposition", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceResult {
    Changed {
        document_id: String,
        command_id: String,
        correlation_id: u64,
        actor: Actor,
        observed_current_revision: u64,
        successor: Box<StudioDocument>,
        revision_updates: Vec<RevisionUpdate>,
        inverse: Inverse,
    },
    NoChange {
        document_id: String,
        command_id: String,
        correlation_id: u64,
        actor: Actor,
        observed_current_revision: u64,
    },
}
#[derive(Clone, Debug)]
pub struct Prepared {
    wire: SourceResult,
    snapshot: Option<Snapshot>,
}
impl Prepared {
    pub fn wire(&self) -> &SourceResult {
        &self.wire
    }
    pub fn successor(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }
    pub fn revision_updates(&self) -> &[RevisionUpdate] {
        match &self.wire {
            SourceResult::Changed {
                revision_updates, ..
            } => revision_updates,
            SourceResult::NoChange { .. } => &[],
        }
    }
    pub fn inverse(&self) -> Option<&Inverse> {
        match &self.wire {
            SourceResult::Changed { inverse, .. } => Some(inverse),
            _ => None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    InvalidInput,
    UnsupportedVersion,
    DuplicateField,
    InvalidActor,
    InvalidCommand,
    WrongDocument,
    InvalidTarget,
    WrongType,
    DuplicateAddress,
    MissingRead,
    RevisionUnavailable,
    RevisionConflict,
    ValueConflict,
    RevisionOverflow,
    BudgetExceeded,
    Canceled,
    FolioRejected,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    pub address: Option<PropertyAddress>,
}
fn failure(code: Code, address: Option<&PropertyAddress>) -> Diagnostic {
    Diagnostic {
        code,
        address: address.cloned(),
    }
}
type Result<T> = std::result::Result<T, Diagnostic>;
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub input_bytes: usize,
    pub reads: usize,
    pub writes: usize,
    pub revisions: usize,
    pub value_bytes: usize,
    pub folio: hsk_studio_folio::Budget,
}
impl Default for Budget {
    fn default() -> Self {
        Self {
            input_bytes: 262_144,
            reads: 1024,
            writes: 1024,
            revisions: 8192,
            value_bytes: 65_536,
            folio: hsk_studio_folio::Budget::default(),
        }
    }
}
fn check(t: &CancellationToken) -> Result<()> {
    t.check().map_err(|_| failure(Code::Canceled, None))
}
fn addr_key(a: &PropertyAddress) -> (String, Property) {
    (a.target.node_id.clone(), a.property)
}
fn address_valid(a: &PropertyAddress) -> Result<()> {
    let prefix = match a.target.node_kind {
        NodeKind::Layer => "SLYR",
        NodeKind::Artboard => "SART",
        NodeKind::PageSpread => "SPGS",
    };
    if !DomainId::parse(&a.target.node_id).is_ok_and(|id| id.prefix() == prefix) {
        return Err(failure(Code::InvalidTarget, None));
    }
    if !addressable(&a.target.node_kind, a.property) {
        return Err(failure(Code::WrongType, Some(a)));
    }
    Ok(())
}
/// Every property chronicle can address on some node kind, in the order `seed_revisions` lists them.
/// Adding a `Property` variant fails to compile in `addressable` and `mask_property` until the
/// table is extended; this list is the one hand-maintained mirror of the enum (compile-time
/// guard in the test `exported_table_is_the_table_prepare_enforces`).
pub const PROPERTIES: [Property; 6] = [
    Property::Name,
    Property::Visible,
    Property::Opacity,
    Property::BlendMode,
    Property::Tiles,
    Property::Placement,
];
/// The addressability table: whether chronicle can address `property` on a `kind` node.
///
/// This is the single owner of that fact. `prepare` refuses an address for which this is false
/// (`Code::WrongType`), and `seed_revisions` and `mask_property` read it, so a consumer needs no
/// copy of the table. Exhaustive over node kind x property with no wildcard: a new variant on
/// either side is a compile error here. It is type-level; whether a given node can be addressed
/// is a document question, answered by `seed_revisions` (an unsupported layer has no entry).
/// Test: `exported_table_is_the_table_prepare_enforces`.
pub fn addressable(kind: &NodeKind, property: Property) -> bool {
    match (kind, property) {
        (
            NodeKind::Layer,
            Property::Name
            | Property::Visible
            | Property::Opacity
            | Property::BlendMode
            | Property::Tiles
            | Property::Placement,
        )
        | (NodeKind::Artboard | NodeKind::PageSpread, Property::Name) => true,
        (
            NodeKind::Artboard | NodeKind::PageSpread,
            Property::Visible
            | Property::Opacity
            | Property::BlendMode
            | Property::Tiles
            | Property::Placement,
        ) => false,
    }
}
/// A layer whose payload folio preserves but does not understand is read-only to chronicle.
fn layer_addressable(layer: &StudioLayer) -> bool {
    !matches!(layer.payload, Payload::Unsupported { .. })
}
/// The revision vector of a freshly opened document: one entry at revision 0 for every
/// addressable (node, property) pair, layers first, then artboards, then page spreads (document
/// order), properties in `PROPERTIES` order. Nodes chronicle cannot address (a layer with an
/// unsupported payload) get no entry, and neither does a pair `addressable` refuses (visibility
/// of an artboard or page spread). `prepare` accepts the whole vector, and accepts a write to
/// each seeded address. Test: `exported_table_is_the_table_prepare_enforces`.
pub fn seed_revisions(document: &StudioDocument) -> Vec<RevisionEntry> {
    let mut vector = Vec::new();
    let mut seed = |node_kind: NodeKind, node_id: &str, raster: bool, placed: bool| {
        for property in PROPERTIES {
            if addressable(&node_kind, property)
                && (raster || property != Property::Tiles)
                && (placed || property != Property::Placement)
            {
                vector.push(RevisionEntry {
                    address: PropertyAddress {
                        target: NodeRef {
                            node_kind: node_kind.clone(),
                            node_id: node_id.to_owned(),
                        },
                        property,
                    },
                    revision: 0,
                });
            }
        }
    };
    for layer in document.layers.iter().filter(|l| layer_addressable(l)) {
        let raster = matches!(layer.payload, Payload::Raster { .. });
        let placed = placement_of(document, &layer.layer_id).is_some();
        seed(NodeKind::Layer, &layer.layer_id, raster, placed);
    }
    for artboard in &document.artboards {
        seed(NodeKind::Artboard, &artboard.artboard_id, false, false);
    }
    for spread in &document.page_spreads {
        seed(NodeKind::PageSpread, &spread.page_spread_id, false, false);
    }
    vector
}
/// Sets the addressed property to a neutral value (empty name, not visible, the kind's identity
/// opacity and blend mode, after which an identity appearance is stored as absent) so two documents can
/// be compared with that property left out, for example to check that a successor differs from
/// its base only at declared addresses. False, with the document untouched, when `addressable`
/// refuses the pair or the document has no such node. Test:
/// `exported_table_is_the_table_prepare_enforces`.
pub fn mask_property(document: &mut StudioDocument, address: &PropertyAddress) -> bool {
    if !addressable(&address.target.node_kind, address.property) {
        return false;
    }
    if address.property == Property::Placement {
        // The layer leaves the document (record, operations, every edge from or into its
        // operations, its root outputs; the stacks it sat in are renumbered densely). Masking an
        // absent layer changes nothing and succeeds: the base of a create holds no such layer.
        remove_layer(document, &address.target.node_id);
        return true;
    }
    let neutral = match address.property {
        Property::Name => PropertyValue::String(String::new()),
        Property::Visible => PropertyValue::Bool(false),
        Property::Opacity => PropertyValue::Number(100.into()),
        Property::BlendMode => {
            let group = document
                .layers
                .iter()
                .find(|l| l.layer_id == address.target.node_id)
                .is_some_and(is_group);
            PropertyValue::Number(LayerAppearance::identity(group).blend_mode.into())
        }
        Property::Tiles => PropertyValue::Tiles(TileList(Vec::new())),
        Property::Placement => return true,
    };
    set(document, address, &neutral).is_ok()
}
enum ValueRef<'a> {
    String(&'a str),
    Bool(bool),
    Number(f64),
    Tiles(&'a [TileRef]),
    Placement(Option<(Parent, u64)>),
}
impl ValueRef<'_> {
    fn equals(&self, v: &PropertyValue) -> bool {
        match (self, v) {
            (Self::String(a), PropertyValue::String(b)) => *a == b.as_str(),
            (Self::Bool(a), PropertyValue::Bool(b)) => a == b,
            (Self::Number(a), PropertyValue::Number(b)) => b.as_f64() == Some(*a),
            (Self::Tiles(a), PropertyValue::Tiles(b)) => *a == b.0.as_slice(),
            (Self::Placement(None), PropertyValue::Placement(None)) => true,
            (Self::Placement(Some((parent, index))), PropertyValue::Placement(Some(p))) => {
                *parent == p.parent && *index == p.index
            }
            _ => false,
        }
    }
    fn owned(&self) -> PropertyValue {
        match self {
            Self::String(s) => PropertyValue::String((*s).into()),
            Self::Bool(v) => PropertyValue::Bool(*v),
            // Folio validated the document: stored appearance numbers are finite. A whole value
            // (a blend-mode discriminant, a whole percent) is written as an integer so the
            // inverse of a blend-mode write passes the integer type check.
            Self::Number(v) if v.fract() == 0.0 && (0.0..=65_535.0).contains(v) => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let whole = *v as u64;
                PropertyValue::Number(whole.into())
            }
            Self::Number(v) => PropertyValue::number(*v).unwrap_or(PropertyValue::Number(0.into())),
            Self::Tiles(t) => PropertyValue::Tiles(TileList(t.to_vec())),
            Self::Placement(p) => {
                PropertyValue::Placement(p.as_ref().map(|(parent, index)| Placement {
                    parent: parent.clone(),
                    index: *index,
                }))
            }
        }
    }
}
/// The kind test `StudioLayer::effective_appearance` uses for the identity default.
fn is_group(l: &StudioLayer) -> bool {
    l.kind == "group" && matches!(l.payload, Payload::Group)
}
fn value<'a>(d: &'a StudioDocument, a: &PropertyAddress) -> Result<ValueRef<'a>> {
    address_valid(a)?;
    match a.target.node_kind {
        NodeKind::Layer => {
            let found = d.layers.iter().find(|l| l.layer_id == a.target.node_id);
            if a.property == Property::Placement {
                return match found {
                    None => Ok(ValueRef::Placement(None)),
                    Some(l) if layer_addressable(l) => placement_of(d, &l.layer_id)
                        .map(|p| ValueRef::Placement(Some(p)))
                        .ok_or_else(|| failure(Code::InvalidTarget, Some(a))),
                    Some(_) => Err(failure(Code::InvalidTarget, Some(a))),
                };
            }
            let l = found.ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            if !layer_addressable(l) {
                return Err(failure(Code::InvalidTarget, Some(a)));
            }
            match a.property {
                Property::Name => Ok(ValueRef::String(&l.name)),
                Property::Visible => Ok(ValueRef::Bool(l.visible)),
                Property::Opacity => Ok(ValueRef::Number(l.effective_appearance().opacity)),
                Property::BlendMode => Ok(ValueRef::Number(f64::from(
                    l.effective_appearance().blend_mode,
                ))),
                Property::Tiles => match &l.payload {
                    Payload::Raster { tiles } => Ok(ValueRef::Tiles(tiles)),
                    _ => Err(failure(Code::InvalidTarget, Some(a))),
                },
                Property::Placement => Err(failure(Code::InvalidTarget, Some(a))),
            }
        }
        NodeKind::Artboard => {
            let l = d
                .artboards
                .iter()
                .find(|l| l.artboard_id == a.target.node_id)
                .ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            Ok(ValueRef::String(&l.name))
        }
        NodeKind::PageSpread => {
            let l = d
                .page_spreads
                .iter()
                .find(|l| l.page_spread_id == a.target.node_id)
                .ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            Ok(ValueRef::String(&l.name))
        }
    }
}
fn value_type(a: &PropertyAddress, v: &PropertyValue) -> Result<()> {
    let ok = match (a.property, v) {
        (Property::Name, PropertyValue::String(_)) | (Property::Visible, PropertyValue::Bool(_)) => {
            true
        }
        (Property::Opacity, PropertyValue::Number(n)) => n.as_f64().is_some_and(f64::is_finite),
        // A discriminant is an integer; range and applicability are folio's (FolioRejected).
        (Property::BlendMode, PropertyValue::Number(n)) => {
            n.as_u64().is_some_and(|d| u16::try_from(d).is_ok())
        }
        (Property::Tiles, PropertyValue::Tiles(_)) => true,
        (Property::Placement, PropertyValue::Placement(_) | PropertyValue::Place(_)) => true,
        _ => false,
    };
    if !ok {
        return Err(failure(Code::WrongType, Some(a)));
    }
    Ok(())
}
fn size(v: &PropertyValue) -> usize {
    match v {
        PropertyValue::String(s) => s.len(),
        PropertyValue::Bool(_) | PropertyValue::Number(_) => 0,
        // Text of every reference plus a fixed 128 for its numbers (as `snapshot_budget` counts).
        PropertyValue::Tiles(list) => list.0.iter().fold(0usize, |sum, t| {
            [
                &t.object_key,
                &t.layer_id,
                &t.format,
                &t.colour_profile_id,
                &t.content_digest.algorithm,
                &t.content_digest.digest,
                &t.artifact_manifest_id,
            ]
            .iter()
            .fold(sum.saturating_add(128), |n, text| n.saturating_add(text.len()))
        }),
        // A placement is two numbers and a parent id (a fixed 128); a create also carries its
        // record and operation, counted by their encoded length.
        PropertyValue::Placement(p) => p.as_ref().map_or(0, |_| 128),
        PropertyValue::Place(p) => p.as_ref().map_or(0, |p| {
            p.create.as_ref().map_or(128, |c| {
                serde_json::to_vec(c).map_or(usize::MAX, |v| v.len().saturating_add(128))
            })
        }),
    }
}
/// Writes an appearance property of a layer. An appearance equal to the kind's identity is stored
/// as absent (folio's encoding of the default), so setting a value and setting it back restores the
/// original document, not only the original pixels.
fn set_appearance(l: &mut StudioLayer, property: Property, n: &serde_json::Number) {
    let group = is_group(l);
    let mut appearance = l.effective_appearance();
    match property {
        Property::Opacity => appearance.opacity = n.as_f64().unwrap_or(f64::NAN),
        Property::BlendMode => {
            appearance.blend_mode = n
                .as_u64()
                .and_then(|d| u16::try_from(d).ok())
                .unwrap_or(0);
            if group {
                appearance.isolation = if appearance.blend_mode == BLEND_PASS_THROUGH {
                    Isolation::PassThrough
                } else {
                    Isolation::Isolated
                };
            }
        }
        Property::Name | Property::Visible | Property::Tiles | Property::Placement => {}
    }
    l.appearance = (appearance != LayerAppearance::identity(group)).then_some(appearance);
}
fn set(d: &mut StudioDocument, a: &PropertyAddress, v: &PropertyValue) -> Result<()> {
    value_type(a, v)?;
    match v {
        PropertyValue::Place(p) => return set_placement(d, a, p.as_ref()),
        // [STU-RAS-114]: index-only placement writes are forbidden.
        PropertyValue::Placement(_) => return Err(failure(Code::InvalidInput, Some(a))),
        _ => {}
    }
    match a.target.node_kind {
        NodeKind::Layer => {
            let l = d
                .layers
                .iter_mut()
                .find(|l| l.layer_id == a.target.node_id)
                .ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            match v {
                PropertyValue::String(v) => l.name = v.clone(),
                PropertyValue::Bool(v) => l.visible = *v,
                PropertyValue::Number(n) => set_appearance(l, a.property, n),
                PropertyValue::Tiles(list) => match &mut l.payload {
                    Payload::Raster { tiles } => tiles.clone_from(&list.0),
                    _ => return Err(failure(Code::InvalidTarget, Some(a))),
                },
                PropertyValue::Placement(_) | PropertyValue::Place(_) => {
                    return Err(failure(Code::WrongType, Some(a)));
                }
            }
        }
        NodeKind::Artboard => {
            let l = d
                .artboards
                .iter_mut()
                .find(|l| l.artboard_id == a.target.node_id)
                .ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            if let PropertyValue::String(v) = v {
                l.name = v.clone();
            }
        }
        NodeKind::PageSpread => {
            let l = d
                .page_spreads
                .iter_mut()
                .find(|l| l.page_spread_id == a.target.node_id)
                .ok_or_else(|| failure(Code::InvalidTarget, Some(a)))?;
            if let PropertyValue::String(v) = v {
                l.name = v.clone();
            }
        }
    }
    Ok(())
}
fn snapshot_budget(s: &Snapshot, b: hsk_studio_folio::Budget, t: &CancellationToken) -> Result<()> {
    check(t)?;
    let d = s.document();
    if s.encoded_bytes().len() > b.input_bytes
        || d.artboards
            .len()
            .checked_add(d.page_spreads.len())
            .and_then(|n| n.checked_add(d.layers.len()))
            .and_then(|n| n.checked_add(d.graph.operations.len()))
            .is_none_or(|n| n > b.nodes)
        || d.graph.edges.len() > b.edges
        || b.depth == 0
        || b.depth > 64
    {
        return Err(failure(Code::BudgetExceeded, None));
    }
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for byte in s.encoded_bytes() {
        check(t)?;
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth = depth.saturating_add(1);
                    if depth > b.depth {
                        return Err(failure(Code::BudgetExceeded, None));
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    let mut bytes = 0usize;
    for l in &d.layers {
        check(t)?;
        let mut add = |n: usize| -> Result<()> {
            bytes = bytes
                .checked_add(n)
                .ok_or_else(|| failure(Code::BudgetExceeded, None))?;
            if bytes > b.payload_bytes {
                return Err(failure(Code::BudgetExceeded, None));
            }
            Ok(())
        };
        match &l.payload {
            Payload::Group => {}
            Payload::Raster { tiles } => {
                for tile in tiles {
                    check(t)?;
                    for text in [
                        &tile.object_key,
                        &tile.layer_id,
                        &tile.format,
                        &tile.colour_profile_id,
                        &tile.content_digest.algorithm,
                        &tile.content_digest.digest,
                        &tile.artifact_manifest_id,
                    ] {
                        add(text.len())?;
                    }
                    add(128)?;
                }
            }
            Payload::Vector { path_ids } => {
                for id in path_ids {
                    check(t)?;
                    add(id.len())?;
                }
            }
            Payload::Text { story_id } => add(story_id.len())?,
            Payload::PrimitiveReference {
                schema_id,
                object_id,
            } => {
                add(schema_id.len())?;
                add(object_id.len())?;
            }
            Payload::Unsupported {
                kind,
                schema_id,
                encoded_bytes,
                reason,
            } => {
                add(kind.len())?;
                add(schema_id.len())?;
                add(encoded_bytes.len())?;
                add(reason.len())?;
            }
        }
    }
    Ok(())
}
struct ByteCounter<'a> {
    bytes: usize,
    token: &'a CancellationToken,
    canceled: bool,
}
impl std::io::Write for ByteCounter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.token.check().is_err() {
            self.canceled = true;
            return Err(std::io::Error::other("canceled"));
        }
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encoded_size<T: Serialize + ?Sized>(v: &T, t: &CancellationToken) -> Result<usize> {
    let mut counter = ByteCounter {
        bytes: 0,
        token: t,
        canceled: false,
    };
    serde_json::to_writer(&mut counter, v).map_err(|_| {
        failure(
            if counter.canceled {
                Code::Canceled
            } else {
                Code::BudgetExceeded
            },
            None,
        )
    })?;
    Ok(counter.bytes)
}
fn scalar_size(v: &PropertyValue, t: &CancellationToken) -> Result<usize> {
    match v {
        PropertyValue::String(s) => encoded_size(s, t),
        PropertyValue::Bool(v) => encoded_size(v, t),
        PropertyValue::Number(n) => encoded_size(n, t),
        PropertyValue::Tiles(list) => encoded_size(list, t),
        PropertyValue::Placement(p) => encoded_size(p, t),
        PropertyValue::Place(p) => encoded_size(p, t),
    }
}
/// Stateless preparation. The caller serializes a fresh current read plus publication.
/// base_revision is provenance only; only declared current property reads gate this patch.
pub fn prepare(
    p: &Patch,
    current: &Snapshot,
    revisions: &[RevisionEntry],
    b: Budget,
    t: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<Prepared> {
    check(t)?;
    if p.transport_version != 1 {
        return Err(failure(Code::UnsupportedVersion, None));
    }
    if p.command_id.is_empty()
        || p.command_id.len() > 128
        || p.command_id.chars().any(char::is_control)
    {
        return Err(failure(Code::InvalidCommand, None));
    }
    p.actor.validated()?;
    if !DomainId::parse(&p.document_id).is_ok_and(|id| id.prefix() == "SDOC")
        || p.document_id != current.document().document_id
    {
        return Err(failure(Code::WrongDocument, None));
    }
    if p.reads.len() > b.reads
        || p.writes.len() > b.writes
        || revisions.len() > b.revisions
        || current.encoded_bytes().len() > b.folio.input_bytes
    {
        return Err(failure(Code::BudgetExceeded, None));
    }
    if encoded_size(p, t)? > b.input_bytes {
        return Err(failure(Code::BudgetExceeded, None));
    }
    snapshot_budget(current, b.folio, t)?;
    if p.writes.is_empty() {
        return Err(failure(Code::InvalidInput, None));
    }
    let mut bytes = 0usize;
    for v in p
        .reads
        .iter()
        .map(|r| &r.expected_value)
        .chain(p.writes.iter().map(|w| &w.value))
    {
        check(t)?;
        bytes = bytes
            .checked_add(size(v))
            .ok_or_else(|| failure(Code::BudgetExceeded, None))?;
        if bytes > b.value_bytes {
            return Err(failure(Code::BudgetExceeded, None));
        }
    }
    let mut vector = BTreeMap::new();
    for e in revisions {
        check(t)?;
        address_valid(&e.address)?;
        if vector.insert(addr_key(&e.address), e.revision).is_some() {
            return Err(failure(Code::DuplicateAddress, Some(&e.address)));
        }
    }
    let mut reads = BTreeMap::new();
    for read in &p.reads {
        check(t)?;
        address_valid(&read.address)?;
        value_type(&read.address, &read.expected_value)?;
        let key = addr_key(&read.address);
        if reads.insert(key.clone(), read).is_some() {
            return Err(failure(Code::DuplicateAddress, Some(&read.address)));
        }
        // Reads quote state; a command value is not a state.
        if matches!(&read.expected_value, PropertyValue::Place(_)) {
            return Err(failure(Code::WrongType, Some(&read.address)));
        }
        let actual = value(current.document(), &read.address)?;
        let revision = revision_at(&vector, &read.address, current.document())?;
        if revision != read.expected_revision {
            return Err(failure(Code::RevisionConflict, Some(&read.address)));
        }
        if !actual.equals(&read.expected_value) {
            return Err(failure(Code::ValueConflict, Some(&read.address)));
        }
    }
    let mut write_keys = BTreeSet::new();
    let mut changed = Vec::new();
    let mut updates = Vec::new();
    let mut inverse_writes = Vec::new();
    for write in &p.writes {
        check(t)?;
        address_valid(&write.address)?;
        value_type(&write.address, &write.value)?;
        let key = addr_key(&write.address);
        if !write_keys.insert(key.clone()) {
            return Err(failure(Code::DuplicateAddress, Some(&write.address)));
        }
        if !reads.contains_key(&key) {
            return Err(failure(Code::MissingRead, Some(&write.address)));
        }
    }
    if p
        .writes
        .iter()
        .any(|w| w.address.property == Property::Placement)
    {
        return prepare_structure(p, current, &vector, b, t, resolver);
    }
    for write in &p.writes {
        check(t)?;
        let actual = value(current.document(), &write.address)?;
        if !actual.equals(&write.value) {
            let previous_revision = revision_at(&vector, &write.address, current.document())?;
            let revision = previous_revision
                .checked_add(1)
                .ok_or_else(|| failure(Code::RevisionOverflow, Some(&write.address)))?;
            updates.push(RevisionUpdate {
                address: write.address.clone(),
                previous_revision,
                revision,
            });
            inverse_writes.push(PropertyWrite {
                address: write.address.clone(),
                value: actual.owned(),
            });
            changed.push(write);
        }
    }
    if changed.is_empty() {
        check(t)?;
        return Ok(no_change(p, current));
    }
    let next = current
        .document()
        .revision
        .checked_add(1)
        .ok_or_else(|| failure(Code::RevisionOverflow, None))?;
    // Count the canonical prospective serialization without owning a second document.
    let mut prospective_bytes = encoded_size(current.document(), t)?;
    prospective_bytes = prospective_bytes
        .checked_sub(encoded_size(&current.document().revision, t)?)
        .and_then(|n| n.checked_add(next.to_string().len()))
        .ok_or_else(|| failure(Code::BudgetExceeded, None))?;
    for write in &changed {
        check(t)?;
        let old = &reads
            .get(&addr_key(&write.address))
            .ok_or_else(|| failure(Code::MissingRead, Some(&write.address)))?
            .expected_value;
        let old_bytes = scalar_size(old, t)?;
        let new_bytes = scalar_size(&write.value, t)?;
        prospective_bytes = prospective_bytes
            .checked_sub(old_bytes)
            .and_then(|n| n.checked_add(new_bytes))
            .ok_or_else(|| failure(Code::BudgetExceeded, None))?;
    }
    if prospective_bytes > b.folio.input_bytes {
        return Err(failure(Code::BudgetExceeded, None));
    }
    let mut d = current.document().clone();
    for write in changed {
        check(t)?;
        set(&mut d, &write.address, &write.value)?;
    }
    d.revision = next;
    finish(p, current, d, updates, inverse_writes, b, t, resolver)
}
fn no_change(p: &Patch, current: &Snapshot) -> Prepared {
    Prepared {
        wire: SourceResult::NoChange {
            document_id: p.document_id.clone(),
            command_id: p.command_id.clone(),
            correlation_id: p.correlation_id,
            actor: p.actor.clone(),
            observed_current_revision: current.document().revision,
        },
        snapshot: None,
    }
}
/// The revision of `a` in the caller's vector. A placement address of a layer the document does
/// not hold and the vector does not name is at revision 0 (never seen); every other missing entry
/// is unavailable (no default zero).
fn revision_at(
    vector: &BTreeMap<(String, Property), u64>,
    a: &PropertyAddress,
    d: &StudioDocument,
) -> Result<u64> {
    if let Some(revision) = vector.get(&addr_key(a)) {
        return Ok(*revision);
    }
    if a.property == Property::Placement && !d.layers.iter().any(|l| l.layer_id == a.target.node_id)
    {
        return Ok(0);
    }
    Err(failure(Code::RevisionUnavailable, Some(a)))
}
/// Patches that move, create or delete layers: writes apply in order on a working copy, each
/// change detected and inverted against the state the previous writes left (so the reversed
/// inverse restores the exact stack), then Folio validates the successor.
fn prepare_structure(
    p: &Patch,
    current: &Snapshot,
    vector: &BTreeMap<(String, Property), u64>,
    b: Budget,
    t: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<Prepared> {
    let next = current
        .document()
        .revision
        .checked_add(1)
        .ok_or_else(|| failure(Code::RevisionOverflow, None))?;
    let mut d = current.document().clone();
    let mut updates = Vec::new();
    let mut inverse_writes = Vec::new();
    for write in &p.writes {
        check(t)?;
        let before = value(&d, &write.address)?;
        let (unchanged, restore) = if write.address.property == Property::Placement {
            let id = write.address.target.node_id.as_str();
            let PropertyValue::Place(command) = &write.value else {
                // [STU-RAS-114]: index-only placement writes are forbidden.
                return Err(failure(Code::InvalidInput, Some(&write.address)));
            };
            let target = match command {
                None => None,
                Some(c) => Some(resolve(&d, id, c, &write.address)?),
            };
            let now = placement_of(&d, id);
            let unchanged = d.layers.iter().any(|l| l.layer_id == id) == target.is_some()
                && now == target;
            if unchanged && command.as_ref().is_some_and(|c| c.create.is_some()) {
                return Err(failure(Code::InvalidInput, Some(&write.address)));
            }
            // The restoring command: re-create a deleted layer exactly, move a moved one back,
            // delete a created one.
            let restore = match (now.is_some(), &target) {
                (false, _) => PropertyValue::Place(None),
                (true, None) => PropertyValue::Place(Some(PlaceCommand {
                    create: Some(Box::new(capture(&d, id).ok_or_else(|| {
                        failure(Code::InvalidTarget, Some(&write.address))
                    })?)),
                    ..restore_command(&d, id)
                        .ok_or_else(|| failure(Code::InvalidTarget, Some(&write.address)))?
                })),
                (true, Some(_)) => PropertyValue::Place(Some(
                    restore_command(&d, id)
                        .ok_or_else(|| failure(Code::InvalidTarget, Some(&write.address)))?,
                )),
            };
            (unchanged, restore)
        } else {
            (before.equals(&write.value), before.owned())
        };
        if unchanged {
            continue;
        }
        let previous_revision = revision_at(vector, &write.address, current.document())?;
        let revision = previous_revision
            .checked_add(1)
            .ok_or_else(|| failure(Code::RevisionOverflow, Some(&write.address)))?;
        updates.push(RevisionUpdate {
            address: write.address.clone(),
            previous_revision,
            revision,
        });
        inverse_writes.push(PropertyWrite {
            address: write.address.clone(),
            value: restore,
        });
        set(&mut d, &write.address, &write.value)?;
    }
    if updates.is_empty() {
        check(t)?;
        return Ok(no_change(p, current));
    }
    inverse_writes.reverse();
    d.revision = next;
    finish(p, current, d, updates, inverse_writes, b, t, resolver)
}
/// Validates the successor and builds the changed result with its inverse.
#[allow(clippy::too_many_arguments)]
fn finish(
    p: &Patch,
    current: &Snapshot,
    d: StudioDocument,
    updates: Vec<RevisionUpdate>,
    inverse_writes: Vec<PropertyWrite>,
    b: Budget,
    t: &CancellationToken,
    resolver: &dyn Resolver,
) -> Result<Prepared> {
    let snapshot = hsk_studio_folio::validate_document(d, b.folio, t, resolver).map_err(|e| {
        failure(
            match e.code {
                hsk_studio_folio::Code::Canceled => Code::Canceled,
                hsk_studio_folio::Code::BudgetExceeded => Code::BudgetExceeded,
                _ => Code::FolioRejected,
            },
            None,
        )
    })?;
    let mut inverse_reads = p.reads.clone();
    for update in &updates {
        check(t)?;
        let read = inverse_reads
            .iter_mut()
            .find(|r| r.address == update.address)
            .ok_or_else(|| failure(Code::MissingRead, Some(&update.address)))?;
        read.expected_revision = update.revision;
        read.expected_value = value(snapshot.document(), &update.address)?.owned();
    }
    let inverse = Inverse {
        transport_version: 1,
        document_id: p.document_id.clone(),
        source_command_id: p.command_id.clone(),
        reads: inverse_reads,
        writes: inverse_writes,
    };
    check(t)?;
    // The wire projects the already validated canonical document; bound this ownership clone too.
    snapshot_budget(&snapshot, b.folio, t)?;
    let wire = SourceResult::Changed {
        document_id: p.document_id.clone(),
        command_id: p.command_id.clone(),
        correlation_id: p.correlation_id,
        actor: p.actor.clone(),
        observed_current_revision: current.document().revision,
        successor: Box::new(snapshot.document().clone()),
        revision_updates: updates,
        inverse,
    };
    check(t)?;
    Ok(Prepared {
        wire,
        snapshot: Some(snapshot),
    })
}
/// Delivery failure stays separate from already returned preparation/acceptance state.
#[derive(Clone, Copy, Debug)]
pub struct DeliveryReport {
    pub receipt: std::result::Result<hsk_studio_observe::Receipt, hsk_studio_observe::Error>,
    pub state: hsk_studio_observe::State,
}
pub fn deliver(
    p: &Patch,
    revision: u64,
    outcome: Outcome,
    sink: &mut impl SinkPort,
    t: &CancellationToken,
) -> Result<DeliveryReport> {
    let resource =
        DomainId::parse(&p.document_id).map_err(|_| failure(Code::WrongDocument, None))?;
    if resource.prefix() != "SDOC" {
        return Err(failure(Code::WrongDocument, None));
    }
    let mut observe = Observe::new(
        p.correlation_id,
        revision,
        resource,
        p.actor.validated()?,
        hsk_studio_observe::Budget::new(0, 0).map_err(|_| failure(Code::InvalidInput, None))?,
    );
    let receipt = observe.emit(
        Observation {
            correlation_id: p.correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        t,
        sink,
    );
    Ok(DeliveryReport {
        receipt,
        state: observe.state(),
    })
}
/// Adapt the actual preparation result to the existing Observe sink. Delivery is separate
/// from source acceptance; the caller retains the supplied result even on delivery failure.
pub fn deliver_result(
    p: &Patch,
    revision: u64,
    result: std::result::Result<&Prepared, &Diagnostic>,
    budget: Budget,
    sink: &mut impl SinkPort,
    token: &CancellationToken,
) -> Result<DeliveryReport> {
    use hsk_studio_observe::{
        DiagnosticAddress, DiagnosticCode, DiagnosticDetail, DiagnosticProperty, DiagnosticTarget,
        Disposition, FailureCode, MAX_DETAIL_COUNT,
    };
    let resource =
        DomainId::parse(&p.document_id).map_err(|_| failure(Code::WrongDocument, None))?;
    if resource.prefix() != "SDOC" {
        return Err(failure(Code::WrongDocument, None));
    }
    let actor = p.actor.validated()?;
    let capped = |n: usize| n.min(MAX_DETAIL_COUNT as usize) as u32;
    let mut counts = [capped(p.reads.len()), capped(p.writes.len()), 0, 0, 0];
    let (disposition, code, address, outcome) = match result {
        Ok(prepared) => {
            if p.reads.len() > budget.reads || p.writes.len() > budget.writes {
                return Err(failure(Code::BudgetExceeded, None));
            }
            let (document_id, command_id, correlation_id, result_actor, changed) =
                match prepared.wire() {
                    SourceResult::Changed {
                        document_id,
                        command_id,
                        correlation_id,
                        actor,
                        ..
                    } => (document_id, command_id, correlation_id, actor, true),
                    SourceResult::NoChange {
                        document_id,
                        command_id,
                        correlation_id,
                        actor,
                        ..
                    } => (document_id, command_id, correlation_id, actor, false),
                };
            if document_id != &p.document_id
                || command_id != &p.command_id
                || *correlation_id != p.correlation_id
                || result_actor != &p.actor
            {
                return Err(failure(Code::InvalidInput, None));
            }
            counts[2] = counts[0];
            counts[3] = counts[1];
            counts[4] = capped(prepared.revision_updates().len());
            (
                if changed {
                    Disposition::Changed
                } else {
                    Disposition::NoChange
                },
                None,
                None,
                Outcome::Success,
            )
        }
        Err(error) => {
            let code = match error.code {
                Code::InvalidInput => DiagnosticCode::InvalidInput,
                Code::UnsupportedVersion => DiagnosticCode::UnsupportedVersion,
                Code::DuplicateField => DiagnosticCode::DuplicateField,
                Code::InvalidActor => DiagnosticCode::InvalidActor,
                Code::InvalidCommand => DiagnosticCode::InvalidCommand,
                Code::WrongDocument => DiagnosticCode::WrongDocument,
                Code::InvalidTarget => DiagnosticCode::InvalidTarget,
                Code::WrongType => DiagnosticCode::WrongType,
                Code::DuplicateAddress => DiagnosticCode::DuplicateAddress,
                Code::MissingRead => DiagnosticCode::MissingRead,
                Code::RevisionUnavailable => DiagnosticCode::RevisionUnavailable,
                Code::RevisionConflict => DiagnosticCode::RevisionConflict,
                Code::ValueConflict => DiagnosticCode::ValueConflict,
                Code::RevisionOverflow => DiagnosticCode::RevisionOverflow,
                Code::BudgetExceeded => DiagnosticCode::BudgetExceeded,
                Code::Canceled => DiagnosticCode::Canceled,
                Code::FolioRejected => DiagnosticCode::ValidationRejected,
            };
            // A diagnostic supplied by a caller cannot serialize an unvalidated ID/property.
            // Observe's closed diagnostic vocabulary names only `name` and `visible`; a failure at an
            // appearance address reports its code without an address (no new telemetry field).
            let address = error
                .address
                .as_ref()
                .filter(|a| address_valid(a).is_ok())
                .filter(|a| matches!(a.property, Property::Name | Property::Visible))
                .map(|a| {
                    let target = match a.target.node_kind {
                        NodeKind::Layer => DiagnosticTarget::Layer,
                        NodeKind::Artboard => DiagnosticTarget::Artboard,
                        NodeKind::PageSpread => DiagnosticTarget::PageSpread,
                    };
                    let property = match a.property {
                        Property::Visible => DiagnosticProperty::Visible,
                        Property::Name
                        | Property::Opacity
                        | Property::BlendMode
                        | Property::Tiles
                        | Property::Placement => DiagnosticProperty::Name,
                    };
                    DiagnosticAddress::new(
                        target,
                        property,
                        DomainId::parse(&a.target.node_id).expect("validated address"),
                    )
                })
                .transpose()
                .map_err(|_| failure(Code::InvalidTarget, None))?;
            (
                Disposition::Rejected,
                Some(code),
                address,
                if error.code == Code::Canceled {
                    Outcome::Canceled
                } else {
                    Outcome::Failure(FailureCode::Validation)
                },
            )
        }
    };
    let detail = DiagnosticDetail::new(disposition, code, address, counts)
        .map_err(|_| failure(Code::InvalidInput, None))?;
    let mut observe = Observe::new(
        p.correlation_id,
        revision,
        resource,
        actor,
        hsk_studio_observe::Budget::new(0, 0).map_err(|_| failure(Code::InvalidInput, None))?,
    );
    let receipt = observe.emit_detail(
        Observation {
            correlation_id: p.correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        &detail,
        token,
        sink,
    );
    Ok(DeliveryReport {
        receipt,
        state: observe.state(),
    })
}
// Bounded unique-key decoding checks array counts before pushing, byte counts before cloning,
// and depth/cancellation at every visited JSON value. Value is temporary transport, not authority.
struct DecodeContext<'a> {
    budget: Budget,
    token: &'a CancellationToken,
    value_bytes: Cell<usize>,
}
struct RejectElement;
impl<'de> DeserializeSeed<'de> for RejectElement {
    type Value = ();
    fn deserialize<D: serde::Deserializer<'de>>(self, _: D) -> std::result::Result<(), D::Error> {
        Err(de::Error::custom("budget"))
    }
}
struct Seed<'a, 'b> {
    context: &'a DecodeContext<'b>,
    depth: usize,
    array_limit: usize,
    value_text: bool,
}
impl<'de> DeserializeSeed<'de> for Seed<'_, '_> {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        d: D,
    ) -> std::result::Result<Value, D::Error> {
        if self.depth > 32 {
            return Err(de::Error::custom("budget"));
        }
        if self.context.token.check().is_err() {
            return Err(de::Error::custom("canceled"));
        }
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Seed<'_, '_> {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("bounded unique-key JSON")
    }
    fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Value, E> {
        Ok(v.into())
    }
    fn visit_f64<E: de::Error>(self, v: f64) -> std::result::Result<Value, E> {
        serde_json::Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid number"))
    }
    fn visit_unit<E: de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Value, E> {
        if self.value_text {
            let n = self
                .context
                .value_bytes
                .get()
                .checked_add(v.len())
                .ok_or_else(|| E::custom("budget"))?;
            if n > self.context.budget.value_bytes {
                return Err(E::custom("budget"));
            }
            self.context.value_bytes.set(n);
        }
        Ok(v.into())
    }
    fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<Value, E> {
        self.visit_str(&v)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> std::result::Result<Value, A::Error> {
        let mut out = Vec::new();
        loop {
            if out.len() >= self.array_limit {
                let _ = a.next_element_seed(RejectElement)?;
                break;
            }
            let next = a.next_element_seed(Seed {
                context: self.context,
                depth: self.depth.saturating_add(1),
                array_limit: 16,
                value_text: false,
            })?;
            let Some(v) = next else {
                break;
            };
            if out.len() >= self.array_limit {
                return Err(de::Error::custom("budget"));
            }
            out.push(v);
        }
        Ok(Value::Array(out))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> std::result::Result<Value, A::Error> {
        let mut out = serde_json::Map::new();
        while let Some(k) = a.next_key::<String>()? {
            if out.contains_key(&k) {
                return Err(de::Error::custom("duplicate_field"));
            }
            if out.len() >= 32 {
                return Err(de::Error::custom("budget"));
            }
            let limit = match k.as_str() {
                "reads" => self.context.budget.reads,
                "writes" => self.context.budget.writes,
                // A tile list value: one entry per tile, bounded like the revision vector.
                "value" | "expected_value" => self.context.budget.revisions,
                // The tile lists inside a created layer's record (a duplicated raster layer).
                "tiles" => self.context.budget.revisions,
                _ => 16,
            };
            let v = a.next_value_seed(Seed {
                context: self.context,
                depth: self.depth.saturating_add(1),
                array_limit: limit,
                value_text: k == "value",
            })?;
            out.insert(k, v);
        }
        Ok(Value::Object(out))
    }
}
fn decode_value(
    input: &[u8],
    b: Budget,
    t: &CancellationToken,
    array_limit: usize,
) -> Result<Value> {
    check(t)?;
    if input.len() > b.input_bytes {
        return Err(failure(Code::BudgetExceeded, None));
    }
    let context = DecodeContext {
        budget: b,
        token: t,
        value_bytes: Cell::new(0),
    };
    let mut decoder = serde_json::Deserializer::from_slice(input);
    let value = Seed {
        context: &context,
        depth: 0,
        array_limit,
        value_text: false,
    }
    .deserialize(&mut decoder)
    .map_err(|e| {
        failure(
            if e.to_string().contains("duplicate_field") {
                Code::DuplicateField
            } else if e.to_string().contains("canceled") {
                Code::Canceled
            } else if e.to_string().contains("budget") {
                Code::BudgetExceeded
            } else {
                Code::InvalidInput
            },
            None,
        )
    })?;
    decoder
        .end()
        .map_err(|_| failure(Code::InvalidInput, None))?;
    check(t)?;
    Ok(value)
}
pub fn decode_patch(input: &[u8], b: Budget, t: &CancellationToken) -> Result<Patch> {
    let v = decode_value(input, b, t, 16)?;
    if v.get("transport_version").and_then(Value::as_u64) != Some(1) {
        return Err(failure(Code::UnsupportedVersion, None));
    }
    serde_json::from_value(v).map_err(|_| failure(Code::InvalidInput, None))
}
pub fn decode_revisions(
    input: &[u8],
    b: Budget,
    t: &CancellationToken,
) -> Result<Vec<RevisionEntry>> {
    serde_json::from_value(decode_value(input, b, t, b.revisions)?)
        .map_err(|_| failure(Code::InvalidInput, None))
}
pub fn patch_schema() -> schemars::Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<Patch>()
}
pub fn result_schema() -> schemars::Schema {
    SchemaSettings::draft2020_12()
        .into_generator()
        .into_root_schema_for::<SourceResult>()
}
pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-CHRONICLE","version":1,"scope":"Pure CON022-026 source-local patch preparation; no persisted StudioEditProposal/HistoryEntry, approval, EventLedger, CRDT, retained history or host acceptance","api":"decode_patch / decode_revisions / prepare / Inverse::invocation / deliver_result / legacy deliver / addressability table: PROPERTIES, addressable(kind, property), seed_revisions(document), mask_property(document, address)","consumer":"chronicle-consumer --document FILE --revisions FILE --patch FILE [--patch FILE ...] [--inverse] [--cancel-before] [--cancel-after] [--delivery delivered|rejected|indeterminate] [--max-bytes N --max-reads N --max-writes N --max-value-bytes N]; --schema; --result-schema; --descriptor","input":"Explicit version1 actor/doc/base provenance, reads+writes of layer name, visible, opacity (percent number), blend_mode (StudioBlendMode discriminant number; a group write also sets its isolation) or tiles (a raster layer's complete tile list, seeded only for raster layers; new digests must be made available by the caller, e.g. session staging), placement (a layer's stack parent and index, absent = no such layer, read as state Placement; written as the enumerated command Place of STU-RAS-114 - place_inside, place_at_beginning, place_at_end, place_before, place_after (directions read like the Photoshop document model, UNVERIFIED) - index-only writes refused invalid_input; create with a LayerCreate record+operation, Place(None) deletes, move/reorder/group/ungroup; writes of one patch apply in order and the inverse restores in reverse, a delete's inverse re-creates the record; a never-seen layer is at revision 0; tests/structure.rs create_reorder_delete_and_undo_restore_the_stack, group_and_ungroup_apply_in_order_and_invert_exactly, hostile_structure_edits_are_typed_and_change_nothing), artboard/page-spread name; an appearance equal to the kind identity is stored absent so a write and its inverse restore the original document (tests appearance_properties_transact_and_invert_to_the_original_document, tiles_property_is_raster_only_and_inverts); caller-owned current revision entries; immutable Folio document; budget/cancellation/resolver","result":"changed with actual canonical successor/revision_updates/conditional inverse, or closed metadata-only no_change; typed failure with stable address and conflict dimension","recovery":"Refresh current snapshot and property vector; re-evaluate conflict against fresh current state; missing entries unavailable, no default zero; correct type/reference; invert with fresh attribution only when conditional reads still hold","parallel":"Caller serializes latest read and local publication; disjoint old-base footprints succeed; global successor revision is bookkeeping, not CAS; no private queue/database","manual":"Same typed transport and generated draft2020-12 schema shared by model/API/operator","argus":{"inspect":"actual snapshot, property updates, inverse and typed diagnostic address","steer":"same bounded prepare API","state":"explicit changed/no_change/failure plus delivery counters","capture":"caller captures protected actual output without focus stealing"},"diagnostics":"Observe emit_detail shared closed version2 stable code/validated conflict address (name and visible only: observe has no opacity/blend_mode/tiles property, so those failures report the code without an address)/source disposition and capped attempted/admitted/changed counters; result retained independently of delivery; names/string values/raw input are not telemetry (test actual_detail_changed_nochange_inverse_conflict_and_privacy); missing/indeterminate delivery cannot undo accepted source result","host":"Pending actual grants, serialized SurrealDB/EventLedger promotion, idempotency/restart, history cursor, native GUI and three diagnostic tiers"}"#;
