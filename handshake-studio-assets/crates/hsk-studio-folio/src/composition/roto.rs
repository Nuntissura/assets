//! Time remap, frame blending and assisted-rotoscoping records of a composition layer (lane
//! SMB-AE-ROTO-WIRING slice 1; parity AE-10, AE-18). Data only; composite evaluates them.
//!
//! Spec basis (module 14, v02.218):
//! - [STU-MOT-045] "Enabling it on a footage or composition layer creates a `time_remap` property
//!   whose value AT a composition time is the SOURCE time to sample", "The property is an ordinary
//!   `StudioProperty` and every keyframe operation, interpolation type and expression applies to
//!   it", "Values outside the source's range are clamped to the first or last frame; this is a
//!   clamp, not an error". Stored as [`StudioCompositionLayer::time_remap`], a one-component
//!   [`StudioKeyedValue`] in SOURCE SECONDS (motion's `TimeRemap` unit), keyframe times in
//!   composition ticks. The record keeps out-of-range values as given; the clamp happens at
//!   evaluation (composite, motion `TimeRemap::sample`). [`time_remap_enabled`] builds the two
//!   keys of contract 1 exactly as motion's `TimeRemap::enable` does (key 2 at in point + the
//!   last frame's start, so the remap is the identity; the spec's "at its out point" equals this
//!   when the out point is the last frame's start, Studio reading).
//! - [STU-MOT-007a] "The frame-blending enumeration is normative: `off`, `frame_mix`,
//!   `pixel_motion`": [`StudioFrameBlending`] (absent = `off`).
//! - [STU-CMP-034] item 1: "A stroke is a frame-bound record with a polarity, `foreground` or
//!   `background`, a brush diameter and a path; a stroke is never replaced by a later stroke ...
//!   Strokes, spans, base frames, the engine choice and every parameter value of the table below
//!   are DOCUMENT STATE under the ownership rules of [STU-CMP-002] and [STU-CMP-003]; the
//!   propagated mattes are a cache, never authority, and a reopened document MUST reproduce them
//!   from that state." [`StudioAssistedRoto`] is that state: `strokes` (append-only), `spans`,
//!   `edge_strokes` (item 5 refine-edge strokes), `engine` and [`StudioRotoParameters`] (the 23
//!   rows of the STU-CMP-034 table). Item 6 freeze: [`StudioRotoFreeze`] snapshots the
//!   propagation inputs (stroke count, spans, engine, parameters) so a reopened document
//!   reproduces the frozen segmentation from state; new strokes and propagation-control changes
//!   do not change a frozen result, the matte controls stay live, unfreezing recomputes.
//!
//! Cache contract (not stored here): the per-frame segmentation and refined mattes live in
//! composite's `RotoRun` value (crate `hsk-studio-composite`, `temporal::roto`), keyed by the
//! digest of the propagation inputs it was computed from; a host keeps it in its render cache
//! (outside Folio, outside the document), and a missing or stale cache is recomputed from these
//! records. Nothing in Folio references a cache entry.
//!
//! Units and bounds: frames are SOURCE frame indices of the layer (for a precomposition layer the
//! nested composition's frames); paths are source-frame pixels. Every numeric row of the
//! STU-CMP-034 v02.218 table carries `unknown` bounds and default, except the two chatter controls
//! (percent, hard and soft bounds 0..100, default 0) ([STU-RAS-106]: "MUST NOT invent a value
//! ... MUST NOT clamp"): each is `Option<f64>`, absent = not set, any finite value is stored as
//! given (never clamped, never range-checked here: the record is document state, and the evaluator
//! REFUSES a chatter outside 0..100 with a typed error, never clamps); enums are closed sets; booleans default
//! `false`. The refine controls are the STU-RAS-121 control set ([`StudioEdgeRefinementControls`],
//! the one edge-refinement record; item 5 "MUST NOT define a second edge-refinement record").
//! Folio hard ceilings (not spec bounds): counts below, coordinates and diameters within
//! `MAX_STREAM_ABS_VALUE`.
//!
//! Refusals (typed, V4-01/02): `InvalidField` with the record path, `BudgetExceeded` for counts;
//! integer arithmetic is denied.
#![deny(clippy::arithmetic_side_effects)]
use super::{MAX_STREAM_ABS_VALUE, StudioCompositionKeyframe, StudioInterpolation, StudioKeyedValue, check, validate_keyed};
use crate::values::Ctx;
use crate::{Code, Result, err};
use hsk_studio_accord::TICKS_PER_SECOND;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Strokes (foreground/background plus refine-edge) per layer (a Folio hard ceiling).
pub const MAX_ROTO_STROKES: usize = 4096;
/// Path points per stroke (a Folio hard ceiling).
pub const MAX_ROTO_STROKE_POINTS: usize = 4096;
/// Spans per layer (a Folio hard ceiling).
pub const MAX_ROTO_SPANS: usize = 256;
/// Largest source frame index (a Folio hard ceiling).
pub const MAX_ROTO_FRAME: u64 = u32::MAX as u64;

/// [STU-MOT-007a] frame blending (absent on the wire = `off`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioFrameBlending {
    #[default]
    Off,
    FrameMix,
    PixelMotion,
}
impl StudioFrameBlending {
    pub fn is_off(&self) -> bool {
        *self == Self::Off
    }
}

/// The two keys of enabling time remap (STU-MOT-045 contract 1; module docs): `(in_point, 0 s)`
/// and `(in_point + last_frame_tick, last frame start in seconds)`, linear. One key when the
/// source has a single frame. `None` when the second key time leaves the composition tick range.
pub fn time_remap_enabled(in_point: u64, last_frame_tick: u64) -> Option<StudioKeyedValue> {
    let key = |time: u64, v: f64| StudioCompositionKeyframe {
        time,
        value: vec![v],
        in_interp: StudioInterpolation::Linear,
        out_interp: StudioInterpolation::Linear,
        in_tangent: Vec::new(),
        out_tangent: Vec::new(),
    };
    let mut keyframes = vec![key(in_point, 0.0)];
    if last_frame_tick > 0 {
        let end = in_point.checked_add(last_frame_tick).filter(|t| *t <= super::MAX_COMPOSITION_TICK)?;
        #[allow(clippy::cast_precision_loss)]
        let last = last_frame_tick as f64 / TICKS_PER_SECOND as f64;
        keyframes.push(key(end, last));
    }
    Some(StudioKeyedValue {
        static_value: vec![0.0],
        keyframes,
    })
}

/// A time remap record: one component, finite, within the Folio stream ceiling; keyframe rules of
/// a transform stream. Out-of-range source times are kept (clamped at evaluation).
pub(super) fn validate_time_remap(v: &StudioKeyedValue, layer: &str, cx: &mut Ctx) -> Result<()> {
    cx.tick()?;
    let t = format!("{layer}/time_remap");
    let ok = |x: &[f64]| x.len() == 1 && x.iter().all(|v| v.is_finite() && v.abs() <= MAX_STREAM_ABS_VALUE);
    validate_keyed(&t, 1, &ok, &v.static_value, &v.keyframes, cx)
}

/// STU-CMP-034 `engine`: closed set, default `classic`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioRotoEngine {
    #[default]
    Classic,
    Learned,
}
impl StudioRotoEngine {
    fn is_default(&self) -> bool {
        *self == Self::Classic
    }
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Learned => "learned",
        }
    }
}

/// Stroke polarity (STU-CMP-034 item 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioRotoPolarity {
    Foreground,
    Background,
}

/// One frame-bound stroke: polarity, brush diameter (source pixels) and path (source pixels).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRotoStroke {
    pub frame: u64,
    pub polarity: StudioRotoPolarity,
    pub diameter: f64,
    pub path: Vec<[f64; 2]>,
}

/// A refine-edge stroke (STU-CMP-034 item 5): designates the band in which the refinement runs on
/// its frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRotoEdgeStroke {
    pub frame: u64,
    pub diameter: f64,
    pub path: Vec<[f64; 2]>,
}

fn yes() -> bool {
    true
}
fn is_true(v: &bool) -> bool {
    *v
}
fn is_false(v: &bool) -> bool {
    !*v
}

/// A span (STU-CMP-034 item 1): base frame, `in_frame` (inclusive) to `out_frame` (exclusive) and
/// the propagation directions (both by default).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRotoSpan {
    pub base_frame: u64,
    pub in_frame: u64,
    pub out_frame: u64,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub forward: bool,
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub backward: bool,
}
impl StudioRotoSpan {
    pub fn contains(&self, frame: u64) -> bool {
        self.in_frame <= frame && frame < self.out_frame
    }
}

/// STU-CMP-034 `quality`: closed set, default `standard`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioRotoQuality {
    #[default]
    Standard,
    Best,
}
/// STU-CMP-034 `edge_detection`: closed set, default `balanced`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioRotoEdgeDetection {
    #[default]
    Balanced,
    FavorPredicted,
    FavorCurrent,
}
/// STU-CMP-034 `view_mode`: closed set, default `alpha_boundary`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StudioRotoViewMode {
    Alpha,
    #[default]
    AlphaBoundary,
    AlphaOverlay,
    Composite,
}

/// The STU-RAS-121 refine controls the roto refine stage consumes (item 5): `smooth`, `feather`,
/// `contrast`, `shift_edge`, `decontaminate_colours`, `decontamination_amount`. Numerics `unknown`
/// bounds and default (STU-RAS-121 "MUST ship as `unknown` per [STU-RAS-106] and MUST NOT be
/// clamped"): absent = not set.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioEdgeRefinementControls {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smooth: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feather: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contrast: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift_edge: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub decontaminate_colours: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decontamination_amount: Option<f64>,
}
impl StudioEdgeRefinementControls {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
    fn numerics(&self) -> [Option<f64>; 5] {
        [self.smooth, self.feather, self.contrast, self.shift_edge, self.decontamination_amount]
    }
}

/// The parameter table of STU-CMP-034 (except `engine`, which is [`StudioAssistedRoto::engine`]).
/// Every member is serde default and skipped when default, so an empty table is `{}`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRotoParameters {
    #[serde(default, skip_serializing_if = "is_default_quality")]
    pub quality: StudioRotoQuality,
    /// Pixels; bounds and default `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_radius: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub view_search_region: bool,
    /// Unit, bounds and default `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_threshold: Option<f64>,
    /// Unit, bounds and default `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion_damping: Option<f64>,
    #[serde(default, skip_serializing_if = "is_default_edge")]
    pub edge_detection: StudioRotoEdgeDetection,
    #[serde(default, skip_serializing_if = "is_false")]
    pub use_alternate_colour_estimation: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub invert_foreground_background: bool,
    /// Gates the four matte controls that follow.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fine_tune_matte: bool,
    /// Pixels; bounds and default `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_feather: Option<f64>,
    /// Percent; bounds and default `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_contrast: Option<f64>,
    /// Percent, signed; bounds and default `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_shift_edge: Option<f64>,
    /// Percent, hard and soft bounds 0..100 (STU-CMP-034 v02.218); outside it the evaluator refuses, never clamps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matte_reduce_chatter: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub render_refine_edge: bool,
    /// Pixels; bounds and default `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_edge_base_radius: Option<f64>,
    /// Gates the STU-RAS-121 refine controls and the temporal controls that follow.
    #[serde(default, skip_serializing_if = "is_false")]
    pub fine_tune_refine_edge_matte: bool,
    /// The STU-RAS-121 controls (item 5).
    #[serde(default, skip_serializing_if = "StudioEdgeRefinementControls::is_default")]
    pub refine: StudioEdgeRefinementControls,
    /// Percent, hard and soft bounds 0..100 (STU-CMP-034 v02.218); 0 uses the current frame only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine_reduce_chatter: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub use_motion_blur: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub extend_where_smoothed: bool,
    /// Pixels; bounds and default `unknown`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decontamination_increase_radius: Option<f64>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub view_decontamination_map: bool,
    #[serde(default, skip_serializing_if = "is_default_view")]
    pub view_mode: StudioRotoViewMode,
}
fn is_default_quality(v: &StudioRotoQuality) -> bool {
    *v == StudioRotoQuality::Standard
}
fn is_default_edge(v: &StudioRotoEdgeDetection) -> bool {
    *v == StudioRotoEdgeDetection::Balanced
}
fn is_default_view(v: &StudioRotoViewMode) -> bool {
    *v == StudioRotoViewMode::AlphaBoundary
}
impl StudioRotoParameters {
    /// Every `unknown`-bounded numeric of the table with its wire name.
    pub fn numerics(&self) -> Vec<(&'static str, Option<f64>)> {
        let r = self.refine.numerics();
        vec![
            ("search_radius", self.search_radius),
            ("motion_threshold", self.motion_threshold),
            ("motion_damping", self.motion_damping),
            ("matte_feather", self.matte_feather),
            ("matte_contrast", self.matte_contrast),
            ("matte_shift_edge", self.matte_shift_edge),
            ("matte_reduce_chatter", self.matte_reduce_chatter),
            ("refine_edge_base_radius", self.refine_edge_base_radius),
            ("refine/smooth", r[0]),
            ("refine/feather", r[1]),
            ("refine/contrast", r[2]),
            ("refine/shift_edge", r[3]),
            ("refine/decontamination_amount", r[4]),
            ("refine_reduce_chatter", self.refine_reduce_chatter),
            ("decontamination_increase_radius", self.decontamination_increase_radius),
        ]
    }
}

/// Freeze state (STU-CMP-034 item 6): the propagation inputs the frozen segmentation was computed
/// from. `stroke_count` = the strokes in force (strokes are append-only, so the first
/// `stroke_count` of [`StudioAssistedRoto::strokes`] are exactly the frozen set).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRotoFreeze {
    pub stroke_count: u32,
    pub spans: Vec<StudioRotoSpan>,
    #[serde(default, skip_serializing_if = "StudioRotoEngine::is_default")]
    pub engine: StudioRotoEngine,
    #[serde(default, skip_serializing_if = "is_default_params")]
    pub parameters: StudioRotoParameters,
}
fn is_default_params(p: &StudioRotoParameters) -> bool {
    *p == StudioRotoParameters::default()
}

/// The assisted-rotoscoping document state of one layer (module docs).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioAssistedRoto {
    #[serde(default, skip_serializing_if = "StudioRotoEngine::is_default")]
    pub engine: StudioRotoEngine,
    /// Append-only, drawing order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strokes: Vec<StudioRotoStroke>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edge_strokes: Vec<StudioRotoEdgeStroke>,
    /// Ordered by `in_frame`, never overlapping.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<StudioRotoSpan>,
    #[serde(default, skip_serializing_if = "is_default_params")]
    pub parameters: StudioRotoParameters,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeze: Option<StudioRotoFreeze>,
}

/// What [`StudioAssistedRoto::add_stroke`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StudioRotoStrokeAdded {
    /// The stroke joined the span with this index.
    Span(usize),
    /// The stroke's frame lay in no span: it became a base frame and opened this span.
    Opened(usize),
}

impl StudioAssistedRoto {
    /// The span holding `frame`.
    pub fn span_of(&self, frame: u64) -> Option<(usize, &StudioRotoSpan)> {
        self.spans.iter().enumerate().find(|(_, s)| s.contains(frame))
    }

    /// Appends a stroke (STU-CMP-034 items 1-2: never replaces one). A frame in no span becomes a
    /// base frame and opens a span covering the free run of `0..source_frames` around it (the
    /// layer duration not already covered by another span). Refuses (`InvalidField`) a frame at or
    /// beyond `source_frames`; the record is unchanged on refusal.
    pub fn add_stroke(&mut self, stroke: StudioRotoStroke, source_frames: u64) -> Result<StudioRotoStrokeAdded> {
        let frame = stroke.frame;
        check(frame < source_frames && frame <= MAX_ROTO_FRAME, "roto", "strokes/frame")?;
        if let Some((i, _)) = self.span_of(frame) {
            self.strokes.push(stroke);
            return Ok(StudioRotoStrokeAdded::Span(i));
        }
        let lo = self.spans.iter().filter(|s| s.out_frame <= frame).map(|s| s.out_frame).max().unwrap_or(0);
        let hi = self.spans.iter().filter(|s| s.in_frame > frame).map(|s| s.in_frame).min().unwrap_or(source_frames);
        let span = StudioRotoSpan {
            base_frame: frame,
            in_frame: lo,
            out_frame: hi,
            forward: true,
            backward: true,
        };
        let at = self.spans.iter().position(|s| s.in_frame > frame).unwrap_or(self.spans.len());
        self.spans.insert(at, span);
        self.strokes.push(stroke);
        Ok(StudioRotoStrokeAdded::Opened(at))
    }

    /// Removes span `index` and every stroke and refine-edge stroke on its frames. Out of range:
    /// `InvalidField`.
    pub fn remove_span(&mut self, index: usize) -> Result<()> {
        if index >= self.spans.len() {
            return Err(err(Code::InvalidField, "roto/spans"));
        }
        let s = self.spans.remove(index);
        self.strokes.retain(|k| !s.contains(k.frame));
        self.edge_strokes.retain(|k| !s.contains(k.frame));
        Ok(())
    }

    /// Freezes: snapshots the propagation inputs (item 6). Already frozen: unchanged.
    pub fn freeze(&mut self) -> Result<()> {
        if self.freeze.is_some() {
            return Ok(());
        }
        let stroke_count = u32::try_from(self.strokes.len()).map_err(|_| err(Code::BudgetExceeded, "roto/strokes"))?;
        self.freeze = Some(StudioRotoFreeze {
            stroke_count,
            spans: self.spans.clone(),
            engine: self.engine,
            parameters: self.parameters.clone(),
        });
        Ok(())
    }

    /// Unfreezes (the next evaluation recomputes from the current state).
    pub fn unfreeze(&mut self) {
        self.freeze = None;
    }
}

fn point_ok(p: &[f64; 2]) -> bool {
    p.iter().all(|v| v.is_finite() && v.abs() <= MAX_STREAM_ABS_VALUE)
}
fn diameter_ok(d: f64) -> bool {
    d.is_finite() && d > 0.0 && d <= MAX_STREAM_ABS_VALUE
}

fn validate_path(path: &[[f64; 2]], t: &str, cx: &mut Ctx) -> Result<()> {
    if path.len() > MAX_ROTO_STROKE_POINTS {
        return Err(err(Code::BudgetExceeded, format!("{t}/path")));
    }
    check(!path.is_empty() && path.iter().all(point_ok), t, "path")?;
    cx.nodes(path.len(), t)
}

fn validate_spans(spans: &[StudioRotoSpan], t: &str, cx: &mut Ctx) -> Result<()> {
    if spans.len() > MAX_ROTO_SPANS {
        return Err(err(Code::BudgetExceeded, format!("{t}/spans")));
    }
    cx.nodes(spans.len(), t)?;
    let mut previous_out: Option<u64> = None;
    for s in spans {
        cx.tick()?;
        let shape = s.in_frame < s.out_frame
            && s.out_frame <= MAX_ROTO_FRAME.saturating_add(1)
            && s.in_frame <= s.base_frame
            && s.base_frame < s.out_frame
            && (s.forward || s.backward);
        check(shape, t, "spans")?;
        check(previous_out.is_none_or(|p| p <= s.in_frame), t, "spans/overlap")?;
        previous_out = Some(s.out_frame);
    }
    Ok(())
}

fn validate_parameters(p: &StudioRotoParameters, t: &str) -> Result<()> {
    for (name, v) in p.numerics() {
        // `unknown` bounds: any finite value is stored as given (STU-RAS-106); the wire cannot
        // carry a non-finite number, a programmatic one is refused.
        check(v.is_none_or(f64::is_finite), t, name)?;
    }
    Ok(())
}

/// The roto record of one layer (module docs).
pub(super) fn validate_roto(r: &StudioAssistedRoto, layer: &str, cx: &mut Ctx) -> Result<()> {
    cx.tick()?;
    let t = format!("{layer}/roto");
    if r.strokes.len().saturating_add(r.edge_strokes.len()) > MAX_ROTO_STROKES {
        return Err(err(Code::BudgetExceeded, format!("{t}/strokes")));
    }
    cx.nodes(r.strokes.len().saturating_add(r.edge_strokes.len()), &t)?;
    validate_spans(&r.spans, &t, cx)?;
    let in_span = |spans: &[StudioRotoSpan], f: u64| spans.iter().any(|s| s.contains(f));
    for s in &r.strokes {
        cx.tick()?;
        check(s.frame <= MAX_ROTO_FRAME && diameter_ok(s.diameter), &t, "strokes")?;
        validate_path(&s.path, &format!("{t}/strokes"), cx)?;
        check(in_span(&r.spans, s.frame), &t, "strokes/span")?;
    }
    for s in &r.edge_strokes {
        cx.tick()?;
        check(s.frame <= MAX_ROTO_FRAME && diameter_ok(s.diameter), &t, "edge_strokes")?;
        validate_path(&s.path, &format!("{t}/edge_strokes"), cx)?;
        check(in_span(&r.spans, s.frame), &t, "edge_strokes/span")?;
    }
    for s in &r.spans {
        check(r.strokes.iter().any(|k| k.frame == s.base_frame), &t, "spans/base_frame")?;
    }
    validate_parameters(&r.parameters, &format!("{t}/parameters"))?;
    if let Some(f) = &r.freeze {
        let tf = format!("{t}/freeze");
        let n = usize::try_from(f.stroke_count).map_err(|_| err(Code::InvalidField, format!("{tf}/stroke_count")))?;
        check(n <= r.strokes.len(), &tf, "stroke_count")?;
        validate_spans(&f.spans, &tf, cx)?;
        let frozen = r.strokes.get(..n).unwrap_or_default();
        for s in &f.spans {
            check(frozen.iter().any(|k| k.frame == s.base_frame), &tf, "spans/base_frame")?;
        }
        validate_parameters(&f.parameters, &format!("{tf}/parameters"))?;
    }
    Ok(())
}

/// The propagation inputs in force (STU-CMP-034 items 1, 6): the freeze snapshot when frozen, else
/// the live state. Matte and refine controls are not propagation inputs (they stay live while
/// frozen).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StudioRotoPropagationInputs<'a> {
    pub frozen: bool,
    pub engine: StudioRotoEngine,
    pub strokes: &'a [StudioRotoStroke],
    pub spans: &'a [StudioRotoSpan],
    pub parameters: &'a StudioRotoParameters,
}

#[derive(Serialize)]
struct PropagationKey<'a> {
    engine: StudioRotoEngine,
    strokes: &'a [StudioRotoStroke],
    spans: &'a [StudioRotoSpan],
    quality: StudioRotoQuality,
    search_radius: Option<f64>,
    motion_threshold: Option<f64>,
    motion_damping: Option<f64>,
    edge_detection: StudioRotoEdgeDetection,
    use_alternate_colour_estimation: bool,
    invert_foreground_background: bool,
}

impl StudioRotoPropagationInputs<'_> {
    /// Canonical bytes of the inputs (JSON of the engine, strokes, spans and the propagation
    /// controls, in a fixed field order): the cache key a host hashes together with the source
    /// frames. Empty on a serialisation failure (cannot happen for validated records).
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let p = self.parameters;
        serde_json::to_vec(&PropagationKey {
            engine: self.engine,
            strokes: self.strokes,
            spans: self.spans,
            quality: p.quality,
            search_radius: p.search_radius,
            motion_threshold: p.motion_threshold,
            motion_damping: p.motion_damping,
            edge_detection: p.edge_detection,
            use_alternate_colour_estimation: p.use_alternate_colour_estimation,
            invert_foreground_background: p.invert_foreground_background,
        })
        .unwrap_or_default()
    }
}

impl StudioAssistedRoto {
    /// The propagation inputs in force (frozen snapshot or live state).
    pub fn propagation_inputs(&self) -> StudioRotoPropagationInputs<'_> {
        match &self.freeze {
            Some(f) => StudioRotoPropagationInputs {
                frozen: true,
                engine: f.engine,
                strokes: usize::try_from(f.stroke_count)
                    .ok()
                    .and_then(|n| self.strokes.get(..n))
                    .unwrap_or_default(),
                spans: &f.spans,
                parameters: &f.parameters,
            },
            None => StudioRotoPropagationInputs {
                frozen: false,
                engine: self.engine,
                strokes: &self.strokes,
                spans: &self.spans,
                parameters: &self.parameters,
            },
        }
    }
}
