//! Develop panel and develop export (lane SMB-HARNESS-DEVELOP-EXPORT; feature `viewer`).
//!
//! Flow: open a camera raw or DNG (the import develops it once into the `Background` raster layer)
//! -> adjust develop sliders -> `hsk_studio_develop::develop_to_output` -> PNG (8/16-bit) or JPEG with
//! the output ICC profile embedded.
//!
//! * The sliders are fields of develop's own recipe types (`DevelopRecipe::exposure_stops` and
//!   `Adjustments` from lanes SMB-DEVELOP-CONTROLS and CONTROLS-2: white balance shift, basic tone,
//!   presence, detail). This module only writes those fields and the group enables; every number is
//!   validated and computed by `hsk-studio-develop` (`DevelopRecipe::validate`, `develop_to_image_threads`,
//!   `develop_to_output`). An out-of-range or non-finite value is develop's typed refusal, nothing
//!   is clamped here and nothing is committed.
//! * Every accepted slider change re-develops the raw from the original import entry, tiles it with
//!   the import's own route (`import::raw_proposal`: interop-psd `raster_proposal`, same ids) and
//!   commits the changed tile bodies as ONE chronicle `tiles` edit of the developed layer
//!   (`Run::set_tiles`, op `develop`): undo/redo are the session's chronicle history (UI-2/UI-3).
//!   The slider values of a pixel state are remembered by the signature of the layer's tile list, so
//!   undo/redo bring the matching values back; with the identity recipe the tiles equal the import's.
//! * Export renders `develop_to_output` (sRGB transfer over linear light with sRGB primaries) with
//!   the CC0 `sRGB-v4.icc` bound as the recipe's output profile, encodes through
//!   hsk-studio-interop-raster (PNG RGB 8/16-bit with iCCP, JPEG baseline 8-bit with APP2 ICC), decodes
//!   it back, then writes atomically. It exports the developed raw only, not the composite of other
//!   layers (the composite export is `Viewer::export`).
//!
//! Not done (typed or stated, never faked): absolute kelvin white balance (develop needs the decoded
//! `DngRaw` for `recipe_for_white`), local corrections, point colour, grading, grayscale mix; the
//! recipe is not saved with the package (the session's document-entry tokens are `selection` and
//! `text` only: session-owned).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::export::write_atomic;
use crate::import::{RawPrepared, SourceFormat, raw_prepare, raw_proposal};
use hsk_studio_controls::{ChangeSource, ControlError, ControlSpec, NumberSpec, Registry, SectionSpec, Value as ControlValue, ValueStore};
use hsk_studio_develop::{DevelopError, DevelopRecipe, OutputDepth, Threads};
use hsk_studio_folio::{Payload, TileRef};
use hsk_studio_interop_raster as raster;
use serde_json::{Value, json};
use std::path::Path;

/// The output profile of the develop export (CC0, see `profiles/README.md`).
pub const SRGB_V4_ICC: &[u8] = include_bytes!("profiles/sRGB-v4.icc");

/// Remembered pixel states (tile-list signature -> slider values) per opened raw.
const MAX_STATES: usize = 64;

/// Recipe group whose enable flag a control needs (develop gates the family on it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gate {
    /// Always applied (exposure, white balance shift, basic tone).
    None,
    /// `enable_colour_adjustments` (vibrance, saturation).
    Colour,
    /// `enable_detail` (clarity, texture, dehaze, sharpening, noise reduction).
    Detail,
}

impl Gate {
    fn key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Colour => "enable_colour_adjustments",
            Self::Detail => "enable_detail",
        }
    }
}

/// One develop slider: the recipe field it writes and the panel's slider range. The panel range is
/// the UI's (develop's documented soft range where it has one); a value outside it set through the
/// API goes to develop, which decides.
pub struct DevelopControl {
    pub id: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    /// Path of the develop field (`DevelopRecipe` / `Adjustments`).
    pub field: &'static str,
    pub panel_range: (f64, f64),
    pub gate: Gate,
}

const fn control(id: &'static str, label: &'static str, help: &'static str, field: &'static str, panel_range: (f64, f64), gate: Gate) -> DevelopControl {
    DevelopControl { id, label, help, field, panel_range, gate }
}

const COUNT: (f64, f64) = (-100.0, 100.0);

/// Every develop slider, in panel order.
pub const CONTROLS: &[DevelopControl] = &[
    control("develop.exposure", "Exposure", "Exposure in stops (EV): multiplies scene-linear values by 2^stops.", "exposure_stops", (-5.0, 5.0), Gate::None),
    control("develop.temperature", "Temperature", "Incremental white balance temperature shift (relative, STU-RAW-110).", "adjust.white_balance.temperature", COUNT, Gate::None),
    control("develop.tint", "Tint", "Incremental white balance tint shift (relative, STU-RAW-110).", "adjust.white_balance.tint", COUNT, Gate::None),
    control("develop.contrast", "Contrast", "Basic tone contrast (STU-RAW-111).", "adjust.tone.contrast", COUNT, Gate::None),
    control("develop.highlights", "Highlights", "Basic tone highlights (STU-RAW-111).", "adjust.tone.highlights", COUNT, Gate::None),
    control("develop.shadows", "Shadows", "Basic tone shadows (STU-RAW-111).", "adjust.tone.shadows", COUNT, Gate::None),
    control("develop.whites", "Whites", "Basic tone whites (STU-RAW-111).", "adjust.tone.whites", COUNT, Gate::None),
    control("develop.blacks", "Blacks", "Basic tone blacks (STU-RAW-111).", "adjust.tone.blacks", COUNT, Gate::None),
    control("develop.vibrance", "Vibrance", "Low-saturation weighted saturation (STU-RAW-111).", "adjust.presence.vibrance", COUNT, Gate::Colour),
    control("develop.saturation", "Saturation", "Saturation about the encoded luma (STU-RAW-111).", "adjust.presence.saturation", COUNT, Gate::Colour),
    control("develop.clarity", "Clarity", "Whole-frame midtone local contrast (STU-RAW-111).", "adjust.presence.clarity", COUNT, Gate::Detail),
    control("develop.texture", "Texture", "Gaussian detail layer gain (STU-RAW-111).", "adjust.detail.texture", COUNT, Gate::Detail),
    control("develop.dehaze", "Dehaze", "Dark channel prior haze removal; negative adds haze (STU-RAW-111).", "adjust.detail.dehaze", COUNT, Gate::Detail),
    control("develop.sharpen_amount", "Sharpening", "Unsharp mask amount 0..150 (STU-RAW-114).", "adjust.detail.sharpen.amount", (0.0, 150.0), Gate::Detail),
    control("develop.luminance_noise", "Luminance NR", "Luminance noise reduction 0..100 (STU-RAW-114).", "adjust.detail.noise.luminance", (0.0, 100.0), Gate::Detail),
    control("develop.colour_noise", "Colour NR", "Colour noise reduction 0..100 (STU-RAW-114).", "adjust.detail.noise.colour", (0.0, 100.0), Gate::Detail),
];

/// The develop field a control writes.
fn slot<'a>(recipe: &'a mut DevelopRecipe, id: &str) -> Option<&'a mut f64> {
    let a = &mut recipe.adjust;
    Some(match id {
        "develop.exposure" => &mut recipe.exposure_stops,
        "develop.temperature" => &mut a.white_balance.temperature,
        "develop.tint" => &mut a.white_balance.tint,
        "develop.contrast" => &mut a.tone.contrast,
        "develop.highlights" => &mut a.tone.highlights,
        "develop.shadows" => &mut a.tone.shadows,
        "develop.whites" => &mut a.tone.whites,
        "develop.blacks" => &mut a.tone.blacks,
        "develop.vibrance" => &mut a.presence.vibrance,
        "develop.saturation" => &mut a.presence.saturation,
        "develop.clarity" => &mut a.presence.clarity,
        "develop.texture" => &mut a.detail.texture,
        "develop.dehaze" => &mut a.detail.dehaze,
        "develop.sharpen_amount" => &mut a.detail.sharpen.amount,
        "develop.luminance_noise" => &mut a.detail.noise.luminance,
        "develop.colour_noise" => &mut a.detail.noise.colour,
        _ => return None,
    })
}

/// A typed refusal: a stable code (develop's, interop-raster's, or this module's) and a detail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub detail: String,
}

impl Refusal {
    fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self { code, detail: detail.into() }
    }

    fn develop(e: DevelopError, what: &str) -> Self {
        Self::new(e.code(), format!("{what}: {e}"))
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.code, self.detail)
    }
}

/// Export format of the develop export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DevelopExport {
    /// PNG RGB at 8 or 16 bits per sample.
    Png { depth: OutputDepth },
    /// Baseline JPEG, quality 1..=100 (8-bit; develop renders 8-bit samples for it).
    Jpeg { quality: u8 },
}

impl DevelopExport {
    pub fn name(self) -> &'static str {
        match self {
            Self::Png { .. } => "png",
            Self::Jpeg { .. } => "jpeg",
        }
    }
}

/// The develop state of an opened raw: the prepared plane and file recipe, the developed layer, the
/// slider values of every pixel state committed in this session.
pub struct DevelopDoc {
    prepared: RawPrepared,
    /// The file's recipe with the export profile bound (identity adjustments).
    base: DevelopRecipe,
    layer_id: String,
    /// Values of the last committed (or opened) state, in [`CONTROLS`] order.
    last: Vec<f64>,
    states: Vec<(String, Vec<f64>)>,
    threads: Threads,
}

/// Signature of a raster layer's tile list (placement and content digests, in order).
fn signature(tiles: &[TileRef]) -> String {
    let mut text = String::new();
    for t in tiles {
        text.push_str(&format!("{},{},{},{};", t.object_key, t.column, t.row, t.content_digest.digest));
    }
    hsk_studio_package::sha256_hex_of(text.as_bytes())
}

/// `base` with the slider `values` written and the group enables set for the active families.
fn recipe_with(base: &DevelopRecipe, values: &[f64]) -> DevelopRecipe {
    let mut recipe = base.clone();
    let (mut colour, mut detail) = (false, false);
    for (c, v) in CONTROLS.iter().zip(values) {
        if let Some(field) = slot(&mut recipe, c.id) {
            *field = *v;
        }
        let active = *v != 0.0;
        match c.gate {
            Gate::Colour => colour |= active,
            Gate::Detail => detail |= active,
            Gate::None => {}
        }
    }
    recipe.enables.enable_colour_adjustments = base.enables.enable_colour_adjustments || colour;
    recipe.enables.enable_detail = base.enables.enable_detail || detail;
    recipe
}

fn identity_values(base: &DevelopRecipe) -> Vec<f64> {
    let mut probe = base.clone();
    CONTROLS.iter().map(|c| slot(&mut probe, c.id).map_or(0.0, |v| *v)).collect()
}

impl DevelopDoc {
    fn layer_tiles<'a>(&self, viewer: &'a Viewer) -> Option<&'a [TileRef]> {
        let snap = viewer.run.snapshot()?;
        let layer = snap.document().layers.iter().find(|l| l.layer_id == self.layer_id)?;
        match &layer.payload {
            Payload::Raster { tiles } => Some(tiles),
            _ => None,
        }
    }

    /// Slider values of the shown pixels, and whether they are known (a committed or the imported
    /// state). Unknown: the layer was painted or the package was reopened (the recipe is not saved).
    fn current(&self, viewer: &Viewer) -> (Vec<f64>, bool) {
        let sig = self.layer_tiles(viewer).map(signature);
        match sig.and_then(|s| self.states.iter().rev().find(|(k, _)| *k == s)) {
            Some((_, v)) => (v.clone(), true),
            None => (self.last.clone(), false),
        }
    }
}

impl Viewer {
    /// Attaches the develop state after an open: only for a camera raw / DNG import (receipt
    /// `source_format` `dng` or `camera_raw`) whose package holds the original bytes.
    pub(super) fn attach_develop(&mut self, original: Option<&[u8]>, just_imported: bool) {
        self.develop = None;
        let format = match self.receipt.as_ref().and_then(|r| r["source_format"].as_str()) {
            Some("dng") => SourceFormat::Tiff,
            Some("camera_raw") => SourceFormat::CameraRaw,
            _ => return,
        };
        let Some(original) = original else {
            self.say("develop: the package holds no original raw bytes; the develop panel is unavailable".into());
            return;
        };
        let Some(snap) = self.run.snapshot() else {
            return;
        };
        let doc = snap.document();
        let raster: Vec<&hsk_studio_folio::StudioLayer> = doc.layers.iter().filter(|l| matches!(l.payload, Payload::Raster { .. })).collect();
        let layer = if doc.layers.len() == 1 { raster.first() } else { raster.iter().find(|l| l.name == "Background") };
        let Some(layer_id) = layer.map(|l| l.layer_id.clone()) else {
            self.say("develop: no developed raster layer (Background) in the document".into());
            return;
        };
        let prepared = match raw_prepare(original, format, self.cancel) {
            Ok(p) => p,
            Err(e) => {
                self.say(format!("develop: the original raw does not decode: {}", e.code()));
                return;
            }
        };
        let digest: [u8; 32] = sha256(SRGB_V4_ICC);
        let mut base = prepared.recipe.clone();
        base.working_profile.sha256 = digest;
        base.output_profile.sha256 = digest;
        let identity = identity_values(&base);
        let mut develop = DevelopDoc { prepared, base, layer_id, last: identity.clone(), states: Vec::new(), threads: Threads::auto() };
        // Right after the import the layer holds the identity develop (same route, same recipe).
        if just_imported && let Some(tiles) = develop.layer_tiles(self) {
            develop.states.push((signature(tiles), identity));
        }
        self.say(format!("develop: {} sliders on layer {}", CONTROLS.len(), develop.layer_id));
        self.develop = Some(develop);
    }

    /// Whether the open document is a developed raw (the develop panel and export apply).
    pub fn develop_available(&self) -> bool {
        self.develop.is_some()
    }

    /// Worker threads of every develop run (`1..=hsk_studio_develop::MAX_THREADS`; default auto). The
    /// output does not depend on it (develop's contract; the CLI test compares 1 and 4).
    pub fn set_develop_threads(&mut self, count: usize) -> Result<(), Refusal> {
        let threads = Threads::new(count).map_err(|e| Refusal::develop(e, "threads"))?;
        let develop = self.develop.as_mut().ok_or_else(|| Refusal::new("not_a_raw_document", "open a camera raw or DNG first"))?;
        develop.threads = threads;
        Ok(())
    }

    /// The recipe of the shown pixels (develop's type; the file recipe plus the slider values).
    pub fn develop_recipe(&self) -> Option<DevelopRecipe> {
        let d = self.develop.as_ref()?;
        Some(recipe_with(&d.base, &d.current(self).0))
    }

    /// Structured develop state for agents (part of [`Viewer::state_json`]).
    pub fn develop_json(&self) -> Value {
        let Some(d) = self.develop.as_ref() else {
            return json!({"available": false});
        };
        let (values, known) = d.current(self);
        let recipe = recipe_with(&d.base, &values);
        json!({
            "available": true,
            "layer_id": d.layer_id,
            "window": [d.prepared.window.x, d.prepared.window.y, d.prepared.window.width, d.prepared.window.height],
            "values": CONTROLS.iter().zip(&values).map(|(c, v)| (c.id.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
            "values_known": known,
            "enables": {"enable_colour_adjustments": recipe.enables.enable_colour_adjustments, "enable_detail": recipe.enables.enable_detail},
            "controls": CONTROLS.iter().map(|c| json!({"id": c.id, "label": c.label, "field": c.field, "panel_range": [c.panel_range.0, c.panel_range.1], "gate": c.gate.key()})).collect::<Vec<_>>(),
            "threads": d.threads.get(),
            "states": d.states.len(),
            "export": {"profile": "sRGB-v4.icc", "profile_sha256": hex(&sha256(SRGB_V4_ICC)), "formats": ["png8", "png16", "jpeg"]},
        })
    }

    /// Sets one develop slider (see [`Viewer::set_develop_many`]).
    pub fn set_develop(&mut self, id: &str, value: f64) -> Result<u64, Refusal> {
        self.set_develop_many(&[(id, value)])
    }

    /// Sets develop sliders as ONE undoable step: the recipe is validated by develop, the raw is
    /// re-developed and re-tiled, the changed tile bodies are staged and committed as one chronicle
    /// `tiles` edit (op `develop`). A refusal (unknown control, develop's validation code, a stale
    /// layer layout) commits nothing. Returns the document revision.
    pub fn set_develop_many(&mut self, changes: &[(&str, f64)]) -> Result<u64, Refusal> {
        let result = self.set_develop_inner(changes);
        match &result {
            Ok(revision) => self.say(format!("develop {} (revision {revision})", changes.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" "))),
            Err(r) => self.say(format!("develop refused: {r}")),
        }
        result
    }

    fn set_develop_inner(&mut self, changes: &[(&str, f64)]) -> Result<u64, Refusal> {
        if self.run.snapshot().is_none() {
            return Err(Refusal::new("not_open", "nothing open"));
        }
        let d = self.develop.as_ref().ok_or_else(|| Refusal::new("not_a_raw_document", "the open document was not imported from a camera raw or DNG"))?;
        if changes.is_empty() {
            return Err(Refusal::new("nothing_to_change", "no develop control given"));
        }
        let (mut values, _) = d.current(self);
        for (id, value) in changes {
            let i = CONTROLS.iter().position(|c| c.id == *id).ok_or_else(|| Refusal::new("unknown_control", format!("{id}: not a develop control")))?;
            if let Some(slot) = values.get_mut(i) {
                *slot = *value;
            }
        }
        let recipe = recipe_with(&d.base, &values);
        recipe.validate().map_err(|e| Refusal::develop(e, "recipe"))?;
        let revision = self.run.snapshot().map_or(0, |s| s.document().revision);
        let (image, _) = hsk_studio_develop::develop_to_image_threads(&d.prepared.plane, &recipe, revision, d.threads, self.cancel).map_err(|e| Refusal::develop(e, "develop"))?;
        let (proposal, _) = raw_proposal(&d.prepared, image, &crate::harness::TrustAll, self.cancel).map_err(|e| Refusal::new(e.code(), format!("tiling: {e}")))?;
        let tiles = d.layer_tiles(self).ok_or_else(|| Refusal::new("develop_layer_missing", format!("layer {} is gone or not raster", d.layer_id)))?.to_vec();
        let layer_id = d.layer_id.clone();
        let mut edited = tiles.clone();
        let mut staged = Vec::new();
        for blob in &proposal.tiles {
            let i = tiles
                .iter()
                .position(|t| t.column == blob.column && t.row == blob.row && t.object_key == blob.object_key)
                .ok_or_else(|| Refusal::new("develop_tile_layout_changed", format!("no tile {} at ({}, {}) on {layer_id}", blob.object_key, blob.column, blob.row)))?;
            if tiles.get(i).is_some_and(|t| t.content_digest.digest == blob.content_sha256_hex) {
                continue;
            }
            let mut body = Vec::with_capacity(blob.colour.len().saturating_add(blob.alpha.len()));
            body.extend_from_slice(&blob.colour);
            body.extend_from_slice(&blob.alpha);
            let digest = self.run.stage_tile(body.clone()).map_err(|code| Refusal::new(code, "stage tile"))?;
            staged.push((digest.digest.clone(), body));
            if let Some(slot) = edited.get_mut(i) {
                slot.content_digest = digest;
            }
        }
        drop(proposal);
        let sig = signature(&edited);
        if !staged.is_empty() {
            self.assets.extend(staged);
            if !self.run.set_tiles("develop", &layer_id, edited) {
                return Err(Refusal::new("develop_commit_refused", self.op_error()));
            }
            self.dirty = true;
        }
        if let Some(d) = self.develop.as_mut() {
            d.states.retain(|(k, _)| *k != sig);
            d.states.push((sig, values.clone()));
            if d.states.len() > MAX_STATES {
                d.states.remove(0);
            }
            d.last = values;
        }
        Ok(self.run.snapshot().map_or(0, |s| s.document().revision))
    }

    /// Develops the raw with the recipe of the shown pixels through `develop_to_output` and writes it
    /// as PNG or JPEG with the output profile embedded (decoded back before an atomic write). Refuses
    /// the opened source path unless `overwrite`. Returns the export report.
    pub fn export_develop(&mut self, path: &Path, format: DevelopExport, overwrite: bool) -> Result<Value, Refusal> {
        let result = self.export_develop_inner(path, format, overwrite);
        match &result {
            Ok(report) => self.say(format!("exported developed raw {} ({} bytes, {})", path.display(), report["bytes"], format.name())),
            Err(r) => self.say(format!("develop export refused: {r}")),
        }
        result
    }

    fn export_develop_inner(&mut self, path: &Path, format: DevelopExport, overwrite: bool) -> Result<Value, Refusal> {
        if path.as_os_str().is_empty() {
            return Err(Refusal::new("export_path", "no output path"));
        }
        if self.is_source(path) && !overwrite {
            return Err(Refusal::new("export_over_source", format!("{} is the opened source (overwrite not set)", path.display())));
        }
        let d = self.develop.as_ref().ok_or_else(|| Refusal::new("not_a_raw_document", "the open document was not imported from a camera raw or DNG"))?;
        let (values, known) = d.current(self);
        let recipe = recipe_with(&d.base, &values);
        let depth = match format {
            DevelopExport::Png { depth } => depth,
            DevelopExport::Jpeg { .. } => OutputDepth::Eight,
        };
        let revision = self.run.snapshot().map_or(0, |s| s.document().revision);
        let (output, receipt) = hsk_studio_develop::develop_to_output(&d.prepared.plane, &recipe, revision, depth, SRGB_V4_ICC, d.threads, self.cancel)
            .map_err(|e| Refusal::develop(e, "develop_to_output"))?;
        let samples = match output.depth_bits {
            16 => raster::Samples::U16(output.data.chunks_exact(2).map(|b| u16::from_be_bytes([b[0], b[1]])).collect()),
            _ => raster::Samples::U8(output.data.clone()),
        };
        let (bytes, subsampling) = match format {
            DevelopExport::Png { .. } => {
                let req = raster::PngEncodeRequest {
                    width: output.width,
                    height: output.height,
                    colour: raster::Colour::Rgb,
                    samples: &samples,
                    icc: Some(&output.icc),
                    compression: raster::PngCompression::Balanced,
                };
                (raster::encode_png(&req).map_err(|e| Refusal::new(e.code(), "png encode"))?, None)
            }
            DevelopExport::Jpeg { quality } => {
                let (subsampling, name) = if quality >= 90 { (raster::JpegSubsampling::S444, "4:4:4") } else { (raster::JpegSubsampling::S420, "4:2:0") };
                let req = raster::JpegEncodeRequest {
                    width: output.width,
                    height: output.height,
                    colour: raster::Colour::Rgb,
                    samples: &samples,
                    quality,
                    subsampling,
                    icc: Some(&output.icc),
                };
                (raster::encode_jpeg(&req).map_err(|e| Refusal::new(e.code(), "jpeg encode"))?, Some(name))
            }
        };
        let back = raster::decode(&bytes, &raster::Limits::default(), self.cancel).map_err(|e| Refusal::new(e.code(), "decode back"))?;
        if (back.width, back.height) != (output.width, output.height) || back.icc.as_deref() != Some(SRGB_V4_ICC) {
            return Err(Refusal::new("export_verify_failed", "decoded export differs in size or profile"));
        }
        write_atomic(path, &bytes).map_err(|code| Refusal::new(code, path.display().to_string()))?;
        Ok(json!({
            "format": format.name(), "path": path.to_string_lossy(), "width": output.width, "height": output.height,
            "depth_bits": output.depth_bits, "bytes": bytes.len(), "file_sha256": hsk_studio_package::sha256_hex_of(&bytes),
            "samples_sha256": hsk_studio_package::sha256_hex_of(&output.data), "transfer": output.transfer,
            "icc_sha256": hex(&output.icc_sha256), "clip": {"below_zero": output.clip.below_zero, "above_one": output.clip.above_one},
            "subsampling": subsampling, "quality": match format { DevelopExport::Jpeg { quality } => Some(quality), DevelopExport::Png { .. } => None },
            "route": receipt.route, "profile_route": receipt.profile_route, "math_token": receipt.math_token,
            "threads": d.threads.get(), "revision": revision, "values_known": known,
            "values": CONTROLS.iter().zip(&values).map(|(c, v)| (c.id.to_string(), json!(v))).collect::<serde_json::Map<_, _>>(),
        }))
    }

    /// The develop panel: one `hsk-studio-controls` slider per develop control (stable ids = the
    /// control ids) and the two export buttons. Returns the committed slider changes.
    pub(super) fn develop_ui(&mut self, ui: &mut egui::Ui) -> Vec<(&'static str, f64)> {
        let mut out = Vec::new();
        let Some(d) = self.develop.as_ref() else {
            return out;
        };
        let (values, known) = d.current(self);
        let (registry, mut store) = match develop_panel(&values) {
            Ok(p) => p,
            Err(e) => {
                ui.colored_label(egui::Color32::RED, format!("develop panel: {}", e.code()));
                return out;
            }
        };
        if !known {
            ui.small("The layer's pixels are not a develop state of this session (painted, or a reopened package: the recipe is not saved). A slider change re-develops from the values shown.");
        }
        let outcome = hsk_studio_controls::egui_adapter::show_panel_and_apply(ui, &registry, &mut store, None);
        for change in outcome.changes.iter().filter(|c| c.is_undo_step()) {
            if let (Some(c), ControlValue::Number(v)) = (CONTROLS.iter().find(|c| c.id == change.control_id), &change.new) {
                out.push((c.id, *v));
            }
        }
        for c in CONTROLS {
            self.drawn.push(c.id.into());
        }
        ui.horizontal_wrapped(|ui| {
            let base = std::path::PathBuf::from(self.save_input.trim());
            if ui.push_id(super::widget::id(super::widget::DEVELOP_EXPORT_PNG), |ui| ui.button("Export developed PNG 16-bit")).inner.on_hover_text("develop_to_output, sRGB-v4 profile embedded, at the save path with extension .develop.png").clicked() {
                let _ = self.export_develop(&base.with_extension("develop.png"), DevelopExport::Png { depth: OutputDepth::Sixteen }, self.overwrite);
            }
            self.drawn.push(super::widget::DEVELOP_EXPORT_PNG.into());
            if ui.push_id(super::widget::id(super::widget::DEVELOP_EXPORT_JPEG), |ui| ui.button("Export developed JPEG")).inner.on_hover_text("develop_to_output 8-bit, quality 90, sRGB-v4 profile embedded, extension .develop.jpg").clicked() {
                let _ = self.export_develop(&base.with_extension("develop.jpg"), DevelopExport::Jpeg { quality: super::export::DEFAULT_JPEG_QUALITY }, self.overwrite);
            }
            self.drawn.push(super::widget::DEVELOP_EXPORT_JPEG.into());
        });
        out
    }
}

/// The develop sliders as `hsk-studio-controls` descriptors with `values` in the store.
pub fn develop_panel(values: &[f64]) -> Result<(Registry, ValueStore), ControlError> {
    let mut registry = Registry::new();
    registry.register_section(SectionSpec::new("develop", "Develop", "Camera raw develop recipe (hsk-studio-develop): every slider re-develops the raw as one undoable step."))?;
    for c in CONTROLS {
        let precision = if c.id == "develop.exposure" { 2 } else { 0 };
        registry.register(ControlSpec::number(c.id, c.label, c.help, NumberSpec::slider(precision, c.panel_range, c.panel_range), 0.0).in_group(&["develop"]))?;
        registry.register_consumer(c.id, "harness.develop.recipe")?;
    }
    let mut store = ValueStore::new(&registry);
    for (c, v) in CONTROLS.iter().zip(values) {
        // A value outside the panel range (set through the API) stays in the recipe; the slider
        // shows the default then.
        let _ = store.set(&registry, c.id, ControlValue::Number(*v), ChangeSource::Model, None);
    }
    Ok((registry, store))
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let hex = hsk_studio_package::sha256_hex_of(bytes);
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(hex.as_bytes().chunks_exact(2)) {
        *slot = std::str::from_utf8(pair).ok().and_then(|s| u8::from_str_radix(s, 16).ok()).unwrap_or(0);
    }
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
