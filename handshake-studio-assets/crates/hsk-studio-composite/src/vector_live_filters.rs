//! Live filter stacks of vector layers (lane SMB-AI-LIVE-EFFECTS-1; STU-FX-002/003/011/027,
//! STU-VEC-154): the one converter from Folio's `StudioLiveFilter` to nib's
//! [`EffectRecord`](hsk_studio_nib::live_effect::EffectRecord) and back, and the evaluation of a
//! layer's enabled entries in list order through `nib::live_effect::evaluate`, each result the next
//! entry's sources (bounds-relative laws read the union of the current sources).
//!
//! `crate::vector`'s Folio adapter calls [`evaluate_stack`] from its own-geometry step, which
//! `prepare_vectors` and `expand_vectors` both use, so render, SVG, PDF and separations read one
//! result; the effected paths then feed the layer's fill, stroke, brush, marker and envelope steps
//! (filters, then envelope: Studio reading, parity UNVERIFIED, D5). The Viewer's typed commands use
//! the same functions (trial evaluation before commit, bake). Geometry is in plan pixels: in a px
//! document nib's `pt` reads as px and `per_in` as per 72 px (lead ruling Q6 2026-11-08); another
//! document unit refuses `vector_geometry_unit`.
//!
//! Refusals are `&'static str` codes per layer, never a silent draw of the source: every nib
//! [`LiveEffectError`] maps to `vector_effect_<nib code>` ([`effect_code`], an exhaustive match, so
//! a new nib variant fails compilation), plus [`REFUSE_EFFECT_NETWORK`],
//! [`REFUSE_EFFECT_TEXT_BOUND`], [`REFUSE_EFFECT_SOURCE`] and [`REFUSE_EFFECT_WORK`].
//!
//! Budget (V4-02): every evaluation charges its source element count against one aggregate budget
//! per `prepare_vectors` / `expand_vectors` call ([`MAX_LIVE_FILTER_ELEMENTS`]) before it runs, so
//! many layers each with a full stack cannot multiply nib's per-evaluation limit.
#![deny(clippy::arithmetic_side_effects)]
use hsk_studio_accord::CancellationToken;
use hsk_studio_folio::{StudioLiveFilter, StudioLiveFilterParam};
use hsk_studio_nib::OwnedPath;
use hsk_studio_nib::live_effect::{EffectRecord, LiveEffectError, ParamValue, evaluate};

/// A network path on a filtered layer (networks as sources are a later slice).
pub const REFUSE_EFFECT_NETWORK: &str = "vector_effect_network";
/// Path or area text bound to a filtered layer (the text would follow the raw path).
pub const REFUSE_EFFECT_TEXT_BOUND: &str = "vector_effect_text_bound";
/// A layer read as raw geometry (clip, vector mask, envelope top object, boolean operand) holds an
/// enabled filter: refused rather than using the unfiltered shape.
pub const REFUSE_EFFECT_SOURCE: &str = "vector_effect_source";
/// The aggregate element budget of one call is spent.
pub const REFUSE_EFFECT_WORK: &str = "vector_effect_work_limit";
/// Source elements all evaluations of one `prepare_vectors` or `expand_vectors` call may read
/// (16 x nib's per-evaluation `MAX_OP_ELEMENTS`). Declared engine limit, not a parameter bound.
pub const MAX_LIVE_FILTER_ELEMENTS: u64 = 1 << 22;

/// The stable composite code of a nib refusal (module docs).
pub const fn effect_code(e: LiveEffectError) -> &'static str {
    use LiveEffectError as E;
    match e {
        E::UnknownEffect => "vector_effect_unknown_effect",
        E::UnsupportedVersion => "vector_effect_unsupported_version",
        E::UnknownParameter => "vector_effect_unknown_parameter",
        E::ParameterNotRecovered => "vector_effect_parameter_not_recovered",
        E::UntypedWrite => "vector_effect_untyped_write",
        E::MalformedValue => "vector_effect_malformed_value",
        E::WrongParameterType => "vector_effect_wrong_parameter_type",
        E::NonFinite => "vector_effect_non_finite",
        E::OutOfHardRange => "vector_effect_out_of_hard_range",
        E::NotACount => "vector_effect_not_a_count",
        E::UnknownEnumMember => "vector_effect_unknown_enum_member",
        E::ParameterUnset => "vector_effect_parameter_unset",
        E::InvalidGeometry => "vector_effect_invalid_geometry",
        E::DegenerateExtent => "vector_effect_degenerate_extent",
        E::DegenerateQuad => "vector_effect_degenerate_quad",
        E::SelfCrossingQuad => "vector_effect_self_crossing_quad",
        E::QuadNotConvex => "vector_effect_quad_not_convex",
        E::PointBeyondHorizon => "vector_effect_point_beyond_horizon",
        E::BeyondEngineRange => "vector_effect_beyond_engine_range",
        E::OutputOverBudget => "vector_effect_output_over_budget",
        E::Canceled => REFUSE_EFFECT_CANCELED,
        E::ConvertToShapeSizingLawNotRecovered => "vector_effect_convert_to_shape_sizing_law_not_recovered",
        E::ObjectMosaicGridLawNotRecovered => "vector_effect_object_mosaic_grid_law_not_recovered",
    }
}

/// A canceled token during an evaluation; the callers turn it into `CompositeError::Canceled`.
pub const REFUSE_EFFECT_CANCELED: &str = "vector_effect_canceled";

/// Folio entry -> nib record: the one reader of the stored parameter map (`filter_kind` is nib's
/// effect id; every entry through `EffectRecord::from_parts`, so a stored value is checked exactly
/// as a fresh write is).
pub fn effect_record(f: &StudioLiveFilter) -> Result<EffectRecord, LiveEffectError> {
    let entries: Vec<(&str, ParamValue)> = f
        .params
        .iter()
        .map(|p| match p {
            StudioLiveFilterParam::Int { key, value } => (key.as_str(), ParamValue::Int(*value)),
            StudioLiveFilterParam::Real { key, value } => (key.as_str(), ParamValue::Real(*value)),
            StudioLiveFilterParam::Bool { key, value } => (key.as_str(), ParamValue::Bool(*value)),
            StudioLiveFilterParam::String { key, value } => (key.as_str(), ParamValue::String(value.clone())),
        })
        .collect();
    EffectRecord::from_parts(&f.filter_kind, f.version, &entries)
}

/// nib record -> Folio parameters: every resolved entry (defaults included), in nib's stable key
/// order, so a stored record does not change if a table default changes later.
pub fn record_params(r: &EffectRecord) -> Vec<StudioLiveFilterParam> {
    r.entries()
        .map(|(k, v)| {
            let key = k.to_string();
            match v {
                ParamValue::Int(value) => StudioLiveFilterParam::Int { key, value: *value },
                ParamValue::Real(value) => StudioLiveFilterParam::Real { key, value: *value },
                ParamValue::Bool(value) => StudioLiveFilterParam::Bool { key, value: *value },
                ParamValue::String(value) => StudioLiveFilterParam::String { key, value: value.clone() },
            }
        })
        .collect()
}

/// True when `filters` holds an enabled entry (a stack of disabled entries draws like none).
pub fn any_enabled(filters: &[StudioLiveFilter]) -> bool {
    filters.iter().any(|f| f.enabled)
}

/// The enabled entries of `filters`, in list order, over `sources`: each result is the next entry's
/// sources. `budget` is the remaining aggregate element budget, charged per evaluation before it
/// runs. Step repeat returns more paths than it took (the sources, then each copy).
pub fn evaluate_stack(filters: &[StudioLiveFilter], sources: Vec<OwnedPath>, budget: &mut u64, cancel: &CancellationToken) -> Result<Vec<OwnedPath>, &'static str> {
    let mut current = sources;
    for f in filters.iter().filter(|f| f.enabled) {
        cancel.check().map_err(|_| REFUSE_EFFECT_CANCELED)?;
        let record = effect_record(f).map_err(effect_code)?;
        let elements = current.iter().map(|p| p.elements.len() as u64).fold(0_u64, u64::saturating_add);
        *budget = budget.checked_sub(elements).ok_or(REFUSE_EFFECT_WORK)?;
        current = evaluate(&current, &record, cancel).map_err(effect_code)?;
    }
    Ok(current)
}

/// Output `k`'s per-path attribute from source `k % n` (`n` = the source count; step repeat
/// copies keep their source's style and width profile).
pub fn cycled<T: Clone>(sources: &[T], outputs: usize) -> Vec<T> {
    (0..outputs).filter_map(|k| k.checked_rem(sources.len()).and_then(|i| sources.get(i)).cloned()).collect()
}
