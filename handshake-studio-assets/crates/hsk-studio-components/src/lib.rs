//! Components, variables, responsive constraints, prototype actions within same document; no
//! separate Figma-like app.
//!
//! Slice 1: component definitions and variant sets, instances with property-granular overrides,
//! variables with collections, modes and alias chains, constraint-based resize, and a prototype
//! action graph validated against the Folio document's frames. Everything is a pure function over
//! borrowed values; results are proposals and typed refusals, never mutations of the inputs.
#![forbid(unsafe_code)]

mod constraints;
mod folio_bridge;
mod instance_resolution;
mod prototype;

pub use constraints::{
    ConstraintKind, Constraints, NodeGeometry, PlacedNode, RectPt, SizePt, apply_constraints,
    resize_component,
};
pub use folio_bridge::{INSTANCE_LAYER_BINDING, instance_layer};
pub use instance_resolution::{
    Collection, ComponentDef, ComponentSet, DefNode, DefRegistry, DroppedOverride, Instance,
    InstanceProp, Limits, Mode, OVERRIDABLE_FIELDS, OverrideEdit, OverrideProposal, OverrideRecord,
    PropType, PropertyDef, Resolution, ResolvedField, ResolvedVariable, Source, Value, VarEntry,
    VarRegistry, VarType, Variable, check_footprint, is_overridable, propose_overrides,
    rematch_overrides, resolve_instance, resolve_variable, swap_component, switch_variant,
};
pub use prototype::{
    Action, FlowLimits, FlowReport, FlowWarning, Navigation, PrototypeFlow, Reaction, Trigger,
    validate_flow,
};

use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{FailureCode, Observation, Observe, Outcome, Receipt, SinkPort};

pub(crate) type R<T> = Result<T, ComponentsError>;

/// Typed refusals. A refusal never carries a partial result and never alters its inputs. Strings
/// are ids or field names (never document text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComponentsError {
    InvalidId(String),
    InvalidValue(String),
    Duplicate(String),
    NoSuchComponent(String),
    NoSuchNode(String),
    NoSuchVariable(String),
    NoSuchCollection(String),
    NoSuchMode {
        collection_id: String,
        mode_id: String,
    },
    /// No set member carries the requested variant assignment.
    NoSuchVariant,
    UnknownVariantValue {
        property: String,
        value: String,
    },
    /// STU-DS-109 `variant_assignment_incomplete`.
    VariantAssignmentIncomplete(String),
    /// STU-DS-109 `variant_assignment_duplicate`.
    VariantAssignmentDuplicate(String),
    AliasCycle(String),
    TypeMismatch(String),
    UnsupportedProperty(String),
    FieldNotOverridable {
        node_id: String,
        field: String,
    },
    AmbiguousSource {
        node_id: String,
        field: String,
    },
    MissingModeValue {
        variable_id: String,
        mode_id: String,
    },
    /// Same key changed since the proposal was made (`field` empty = record-level key).
    StaleProperty {
        node_id: String,
        field: String,
    },
    InvalidGeometry,
    /// A prototype destination or start frame is not a frame of the document.
    PrototypeTarget(String),
    UnsupportedPrototype(String),
    BudgetExceeded,
    Canceled,
}
impl ComponentsError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidId(_) => "invalid_id",
            Self::InvalidValue(_) => "invalid_value",
            Self::Duplicate(_) => "duplicate",
            Self::NoSuchComponent(_) => "no_such_component",
            Self::NoSuchNode(_) => "no_such_node",
            Self::NoSuchVariable(_) => "no_such_variable",
            Self::NoSuchCollection(_) => "no_such_collection",
            Self::NoSuchMode { .. } => "no_such_mode",
            Self::NoSuchVariant => "no_such_variant",
            Self::UnknownVariantValue { .. } => "unknown_variant_value",
            Self::VariantAssignmentIncomplete(_) => "variant_assignment_incomplete",
            Self::VariantAssignmentDuplicate(_) => "variant_assignment_duplicate",
            Self::AliasCycle(_) => "alias_cycle",
            Self::TypeMismatch(_) => "type_mismatch",
            Self::UnsupportedProperty(_) => "unsupported_property",
            Self::FieldNotOverridable { .. } => "field_not_overridable",
            Self::AmbiguousSource { .. } => "ambiguous_source",
            Self::MissingModeValue { .. } => "missing_mode_value",
            Self::StaleProperty { .. } => "stale_property",
            Self::InvalidGeometry => "invalid_geometry",
            Self::PrototypeTarget(_) => "prototype_target",
            Self::UnsupportedPrototype(_) => "unsupported_prototype",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Canceled => "canceled",
        }
    }
    /// `None` for cancellation (reported as the Canceled outcome).
    pub fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::BudgetExceeded | Self::UnsupportedPrototype(_) => Some(FailureCode::Unsupported),
            Self::NoSuchComponent(_) | Self::NoSuchVariable(_) | Self::NoSuchCollection(_) => {
                Some(FailureCode::Unavailable)
            }
            _ => Some(FailureCode::Validation),
        }
    }
}
impl std::fmt::Display for ComponentsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ComponentsError {}

/// One Observe terminal outcome per call; no ids, names or values are serialized.
pub fn report<T>(
    result: &Result<T, ComponentsError>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(e) => e.failure_code().map_or(Outcome::Canceled, Outcome::Failure),
    };
    observe.emit(
        Observation {
            correlation_id,
            revision,
            outcome,
            progress: None,
            private_project_text: None,
        },
        cancel,
        sink,
    )
}

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-COMPONENTS","version":1,"operation":"resolve_instance / propose_overrides / resolve_variable / apply_constraints / resize_component / validate_flow: component definitions and variant sets, instances with property-granular overrides, variables with collections, modes and alias chains, constraint-based resize and a prototype action graph, all as pure values and revision-footprint proposals","operator":{"intent":"See exactly which value an instance field gets and from where, change one property without disturbing others, resize a component and see where children land, and check that prototype clicks lead to real frames","result":"Resolution {component_id, fields[{node_id, field, value, source, reads}]}, OverrideProposal {instance, resolution, write_footprint, read_footprint}, PlacedNode[] in points, FlowReport {reachable_frames, warnings} or a typed refusal","recovery":"alias_cycle/type_mismatch: fix the variable definitions; missing_mode_value: give every collection mode a value; unsupported_property: define the component property first; field_not_overridable: only the STU-DS-115 fields can be overridden; ambiguous_source: remove the duplicate literal/variable binding or duplicate override; stale_property: re-read the instance and re-propose; invalid_geometry: use finite non-negative point sizes and a parent large enough for stretched children; prototype_target: point actions at an existing artboard; canceled: nothing was produced"},"model":{"precedence":"STU-DS-116: local override, then instance component property (else the property default), then the definition literal, then the definition variable binding; exactly one source per field","variants":"instance of a set resolves to default_variant_id; VARIANT-type instance properties re-select the member; completeness and duplicate assignments are refused when the set is registered","variables":"collection modes (min 1), exactly one value per mode, alias chains walked with cycle and type checks, mode chosen per collection (default mode otherwise); a missing mode value is an error, never zero","overrides":"STU-DS-117 shape plus a values map on each record (spec gap: value location is unspecified); never a flattened subtree copy; base definitions are never mutated","constraints":"MIN/CENTER/MAX/STRETCH/SCALE per axis in points, default MIN/MIN; a stretched child that would collapse below zero is refused, not clamped","prototype":"reactions {node_id, trigger|null, actions[]}; NAVIGATE/OVERLAY/SWAP destinations must be artboards of the Folio document; reachability from the start frame; actions after a navigation are warnings","footprints":"proposal footprint = written (node, field) keys plus the definition/variable record keys they read; drift outside it is admissible, drift inside it is stale_property (STU-ARC-011, STU-DS-163)"},"limits":{"max_nodes":4096,"max_fields":65536,"max_alias_depth":32,"max_reactions":4096,"max_actions":16384},"cancel":"CancellationToken is checked at entry, per node and per alias hop","argus":{"inspect":"all result types are plain data","action":"the same call with the same token","capture":"caller captures the result and the Observe frame bytes under a fresh grant"},"diagnostics":"report() emits one Observe terminal outcome; no ids, names or values are serialized by this crate","authority":"Folio has no design-frame, component, instance-record, variable or reaction type: records stay crate-local values and an instance is a Folio layer of kind component_instance referencing SCIN-*; host grants, history and acceptance remain pending"}"#;
