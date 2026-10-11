//! Evaluation receipt, descriptor and observe reporting.
use crate::{
    error::MotionError,
    expression::{ExprErrorCode, SourceSpan},
    property::PropertyState,
    time::EvalTick,
};
use hsk_studio_accord::CancellationToken;
use hsk_studio_observe::{Observation, Observe, Outcome, Receipt, SinkPort};

/// Profile id of a receipt produced without any expression engine.
pub const PROFILE_KEYFRAMES: &str = "keyframes";

/// What was evaluated: revision, exact tick, explicit seed, profile and state; plus the
/// disabled-by-error disclosure required by STU-MOT-071.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvalReceipt {
    pub revision: u64,
    pub tick: EvalTick,
    pub seed: u64,
    pub profile: &'static str,
    pub state: PropertyState,
    pub disabled_by_error: Option<ExprErrorCode>,
    pub error_span: Option<SourceSpan>,
    pub fuel_used: u64,
}

/// Delivers the bounded terminal outcome of one operation through a caller-owned sink. No project
/// text, values or expression source reach the sink. `correlation_id` and `revision` must match
/// the `Observe` emitter's.
pub fn report<T>(
    result: &Result<T, MotionError>,
    correlation_id: u64,
    revision: u64,
    cancel: &CancellationToken,
    observe: &mut Observe,
    sink: &mut (impl SinkPort + ?Sized),
) -> Result<Receipt, hsk_studio_observe::Error> {
    let outcome = match result {
        Ok(_) => Outcome::Success,
        Err(MotionError::Canceled) => Outcome::Canceled,
        Err(error) => Outcome::Failure(error.failure_code()),
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

pub const DESCRIPTOR: &str = r#"{"owner":"STUDIO-MODULE-MOTION","version":1,"operation":"Property::sample/evaluate_with, PropertyStore::evaluate_tick, DepGraph::commit_expression/eval_order, DateProfile::utc_ms, to_iso_string/parse_iso_exact","scope":"Pure Rust keyframe/easing evaluation and typed expression, time, seed and dependency policy. No JS engine (MotionJS), no composition/layer plan (Composite), no spatial tangents, roving, time remap, seeded noise, GUI, persistence or host wiring","operator":{"intent":"Evaluate a property at an exact tick deterministically, or reject with a typed code","result":"Value vector plus EvalReceipt{revision,tick,seed,profile,state,disabled_by_error,fuel_used}, or MotionError.code()","recovery":"stale_revision: reload property revision and retry; dependency_cycle: drop one read edge (participants listed); expression disabled_by_error: fix expression then set_expression/set_expression_enabled; never retry an identical rejected input"},"model":{"api":"Property::new(key,static_value,keys); sample(EvalTick,&CancellationToken); evaluate_with(&EvalContext,&dyn ExpressionProvider,&dyn HostRead,&mut Fuel,&CancellationToken); evaluate_pure; PropertyStore::insert/set_reads/update/evaluate_tick(&StoreContext,&dyn ExpressionProvider,&mut Fuel,&CancellationToken); DepGraph; DateProfile; report(result,correlation,revision,cancel,&mut Observe,&mut SinkPort)","consumer":"motion-consumer [--descriptor] --keys \"tick:value:in:out,...\" --tick N  (in/out: linear|bezier|hold; bezier uses default ease speed 0 influence 0.16666666666)","time":"254016000000 ticks/s; key times u64 Ticks that fit i64; effective sample tick EvalTick is signed i64; before first/after last key end values are held","interpolation":"in and out independent; hold out = step; both non-bezier = linear; otherwise cubic Bezier in normalised (time,value) space X=[0,io,1-ii,1], Y=[v0,v0+s0*io*dt_s,v1-s1*ii*dt_s,v1], x(s)=u solved by 64 fixed bisection steps; mixed linear/bezier uses a chord handle (speed=chord slope, influence 1/3); tangents per component, one stored tangent broadcasts; adobe numeric equivalence UNVERIFIED pending oracle","state":"static|keyframed|expression_driven|expression_over_keyframes; static value retained; expression error disables expression and falls back to keyframed/static with disabled_by_error; cancellation and out_of_bounds never yield a partial value","store":"PropertyStore evaluates every property at one tick in dependency order and serves evaluated values as HostRead: reads limited to the declared read set, canonical-tick read returns the evaluated value, other-tick read returns the keyframed/static value and is unsupported for an expression-active target; one shared Fuel; store revision advances on accepted mutations, stale revision rejected; a hard error aborts the tick with no partial result","expression":"ExpressionProvider and HostRead are typed ports owned by the caller (MotionJS); NoProvider resolves pure Motion with unsupported; Fuel is one shared allowance; no wall clock, no side effects","date":"MOT-256: N=epoch_ms*254016000000+(tick-origin_tick)*1000 in i128, reject |N|>8.64e15*254016000000, utc_ms=trunc_toward_zero(N/254016000000); MOT-257 proleptic Gregorian UTC, floor division for negatives, ISO years 0000..9999 else signed six digits, always .sssZ","limits":{"dimension":64,"keyframes":65536,"expression_bytes":65536,"path_bytes":256,"key_bytes":128,"reads_per_expression":4096,"graph_nodes":65536},"cancel":"CancellationToken polled at sample/evaluate entry and after provider return","undo":"Read-only evaluation; Property mutators advance revision and are the caller's undo unit","diagnostics":"report() emits only a closed Success/Failure/Canceled outcome via observe::SinkPort; no values, keys or expression text are emitted"}}"#;
