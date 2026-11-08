//! Object-scope live filter stacks of vector layers (lane SMB-AI-LIVE-EFFECTS-1; STU-FX-001/002/003,
//! STU-FX-007, STU-FX-027, STU-VEC-022/023, STU-VEC-154), held in
//! `StudioVectorRecords::live_filters` (absent and empty are the same set; empty is not encoded, so
//! record sets written before it are byte-identical, test `tests/vector_live_filters.rs`).
//!
//! * [`StudioLiveFilterStack`] (Studio-minted holder, lead ruling Q3 2026-11-08): `layer_id` (a
//!   vector layer, at most one stack per layer) and `filters`, in application order (STU-FX-003).
//! * [`StudioLiveFilter`] (STU-FX-002 field names): `instance_id` (set-local key, unique in its
//!   stack), `filter_kind` (the catalogue key; for the vector live effects nib's effect id
//!   `hsk.studio.vector_effect.<snake>`), `version` (the law set the parameters were written for),
//!   `enabled`, and `params`.
//! * [`StudioLiveFilterParam`]: one STU-VEC-154 `(type, key, value)` triple, a serde enum tagged by
//!   `type` with exactly the members `Int`, `Real`, `Bool`, `String`, so an untyped or wrongly typed
//!   value cannot deserialize. List order is kept; a key appears once per entry.
//!
//! Folio validates shape only (Folio has no nib edge): the layer, ids and keys, finite reals, bounded
//! strings and every count before it is walked. An unknown `filter_kind`, version or parameter is not
//! Folio's refusal, so a later law set stays loadable; composite refuses it per layer with nib's code.
//! Out of this slice (STU-VEC-023 object scope partly met; holder placement in the shared
//! `StudioEffectStack` open): group/layer target ring and appearance-row scope, and the STU-FX-002
//! per-entry mask, blend, opacity and `render_scope`.
//!
//! The records arrive from untrusted wire JSON: integer arithmetic is denied (V4-01).
#![deny(clippy::arithmetic_side_effects)]
use super::{Code, Payload, Result, StudioLayer, StudioVectorRecords, err, id, key, values::Ctx};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Most entries of one stack: a declared engine limit, not a per-kind cap (STU-FX-006b allows only a
/// performance budget; STU-RAS-156 forbids a per-kind cap below 10).
pub const LIVE_FILTERS_MAX_PER_STACK: usize = 64;
/// Most parameters of one entry (the largest STU-VEC-155 table, free distort, has 16 rows).
pub const LIVE_FILTER_MAX_PARAMS: usize = 32;
/// Longest `filter_kind` and parameter string value, in bytes.
pub const LIVE_FILTER_MAX_TEXT: usize = 256;
/// Longest `filter_kind`, in bytes.
pub const LIVE_FILTER_KIND_MAX: usize = 128;

/// One typed parameter (STU-VEC-154).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum StudioLiveFilterParam {
    Int { key: String, value: i64 },
    Real { key: String, value: f64 },
    Bool { key: String, value: bool },
    String { key: String, value: String },
}

impl StudioLiveFilterParam {
    /// The parameter key.
    pub fn key(&self) -> &str {
        match self {
            Self::Int { key, .. } | Self::Real { key, .. } | Self::Bool { key, .. } | Self::String { key, .. } => key,
        }
    }
}

/// One stack entry (STU-FX-002).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLiveFilter {
    pub instance_id: String,
    pub filter_kind: String,
    pub version: u32,
    pub enabled: bool,
    pub params: Vec<StudioLiveFilterParam>,
}

/// The live filter stack of one vector layer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioLiveFilterStack {
    pub layer_id: String,
    pub filters: Vec<StudioLiveFilter>,
}

impl StudioLiveFilterStack {
    /// True when at least one entry is enabled (a stack of disabled entries draws like none).
    pub fn any_enabled(&self) -> bool {
        self.filters.iter().any(|f| f.enabled)
    }
}

impl StudioVectorRecords {
    /// The live filter stack of layer `layer_id`.
    pub fn live_filter_stack(&self, layer_id: &str) -> Option<&StudioLiveFilterStack> {
        self.live_filters.iter().find(|s| s.layer_id == layer_id)
    }
}

fn text_ok(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
}

fn kind_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= LIVE_FILTER_KIND_MAX && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

fn invalid(target: &str, field: &str) -> super::Diagnostic {
    err(Code::InvalidField, format!("{target}/{field}"))
}

/// The `live_filters` member inside `validate_vector_records` (module docs). The member count is
/// checked and charged by the caller with the other tool members.
pub(crate) fn validate_live_filters(r: &StudioVectorRecords, layers: &BTreeMap<&str, &StudioLayer>, cx: &mut Ctx) -> Result<()> {
    let mut stacked: BTreeSet<&str> = BTreeSet::new();
    for s in &r.live_filters {
        cx.tick()?;
        let t = s.layer_id.as_str();
        id(t, "SLYR", "live_filters/layer_id")?;
        let l = layers.get(t).ok_or_else(|| err(Code::DanglingReference, format!("{t}/live_filters")))?;
        if !matches!(l.payload, Payload::Vector { .. }) {
            return Err(err(Code::PayloadMismatch, format!("{t}/live_filters")));
        }
        if !stacked.insert(t) {
            return Err(err(Code::DuplicateIdentity, format!("{t}/live_filters")));
        }
        if s.filters.len() > LIVE_FILTERS_MAX_PER_STACK {
            return Err(err(Code::BudgetExceeded, format!("{t}/live_filters/filters")));
        }
        cx.nodes(s.filters.len(), t)?;
        let mut instances: BTreeSet<&str> = BTreeSet::new();
        for f in &s.filters {
            cx.tick()?;
            if !key(&f.instance_id) {
                return Err(err(Code::InvalidId, format!("{t}/live_filters/instance_id")));
            }
            if !instances.insert(f.instance_id.as_str()) {
                return Err(err(Code::DuplicateIdentity, format!("{t}/live_filters/{}", f.instance_id)));
            }
            let ft = format!("{t}/live_filters/{}", f.instance_id);
            if !kind_ok(&f.filter_kind) {
                return Err(invalid(&ft, "filter_kind"));
            }
            if f.params.len() > LIVE_FILTER_MAX_PARAMS {
                return Err(err(Code::BudgetExceeded, format!("{ft}/params")));
            }
            let mut keys: BTreeSet<&str> = BTreeSet::new();
            for p in &f.params {
                if !key(p.key()) {
                    return Err(invalid(&ft, "params/key"));
                }
                if !keys.insert(p.key()) {
                    return Err(err(Code::DuplicateIdentity, format!("{ft}/params/{}", p.key())));
                }
                let ok = match p {
                    StudioLiveFilterParam::Real { value, .. } => value.is_finite(),
                    StudioLiveFilterParam::String { value, .. } => text_ok(value, LIVE_FILTER_MAX_TEXT),
                    StudioLiveFilterParam::Int { .. } | StudioLiveFilterParam::Bool { .. } => true,
                };
                if !ok {
                    return Err(invalid(&ft, &format!("params/{}", p.key())));
                }
            }
        }
    }
    Ok(())
}
