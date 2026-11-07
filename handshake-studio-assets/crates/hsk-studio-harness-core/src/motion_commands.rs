//! Typed motion commands (lane SMB-MOTION-COMMANDS-1; [STU-MOT-130], [STU-MOT-038], [STU-CMP-110]): the
//! `composition.*`, `property.*`, `keyframe.*` and `expression.*` rows of `RUN_COMMANDS`, run through
//! [`Run::motion_command`]. A row's [`spec_id`] is the `STUDIO_*` id [STU-MOT-130] names; the row id is
//! the lowercase spelling because the business-event envelope admits only the RUN_COMMANDS grammar
//! `[a-z0-9._-]{1,64}`. Both spellings run.
//!
//! One command: the stored composition and motion records (validated on read), one edit of a copy,
//! Folio validation of the result against the document, then one commit and one named history step
//! carrying the caller's `CommandContext`. A refusal changes nothing and records a typed `error` and a
//! bounded `target`. Record edits are document entries, so they make no document revision (the
//! reading of the existing entry-only steps); a command that creates a document layer also commits
//! that layer, and undo restores layer and entries together.
//!
//! Property addresses (`property` parameter): `anchor`, `position`, `scale`, `rotation`, `opacity`,
//! `time_remap`, `corner_pin`, `three_d.<position_z|anchor_z|rotation_x|rotation_y|orientation>`,
//! `three_d.material.<name>`, `camera.<zoom|point_of_interest|name>`,
//! `light.<colour|intensity|point_of_interest|name>`, `effect.<effect_id>.<param_key>`,
//! `mask.<mask_id>.<path|feather|opacity|expansion>`, `shape.<index>...<field>` (the item at the index
//! path, or the group for `group_*`, the root without an index) and
//! `text.<animator>.<selector>.<selector param>` or `text.<animator>.<param>`.
//!
//! `keyframe.add_at_current` keeps every sample ([STU-MOT-038]: "adding a keyframe never changes the
//! render"): [`add_preserving`] splits a Bezier segment exactly and flattens an end segment. The curve
//! model here mirrors motion's `keyframe::shape` because harness-core has no edge to motion; the
//! feature `media` test `add_at_current_leaves_the_real_evaluator_unchanged` pins it to motion through
//! composite's scene evaluation.
//!
//! Declared limits: a composition property has no expression store in Folio, so `expression.set_source`
//! edits the expression field that exists (a text selector's `source`); a `recordable: false` row is
//! reported `not_recordable` while recording; `set_parent` does not compensate the child's transform;
//! keyframe selection, copy and paste are not commands here.
#![deny(clippy::arithmetic_side_effects)]
use super::{EntryRestore, HistoryStep, Place, PlacementWrite, Run};
use crate::action::{self, CommandEntry, ValueType};
use hsk_studio_accord::TICKS_PER_SECOND;
use hsk_studio_folio::{
    self as folio, CompositionSchema, StudioAssistedRoto, StudioAudioLayer, StudioCameraOptions, StudioComposition, StudioCompositionAudio, StudioCompositionEffect, StudioCompositionKeyframe as Key,
    StudioCompositionLayer, StudioCompositionMask, StudioCompositionMotionSettings, StudioCompositionRecords, StudioCompositionWorkArea, StudioCornerPin, StudioDocument, StudioFrameBlending,
    StudioFrameRate, StudioInterpolation as Interp, StudioKeyedValue, StudioLayer, StudioLayer3d, StudioLayerProperty, StudioLayerTiming, StudioLightOptions,
    StudioMotionRecords, StudioOutputModule, StudioPrecompSource, StudioPropertyStream, StudioRational, StudioRenderQueueItem, StudioRenderSettings, StudioShapeField, StudioShapeGroup, StudioShapeItem,
    StudioShapeLayer, StudioShapeStream, StudioTemporalTangent, StudioTextAnimator, StudioTextAnimators, StudioTextParam, StudioTextSelector, StudioTextStream, StudioTrackMatte,
};
use hsk_studio_session::{COMPOSITION_ENTRY, MOTION_ENTRY, SessionError, session_error_diagnostic};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Longest property address and longest refusal target.
const MAX_ADDRESS: usize = 256;
/// [STU-MOT-034a] default ease influence (motion `DEFAULT_INFLUENCE`).
const DEFAULT_INFLUENCE: f64 = 0.16666666666;
/// Motion's chord handle of a linear side beside a Bezier side.
const CHORD_INFLUENCE: f64 = 1.0 / 6.0;
const BISECTION_STEPS: u32 = 64;
/// Angle of view of a new camera (composite `three_d::default_camera`).
const DEFAULT_CAMERA_ANGLE: f64 = 39.5978;

/// The `STUDIO_*` id [STU-MOT-130] names for a row: `composition.new` is `STUDIO_COMPOSITION_NEW`.
pub fn spec_id(command_id: &str) -> String {
    format!("STUDIO_{}", command_id.replace('.', "_").to_ascii_uppercase())
}

/// The motion row of `command_id` (its row id or its `STUDIO_*` spelling).
pub fn row(command_id: &str) -> Option<&'static CommandEntry> {
    if command_id.len() > MAX_ADDRESS {
        return None;
    }
    action::RUN_COMMANDS.iter().find(|c| c.run_method == "motion_command" && (c.command_id == command_id || spec_id(c.command_id) == command_id))
}

/// A row's parameters from text (the drive loop carries text): numbers, booleans, lists and classes
/// are JSON, strings and enumerations stay text, `null` clears. Refused: `(code, key)`.
pub fn params_from_text(row: &CommandEntry, params: &BTreeMap<String, String>) -> Result<Value, (&'static str, String)> {
    let mut out = Map::new();
    for (key, text) in params {
        let Some(spec) = row.parameters.iter().find(|p| p.key == key) else {
            return Err(("parameter_unknown", bounded(key)));
        };
        let value = if text == "null" {
            Value::Null
        } else {
            match spec.value_type {
                ValueType::Reference | ValueType::String | ValueType::Enumerated => match serde_json::from_str::<Value>(text) {
                    // A class-shaped enumeration (`resolution`, `time_span`) arrives as JSON text.
                    Ok(v @ Value::Object(_)) => v,
                    _ => Value::String(text.clone()),
                },
                ValueType::Boolean | ValueType::Number | ValueType::Measured | ValueType::List | ValueType::Class => {
                    let v: Value = serde_json::from_str(text).map_err(|_| ("parameter_invalid", bounded(key)))?;
                    let fits = match spec.value_type {
                        ValueType::Boolean => v.is_boolean(),
                        ValueType::Number | ValueType::Measured => v.is_number(),
                        ValueType::List => v.is_array(),
                        // A class parameter may be a bare member of an enumeration (`time_span`).
                        _ => v.is_object() || v.is_string(),
                    };
                    if !fits {
                        return Err(("parameter_invalid", bounded(key)));
                    }
                    v
                }
            }
        };
        out.insert(key.clone(), value);
    }
    Ok(Value::Object(out))
}

fn bounded(text: &str) -> String {
    text.chars().take(MAX_ADDRESS).collect()
}

/// A typed refusal: a stable code and a bounded target.
struct No(&'static str, String);
type R<T> = Result<T, No>;

fn no<T>(code: &'static str, target: &str) -> R<T> {
    Err(No(code, bounded(target)))
}

/// A session refusal: a Folio diagnostic keeps its own code and record path.
fn session_no(e: &SessionError) -> No {
    let Some(d) = session_error_diagnostic(e) else {
        return No(e.code(), String::new());
    };
    let code = match d.code {
        folio::Code::InvalidField => "invalid_field",
        folio::Code::InvalidId => "invalid_id",
        folio::Code::DuplicateIdentity => "duplicate_identity",
        folio::Code::DanglingReference => "dangling_reference",
        folio::Code::ContainmentCycle => "containment_cycle",
        folio::Code::RenderCycle => "render_cycle",
        folio::Code::BudgetExceeded => "budget_exceeded",
        folio::Code::Canceled => "canceled",
        _ => e.code(),
    };
    No(code, bounded(&d.target))
}

/// The declared parameters of one call.
struct P<'a>(&'a Map<String, Value>);

impl P<'_> {
    fn has(&self, key: &str) -> bool {
        self.0.contains_key(key)
    }
    fn text(&self, key: &str) -> R<String> {
        match self.0.get(key) {
            None | Some(Value::Null) => no("parameter_missing", key),
            Some(Value::String(s)) => Ok(s.clone()),
            Some(_) => no("parameter_invalid", key),
        }
    }
    /// The parameters of `row` other than `skip`, as overlay members.
    fn members(&self, row: &CommandEntry, skip: &[&str]) -> Vec<(&str, &Value)> {
        row.parameters.iter().filter(|p| !skip.contains(&p.key)).filter_map(|p| self.0.get_key_value(p.key).map(|(k, v)| (k.as_str(), v))).collect()
    }
}

/// `$current` as JSON with `members` over it (`null` removes a member).
fn merged(mut value: Value, members: &[(&str, &Value)]) -> R<Value> {
    let Value::Object(map) = &mut value else {
        return no("parameter_invalid", "");
    };
    for (key, v) in members {
        if v.is_null() {
            map.remove(*key);
        } else {
            map.insert((*key).to_string(), (*v).clone());
        }
    }
    Ok(value)
}

// harness-core names no serde trait (no direct serde edge): these expand where the types are concrete.
/// An optional parameter: absent or `null` is `None`.
macro_rules! opt {
    ($p:expr, $key:expr) => {
        match $p.0.get($key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => serde_json::from_value(v.clone()).map(Some).map_err(|_| No("parameter_invalid", bounded($key))),
        }
    };
    ($p:expr, $key:expr, $t:ty) => {
        match $p.0.get($key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => serde_json::from_value::<$t>(v.clone()).map(Some).map_err(|_| No("parameter_invalid", bounded($key))),
        }
    };
}
/// A required parameter.
macro_rules! req {
    ($p:expr, $key:expr) => {
        match $p.0.get($key) {
            None | Some(Value::Null) => Err(No("parameter_missing", bounded($key))),
            Some(v) => serde_json::from_value(v.clone()).map_err(|_| No("parameter_invalid", bounded($key))),
        }
    };
    ($p:expr, $key:expr, $t:ty) => {
        match $p.0.get($key) {
            None | Some(Value::Null) => Err(No("parameter_missing", bounded($key))),
            Some(v) => serde_json::from_value::<$t>(v.clone()).map_err(|_| No("parameter_invalid", bounded($key))),
        }
    };
}
/// `$current` with `$members` over it, as its own type.
macro_rules! overlay {
    ($current:expr, $members:expr) => {
        serde_json::to_value(&*$current).map_err(|_| No("parameter_invalid", String::new())).and_then(|v| merged(v, $members)).and_then(|v| serde_json::from_value(v).map_err(|_| No("parameter_invalid", String::new())))
    };
}
/// `$current` with the row's other parameters over it.
macro_rules! patched {
    ($p:expr, $row:expr, $skip:expr, $current:expr) => {
        overlay!($current, &$p.members($row, $skip))
    };
}

/// The copies a command edits, what it creates, and what it reports.
struct Edit<'r> {
    row: &'static CommandEntry,
    doc: &'r StudioDocument,
    comp: StudioCompositionRecords,
    motion: Option<StudioMotionRecords>,
    create: Vec<(StudioLayer, folio::Operation)>,
    detail: Map<String, Value>,
}

impl Edit<'_> {
    fn motion_mut(&mut self) -> &mut StudioMotionRecords {
        let id = self.comp.document_id.clone();
        self.motion.get_or_insert_with(|| StudioMotionRecords::new(id))
    }
    fn composition_mut(&mut self, id: &str) -> R<&mut StudioComposition> {
        match self.comp.compositions.iter_mut().find(|c| c.composition_id == id) {
            Some(c) => Ok(c),
            None => no("composition_not_found", id),
        }
    }
    fn layer_mut(&mut self, id: &str) -> R<&mut StudioCompositionLayer> {
        match self.comp.compositions.iter_mut().flat_map(|c| c.layers.iter_mut()).find(|l| l.layer_id == id) {
            Some(l) => Ok(l),
            None => no("layer_not_in_composition", id),
        }
    }
    fn composition_of(&self, layer_id: &str) -> R<&StudioComposition> {
        match self.comp.layer(layer_id) {
            Some((c, _)) => Ok(c),
            None => no("layer_not_in_composition", layer_id),
        }
    }
    fn say(&mut self, key: &str, value: Value) {
        self.detail.insert(key.to_string(), value);
    }
    fn layer_taken(&self, id: &str) -> bool {
        self.doc.layers.iter().any(|l| l.layer_id == id) || self.create.iter().any(|(l, _)| l.layer_id == id)
    }
    fn new_layer_id(&self, wanted: Option<String>) -> R<String> {
        match wanted {
            Some(id) => Ok(id),
            None => mint("SLYR", self.doc.layers.len().saturating_add(self.create.len()), |id| self.layer_taken(id)),
        }
    }
    fn new_composition_id(&self, wanted: Option<String>) -> R<String> {
        match wanted {
            Some(id) => Ok(id),
            None => mint("SCOM", self.comp.compositions.len(), |id| self.comp.composition(id).is_some()),
        }
    }
    /// A document layer this command creates, placed at the top of the document's stack.
    fn create_layer(&mut self, id: &str, name: &str, group: bool) -> R<()> {
        if self.layer_taken(id) {
            return no("duplicate_identity", id);
        }
        let (kind, payload, op) = if group {
            ("group", json!({"payload_kind": "group"}), json!({"layer_id": id, "operation_key": "mix", "operation_kind": "composite",
                "inputs": [{"port_key": "inputs", "value_type": "image", "cardinality": "many", "semantic": "ordered_composite"}],
                "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "composite_result"}]}))
        } else {
            ("raster", json!({"payload_kind": "raster", "tiles": []}), json!({"layer_id": id, "operation_key": "source", "operation_kind": "source", "inputs": [],
                "outputs": [{"port_key": "out", "value_type": "image", "cardinality": "one", "semantic": "source"}]}))
        };
        let layer = serde_json::from_value(json!({"schema_id": "hsk.studio.layer@1", "layer_id": id, "name": name, "parent": null, "kind": kind, "payload": payload, "visible": true}));
        let (Ok(layer), Ok(op)) = (layer, serde_json::from_value(op)) else {
            return no("parameter_invalid", "layer_id");
        };
        self.create.push((layer, op));
        Ok(())
    }
}

/// The first free id of the fixed-time form `<prefix>-0195f000-0000-7000-8000-<ordinal>`: deterministic,
/// and free within the record (at most `used + 1` candidates).
fn mint(prefix: &str, used: usize, taken: impl Fn(&str) -> bool) -> R<String> {
    for n in 1..=u64::try_from(used).unwrap_or(u64::MAX).saturating_add(1) {
        let id = format!("{prefix}-0195f000-0000-7000-8000-{n:012x}");
        if !taken(&id) {
            return Ok(id);
        }
    }
    no("id_space_exhausted", prefix)
}

/// Runs one motion command (`Run::motion_command`).
pub(super) fn run(run: &mut Run<'_>, command_id: &str, params: &Value) -> bool {
    let before = run.frames.count();
    let Some(row) = row(command_id) else {
        let r = run.own_or_push("motion_command", before, Err("unknown_command"));
        target(run, &bounded(command_id));
        return r;
    };
    let op = row.command_id;
    run.note_unrecordable(op);
    if run.session.is_none() {
        return run.push(op, before, Err("not_open"), None, None);
    }
    let _ = run.begin(op, None);
    match plan(run, row, params) {
        Err(No(code, at)) => {
            let r = run.own_op(op, before, Err(code));
            target(run, &at);
            r
        }
        Ok(planned) if !row.mutates_authority => run.own_op(op, before, Ok(Value::Object(planned.detail))),
        Ok(planned) => commit(run, row, before, planned),
    }
}

fn target(run: &mut Run<'_>, at: &str) {
    if let Some(record) = run.ops.last_mut() {
        record["target"] = json!(at);
    }
}

/// What a validated edit commits.
struct Planned {
    composition: Option<Vec<u8>>,
    motion: Option<Option<Vec<u8>>>,
    create: Vec<(StudioLayer, folio::Operation)>,
    detail: Map<String, Value>,
}

fn plan(run: &Run<'_>, row: &'static CommandEntry, params: &Value) -> R<Planned> {
    run.cancel.check().map_err(|_| No("canceled", String::new()))?;
    let Value::Object(map) = params else {
        return no("parameter_invalid", "parameters");
    };
    if let Some(key) = map.keys().find(|k| !row.parameters.iter().any(|p| p.key == k.as_str())) {
        return no("parameter_unknown", key);
    }
    let session = run.session.as_ref().ok_or(No("not_open", String::new()))?;
    let resolver = run.harness.resolver();
    let doc = session.snapshot().document();
    let comp = session.composition_records(&*resolver, run.cancel).map_err(|e| session_no(&e))?.unwrap_or_else(|| StudioCompositionRecords::new(doc.document_id.clone()));
    let motion = session.motion_records(&*resolver, run.cancel).map_err(|e| session_no(&e))?;
    let mut edit = Edit { row, doc, comp, motion, create: Vec::new(), detail: Map::new() };
    apply(&mut edit, &P(map))?;
    let Edit { comp, motion, create, detail, .. } = edit;
    if !row.mutates_authority {
        return Ok(Planned { composition: None, motion: None, create, detail });
    }
    let created: Vec<StudioLayer> = create.iter().map(|(l, _)| l.clone()).collect();
    let (comp_bytes, motion_bytes) = session.motion_entries_bytes(&comp, motion.as_ref(), &created, &*resolver, run.cancel).map_err(|e| session_no(&e))?;
    let composition = (session.document_entry(COMPOSITION_ENTRY) != Some(comp_bytes.as_slice())).then_some(comp_bytes);
    let motion = (session.document_entry(MOTION_ENTRY) != motion_bytes.as_deref()).then_some(motion_bytes);
    if composition.is_none() && motion.is_none() && create.is_empty() {
        return no("nothing_to_change", row.command_id);
    }
    Ok(Planned { composition, motion, create, detail })
}

/// Commits a plan as one record and one history step (module docs).
fn commit(run: &mut Run<'_>, row: &'static CommandEntry, before: usize, planned: Planned) -> bool {
    let op = row.command_id;
    let Planned { composition, motion, create, mut detail } = planned;
    let cancel = run.cancel;
    let entry = |run: &Run<'_>, token: &str| run.session.as_ref().and_then(|s| s.document_entry(token)).map(<[u8]>::to_vec);
    let mut restores = Vec::new();
    if composition.is_some() {
        restores.push(EntryRestore { token: COMPOSITION_ENTRY.to_string(), bytes: entry(run, COMPOSITION_ENTRY) });
    }
    if motion.is_some() {
        restores.push(EntryRestore { token: MOTION_ENTRY.to_string(), bytes: entry(run, MOTION_ENTRY) });
    }
    let mut steps = Vec::new();
    let created = !create.is_empty();
    if created {
        let depth = run.undo_stack.len();
        let writes: Vec<PlacementWrite> = create.into_iter().map(|(layer, operation)| PlacementWrite { layer_id: layer.layer_id.clone(), to: Some(Place::AtBeginning(None)), create: Some((layer, operation)) }).collect();
        if !run.quietly(|r| r.restructure_inner(op, &writes, op)) {
            return false;
        }
        if run.undo_stack.len() > depth {
            steps.extend(run.undo_stack.pop());
        }
    }
    let writes = [composition.map(|b| (COMPOSITION_ENTRY, Some(b))), motion.map(|b| (MOTION_ENTRY, b))];
    let mut failed = None;
    for (token, bytes) in writes.into_iter().flatten() {
        let Some(session) = run.session.as_mut() else {
            failed = Some("not_open");
            break;
        };
        if let Err(e) = session.set_document_entry(token, bytes, cancel) {
            failed = Some(e.code());
            break;
        }
    }
    if let Some(code) = failed {
        // Nothing stays half done: the entries written so far and the created layer go back.
        for restore in &restores {
            if let Some(session) = run.session.as_mut() {
                let _ = session.set_document_entry(&restore.token, restore.bytes.clone(), cancel);
            }
        }
        if let Some(step) = steps.pop() {
            run.undo_stack.push(step);
            let _ = run.quietly(Run::undo);
            run.redo_stack.clear();
        }
        run.current = run.last_command.take();
        return run.own_op(op, before, Err(code));
    }
    if created {
        // The structure commit wrote the command's record; its context attributes the step.
        let ctx = run.last_command.clone();
        let revision = run.revision().unwrap_or(0);
        let ctx = ctx.unwrap_or_else(|| run.context.command(Some(op), revision, None));
        for restore in restores {
            steps.push(HistoryStep { label: row.display_name.to_string(), inverse: Value::Null, entry: Some(restore), timeline: None, group: None, sequences: None, ctx: ctx.clone(), revision });
        }
        push_step(run, row, steps, ctx, revision);
        if let Some(record) = run.ops.last_mut() {
            for (k, v) in std::mem::take(&mut detail) {
                record["detail"][k] = v;
            }
        }
        return true;
    }
    let revision = run.revision().unwrap_or(0);
    let ctx = run.current.clone().unwrap_or_else(|| run.context.command(Some(op), revision, None));
    for restore in restores {
        steps.push(HistoryStep { label: row.display_name.to_string(), inverse: Value::Null, entry: Some(restore), timeline: None, group: None, sequences: None, ctx: ctx.clone(), revision });
    }
    push_step(run, row, steps, ctx, revision);
    detail.insert("step".to_string(), json!(row.display_name));
    run.own_op(op, before, Ok(Value::Object(detail)))
}

/// The steps as ONE history entry, named by the row: one step as it is, several as a group.
fn push_step(run: &mut Run<'_>, row: &CommandEntry, mut steps: Vec<HistoryStep>, ctx: hsk_studio_accord::CommandContext, revision: u64) {
    let step = if steps.len() == 1 {
        steps.pop()
    } else {
        Some(HistoryStep { label: row.display_name.to_string(), inverse: Value::Null, entry: None, timeline: None, group: Some(steps), sequences: None, ctx, revision })
    };
    if let Some(mut step) = step {
        step.label = row.display_name.to_string();
        run.undo_stack.push(step);
        run.redo_stack.clear();
    }
}

// ----- the commands ----------------------------------------------------------------------------

fn apply(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let row = e.row;
    match row.command_id {
        "composition.new" => composition_new(e, p),
        "composition.set_settings" => {
            let id = p.text("composition_id")?;
            let c = e.composition_mut(&id)?;
            *c = patched!(p, row, &["composition_id"], c)?;
            Ok(())
        }
        "composition.delete" => composition_delete(e, p),
        "composition.read" => composition_read(e, p),
        "composition.set_work_area" => work_area(e, p),
        "composition.set_motion_blur" => motion_blur(e, p),
        "composition.add_audio_layer" => audio_add(e, p),
        "composition.remove_audio_layer" => audio_remove(e, p),
        "composition.queue_add" => queue_add(e, p),
        "composition.queue_remove" => {
            let id = p.text("item_id")?;
            let m = e.motion_mut();
            let n = m.render_queue.len();
            m.render_queue.retain(|q| q.item_id != id);
            if m.render_queue.len() == n { no("queue_item_not_found", &id) } else { Ok(()) }
        }
        "composition.queue_set_settings" => {
            let id = p.text("item_id")?;
            let q = queue_item(e, &id)?;
            q.settings = patched!(p, row, &["item_id"], &q.settings)?;
            Ok(())
        }
        "composition.queue_add_module" => {
            let id = p.text("item_id")?;
            let q = queue_item(e, &id)?;
            let module_id = match opt!(p, "module_id", String)? {
                Some(m) => m,
                None => format!("m{}", q.output_modules.len().saturating_add(1)),
            };
            let format: Option<String> = opt!(p, "format")?;
            let module = patched!(p, row, &["item_id", "module_id"], &default_module(&id, &module_id, format.as_deref().unwrap_or("mov_vc3"))?)?;
            q.output_modules.push(module);
            Ok(())
        }
        "composition.queue_set_module" => {
            let (id, module) = (p.text("item_id")?, p.text("module_id")?);
            let q = queue_item(e, &id)?;
            let Some(m) = q.output_modules.iter_mut().find(|m| m.module_id == module) else {
                return no("output_module_not_found", &module);
            };
            *m = patched!(p, row, &["item_id", "module_id"], m)?;
            Ok(())
        }
        "composition.queue_remove_module" => {
            let (id, module) = (p.text("item_id")?, p.text("module_id")?);
            let q = queue_item(e, &id)?;
            let n = q.output_modules.len();
            q.output_modules.retain(|m| m.module_id != module);
            if q.output_modules.len() == n { no("output_module_not_found", &module) } else { Ok(()) }
        }
        "composition.add_layer" => layer_add(e, p),
        "composition.remove_layer" => {
            let id = p.text("layer_id")?;
            remove_layer_records(e, &id)?;
            e.say("layer_id", json!(&id));
            Ok(())
        }
        "composition.reorder_layer" => {
            let (id, index): (String, usize) = (p.text("layer_id")?, req!(p, "index")?);
            let owner = e.composition_of(&id)?.composition_id.clone();
            let c = e.composition_mut(&owner)?;
            let Some(at) = c.layers.iter().position(|l| l.layer_id == id) else {
                return no("layer_not_in_composition", &id);
            };
            if index >= c.layers.len() {
                return no("parameter_invalid", "index");
            }
            let l = c.layers.remove(at);
            c.layers.insert(index, l);
            Ok(())
        }
        "composition.duplicate_layer" => layer_duplicate(e, p),
        "composition.set_layer_timing" => {
            let id = p.text("layer_id")?;
            let l = e.layer_mut(&id)?;
            l.timing = patched!(p, row, &["layer_id"], &l.timing)?;
            Ok(())
        }
        "composition.set_parent" => {
            let id = p.text("layer_id")?;
            let parent: Option<String> = opt!(p, "parent")?;
            e.layer_mut(&id)?.parent = parent;
            Ok(())
        }
        "composition.set_track_matte" => {
            let id = p.text("layer_id")?;
            let matte = match opt!(p, "source_layer_id", String)? {
                None => None,
                Some(source_layer_id) => Some(StudioTrackMatte { source_layer_id, mode: req!(p, "mode")?, inverted: opt!(p, "inverted")?.unwrap_or(false) }),
            };
            e.layer_mut(&id)?.track_matte = matte;
            Ok(())
        }
        "composition.set_switches" => switches(e, p),
        "composition.precompose" => precompose(e, p),
        "composition.set_time_remap" => time_remap(e, p),
        "composition.set_camera" => {
            let id = p.text("layer_id")?;
            let Some(c) = e.layer_mut(&id)?.camera.as_mut() else {
                return no("layer_not_camera", &id);
            };
            *c = patched!(p, row, &["layer_id"], c)?;
            Ok(())
        }
        "composition.set_light" => {
            let id = p.text("layer_id")?;
            let Some(l) = e.layer_mut(&id)?.light.as_mut() else {
                return no("layer_not_light", &id);
            };
            *l = patched!(p, row, &["layer_id"], l)?;
            Ok(())
        }
        "composition.set_material" => {
            let id = p.text("layer_id")?;
            let Some(d) = e.layer_mut(&id)?.three_d.as_mut() else {
                return no("layer_not_3d", &id);
            };
            d.material = patched!(p, row, &["layer_id"], &d.material)?;
            Ok(())
        }
        "composition.add_mask" => mask_add(e, p),
        "composition.set_mask" => {
            let (id, mask) = (p.text("layer_id")?, p.text("mask_id")?);
            let m = mask_mut(e.layer_mut(&id)?, &mask)?;
            let path: Option<Vec<f64>> = opt!(p, "path")?;
            *m = patched!(p, row, &["layer_id", "mask_id", "path"], m)?;
            if let Some(path) = path {
                m.static_value = path;
            }
            Ok(())
        }
        "composition.remove_mask" => {
            let (id, mask) = (p.text("layer_id")?, p.text("mask_id")?);
            let l = e.layer_mut(&id)?;
            let n = l.masks.len();
            l.masks.retain(|m| m.mask_id != mask);
            if l.masks.len() == n { no("mask_not_found", &mask) } else { Ok(()) }
        }
        "composition.add_effect" => effect_add(e, p),
        "composition.set_effect" => effect_set(e, p),
        "composition.remove_effect" => {
            let (id, fx) = (p.text("layer_id")?, p.text("effect_id")?);
            let l = e.layer_mut(&id)?;
            let n = l.effects.len();
            l.effects.retain(|x| x.effect_id != fx);
            if l.effects.len() == n { no("effect_not_found", &fx) } else { Ok(()) }
        }
        "composition.set_corner_pin" => corner_pin(e, p),
        "composition.add_roto_stroke" => {
            let id = p.text("layer_id")?;
            let (stroke, frames) = (req!(p, "stroke")?, req!(p, "source_frames", u64)?);
            let l = e.layer_mut(&id)?;
            let roto = l.roto.get_or_insert_with(|| Box::new(StudioAssistedRoto::default()));
            roto.add_stroke(stroke, frames).map_err(|d| No("invalid_field", bounded(&d.target)))?;
            Ok(())
        }
        "composition.set_roto" => {
            let id = p.text("layer_id")?;
            let roto: Option<Box<StudioAssistedRoto>> = opt!(p, "roto")?;
            e.layer_mut(&id)?.roto = roto;
            Ok(())
        }
        "composition.add_shape_item" => shape_add(e, p),
        "composition.set_shape_item" => {
            let (id, path): (String, Vec<u32>) = (p.text("layer_id")?, req!(p, "path")?);
            let item = req!(p, "item")?;
            let shape = shape_layer_mut(e, &id)?;
            let Some(slot) = shape_item_mut(&mut shape.root, &path) else {
                return no("shape_item_not_found", &id);
            };
            *slot = item;
            Ok(())
        }
        "composition.remove_shape_item" => shape_remove(e, p),
        "composition.add_text_animator" => {
            let id = p.text("layer_id")?;
            e.layer_mut(&id)?;
            let animator = StudioTextAnimator { name: opt!(p, "name")?.unwrap_or_else(|| "Animator".to_string()), properties: opt!(p, "properties")?.unwrap_or_default(), selectors: opt!(p, "selectors")?.unwrap_or_default() };
            let m = e.motion_mut();
            if !m.text_animators.iter().any(|t| t.layer_id == id) {
                m.text_animators.push(StudioTextAnimators { layer_id: id.clone(), animators: Vec::new(), streams: Vec::new() });
            }
            let t = text_of(e, &id)?;
            t.animators.push(animator);
            let index = t.animators.len().saturating_sub(1);
            e.say("animator", json!(index));
            Ok(())
        }
        "composition.set_text_animator" => {
            let (id, index): (String, usize) = (p.text("layer_id")?, req!(p, "animator")?);
            let a = animator_mut(text_of(e, &id)?, index)?;
            if let Some(name) = opt!(p, "name")? {
                a.name = name;
            }
            if let Some(properties) = opt!(p, "properties")? {
                a.properties = properties;
            }
            Ok(())
        }
        "composition.add_text_selector" => {
            let (id, index): (String, usize) = (p.text("layer_id")?, req!(p, "animator")?);
            let selector: StudioTextSelector = req!(p, "selector")?;
            animator_mut(text_of(e, &id)?, index)?.selectors.push(selector);
            Ok(())
        }
        "property.read" => property_read(e, p),
        "property.set_value" => property_set(e, p),
        "property.enable_animation" => {
            let (id, prop, time) = key_address(p)?;
            let value: Option<Vec<f64>> = opt!(p, "value")?;
            let ((sv, keys), _) = slot(e, &id, &prop, value.as_deref())?;
            if !keys.is_empty() {
                return no("property_already_animated", &prop);
            }
            let v = value.unwrap_or_else(|| sv.clone());
            keys.push(linear_key(time, v));
            Ok(())
        }
        "property.disable_animation" => {
            let (id, prop) = (p.text("layer_id")?, p.text("property")?);
            let ((_, keys), _) = slot(e, &id, &prop, None)?;
            keys.clear();
            Ok(())
        }
        "keyframe.add_at_current" => {
            let (id, prop, time) = key_address(p)?;
            let value: Option<Vec<f64>> = opt!(p, "value")?;
            let ((sv, keys), interpolable) = slot(e, &id, &prop, value.as_deref())?;
            add_at_current(sv, keys, time, interpolable, value)
        }
        "keyframe.toggle_at_current" => {
            let (id, prop, time) = key_address(p)?;
            let value: Option<Vec<f64>> = opt!(p, "value")?;
            let ((sv, keys), interpolable) = slot(e, &id, &prop, value.as_deref())?;
            let toggled = if let Ok(at) = keys.binary_search_by_key(&time, |k| k.time) {
                keys.remove(at);
                "deleted"
            } else {
                add_at_current(sv, keys, time, interpolable, value)?;
                "added"
            };
            e.say("toggled", json!(toggled));
            Ok(())
        }
        "keyframe.set" => key_set(e, p),
        "keyframe.delete" => {
            let (id, prop, time) = key_address(p)?;
            let ((_, keys), _) = slot(e, &id, &prop, None)?;
            let Ok(at) = keys.binary_search_by_key(&time, |k| k.time) else {
                return no("keyframe_not_found", &prop);
            };
            keys.remove(at);
            Ok(())
        }
        "keyframe.move" => {
            let (id, prop, time) = key_address(p)?;
            let to: u64 = req!(p, "to")?;
            let ((_, keys), _) = slot(e, &id, &prop, None)?;
            let Ok(at) = keys.binary_search_by_key(&time, |k| k.time) else {
                return no("keyframe_not_found", &prop);
            };
            if keys.binary_search_by_key(&to, |k| k.time).is_ok() {
                return no("keyframe_exists", &prop);
            }
            let mut key = keys.remove(at);
            key.time = to;
            let insert = keys.partition_point(|k| k.time < to);
            keys.insert(insert, key);
            Ok(())
        }
        "keyframe.shift" => key_shift(e, p),
        "keyframe.set_interpolation" => {
            let (id, prop, time) = key_address(p)?;
            let (in_interp, out_interp): (Option<Interp>, Option<Interp>) = (opt!(p, "in_interp")?, opt!(p, "out_interp")?);
            let (in_tangent, out_tangent): (Option<Vec<StudioTemporalTangent>>, Option<Vec<StudioTemporalTangent>>) = (opt!(p, "in_tangent")?, opt!(p, "out_tangent")?);
            let ((_, keys), _) = slot(e, &id, &prop, None)?;
            let Ok(at) = keys.binary_search_by_key(&time, |k| k.time) else {
                return no("keyframe_not_found", &prop);
            };
            apply_interpolation(&mut keys[at], in_interp, out_interp, in_tangent, out_tangent);
            Ok(())
        }
        "expression.set_source" => {
            let (id, index, selector, source): (String, usize, usize, String) = (p.text("layer_id")?, req!(p, "animator")?, req!(p, "selector")?, req!(p, "source")?);
            let a = animator_mut(text_of(e, &id)?, index)?;
            match a.selectors.get_mut(selector) {
                Some(StudioTextSelector::Expression { source: s, .. }) => {
                    *s = source;
                    Ok(())
                }
                Some(_) => no("selector_not_expression", &id),
                None => no("property_not_found", &id),
            }
        }
        other => no("unknown_command", other),
    }
}

// ----- compositions and motion records ---------------------------------------------------------

fn composition_new(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let wanted: Option<String> = opt!(p, "composition_id")?;
    let id = e.new_composition_id(wanted)?;
    let c = StudioComposition {
        schema_id: CompositionSchema::V1,
        composition_id: id.clone(),
        name: req!(p, "name")?,
        width: req!(p, "width")?,
        height: req!(p, "height")?,
        pixel_aspect: opt!(p, "pixel_aspect")?.unwrap_or(StudioRational { numerator: 1, denominator: 1 }),
        frame_rate: req!(p, "frame_rate", StudioFrameRate)?,
        duration: req!(p, "duration")?,
        background: opt!(p, "background")?,
        immersive: None,
        layers: Vec::new(),
    };
    e.comp.compositions.push(c);
    e.say("composition_id", json!(&id));
    Ok(())
}

fn composition_delete(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let id = p.text("composition_id")?;
    let c = e.composition_mut(&id)?;
    let layers: BTreeSet<String> = c.layers.iter().map(|l| l.layer_id.clone()).collect();
    e.comp.compositions.retain(|c| c.composition_id != id);
    if let Some(m) = e.motion.as_mut() {
        // The records of the composition go with it; a queue item that names it stays, and Folio refuses.
        m.shape_layers.retain(|s| !layers.contains(&s.layer_id));
        m.text_animators.retain(|t| !layers.contains(&t.layer_id));
        m.composition_audio.retain(|a| a.composition_id != id);
        m.composition_settings.retain(|s| s.composition_id != id);
        m.composition_work_areas.retain(|w| w.composition_id != id);
    }
    Ok(())
}

fn composition_read(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    match opt!(p, "composition_id", String)? {
        Some(id) => {
            let c = e.composition_mut(&id)?.clone();
            e.say("composition", json!(c));
        }
        None => {
            let ids: Vec<&str> = e.comp.compositions.iter().map(|c| c.composition_id.as_str()).collect();
            let ids = serde_json::to_value(ids).unwrap_or(Value::Null);
            e.detail.insert("composition_ids".to_string(), ids);
        }
    }
    let motion = e.motion.clone();
    e.say("motion", json!(motion));
    Ok(())
}

fn work_area(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let id = p.text("composition_id")?;
    e.composition_mut(&id)?;
    let area = match (opt!(p, "start_tick", u64)?, opt!(p, "end_tick", u64)?) {
        (None, None) => None,
        (Some(start_tick), Some(end_tick)) => Some(StudioCompositionWorkArea { composition_id: id.clone(), start_tick, end_tick }),
        _ => return no("parameter_missing", "start_tick"),
    };
    let m = e.motion_mut();
    m.composition_work_areas.retain(|w| w.composition_id != id);
    m.composition_work_areas.extend(area);
    Ok(())
}

/// The composition's motion settings, created with the STU-MOT-004 defaults.
fn settings_mut<'m>(m: &'m mut StudioMotionRecords, id: &str) -> R<&'m mut StudioCompositionMotionSettings> {
    if !m.composition_settings.iter().any(|s| s.composition_id == id) {
        m.composition_settings.push(StudioCompositionMotionSettings { composition_id: id.to_string(), motion_blur: false, shutter_angle: 180.0, shutter_phase: 0.0, samples_per_frame: 16, adaptive_sample_limit: 128, motion_blur_layers: Vec::new() });
    }
    match m.composition_settings.iter_mut().find(|s| s.composition_id == id) {
        Some(s) => Ok(s),
        None => no("composition_not_found", id),
    }
}

fn motion_blur(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let id = p.text("composition_id")?;
    e.composition_mut(&id)?;
    let row = e.row;
    let s = settings_mut(e.motion_mut(), &id)?;
    *s = patched!(p, row, &["composition_id"], s)?;
    Ok(())
}

fn audio_add(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let id = p.text("composition_id")?;
    e.composition_mut(&id)?;
    let layer = StudioAudioLayer { audio_id: req!(p, "audio_id")?, start_tick: opt!(p, "start_tick")?.unwrap_or(0), gain: opt!(p, "gain")?.unwrap_or(1.0) };
    let m = e.motion_mut();
    if !m.composition_audio.iter().any(|a| a.composition_id == id) {
        m.composition_audio.push(StudioCompositionAudio { composition_id: id.clone(), layers: Vec::new() });
    }
    if let Some(a) = m.composition_audio.iter_mut().find(|a| a.composition_id == id) {
        a.layers.push(layer);
    }
    Ok(())
}

fn audio_remove(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let (id, audio) = (p.text("composition_id")?, p.text("audio_id")?);
    let m = e.motion_mut();
    let Some(a) = m.composition_audio.iter_mut().find(|a| a.composition_id == id) else {
        return no("audio_layer_not_found", &audio);
    };
    let n = a.layers.len();
    a.layers.retain(|l| l.audio_id != audio);
    if a.layers.len() == n {
        return no("audio_layer_not_found", &audio);
    }
    m.composition_audio.retain(|a| !a.layers.is_empty());
    Ok(())
}

fn default_module(item: &str, module: &str, format: &str) -> R<StudioOutputModule> {
    let (ext, codec) = match format {
        "mov_vc3" => ("mov", "dnxhr_hq"),
        "mov_av1" => ("mov", "64"),
        "mp4_av1" => ("mp4", "64"),
        _ => return no("parameter_invalid", "format"),
    };
    serde_json::from_value(json!({"module_id": module, "format": format, "codec_option": codec, "include_project_link": false, "post_render_action": "none",
        "include_source_metadata": false, "channels": "rgb", "depth": 8, "color": "premultiplied", "starting_number": 0, "use_comp_frame_number": false,
        "audio": {"output": "off", "sample_rate": 48000, "bit_depth": 32, "float": true, "channels": 2},
        "colour": {"preserve_rgb": false, "output_color_space": "rec709", "embed_color_space": true},
        "output_path": format!("renders/{item}-{module}.{ext}")}))
    .map_err(|_| No("parameter_invalid", "output_module".to_string()))
}

fn queue_add(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let composition_id = p.text("composition_id")?;
    let used = e.motion.as_ref().map_or(0, |m| m.render_queue.len());
    let item_id = match opt!(p, "item_id", String)? {
        Some(id) => id,
        None => format!("rq{}", used.saturating_add(1)),
    };
    let module_id = opt!(p, "module_id", String)?.unwrap_or_else(|| "master".to_string());
    let format: Option<String> = opt!(p, "format")?;
    let mut module = default_module(&item_id, &module_id, format.as_deref().unwrap_or("mov_vc3"))?;
    if let Some(path) = opt!(p, "output_path")? {
        module.output_path = path;
    }
    let settings: StudioRenderSettings = serde_json::from_value(json!({"quality": "current_settings", "resolution": "current_settings", "disk_cache": "current_settings", "proxy_use": "current_settings",
        "effects": "current_settings", "solo_switches": "current_settings", "guide_layers": "current_settings", "color_depth": "current_settings", "frame_blending": "current_settings",
        "field_render": "off", "pulldown_3_2": "off", "motion_blur": "current_settings", "time_span": "length_of_comp"}))
    .map_err(|_| No("parameter_invalid", "settings".to_string()))?;
    let m = e.motion_mut();
    if m.render_queue.iter().any(|q| q.item_id == item_id) {
        return no("duplicate_identity", &item_id);
    }
    m.render_queue.push(StudioRenderQueueItem { item_id: item_id.clone(), composition_id, settings, output_modules: vec![module] });
    e.say("item_id", json!(&item_id));
    Ok(())
}

fn queue_item<'e>(e: &'e mut Edit<'_>, id: &str) -> R<&'e mut StudioRenderQueueItem> {
    match e.motion_mut().render_queue.iter_mut().find(|q| q.item_id == id) {
        Some(q) => Ok(q),
        None => no("queue_item_not_found", id),
    }
}

// ----- layers ------------------------------------------------------------------------------------

fn layer_add(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let comp_id = p.text("composition_id")?;
    let kind = p.text("kind")?;
    let (duration, width, stack) = {
        let c = e.composition_mut(&comp_id)?;
        (c.duration, c.width, c.layers.len())
    };
    let wanted: Option<String> = opt!(p, "layer_id")?;
    let id = match kind.as_str() {
        "existing" => match wanted {
            Some(id) => id,
            None => return no("parameter_missing", "layer_id"),
        },
        "null" | "shape" | "camera" | "light" | "precomp" => e.new_layer_id(wanted)?,
        other => return no("layer_kind_unsupported", other),
    };
    let at: usize = opt!(p, "at")?.unwrap_or(0);
    if at > stack {
        return no("parameter_invalid", "at");
    }
    let mut layer = StudioCompositionLayer {
        layer_id: id.clone(),
        timing: StudioLayerTiming { in_point: opt!(p, "in_point")?.unwrap_or(0), out_point: opt!(p, "out_point")?.unwrap_or(duration), start_time: opt!(p, "start_time")?.unwrap_or(0), stretch: StudioRational { numerator: 1, denominator: 1 } },
        parent: None,
        precomp: None,
        track_matte: None,
        streams: Vec::new(),
        corner_pin: None,
        masks: Vec::new(),
        three_d: None,
        camera: None,
        light: None,
        time_remap: None,
        frame_blending: StudioFrameBlending::default(),
        roto: None,
        effects: Vec::new(),
    };
    let options: Map<String, Value> = opt!(p, "options")?.unwrap_or_default();
    let name: Option<String> = opt!(p, "name")?;
    let members: Vec<(&str, &Value)> = options.iter().map(|(k, v)| (k.as_str(), v)).collect();
    match kind.as_str() {
        "existing" => {}
        "null" => e.create_layer(&id, name.as_deref().unwrap_or("Null"), true)?,
        "shape" => {
            e.create_layer(&id, name.as_deref().unwrap_or("Shape"), false)?;
            let root: StudioShapeGroup = opt!(p, "root")?.unwrap_or_default();
            e.motion_mut().shape_layers.push(StudioShapeLayer { layer_id: id.clone(), root, streams: Vec::new() });
        }
        "camera" => {
            e.create_layer(&id, name.as_deref().unwrap_or("Camera"), true)?;
            let zoom = folio::camera_zoom_of_angle(DEFAULT_CAMERA_ANGLE, f64::from(width));
            let camera = StudioCameraOptions { zoom: StudioKeyedValue::constant(vec![zoom]), point_of_interest: None, depth_of_field: false, iris_shape: folio::StudioIrisShape::default(), values: BTreeMap::new() };
            layer.camera = Some(overlay!(&camera, &members)?);
            layer.three_d = Some(StudioLayer3d::default());
        }
        "light" => {
            e.create_layer(&id, name.as_deref().unwrap_or("Light"), true)?;
            let light = StudioLightOptions {
                light_type: folio::StudioLightType::Point,
                colour: StudioKeyedValue::constant(vec![1.0, 1.0, 1.0]),
                colour_profile_id: None,
                intensity: StudioKeyedValue::constant(vec![100.0]),
                falloff_type: folio::StudioLightFalloff::default(),
                casts_shadows: false,
                background_visible: false,
                environment_atom: None,
                point_of_interest: None,
                values: BTreeMap::new(),
            };
            layer.light = Some(overlay!(&light, &members)?);
            layer.three_d = Some(StudioLayer3d::default());
        }
        _ => {
            e.create_layer(&id, name.as_deref().unwrap_or("Pre-comp"), true)?;
            layer.precomp = Some(StudioPrecompSource { composition_id: req!(p, "source_composition_id")?, collapse_transformations: opt!(p, "collapse_transformations")?.unwrap_or(false) });
        }
    }
    e.composition_mut(&comp_id)?.layers.insert(at, layer);
    e.say("layer_id", json!(&id));
    Ok(())
}

/// Takes a layer out of its composition and out of every record naming it. Its document layer stays.
fn remove_layer_records(e: &mut Edit<'_>, id: &str) -> R<()> {
    let owner = e.composition_of(id)?.composition_id.clone();
    let c = e.composition_mut(&owner)?;
    c.layers.retain(|l| l.layer_id != id);
    for l in &mut c.layers {
        if l.parent.as_deref() == Some(id) {
            l.parent = None;
        }
        if l.track_matte.as_ref().is_some_and(|m| m.source_layer_id == id) {
            l.track_matte = None;
        }
    }
    if let Some(m) = e.motion.as_mut() {
        m.shape_layers.retain(|s| s.layer_id != id);
        m.text_animators.retain(|t| t.layer_id != id);
        for s in &mut m.composition_settings {
            s.motion_blur_layers.retain(|l| l != id);
        }
    }
    Ok(())
}

fn layer_duplicate(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let id = p.text("layer_id")?;
    let wanted: Option<String> = opt!(p, "new_layer_id")?;
    let new_id = e.new_layer_id(wanted)?;
    let owner = e.composition_of(&id)?.composition_id.clone();
    let Some(source) = e.doc.layers.iter().find(|l| l.layer_id == id) else {
        return no("layer_not_in_composition", &id);
    };
    let group = match &source.payload {
        folio::Payload::Group => true,
        folio::Payload::Raster { tiles } if tiles.is_empty() => false,
        // A content layer's tiles, paths or story bind to its id: a copy is a content operation.
        _ => return no("layer_duplicate_unsupported", &id),
    };
    let name: Option<String> = opt!(p, "name")?;
    let source_name = source.name.clone();
    e.create_layer(&new_id, name.as_deref().unwrap_or(&source_name), group)?;
    let c = e.composition_mut(&owner)?;
    let Some(at) = c.layers.iter().position(|l| l.layer_id == id) else {
        return no("layer_not_in_composition", &id);
    };
    let mut copy = c.layers[at].clone();
    copy.layer_id.clone_from(&new_id);
    c.layers.insert(at, copy);
    if let Some(m) = e.motion.as_mut()
        && let Some(s) = m.shape_layer(&id).cloned()
    {
        m.shape_layers.push(StudioShapeLayer { layer_id: new_id.clone(), ..s });
    }
    e.say("layer_id", json!(&new_id));
    Ok(())
}

fn switches(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let id = p.text("layer_id")?;
    let owner = e.composition_of(&id)?.composition_id.clone();
    let collapse: Option<bool> = opt!(p, "collapse_transformations")?;
    let blending: Option<StudioFrameBlending> = opt!(p, "frame_blending")?;
    let three_d: Option<bool> = opt!(p, "three_d")?;
    let blur: Option<bool> = opt!(p, "motion_blur")?;
    let l = e.layer_mut(&id)?;
    if let Some(on) = collapse {
        match l.precomp.as_mut() {
            Some(pre) => pre.collapse_transformations = on,
            None => return no("layer_not_precomp", &id),
        }
    }
    if let Some(b) = blending {
        l.frame_blending = b;
    }
    match three_d {
        Some(true) if l.three_d.is_none() => l.three_d = Some(StudioLayer3d::default()),
        Some(false) => l.three_d = None,
        _ => {}
    }
    if let Some(on) = blur {
        let s = settings_mut(e.motion_mut(), &owner)?;
        s.motion_blur_layers.retain(|x| *x != id);
        if on {
            s.motion_blur_layers.push(id);
        }
    }
    Ok(())
}

fn precompose(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let ids: Vec<String> = req!(p, "layer_ids")?;
    let name = p.text("name")?;
    let moved: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
    if ids.is_empty() || moved.len() != ids.len() {
        return no("parameter_invalid", "layer_ids");
    }
    let owner = e.composition_of(&ids[0])?.composition_id.clone();
    for id in &ids {
        if e.composition_of(id)?.composition_id != owner {
            return no("layers_not_in_one_composition", id);
        }
    }
    let wanted: Option<String> = opt!(p, "new_composition_id")?;
    let new_id = e.new_composition_id(wanted)?;
    let wanted: Option<String> = opt!(p, "layer_id")?;
    let outer_id = e.new_layer_id(wanted)?;
    let inside = |id: &str| moved.contains(id);
    let c = e.composition_mut(&owner)?;
    for l in &c.layers {
        let links = l.parent.iter().map(String::as_str).chain(l.track_matte.iter().map(|m| m.source_layer_id.as_str()));
        for link in links {
            if inside(&l.layer_id) != inside(link) {
                // Studio reading: a link across the selection would change the render, so it is refused.
                return no("precompose_crosses_selection", &l.layer_id);
            }
        }
    }
    let top = c.layers.iter().position(|l| inside(&l.layer_id)).unwrap_or(0);
    let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut c.layers).into_iter().partition(|l| inside(&l.layer_id));
    c.layers = kept;
    let (width, height, pixel_aspect, frame_rate, duration) = (c.width, c.height, c.pixel_aspect, c.frame_rate, c.duration);
    c.layers.insert(top.min(c.layers.len()), StudioCompositionLayer {
        layer_id: outer_id.clone(),
        timing: StudioLayerTiming { in_point: 0, out_point: duration, start_time: 0, stretch: StudioRational { numerator: 1, denominator: 1 } },
        parent: None,
        precomp: Some(StudioPrecompSource { composition_id: new_id.clone(), collapse_transformations: false }),
        track_matte: None,
        streams: Vec::new(),
        corner_pin: None,
        masks: Vec::new(),
        three_d: None,
        camera: None,
        light: None,
        time_remap: None,
        frame_blending: StudioFrameBlending::default(),
        roto: None,
        effects: Vec::new(),
    });
    e.comp.compositions.push(StudioComposition { schema_id: CompositionSchema::V1, composition_id: new_id.clone(), name: name.clone(), width, height, pixel_aspect, frame_rate, duration, background: None, immersive: None, layers: taken });
    // The per-layer motion blur switch follows its layer, under the same shutter.
    if let Some(m) = e.motion.as_mut()
        && let Some(s) = m.composition_settings.iter_mut().find(|s| s.composition_id == owner)
    {
        let (out, stay): (Vec<_>, Vec<_>) = std::mem::take(&mut s.motion_blur_layers).into_iter().partition(|l| moved.contains(l.as_str()));
        s.motion_blur_layers = stay;
        if !out.is_empty() {
            let inner = StudioCompositionMotionSettings { composition_id: new_id.clone(), motion_blur_layers: out, ..s.clone() };
            m.composition_settings.push(inner);
        }
    }
    e.create_layer(&outer_id, &name, true)?;
    e.say("composition_id", json!(&new_id));
    e.say("layer_id", json!(&outer_id));
    Ok(())
}

fn time_remap(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let id = p.text("layer_id")?;
    let enabled: bool = req!(p, "enabled")?;
    let l = e.layer_mut(&id)?;
    if !enabled {
        l.time_remap = None;
        return Ok(());
    }
    if l.time_remap.is_some() {
        return no("nothing_to_change", &id);
    }
    // Enabling keeps the mapping: source seconds at the window's two ends, linear between them.
    let seconds = |tick: u64| l.timing.source_tick(tick).map(|t| t as f64 / TICKS_PER_SECOND as f64);
    let (Some(a), Some(b)) = (seconds(l.timing.in_point), seconds(l.timing.out_point)) else {
        return no("tick_overflow", &id);
    };
    let keys = vec![linear_key(l.timing.in_point, vec![a]), linear_key(l.timing.out_point, vec![b])];
    l.time_remap = Some(StudioKeyedValue { static_value: vec![a], keyframes: keys });
    Ok(())
}

fn mask_mut<'l>(l: &'l mut StudioCompositionLayer, id: &str) -> R<&'l mut StudioCompositionMask> {
    match l.masks.iter_mut().find(|m| m.mask_id == id) {
        Some(m) => Ok(m),
        None => no("mask_not_found", id),
    }
}

fn mask_add(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let id = p.text("layer_id")?;
    let path: Vec<f64> = req!(p, "path")?;
    let wanted: Option<String> = opt!(p, "mask_id")?;
    let mode = opt!(p, "mode")?.unwrap_or(folio::StudioCompositionMaskMode::Add);
    let (inverted, closed) = (opt!(p, "inverted")?.unwrap_or(false), opt!(p, "closed")?.unwrap_or(true));
    let l = e.layer_mut(&id)?;
    let mask_id = wanted.unwrap_or_else(|| format!("mask-{}", l.masks.len().saturating_add(1)));
    l.masks.push(StudioCompositionMask { mask_id: mask_id.clone(), mode, inverted, closed, static_value: path, keyframes: Vec::new(), feather: None, opacity: None, expansion: None });
    e.say("mask_id", json!(&mask_id));
    Ok(())
}

fn effect_add(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let id = p.text("layer_id")?;
    let row = e.row;
    let kind: folio::StudioCompositionEffectKind = req!(p, "filter_kind")?;
    let wanted: Option<String> = opt!(p, "effect_id")?;
    let at: Option<usize> = opt!(p, "at")?;
    let values: BTreeMap<String, Vec<f64>> = opt!(p, "values")?.unwrap_or_default();
    let l = e.layer_mut(&id)?;
    let effect_id = wanted.unwrap_or_else(|| format!("fx-{}", l.effects.len().saturating_add(1)));
    if l.effects.iter().any(|x| x.effect_id == effect_id) {
        return no("duplicate_identity", &effect_id);
    }
    let start: StudioCompositionEffect = serde_json::from_value(json!({"effect_id": effect_id, "filter_kind": kind})).map_err(|_| No("parameter_invalid", "filter_kind".to_string()))?;
    let mut effect: StudioCompositionEffect = patched!(p, row, &["layer_id", "effect_id", "filter_kind", "values", "at"], &start)?;
    effect.values = values.into_iter().map(|(k, v)| (k, StudioKeyedValue::constant(v))).collect();
    let index = at.unwrap_or(l.effects.len());
    if index > l.effects.len() {
        return no("parameter_invalid", "at");
    }
    l.effects.insert(index, effect);
    e.say("effect_id", json!(&effect_id));
    Ok(())
}

fn effect_set(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let (id, fx) = (p.text("layer_id")?, p.text("effect_id")?);
    let row = e.row;
    if p.has("filter_kind") {
        return no("parameter_invalid", "filter_kind");
    }
    let at: Option<usize> = opt!(p, "at")?;
    let values: Option<BTreeMap<String, Vec<f64>>> = opt!(p, "values")?;
    let l = e.layer_mut(&id)?;
    let Some(index) = l.effects.iter().position(|x| x.effect_id == fx) else {
        return no("effect_not_found", &fx);
    };
    let mut effect: StudioCompositionEffect = patched!(p, row, &["layer_id", "effect_id", "filter_kind", "values", "at"], &l.effects[index])?;
    for (k, v) in values.into_iter().flatten() {
        effect.values.insert(k, StudioKeyedValue::constant(v));
    }
    l.effects.remove(index);
    let to = at.unwrap_or(index);
    if to > l.effects.len() {
        return no("parameter_invalid", "at");
    }
    l.effects.insert(to, effect);
    Ok(())
}

fn corner_pin(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let id = p.text("layer_id")?;
    if opt!(p, "clear", bool)?.unwrap_or(false) {
        e.layer_mut(&id)?.corner_pin = None;
        return Ok(());
    }
    let from: [[f64; 2]; 4] = req!(p, "from")?;
    let to: Option<Vec<f64>> = opt!(p, "to")?;
    let l = e.layer_mut(&id)?;
    let static_value = to.unwrap_or_else(|| from.iter().flatten().copied().collect());
    let keyframes = l.corner_pin.take().map(|c| c.keyframes).unwrap_or_default();
    l.corner_pin = Some(StudioCornerPin { from, static_value, keyframes });
    Ok(())
}

// ----- shape items and text animators ------------------------------------------------------------

fn shape_layer_mut<'e>(e: &'e mut Edit<'_>, id: &str) -> R<&'e mut StudioShapeLayer> {
    match e.motion_mut().shape_layers.iter_mut().find(|s| s.layer_id == id) {
        Some(s) => Ok(s),
        None => no("layer_not_shape", id),
    }
}

fn group_mut<'g>(root: &'g mut StudioShapeGroup, path: &[u32]) -> Option<&'g mut StudioShapeGroup> {
    let mut g = root;
    for i in path {
        match g.items.get_mut(usize::try_from(*i).ok()?)? {
            StudioShapeItem::Group { group } => g = group,
            _ => return None,
        }
    }
    Some(g)
}

fn shape_item_mut<'g>(root: &'g mut StudioShapeGroup, path: &[u32]) -> Option<&'g mut StudioShapeItem> {
    let (last, parents) = path.split_last()?;
    group_mut(root, parents)?.items.get_mut(usize::try_from(*last).ok()?)
}

fn shape_add(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let id = p.text("layer_id")?;
    let path: Vec<u32> = opt!(p, "group_path")?.unwrap_or_default();
    let item: StudioShapeItem = req!(p, "item")?;
    let shape = shape_layer_mut(e, &id)?;
    let Some(group) = group_mut(&mut shape.root, &path) else {
        return no("shape_item_not_found", &id);
    };
    group.items.push(item);
    Ok(())
}

fn shape_remove(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let id = p.text("layer_id")?;
    let path: Vec<u32> = req!(p, "path")?;
    let Some((last, parents)) = path.split_last() else {
        return no("parameter_invalid", "path");
    };
    let shape = shape_layer_mut(e, &id)?;
    // A stream on the item or inside it would dangle; later siblings' streams follow the shift.
    if shape.streams.iter().any(|s| s.path.starts_with(&path)) {
        return no("shape_item_animated", &id);
    }
    let Some(group) = group_mut(&mut shape.root, parents) else {
        return no("shape_item_not_found", &id);
    };
    let Some(index) = usize::try_from(*last).ok().filter(|i| *i < group.items.len()) else {
        return no("shape_item_not_found", &id);
    };
    group.items.remove(index);
    for s in &mut shape.streams {
        if s.path.len() > parents.len() && s.path.starts_with(parents) && s.path[parents.len()] > *last {
            s.path[parents.len()] = s.path[parents.len()].saturating_sub(1);
        }
    }
    Ok(())
}

fn text_of<'e>(e: &'e mut Edit<'_>, id: &str) -> R<&'e mut StudioTextAnimators> {
    match e.motion_mut().text_animators.iter_mut().find(|t| t.layer_id == id) {
        Some(t) => Ok(t),
        None => no("layer_not_text_animated", id),
    }
}

fn animator_mut(t: &mut StudioTextAnimators, index: usize) -> R<&mut StudioTextAnimator> {
    let layer = t.layer_id.clone();
    match t.animators.get_mut(index) {
        Some(a) => Ok(a),
        None => no("property_not_found", &layer),
    }
}

// ----- property addresses ------------------------------------------------------------------------

type Slot<'a> = (&'a mut Vec<f64>, &'a mut Vec<Key>);

fn keyed(v: &mut StudioKeyedValue) -> Slot<'_> {
    (&mut v.static_value, &mut v.keyframes)
}

/// The keyed value of an optional member, created with `default` (or the caller's value). `None` when
/// the member is absent and has no knowable default.
fn member<'v>(v: &'v mut Option<StudioKeyedValue>, default: Option<&[f64]>, hint: Option<&[f64]>, prop: &str) -> R<Slot<'v>> {
    if v.is_none() {
        let Some(start) = default.or(hint) else {
            return no("property_default_unknown", prop);
        };
        *v = Some(StudioKeyedValue::constant(start.to_vec()));
    }
    match v.as_mut() {
        Some(k) => Ok(keyed(k)),
        None => no("property_not_found", prop),
    }
}

/// The value in a by-name map (material, camera, light), created with its authored default.
fn named<'m>(m: &'m mut BTreeMap<String, StudioKeyedValue>, name: &str, prop: &str) -> R<Slot<'m>> {
    let Some(bound) = folio::bound(name) else {
        return no("property_not_found", prop);
    };
    let v = m.entry(name.to_string()).or_insert_with(|| StudioKeyedValue::constant(bound.default.to_vec()));
    Ok(keyed(v))
}

/// Resolves a property address (module docs) to its static value and keys, and whether the property
/// interpolates. A member the record omits is created at its implied value, so that creating it
/// changes no render and disabling animation returns to it ([STU-MOT-020b]); `hint` is the caller's
/// value where no implied value is knowable.
fn slot<'e>(e: &'e mut Edit<'_>, layer_id: &str, prop: &str, hint: Option<&[f64]>) -> R<(Slot<'e>, bool)> {
    if prop.is_empty() || prop.len() > MAX_ADDRESS {
        return no("parameter_invalid", "property");
    }
    let parts: Vec<&str> = prop.split('.').collect();
    match parts.as_slice() {
        ["shape", rest @ ..] => {
            e.layer_mut(layer_id)?;
            return Ok((shape_slot(e, layer_id, rest, prop)?, true));
        }
        ["text", rest @ ..] => {
            e.layer_mut(layer_id)?;
            return Ok((text_slot(e, layer_id, rest, hint, prop)?, true));
        }
        _ => {}
    }
    let l = e.layer_mut(layer_id)?;
    let mut interpolable = true;
    let out = match parts.as_slice() {
        [name @ ("anchor" | "position" | "scale" | "rotation" | "opacity")] => {
            let (property, default): (StudioLayerProperty, Option<&[f64]>) = match *name {
                "anchor" => (StudioLayerProperty::Anchor, None),
                "position" => (StudioLayerProperty::Position, None),
                "scale" => (StudioLayerProperty::Scale, Some(&[100.0, 100.0])),
                "rotation" => (StudioLayerProperty::Rotation, Some(&[0.0])),
                _ => (StudioLayerProperty::Opacity, Some(&[100.0])),
            };
            let at = match l.streams.iter().position(|s| s.property == property) {
                Some(at) => at,
                None => {
                    let Some(start) = default.or(hint) else {
                        // The implied anchor and position depend on the content bounds.
                        return no("property_default_unknown", prop);
                    };
                    l.streams.push(StudioPropertyStream { property, static_value: start.to_vec(), keyframes: Vec::new() });
                    l.streams.len().saturating_sub(1)
                }
            };
            let s = &mut l.streams[at];
            (&mut s.static_value, &mut s.keyframes)
        }
        ["time_remap"] => match l.time_remap.as_mut() {
            Some(v) => keyed(v),
            None => return no("property_not_found", prop),
        },
        ["corner_pin"] => match l.corner_pin.as_mut() {
            Some(c) => (&mut c.static_value, &mut c.keyframes),
            None => return no("property_not_found", prop),
        },
        ["three_d", rest @ ..] => {
            let Some(d) = l.three_d.as_mut() else {
                return no("layer_not_3d", layer_id);
            };
            match rest {
                ["position_z"] => member(&mut d.position_z, Some(&[0.0]), hint, prop)?,
                ["anchor_z"] => member(&mut d.anchor_z, Some(&[0.0]), hint, prop)?,
                ["rotation_x"] => member(&mut d.rotation_x, Some(&[0.0]), hint, prop)?,
                ["rotation_y"] => member(&mut d.rotation_y, Some(&[0.0]), hint, prop)?,
                ["orientation"] => member(&mut d.orientation, Some(&[0.0, 0.0, 0.0]), hint, prop)?,
                ["material", name] => named(&mut d.material.values, name, prop)?,
                _ => return no("property_not_found", prop),
            }
        }
        ["camera", name] => {
            let Some(c) = l.camera.as_mut() else {
                return no("layer_not_camera", layer_id);
            };
            match *name {
                "zoom" => keyed(&mut c.zoom),
                // A point of interest makes the camera two-node: only with the caller's value.
                "point_of_interest" => member(&mut c.point_of_interest, None, hint, prop)?,
                other => named(&mut c.values, other, prop)?,
            }
        }
        ["light", name] => {
            let Some(c) = l.light.as_mut() else {
                return no("layer_not_light", layer_id);
            };
            match *name {
                "colour" => keyed(&mut c.colour),
                "intensity" => keyed(&mut c.intensity),
                "point_of_interest" => member(&mut c.point_of_interest, None, hint, prop)?,
                other => named(&mut c.values, other, prop)?,
            }
        }
        ["effect", id, key] => {
            let Some(fx) = l.effects.iter_mut().find(|x| x.effect_id == *id) else {
                return no("property_not_found", prop);
            };
            let Some(spec) = fx.filter_kind.param(key) else {
                return no("property_not_found", prop);
            };
            interpolable = spec.interpolable;
            let v = fx.values.entry((*key).to_string()).or_insert_with(|| StudioKeyedValue::constant(spec.default.to_vec()));
            keyed(v)
        }
        ["mask", id, member_name] => {
            let Some(m) = l.masks.iter_mut().find(|m| m.mask_id == *id) else {
                return no("property_not_found", prop);
            };
            match *member_name {
                "path" => (&mut m.static_value, &mut m.keyframes),
                "feather" => member(&mut m.feather, Some(&[0.0, 0.0]), hint, prop)?,
                "opacity" => member(&mut m.opacity, Some(&[1.0]), hint, prop)?,
                "expansion" => member(&mut m.expansion, Some(&[0.0]), hint, prop)?,
                _ => return no("property_not_found", prop),
            }
        }
        _ => return no("property_not_found", prop),
    };
    Ok((out, interpolable))
}

fn shape_slot<'e>(e: &'e mut Edit<'_>, id: &str, rest: &[&str], prop: &str) -> R<Slot<'e>> {
    let Some((field, indices)) = rest.split_last() else {
        return no("property_not_found", prop);
    };
    let Some(field) = serde_json::from_value::<StudioShapeField>(Value::String((*field).to_string())).ok() else {
        return no("property_not_found", prop);
    };
    let mut path = Vec::with_capacity(indices.len());
    for i in indices {
        let Ok(i) = i.parse::<u32>() else {
            return no("property_not_found", prop);
        };
        path.push(i);
    }
    let shape = shape_layer_mut(e, id)?;
    let at = match shape.streams.iter().position(|s| s.field == field && s.path == path) {
        Some(at) => at,
        None => {
            let Some(base) = shape_base(&shape.root, &path, field) else {
                return no("property_not_found", prop);
            };
            shape.streams.push(StudioShapeStream { path, field, value: StudioKeyedValue::constant(base) });
            shape.streams.len().saturating_sub(1)
        }
    };
    Ok(keyed(&mut shape.streams[at].value))
}

/// The authored value of a shape field; `None` when the field does not fit the address.
fn shape_base(root: &StudioShapeGroup, path: &[u32], field: StudioShapeField) -> Option<Vec<f64>> {
    use StudioShapeField as F;
    use StudioShapeItem as I;
    if matches!(field, F::GroupPosition | F::GroupScale | F::GroupRotation | F::GroupOpacity) {
        let g = if path.is_empty() {
            root
        } else {
            match folio::shape_item_at(root, path)? {
                I::Group { group } => group,
                _ => return None,
            }
        };
        let t = &g.transform;
        return Some(match field {
            F::GroupPosition => t.position.to_vec(),
            F::GroupScale => t.scale.to_vec(),
            F::GroupRotation => vec![t.rotation],
            _ => vec![t.opacity],
        });
    }
    Some(match (folio::shape_item_at(root, path)?, field) {
        (I::Trim { start, .. }, F::TrimStart) => vec![*start],
        (I::Trim { end, .. }, F::TrimEnd) => vec![*end],
        (I::Trim { offset, .. }, F::TrimOffset) => vec![*offset],
        (I::Repeater { copies, .. }, F::RepeaterCopies) => vec![*copies],
        (I::Repeater { offset, .. }, F::RepeaterOffset) => vec![*offset],
        (I::Repeater { rotation, .. }, F::RepeaterRotation) => vec![*rotation],
        (I::Offset { amount, .. }, F::OffsetAmount) => vec![*amount],
        (I::PuckerBloat { amount }, F::PuckerAmount) => vec![*amount],
        (I::RoundCorners { radius }, F::RoundRadius) => vec![*radius],
        (I::Twist { angle, .. }, F::TwistAngle) => vec![*angle],
        (I::ZigZag { size, .. }, F::ZigZagSize) => vec![*size],
        (I::Rect { size, .. }, F::RectSize) => size.to_vec(),
        (I::Rect { position, .. }, F::RectPosition) => position.to_vec(),
        (I::Rect { roundness, .. }, F::RectRoundness) => vec![*roundness],
        (I::Ellipse { size, .. }, F::EllipseSize) => size.to_vec(),
        (I::Ellipse { position, .. }, F::EllipsePosition) => position.to_vec(),
        (I::Polystar { outer_radius, .. }, F::PolystarOuterRadius) => vec![*outer_radius],
        (I::Polystar { rotation, .. }, F::PolystarRotation) => vec![*rotation],
        (I::Fill { opacity, .. } | I::GradientFill { opacity, .. }, F::FillOpacity) => vec![*opacity],
        (I::Stroke { style, .. } | I::GradientStroke { style, .. }, F::StrokeWidth) => vec![style.width],
        (I::Stroke { opacity, .. } | I::GradientStroke { opacity, .. }, F::StrokeOpacity) => vec![*opacity],
        _ => return None,
    })
}

fn text_slot<'e>(e: &'e mut Edit<'_>, id: &str, rest: &[&str], hint: Option<&[f64]>, prop: &str) -> R<Slot<'e>> {
    let (animator, selector, param) = match rest {
        [a, s, param] => (a.parse::<u32>().ok(), s.parse::<u32>().ok(), *param),
        [a, param] => (a.parse::<u32>().ok(), Some(0), *param),
        _ => return no("property_not_found", prop),
    };
    let (Some(animator), Some(selector), Some(param)) = (animator, selector, serde_json::from_value::<StudioTextParam>(Value::String(param.to_string())).ok()) else {
        return no("property_not_found", prop);
    };
    if param.on_selector() != (rest.len() == 3) {
        return no("property_not_found", prop);
    }
    let t = text_of(e, id)?;
    let at = match t.streams.iter().position(|s| s.animator == animator && s.param == param && (!param.on_selector() || s.selector == selector)) {
        Some(at) => at,
        None => {
            let Some(a) = usize::try_from(animator).ok().and_then(|i| t.animators.get(i)) else {
                return no("property_not_found", prop);
            };
            // The address must fit even when the caller gives the value.
            let Some(start) = text_base(a, selector, param, prop)?.or_else(|| hint.map(<[f64]>::to_vec)) else {
                return no("property_default_unknown", prop);
            };
            t.streams.push(StudioTextStream { animator, selector, param, value: StudioKeyedValue::constant(start) });
            t.streams.len().saturating_sub(1)
        }
    };
    Ok(keyed(&mut t.streams[at].value))
}

/// The authored value of a text parameter (`None`: the property is absent, so no value is implied).
fn text_base(a: &StudioTextAnimator, selector: u32, param: StudioTextParam, prop: &str) -> R<Option<Vec<f64>>> {
    use StudioTextParam as T;
    use StudioTextSelector as S;
    if param.on_selector() {
        let sel = usize::try_from(selector).ok().and_then(|i| a.selectors.get(i));
        return Ok(Some(match (sel, param) {
            (Some(S::Range { start, .. }), T::RangeStart) => vec![*start],
            (Some(S::Range { end, .. }), T::RangeEnd) => vec![*end],
            (Some(S::Range { offset, .. }), T::RangeOffset) => vec![*offset],
            (Some(S::Range { amount, .. }), T::RangeAmount) => vec![*amount],
            (Some(S::Wiggly { max_amount, .. }), T::WigglyMax) => vec![*max_amount],
            (Some(S::Wiggly { min_amount, .. }), T::WigglyMin) => vec![*min_amount],
            (Some(S::Wiggly { wiggles_per_second, .. }), T::WigglyFrequency) => vec![*wiggles_per_second],
            _ => return no("property_not_found", prop),
        }));
    }
    let p = &a.properties;
    Ok(match param {
        T::Position => p.position.map(|v| v.to_vec()),
        T::Scale => p.scale.map(|v| v.to_vec()),
        T::Rotation => p.rotation.map(|v| vec![v]),
        T::Opacity => p.opacity.map(|v| vec![v]),
        T::Fill => p.fill.map(|v| v.to_vec()),
        T::Tracking => p.tracking.map(|v| vec![v]),
        _ => return no("property_not_found", prop),
    })
}

// ----- property and keyframe operations ----------------------------------------------------------

fn key_address(p: &P<'_>) -> R<(String, String, u64)> {
    Ok((p.text("layer_id")?, p.text("property")?, req!(p, "time")?))
}

fn linear_key(time: u64, value: Vec<f64>) -> Key {
    Key { time, value, in_interp: Interp::Linear, out_interp: Interp::Linear, in_tangent: Vec::new(), out_tangent: Vec::new() }
}

fn property_read(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let (id, prop) = (p.text("layer_id")?, p.text("property")?);
    let time: Option<u64> = opt!(p, "time")?;
    let ((sv, keys), interpolable) = slot(e, &id, &prop, None)?;
    let (static_value, keyframes) = (sv.clone(), keys.clone());
    if let Some(t) = time {
        let v = value_at(&static_value, &keyframes, t, interpolable);
        e.say("value_at", json!(v));
    }
    e.say("static_value", json!(static_value));
    e.say("keyframes", json!(keyframes));
    Ok(())
}

fn property_set(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let (id, prop) = (p.text("layer_id")?, p.text("property")?);
    let value: Vec<f64> = req!(p, "value")?;
    let time: Option<u64> = opt!(p, "time")?;
    let ((sv, keys), _) = slot(e, &id, &prop, Some(&value))?;
    if keys.is_empty() {
        *sv = value;
        return Ok(());
    }
    // An animated property takes the value as a key at the time (the static value stays retained).
    let Some(t) = time else {
        return no("property_animated_needs_time", &prop);
    };
    match keys.binary_search_by_key(&t, |k| k.time) {
        Ok(at) => keys[at].value = value,
        Err(at) => keys.insert(at, linear_key(t, value)),
    }
    Ok(())
}

fn add_at_current(sv: &[f64], keys: &mut Vec<Key>, time: u64, interpolable: bool, value: Option<Vec<f64>>) -> R<()> {
    if keys.binary_search_by_key(&time, |k| k.time).is_ok() {
        return no("keyframe_exists", "time");
    }
    add_preserving(sv, keys, time, interpolable);
    if keys.len() == 1
        && let Some(v) = value
    {
        // The caller's value seeds a property without a knowable implied value.
        keys[0].value = v;
    }
    Ok(())
}

fn default_tangent() -> Vec<StudioTemporalTangent> {
    vec![StudioTemporalTangent { speed: 0.0, influence: DEFAULT_INFLUENCE }]
}

fn apply_interpolation(key: &mut Key, in_interp: Option<Interp>, out_interp: Option<Interp>, in_tangent: Option<Vec<StudioTemporalTangent>>, out_tangent: Option<Vec<StudioTemporalTangent>>) {
    if let Some(i) = in_interp {
        key.in_interp = i;
    }
    if let Some(i) = out_interp {
        key.out_interp = i;
    }
    if let Some(t) = in_tangent {
        key.in_tangent = t;
    }
    if let Some(t) = out_tangent {
        key.out_tangent = t;
    }
    if key.in_interp.is_bezier() && key.in_tangent.is_empty() {
        key.in_tangent = default_tangent();
    }
    if key.out_interp.is_bezier() && key.out_tangent.is_empty() {
        key.out_tangent = default_tangent();
    }
}

fn key_set(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let (id, prop, time) = key_address(p)?;
    let value: Vec<f64> = req!(p, "value")?;
    let (in_interp, out_interp): (Option<Interp>, Option<Interp>) = (opt!(p, "in_interp")?, opt!(p, "out_interp")?);
    let (in_tangent, out_tangent): (Option<Vec<StudioTemporalTangent>>, Option<Vec<StudioTemporalTangent>>) = (opt!(p, "in_tangent")?, opt!(p, "out_tangent")?);
    let ((_, keys), _) = slot(e, &id, &prop, Some(&value))?;
    let at = match keys.binary_search_by_key(&time, |k| k.time) {
        Ok(at) => {
            keys[at].value = value;
            at
        }
        Err(at) => {
            keys.insert(at, linear_key(time, value));
            at
        }
    };
    apply_interpolation(&mut keys[at], in_interp, out_interp, in_tangent, out_tangent);
    Ok(())
}

fn key_shift(e: &mut Edit<'_>, p: &P<'_>) -> R<()> {
    let (id, prop) = (p.text("layer_id")?, p.text("property")?);
    let frames: i64 = req!(p, "frames")?;
    let times: Option<BTreeSet<u64>> = opt!(p, "times")?;
    let tpf = e.composition_of(&id)?.frame_rate.ticks_per_frame().unwrap_or(0);
    let delta = i128::from(frames).checked_mul(i128::from(tpf)).ok_or_else(|| No("tick_overflow", bounded(&prop)))?;
    let ((_, keys), _) = slot(e, &id, &prop, None)?;
    if let Some(t) = times.iter().flatten().find(|t| keys.binary_search_by_key(*t, |k| k.time).is_err()) {
        return no("keyframe_not_found", &t.to_string());
    }
    for k in keys.iter_mut() {
        if times.as_ref().is_none_or(|ts| ts.contains(&k.time)) {
            let moved = i128::from(k.time).checked_add(delta).and_then(|t| u64::try_from(t).ok()).filter(|t| *t <= folio::MAX_COMPOSITION_TICK);
            let Some(t) = moved else {
                return no("tick_overflow", &prop);
            };
            k.time = t;
        }
    }
    keys.sort_by_key(|k| k.time);
    if keys.windows(2).any(|w| w[0].time == w[1].time) {
        return no("keyframe_exists", &prop);
    }
    Ok(())
}

// ----- the curve (motion `keyframe::shape`, module docs) -------------------------------------------

enum Curve {
    Step,
    Lerp,
    Cubic { x1: f64, x2: f64, y: [f64; 4] },
}

fn tangent_for(tangents: &[StudioTemporalTangent], component: usize) -> Option<StudioTemporalTangent> {
    match tangents {
        [] => None,
        [one] => Some(*one),
        many => many.get(component).copied(),
    }
}

fn curve(k0: &Key, k1: &Key, component: usize, dts: f64, interpolable: bool) -> Curve {
    if !interpolable || k0.out_interp == Interp::Hold {
        return Curve::Step;
    }
    let (out_bezier, in_bezier) = (k0.out_interp.is_bezier(), k1.in_interp.is_bezier());
    if !out_bezier && !in_bezier {
        return Curve::Lerp;
    }
    let (v0, v1) = (k0.value[component], k1.value[component]);
    let slope = (v1 - v0) / dts;
    let out = tangent_for(&k0.out_tangent, component).filter(|_| out_bezier);
    let arrive = tangent_for(&k1.in_tangent, component).filter(|_| in_bezier);
    let (s0, io) = out.map_or((slope, CHORD_INFLUENCE), |t| (t.speed, t.influence));
    let (s1, ii) = arrive.map_or((slope, CHORD_INFLUENCE), |t| (t.speed, t.influence));
    Curve::Cubic { x1: io, x2: 1.0 - ii, y: [v0, v0 + s0 * io * dts, v1 - s1 * ii * dts, v1] }
}

fn bezier(p: [f64; 4], s: f64) -> f64 {
    let m = 1.0 - s;
    m * m * m * p[0] + 3.0 * m * m * s * p[1] + 3.0 * m * s * s * p[2] + s * s * s * p[3]
}

fn solve(x1: f64, x2: f64, u: f64) -> f64 {
    let x = [0.0, x1, x2, 1.0];
    let (mut lo, mut hi) = (0.0_f64, 1.0_f64);
    for _ in 0..BISECTION_STEPS {
        let mid = 0.5 * (lo + hi);
        if bezier(x, mid) < u {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

fn seconds(dt: u64) -> f64 {
    dt as f64 / TICKS_PER_SECOND as f64
}

/// The segment around `tick` and the fraction of it elapsed; `None` outside the keys.
fn segment(keys: &[Key], tick: u64) -> Option<(usize, f64, f64)> {
    let after = keys.partition_point(|k| k.time <= tick);
    if after == 0 || after == keys.len() {
        return None;
    }
    let (t0, t1) = (keys[after.saturating_sub(1)].time, keys[after].time);
    let dt = t1.saturating_sub(t0);
    Some((after, tick.saturating_sub(t0) as f64 / dt as f64, seconds(dt)))
}

/// The value at `tick`: the end values hold outside the keys (motion `Property::base_value`).
fn value_at(static_value: &[f64], keys: &[Key], tick: u64, interpolable: bool) -> Vec<f64> {
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else {
        return static_value.to_vec();
    };
    if tick < first.time {
        return first.value.clone();
    }
    let Some((after, u, dts)) = segment(keys, tick) else {
        return last.value.clone();
    };
    let (k0, k1) = (&keys[after.saturating_sub(1)], &keys[after]);
    if u == 0.0 {
        return k0.value.clone();
    }
    (0..k0.value.len().min(k1.value.len()))
        .map(|c| match curve(k0, k1, c, dts, interpolable) {
            Curve::Step => k0.value[c],
            Curve::Lerp => k0.value[c] + (k1.value[c] - k0.value[c]) * u,
            Curve::Cubic { x1, x2, y } => bezier(y, solve(x1, x2, u)),
        })
        .collect()
}

/// Inserts a key at `t` (not a key time) with no sample of the property changing ([STU-MOT-038]).
fn add_preserving(static_value: &[f64], keys: &mut Vec<Key>, t: u64, interpolable: bool) {
    if keys.is_empty() {
        keys.push(linear_key(t, static_value.to_vec()));
        return;
    }
    let at = keys.partition_point(|k| k.time < t);
    if at == 0 {
        // Before the first key the value holds: a flat linear segment keeps it.
        let first = &mut keys[0];
        if first.in_interp.is_bezier() {
            first.in_interp = Interp::Linear;
            first.in_tangent.clear();
        }
        let v = first.value.clone();
        keys.insert(0, linear_key(t, v));
        return;
    }
    if at == keys.len() {
        let last = &mut keys[at.saturating_sub(1)];
        if last.out_interp.is_bezier() {
            last.out_interp = Interp::Linear;
            last.out_tangent.clear();
        }
        let v = last.value.clone();
        keys.push(linear_key(t, v));
        return;
    }
    let before = at.saturating_sub(1);
    let (k0, k1) = (keys[before].clone(), keys[at].clone());
    let Some((_, u, dts)) = segment(keys, t) else {
        return;
    };
    let dim = k0.value.len().min(k1.value.len());
    let mut key = linear_key(t, value_at(static_value, keys, t, interpolable));
    match curve(&k0, &k1, 0, dts, interpolable) {
        Curve::Step => {
            key.in_interp = Interp::Hold;
            key.out_interp = Interp::Hold;
        }
        Curve::Lerp => {}
        Curve::Cubic { .. } => {
            let (mut out0, mut in_new, mut out_new, mut in1) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            let (left, right) = (dts * u, dts * (1.0 - u));
            for c in 0..dim {
                let Curve::Cubic { x1, x2, y } = curve(&k0, &k1, c, dts, interpolable) else {
                    continue;
                };
                let s = solve(x1, x2, u);
                let (lx, rx) = split([0.0, x1, x2, 1.0], s);
                let (ly, ry) = split(y, s);
                key.value[c] = ly[3];
                // Normalised to each new segment's own time; handles to speed and influence.
                let (a1, a2) = (lx[1] / lx[3], lx[2] / lx[3]);
                let (b1, b2) = ((rx[1] - rx[0]) / (1.0 - rx[0]), (rx[2] - rx[0]) / (1.0 - rx[0]));
                out0.push(tangent(ly[1] - ly[0], a1, left));
                in_new.push(tangent(ly[3] - ly[2], 1.0 - a2, left));
                out_new.push(tangent(ry[1] - ry[0], b1, right));
                in1.push(tangent(ry[3] - ry[2], 1.0 - b2, right));
            }
            if !k0.out_interp.is_bezier() {
                keys[before].out_interp = Interp::Bezier;
            }
            if !k1.in_interp.is_bezier() {
                keys[at].in_interp = Interp::Bezier;
            }
            keys[before].out_tangent = out0;
            keys[at].in_tangent = in1;
            key.in_interp = Interp::Bezier;
            key.out_interp = Interp::Bezier;
            key.in_tangent = in_new;
            key.out_tangent = out_new;
        }
    }
    keys.insert(at, key);
}

/// De Casteljau at `s`: the left and right control points.
fn split(p: [f64; 4], s: f64) -> ([f64; 4], [f64; 4]) {
    let l = |a: f64, b: f64| a + (b - a) * s;
    let (a, b, c) = (l(p[0], p[1]), l(p[1], p[2]), l(p[2], p[3]));
    let (d, e) = (l(a, b), l(b, c));
    let f = l(d, e);
    ([p[0], a, d, f], [f, e, c, p[3]])
}

/// A handle as a tangent: `rise` over `influence` of a segment of `seconds`.
fn tangent(rise: f64, influence: f64, seconds: f64) -> StudioTemporalTangent {
    let influence = influence.clamp(0.0, 1.0);
    let span = influence * seconds;
    StudioTemporalTangent { speed: if span > 0.0 { rise / span } else { 0.0 }, influence }
}
