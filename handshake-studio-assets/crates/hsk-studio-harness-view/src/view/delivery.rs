//! The delivery queue (lane SMB-DELIVERY-QUEUE-1; feature `media`; STU-VID-063, STU-VID-064, STU-MOT-120,
//! STU-MOT-122, STU-MOT-124, STU-MOT-125, STU-IO-110, STU-IO-164): ONE headless queue of typed items for both
//! export entry points STU-MOT-124 names, a sequence export recipe and a composition render queue item, with
//! one receipt per item and output module. No toolkit code.
//!
//! # Manual (no-context operator or model)
//!
//! - **Items.** `sequence`: a document sequence, a recipe id (a shipped preset id, which is the same
//!   `StudioExportRecipe` record, or a user recipe of the document entry `export_recipes`), an optional span
//!   (`in_tick` / `out_tick`, whole frames; default the work area, else the whole sequence), raster (default
//!   the recipe's `ADBEVideoWidth` / `ADBEVideoHeight`, else 1920x1080), bit depth (8) and a RELATIVE output
//!   path (resolved against the save root; absolute paths, drive prefixes and `..` are
//!   `delivery_output_path`). `composition`: a render queue item id of the motion records
//!   (`StudioMotionRecords.render_queue`); its output modules name their own relative paths.
//!   [`Viewer::delivery_add`] checks the item before it queues it (the sequence, the recipe and its codec in
//!   this build; the render queue item and every module's settings, so `output_alpha_codec` and
//!   `output_post_render` are typed at add). At most [`MAX_DELIVERY_ITEMS`] items (`delivery_queue_full`).
//! - **State.** The queue is per Viewer and not saved (lead ruling 2026-11-08): composition items already
//!   persist as `StudioRenderQueueItem`s, sequence items are cheap to add again.
//! - **Run** ([`Viewer::delivery_run`]): before any frame, every output path of every item (sequence
//!   outputs, composition module outputs, PNG name patterns) is resolved and two equal ones refuse the whole
//!   run `delivery_output_collision`. Then the items run in queue order, one at a time, each on a worker
//!   thread (`hsk-delivery`), admitted through the session job port before its thread starts (job
//!   admission, `AdmittedExportQueue` for reel jobs, `JobAdmission::admit` for a PNG module). A sequence
//!   item is `Viewer::export_sequence`'s one implementation: the recipe resolves through reel's
//!   `job_from_recipe` (the six-identifier writer binding; any other parameter is
//!   `export_parameter_not_consumed` before the first frame), frames come from the program monitor's port,
//!   served by this thread. A composition item renders its composition once for all its modules
//!   (`SharedFrames`) from the open document's tiles ([`DocumentTiles`], the planes the composition layers
//!   name, copied and charged against [`MAX_DOCUMENT_TILE_BYTES`]), animated glyphs from the text entry and
//!   the bound sound port for audio layers (an `audio_id` `sha256:<hex>` names the media clip source the
//!   sound port holds, at the module's rate).
//! - **Receipts** (one per item and module, `delivery_run`'s `receipts` and [`Viewer::delivery_receipts`]):
//!   `{delivery_id, kind, recipe_id, sequence_id | item_id + module_id, span {start_ticks, end_ticks},
//!   outcome complete|canceled|failed|refused, code?, detail?, artifacts [relative paths written], partial
//!   (outcome not complete and artifacts named), receipt (reel's receipt JSON, or the PNG sequence receipt),
//!   job (the admission settlement: completed, completed_stale, canceled, failed)}`. A refusal of one item
//!   (build or admission) is its receipt (`refused` with the code); the run goes on with the next item.
//! - **Cancel** ([`Viewer::delivery_cancel`]): one item only. A queued item never starts (its receipt is
//!   `canceled`, nothing written); [`Viewer::delivery_cancel_handle`] gives a `Send` handle that cancels the
//!   item while it runs (reel stops at the next frame and publishes nothing; a PNG sequence stops before the
//!   next frame and its receipt names the frames already written, STU-MOT-125 "a stopped render's partial
//!   output is named in the receipt").
//! - **Refusals:** `not_open`, `delivery_queue_full`, `delivery_output_path`, `unknown_sequence`,
//!   `unknown_preset`, `codec_unavailable` and reel's recipe codes, `render_queue_item`,
//!   `render_queue_composition`, `session_records`, the render queue's `output_*` and `render_*` codes,
//!   `unknown_delivery_item`, `delivery_queue_empty`, `delivery_output_collision`.
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::job_admission::{AdmittedExportQueue, ExportSettlements, refusal_json, settlement_json};
use super::monitor::MonitorFail;
use super::timeline::ExportSequenceRequest;
use super::timeline_export::composition::{PngModule, PngSequenceReceipt, run_png_sequence};
use super::timeline_export::render_queue::{RenderQueueBuild, TemporalScenes, check_item_outputs, records_render_queue_build};
use super::timeline_export::{admitted_export_queue, recipe_record};
use hsk_studio_accord::CancellationToken;
use hsk_studio_composite::{ScenePixels, TileInfo, TileInfoSource};
use hsk_studio_folio::{self as folio, StudioCompositionRecords, StudioMotionRecords, TileRef};
use hsk_studio_pigment::render_plan::{AlphaConvention, ColourTag, SampleFormat, TilePlanes};
use hsk_studio_reel::{ExportHandle, ExportReceipt};
use hsk_studio_session::{Action, COMPOSITION_ENTRY, JobOutcome, MOTION_ENTRY};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Most items one queue holds.
pub const MAX_DELIVERY_ITEMS: usize = 4096;
/// Most tile bytes a composition item copies from the open document.
pub const MAX_DOCUMENT_TILE_BYTES: u64 = 1 << 30;
/// Side of one stored raster tile cell (the document's tiles; `render.rs` `CELL`).
const CELL: usize = 256;
const COLOUR_CELL: usize = CELL * CELL * 12;
const ALPHA_CELL: usize = CELL * CELL * 4;

fn fail(code: &'static str, detail: impl Into<String>) -> MonitorFail {
    MonitorFail { code, detail: detail.into() }
}

/// A sequence recipe item (module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct SequenceItem {
    pub sequence_id: String,
    pub recipe_id: String,
    pub in_tick: Option<u64>,
    pub out_tick: Option<u64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bit_depth: u8,
    /// Relative to the save root.
    pub output: String,
    pub overwrite: bool,
}

impl SequenceItem {
    /// `sequence_id` with recipe `recipe_id` to `output` (relative), every other field defaulted.
    pub fn new(sequence_id: &str, recipe_id: &str, output: &str) -> Self {
        Self { sequence_id: sequence_id.into(), recipe_id: recipe_id.into(), in_tick: None, out_tick: None, width: None, height: None, bit_depth: 8, output: output.into(), overwrite: false }
    }
}

/// One item of the delivery queue (module docs).
#[derive(Clone, Debug, PartialEq)]
pub enum DeliveryItem {
    Sequence(SequenceItem),
    /// A render queue item of the motion records; `overwrite` replaces existing module outputs.
    Composition { item_id: String, overwrite: bool },
}

impl DeliveryItem {
    fn json(&self) -> Value {
        match self {
            Self::Sequence(s) => json!({"kind": "sequence", "sequence_id": s.sequence_id, "recipe_id": s.recipe_id, "in_tick": s.in_tick, "out_tick": s.out_tick,
                "width": s.width, "height": s.height, "bit_depth": s.bit_depth, "output": s.output, "overwrite": s.overwrite}),
            Self::Composition { item_id, overwrite } => json!({"kind": "composition", "item_id": item_id, "overwrite": overwrite}),
        }
    }
}

/// Cancels one delivery item, also while it runs (module docs); `Send`, cloneable.
#[derive(Clone, Default)]
pub struct DeliveryCancel(Arc<Mutex<CancelState>>);

#[derive(Default)]
struct CancelState {
    requested: bool,
    handles: Vec<ExportHandle>,
    tokens: Vec<CancellationToken>,
}

impl DeliveryCancel {
    pub fn cancel(&self) {
        if let Ok(mut s) = self.0.lock() {
            s.requested = true;
            s.handles.iter().for_each(ExportHandle::cancel);
            s.tokens.iter().for_each(CancellationToken::cancel);
        }
    }
    pub fn requested(&self) -> bool {
        self.0.lock().is_ok_and(|s| s.requested)
    }
    fn register_handle(&self, handle: &ExportHandle) {
        if let Ok(mut s) = self.0.lock() {
            if s.requested {
                handle.cancel();
            }
            s.handles.push(handle.clone());
        }
    }
    fn register_token(&self, token: &CancellationToken) {
        if let Ok(mut s) = self.0.lock() {
            if s.requested {
                token.cancel();
            }
            s.tokens.push(token.clone());
        }
    }
}

struct Queued {
    id: u64,
    item: DeliveryItem,
    cancel: DeliveryCancel,
}

/// The per-Viewer queue (module docs; not saved).
#[derive(Default)]
pub struct DeliveryState {
    items: Vec<Queued>,
    next_id: u64,
    receipts: Vec<Value>,
}

/// `output` as a relative path with no root, drive, `..` or empty part (module docs).
fn relative(output: &str) -> Result<PathBuf, MonitorFail> {
    let path = Path::new(output);
    let ok = !output.is_empty()
        && output.len() <= 1024
        && !output.contains(':')
        && !output.starts_with(['/', '\\'])
        && path.components().all(|c| matches!(c, Component::Normal(_)))
        && output.split(['/', '\\']).all(|s| !s.is_empty() && s != ".." && s != ".");
    if ok { Ok(path.to_path_buf()) } else { Err(fail("delivery_output_path", format!("{output}: a relative path inside the save root (no root, drive or ..)"))) }
}

/// Lower-cased form of an output path for the collision check (file systems that fold case).
fn collision_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").to_ascii_lowercase()
}

/// The planes of the document tiles a composition names, copied (module docs).
pub struct DocumentTiles {
    /// By content digest.
    planes: BTreeMap<String, (Vec<u8>, Vec<u8>, [u8; 32], [u8; 32])>,
    /// Scene tile id `"{layer_id}/{object_key}"` to content digest and size.
    tiles: BTreeMap<String, (String, u32, u32)>,
}

impl DocumentTiles {
    /// The raster tiles of the document layers any composition of `compositions` holds, from `assets`
    /// (digest to bytes), at most [`MAX_DOCUMENT_TILE_BYTES`] (`document_tiles_over_budget`).
    pub fn of(document: &folio::StudioDocument, compositions: &StudioCompositionRecords, assets: &super::render::Assets) -> Result<Self, MonitorFail> {
        let mut planes = BTreeMap::new();
        let mut tiles = BTreeMap::new();
        let mut charged = 0u64;
        let named: std::collections::BTreeSet<&str> = compositions.compositions.iter().flat_map(|c| c.layers.iter().map(|l| l.layer_id.as_str())).collect();
        for layer in document.layers.iter().filter(|l| named.contains(l.layer_id.as_str())) {
            let folio::Payload::Raster { tiles: refs } = &layer.payload else { continue };
            for t in refs {
                let side = |l: &folio::Length| (l.value.is_finite() && l.value >= 0.0 && l.value <= CELL as f64).then_some(l.value as u32);
                let (Some(w), Some(h)) = (side(&t.width), side(&t.height)) else { continue };
                let digest = t.content_digest.digest.clone();
                if !planes.contains_key(&digest)
                    && let Some(bytes) = assets.get(&digest).filter(|b| b.len() == COLOUR_CELL.saturating_add(ALPHA_CELL))
                {
                    charged = charged.saturating_add(bytes.len() as u64);
                    if charged > MAX_DOCUMENT_TILE_BYTES {
                        return Err(fail("document_tiles_over_budget", format!("more than {MAX_DOCUMENT_TILE_BYTES} tile bytes")));
                    }
                    let (colour, alpha) = bytes.split_at(COLOUR_CELL);
                    planes.insert(digest.clone(), (colour.to_vec(), alpha.to_vec(), hsk_studio_prism::profile_hash(colour), hsk_studio_prism::profile_hash(alpha)));
                }
                tiles.insert(format!("{}/{}", t.layer_id, t.object_key), (digest, w, h));
            }
        }
        Ok(Self { planes, tiles })
    }
}

impl TileInfoSource for DocumentTiles {
    fn tile_info(&self, tile: &TileRef) -> Option<TileInfo> {
        if tile.format != SampleFormat::RgbF32Le.key() {
            return None;
        }
        let (_, _, c, a) = self.planes.get(tile.content_digest.digest.as_str())?;
        Some(TileInfo { format: SampleFormat::RgbF32Le, alpha: AlphaConvention::Straight, colour_sha256: *c, alpha_sha256: *a })
    }
    fn mask_sha256(&self, _: &[TileRef]) -> Option<[u8; 32]> {
        None
    }
}

/// [`DocumentTiles`] as scene pixels (shared by the scenes of one item).
#[derive(Clone)]
pub struct DocumentPixels(Arc<DocumentTiles>, ColourTag);

impl ScenePixels for DocumentPixels {
    fn tile(&self, tile_id: &str) -> Option<TilePlanes<'_>> {
        let (digest, width, height) = self.0.tiles.get(tile_id)?;
        let (colour, alpha, cs, a) = self.0.planes.get(digest)?;
        Some(TilePlanes {
            width: *width,
            height: *height,
            colour,
            colour_stride_bytes: (CELL * 12) as u64,
            alpha,
            alpha_stride_bytes: (CELL * 4) as u64,
            colour_sha256: *cs,
            alpha_sha256: *a,
            format: SampleFormat::RgbF32Le,
            alpha_convention: AlphaConvention::Straight,
            space: self.1,
        })
    }
}

/// The opened session already resolved every reference; the copies read here re-state it (as `render.rs`'s
/// view resolver does).
struct Resolved;
impl folio::Resolver for Resolved {
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

/// The working space the static renderer tags document tiles with (`render.rs`).
fn working_space() -> ColourTag {
    ColourTag { profile_sha256: hsk_studio_prism::profile_hash(hsk_studio_prism::WORKING_PROFILE_LINEAR_SRGB), blend_space: hsk_studio_composite::BlendSpace::LinearLight }
}

/// The settlement JSON of a PNG module.
fn png_outcome(r: &PngSequenceReceipt) -> JobOutcome {
    match r.outcome {
        "complete" => JobOutcome::Completed,
        "canceled" => JobOutcome::Canceled,
        _ => JobOutcome::Failed(r.code.unwrap_or("png_sequence_failed")),
    }
}

fn reel_outcome(r: &ExportReceipt) -> (&'static str, Option<&'static str>, Option<String>) {
    match &r.outcome {
        hsk_studio_reel::ExportOutcome::Complete => ("complete", None, None),
        hsk_studio_reel::ExportOutcome::Canceled => ("canceled", Some("MEDIA_CANCELED"), None),
        hsk_studio_reel::ExportOutcome::Failed { code, detail } => ("failed", Some(*code), Some(detail.clone())),
    }
}

impl Viewer {
    /// Queues `item` after checking it (module docs); returns its delivery id.
    pub fn delivery_add(&mut self, item: DeliveryItem) -> Result<u64, MonitorFail> {
        if self.delivery.items.len() >= MAX_DELIVERY_ITEMS {
            return Err(fail("delivery_queue_full", format!("{MAX_DELIVERY_ITEMS} items")));
        }
        let doc = self.run.snapshot().map(|s| s.document()).ok_or_else(|| fail("not_open", "nothing is open"))?;
        match &item {
            DeliveryItem::Sequence(s) => {
                relative(&s.output)?;
                if !doc.sequences.iter().any(|q| q.sequence_id == s.sequence_id) {
                    return Err(fail("unknown_sequence", s.sequence_id.clone()));
                }
                let (_, os) = admitted_export_queue(None, self.job_admission());
                let record = recipe_record(&s.recipe_id, &self.user_recipes()?)?;
                super::timeline_export::resolve_recipe(&record, os)?;
            }
            DeliveryItem::Composition { item_id, .. } => {
                let (compositions, motion) = self.motion_records_read()?;
                let q = motion.render_queue.iter().find(|q| &q.item_id == item_id).ok_or_else(|| fail("render_queue_item", item_id.clone()))?;
                let comp = compositions.composition(&q.composition_id).ok_or_else(|| fail("render_queue_composition", q.composition_id.clone()))?;
                let has_audio = motion.composition_audio(&comp.composition_id).is_some_and(|a| !a.layers.is_empty());
                check_item_outputs(q, comp, has_audio)?;
                for m in &q.output_modules {
                    relative(&m.output_path)?;
                }
            }
        }
        self.delivery.next_id = self.delivery.next_id.saturating_add(1);
        let id = self.delivery.next_id;
        self.delivery.items.push(Queued { id, item, cancel: DeliveryCancel::default() });
        Ok(id)
    }

    /// Removes a queued item (`unknown_delivery_item`).
    pub fn delivery_remove(&mut self, id: u64) -> Result<(), MonitorFail> {
        let n = self.delivery.items.len();
        self.delivery.items.retain(|q| q.id != id);
        if self.delivery.items.len() == n { Err(fail("unknown_delivery_item", id.to_string())) } else { Ok(()) }
    }

    /// Cancels one queued item (module docs; `unknown_delivery_item`).
    pub fn delivery_cancel(&mut self, id: u64) -> Result<(), MonitorFail> {
        self.delivery_cancel_handle(id).ok_or_else(|| fail("unknown_delivery_item", id.to_string()))?.cancel();
        Ok(())
    }

    /// A handle that cancels item `id` also while [`Viewer::delivery_run`] runs it.
    pub fn delivery_cancel_handle(&self, id: u64) -> Option<DeliveryCancel> {
        self.delivery.items.iter().find(|q| q.id == id).map(|q| q.cancel.clone())
    }

    /// `{items: [{delivery_id, canceled, ...item}]}`.
    pub fn delivery_list(&self) -> Value {
        let items: Vec<Value> = self
            .delivery
            .items
            .iter()
            .map(|q| {
                let mut v = q.item.json();
                v["delivery_id"] = json!(q.id);
                v["canceled"] = json!(q.cancel.requested());
                v
            })
            .collect();
        json!({"items": items})
    }

    /// The receipts of the last run.
    pub fn delivery_receipts(&self) -> Value {
        json!({"receipts": self.delivery.receipts})
    }

    /// Runs every queued item (module docs) and returns `{receipts, counts}`; the queue is empty afterwards.
    pub fn delivery_run(&mut self) -> Result<Value, MonitorFail> {
        if self.run.snapshot().is_none() {
            return Err(fail("not_open", "nothing is open"));
        }
        if self.delivery.items.is_empty() {
            return Err(fail("delivery_queue_empty", "no item is queued"));
        }
        self.delivery_collisions()?;
        let items = std::mem::take(&mut self.delivery.items);
        let mut receipts = Vec::new();
        for q in &items {
            let mut out = match &q.item {
                DeliveryItem::Sequence(s) => vec![self.delivery_sequence(s, &q.cancel)],
                DeliveryItem::Composition { item_id, overwrite } => self.delivery_composition(item_id, *overwrite, &q.cancel),
            };
            for r in &mut out {
                r["delivery_id"] = json!(q.id);
            }
            receipts.extend(out);
        }
        let count = |o: &str| receipts.iter().filter(|r| r["outcome"] == o).count();
        let counts = json!({"complete": count("complete"), "canceled": count("canceled"), "failed": count("failed"), "refused": count("refused")});
        self.say(format!("delivery: {} receipt(s) {counts}", receipts.len()));
        self.delivery.receipts = receipts.clone();
        Ok(json!({"receipts": receipts, "counts": counts}))
    }

    /// Every output path of the queue, resolved; two equal ones refuse the run (module docs).
    fn delivery_collisions(&self) -> Result<(), MonitorFail> {
        let root = self.export_root()?;
        let mut seen: BTreeMap<String, u64> = BTreeMap::new();
        let mut records = None;
        for q in &self.delivery.items {
            let paths: Vec<PathBuf> = match &q.item {
                DeliveryItem::Sequence(s) => vec![root.join(relative(&s.output)?)],
                DeliveryItem::Composition { item_id, .. } => {
                    if records.is_none() {
                        records = Some(self.motion_records_read()?);
                    }
                    let (_, motion) = records.as_ref().ok_or_else(|| fail("session_records", "no motion records"))?;
                    let item = motion.render_queue.iter().find(|r| &r.item_id == item_id).ok_or_else(|| fail("render_queue_item", item_id.clone()))?;
                    item.output_modules.iter().map(|m| root.join(&m.output_path)).collect()
                }
            };
            for p in paths {
                if let Some(other) = seen.insert(collision_key(&p), q.id) {
                    return Err(fail("delivery_output_collision", format!("items {other} and {} both write {}", q.id, p.display())));
                }
            }
        }
        Ok(())
    }

    /// The user export recipes of the open document (module `timeline_export::user_recipes`).
    pub fn user_recipes(&self) -> Result<Vec<hsk_studio_reel::StudioExportRecipe>, MonitorFail> {
        super::timeline_export::user_recipes(self.run.document_entry(hsk_studio_session::EXPORT_RECIPES_ENTRY))
    }

    /// The composition and motion records of the open document, validated against it.
    fn motion_records_read(&self) -> Result<(StudioCompositionRecords, StudioMotionRecords), MonitorFail> {
        let snap = self.run.snapshot().ok_or_else(|| fail("not_open", "nothing is open"))?;
        let doc = snap.document();
        let read = |token: &str| self.run.document_entry(token).ok_or_else(|| fail("session_records", format!("no {token} records")));
        let compositions: StudioCompositionRecords = serde_json::from_slice(read(COMPOSITION_ENTRY)?).map_err(|_| fail("session_records", "composition records do not parse"))?;
        folio::validate_composition_records(&compositions, doc, folio::Budget::default(), self.cancel, &Resolved).map_err(|d| fail("session_records", format!("{d:?}")))?;
        let motion: StudioMotionRecords = serde_json::from_slice(read(MOTION_ENTRY)?).map_err(|_| fail("session_records", "motion records do not parse"))?;
        folio::validate_motion_records(&motion, doc, &compositions, folio::Budget::default(), self.cancel, &Resolved).map_err(|d| fail("session_records", format!("{d:?}")))?;
        Ok((compositions, motion))
    }

    /// One sequence item through `export_sequence`'s implementation (module docs).
    fn delivery_sequence(&mut self, s: &SequenceItem, cancel: &DeliveryCancel) -> Value {
        let span = |r: &Value| json!({"start_ticks": r["in_tick"], "end_ticks": r["out_tick"]});
        let base = json!({"kind": "sequence", "sequence_id": s.sequence_id, "recipe_id": s.recipe_id, "output": s.output});
        // A cancel before the run cancels the admitted job's handle at submit: reel never starts it and the
        // admission settles it `canceled`.
        let request = match self.delivery_request(s) {
            Ok(r) => r,
            Err(e) => return refused(base, &e),
        };
        match self.export_sequence_with(&s.sequence_id, &request, &mut |h| cancel.register_handle(h)) {
            Ok(result) => {
                let mut r = base;
                let outcome = result["receipt"]["outcome"].as_str().unwrap_or("failed").to_owned();
                r["span"] = span(&result);
                r["artifacts"] = if outcome == "complete" { json!([s.output]) } else { json!([]) };
                r["partial"] = json!(false);
                r["code"] = result["receipt"]["code"].clone();
                r["detail"] = result["receipt"]["detail"].clone();
                r["outcome"] = json!(outcome);
                r["receipt"] = result["receipt"].clone();
                r["job"] = result["job"].clone();
                r
            }
            Err(e) => refused(base, &e),
        }
    }

    /// The export request of a sequence item: raster from the item, else the recipe, else the default.
    fn delivery_request(&self, s: &SequenceItem) -> Result<ExportSequenceRequest, MonitorFail> {
        let record = recipe_record(&s.recipe_id, &self.user_recipes()?)?;
        let declared = |id: &str| match record.parameters().get(id) {
            Some(hsk_studio_reel::ExportValue::Integer(v)) => u32::try_from(*v).ok(),
            _ => None,
        };
        let output = self.export_root()?.join(relative(&s.output)?);
        let mut r = ExportSequenceRequest::new(&s.recipe_id, output);
        r.in_tick = s.in_tick;
        r.out_tick = s.out_tick;
        r.width = s.width.or_else(|| declared("ADBEVideoWidth")).unwrap_or(r.width);
        r.height = s.height.or_else(|| declared("ADBEVideoHeight")).unwrap_or(r.height);
        r.bit_depth = s.bit_depth;
        r.overwrite = s.overwrite;
        Ok(r)
    }

    /// One composition item: one receipt per output module (module docs).
    fn delivery_composition(&mut self, item_id: &str, overwrite: bool, cancel: &DeliveryCancel) -> Vec<Value> {
        let base = json!({"kind": "composition", "item_id": item_id});
        let built = self.delivery_build(item_id, overwrite);
        let (build, root) = match built {
            Ok(b) => b,
            Err(e) => return vec![refused(base, &e)],
        };
        let RenderQueueBuild { jobs, shared, notes, pngs, span } = build;
        let span_json = json!({"start_ticks": span.in_tick, "end_ticks": span.out_tick});
        let admission = self.job_admission();
        let head = self.document_head();
        let bound = head;
        let (mut queue, _) = admitted_export_queue(None, admission.clone());
        let mut out = Vec::new();
        let mut reel_modules = Vec::new();
        for (module_id, job) in jobs {
            let recipe_id = job.recipe.recipe_id.clone();
            let output = job.output.clone();
            let bytes = super::job_admission::frame_bytes(span.width, span.height);
            match queue.submit(job, bytes, bound, head, self.cancel) {
                Ok(handle) => {
                    cancel.register_handle(&handle);
                    reel_modules.push((module_id, recipe_id, output, handle.id));
                }
                Err(r) => {
                    let mut v = base.clone();
                    v["module_id"] = json!(module_id);
                    v["recipe_id"] = json!(recipe_id);
                    v["span"] = span_json.clone();
                    v["outcome"] = json!("refused");
                    v["code"] = json!(r.code());
                    v["detail"] = refusal_json(r);
                    v["artifacts"] = json!([]);
                    v["partial"] = json!(false);
                    v["job"] = Value::Null;
                    out.push(v);
                }
            }
        }
        let mut png_jobs = Vec::new();
        for (module_id, module) in pngs {
            let correlation = admission.next_correlation();
            let bytes = super::job_admission::frame_bytes(span.width, span.height);
            match admission.admit(admission.request(Action::Export, bytes, bound, correlation), head, self.cancel) {
                Ok(job) => {
                    let token = CancellationToken::default();
                    cancel.register_token(&token);
                    png_jobs.push((module_id, module, job, token));
                }
                Err(r) => {
                    let mut v = base.clone();
                    v["module_id"] = json!(module_id);
                    v["recipe_id"] = json!("png_sequence");
                    v["span"] = span_json.clone();
                    v["outcome"] = json!("refused");
                    v["code"] = json!(r.code());
                    v["detail"] = refusal_json(r);
                    v["artifacts"] = json!([]);
                    v["partial"] = json!(false);
                    v["job"] = Value::Null;
                    out.push(v);
                }
            }
        }
        let (reel_queue, settlements): (hsk_studio_reel::ExportQueue, ExportSettlements) = AdmittedExportQueue::into_worker(queue);
        let png_runs: Vec<(String, PngModule, CancellationToken)> = png_jobs.iter().map(|(id, m, _, t)| (id.clone(), m.clone(), t.clone())).collect();
        let frames = span.frames;
        let shared_png = Arc::clone(&shared);
        let worker = std::thread::Builder::new().name("hsk-delivery".into()).spawn(move || {
            let mut reel_queue = reel_queue;
            let receipts = reel_queue.run();
            let pngs: Vec<PngSequenceReceipt> = png_runs.iter().map(|(_, m, t)| run_png_sequence(&|i, c| shared_png.frame(i, c), frames, m, t)).collect();
            (receipts, pngs)
        });
        let joined = match worker {
            Ok(w) => w.join().map_err(|_| "the delivery worker panicked"),
            Err(_) => Err("the delivery worker did not start"),
        };
        let head_after = self.document_head();
        let (receipts, png_receipts) = match joined {
            Ok(r) => r,
            Err(why) => {
                for s in settlements.fail("export_worker") {
                    let mut v = base.clone();
                    v["outcome"] = json!("failed");
                    v["code"] = json!("export_worker");
                    v["detail"] = json!(why);
                    v["job"] = settlement_json(&s);
                    out.push(v);
                }
                for (_, _, job, _) in png_jobs {
                    let s = job.settle(JobOutcome::Failed("export_worker"));
                    let mut v = base.clone();
                    v["outcome"] = json!("failed");
                    v["code"] = json!("export_worker");
                    v["job"] = settlement_json(&s);
                    out.push(v);
                }
                return out;
            }
        };
        let settled = settlements.settle(&receipts, head_after);
        for ((module_id, recipe_id, output, item), job) in reel_modules.into_iter().zip(settled) {
            let receipt = receipts.iter().find(|r| r.item_id == item);
            let mut v = base.clone();
            v["module_id"] = json!(module_id);
            v["recipe_id"] = json!(recipe_id);
            v["span"] = span_json.clone();
            match receipt {
                Some(r) => {
                    let (outcome, code, detail) = reel_outcome(r);
                    let rel = output.strip_prefix(&root).map_or_else(|_| output.to_string_lossy().into_owned(), |p| p.to_string_lossy().replace('\\', "/"));
                    v["outcome"] = json!(outcome);
                    v["code"] = json!(code);
                    v["detail"] = json!(detail);
                    v["artifacts"] = if r.published_media { json!([rel]) } else { json!([]) };
                    v["partial"] = json!(outcome != "complete" && r.published_media);
                    v["receipt"] = serde_json::from_str(&r.to_json()).unwrap_or(Value::Null);
                }
                None => {
                    v["outcome"] = json!("failed");
                    v["code"] = json!("export_worker");
                    v["artifacts"] = json!([]);
                    v["partial"] = json!(false);
                }
            }
            v["job"] = settlement_json(&job);
            v["notes"] = json!(notes.iter().map(|n| n.code).collect::<Vec<_>>());
            out.push(v);
        }
        for ((module_id, _, job, _), r) in png_jobs.into_iter().zip(png_receipts) {
            let outcome = png_outcome(&r);
            let settlement = match outcome {
                JobOutcome::Completed => job.complete(head_after),
                other => job.settle(other),
            };
            let mut v = base.clone();
            v["module_id"] = json!(module_id);
            v["recipe_id"] = json!("png_sequence");
            v["span"] = span_json.clone();
            v["outcome"] = json!(r.outcome);
            v["code"] = json!(r.code);
            v["detail"] = json!(r.detail);
            v["artifacts"] = json!(r.artifacts);
            v["partial"] = json!(r.outcome != "complete" && !r.artifacts.is_empty());
            v["receipt"] = r.json();
            v["job"] = settlement_json(&settlement);
            v["notes"] = json!(notes.iter().map(|n| n.code).collect::<Vec<_>>());
            out.push(v);
        }
        out
    }

    /// The render-once build of render queue item `item_id` over the open document (module docs) and the
    /// save root its outputs resolve against.
    fn delivery_build(&self, item_id: &str, overwrite: bool) -> Result<(RenderQueueBuild<TemporalScenes<DocumentPixels>>, PathBuf), MonitorFail> {
        let (compositions, motion) = self.motion_records_read()?;
        let snap = self.run.snapshot().ok_or_else(|| fail("not_open", "nothing is open"))?;
        let tiles = Arc::new(DocumentTiles::of(snap.document(), &compositions, &self.assets)?);
        let space = working_space();
        let glyphs = if motion.text_animators.is_empty() {
            BTreeMap::new()
        } else {
            let entry = self.text_entry().map_err(|e| fail("text_entry", e))?;
            super::text::animated_glyphs(snap.document(), &entry, &self.fonts, self.run.actor(), self.cancel).map_err(|e| fail("text_animator_glyphs", e))?
        };
        let port = self.timeline.as_ref().and_then(|t| t.export.sound());
        let audio = move |audio_id: &str, rate: u32| -> Option<Arc<Vec<f32>>> {
            let (sources, port_rate) = port.as_ref()?;
            if *port_rate != rate {
                return None;
            }
            let hex = audio_id.strip_prefix("sha256:")?;
            let source = folio::ClipSource::Media { artifact_manifest_id: audio_id.to_owned(), content_digest: folio::ContentDigest { algorithm: "sha256".into(), digest: hex.to_owned() }, asset_id: None };
            let planes = sources.audio(&source)?;
            let (l, r) = match planes {
                [m] => (m, m),
                [l, r, ..] => (l, r),
                [] => return None,
            };
            Some(Arc::new(l.iter().zip(r.iter()).flat_map(|(a, b)| [*a, *b]).collect()))
        };
        let root = self.export_root()?;
        let pixels = DocumentPixels(Arc::clone(&tiles), space);
        let build = records_render_queue_build(snap, compositions, &motion, item_id, &Resolved, tiles.as_ref(), &move || pixels.clone(), &glyphs, &audio, space, &root, overwrite, self.cancel)?;
        Ok((build, root))
    }
}

/// One recipe as `export.recipes` lists it.
fn recipe_row(r: &hsk_studio_reel::StudioExportRecipe, shipped: bool, os: bool) -> Value {
    let available = super::timeline_export::resolve_recipe(r, os).err().map(|e| e.code);
    json!({"recipe_id": r.recipe_id(), "name": r.name(), "shipped": shipped, "does_video": r.does_video(), "does_audio": r.does_audio(),
        "available": available.is_none(), "unavailable_code": available, "recipe": serde_json::from_str::<Value>(&r.to_json()).unwrap_or(Value::Null)})
}

impl Viewer {
    /// `export.recipe_save` (module docs of `timeline_export::user_recipes`; STU-VID-063): a new user recipe
    /// `recipe_id` from the shipped preset or user recipe `from`, renamed `name` and with each parameter edit
    /// applied (`null` removes the identifier); every value goes through reel's checks. One entry-only undo
    /// step (`Run::set_export_recipes`). Refusals: `not_open`, `export_recipe_exists` (a shipped or user id),
    /// `unknown_preset`, reel's recipe codes, `export_recipes_full`.
    pub fn export_recipe_save(&mut self, from: &str, recipe_id: &str, name: Option<&str>, edits: &[(String, Value)]) -> Result<Value, MonitorFail> {
        if self.run.snapshot().is_none() {
            return Err(fail("not_open", "nothing is open"));
        }
        let mut recipes = self.user_recipes()?;
        if hsk_studio_reel::presets::export_preset(recipe_id).is_some() || recipes.iter().any(|r| r.recipe_id() == recipe_id) {
            return Err(fail("export_recipe_exists", recipe_id.to_owned()));
        }
        if recipes.len() >= super::timeline_export::MAX_USER_RECIPES {
            return Err(fail("export_recipes_full", format!("{} recipes", super::timeline_export::MAX_USER_RECIPES)));
        }
        let base = recipe_record(from, &recipes)?;
        let mut v: Value = serde_json::from_str(&base.to_json()).map_err(|_| fail("export_recipe_field_invalid", "recipe"))?;
        v["recipe_id"] = json!(recipe_id);
        if let Some(n) = name {
            v["name"] = json!(n);
        }
        for (id, value) in edits {
            match (v["parameters"].as_object_mut(), value) {
                (Some(p), Value::Null) => {
                    p.remove(id);
                }
                (Some(p), value) => {
                    p.insert(id.clone(), value.clone());
                }
                (None, _) => return Err(fail("export_recipe_field_invalid", "parameters")),
            }
        }
        let recipe = super::timeline_export::recipe_from_json(&v)?;
        let row = recipe_row(&recipe, false, admitted_export_queue(None, self.job_admission()).1);
        recipes.push(recipe);
        let bytes = super::timeline_export::user_recipes_entry(&recipes);
        let ok = self.run.set_export_recipes(false, recipe_id, bytes, recipes.len());
        self.after_edit(ok, format!("export recipe save {recipe_id}")).map_err(|e| fail("export_recipes_refused", e))?;
        Ok(row)
    }

    /// `export.recipe_delete`: removes user recipe `recipe_id` (the last one removes the entry), one
    /// entry-only undo step. Refusals: `not_open`, `unknown_recipe` (shipped presets are not deletable).
    pub fn export_recipe_delete(&mut self, recipe_id: &str) -> Result<Value, MonitorFail> {
        if self.run.snapshot().is_none() {
            return Err(fail("not_open", "nothing is open"));
        }
        let mut recipes = self.user_recipes()?;
        let n = recipes.len();
        recipes.retain(|r| r.recipe_id() != recipe_id);
        if recipes.len() == n {
            return Err(fail("unknown_recipe", recipe_id.to_owned()));
        }
        let bytes = super::timeline_export::user_recipes_entry(&recipes);
        let ok = self.run.set_export_recipes(true, recipe_id, bytes, recipes.len());
        self.after_edit(ok, format!("export recipe delete {recipe_id}")).map_err(|e| fail("export_recipes_refused", e))?;
        Ok(json!({"recipe_id": recipe_id, "recipes": recipes.len()}))
    }

    /// `export.recipes`: `{shipped: [...], user: [...]}`, each `{recipe_id, name, shipped, does_video,
    /// does_audio, available, unavailable_code, recipe}` (availability in this build's queue).
    pub fn export_recipes(&self) -> Result<Value, MonitorFail> {
        let os = admitted_export_queue(None, self.job_admission()).1;
        let shipped: Vec<Value> = hsk_studio_reel::presets::EXPORT_PRESETS.iter().map(|p| recipe_row(&p.studio_recipe(), true, os)).collect();
        let user: Vec<Value> = self.user_recipes()?.iter().map(|r| recipe_row(r, false, os)).collect();
        Ok(json!({"shipped": shipped, "user": user}))
    }
}

/// A refusal before any frame as the item's receipt.
fn refused(mut base: Value, e: &MonitorFail) -> Value {
    base["outcome"] = json!("refused");
    base["code"] = json!(e.code);
    base["detail"] = json!(e.detail);
    base["artifacts"] = json!([]);
    base["partial"] = json!(false);
    base["job"] = Value::Null;
    base
}
