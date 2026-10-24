//! Info panel (lane SMB-PS-INFO-HISTOGRAM-1; STU-RAS-049, STU-RAS-163; panel registry row `info`):
//! WORKSPACE > Panels > Info (Ctrl+Shift+F1). A live histogram of the displayed composite (or of the
//! active selection) per channel and luminosity with clip counts, a readout of the pixel under the
//! pointer (RGBA and one selectable colour model), its position, the document dimensions and the
//! selection bounds; persistent colour samplers; and an eyedropper sampling into the active swatch
//! (the brush colour) with a sample size and a sample scope. The numbers are Pigment's
//! (`hsk_studio_pigment::diagnostics`); every one of them is in `state_json()["info"]`.
//!
//! One implementation, two projections: the panel's controls and canvas clicks call
//! [`Viewer::info_command`] / [`Viewer::info_eyedropper`], the typed model surface. Sampler points,
//! the readout model, the sample size and the sample scope are the session document entry `info`
//! (JSON [`SCHEMA`], validated on every write and every read by [`InfoRecords::parse`]); every change
//! is ONE undo step (`Run::set_tiles_and_entry`, entry only) and save / reopen keep it.
//!
//! Studio readings, declared (Photoshop behaviour UNVERIFIED, D5; lane notes `spec_deltas`):
//! - the histogram, pointer readout and samplers read the canvas composite as rendered (8-bit sRGB,
//!   straight alpha, `Frame::rgba8`); on the GPU shared-device display path (no CPU buffer) they read
//!   the render-cpu read-back frame (as Export does) and the state says `read_back`;
//! - the histogram covers the active selection when there is one (pixels with coverage above 0),
//!   else the whole frame; it is computed when the frame or the selection changes and cached;
//! - the pointer readout and the samplers use the entry's sample size; the samplers read the
//!   composite; the eyedropper reads the composite of its scope: `all_layers` the canvas composite,
//!   `current_layer` the selected layer (with its group ancestors) alone, `current_and_below` the
//!   selected layer and every layer below it in the stack, each rendered by render-cpu over a copy of
//!   the document whose other layers are hidden (the document is not changed);
//! - an eyedropper sample with alpha 0 does not change the swatch (`eyedropper_transparent`);
//! - at most [`MAX_SAMPLERS`] samplers (the spec floor is four; Photoshop's limit UNVERIFIED);
//! - the CMYK readout is the unmanaged device formula (no CMYK profile; a profiled readout is a gap).
//!
//! Key binding: the spec declares none for `info`; Ctrl+Shift+F1 is a Studio choice (the
//! Ctrl+Shift+F-key family of the hosted panels; plain F1 stays free).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::render::{Frame, Prefer};
use super::widget as vw;
use super::wrap::{wrap_item, wrap_measured};
use hsk_studio_folio::{self as folio, Inspection, Parent, TileRef};
use hsk_studio_pigment::diagnostics::{self as diag, ColourModel, Histogram, HistogramChannel, Rgba8Plane, SampleSize};
use hsk_studio_pigment::selection::CoveragePlane;
use serde_json::{Value, json};

/// Session document entry token of the info records.
pub const ENTRY: &str = "info";
/// Schema id of the entry JSON.
pub const SCHEMA: &str = "hsk.studio.harness.info@1";
/// Most samplers a document holds.
pub const MAX_SAMPLERS: usize = 10;
/// Largest entry accepted (bytes).
pub const MAX_ENTRY_BYTES: usize = 64 * 1024;
/// Largest absolute sampler coordinate (document pixels).
pub const MAX_COORD: f64 = 2_147_483_647.0;
/// Histogram bins per channel (one per 8-bit value).
pub const BINS: usize = 256;

/// Stable widget names of the Info panel.
pub mod widget {
    pub const PANEL: &str = "info";
    pub const MODEL: &str = "info.model";
    pub const SAMPLE_SIZE: &str = "info.sample_size";
    pub const SCOPE: &str = "info.scope";
    pub const CHANNEL: &str = "info.channel";
    pub const HISTOGRAM: &str = "info.histogram";
    pub const PICK_SAMPLER: &str = "info.pick.sampler";
    pub const PICK_EYEDROPPER: &str = "info.pick.eyedropper";
    /// `info.sampler.<i>.remove`.
    pub fn sampler_remove(i: usize) -> String {
        format!("info.sampler.{i}.remove")
    }
}

/// The eyedropper's sample scope (STU-RAS-163).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SampleScope {
    CurrentLayer,
    CurrentAndBelow,
    #[default]
    AllLayers,
}

impl SampleScope {
    pub const ALL: [SampleScope; 3] = [Self::CurrentLayer, Self::CurrentAndBelow, Self::AllLayers];
    pub const fn code(self) -> &'static str {
        match self {
            Self::CurrentLayer => "current_layer",
            Self::CurrentAndBelow => "current_and_below",
            Self::AllLayers => "all_layers",
        }
    }
    pub const fn label(self) -> &'static str {
        match self {
            Self::CurrentLayer => "Current layer",
            Self::CurrentAndBelow => "Current and below",
            Self::AllLayers => "All layers",
        }
    }
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.code() == code)
    }
}

/// What a canvas click does while the Info panel picks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pick {
    #[default]
    None,
    /// A click adds a colour sampler.
    Sampler,
    /// A click samples into the active swatch.
    Eyedropper,
}

impl Pick {
    pub const fn code(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Sampler => "sampler",
            Self::Eyedropper => "eyedropper",
        }
    }
}

/// One persistent colour sampler: a document-space position and its readout model.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sampler {
    pub x: f64,
    pub y: f64,
    pub model: ColourModel,
}

/// The document-scoped info state (the entry `info`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct InfoRecords {
    pub model: ColourModel,
    pub sample_size: SampleSize,
    pub scope: SampleScope,
    pub samplers: Vec<Sampler>,
}

fn coord(v: f64, what: &str) -> Result<f64, String> {
    if v.is_finite() && v.abs() <= MAX_COORD { Ok(v) } else { Err(format!("info_coordinate_out_of_range: {what}")) }
}

impl InfoRecords {
    /// Checks the sampler count and every coordinate.
    pub fn validate(&self) -> Result<(), String> {
        if self.samplers.len() > MAX_SAMPLERS {
            return Err(format!("info_too_many_samplers: {} > {MAX_SAMPLERS}", self.samplers.len()));
        }
        for s in &self.samplers {
            coord(s.x, "x")?;
            coord(s.y, "y")?;
        }
        Ok(())
    }

    /// The entry JSON.
    pub fn to_json(&self) -> Value {
        json!({
            "schema": SCHEMA,
            "model": self.model.code(),
            "sample_size": self.sample_size.code(),
            "scope": self.scope.code(),
            "samplers": self.samplers.iter().map(|s| json!({"x": s.x, "y": s.y, "model": s.model.code()})).collect::<Vec<_>>(),
        })
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.to_json().to_string().into_bytes()
    }

    /// The hostile decoder of the entry: size cap before parsing, exact keys, known codes, finite
    /// bounded coordinates, at most [`MAX_SAMPLERS`] samplers (checked before they are built).
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_ENTRY_BYTES {
            return Err(format!("info_entry_too_large: {} > {MAX_ENTRY_BYTES}", bytes.len()));
        }
        let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("info_entry_json: {e}"))?;
        let o = v.as_object().ok_or("info_entry_not_an_object")?;
        let keys = ["schema", "model", "sample_size", "scope", "samplers"];
        if let Some(k) = o.keys().find(|k| !keys.contains(&k.as_str())) {
            return Err(format!("info_entry_unknown_key: {k}"));
        }
        let text = |k: &str| o.get(k).and_then(Value::as_str).ok_or_else(|| format!("info_entry_missing: {k}"));
        if text("schema")? != SCHEMA {
            return Err("info_entry_schema".into());
        }
        let model = ColourModel::from_code(text("model")?).ok_or("info_entry_model")?;
        let sample_size = SampleSize::from_code(text("sample_size")?).ok_or("info_entry_sample_size")?;
        let scope = SampleScope::from_code(text("scope")?).ok_or("info_entry_scope")?;
        let list = o.get("samplers").and_then(Value::as_array).ok_or("info_entry_missing: samplers")?;
        if list.len() > MAX_SAMPLERS {
            return Err(format!("info_too_many_samplers: {} > {MAX_SAMPLERS}", list.len()));
        }
        let mut samplers = Vec::with_capacity(list.len());
        for s in list {
            let so = s.as_object().ok_or("info_entry_sampler_not_an_object")?;
            if so.len() != 3 {
                return Err("info_entry_sampler_keys".into());
            }
            let num = |k: &str| so.get(k).and_then(Value::as_f64).ok_or_else(|| format!("info_entry_sampler_missing: {k}"));
            let model = so.get("model").and_then(Value::as_str).and_then(ColourModel::from_code).ok_or("info_entry_sampler_model")?;
            samplers.push(Sampler { x: coord(num("x")?, "x")?, y: coord(num("y")?, "y")?, model });
        }
        let r = Self { model, sample_size, scope, samplers };
        r.validate()?;
        Ok(r)
    }
}

/// A typed info command (the model surface; the panel and canvas clicks call the same).
#[derive(Clone, Debug, PartialEq)]
pub enum InfoCommand {
    AddSampler { x: f64, y: f64 },
    MoveSampler { index: usize, x: f64, y: f64 },
    RemoveSampler(usize),
    SetSamplerModel { index: usize, model: ColourModel },
    SetSampleSize(SampleSize),
    SetSampleScope(SampleScope),
    SetModel(ColourModel),
}

impl InfoCommand {
    fn op(&self) -> &'static str {
        match self {
            Self::AddSampler { .. } => "info_add_sampler",
            Self::MoveSampler { .. } => "info_move_sampler",
            Self::RemoveSampler(_) => "info_remove_sampler",
            Self::SetSamplerModel { .. } => "info_set_sampler_model",
            Self::SetSampleSize(_) => "info_set_sample_size",
            Self::SetSampleScope(_) => "info_set_sample_scope",
            Self::SetModel(_) => "info_set_model",
        }
    }
}

/// An owned 8-bit RGBA plane (a read-back or scope render).
#[derive(Clone, Debug, PartialEq)]
struct PlaneBuf {
    x: i64,
    y: i64,
    width: u32,
    height: u32,
    rgba8: Vec<u8>,
}

impl PlaneBuf {
    fn of(f: Frame) -> Self {
        Self { x: f.x, y: f.y, width: f.width, height: f.height, rgba8: f.rgba8 }
    }
}

/// The pixels the panel reads: the frame's own bytes (no copy), the cached read-back, or a fresh
/// render.
enum Plane<'a> {
    Frame(&'a Frame),
    Cached(&'a PlaneBuf),
    Owned(PlaneBuf),
}

impl Plane<'_> {
    fn plane(&self) -> Rgba8Plane<'_> {
        match self {
            Self::Frame(f) => Rgba8Plane { x: f.x, y: f.y, width: f.width, height: f.height, data: &f.rgba8 },
            Self::Cached(p) => Rgba8Plane { x: p.x, y: p.y, width: p.width, height: p.height, data: &p.rgba8 },
            Self::Owned(p) => Rgba8Plane { x: p.x, y: p.y, width: p.width, height: p.height, data: &p.rgba8 },
        }
    }
}

/// Histogram cache key: frame generation and the selection's bounds and coverage sum.
type Key = (u64, Option<(i64, i64, u32, u32, u64)>);

/// Viewer-held state of the Info panel (the document-scoped part is the entry).
#[derive(Default)]
pub struct InfoState {
    pub(super) open: bool,
    pick: Pick,
    channel: Option<HistogramChannel>,
    /// The document position under the pointer (set by the canvas each frame).
    pub(super) hover: Option<[f64; 2]>,
    /// The canvas mapping of the last frame: shown rect min, zoom, frame origin.
    pub(super) view: Option<([f32; 2], f32, [f32; 2])>,
    frame_gen: u64,
    cache: Option<(Key, Result<Histogram, String>)>,
    read_back: Option<(u64, PlaneBuf)>,
    eyedropper: Option<Value>,
}

impl InfoState {
    pub(super) fn frame_changed(&mut self) {
        self.frame_gen = self.frame_gen.wrapping_add(1);
    }
}

struct ScopeResolver;
impl folio::Resolver for ScopeResolver {
    fn primitive(&self, _: &str, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn profile(&self, _: &str) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn tile(&self, _: &TileRef) -> folio::Resolution {
        folio::Resolution::Available
    }
    fn format_supported(&self, _: &str) -> bool {
        true
    }
    fn primitive_layer_binding(&self, kind: &str, schema_id: &str) -> bool {
        kind == "adjustment" && schema_id == folio::ADJUSTMENT_SCHEMA
    }
}

fn selection_key(c: Option<&CoveragePlane>) -> Option<(i64, i64, u32, u32, u64)> {
    c.map(|c| (c.bounds.x, c.bounds.y, c.bounds.width, c.bounds.height, c.data.iter().map(|v| u64::from(*v)).fold(0u64, u64::saturating_add)))
}

/// Clip percentages are of the channel's binned samples (colour channels skip transparent pixels).
fn channel_json(h: &diag::ChannelHistogram) -> Value {
    let pct = |n: u64| if h.count == 0 { 0.0 } else { n as f64 * 100.0 / h.count as f64 };
    json!({
        "bins": h.bins, "count": h.count, "clip_low": h.clip_low, "clip_high": h.clip_high,
        "clip_low_pct": pct(h.clip_low), "clip_high_pct": pct(h.clip_high),
        "mean": h.mean, "std_dev": h.std_dev, "median": h.median,
    })
}

/// `state_json()["info"]["histogram"]` of a histogram.
pub fn histogram_json(h: &Histogram) -> Value {
    let channels: serde_json::Map<String, Value> = h.channels.iter().map(|c| (c.channel.code().to_owned(), channel_json(c))).collect();
    json!({"bins": h.bin_count, "pixels": h.pixels, "transparent": h.transparent, "channels": channels, "luminosity_weights_per_mille": diag::LUMINOSITY_WEIGHTS})
}

fn readout_json(rgb: [f64; 3], model: ColourModel) -> Value {
    match diag::readout(rgb, model) {
        Ok(v) => json!({"model": model.code(), "components": model.components(), "values": v}),
        Err(e) => json!({"model": model.code(), "error": e.code()}),
    }
}

fn sample_json(s: Option<diag::Sample>, model: ColourModel) -> Value {
    match s {
        Some(s) => json!({"in_bounds": true, "rgba8": s.rgba8, "rgb": s.rgb, "alpha": s.alpha, "pixels": s.pixels, "readout": readout_json(s.rgb, model)}),
        None => json!({"in_bounds": false}),
    }
}

/// The document pixel of a document position.
fn pixel_of(x: f64, y: f64) -> (i64, i64) {
    (x.floor() as i64, y.floor() as i64)
}

impl Viewer {
    // ----- records ---------------------------------------------------------------------------

    /// The info records of the open document (defaults when none are stored; `Err` when the stored
    /// entry is refused, which leaves it stored unchanged).
    pub fn info_records(&self) -> Result<InfoRecords, String> {
        match self.run.document_entry(ENTRY) {
            Some(bytes) => InfoRecords::parse(bytes),
            None => Ok(InfoRecords::default()),
        }
    }

    /// Runs one typed info command: the new records are validated and written to the entry `info`
    /// as ONE undo step. Refusals change nothing and are said in the status log.
    pub fn info_command(&mut self, command: InfoCommand) -> Result<(), String> {
        let op = command.op();
        let result = self.info_command_inner(command);
        match &result {
            Ok(()) => self.say(format!("{op}: info state written")),
            Err(e) => self.say(format!("{op}: {e}")),
        }
        result
    }

    fn info_command_inner(&mut self, command: InfoCommand) -> Result<(), String> {
        let snap = self.run.snapshot().ok_or("nothing open")?;
        let document_id = snap.document().document_id.clone();
        let mut r = self.info_records().map_err(|e| format!("stored info entry refused: {e}"))?;
        let at = |r: &InfoRecords, i: usize| if i < r.samplers.len() { Ok(i) } else { Err(format!("no sampler {i} (there are {})", r.samplers.len())) };
        match command.clone() {
            InfoCommand::AddSampler { x, y } => {
                if r.samplers.len() >= MAX_SAMPLERS {
                    return Err(format!("info_too_many_samplers: at most {MAX_SAMPLERS}"));
                }
                let model = r.model;
                r.samplers.push(Sampler { x: coord(x, "x")?, y: coord(y, "y")?, model });
            }
            InfoCommand::MoveSampler { index, x, y } => {
                let i = at(&r, index)?;
                r.samplers[i].x = coord(x, "x")?;
                r.samplers[i].y = coord(y, "y")?;
            }
            InfoCommand::RemoveSampler(index) => {
                let i = at(&r, index)?;
                r.samplers.remove(i);
            }
            InfoCommand::SetSamplerModel { index, model } => {
                let i = at(&r, index)?;
                r.samplers[i].model = model;
            }
            InfoCommand::SetSampleSize(s) => r.sample_size = s,
            InfoCommand::SetSampleScope(s) => r.scope = s,
            InfoCommand::SetModel(m) => r.model = m,
        }
        r.validate()?;
        let bytes = r.to_bytes();
        // The bytes written must read back as the same records (validated on set and on read).
        if InfoRecords::parse(&bytes)? != r {
            return Err("info_entry_round_trip".into());
        }
        if self.run.set_tiles_and_entry(command.op(), &document_id, None, ENTRY, bytes) { Ok(()) } else { Err(self.op_error()) }
    }

    /// Sets what a canvas click does (panel buttons; not document state).
    pub fn info_set_pick(&mut self, pick: Pick) {
        self.info.pick = pick;
    }

    /// The histogram channel the panel draws (not document state).
    pub fn info_set_channel(&mut self, channel: HistogramChannel) {
        self.info.channel = Some(channel);
    }

    pub(super) fn info_pick_active(&self) -> bool {
        self.info.open && self.shell.visible(super::shell::Panel::Info) && self.info.pick != Pick::None
    }

    /// A canvas click at document position `(x, y)` while picking.
    pub(super) fn info_canvas_click(&mut self, x: f64, y: f64) {
        let _ = match self.info.pick {
            Pick::Sampler => self.info_command(InfoCommand::AddSampler { x, y }),
            Pick::Eyedropper => self.info_eyedropper(x, y).map(|_| ()),
            Pick::None => Ok(()),
        };
    }

    pub(super) fn info_close_document(&mut self) {
        let open = self.info.open;
        self.info = InfoState { open, channel: self.info.channel, ..InfoState::default() };
    }

    // ----- pixels ------------------------------------------------------------------------------

    /// The composite the panel reads: the frame's bytes, else the cached or a fresh render-cpu
    /// read-back (`true`).
    fn info_composite(&self) -> Result<Option<(Plane<'_>, bool)>, String> {
        let Some(frame) = &self.frame else { return Ok(None) };
        if !frame.rgba8.is_empty() {
            return Ok(Some((Plane::Frame(frame), false)));
        }
        if let Some((g, p)) = &self.info.read_back
            && *g == self.info.frame_gen
        {
            return Ok(Some((Plane::Cached(p), true)));
        }
        let snap = self.run.snapshot().ok_or("nothing open")?;
        Ok(Some((Plane::Owned(self.info_render(snap)?), true)))
    }

    fn info_render(&self, snap: &folio::Snapshot) -> Result<PlaneBuf, String> {
        let text = self.prepared_text()?;
        let vectors = self.prepared_vectors()?;
        let f = super::render::render_full_with_records(snap, &self.assets, Some(&text), vectors.as_ref(), self.records.as_ref(), Prefer::Cpu, None, None, self.cancel)
            .map_err(|e| format!("info read-back: {e}"))?;
        Ok(PlaneBuf::of(f))
    }

    /// The layers a scope keeps visible (`None`: all, the canvas composite).
    fn info_scope_layers(&self, scope: SampleScope) -> Result<Option<Vec<String>>, String> {
        if scope == SampleScope::AllLayers {
            return Ok(None);
        }
        let selected = self.selected.clone().ok_or_else(|| format!("eyedropper scope {}: select a layer first", scope.code()))?;
        let snap = self.run.snapshot().ok_or("nothing open")?;
        let doc = snap.document();
        let rows = self.layers();
        let at = rows.iter().position(|r| r.layer_id == selected).ok_or("the selected layer is gone")?;
        // The selected layer's group ancestors stay visible (a hidden group hides its children).
        let mut keep: Vec<String> = Vec::new();
        let mut cursor = selected.clone();
        while let Some(l) = doc.layers.iter().find(|l| l.layer_id == cursor) {
            keep.push(l.layer_id.clone());
            match &l.parent {
                Parent::Node(n) if !keep.contains(&n.node_id) => cursor = n.node_id.clone(),
                _ => break,
            }
        }
        let depth = rows[at].depth;
        let below = rows.iter().skip(at.saturating_add(1));
        match scope {
            // The layer and its descendants (the rows after it that are deeper).
            SampleScope::CurrentLayer => keep.extend(below.take_while(|r| r.depth > depth).map(|r| r.layer_id.clone())),
            // Everything after it in the top-first stack.
            SampleScope::CurrentAndBelow => keep.extend(below.map(|r| r.layer_id.clone())),
            SampleScope::AllLayers => {}
        }
        Ok(Some(keep))
    }

    /// The composite of `scope` (module docs): the canvas composite, or a render-cpu render of a
    /// document copy whose layers outside the scope are hidden.
    fn info_scope_plane(&self, scope: SampleScope) -> Result<Plane<'_>, String> {
        let Some(keep) = self.info_scope_layers(scope)? else {
            return self.info_composite()?.map(|(p, _)| p).ok_or_else(|| "no rendered frame".into());
        };
        let snap = self.run.snapshot().ok_or("nothing open")?;
        let mut wire: Value = serde_json::from_slice(snap.encoded_bytes()).map_err(|e| format!("info scope document: {e}"))?;
        if let Some(layers) = wire["layers"].as_array_mut() {
            for l in layers.iter_mut() {
                let id = l["layer_id"].as_str().unwrap_or_default().to_owned();
                if !keep.contains(&id) {
                    l["visible"] = json!(false);
                }
            }
        }
        let bytes = serde_json::to_vec(&wire).map_err(|e| format!("info scope document: {e}"))?;
        let copy = match folio::inspect_bytes(&bytes, folio::Budget::default(), self.cancel, &ScopeResolver) {
            Ok(Inspection::Editable(s)) => s,
            Ok(Inspection::ReadOnly(r)) => return Err(format!("info scope document read-only: {:?}", r.diagnostic())),
            Err(d) => return Err(format!("info scope document rejected: {d:?}")),
        };
        self.info_render(&copy).map(Plane::Owned)
    }

    /// Samples the document position `(x, y)` with the entry's size and scope into the active swatch
    /// (the brush colour). Returns the sample (also `state_json()["info"]["eyedropper"]`). A sample
    /// outside the canvas or with alpha 0 leaves the swatch unchanged and is refused.
    pub fn info_eyedropper(&mut self, x: f64, y: f64) -> Result<Value, String> {
        let result = self.info_eyedropper_inner(x, y);
        match &result {
            Ok(v) => {
                self.say(format!("eyedropper: {} at ({x}, {y})", v["rgba8"]));
                self.info.eyedropper = Some(v.clone());
            }
            Err(e) => {
                self.say(format!("eyedropper: {e}"));
                self.info.eyedropper = Some(json!({"error": e, "x": x, "y": y}));
            }
        }
        result
    }

    fn info_eyedropper_inner(&mut self, x: f64, y: f64) -> Result<Value, String> {
        let r = self.info_records().map_err(|e| format!("stored info entry refused: {e}"))?;
        coord(x, "x")?;
        coord(y, "y")?;
        let plane = self.info_scope_plane(r.scope)?;
        let (px, py) = pixel_of(x, y);
        let s = diag::sample(&plane.plane(), px, py, r.sample_size).map_err(|e| format!("eyedropper: {}", e.code()))?.ok_or("eyedropper_outside_canvas")?;
        let v = json!({"x": x, "y": y, "pixel": [px, py], "scope": r.scope.code(), "sample_size": r.sample_size.code(), "rgba8": s.rgba8, "rgb": s.rgb, "alpha": s.alpha, "pixels": s.pixels, "readout": readout_json(s.rgb, r.model)});
        if s.rgba8[3] == 0 {
            return Err(format!("eyedropper_transparent: {v}"));
        }
        self.edit.brush.rgb8 = [s.rgba8[0], s.rgba8[1], s.rgba8[2]];
        let mut v = v;
        v["swatch"] = json!(self.edit.brush.rgb8);
        Ok(v)
    }

    /// The histogram of `plane` over the active selection (else the whole plane).
    fn info_histogram_of(&self, plane: &Plane<'_>) -> Result<Histogram, String> {
        diag::histogram(&plane.plane(), self.edit.selection.as_ref(), BINS).map_err(|e| format!("histogram: {}", e.code()))
    }

    fn info_key(&self) -> Key {
        (self.info.frame_gen, selection_key(self.edit.selection.as_ref()))
    }

    /// Recomputes the cached histogram (and the read-back plane) when the frame or the selection
    /// changed. Called by the panel each frame; cheap when nothing changed.
    pub(super) fn info_refresh(&mut self) {
        let key = self.info_key();
        if self.info.cache.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let composite = self.info_composite();
        let h = match &composite {
            Ok(Some((p, _))) => self.info_histogram_of(p),
            Ok(None) => Err("no rendered frame".into()),
            Err(e) => Err(e.clone()),
        };
        let read_back = match composite {
            Ok(Some((Plane::Owned(p), true))) => Some(p),
            _ => None,
        };
        if let Some(p) = read_back {
            self.info.read_back = Some((self.info.frame_gen, p));
        }
        self.info.cache = Some((key, h));
    }

    // ----- inspection --------------------------------------------------------------------------

    /// `state_json()["info"]`: every number the panel shows (STU-RAS-163: bin and clip counts as
    /// structured data).
    pub(super) fn info_json(&self) -> Value {
        let (records, error) = match self.info_records() {
            Ok(r) => (r, Value::Null),
            Err(e) => (InfoRecords::default(), json!(e)),
        };
        let composite = self.info_composite();
        let (plane, read_back, composite_error) = match composite {
            Ok(Some((p, rb))) => (Some(p), rb, Value::Null),
            Ok(None) => (None, false, Value::Null),
            Err(e) => (None, false, json!(e)),
        };
        let key = self.info_key();
        let computed;
        // No frame: no histogram (the reason, if any, is `composite_error`).
        let histogram: Option<&Result<Histogram, String>> = match (&self.info.cache, &plane) {
            (_, None) => None,
            (Some((k, h)), _) if *k == key => Some(h),
            (_, Some(p)) => {
                computed = self.info_histogram_of(p);
                Some(&computed)
            }
        };
        let size = records.sample_size;
        let sample_at = |x: f64, y: f64, model: ColourModel| -> Value {
            let (px, py) = pixel_of(x, y);
            let mut v = match &plane {
                Some(p) => match diag::sample(&p.plane(), px, py, size) {
                    Ok(s) => sample_json(s, model),
                    Err(e) => json!({"error": e.code()}),
                },
                None => json!({"in_bounds": false}),
            };
            v["x"] = json!(x);
            v["y"] = json!(y);
            v["pixel"] = json!([px, py]);
            v
        };
        let pointer = self.info.hover.map(|[x, y]| sample_at(x, y, records.model));
        let samplers: Vec<Value> = records
            .samplers
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut v = sample_at(s.x, s.y, s.model);
                v["index"] = json!(i);
                v
            })
            .collect();
        let selection = match self.edit.selection.as_ref().map(diag::coverage_bounds) {
            Some(Ok(Some((r, n)))) => json!({"bounds": {"x": r.x, "y": r.y, "width": r.width, "height": r.height}, "pixels": n}),
            Some(Ok(None)) => json!({"bounds": null, "pixels": 0}),
            Some(Err(e)) => json!({"error": e.code()}),
            None => Value::Null,
        };
        json!({
            "open": self.info.open,
            "visible": self.panel_visible(super::shell::Panel::Info),
            "pick": self.info.pick.code(),
            "channel": self.info.channel.unwrap_or(HistogramChannel::Luminosity).code(),
            "entry": self.run.document_entry(ENTRY).is_some(),
            "error": error,
            "model": records.model.code(),
            "models": ColourModel::ALL.iter().map(|m| m.code()).collect::<Vec<_>>(),
            "sample_size": size.code(),
            "sample_sizes": SampleSize::ALL.iter().map(|s| s.code()).collect::<Vec<_>>(),
            "scope": records.scope.code(),
            "scopes": SampleScope::ALL.iter().map(|s| s.code()).collect::<Vec<_>>(),
            "max_samplers": MAX_SAMPLERS,
            "document": self.frame.as_ref().map(|f| json!({"x": f.x, "y": f.y, "width": f.width, "height": f.height, "revision": f.revision, "unit": "pixels"})),
            "read_back": read_back,
            "composite_error": composite_error,
            "view": self.info.view.map(|(min, zoom, origin)| json!({"shown_min": min, "zoom": zoom, "origin": origin})),
            "pointer": pointer,
            "selection": selection,
            "region": if self.edit.selection.is_some() { "selection" } else { "document" },
            "histogram": histogram.map(|h| match h {
                Ok(h) => histogram_json(h),
                Err(e) => json!({"error": e}),
            }),
            "samplers": samplers,
            "eyedropper": self.info.eyedropper,
            "swatch": self.edit.brush.rgb8,
            "not_proven": [
                "NOT_PROVEN: equality with Photoshop's Histogram / Info panels and colour samplers (bins, luminosity weights, sample edge handling, CMYK readout profile; no D5 case for PS-30)",
                "NOT_PROVEN: GPU shared-device display path (desktop window): the panel reads the render-cpu read-back frame"
            ],
        })
    }

    // ----- panel -------------------------------------------------------------------------------

    /// The Info panel (a right side panel of its own).
    pub(super) fn info_ui(&mut self, ctx: &egui::Context) {
        self.info_refresh();
        let mut commands: Vec<InfoCommand> = Vec::new();
        let mut pick = self.info.pick;
        let mut channel = self.info.channel.unwrap_or(HistogramChannel::Luminosity);
        let state = self.info_json();
        let mut drawn: Vec<String> = Vec::new();
        egui::SidePanel::right(vw::id(widget::PANEL)).resizable(true).default_width(260.0).show(ctx, |ui| {
            // A SidePanel keeps the width its content used last frame: span the panel (lane
            // SMB-SHELL-ROW-WRAP-1 pattern) so wrapped rows never narrow it.
            ui.set_min_width(ui.available_width());
            ui.heading("Info");
            egui::ScrollArea::vertical().id_salt(vw::id("info.scroll")).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let open = self.run.snapshot().is_some();
                // Readout model, sample size, scope.
                ui.horizontal_wrapped(|ui| {
                    let model = ColourModel::from_code(state["model"].as_str().unwrap_or("rgb")).unwrap_or_default();
                    let mut m = model;
                    wrap_measured(ui, vw::id(widget::MODEL), |ui| {
                        egui::ComboBox::from_label("model").selected_text(m.label()).show_ui(ui, |ui| {
                            for c in ColourModel::ALL {
                                ui.selectable_value(&mut m, c, c.label());
                            }
                        })
                    });
                    if m != model {
                        commands.push(InfoCommand::SetModel(m));
                    }
                    drawn.push(widget::MODEL.into());
                    let size = SampleSize::from_code(state["sample_size"].as_str().unwrap_or("point")).unwrap_or_default();
                    let mut s = size;
                    wrap_measured(ui, vw::id(widget::SAMPLE_SIZE), |ui| {
                        egui::ComboBox::from_label("sample").selected_text(s.code()).show_ui(ui, |ui| {
                            for c in SampleSize::ALL {
                                ui.selectable_value(&mut s, c, c.code());
                            }
                        })
                    });
                    if s != size {
                        commands.push(InfoCommand::SetSampleSize(s));
                    }
                    drawn.push(widget::SAMPLE_SIZE.into());
                    let scope = SampleScope::from_code(state["scope"].as_str().unwrap_or("all_layers")).unwrap_or_default();
                    let mut sc = scope;
                    wrap_measured(ui, vw::id(widget::SCOPE), |ui| {
                        egui::ComboBox::from_label("scope").selected_text(sc.label()).show_ui(ui, |ui| {
                            for c in SampleScope::ALL {
                                ui.selectable_value(&mut sc, c, c.label());
                            }
                        })
                    });
                    if sc != scope {
                        commands.push(InfoCommand::SetSampleScope(sc));
                    }
                    drawn.push(widget::SCOPE.into());
                });
                ui.horizontal_wrapped(|ui| {
                    for (name, label, p, hover) in [
                        (widget::PICK_SAMPLER, "Place sampler", Pick::Sampler, "A canvas click adds a colour sampler"),
                        (widget::PICK_EYEDROPPER, "Eyedropper", Pick::Eyedropper, "A canvas click samples into the brush colour (size and scope above)"),
                    ] {
                        let on = pick == p;
                        if wrap_item(ui, vw::id(name), label, egui::TextStyle::Button, |ui| ui.add_enabled(open, egui::Button::selectable(on, label))).on_hover_text(hover).clicked() {
                            pick = if on { Pick::None } else { p };
                        }
                        drawn.push(name.into());
                    }
                });
                ui.separator();
                match &state["document"] {
                    Value::Null => {
                        ui.label("Open a file to read pixel values.");
                    }
                    d => {
                        ui.label(format!("Document: {} x {} px (origin {}, {})", d["width"], d["height"], d["x"], d["y"]));
                    }
                }
                let p = &state["pointer"];
                if p.is_null() {
                    ui.label("Pointer: off the canvas");
                } else {
                    ui.label(format!("Pointer: {:.1}, {:.1} (pixel {}, {})", p["x"].as_f64().unwrap_or(0.0), p["y"].as_f64().unwrap_or(0.0), p["pixel"][0], p["pixel"][1]));
                    if p["in_bounds"] == true {
                        ui.label(format!("RGBA: {}", p["rgba8"]));
                        ui.label(readout_text(&p["readout"]));
                    }
                }
                match &state["selection"] {
                    Value::Null => {
                        ui.label("Selection: none");
                    }
                    s if s["bounds"].is_object() => {
                        let b = &s["bounds"];
                        ui.label(format!("Selection: {}, {}  {} x {} px ({} px covered)", b["x"], b["y"], b["width"], b["height"], s["pixels"]));
                    }
                    _ => {
                        ui.label("Selection: empty");
                    }
                }
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.strong(if state["region"] == "selection" { "Histogram (selection)" } else { "Histogram" });
                    wrap_measured(ui, vw::id(widget::CHANNEL), |ui| {
                        egui::ComboBox::from_label("channel").selected_text(channel.code()).show_ui(ui, |ui| {
                            for c in HistogramChannel::ALL {
                                ui.selectable_value(&mut channel, c, c.code());
                            }
                        })
                    });
                    drawn.push(widget::CHANNEL.into());
                });
                let h = &state["histogram"];
                let c = &h["channels"][channel.code()];
                let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 96.0), egui::Sense::hover());
                drawn.push(widget::HISTOGRAM.into());
                draw_histogram(ui.painter(), rect, c["bins"].as_array());
                if let Some(e) = h["error"].as_str() {
                    ui.colored_label(egui::Color32::RED, e);
                } else if c.is_object() {
                    ui.label(format!("Pixels: {}  mean {:.2}  std dev {:.2}  median {}", c["count"], c["mean"].as_f64().unwrap_or(0.0), c["std_dev"].as_f64().unwrap_or(0.0), c["median"]));
                    let text = ui.visuals().text_color();
                    let colour = |n: &Value| if n.as_u64().unwrap_or(0) > 0 { egui::Color32::from_rgb(230, 120, 30) } else { text };
                    ui.colored_label(colour(&c["clip_low"]), format!("Clipped shadows: {} ({:.2}%)", c["clip_low"], c["clip_low_pct"].as_f64().unwrap_or(0.0)));
                    ui.colored_label(colour(&c["clip_high"]), format!("Clipped highlights: {} ({:.2}%)", c["clip_high"], c["clip_high_pct"].as_f64().unwrap_or(0.0)));
                }
                ui.separator();
                ui.strong(format!("Colour samplers ({} of {MAX_SAMPLERS})", state["samplers"].as_array().map_or(0, Vec::len)));
                for s in state["samplers"].as_array().cloned().unwrap_or_default() {
                    let i = usize::try_from(s["index"].as_u64().unwrap_or(0)).unwrap_or(0);
                    ui.horizontal_wrapped(|ui| {
                        let text = if s["in_bounds"] == true { format!("#{}  ({}, {})  {}  {}", i.saturating_add(1), s["pixel"][0], s["pixel"][1], s["rgba8"], readout_text(&s["readout"])) } else { format!("#{}  ({}, {})  off the canvas", i.saturating_add(1), s["pixel"][0], s["pixel"][1]) };
                        ui.label(text);
                        let name = widget::sampler_remove(i);
                        if wrap_item(ui, vw::id(&name), "Remove", egui::TextStyle::Button, |ui| ui.button("Remove")).clicked() {
                            commands.push(InfoCommand::RemoveSampler(i));
                        }
                        drawn.push(name);
                    });
                }
                if let Some(e) = state["eyedropper"].as_object() {
                    ui.separator();
                    match e.get("error").and_then(Value::as_str) {
                        Some(err) => ui.colored_label(egui::Color32::RED, format!("Eyedropper: {err}")),
                        None => ui.label(format!("Eyedropper: {} ({} {}) -> swatch {}", e.get("rgba8").cloned().unwrap_or_default(), e.get("scope").cloned().unwrap_or_default(), e.get("sample_size").cloned().unwrap_or_default(), state["swatch"])),
                    };
                }
                if state["read_back"] == true {
                    ui.small("Reading the render-cpu read-back frame (the GPU display texture has no CPU buffer).");
                }
            });
        });
        self.drawn.push(widget::PANEL.into());
        self.drawn.extend(drawn);
        self.info.pick = pick;
        self.info.channel = Some(channel);
        for c in commands {
            let _ = self.info_command(c);
        }
    }
}

fn readout_text(r: &Value) -> String {
    let names = r["components"].as_array().cloned().unwrap_or_default();
    let values = r["values"].as_array().cloned().unwrap_or_default();
    let parts: Vec<String> = names.iter().zip(values.iter()).map(|(n, v)| format!("{} {:.1}", n.as_str().unwrap_or("?"), v.as_f64().unwrap_or(0.0))).collect();
    format!("{}: {}", r["model"].as_str().unwrap_or("?").to_uppercase(), parts.join("  "))
}

fn draw_histogram(painter: &egui::Painter, rect: egui::Rect, bins: Option<&Vec<Value>>) {
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(24));
    let Some(bins) = bins else { return };
    let counts: Vec<f32> = bins.iter().map(|b| b.as_u64().unwrap_or(0) as f32).collect();
    let max = counts.iter().copied().fold(0.0f32, f32::max);
    if max <= 0.0 || counts.is_empty() {
        return;
    }
    let w = rect.width() / counts.len() as f32;
    let shapes: Vec<egui::Shape> = counts
        .iter()
        .enumerate()
        .filter(|(_, c)| **c > 0.0)
        .map(|(i, c)| {
            let x = rect.min.x + i as f32 * w;
            let h = rect.height() * c / max;
            egui::Shape::rect_filled(egui::Rect::from_min_max(egui::pos2(x, rect.max.y - h), egui::pos2(x + w.max(1.0), rect.max.y)), 0.0, egui::Color32::from_gray(200))
        })
        .collect();
    painter.extend(shapes);
}
