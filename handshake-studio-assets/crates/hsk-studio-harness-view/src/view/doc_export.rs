//! Document export recipe (lane SMB-DOC-EXPORT-1; scenario photo-retouch steps 105-110, 112, 116): one recipe
//! ([`DocExportRecipe`], the `StudioExportRecipe` surface of STU-RAS-042 for a raster document) writes the
//! composite as PNG, JPEG, TIFF or layered PSD, with an optional crop, the develop export stage (image sizing,
//! output sharpening, graphic watermark: `hsk_studio_develop::apply_export_stage`, STU-IO-134/135/136, the same
//! stage and field texts as the develop export dialog) and an optional CMYK conversion through Prism's wide path
//! into the caller's printer profile. No egui; the window and the batch runner call the same entry
//! ([`Viewer::export_document`], [`RecipeTargets`]).
//!
//! Pipeline, in order:
//! 1. The composite in linear light ([`super::render::render_linear_with_records`], render-cpu): the viewer's own
//!    plan, so the file shows what the canvas shows. Exports are derived deliverables: the document is unchanged.
//! 2. Crop (`crop`, canvas pixels of the frame): an integer copy. A PSD keeps every layer's pixels outside the
//!    crop (layer rectangles past the canvas), so nothing is deleted from the layered master.
//! 3. The export stage when any of its groups is on (`any_group_on`): sizing, sharpening at the output size,
//!    watermark, in linear light. A frame with transparency is flattened over white first (note
//!    `alpha_flattened_on_white`): develop's stage is RGB. The stage's resolution is written to TIFF and recorded
//!    for every format even when no group acts. A PSD refuses an acting stage (`psd_export_stage_unsupported`):
//!    resizing or sharpening a layered master is not an export (STU-RAS-142 is a document edit).
//! 4. Colour. sRGB: the IEC 61966-2-1 transfer at 8 or 16 bits, tagged with the CC0 `sRGB-v4.icc` (PNG iCCP, JPEG
//!    APP2, TIFF 34675, PSD 1039). CMYK (TIFF only; other formats refuse `cmyk_format_unsupported`): the linear
//!    frame (alpha flattened on white) through `WideEngine` from the linear sRGB working profile into the given
//!    CMYK profile with the given intent and black point compensation, chunked at `MAX_WIDE_PIXELS`; ink values
//!    clamped to 0..1 (counted), tagged with that profile; the report carries the largest total ink in percent
//!    and the pixel count over 300 % (Studio reading of a common press limit, not a spec bound).
//! 5. Encode through the codec owners (interop-raster, interop-tiff, interop-psd `compose_psd`), decode back
//!    (dimensions must match, `export_verify_failed`), then write through `export.rs`'s unique temp: replace with
//!    `overwrite`, else no-clobber (`destination_exists`).
//!
//! Layered PSD (STU-RAS-043): every stacked raster layer becomes a pixel record with its own tile pixels
//! (sRGB-encoded), name, visibility, opacity, fill opacity, blend mode and clipping; groups become folder records
//! (pass-through kept). What PSD cannot take from this writer is a typed loss in the report, never dropped
//! silently: other layer kinds (text, vector, adjustment, fill) `psd_layer_kind_not_exported` (their pixels are in
//! the merged composite), masks `psd_layer_mask_not_exported`, a blend mode without a PSD key
//! `psd_blend_mode_written_normal`, layers outside the graph stack `psd_layer_not_in_stack`.
//!
//! Hostile input: recipe keys are closed (`recipe_key_unknown`); profile and watermark files are size-checked
//! before reading; crop, stage output and PSD layer areas are checked against their budgets before allocation.
//! Parity with Photoshop's Export As / Save As, Lightroom's export and their CMYK separation is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::develop::export_dialog::{any_group_on, read_watermark_graphic, receipt_json, stage_from_fields};
use super::develop::{Refusal, SRGB_V4_ICC};
use super::export::{create_unique_sibling, write_atomic, write_new};
use super::job_admission::{JobAdmission, refusal_json, settlement_json};
use super::render::{self, Assets, LinearFrame, Prefer};
use hsk_studio_accord::{ActorContext, CancellationToken, DomainId};
use hsk_studio_develop::{DevelopedImage, ExportStage, apply_export_stage};
use hsk_studio_folio::{Payload, StudioDocument};
use hsk_studio_harness_core::RunContext;
use hsk_studio_harness_core::batch::{OutputTarget, TargetExporter};
use hsk_studio_interop_psd as psd;
use hsk_studio_interop_raster as raster;
use hsk_studio_interop_tiff as tiff;
use hsk_studio_prism::{Intent, MAX_WIDE_PIXELS, MAX_WIDE_PROFILE_BYTES, ProfileInput, WORKING_PROFILE_LINEAR_SRGB, WideEngine, WideSource, WideTransformRequest, profile_hash};
use hsk_studio_session::{Action, AdmittedJob, JobOutcome};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// Schema id of a recipe file.
pub const RECIPE_SCHEMA: &str = "hsk.studio.doc_export_recipe@1";
/// Largest recipe file read.
pub const MAX_RECIPE_BYTES: u64 = 1 << 20;
/// Studio reading of a common press total-ink limit (percent), reported, never enforced.
pub const TOTAL_INK_REPORT_PERCENT: f32 = 300.0;
/// Tile edge of the native raster tiles (`rgb_f32le` cell then the `f32le` alpha cell).
const CELL: usize = 256;
const COLOUR_CELL: usize = CELL * CELL * 12;
const ALPHA_CELL: usize = CELL * CELL * 4;

fn refuse(code: &'static str, detail: impl Into<String>) -> Refusal {
    Refusal { code, detail: detail.into() }
}

/// TIFF compression (STU-IO-136: None, LZW, ZIP).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TiffCompression {
    None,
    Lzw,
    Zip,
}

/// Output format and its encoder options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocFormat {
    /// 8 or 16 bits.
    Png { depth: u8 },
    /// 1..=100, 8-bit baseline.
    Jpeg { quality: u8 },
    /// 8 or 16 bits; the predictor needs a compression.
    Tiff { depth: u8, compression: TiffCompression, predictor: bool },
    /// Layered, 8 or 16 bits.
    Psd { depth: u8 },
}

impl DocFormat {
    /// The batch `format_id` (STU-AUT-129) of this format.
    pub const fn format_id(self) -> &'static str {
        match self {
            Self::Png { .. } => "png",
            Self::Jpeg { .. } => "jpeg",
            Self::Tiff { .. } => "tiff",
            Self::Psd { .. } => "psd",
        }
    }

    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png { .. } => "png",
            Self::Jpeg { .. } => "jpg",
            Self::Tiff { .. } => "tif",
            Self::Psd { .. } => "psd",
        }
    }

    /// The default options of a `format_id` (PNG 8-bit, JPEG quality 90, TIFF 16-bit ZIP with predictor, PSD 8-bit).
    pub fn default_for(format_id: &str) -> Option<Self> {
        Some(match format_id {
            "png" => Self::Png { depth: 8 },
            "jpeg" => Self::Jpeg { quality: super::export::DEFAULT_JPEG_QUALITY },
            "tiff" => Self::Tiff { depth: 16, compression: TiffCompression::Zip, predictor: true },
            "psd" => Self::Psd { depth: 8 },
            _ => return None,
        })
    }

    fn depth(self) -> u8 {
        match self {
            Self::Png { depth } | Self::Tiff { depth, .. } | Self::Psd { depth } => depth,
            Self::Jpeg { .. } => 8,
        }
    }
}

/// Output colour.
#[derive(Clone, Debug, PartialEq)]
pub enum DocColour {
    /// sRGB-encoded, tagged `sRGB-v4.icc`.
    Srgb,
    /// Converted into `profile` (CMYK ICC) through Prism's wide path; TIFF only.
    Cmyk { profile: Vec<u8>, intent: Intent, black_point_compensation: bool },
}

/// A crop rectangle in frame pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One document export recipe (module notes).
#[derive(Clone, Debug, PartialEq)]
pub struct DocExportRecipe {
    pub recipe_id: Option<String>,
    pub format: DocFormat,
    pub crop: Option<CropRect>,
    /// The develop export stage; `None` = no stage (no resolution recorded).
    pub stage: Option<ExportStage>,
    pub colour: DocColour,
}

const RECIPE_KEYS: [&str; 11] = ["schema_id", "recipe_id", "format", "depth", "quality", "compression", "predictor", "crop", "stage", "watermark_graphic", "colour"];
const INTENTS: [(&str, Intent); 4] = [
    ("relative_colorimetric", Intent::RelativeColorimetric),
    ("perceptual", Intent::Perceptual),
    ("saturation", Intent::Saturation),
    ("absolute_colorimetric", Intent::AbsoluteColorimetric),
];

fn scalar_text(v: &Value, key: &str) -> Result<String, Refusal> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(refuse("recipe_value_invalid", key.to_owned())),
    }
}

fn resolve(base: Option<&Path>, text: &str) -> PathBuf {
    let p = PathBuf::from(text);
    match base {
        Some(b) if p.is_relative() => b.join(p),
        _ => p,
    }
}

fn read_bounded(path: &Path, max: u64, code: &'static str) -> Result<Vec<u8>, Refusal> {
    let len = std::fs::metadata(path).map_err(|_| refuse(code, path.display().to_string()))?.len();
    if len > max {
        return Err(refuse("recipe_file_too_large", format!("{} ({len} bytes, limit {max})", path.display())));
    }
    std::fs::read(path).map_err(|_| refuse(code, path.display().to_string()))
}

impl DocExportRecipe {
    /// The recipe that writes `format` with no crop, stage or conversion.
    pub fn of_format(format: DocFormat) -> Self {
        Self { recipe_id: None, format, crop: None, stage: None, colour: DocColour::Srgb }
    }

    /// Decodes a recipe object (closed keys; see the module notes and `manual::DRIVE` for the shape). Relative
    /// file names (`watermark_graphic`, `colour.profile`) resolve against `base`.
    pub fn from_json(v: &Value, base: Option<&Path>, cancel: &CancellationToken) -> Result<Self, Refusal> {
        let o = v.as_object().ok_or_else(|| refuse("recipe_not_an_object", ""))?;
        if let Some(k) = o.keys().find(|k| !RECIPE_KEYS.contains(&k.as_str())) {
            return Err(refuse("recipe_key_unknown", k.clone()));
        }
        if let Some(s) = o.get("schema_id")
            && s.as_str() != Some(RECIPE_SCHEMA)
        {
            return Err(refuse("recipe_schema_unknown", s.to_string()));
        }
        let recipe_id = match o.get("recipe_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control) => Some(s.clone()),
            Some(_) => return Err(refuse("recipe_value_invalid", "recipe_id")),
        };
        let format_id = o.get("format").and_then(Value::as_str).ok_or_else(|| refuse("recipe_format_missing", "format"))?;
        let depth = match o.get("depth") {
            None => None,
            Some(d) => Some(d.as_u64().filter(|d| *d == 8 || *d == 16).and_then(|d| u8::try_from(d).ok()).ok_or_else(|| refuse("recipe_depth_invalid", d.to_string()))?),
        };
        let only = |key: &str, formats: &[&str]| -> Result<(), Refusal> {
            if o.contains_key(key) && !formats.contains(&format_id) { Err(refuse("recipe_key_not_for_format", format!("{key} with {format_id}"))) } else { Ok(()) }
        };
        only("quality", &["jpeg"])?;
        only("compression", &["tiff"])?;
        only("predictor", &["tiff"])?;
        let format = match format_id {
            "png" => DocFormat::Png { depth: depth.unwrap_or(8) },
            "jpeg" => {
                if depth.is_some_and(|d| d != 8) {
                    return Err(refuse("recipe_depth_invalid", "jpeg is 8-bit"));
                }
                let quality = match o.get("quality") {
                    None => super::export::DEFAULT_JPEG_QUALITY,
                    Some(q) => q.as_u64().filter(|q| (1..=100).contains(q)).and_then(|q| u8::try_from(q).ok()).ok_or_else(|| refuse("recipe_quality_invalid", q.to_string()))?,
                };
                DocFormat::Jpeg { quality }
            }
            "tiff" => {
                let compression = match o.get("compression").map(|c| c.as_str()) {
                    None | Some(Some("zip")) => TiffCompression::Zip,
                    Some(Some("lzw")) => TiffCompression::Lzw,
                    Some(Some("none")) => TiffCompression::None,
                    Some(_) => return Err(refuse("recipe_compression_invalid", "compression")),
                };
                let predictor = match o.get("predictor") {
                    None => compression != TiffCompression::None,
                    Some(p) => p.as_bool().ok_or_else(|| refuse("recipe_value_invalid", "predictor"))?,
                };
                if predictor && compression == TiffCompression::None {
                    return Err(refuse("recipe_predictor_needs_compression", "predictor"));
                }
                DocFormat::Tiff { depth: depth.unwrap_or(16), compression, predictor }
            }
            "psd" => DocFormat::Psd { depth: depth.unwrap_or(8) },
            other => return Err(refuse("recipe_format_unknown", other.to_owned())),
        };
        let crop = match o.get("crop") {
            None | Some(Value::Null) => None,
            Some(c) => {
                let field = |k: &str| c.get(k).and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).ok_or_else(|| refuse("recipe_crop_invalid", k.to_owned()));
                let rect = CropRect { x: field("x")?, y: field("y")?, width: field("width")?, height: field("height")? };
                if c.as_object().is_none_or(|m| m.len() != 4) || rect.width == 0 || rect.height == 0 {
                    return Err(refuse("recipe_crop_invalid", "crop"));
                }
                Some(rect)
            }
        };
        let graphic = match o.get("watermark_graphic") {
            None | Some(Value::Null) => None,
            Some(Value::String(p)) => Some(read_watermark_graphic(&resolve(base, p), cancel)?),
            Some(_) => return Err(refuse("recipe_value_invalid", "watermark_graphic")),
        };
        let stage = match o.get("stage") {
            None | Some(Value::Null) => {
                if graphic.is_some() {
                    return Err(refuse("recipe_watermark_needs_stage", "watermark_graphic"));
                }
                None
            }
            Some(Value::Object(m)) => {
                let fields = m.iter().map(|(k, v)| Ok((k.clone(), scalar_text(v, k)?))).collect::<Result<Vec<_>, Refusal>>()?;
                Some(stage_from_fields(&fields, graphic)?)
            }
            Some(_) => return Err(refuse("recipe_value_invalid", "stage")),
        };
        let colour = match o.get("colour") {
            None | Some(Value::Null) => DocColour::Srgb,
            Some(Value::String(s)) if s == "srgb" => DocColour::Srgb,
            Some(Value::Object(c)) => {
                if let Some(k) = c.keys().find(|k| !["space", "profile", "intent", "black_point_compensation"].contains(&k.as_str())) {
                    return Err(refuse("recipe_key_unknown", format!("colour.{k}")));
                }
                match c.get("space").and_then(Value::as_str) {
                    Some("srgb") if c.len() == 1 => DocColour::Srgb,
                    Some("cmyk") => {
                        let path = c.get("profile").and_then(Value::as_str).ok_or_else(|| refuse("recipe_profile_missing", "colour.profile"))?;
                        let profile = read_bounded(&resolve(base, path), u64::try_from(MAX_WIDE_PROFILE_BYTES).unwrap_or(u64::MAX), "recipe_profile_unreadable")?;
                        let intent = match c.get("intent") {
                            None => Intent::RelativeColorimetric,
                            Some(i) => INTENTS.iter().find(|(k, _)| Some(*k) == i.as_str()).map(|(_, v)| *v).ok_or_else(|| refuse("recipe_intent_invalid", i.to_string()))?,
                        };
                        let black_point_compensation = match c.get("black_point_compensation") {
                            None => true,
                            Some(b) => b.as_bool().ok_or_else(|| refuse("recipe_value_invalid", "colour.black_point_compensation"))?,
                        };
                        DocColour::Cmyk { profile, intent, black_point_compensation }
                    }
                    _ => return Err(refuse("recipe_colour_invalid", "colour.space")),
                }
            }
            Some(_) => return Err(refuse("recipe_colour_invalid", "colour")),
        };
        Ok(Self { recipe_id, format, crop, stage, colour })
    }

    /// Reads a recipe file (at most [`MAX_RECIPE_BYTES`]); relative names resolve against its folder.
    pub fn read(path: &Path, cancel: &CancellationToken) -> Result<Self, Refusal> {
        let bytes = read_bounded(path, MAX_RECIPE_BYTES, "recipe_unreadable")?;
        let v: Value = serde_json::from_slice(&bytes).map_err(|_| refuse("recipe_not_json", path.display().to_string()))?;
        Self::from_json(&v, path.parent(), cancel)
    }

    /// The recipe as the report states it (codes and numbers; no file bytes).
    pub fn summary(&self) -> Value {
        let format = match self.format {
            DocFormat::Png { depth } => json!({"format": "png", "depth": depth}),
            DocFormat::Jpeg { quality } => json!({"format": "jpeg", "quality": quality}),
            DocFormat::Tiff { depth, compression, predictor } => json!({"format": "tiff", "depth": depth, "compression": match compression {
                TiffCompression::None => "none", TiffCompression::Lzw => "lzw", TiffCompression::Zip => "zip" }, "predictor": predictor}),
            DocFormat::Psd { depth } => json!({"format": "psd", "depth": depth}),
        };
        json!({
            "recipe_id": self.recipe_id, "output": format,
            "crop": self.crop.map(|c| json!([c.x, c.y, c.width, c.height])),
            "stage": self.stage.as_ref().map(|s| json!({"acts": any_group_on(s), "resolution_ppi": s.sizing.pixels_per_inch()})),
            "colour": match &self.colour {
                DocColour::Srgb => json!({"space": "srgb"}),
                DocColour::Cmyk { profile, intent, black_point_compensation } => json!({"space": "cmyk", "profile_sha256": hex(&profile_hash(profile)), "intent": intent.code(), "black_point_compensation": black_point_compensation}),
            },
        })
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An encoded document export.
#[derive(Clone, Debug, PartialEq)]
pub struct DocExported {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub report: Value,
}

/// The document a layered PSD is built from.
pub struct LayerSource<'a> {
    pub doc: &'a StudioDocument,
    pub assets: &'a Assets,
}

/// Linear -> sRGB transfer (IEC 61966-2-1) in 0..1, clamped.
fn oetf(v: f32) -> f32 {
    let v = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

fn unit_finite(v: f32) -> f32 {
    if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }
}

// Clamped to 0..=1 first: the products are in 0..=255 and 0..=65535.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn q8(v: f32) -> u8 {
    (unit_finite(v) * 255.0).round() as u8
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn q16(v: f32) -> u16 {
    (unit_finite(v) * 65535.0).round() as u16
}

/// Interleaved samples of `values` (already in 0..1) at `depth`.
fn quantize(values: &[f32], depth: u8) -> Samples {
    if depth == 16 { Samples::U16(values.iter().map(|v| q16(*v)).collect()) } else { Samples::U8(values.iter().map(|v| q8(*v)).collect()) }
}

enum Samples {
    U8(Vec<u8>),
    U16(Vec<u16>),
}

/// The pixels being exported: linear straight RGBA.
struct Image {
    width: u32,
    height: u32,
    pixels: Vec<[f32; 4]>,
}

impl Image {
    fn opaque(&self) -> bool {
        self.pixels.iter().all(|p| p[3] >= 1.0)
    }

    /// Linear RGB over white.
    fn flattened_rgb(&self) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.pixels.len().saturating_mul(3));
        for [r, g, b, a] in &self.pixels {
            let a = unit_finite(*a);
            out.extend([*r, *g, *b].map(|c| unit_finite(c) * a + (1.0 - a)));
        }
        out
    }

    /// sRGB-encoded straight samples, RGB (`alpha` false) or RGBA.
    fn encoded(&self, alpha: bool) -> Vec<f32> {
        let mut out = Vec::with_capacity(self.pixels.len().saturating_mul(4));
        for [r, g, b, a] in &self.pixels {
            out.extend([oetf(*r), oetf(*g), oetf(*b)]);
            if alpha {
                out.push(unit_finite(*a));
            }
        }
        out
    }
}

fn crop(frame: &LinearFrame, rect: Option<CropRect>) -> Result<Image, Refusal> {
    let Some(c) = rect else {
        return Ok(Image { width: frame.width, height: frame.height, pixels: frame.pixels.clone() });
    };
    let inside = c.x.checked_add(c.width).is_some_and(|e| e <= frame.width) && c.y.checked_add(c.height).is_some_and(|e| e <= frame.height);
    if !inside {
        return Err(refuse("export_crop_outside", format!("{}x{} at {},{} outside {}x{}", c.width, c.height, c.x, c.y, frame.width, frame.height)));
    }
    let (fw, w) = (usize::try_from(frame.width).map_err(|_| refuse("export_crop_outside", ""))?, usize::try_from(c.width).map_err(|_| refuse("export_crop_outside", ""))?);
    let n = w.checked_mul(usize::try_from(c.height).map_err(|_| refuse("export_crop_outside", ""))?).ok_or_else(|| refuse("export_crop_outside", ""))?;
    let mut pixels = Vec::with_capacity(n);
    for y in 0..c.height {
        let row = usize::try_from(c.y.saturating_add(y)).unwrap_or(usize::MAX).checked_mul(fw).and_then(|r| r.checked_add(usize::try_from(c.x).ok()?)).ok_or_else(|| refuse("export_crop_outside", ""))?;
        let end = row.checked_add(w).ok_or_else(|| refuse("export_crop_outside", ""))?;
        pixels.extend_from_slice(frame.pixels.get(row..end).ok_or_else(|| refuse("export_crop_outside", ""))?);
    }
    Ok(Image { width: c.width, height: c.height, pixels })
}

/// Silent sink for Prism's frames (the export records its own operation).
struct Quiet;
impl hsk_studio_observe::SinkPort for Quiet {
    fn try_send(&mut self, _class: hsk_studio_observe::DeliveryClass, _bytes: &[u8]) -> Result<(), hsk_studio_observe::DeliveryError> {
        Ok(())
    }
}

/// CMYK ink values (0..1, ICC convention: 1 is full ink) of linear RGB, and the clamped value count.
fn to_cmyk(rgb: &[f32], profile: &[u8], intent: Intent, bpc: bool, resource: &DomainId, actor: &ActorContext, cancel: &CancellationToken) -> Result<(Vec<f32>, u64), Refusal> {
    let fail = |code: &'static str| refuse(code, "cmyk conversion");
    let source_id = DomainId::parse("SCPF-019abcde-0000-7000-8000-00000000e001").map_err(|_| fail("profile_id"))?;
    let destination_id = DomainId::parse("SCPF-019abcde-0000-7000-8000-00000000e002").map_err(|_| fail("profile_id"))?;
    let mut engine = WideEngine::new(actor.clone(), 2).map_err(|e| refuse(e.code(), "wide engine"))?;
    let mut out = Vec::with_capacity(rgb.len().checked_div(3).and_then(|n| n.checked_mul(4)).ok_or_else(|| fail("export_too_large"))?);
    let mut clamped = 0u64;
    let chunk = MAX_WIDE_PIXELS.saturating_mul(3);
    for (i, pixels) in rgb.chunks(chunk).enumerate() {
        cancel.check().map_err(|_| fail("canceled"))?;
        let request = WideTransformRequest {
            source: WideSource::Profile(ProfileInput { profile_id: &source_id, bytes: WORKING_PROFILE_LINEAR_SRGB, expected_sha256: profile_hash(WORKING_PROFILE_LINEAR_SRGB) }),
            destination: ProfileInput { profile_id: &destination_id, bytes: profile, expected_sha256: profile_hash(profile) },
            pixels,
            intent,
            black_point_compensation: bpc,
            revision: 0,
            expected_revision: 0,
            correlation_id: u64::try_from(i).unwrap_or(u64::MAX).saturating_add(1),
            resource_id: resource,
            actor,
        };
        let result = engine.transform(&request, cancel, &mut Quiet).map_err(|f| refuse(f.error.code(), "cmyk conversion"))?;
        if result.receipt.destination_space != hsk_studio_prism::DeviceSpace::Cmyk {
            return Err(refuse("export_profile_not_cmyk", result.receipt.destination_space.code()));
        }
        for v in result.pixels {
            let c = unit_finite(v);
            if c != v {
                clamped = clamped.saturating_add(1);
            }
            out.push(c);
        }
    }
    Ok((out, clamped))
}

fn verify<T>(r: Result<T, Refusal>) -> Result<T, Refusal> {
    r.map_err(|e| refuse("export_verify_failed", format!("{}: {}", e.code, e.detail)))
}

/// Encodes `frame` by `recipe` (module notes). `layers` is required for PSD. Nothing is written.
pub fn export_frame(frame: &LinearFrame, recipe: &DocExportRecipe, layers: Option<&LayerSource<'_>>, resource: &DomainId, actor: &ActorContext, cancel: &CancellationToken) -> Result<DocExported, Refusal> {
    let mut notes: Vec<&'static str> = Vec::new();
    let mut image = crop(frame, recipe.crop)?;
    let acting = recipe.stage.as_ref().filter(|s| any_group_on(s));
    if matches!(recipe.format, DocFormat::Psd { .. }) {
        if acting.is_some() {
            return Err(refuse("psd_export_stage_unsupported", "sizing, sharpening and watermark are flat-output stages"));
        }
        if recipe.colour != DocColour::Srgb {
            return Err(refuse("cmyk_format_unsupported", "psd"));
        }
    }
    if matches!(recipe.colour, DocColour::Cmyk { .. }) && !matches!(recipe.format, DocFormat::Tiff { .. }) {
        return Err(refuse("cmyk_format_unsupported", recipe.format.format_id()));
    }
    let mut stage_json = Value::Null;
    if let Some(stage) = acting {
        if !image.opaque() {
            notes.push("alpha_flattened_on_white");
        }
        let developed = DevelopedImage { width: image.width, height: image.height, rgb: image.flattened_rgb() };
        let (out, receipt) = apply_export_stage(&developed, stage, cancel).map_err(|e| refuse(e.code(), if e.detail().is_empty() { format!("{e:?}") } else { e.detail().to_owned() }))?;
        stage_json = receipt_json(&receipt);
        image = Image { width: out.width, height: out.height, pixels: out.rgb.chunks_exact(3).map(|p| [p[0], p[1], p[2], 1.0]).collect() };
    }
    let ppi = recipe.stage.as_ref().map(|s| s.sizing.pixels_per_inch());
    let depth = recipe.format.depth();
    let mut extra = Map::new();
    let (bytes, colour, icc_sha256): (Vec<u8>, &str, String) = match (recipe.format, &recipe.colour) {
        (DocFormat::Png { depth }, _) => {
            let alpha = !image.opaque();
            let samples = match quantize(&image.encoded(alpha), depth) {
                Samples::U8(v) => raster::Samples::U8(v),
                Samples::U16(v) => raster::Samples::U16(v),
            };
            let req = raster::PngEncodeRequest {
                width: image.width,
                height: image.height,
                colour: if alpha { raster::Colour::Rgba } else { raster::Colour::Rgb },
                samples: &samples,
                icc: Some(SRGB_V4_ICC),
                compression: raster::PngCompression::Balanced,
            };
            let bytes = raster::encode_png(&req).map_err(|e| refuse(e.code(), "png"))?;
            (bytes, if alpha { "Rgba" } else { "Rgb" }, hex(&profile_hash(SRGB_V4_ICC)))
        }
        (DocFormat::Jpeg { quality }, _) => {
            if !image.opaque() {
                notes.push("alpha_flattened_on_white");
            }
            let flat = Image { width: image.width, height: image.height, pixels: image.flattened_rgb().chunks_exact(3).map(|p| [p[0], p[1], p[2], 1.0]).collect() };
            let samples = raster::Samples::U8(flat.encoded(false).iter().map(|v| q8(*v)).collect());
            let subsampling = if quality >= 90 { raster::JpegSubsampling::S444 } else { raster::JpegSubsampling::S420 };
            extra.insert("subsampling".into(), json!(if quality >= 90 { "4:4:4" } else { "4:2:0" }));
            let req = raster::JpegEncodeRequest { width: flat.width, height: flat.height, colour: raster::Colour::Rgb, samples: &samples, quality, subsampling, icc: Some(SRGB_V4_ICC) };
            let bytes = raster::encode_jpeg(&req).map_err(|e| refuse(e.code(), "jpeg"))?;
            (bytes, "Rgb", hex(&profile_hash(SRGB_V4_ICC)))
        }
        (DocFormat::Tiff { depth, compression, predictor }, colour) => {
            let (model, alpha, values, icc): (tiff::ColorModel, tiff::Alpha, Vec<f32>, &[u8]) = match colour {
                DocColour::Srgb if image.opaque() => (tiff::ColorModel::Rgb, tiff::Alpha::None, image.encoded(false), SRGB_V4_ICC),
                DocColour::Srgb => (tiff::ColorModel::Rgba, tiff::Alpha::Unassociated, image.encoded(true), SRGB_V4_ICC),
                DocColour::Cmyk { profile, intent, black_point_compensation } => {
                    if !image.opaque() {
                        notes.push("alpha_flattened_on_white");
                    }
                    let (ink, clamped) = to_cmyk(&image.flattened_rgb(), profile, *intent, *black_point_compensation, resource, actor, cancel)?;
                    let mut max_ink = 0.0f32;
                    let mut over = 0u64;
                    for p in ink.chunks_exact(4) {
                        let total = (p[0] + p[1] + p[2] + p[3]) * 100.0;
                        max_ink = max_ink.max(total);
                        if total > TOTAL_INK_REPORT_PERCENT {
                            over = over.saturating_add(1);
                        }
                    }
                    extra.insert("cmyk".into(), json!({"intent": intent.code(), "black_point_compensation": black_point_compensation, "clamped_values": clamped,
                        "total_ink_max_percent": (max_ink * 10.0).round() / 10.0, "total_ink_report_limit_percent": TOTAL_INK_REPORT_PERCENT, "pixels_over_report_limit": over}));
                    (tiff::ColorModel::Cmyk, tiff::Alpha::None, ink, profile.as_slice())
                }
            };
            let samples = match quantize(&values, depth) {
                Samples::U8(v) => tiff::Samples::U8(v),
                Samples::U16(v) => tiff::Samples::U16(v),
            };
            // Whole hundredths of a pixel per inch (ResolutionUnit 2).
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let resolution = ppi.filter(|p| p.is_finite() && *p > 0.0 && *p < 1.0e7).map(|p| {
                let n = (p * 100.0).round() as u32;
                tiff::Resolution { x: (n, 100), y: (n, 100), unit: 2 }
            });
            let req = tiff::EncodeRequest {
                width: image.width,
                height: image.height,
                color: model,
                alpha,
                bit_depth: depth,
                samples: &samples,
                icc: Some(icc),
                compression: match compression {
                    TiffCompression::None => tiff::Compression::None,
                    TiffCompression::Lzw => tiff::Compression::Lzw,
                    TiffCompression::Zip => tiff::Compression::Deflate,
                },
                predictor: if predictor { tiff::Predictor::Horizontal } else { tiff::Predictor::None },
                big_tiff: false,
                resolution,
                orientation: None,
            };
            let opts = tiff::EncodeOptions { metadata: tiff::Metadata { software: Some("Handshake Studio".into()), ..tiff::Metadata::default() }, ..tiff::EncodeOptions::default() };
            let bytes = tiff::encode_with(&req, &opts).map_err(|e| refuse(e.code(), format!("tiff {e:?}")))?;
            let name = match model {
                tiff::ColorModel::Cmyk => "Cmyk",
                tiff::ColorModel::Rgba => "Rgba",
                _ => "Rgb",
            };
            (bytes, name, hex(&profile_hash(icc)))
        }
        (DocFormat::Psd { depth }, _) => {
            let layers = layers.ok_or_else(|| refuse("psd_layers_missing", "no document"))?;
            let origin = (frame.x.saturating_add(i64::from(recipe.crop.map_or(0, |c| c.x))), frame.y.saturating_add(i64::from(recipe.crop.map_or(0, |c| c.y))));
            let limits = psd::Limits::default();
            let mut loss = Vec::new();
            let records = psd_records(layers, origin, depth, &limits, &mut loss, cancel)?;
            let merged_alpha = !image.opaque();
            let merged = match quantize(&image.encoded(true), depth) {
                Samples::U8(v) => psd::ComposeSamples::U8(v),
                Samples::U16(v) => psd::ComposeSamples::U16(v),
            };
            let count = records.len();
            let req = psd::ComposeRequest { width: image.width, height: image.height, merged, merged_alpha, icc: Some(SRGB_V4_ICC.to_vec()), records };
            let bytes = psd::compose_psd(&req, &limits).map_err(|e| refuse(e.code(), "psd"))?;
            let back = verify(psd::read_psd(&bytes, &limits, cancel).map_err(|e| refuse(e.code(), "psd")))?;
            let read = back.layer_section.layer_info.as_ref().map_or(0, |i| i.layers.len());
            if (back.header.width, back.header.height) != (image.width, image.height) || read != count {
                return Err(refuse("export_verify_failed", "psd"));
            }
            extra.insert("psd".into(), json!({"records": count, "loss": loss}));
            (bytes, if merged_alpha { "Rgba" } else { "Rgb" }, hex(&profile_hash(SRGB_V4_ICC)))
        }
    };
    // Decode back through the codec owner before anything is written (PSD was read back above).
    match recipe.format {
        DocFormat::Png { .. } | DocFormat::Jpeg { .. } => {
            let back = verify(raster::decode(&bytes, &raster::Limits::default(), cancel).map_err(|e| refuse(e.code(), "decode")))?;
            if (back.width, back.height) != (image.width, image.height) {
                return Err(refuse("export_verify_failed", "dimensions"));
            }
        }
        DocFormat::Tiff { .. } => {
            let back = verify(tiff::decode(&bytes, &tiff::Limits::default(), cancel).map_err(|e| refuse(e.code(), "decode")))?;
            let page = back.pages.first().ok_or_else(|| refuse("export_verify_failed", "no page"))?;
            if (page.info.width, page.info.height) != (image.width, image.height) {
                return Err(refuse("export_verify_failed", "dimensions"));
            }
        }
        DocFormat::Psd { .. } => {}
    }
    let mut report = json!({
        "format": recipe.format.format_id(), "width": image.width, "height": image.height, "depth": depth, "bytes": bytes.len(),
        "colour": colour, "icc_sha256": icc_sha256, "resolution_ppi": ppi, "notes": notes, "stage": stage_json, "recipe": recipe.summary(),
    });
    for (k, v) in extra {
        report[k] = v;
    }
    Ok(DocExported { width: image.width, height: image.height, bytes, report })
}

/// The PSD records of the document's stack, bottom-most first (module notes).
fn psd_records(src: &LayerSource<'_>, origin: (i64, i64), depth: u8, limits: &psd::Limits, loss: &mut Vec<Value>, cancel: &CancellationToken) -> Result<Vec<psd::ComposeRecord>, Refusal> {
    let mut out = Vec::new();
    let mut total = 0usize;
    emit(src, None, origin, depth, limits, 0, &mut out, &mut total, loss, cancel)?;
    let stacked = stacked_ids(src.doc);
    for l in src.doc.layers.iter().filter(|l| !stacked.contains(&l.layer_id)) {
        loss.push(json!({"layer_id": l.layer_id, "code": "psd_layer_not_in_stack"}));
    }
    Ok(out)
}

fn stacked_ids(doc: &StudioDocument) -> Vec<String> {
    let mut all = Vec::new();
    let mut todo = vec![None::<String>];
    while let Some(parent) = todo.pop() {
        for id in super::layers::stack(doc, parent.as_deref()) {
            if all.contains(&id) || all.len() > doc.layers.len() {
                continue;
            }
            todo.push(Some(id.clone()));
            all.push(id);
        }
    }
    all
}

fn look(layer: &hsk_studio_folio::StudioLayer, group: bool, loss: &mut Vec<Value>) -> psd::ComposeLook {
    let ap = layer.effective_appearance();
    // Percent clamped to 0..=100 first: the product is in 0..=255.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let byte = |p: f64| (if p.is_finite() { p.clamp(0.0, 100.0) } else { 100.0 } * 2.55).round() as u8;
    let blend = if group && ap.isolation == hsk_studio_folio::Isolation::PassThrough {
        psd::StudioBlendMode::PassThrough
    } else {
        match psd::StudioBlendMode::try_from(ap.blend_mode).ok().filter(|m| psd::psd_key_for(*m).is_some_and(|k| &k != b"pass")) {
            Some(m) => m,
            None => {
                loss.push(json!({"layer_id": layer.layer_id, "code": "psd_blend_mode_written_normal", "blend_mode": ap.blend_mode}));
                psd::StudioBlendMode::Normal
            }
        }
    };
    if ap.mask.is_some() {
        loss.push(json!({"layer_id": layer.layer_id, "code": "psd_layer_mask_not_exported"}));
    }
    psd::ComposeLook { name: layer.name.clone(), opacity: byte(ap.opacity), fill_opacity: byte(ap.fill_opacity), blend, visible: layer.visible, clipped: ap.clipped }
}

#[allow(clippy::too_many_arguments)]
fn emit(
    src: &LayerSource<'_>,
    parent: Option<&str>,
    origin: (i64, i64),
    depth: u8,
    limits: &psd::Limits,
    level: usize,
    out: &mut Vec<psd::ComposeRecord>,
    total: &mut usize,
    loss: &mut Vec<Value>,
    cancel: &CancellationToken,
) -> Result<(), Refusal> {
    if level > limits.max_group_depth {
        return Err(refuse("psd_group_depth", "groups nest deeper than the PSD limit"));
    }
    for id in super::layers::stack(src.doc, parent) {
        cancel.check().map_err(|_| refuse("canceled", "psd"))?;
        let Some(layer) = src.doc.layers.iter().find(|l| l.layer_id == id) else { continue };
        match &layer.payload {
            // A composition adjustment layer (kind adjustment, group payload; STU-CMP-015) has no
            // PSD form here: a typed loss, never an empty group (lane SMB-MOTION-COMMANDS-2, F7).
            Payload::Group if layer.kind == "adjustment" => loss.push(json!({"layer_id": layer.layer_id, "code": "psd_layer_kind_not_exported", "kind": layer.kind})),
            Payload::Group => {
                out.push(psd::ComposeRecord::GroupEnd);
                emit(src, Some(id.as_str()), origin, depth, limits, level.saturating_add(1), out, total, loss, cancel)?;
                out.push(psd::ComposeRecord::GroupOpen { look: look(layer, true, loss) });
            }
            Payload::Raster { tiles } => out.push(raster_record(src, layer, tiles, origin, depth, limits, total, loss)?),
            _ => loss.push(json!({"layer_id": layer.layer_id, "code": "psd_layer_kind_not_exported", "kind": layer.kind})),
        }
    }
    Ok(())
}

fn whole_edge(l: &hsk_studio_folio::Length) -> Option<i64> {
    // Integral and within 0..=CELL: the conversion is exact.
    #[allow(clippy::cast_possible_truncation)]
    (l.value.is_finite() && l.value >= 0.0 && l.value <= CELL as f64 && l.value.fract() == 0.0).then_some(l.value as i64)
}

#[allow(clippy::too_many_arguments)]
fn raster_record(
    src: &LayerSource<'_>,
    layer: &hsk_studio_folio::StudioLayer,
    tiles: &[hsk_studio_folio::TileRef],
    origin: (i64, i64),
    depth: u8,
    limits: &psd::Limits,
    total: &mut usize,
    loss: &mut Vec<Value>,
) -> Result<psd::ComposeRecord, Refusal> {
    let bad = |what: &str| refuse("psd_tile_invalid", format!("{} {what}", layer.layer_id));
    let cell = i64::try_from(CELL).unwrap_or(256);
    let mut placed = Vec::with_capacity(tiles.len());
    let (mut x0, mut y0, mut x1, mut y1) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
    for t in tiles {
        if t.format != "rgb_f32le" {
            return Err(bad("format"));
        }
        let (w, h) = (whole_edge(&t.width).ok_or_else(|| bad("width"))?, whole_edge(&t.height).ok_or_else(|| bad("height"))?);
        let (x, y) = (t.column.checked_mul(cell).ok_or_else(|| bad("column"))?, t.row.checked_mul(cell).ok_or_else(|| bad("row"))?);
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x.checked_add(w).ok_or_else(|| bad("column"))?);
        y1 = y1.max(y.checked_add(h).ok_or_else(|| bad("row"))?);
        placed.push((t, x, y, w, h));
    }
    let look = look(layer, false, loss);
    if placed.is_empty() {
        let rgba = if depth == 16 { psd::ComposeSamples::U16(Vec::new()) } else { psd::ComposeSamples::U8(Vec::new()) };
        return Ok(psd::ComposeRecord::Pixels { look, left: 0, top: 0, width: 0, height: 0, rgba });
    }
    let width = u32::try_from(x1.checked_sub(x0).ok_or_else(|| bad("extent"))?).map_err(|_| bad("extent"))?;
    let height = u32::try_from(y1.checked_sub(y0).ok_or_else(|| bad("extent"))?).map_err(|_| bad("extent"))?;
    let area = u64::from(width).checked_mul(u64::from(height)).ok_or_else(|| bad("extent"))?;
    if area > limits.max_pixels {
        return Err(refuse("psd_layer_too_large", layer.layer_id.clone()));
    }
    let samples = usize::try_from(area).ok().and_then(|a| a.checked_mul(4)).ok_or_else(|| bad("extent"))?;
    // V4-02: the running aggregate of every layer's samples at the file depth, checked before this layer is
    // allocated; the samples are quantized straight into that buffer (no wider transient).
    *total = samples.checked_mul(usize::from(depth / 8)).and_then(|b| total.checked_add(b)).ok_or_else(|| refuse("psd_too_large", ""))?;
    if *total > limits.max_input_bytes {
        return Err(refuse("psd_too_large", format!("{} bytes of layer pixels", *total)));
    }
    let mut rgba = if depth == 16 { psd::ComposeSamples::U16(vec![0; samples]) } else { psd::ComposeSamples::U8(vec![0; samples]) };
    let w = usize::try_from(width).map_err(|_| bad("extent"))?;
    for (t, x, y, tw, th) in placed {
        let body = src.assets.get(&t.content_digest.digest).ok_or_else(|| refuse("psd_tile_missing", t.content_digest.digest.clone()))?;
        if body.len() != COLOUR_CELL.saturating_add(ALPHA_CELL) {
            return Err(bad("size"));
        }
        let (colour, alpha) = body.split_at(COLOUR_CELL);
        let f = |bytes: &[u8], at: usize| -> Option<f32> { Some(f32::from_le_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?)) };
        let (dx, dy) = (usize::try_from(x.checked_sub(x0).ok_or_else(|| bad("column"))?).map_err(|_| bad("column"))?, usize::try_from(y.checked_sub(y0).ok_or_else(|| bad("row"))?).map_err(|_| bad("row"))?);
        for ty in 0..usize::try_from(th).map_err(|_| bad("height"))? {
            for tx in 0..usize::try_from(tw).map_err(|_| bad("width"))? {
                let i = ty.checked_mul(CELL).and_then(|r| r.checked_add(tx)).ok_or_else(|| bad("index"))?;
                let c = i.checked_mul(12).ok_or_else(|| bad("index"))?;
                let px = [
                    f(colour, c).ok_or_else(|| bad("index"))?,
                    f(colour, c.checked_add(4).ok_or_else(|| bad("index"))?).ok_or_else(|| bad("index"))?,
                    f(colour, c.checked_add(8).ok_or_else(|| bad("index"))?).ok_or_else(|| bad("index"))?,
                    f(alpha, i.checked_mul(4).ok_or_else(|| bad("index"))?).ok_or_else(|| bad("index"))?,
                ];
                let at = dy.checked_add(ty).and_then(|r| r.checked_mul(w)).and_then(|r| r.checked_add(dx)).and_then(|r| r.checked_add(tx)).and_then(|r| r.checked_mul(4)).ok_or_else(|| bad("index"))?;
                let end = at.checked_add(4).ok_or_else(|| bad("index"))?;
                let encoded = [oetf(px[0]), oetf(px[1]), oetf(px[2]), unit_finite(px[3])];
                match &mut rgba {
                    psd::ComposeSamples::U8(v) => v.get_mut(at..end).ok_or_else(|| bad("index"))?.copy_from_slice(&encoded.map(q8)),
                    psd::ComposeSamples::U16(v) => v.get_mut(at..end).ok_or_else(|| bad("index"))?.copy_from_slice(&encoded.map(q16)),
                }
            }
        }
    }
    let to_i32 = |v: i64| i32::try_from(v).map_err(|_| refuse("psd_layer_offset", layer.layer_id.clone()));
    Ok(psd::ComposeRecord::Pixels { look, left: to_i32(x0.saturating_sub(origin.0))?, top: to_i32(y0.saturating_sub(origin.1))?, width, height, rgba })
}

impl Viewer {
    /// Exports the composite by `recipe` (module notes) to exactly `path`: replaced only with `overwrite`, else
    /// `destination_exists`; the opened source refuses `export_over_source` without `overwrite`. Read-only on the
    /// document. One op record `export_document` with the run's attribution and one terminal Observe frame
    /// (`Run::record_host_outcome`). Returns the report (format, size, depth, colour, profile hash, resolution,
    /// notes, stage receipt, CMYK ink readout or PSD records and losses, path, revision).
    ///
    /// Admission (lane SMB-R60-FOLLOWUPS-1 item 1, HBR-HOST-004): after the path, source and open-document
    /// checks and before anything is rendered or written, the export is one job admitted through the viewer's
    /// JobPort ([`Viewer::set_job_port`]); `overloaded`, `canceled` or `stale_revision` refuse it typed with no
    /// file written, and an admitted export settles once (`completed`, `canceled`, or `failed` with the
    /// refusal's code), reported as the report's `job`.
    pub fn export_document(&mut self, path: &Path, recipe: &DocExportRecipe, overwrite: bool) -> Result<Value, Refusal> {
        let result = self.export_document_admitted(path, recipe, overwrite);
        let recorded = match &result {
            Ok(r) => Ok(json!({"format": r["format"], "width": r["width"], "height": r["height"], "bytes": r["bytes"]})),
            Err(r) => Err(r.code),
        };
        self.run.record_host_outcome("export_document", recorded);
        match &result {
            Ok(r) => self.say(format!("exported {} ({} bytes, {})", path.display(), r["bytes"], recipe.format.format_id())),
            Err(r) => self.say(format!("export {}: {r}", path.display())),
        }
        result
    }

    fn export_document_admitted(&mut self, path: &Path, recipe: &DocExportRecipe, overwrite: bool) -> Result<Value, Refusal> {
        if path.as_os_str().is_empty() {
            return Err(refuse("export_path", "no output path"));
        }
        if self.is_source(path) && !overwrite {
            return Err(refuse("export_over_source", path.display().to_string()));
        }
        if self.run.snapshot().is_none() {
            return Err(refuse("nothing_open", "open a document first"));
        }
        let job = self.admit_export_job()?;
        let result = self.export_document_inner(path, recipe, overwrite);
        self.settle_export_job(job, result)
    }

    /// One export job of the open document admitted through the viewer's JobPort before anything is
    /// rendered or written (the `as_export_job` pattern of `layout_export`): a refusal is typed
    /// `overloaded`, `canceled` or `stale_revision` with the port's detail.
    pub(in crate::view) fn admit_export_job(&self) -> Result<AdmittedJob, Refusal> {
        self.admit_export_job_with(&self.job_admission())
    }

    /// [`Viewer::admit_export_job`] through `admission` (an export of a derived resource:
    /// [`Viewer::job_admission_of`]).
    pub(in crate::view) fn admit_export_job_with(&self, admission: &JobAdmission) -> Result<AdmittedJob, Refusal> {
        let head = self.document_head();
        let correlation = admission.next_correlation();
        admission.admit(admission.request(Action::Export, 0, head, correlation), head, self.cancel).map_err(|r| refuse(r.code(), refusal_json(r).to_string()))
    }

    /// Settles `job` once for an export route whose refusal is text (`Viewer::export`, the vector exports;
    /// lane SMB-R60-FOLLOWUPS-1, review F3 of c43f2d90): `completed` (`completed_stale` when the head moved)
    /// or `failed` with the route's `code`.
    pub(in crate::view) fn settle_export_text<T>(&self, job: AdmittedJob, result: Result<T, String>, code: &'static str) -> Result<T, String> {
        let _ = match &result {
            Ok(_) => job.complete(self.document_head()),
            Err(_) => job.settle(JobOutcome::Failed(code)),
        };
        result
    }

    /// Settles `job` once from its export's result: `completed` (`completed_stale` when the head moved),
    /// `canceled`, or `failed` with the refusal's code; a result carries the settlement as `job`.
    pub(in crate::view) fn settle_export_job(&self, job: AdmittedJob, result: Result<Value, Refusal>) -> Result<Value, Refusal> {
        match result {
            Ok(mut report) => {
                report["job"] = settlement_json(&job.complete(self.document_head()));
                Ok(report)
            }
            Err(e) => {
                job.settle(if e.code == "canceled" { JobOutcome::Canceled } else { JobOutcome::Failed(e.code) });
                Err(e)
            }
        }
    }

    fn export_document_inner(&mut self, path: &Path, recipe: &DocExportRecipe, overwrite: bool) -> Result<Value, Refusal> {
        let text = self.prepared_text().map_err(|e| refuse("render_text", e))?;
        let vectors = self.prepared_vectors().map_err(|e| refuse("render_vector", e))?;
        let snap = self.run.snapshot().ok_or_else(|| refuse("nothing_open", "open a document first"))?;
        // The artboard Go to Artboard chose, as the canvas and the PNG export frame it (lane SMB-ARTBOARD-EXPORT-1, V4-06).
        let framed = self.framed_snapshot().map_err(|e| refuse(e.code, e.detail))?;
        let frame = render::render_linear_with_records(framed.as_ref().unwrap_or(snap), &self.assets, Some(&text), vectors.as_ref(), self.records.as_ref(), self.cancel).map_err(|e| refuse(e.code, e.detail))?;
        let doc = snap.document();
        let resource = DomainId::parse(&doc.document_id).map_err(|_| refuse("document_id", "unparseable"))?;
        let source = LayerSource { doc, assets: &self.assets };
        let exported = export_frame(&frame, recipe, Some(&source), &resource, self.run.actor(), self.cancel)?;
        let written = if overwrite { write_atomic(path, &exported.bytes) } else { write_new(path, &exported.bytes) };
        written.map_err(|code| refuse(code, path.display().to_string()))?;
        let mut report = exported.report;
        report["path"] = json!(path.to_string_lossy());
        report["backend"] = json!(frame.backend);
        report["revision"] = json!(frame.revision);
        report["render_notes"] = json!(frame.notes);
        // The canvas is the uncropped develop; a stored develop crop is applied by the develop export only.
        if self.develop_crop_stored()
            && let Some(notes) = report["notes"].as_array_mut()
        {
            notes.push(json!("develop_crop_not_applied"));
        }
        Ok(report)
    }
}

/// The batch runner's non-native output targets (STU-AUT-129, scenario photo-retouch step 116): `format_id`
/// `png`, `jpeg`, `tiff` or `psd` with its default options, or `export_recipe_id` naming a recipe added here (its
/// format must be the target's). Each input's document is checkpointed into a unique temp beside the output,
/// opened in a viewer attributed to the job's context, exported by [`Viewer::export_document`] and the temp is
/// removed. Admission (lane SMB-R60-FOLLOWUPS-1 item 1, review c43f2d90 batch-port ruling): with
/// [`RecipeTargets::with_job_port`] that viewer is bound to the caller's JobPort, so its `export_document`
/// admits through it once per target (a refusal is the target's typed code, `overloaded`, `canceled` or
/// `stale_revision`, and no output is written); without one (the CLI, no host port) it admits through the
/// viewer's own asset-local port. Studio reading: `colour_profile_policy` and `resize_policy` stay refused by the runner; the recipe
/// carries colour and size.
pub struct RecipeTargets {
    recipes: BTreeMap<String, DocExportRecipe>,
    context: RunContext,
    /// The caller's JobPort each export viewer is bound to ([`RecipeTargets::with_job_port`]).
    port: Option<std::sync::Arc<dyn hsk_studio_session::JobPort>>,
    /// The last export report, by output path.
    pub reports: Vec<(PathBuf, Value)>,
}

impl RecipeTargets {
    pub fn new(context: RunContext) -> Self {
        Self { recipes: BTreeMap::new(), context, port: None, reports: Vec::new() }
    }

    /// Targets whose exports admit through `port`, the caller's bound JobPort (type docs).
    #[must_use]
    pub fn with_job_port(mut self, port: std::sync::Arc<dyn hsk_studio_session::JobPort>) -> Self {
        self.port = Some(port);
        self
    }

    /// Adds a recipe by its `recipe_id` (required; a second recipe with one id is `recipe_id_duplicate`).
    pub fn insert(&mut self, recipe: DocExportRecipe) -> Result<(), Refusal> {
        let id = recipe.recipe_id.clone().ok_or_else(|| refuse("recipe_id_missing", "a batch recipe needs recipe_id"))?;
        if self.recipes.contains_key(&id) {
            return Err(refuse("recipe_id_duplicate", id));
        }
        self.recipes.insert(id, recipe);
        Ok(())
    }

    fn recipe(&self, target: &OutputTarget) -> Result<DocExportRecipe, &'static str> {
        match &target.export_recipe_id {
            None => DocFormat::default_for(&target.format_id).map(DocExportRecipe::of_format).ok_or("batch_output_format_unsupported"),
            Some(id) => {
                let r = self.recipes.get(id).ok_or("batch_export_recipe_unknown")?;
                if r.format.format_id() != target.format_id {
                    return Err("batch_export_recipe_format_mismatch");
                }
                Ok(r.clone())
            }
        }
    }
}

impl TargetExporter for RecipeTargets {
    fn extension(&self, target: &OutputTarget) -> Result<&'static str, &'static str> {
        self.recipe(target).map(|r| r.format.extension())
    }

    fn write(&mut self, run: &mut hsk_studio_harness_core::Run<'_>, target: &OutputTarget, path: &Path, replace: bool) -> Result<(), String> {
        let recipe = self.recipe(target).map_err(str::to_string)?;
        let bytes = run.checkpoint().ok_or_else(|| "batch_checkpoint_failed".to_string())?;
        let (temp, mut file) = create_unique_sibling(path, ".batch-src.handshake").map_err(|_| "batch_destination_unavailable".to_string())?;
        let written = file.write_all(&bytes).and_then(|()| file.sync_all());
        drop(file);
        let result = written.map_err(|_| "batch_destination_unavailable".to_string()).and_then(|()| {
            let mut viewer = Viewer::with_context(Prefer::Cpu, hsk_studio_render_gpu::DeviceOptions::default(), self.context.clone());
            if let Some(port) = &self.port {
                viewer.set_job_port(std::sync::Arc::clone(port));
            }
            viewer.open(&temp).map_err(|_| "batch_export_open_failed".to_string())?;
            let report = viewer.export_document(path, &recipe, replace).map_err(|r| r.code.to_string())?;
            self.reports.push((path.to_path_buf(), report));
            Ok(())
        });
        let _ = std::fs::remove_file(&temp);
        result
    }
}
