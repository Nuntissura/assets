//! Components, variables, responsive constraints, prototype actions within same document; no
//! separate Figma-like app.
//!
//! The design records are Folio's: `hsk_studio_folio::StudioDesignRecords` (components, component
//! sets, instances, variable collections and variables, per-node auto layout, child layout, size
//! clamps, constraints, reactions and component property references), validated by
//! `folio::validate_design_records`. This crate keeps no copy of them; it evaluates them: component
//! definitions and variant sets, instances with property-granular overrides, variables with
//! collections, modes and alias chains, constraint-based resize, auto layout, and a prototype action
//! graph validated against the Folio document's frames. A [`Design`] indexes a Folio document, its
//! record set and the [`LocalContent`] that Folio has no home for (definition-field literals,
//! variable bindings, authored layer rectangles). Everything is a pure function over borrowed
//! values; results are proposals and typed refusals, never mutations of the inputs.
#![forbid(unsafe_code)]

mod auto_layout;
mod constraints;
mod design;
mod folio_bridge;
mod instance_resolution;
mod prototype;

pub use auto_layout::{AutoLayoutResult, LayoutLimits, PlacedBox, solve_auto_layout};
pub use constraints::{PlacedNode, RectPt, SizePt, apply_constraints, resize_component};
pub use design::{Design, LocalContent, NodeLocal};
pub use folio_bridge::{INSTANCE_LAYER_BINDING, instance_layer};
pub use instance_resolution::{
    DroppedOverride, Limits, OverrideEdit, OverrideProposal, Resolution, ResolvedField,
    ResolvedVariable, Source, VariableTable, check_footprint, is_overridable, propose_overrides,
    rematch_overrides, resolve_instance, resolve_variable, swap_component, switch_variant,
};
pub use prototype::{FlowLimits, FlowReport, FlowWarning, PrototypeFlow, validate_flow};

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
    /// Same footprint key changed since the proposal was made: `(record_id, "")` record level (an
    /// override proposal writes its instance record and reads definition and variable records) or
    /// `(instance_id, "property/<name>")`.
    StaleProperty {
        node_id: String,
        field: String,
    },
    InvalidGeometry,
    /// Auto-layout input outside its STU-DS-144..149 contract (range, clamp order, id, root FILL).
    InvalidLayout(String),
    /// Container AUTO/FIXED, child HUG/FILL/FIXED, `layout_grow` and `layout_align` disagree.
    InconsistentSizing(String),
    /// Spec-recognised auto-layout feature this slice refuses (GRID, BASELINE, stroke accounting...).
    UnsupportedLayout(String),
    /// A prototype destination or start frame is not a frame of the document.
    PrototypeTarget(String),
    UnsupportedPrototype(String),
    /// A Folio validator refusal that has no more specific variant here (`validate_design_records`
    /// reports the code and the offending record or field path, never document text).
    Folio(hsk_studio_folio::Diagnostic),
    BudgetExceeded,
    Canceled,
}
impl From<hsk_studio_folio::Diagnostic> for ComponentsError {
    /// Folio codes that name the same refusal keep their variant (variant completeness and
    /// uniqueness, alias cycle, duplicate identity, budget, cancellation); the rest stays a
    /// `Folio` refusal with Folio's code and target.
    fn from(d: hsk_studio_folio::Diagnostic) -> Self {
        use hsk_studio_folio::Code;
        match d.code {
            Code::Canceled => Self::Canceled,
            Code::BudgetExceeded => Self::BudgetExceeded,
            Code::VariantAssignmentIncomplete => Self::VariantAssignmentIncomplete(d.target),
            Code::VariantAssignmentDuplicate => Self::VariantAssignmentDuplicate(d.target),
            Code::VariableAliasCycle => Self::AliasCycle(d.target),
            Code::DuplicateIdentity => Self::Duplicate(d.target),
            _ => Self::Folio(d),
        }
    }
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
            Self::InvalidLayout(_) => "invalid_layout",
            Self::InconsistentSizing(_) => "inconsistent_sizing",
            Self::UnsupportedLayout(_) => "unsupported_layout",
            Self::PrototypeTarget(_) => "prototype_target",
            Self::UnsupportedPrototype(_) => "unsupported_prototype",
            Self::Folio(_) => "folio_validation",
            Self::BudgetExceeded => "budget_exceeded",
            Self::Canceled => "canceled",
        }
    }
    /// `None` for cancellation (reported as the Canceled outcome).
    pub fn failure_code(&self) -> Option<FailureCode> {
        match self {
            Self::Canceled => None,
            Self::BudgetExceeded | Self::UnsupportedPrototype(_) | Self::UnsupportedLayout(_) => {
                Some(FailureCode::Unsupported)
            }
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

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-COMPONENTS","version":1,"operation":"resolve_instance / propose_overrides / resolve_variable / apply_constraints / resize_component / validate_flow / solve_auto_layout over Folio design records (StudioDesignRecords): component definitions and variant sets, instances with property-granular overrides, variables with collections, modes and alias chains, constraint-based resize, auto layout and a prototype action graph, all as pure values and revision-footprint proposals","operator":{"intent":"See exactly which value an instance field gets and from where, change one property without disturbing others, resize a component and see where children land, and check that prototype clicks lead to real frames","result":"Resolution {component_id, fields[{node_id, field, value, source, reads}]}, OverrideProposal {instance, resolution, write_footprint, read_footprint} whose successor instance validates in Folio, PlacedNode[] and PlacedBox[] in the document geometry unit, FlowReport {reachable_frames, warnings} or a typed refusal","recovery":"folio_validation: Folio refused a record, correct the record its code and path name; duplicate: give every document node, design entry, component, set, container layer, collection, variable, component instance and paint style its own id; alias_cycle/type_mismatch: fix the variable definitions; missing_mode_value: give every collection mode a value; unsupported_property: define the component property first; field_not_overridable: only the STU-DS-115 fields can be overridden; ambiguous_source: remove the duplicate literal/variable binding or duplicate override; stale_property: re-read the instance and re-propose; invalid_geometry: use finite non-negative sizes and a parent large enough for stretched children; invalid_layout/inconsistent_sizing: give every node an authored rectangle and make container AUTO/FIXED agree with child HUG/FILL/FIXED, grow and align; prototype_target: point actions at an existing artboard; canceled: nothing was produced"},"model":{"records":"Folio owns the records (StudioDesignRecords hsk.studio.design_records@1: components, component sets, instances, variable collections, variables, and per node auto layout, child layout, size clamps, constraints, reactions, component property references), validated by folio::validate_design_records; Design::validate runs it and then the crate rules: two or more members per set, SLOT refused, one source per field, a container rectangle for any definition geometry; a definition's nodes are the document layers under the component's container layer; Design::new refuses a repeated id through folio::check_identity_uniqueness, the single owner of the id namespaces (document node, design entry, component or set, container layer, collection, variable, component instance, paint style) so no lookup picks one of two records","local":"LocalContent per layer, kept here because Folio has no home for it: literal field values of definition nodes, variable bindings of definition fields, and the authored rectangle (numbers in the document geometry unit); PrototypeFlow wraps only the start frame because Folio has no flow container","precedence":"STU-DS-116: local override, then instance component property (else the property default), then the definition literal, then the definition variable binding; exactly one source per field","variants":"an instance of a set or of a member resolves to the member its VARIANT properties select, default_variant_id for a set; completeness and duplicate assignments are Folio refusals (variant_assignment_incomplete/duplicate)","variables":"collection modes (min 1), exactly one value per mode, alias chains walked with cycle and type checks, mode chosen per collection (default mode otherwise); a missing mode value is an error, never zero","overrides":"STU-DS-117 shape; the value of each overridden field is in the record values map (a VARIABLE_ALIAS value resolves through its variable); never a flattened subtree copy; base definitions are never mutated","constraints":"MIN/CENTER/MAX/STRETCH/SCALE per axis from StudioConstraint, default MIN/MIN; a stretched child that would collapse below zero is refused, not clamped","auto_layout":"STU-DS-144..149 subset in f64: horizontal/vertical flow, wrap, gaps (negative legal), padding, primary/counter alignment, SPACE_BETWEEN, fixed/hug/fill with min/max clamps and equal-share grow, absolute children placed by constraints, nesting; container AUTO/FIXED and child HUG/FILL/layout_grow/layout_align must agree (inconsistent_sizing, Folio enforces neither); GRID, BASELINE, strokes_included_in_layout, hug with wrap or space-between are refused as unsupported_layout; taffy not adopted (f32/negative-gap behaviour UNVERIFIED)","prototype":"reactions are the StudioReaction records of the per-node design entries; NAVIGATE/OVERLAY/SWAP destinations must be artboards of the Folio document; reachability from the start frame through armed triggers; actions after a NODE, BACK or CLOSE are warnings (STU-PRO-104)","footprints":"an override proposal carries the whole successor instance, so its write footprint is the instance record key (instance_id, \"\"): two proposals that edit different fields of one instance conflict once either is accepted (test concurrent_proposals_on_one_instance_conflict), two instances of one component stay disjoint (test override_write_keys_are_scoped_to_the_instance); the read footprint holds the definition/variable record keys and instance property keys (instance_id, property/<name>) the edited fields depend on; the host moves (instance_id, \"\") on every accepted write into that record; drift outside the footprint is admissible, drift inside it is stale_property (STU-ARC-011, STU-DS-163)"},"limits":{"max_nodes":4096,"max_fields":65536,"max_alias_depth":32,"max_layout_nodes":4096,"max_layout_depth":32,"max_reactions":4096,"max_actions":16384},"cancel":"CancellationToken is checked at entry, per node and per alias hop","argus":{"inspect":"all result types are plain data","action":"the same call with the same token","capture":"caller captures the result and the Observe frame bytes under a fresh grant"},"diagnostics":"report() emits one Observe terminal outcome; no ids, names or values are serialized by this crate","authority":"Folio is the single authority for the design records and their validation; an instance is a Folio layer of kind component_instance referencing SCIN-* and names a component (a set id is accepted by the evaluator but a proposal naming one is refused by Folio); host grants, history and acceptance remain pending"}"#;
