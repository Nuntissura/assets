//! Develop panel and develop export (lanes SMB-HARNESS-DEVELOP-EXPORT and SMB-HARNESS-DEVELOP-2;
//! feature `viewer`).
//!
//! Flow: open a camera raw or DNG (the import develops it once into the `Background` raster layer)
//! -> adjust the develop edit -> `hsk_studio_develop::develop_to_output` -> PNG (8/16-bit) or JPEG with
//! the output ICC profile embedded.
//!
//! * The edit ([`DevelopEdit`]) is develop's own types: `DevelopRecipe::exposure_stops` and the whole
//!   `Adjustments` (lanes SMB-DEVELOP-CONTROLS and CONTROLS-2): white balance shift, raw-form kelvin /
//!   tint (`recipe_for_white_point` re-solves the file's DNG colour tags), basic tone, presence,
//!   colour grading, point colours, detail, local corrections with luminance- and colour-range masks.
//!   This module only writes those fields and the group enables; every number is validated and
//!   computed by `hsk-studio-develop` (`DevelopRecipe::validate`, `develop_to_image_threads`,
//!   `develop_to_output`). An out-of-range or non-finite value is develop's typed refusal: nothing is
//!   clamped here and nothing is committed.
//! * Every numeric control has a stable id ([`controls`]: `develop.exposure`,
//!   `develop.grading.shadows.hue`, `develop.point.0.hue_shift`, `develop.local.0.mask.0.lo`, ...);
//!   structure changes (kelvin on/off, add/remove a point colour or a range-masked correction) are
//!   [`DevelopOp`]s. Each accepted change re-develops the raw from the original import entry, tiles it
//!   with the import's own route (`import::raw_proposal`: interop-psd `raster_proposal`, same ids)
//!   and commits the changed tile bodies as ONE chronicle `tiles` edit (`Run::set_tiles`, op
//!   `develop`): undo/redo are the session's chronicle history (UI-2/UI-3). The edit of a pixel
//!   state is remembered by the signature of the layer's tile list, so undo/redo bring the matching
//!   edit back; with the identity edit the tiles equal the import's.
//! * Persistence: `Viewer::save` writes the session document entry `develop` (module
//!   [`super::develop_entry`]: the edit, the layer id and the tile signature); a reopened package
//!   restores it (no loss note: the session knows the token).
//! * Export renders `develop_to_output` (sRGB transfer over linear light with sRGB primaries) with
//!   the CC0 `sRGB-v4.icc` bound as the recipe's output profile, encodes through
//!   hsk-studio-interop-raster (PNG RGB 8/16-bit with iCCP, JPEG baseline 8-bit with APP2 ICC), decodes
//!   it back, then writes atomically. It exports the developed raw only, not the composite of other
//!   layers (the composite export is `Viewer::export`).
//!
//! Not in the panel (the recipe types carry them and the entry stores them): HSL mixer, parametric
//! curve, grayscale mix, gradient and brush masks (the entry refuses gradient/brush masks).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::develop_entry;
use super::export::write_atomic;
use crate::import::{RawPrepared, SourceFormat, raw_prepare, raw_proposal};
use hsk_studio_controls::{ChangeSource, ControlError, ControlSpec, NumberSpec, Registry, SectionSpec, Value as ControlValue, ValueStore};
use hsk_studio_develop::{
    AbsoluteWhiteBalance, Adjustments, BaselineExposure, ColourGrading, ColourRange, Correction, Detail, DevelopError, DevelopRecipe,
    LocalStack, LuminanceRange, Mask, MaskBlend, MaskSource, OutputDepth, PointColour, Threads,
};
use hsk_studio_folio::{Payload, TileRef};
use hsk_studio_interop_raster as raster;
use serde_json::{Value, json};
use std::path::Path;

/// The output profile of the develop export (CC0, see `profiles/README.md`).
pub const SRGB_V4_ICC: &[u8] = include_bytes!("profiles/sRGB-v4.icc");

/// Remembered pixel states (tile-list signature -> edit) per opened raw.
const MAX_STATES: usize = 64;

/// Kelvin / tint a raw-form white balance starts at when switched on.
pub const KELVIN_START: (f64, f64) = (5000.0, 0.0);

/// The develop panel's edit: develop's own types (the recipe is the file's recipe plus this).
#[derive(Clone, Debug, PartialEq)]
pub struct DevelopEdit {
    pub exposure_stops: f64,
    pub adjust: Adjustments,
}

/// One numeric control of the panel: a stable id addressing a field of [`DevelopEdit`], the panel's
/// slider range (develop's documented range where it has one; a value set through the API goes to
/// develop, which decides) and the section it is shown in.
#[derive(Clone, Debug, PartialEq)]
pub struct DevelopControl {
    pub id: String,
    pub label: String,
    pub section: &'static str,
    pub panel_range: (f64, f64),
    pub precision: u8,
}

const COUNT: (f64, f64) = (-100.0, 100.0);
const UNIT: (f64, f64) = (-1.0, 1.0);
const POSITIVE: (f64, f64) = (0.0, 100.0);

/// A fixed control: (id suffix, label, section, range, precision).
type Fixed = (&'static str, &'static str, &'static str, (f64, f64), u8);
const FIXED: &[Fixed] = &[
    ("exposure", "Exposure", "basic", (-5.0, 5.0), 2),
    ("temperature", "Temperature", "basic", COUNT, 0),
    ("tint", "Tint", "basic", COUNT, 0),
    ("contrast", "Contrast", "basic", COUNT, 0),
    ("highlights", "Highlights", "basic", COUNT, 0),
    ("shadows", "Shadows", "basic", COUNT, 0),
    ("whites", "Whites", "basic", COUNT, 0),
    ("blacks", "Blacks", "basic", COUNT, 0),
    ("vibrance", "Vibrance", "basic", COUNT, 0),
    ("saturation", "Saturation", "basic", COUNT, 0),
    ("clarity", "Clarity", "basic", COUNT, 0),
    ("texture", "Texture", "detail", COUNT, 0),
    ("dehaze", "Dehaze", "detail", COUNT, 0),
    ("sharpen_amount", "Sharpening", "detail", (0.0, 150.0), 0),
    ("sharpen_radius", "Sharpen radius", "detail", (0.5, 3.0), 1),
    ("sharpen_detail", "Sharpen detail", "detail", POSITIVE, 0),
    ("sharpen_masking", "Sharpen masking", "detail", POSITIVE, 0),
    ("luminance_noise", "Luminance NR", "detail", POSITIVE, 0),
    ("colour_noise", "Colour NR", "detail", POSITIVE, 0),
    ("grading.blending", "Grading blending", "grading", POSITIVE, 0),
    ("grading.balance", "Grading balance", "grading", COUNT, 0),
];
const WHEELS: [&str; 4] = ["shadows", "midtones", "highlights", "global"];
const POINT_FIELDS: [(&str, (f64, f64), u8); 9] = [
    ("source_r", (0.0, 1.0), 3),
    ("source_g", (0.0, 1.0), 3),
    ("source_b", (0.0, 1.0), 3),
    ("hue_shift", COUNT, 0),
    ("saturation", COUNT, 0),
    ("luminance", COUNT, 0),
    ("hue_range", POSITIVE, 0),
    ("saturation_range", POSITIVE, 0),
    ("luminance_range", POSITIVE, 0),
];
const STACK_FIELDS: [&str; 10] = ["exposure", "contrast", "highlights", "shadows", "whites", "blacks", "clarity", "saturation", "temperature", "tint"];

/// Every numeric control of `edit`, in panel order (the list follows the edit's point colours,
/// corrections and masks; the kelvin pair exists while a raw-form white balance is set).
pub fn controls(edit: &DevelopEdit) -> Vec<DevelopControl> {
    let c = |id: String, label: String, section: &'static str, panel_range: (f64, f64), precision: u8| DevelopControl { id, label, section, panel_range, precision };
    let mut out: Vec<DevelopControl> = FIXED.iter().map(|(id, label, section, range, p)| c(format!("develop.{id}"), (*label).to_string(), section, *range, *p)).collect();
    if edit.adjust.absolute_white_balance.is_some() {
        out.push(c("develop.wb_kelvin".into(), "Kelvin".into(), "white", (2000.0, 25000.0), 0));
        out.push(c("develop.wb_tint".into(), "Kelvin tint".into(), "white", (-150.0, 150.0), 0));
    }
    for w in WHEELS {
        out.push(c(format!("develop.grading.{w}.hue"), format!("{w} hue"), "grading", (0.0, 360.0), 0));
        out.push(c(format!("develop.grading.{w}.saturation"), format!("{w} saturation"), "grading", POSITIVE, 0));
        out.push(c(format!("develop.grading.{w}.luminance"), format!("{w} luminance"), "grading", COUNT, 0));
    }
    for i in 0..edit.adjust.point_colours.len() {
        for (f, range, p) in POINT_FIELDS {
            out.push(c(format!("develop.point.{i}.{f}"), format!("point {i} {f}"), "point", range, p));
        }
    }
    for (i, corr) in edit.adjust.local.iter().enumerate() {
        out.push(c(format!("develop.local.{i}.amount"), format!("local {i} amount"), "local", (0.0, 4.0), 2));
        for f in STACK_FIELDS {
            out.push(c(format!("develop.local.{i}.{f}"), format!("local {i} {f}"), "local", UNIT, 2));
        }
        for (j, m) in corr.masks.iter().enumerate() {
            out.push(c(format!("develop.local.{i}.mask.{j}.value"), format!("local {i} mask {j} value"), "local", (0.0, 1.0), 2));
            let fields: &[&str] = match m.source {
                MaskSource::LuminanceRange(_) => &["lo", "hi", "feather"],
                MaskSource::ColourRange(_) => &["breadth"],
                _ => &[],
            };
            for f in fields {
                out.push(c(format!("develop.local.{i}.mask.{j}.{f}"), format!("local {i} mask {j} {f}"), "local", (0.0, 1.0), 3));
            }
        }
    }
    out
}

fn index(s: &str) -> Option<usize> {
    s.parse::<usize>().ok()
}

/// The develop field a control id writes.
fn slot<'a>(e: &'a mut DevelopEdit, id: &str) -> Option<&'a mut f64> {
    let rest = id.strip_prefix("develop.")?;
    if rest == "exposure" {
        return Some(&mut e.exposure_stops);
    }
    let a = &mut e.adjust;
    if let Some(r) = rest.strip_prefix("point.") {
        let (i, f) = r.split_once('.')?;
        let p = a.point_colours.get_mut(index(i)?)?;
        return Some(match f {
            "source_r" => &mut p.source[0],
            "source_g" => &mut p.source[1],
            "source_b" => &mut p.source[2],
            "hue_shift" => &mut p.hue_shift,
            "saturation" => &mut p.saturation,
            "luminance" => &mut p.luminance,
            "hue_range" => &mut p.hue_range,
            "saturation_range" => &mut p.saturation_range,
            "luminance_range" => &mut p.luminance_range,
            _ => return None,
        });
    }
    if let Some(r) = rest.strip_prefix("local.") {
        let (i, f) = r.split_once('.')?;
        let corr = a.local.get_mut(index(i)?)?;
        if let Some(m) = f.strip_prefix("mask.") {
            let (j, g) = m.split_once('.')?;
            let mask = corr.masks.get_mut(index(j)?)?;
            if g == "value" {
                return Some(&mut mask.value);
            }
            return match (&mut mask.source, g) {
                (MaskSource::LuminanceRange(l), "lo") => Some(&mut l.lo),
                (MaskSource::LuminanceRange(l), "hi") => Some(&mut l.hi),
                (MaskSource::LuminanceRange(l), "feather") => Some(&mut l.feather),
                (MaskSource::ColourRange(c), "breadth") => Some(&mut c.breadth),
                _ => None,
            };
        }
        let s = &mut corr.stack;
        return Some(match f {
            "amount" => &mut corr.amount,
            "exposure" => &mut s.exposure,
            "contrast" => &mut s.contrast,
            "highlights" => &mut s.highlights,
            "shadows" => &mut s.shadows,
            "whites" => &mut s.whites,
            "blacks" => &mut s.blacks,
            "clarity" => &mut s.clarity,
            "saturation" => &mut s.saturation,
            "temperature" => &mut s.temperature,
            "tint" => &mut s.tint,
            _ => return None,
        });
    }
    if let Some(r) = rest.strip_prefix("grading.") {
        let g = &mut a.grading;
        if let Some((w, f)) = r.split_once('.') {
            let wheel = match w {
                "shadows" => &mut g.shadows,
                "midtones" => &mut g.midtones,
                "highlights" => &mut g.highlights,
                "global" => &mut g.global,
                _ => return None,
            };
            return Some(match f {
                "hue" => &mut wheel.hue,
                "saturation" => &mut wheel.saturation,
                "luminance" => &mut wheel.luminance,
                _ => return None,
            });
        }
        return Some(match r {
            "blending" => &mut g.blending,
            "balance" => &mut g.balance,
            _ => return None,
        });
    }
    let d = &mut a.detail;
    Some(match rest {
        "temperature" => &mut a.white_balance.temperature,
        "tint" => &mut a.white_balance.tint,
        "contrast" => &mut a.tone.contrast,
        "highlights" => &mut a.tone.highlights,
        "shadows" => &mut a.tone.shadows,
        "whites" => &mut a.tone.whites,
        "blacks" => &mut a.tone.blacks,
        "vibrance" => &mut a.presence.vibrance,
        "saturation" => &mut a.presence.saturation,
        "clarity" => &mut a.presence.clarity,
        "texture" => &mut d.texture,
        "dehaze" => &mut d.dehaze,
        "sharpen_amount" => &mut d.sharpen.amount,
        "sharpen_radius" => &mut d.sharpen.radius,
        "sharpen_detail" => &mut d.sharpen.detail,
        "sharpen_masking" => &mut d.sharpen.masking,
        "luminance_noise" => &mut d.noise.luminance,
        "colour_noise" => &mut d.noise.colour,
        "wb_kelvin" => &mut a.absolute_white_balance.as_mut()?.kelvin,
        "wb_tint" => &mut a.absolute_white_balance.as_mut()?.tint,
        _ => return None,
    })
}

/// Structure changes of the edit (each one undoable step, like a slider change).
#[derive(Clone, Debug, PartialEq)]
pub enum DevelopOp {
    /// Raw-form white balance at kelvin / tint (STU-RAW-110; develop re-solves the DNG colour tags).
    Kelvin { kelvin: f64, tint: f64 },
    /// Back to the file's as-shot white.
    AsShotWhite,
    /// A point colour sampled at `source` (encoded sRGB 0..1), develop's default ranges.
    AddPointColour { source: [f64; 3] },
    RemovePointColour(usize),
    /// A local correction masked by an encoded-luma window (develop `MaskSource::LuminanceRange`).
    AddLuminanceRange { lo: f64, hi: f64, feather: f64 },
    /// A local correction masked by colour samples (develop `MaskSource::ColourRange`).
    AddColourRange { samples: Vec<[f64; 3]>, breadth: f64 },
    RemoveCorrection(usize),
}

impl DevelopOp {
    fn label(&self) -> String {
        match self {
            Self::Kelvin { kelvin, tint } => format!("kelvin {kelvin} tint {tint}"),
            Self::AsShotWhite => "as-shot white".into(),
            Self::AddPointColour { .. } => "add point colour".into(),
            Self::RemovePointColour(i) => format!("remove point colour {i}"),
            Self::AddLuminanceRange { .. } => "add luminance-range correction".into(),
            Self::AddColourRange { .. } => "add colour-range correction".into(),
            Self::RemoveCorrection(i) => format!("remove correction {i}"),
        }
    }

    fn apply(&self, e: &mut DevelopEdit) -> Result<(), Refusal> {
        let a = &mut e.adjust;
        let range = |what: &str, i: usize, len: usize| Refusal::new("unknown_item", format!("{what} {i}: there are {len}"));
        let mask = |n: usize, source: MaskSource| Mask { id: format!("m{n}"), active: true, inverted: false, value: 1.0, blend: MaskBlend::Add, source };
        let next_id = |a: &Adjustments| a.local.iter().filter_map(|c| c.id.strip_prefix('c').and_then(index)).max().map_or(1, |n| n.saturating_add(1));
        match self {
            Self::Kelvin { kelvin, tint } => a.absolute_white_balance = Some(AbsoluteWhiteBalance { kelvin: *kelvin, tint: *tint }),
            Self::AsShotWhite => a.absolute_white_balance = None,
            Self::AddPointColour { source } => {
                if a.point_colours.len() >= hsk_studio_develop::MAX_POINT_COLOURS {
                    return Err(Refusal::new("develop_limit", format!("at most {} point colours", hsk_studio_develop::MAX_POINT_COLOURS)));
                }
                a.point_colours.push(PointColour::new(*source));
            }
            Self::RemovePointColour(i) => {
                if *i >= a.point_colours.len() {
                    return Err(range("point colour", *i, a.point_colours.len()));
                }
                a.point_colours.remove(*i);
            }
            Self::AddLuminanceRange { lo, hi, feather } => {
                let n = next_id(a);
                a.local.push(Correction::new(&format!("c{n}"), vec![mask(1, MaskSource::LuminanceRange(LuminanceRange { lo: *lo, hi: *hi, feather: *feather }))], LocalStack::default()));
            }
            Self::AddColourRange { samples, breadth } => {
                let n = next_id(a);
                a.local.push(Correction::new(&format!("c{n}"), vec![mask(1, MaskSource::ColourRange(ColourRange { samples: samples.clone(), breadth: *breadth }))], LocalStack::default()));
            }
            Self::RemoveCorrection(i) => {
                if *i >= a.local.len() {
                    return Err(range("correction", *i, a.local.len()));
                }
                a.local.remove(*i);
            }
        }
        if a.local.len() > hsk_studio_develop::local::MAX_CORRECTIONS {
            return Err(Refusal::new("develop_limit", format!("at most {} corrections", hsk_studio_develop::local::MAX_CORRECTIONS)));
        }
        Ok(())
    }
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

/// How the develop entry of a reopened package was handled.
#[derive(Clone, Debug, PartialEq, Eq)]
enum EntryStatus {
    /// A fresh import (no entry expected).
    Imported,
    Absent,
    /// Restored; its tile signature matches the layer.
    Restored,
    /// Decoded and valid, but saved for other pixels (or none): the edit is shown, values_known false.
    Stale,
    Refused(&'static str),
}

/// The develop state of an opened raw.
pub struct DevelopDoc {
    prepared: RawPrepared,
    /// The file's as-shot recipe with the export profile bound (identity adjustments).
    base: DevelopRecipe,
    layer_id: String,
    /// The last committed (or opened) edit.
    last: DevelopEdit,
    states: Vec<(String, DevelopEdit)>,
    threads: Threads,
    entry: EntryStatus,
}

/// Signature of a raster layer's tile list (placement and content digests, in order).
fn signature(tiles: &[TileRef]) -> String {
    let mut text = String::new();
    for t in tiles {
        text.push_str(&format!("{},{},{},{};", t.object_key, t.column, t.row, t.content_digest.digest));
    }
    hsk_studio_package::sha256_hex_of(text.as_bytes())
}

impl DevelopDoc {
    /// The recipe of `edit`: the file recipe (re-solved for a raw-form white balance), the edit's
    /// exposure and adjustments, the group enables of the active families; validated by develop.
    fn recipe(&self, edit: &DevelopEdit) -> Result<DevelopRecipe, Refusal> {
        let mut recipe = match edit.adjust.absolute_white_balance {
            Some(w) => {
                let (r, _) = hsk_studio_develop::recipe_for_white_point(
                    &self.prepared.colour,
                    &self.prepared.to_profile,
                    self.base.working_profile.clone(),
                    BaselineExposure::Ignore,
                    w.kelvin,
                    w.tint,
                )
                .map_err(|e| Refusal::develop(e, "kelvin white balance"))?;
                r
            }
            None => self.base.clone(),
        };
        recipe.working_profile = self.base.working_profile.clone();
        recipe.output_profile = self.base.output_profile.clone();
        recipe.exposure_stops = edit.exposure_stops;
        recipe.adjust = edit.adjust.clone();
        let a = &edit.adjust;
        let colour = a.presence.vibrance != 0.0 || a.presence.saturation != 0.0 || !a.point_colours.is_empty() || a.grading != ColourGrading::default();
        let detail = a.presence.clarity != 0.0 || a.detail != Detail::default();
        let e = &mut recipe.enables;
        e.enable_colour_adjustments = self.base.enables.enable_colour_adjustments || colour;
        e.enable_detail = self.base.enables.enable_detail || detail;
        e.enable_mask_group_based_corrections = self.base.enables.enable_mask_group_based_corrections || !a.local.is_empty();
        recipe.validate().map_err(|e| Refusal::develop(e, "recipe"))?;
        Ok(recipe)
    }

    fn layer_tiles<'a>(&self, viewer: &'a Viewer) -> Option<&'a [TileRef]> {
        let snap = viewer.run.snapshot()?;
        let layer = snap.document().layers.iter().find(|l| l.layer_id == self.layer_id)?;
        match &layer.payload {
            Payload::Raster { tiles } => Some(tiles),
            _ => None,
        }
    }

    /// The edit of the shown pixels and whether it is known (a committed, imported or restored state).
    fn current(&self, viewer: &Viewer) -> (DevelopEdit, bool) {
        let sig = self.layer_tiles(viewer).map(signature);
        match sig.and_then(|s| self.states.iter().rev().find(|(k, _)| *k == s)) {
            Some((_, e)) => (e.clone(), true),
            None => (self.last.clone(), false),
        }
    }
}

fn values_json(edit: &DevelopEdit) -> serde_json::Map<String, Value> {
    let mut probe = edit.clone();
    controls(edit).into_iter().map(|c| (c.id.clone(), json!(slot(&mut probe, &c.id).map(|v| *v)))).collect()
}

impl Viewer {
    /// Attaches the develop state after an open: only for a camera raw / DNG import (receipt
    /// `source_format` `dng` or `camera_raw`) whose package holds the original bytes. A reopened
    /// package restores the session document entry `develop`.
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
        let identity = DevelopEdit { exposure_stops: base.exposure_stops, adjust: base.adjust.clone() };
        let mut develop = DevelopDoc { prepared, base, layer_id, last: identity.clone(), states: Vec::new(), threads: Threads::auto(), entry: EntryStatus::Imported };
        let sig = develop.layer_tiles(self).map(signature);
        if just_imported {
            // Right after the import the layer holds the identity develop (same route, same recipe).
            if let Some(sig) = sig {
                develop.states.push((sig, identity));
            }
        } else {
            develop.entry = match self.run.document_entry("develop").map(develop_entry::decode) {
                None => EntryStatus::Absent,
                Some(Err(r)) => EntryStatus::Refused(r.code),
                Some(Ok(stored)) => match develop.recipe(&stored.edit) {
                    Err(r) => EntryStatus::Refused(r.code),
                    Ok(_) => {
                        let matches = stored.layer_id == develop.layer_id && stored.tiles_signature.is_some() && stored.tiles_signature == sig;
                        if matches && let Some(sig) = sig {
                            develop.states.push((sig, stored.edit.clone()));
                        }
                        develop.last = stored.edit;
                        if matches { EntryStatus::Restored } else { EntryStatus::Stale }
                    }
                },
            };
            if let EntryStatus::Refused(code) = develop.entry {
                self.say(format!("develop: the saved develop entry is refused ({code}); the panel starts from the file recipe"));
            }
        }
        self.say(format!("develop: panel on layer {} (entry {:?})", develop.layer_id, develop.entry));
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

    /// The edit of the shown pixels.
    pub fn develop_edit(&self) -> Option<DevelopEdit> {
        let d = self.develop.as_ref()?;
        Some(d.current(self).0)
    }

    /// The recipe of the shown pixels (develop's type: the file recipe plus the edit).
    pub fn develop_recipe(&self) -> Option<DevelopRecipe> {
        let d = self.develop.as_ref()?;
        d.recipe(&d.current(self).0).ok()
    }

    /// Structured develop state for agents (part of [`Viewer::state_json`]).
    pub fn develop_json(&self) -> Value {
        let Some(d) = self.develop.as_ref() else {
            return json!({"available": false});
        };
        let (edit, known) = d.current(self);
        let recipe = d.recipe(&edit).ok();
        let a = &edit.adjust;
        json!({
            "available": true,
            "layer_id": d.layer_id,
            "window": [d.prepared.window.x, d.prepared.window.y, d.prepared.window.width, d.prepared.window.height],
            "values": values_json(&edit),
            "values_known": known,
            "entry": format!("{:?}", d.entry),
            "white_balance": a.absolute_white_balance.map_or_else(|| json!("as_shot"), |w| json!({"kelvin": w.kelvin, "tint": w.tint})),
            "point_colours": a.point_colours.iter().map(|p| json!(p.source)).collect::<Vec<_>>(),
            "local": a.local.iter().map(|c| json!({"id": c.id, "masks": c.masks.iter().map(|m| match m.source { MaskSource::LuminanceRange(_) => "luminance_range", MaskSource::ColourRange(_) => "colour_range", _ => "other" }).collect::<Vec<_>>()})).collect::<Vec<_>>(),
            "enables": recipe.as_ref().map(|r| json!({"enable_colour_adjustments": r.enables.enable_colour_adjustments, "enable_detail": r.enables.enable_detail, "enable_mask_group_based_corrections": r.enables.enable_mask_group_based_corrections})),
            "controls": controls(&edit).iter().map(|c| json!({"id": c.id, "label": c.label, "section": c.section, "panel_range": [c.panel_range.0, c.panel_range.1]})).collect::<Vec<_>>(),
            "threads": d.threads.get(),
            "states": d.states.len(),
            "export": {"profile": "sRGB-v4.icc", "profile_sha256": hex(&sha256(SRGB_V4_ICC)), "formats": ["png8", "png16", "jpeg"]},
        })
    }

    /// Sets one develop control (see [`Viewer::set_develop_many`]).
    pub fn set_develop(&mut self, id: &str, value: f64) -> Result<u64, Refusal> {
        self.set_develop_many(&[(id, value)])
    }

    /// Sets develop controls as ONE undoable step: develop validates the recipe, the raw is
    /// re-developed and re-tiled, the changed tile bodies are staged and committed as one chronicle
    /// `tiles` edit (op `develop`). Writing `develop.wb_kelvin` / `develop.wb_tint` while the white
    /// balance is as-shot switches to raw-form white at [`KELVIN_START`] first. A refusal (unknown
    /// control, develop's validation code, a stale layer layout) commits nothing. Returns the revision.
    pub fn set_develop_many(&mut self, changes: &[(&str, f64)]) -> Result<u64, Refusal> {
        let label = changes.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ");
        let result = self.develop_change(|edit| {
            if changes.is_empty() {
                return Err(Refusal::new("nothing_to_change", "no develop control given"));
            }
            for (id, value) in changes {
                if matches!(*id, "develop.wb_kelvin" | "develop.wb_tint") && edit.adjust.absolute_white_balance.is_none() {
                    edit.adjust.absolute_white_balance = Some(AbsoluteWhiteBalance { kelvin: KELVIN_START.0, tint: KELVIN_START.1 });
                }
                *slot(edit, id).ok_or_else(|| Refusal::new("unknown_control", format!("{id}: not a develop control of this edit")))? = *value;
            }
            Ok(())
        });
        self.report_change(&label, &result);
        result
    }

    /// A structure change of the edit as ONE undoable step (see [`DevelopOp`]).
    pub fn develop_op(&mut self, op: DevelopOp) -> Result<u64, Refusal> {
        let label = op.label();
        let result = self.develop_change(|edit| op.apply(edit));
        self.report_change(&label, &result);
        result
    }

    fn report_change(&mut self, label: &str, result: &Result<u64, Refusal>) {
        match result {
            Ok(revision) => self.say(format!("develop {label} (revision {revision})")),
            Err(r) => self.say(format!("develop {label} refused: {r}")),
        }
    }

    fn develop_change(&mut self, change: impl FnOnce(&mut DevelopEdit) -> Result<(), Refusal>) -> Result<u64, Refusal> {
        if self.run.snapshot().is_none() {
            return Err(Refusal::new("not_open", "nothing open"));
        }
        let d = self.develop.as_ref().ok_or_else(|| Refusal::new("not_a_raw_document", "the open document was not imported from a camera raw or DNG"))?;
        let (mut edit, _) = d.current(self);
        change(&mut edit)?;
        let recipe = d.recipe(&edit)?;
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
            d.states.push((sig, edit.clone()));
            if d.states.len() > MAX_STATES {
                d.states.remove(0);
            }
            d.last = edit;
        }
        Ok(self.run.snapshot().map_or(0, |s| s.document().revision))
    }

    /// Writes (or removes) the session document entry `develop` for the next save: the edit of the
    /// shown pixels with their tile signature, or the last edit without one when the pixels are not
    /// a develop state. Nothing when the document is not a developed raw.
    pub(super) fn store_develop_entry(&mut self) -> Result<(), Refusal> {
        let Some(d) = self.develop.as_ref() else {
            return Ok(());
        };
        let (edit, known) = d.current(self);
        let sig = if known { d.layer_tiles(self).map(signature) } else { None };
        let bytes = develop_entry::encode(&d.layer_id, sig.as_deref(), &edit)?;
        self.run.set_document_entry("develop", Some(bytes)).map_err(|code| Refusal::new(code, "session document entry develop"))
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
        let (edit, known) = d.current(self);
        let recipe = d.recipe(&edit)?;
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
            "values": values_json(&edit),
        }))
    }

    /// Encoded sRGB of the composite's centre pixel (the sample of new point colours and colour
    /// ranges); mid grey when nothing is rendered.
    fn centre_sample(&self) -> [f64; 3] {
        let Some(f) = self.frame.as_ref().filter(|f| !f.rgba8.is_empty()) else {
            return [0.5, 0.5, 0.5];
        };
        let at = (u64::from(f.height) / 2).saturating_mul(u64::from(f.width)).saturating_add(u64::from(f.width) / 2).saturating_mul(4);
        let i = usize::try_from(at).unwrap_or(0);
        match f.rgba8.get(i..i.saturating_add(3)) {
            Some(p) => [f64::from(p[0]) / 255.0, f64::from(p[1]) / 255.0, f64::from(p[2]) / 255.0],
            None => [0.5, 0.5, 0.5],
        }
    }

    /// The develop panel: one `hsk-studio-controls` slider per control of the edit (stable ids = the
    /// control ids), structure buttons and the two export buttons. Returns the committed changes.
    pub(super) fn develop_ui(&mut self, ui: &mut egui::Ui) -> (Vec<(String, f64)>, Vec<DevelopOp>) {
        let (mut changes, mut ops) = (Vec::new(), Vec::new());
        let Some(d) = self.develop.as_ref() else {
            return (changes, ops);
        };
        let (edit, known) = d.current(self);
        let (registry, mut store) = match develop_panel(&edit) {
            Ok(p) => p,
            Err(e) => {
                ui.colored_label(egui::Color32::RED, format!("develop panel: {}", e.code()));
                return (changes, ops);
            }
        };
        if !known {
            ui.small("The layer's pixels are not a develop state of this session (painted, or a saved entry for other pixels). A change re-develops from the edit shown.");
        }
        let outcome = hsk_studio_controls::egui_adapter::show_panel_and_apply(ui, &registry, &mut store, None);
        for change in outcome.changes.iter().filter(|c| c.is_undo_step()) {
            if let ControlValue::Number(v) = &change.new {
                changes.push((change.control_id.clone(), *v));
            }
        }
        for c in controls(&edit) {
            self.drawn.push(c.id);
        }
        let sample = self.centre_sample();
        let w = super::widget::id;
        ui.horizontal_wrapped(|ui| {
            let mut button = |name: &'static str, label: &str, hover: &str, op: DevelopOp, ops: &mut Vec<DevelopOp>, drawn: &mut Vec<String>| {
                if ui.push_id(w(name), |ui| ui.button(label)).inner.on_hover_text(hover).clicked() {
                    ops.push(op);
                }
                drawn.push(name.into());
            };
            let mut drawn = Vec::new();
            if edit.adjust.absolute_white_balance.is_some() {
                button(super::widget::DEVELOP_AS_SHOT, "As-shot white", "Back to the file's as-shot white balance", DevelopOp::AsShotWhite, &mut ops, &mut drawn);
            } else {
                button(super::widget::DEVELOP_KELVIN, "Kelvin white", "Raw-form white balance (kelvin / tint, develop re-solves the DNG colour)", DevelopOp::Kelvin { kelvin: KELVIN_START.0, tint: KELVIN_START.1 }, &mut ops, &mut drawn);
            }
            button(super::widget::DEVELOP_ADD_POINT, "Add point colour", "Point colour sampled at the image centre", DevelopOp::AddPointColour { source: sample }, &mut ops, &mut drawn);
            button(super::widget::DEVELOP_ADD_LUMINANCE, "Add luminance mask", "Local correction masked by a luminance range (highlights 0.6..1)", DevelopOp::AddLuminanceRange { lo: 0.6, hi: 1.0, feather: 0.2 }, &mut ops, &mut drawn);
            button(super::widget::DEVELOP_ADD_COLOUR, "Add colour mask", "Local correction masked by the colour at the image centre", DevelopOp::AddColourRange { samples: vec![sample], breadth: 0.3 }, &mut ops, &mut drawn);
            self.drawn.extend(drawn);
        });
        for i in 0..edit.adjust.point_colours.len() {
            let name = format!("develop.point.{i}.remove");
            if ui.push_id(w(&name), |ui| ui.small_button(format!("Remove point colour {i}"))).inner.clicked() {
                ops.push(DevelopOp::RemovePointColour(i));
            }
            self.drawn.push(name);
        }
        for i in 0..edit.adjust.local.len() {
            let name = format!("develop.local.{i}.remove");
            if ui.push_id(w(&name), |ui| ui.small_button(format!("Remove correction {i}"))).inner.clicked() {
                ops.push(DevelopOp::RemoveCorrection(i));
            }
            self.drawn.push(name);
        }
        ui.horizontal_wrapped(|ui| {
            let base = std::path::PathBuf::from(self.save_input.trim());
            if ui.push_id(w(super::widget::DEVELOP_EXPORT_PNG), |ui| ui.button("Export developed PNG 16-bit")).inner.on_hover_text("develop_to_output, sRGB-v4 profile embedded, at the save path with extension .develop.png").clicked() {
                let _ = self.export_develop(&base.with_extension("develop.png"), DevelopExport::Png { depth: OutputDepth::Sixteen }, self.overwrite);
            }
            self.drawn.push(super::widget::DEVELOP_EXPORT_PNG.into());
            if ui.push_id(w(super::widget::DEVELOP_EXPORT_JPEG), |ui| ui.button("Export developed JPEG")).inner.on_hover_text("develop_to_output 8-bit, quality 90, sRGB-v4 profile embedded, extension .develop.jpg").clicked() {
                let _ = self.export_develop(&base.with_extension("develop.jpg"), DevelopExport::Jpeg { quality: super::export::DEFAULT_JPEG_QUALITY }, self.overwrite);
            }
            self.drawn.push(super::widget::DEVELOP_EXPORT_JPEG.into());
        });
        (changes, ops)
    }
}

/// The controls of `edit` as `hsk-studio-controls` descriptors with the edit's values in the store.
pub fn develop_panel(edit: &DevelopEdit) -> Result<(Registry, ValueStore), ControlError> {
    let mut registry = Registry::new();
    for (id, label, help) in [
        ("basic", "Basic", "Exposure, white balance shift, tone and presence."),
        ("white", "White balance", "Raw-form kelvin / tint (develop re-solves the DNG colour tags)."),
        ("grading", "Colour grading", "Shadows, midtones, highlights and global wheels, blending, balance."),
        ("point", "Point colour", "Hue, saturation and luminance shifts of sampled colours."),
        ("detail", "Detail", "Texture, dehaze, sharpening and noise reduction."),
        ("local", "Local corrections", "Corrections masked by luminance or colour ranges."),
    ] {
        registry.register_section(SectionSpec::new(id, label, help))?;
    }
    let mut probe = edit.clone();
    let mut values = Vec::new();
    for c in controls(edit) {
        let (lo, hi) = c.panel_range;
        let value = slot(&mut probe, &c.id).map_or(lo, |v| *v);
        registry.register(ControlSpec::number(&c.id, &c.label, "Develop recipe field (hsk-studio-develop validates it).", NumberSpec::slider(c.precision, c.panel_range, c.panel_range), value.clamp(lo, hi)).in_group(&[c.section]))?;
        registry.register_consumer(&c.id, "harness.develop.recipe")?;
        values.push((c.id, value));
    }
    let mut store = ValueStore::new(&registry);
    for (id, v) in values {
        // A value outside the panel range (set through the API) stays in the edit; the slider shows
        // its clamped default then.
        let _ = store.set(&registry, &id, ControlValue::Number(v), ChangeSource::Model, None);
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
