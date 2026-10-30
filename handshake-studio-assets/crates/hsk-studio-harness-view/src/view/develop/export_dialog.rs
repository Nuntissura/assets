//! Export dialog of the develop panel (lane SMB-LR-EXPORT-GUI-1; LR-33 of
//! `research/parity-20261023-adobe-print.json`): the export stage of `hsk-studio-develop` (lane
//! SMB-LR-EXPORT-SIZING-1: image sizing, output sharpening, graphic watermark between development and
//! the output transfer) as viewer state beside [`super::DevelopExport`], plus the window's TIFF and DNG
//! buttons.
//!
//! * The stage is per developed document (`DevelopDoc`) and OFF by default: no stage means the
//!   unstaged develop entries run, so every export is byte-identical to the exports before this lane
//!   (pinned in `harness tests/viewer_develop_export_stage.rs`). Switching it on starts from
//!   `ExportStage::default()`, develop's own defaults (`EXPORT_STAGE_KEYS` names each one's source;
//!   `studio_declared` ones are labelled so in the dialog). This module restates no default and no bound.
//! * Every field has a stable id `develop.export.<key>` (the keys of `EXPORT_STAGE_KEYS`, plus
//!   `watermark_size_value`, `watermark_text` and `watermark_graphic`); [`Viewer::set_develop_export_field`]
//!   writes one field from its text exactly as the window does, so agents, the CLI and the window share
//!   one path. Nothing is clamped here: a number goes to the stage as typed and develop's
//!   `validate_export_stage` / `fit_output_size` decide; their typed error is shown inline and disables
//!   the PNG / JPEG / TIFF export. Text that is not a number is this module's typed refusal
//!   `export_field_not_a_number` (shown the same way).
//! * Groups follow STU-IO-135's order (image_sizing, watermarking, output_sharpening); each row shows the
//!   STU-IO-134 class of its key (stored setting or unclassified, never promoted) and its default source.
//! * The DNG export does not run the stage (develop's DNG preview is not staged); its report says so.
//! * Text watermarks are develop's typed refusal `text_watermark_unsupported` (needs hsk-studio-type).
//!
//! Lightroom's export dialog layout is the reference and is UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::file_dialog::{FileDialog, Outcome, Purpose};
use super::{DevelopExport, Refusal};
use hsk_studio_develop::{
    DngCompression, DngPreview, EXPORT_STAGE_KEYS, ExportColourSpace, ExportStage, ExportStageError, ExportStageReceipt, KeyStorage,
    MAX_WATERMARK_PIXELS, MAX_WATERMARK_SIDE, MarkTransfer, OutputDepth, ResizeType, ResolutionUnit, SharpenMedia, SizeFit, SizeUnit,
    TiffCompression, WatermarkAnchor, WatermarkGraphic, WatermarkRotation, WatermarkSize, WatermarkSource, fit_output_size,
    validate_export_stage,
};
use hsk_studio_interop_raster as raster;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Stable widget names of the dialog (egui `Id` sources through `view::widget::id`).
pub mod widget {
    /// The stage on / off checkbox ("Apply export settings").
    pub const STAGE: &str = "develop.export.stage";
    /// The "W x H px" size preview.
    pub const PREVIEW: &str = "develop.export.preview";
    /// The inline typed error of the stage.
    pub const ERROR: &str = "develop.export.error";
    pub const EXPORT_TIFF: &str = "develop.export.tiff";
    pub const EXPORT_DNG: &str = "develop.export.dng";
    /// Percent of a proportional watermark size.
    pub const WATERMARK_SIZE_VALUE: &str = "develop.export.watermark_size_value";
    /// Text of a text watermark (refused typed at export).
    pub const WATERMARK_TEXT: &str = "develop.export.watermark_text";
    /// The graphic's path; its button opens the file dialog.
    pub const WATERMARK_GRAPHIC: &str = "develop.export.watermark_graphic";
    /// A stage key row: `develop.export.<key>` (keys of `hsk_studio_develop::EXPORT_STAGE_KEYS`).
    pub fn key(key: &str) -> String {
        format!("develop.export.{key}")
    }
    /// A group header: `develop.export.group.<group>`.
    pub fn group(group: &str) -> String {
        format!("develop.export.group.{group}")
    }
}

/// STU-IO-135 group order (the subset the stage carries) and the dialog's group titles.
pub const GROUPS: [(&str, &str); 3] = [("image_sizing", "Image Sizing"), ("watermarking", "Watermarking"), ("output_sharpening", "Output Sharpening")];

/// The window's TIFF button: develop's TIFF defaults (`TiffExportSettings::new`, the CLI's `--tiff`
/// defaults: 16-bit, uncompressed, sRGB, little-endian, classic TIFF, no dpi).
pub const TIFF_BUTTON: DevelopExport = DevelopExport::Tiff {
    depth: OutputDepth::Sixteen,
    compression: TiffCompression::None,
    predictor: false,
    colour_space: ExportColourSpace::Srgb,
    big_endian: false,
    big_tiff: false,
    dpi: None,
};

/// The window's DNG button: the CLI's `--dng` defaults (uncompressed, medium preview, sources with
/// features the DNG cannot carry refused).
pub const DNG_BUTTON: DevelopExport = DevelopExport::Dng { compression: DngCompression::None, preview: DngPreview::Medium, allow_dropped: false };

/// Fields holding a number (typed text; the stage keeps the last parsed value).
const NUMBER_KEYS: [&str; 9] =
    ["max_width", "max_height", "resize_percentage", "megapixels", "resolution", "watermark_opacity", "watermark_inset_x", "watermark_inset_y", "watermark_size_value"];

const RESIZE_TYPES: [ResizeType; 6] =
    [ResizeType::WidthHeight, ResizeType::Dimensions, ResizeType::LongEdge, ResizeType::ShortEdge, ResizeType::Megapixels, ResizeType::Percentage];
const SIZE_UNITS: [SizeUnit; 3] = [SizeUnit::Pixels, SizeUnit::Inches, SizeUnit::Centimetres];
const RESOLUTION_UNITS: [ResolutionUnit; 2] = [ResolutionUnit::PixelsPerInch, ResolutionUnit::PixelsPerCm];
const MEDIA: [SharpenMedia; 3] = [SharpenMedia::Screen, SharpenMedia::Glossy, SharpenMedia::Matte];
const LEVELS: [(&str, &str); 3] = [("1", "Low"), ("2", "Standard"), ("3", "High")];
const SOURCES: [&str; 3] = ["none", "graphic", "text"];
const SIZES: [&str; 3] = ["proportional", "fit", "fill"];
const ROTATIONS: [WatermarkRotation; 4] = [WatermarkRotation::R0, WatermarkRotation::R90, WatermarkRotation::R180, WatermarkRotation::R270];
const ANCHORS: [WatermarkAnchor; 9] = [
    WatermarkAnchor::TopLeft,
    WatermarkAnchor::Top,
    WatermarkAnchor::TopRight,
    WatermarkAnchor::Left,
    WatermarkAnchor::Centre,
    WatermarkAnchor::Right,
    WatermarkAnchor::BottomLeft,
    WatermarkAnchor::Bottom,
    WatermarkAnchor::BottomRight,
];

/// Key of a watermark anchor (develop's default text `bottom_right`).
pub fn anchor_key(a: WatermarkAnchor) -> &'static str {
    match a {
        WatermarkAnchor::TopLeft => "top_left",
        WatermarkAnchor::Top => "top",
        WatermarkAnchor::TopRight => "top_right",
        WatermarkAnchor::Left => "left",
        WatermarkAnchor::Centre => "centre",
        WatermarkAnchor::Right => "right",
        WatermarkAnchor::BottomLeft => "bottom_left",
        WatermarkAnchor::Bottom => "bottom",
        WatermarkAnchor::BottomRight => "bottom_right",
    }
}

fn storage_key(s: KeyStorage) -> &'static str {
    match s {
        KeyStorage::StoredSetting => "stored_setting",
        KeyStorage::Unclassified => "unclassified",
    }
}

/// What the watermark source combo shows (the stage holds `None` while "graphic" has no graphic yet).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SourceKind {
    #[default]
    None,
    Graphic,
    Text,
}

impl SourceKind {
    fn key(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Graphic => "graphic",
            Self::Text => "text",
        }
    }
}

/// Per-document state of the dialog.
#[derive(Default)]
pub(in crate::view) struct ExportDialog {
    /// The export stage; `None` = off (unstaged exports, today's bytes).
    stage: Option<ExportStage>,
    /// Number fields as typed (key -> text); a key without text shows the stage's value.
    text: BTreeMap<&'static str, String>,
    /// Number fields whose text does not parse (the stage keeps the last parsed value).
    unparsed: BTreeMap<&'static str, String>,
    kind: SourceKind,
    watermark_text: String,
    /// The loaded graphic and the path it came from.
    graphic: Option<(PathBuf, WatermarkGraphic)>,
    /// The file dialog choosing the graphic (own window; opened only while the shell's is closed).
    file: Option<FileDialog>,
}

fn field_key(id: &str) -> Option<&'static str> {
    let key = id.strip_prefix("develop.export.")?;
    let all = EXPORT_STAGE_KEYS.iter().map(|k| k.key).chain(["stage", "watermark_size_value", "watermark_text", "watermark_graphic"]);
    all.into_iter().find(|k| *k == key)
}

fn boolean(v: &str) -> Option<bool> {
    match v {
        "true" | "on" => Some(true),
        "false" | "off" => Some(false),
        _ => None,
    }
}

fn number_text(v: f64) -> String {
    format!("{v}")
}

/// The stage's value of `key` as field text.
fn value_text(stage: &ExportStage, key: &str) -> String {
    let (s, sh, w) = (&stage.sizing, &stage.sharpening, &stage.watermark);
    match key {
        "constrain_size" => s.constrain_size.to_string(),
        "resize_type" => s.resize_type.spec_member().into(),
        "max_width" => number_text(s.max_width),
        "max_height" => number_text(s.max_height),
        "size_unit" => s.size_unit.spec_member().into(),
        "resize_percentage" => number_text(s.percentage),
        "megapixels" => number_text(s.megapixels),
        "resolution" => number_text(s.resolution),
        "resolution_unit" => s.resolution_unit.spec_member().into(),
        "do_not_enlarge" => s.do_not_enlarge.to_string(),
        "output_sharpening_enabled" => sh.enabled.to_string(),
        "output_sharpening_media" => sh.media.spec_member().into(),
        "output_sharpening_level" => sh.level.to_string(),
        "use_watermark" => w.use_watermark.to_string(),
        "watermark_source" => match &w.source {
            WatermarkSource::None => "none".into(),
            WatermarkSource::Graphic(_) => "graphic".into(),
            WatermarkSource::Text(_) => "text".into(),
        },
        "watermark_opacity" => number_text(w.opacity_percent),
        "watermark_size" => w.size.rule().into(),
        "watermark_size_value" => match w.size {
            WatermarkSize::Proportional(p) => number_text(p),
            WatermarkSize::Fit | WatermarkSize::Fill => String::new(),
        },
        "watermark_anchor" => anchor_key(w.anchor).into(),
        "watermark_inset_x" => number_text(w.inset_x_percent),
        "watermark_inset_y" => number_text(w.inset_y_percent),
        "watermark_rotation" => w.rotation.degrees().to_string(),
        _ => String::new(),
    }
}

fn not_a_value(key: &str, value: &str) -> Refusal {
    Refusal::new("export_field_value", format!("{key}: `{value}` is not one of its values"))
}

impl ExportDialog {
    pub(super) fn stage(&self) -> Option<&ExportStage> {
        self.stage.as_ref()
    }

    fn set_stage(&mut self, stage: Option<ExportStage>) {
        self.text.clear();
        self.unparsed.clear();
        self.kind = match stage.as_ref().map(|s| &s.watermark.source) {
            Some(WatermarkSource::Graphic(g)) => {
                if self.graphic.as_ref().is_none_or(|(_, have)| have != g) {
                    self.graphic = Some((PathBuf::new(), g.clone()));
                }
                SourceKind::Graphic
            }
            Some(WatermarkSource::Text(t)) => {
                self.watermark_text = t.clone();
                SourceKind::Text
            }
            Some(WatermarkSource::None) | None => SourceKind::None,
        };
        self.stage = stage;
    }

    /// The text a field shows.
    fn shown(&self, key: &'static str) -> String {
        if let Some(t) = self.text.get(key) {
            return t.clone();
        }
        match (key, self.stage.as_ref()) {
            ("watermark_source", _) => self.kind.key().into(),
            ("watermark_text", _) => self.watermark_text.clone(),
            ("watermark_graphic", _) => self.graphic.as_ref().map(|(p, _)| p.to_string_lossy().into_owned()).unwrap_or_default(),
            (_, Some(s)) => value_text(s, key),
            (_, None) => String::new(),
        }
    }

    fn source(&self) -> WatermarkSource {
        match self.kind {
            SourceKind::None => WatermarkSource::None,
            SourceKind::Graphic => self.graphic.as_ref().map_or(WatermarkSource::None, |(_, g)| WatermarkSource::Graphic(g.clone())),
            SourceKind::Text => WatermarkSource::Text(self.watermark_text.clone()),
        }
    }

    /// Writes one field from its text (the window's path and [`Viewer::set_develop_export_field`]).
    fn write(&mut self, key: &'static str, value: &str) -> Result<(), Refusal> {
        if key == "stage" {
            let on = boolean(value).ok_or_else(|| not_a_value(key, value))?;
            if on != self.stage.is_some() {
                self.set_stage(on.then(ExportStage::default));
            }
            return Ok(());
        }
        if key == "watermark_graphic" {
            return Err(Refusal::new("export_field_value", "watermark_graphic is chosen through the file dialog (Viewer::load_develop_watermark)"));
        }
        let Some(stage) = self.stage.as_mut() else {
            return Err(Refusal::new("export_stage_off", format!("{key}: switch the export settings on first ({})", widget::STAGE)));
        };
        if NUMBER_KEYS.contains(&key) {
            self.text.insert(key, value.to_owned());
            let Ok(v) = value.trim().parse::<f64>() else {
                self.unparsed.insert(key, value.to_owned());
                return Err(Refusal::new("export_field_not_a_number", format!("{key}: `{value}` is not a number")));
            };
            self.unparsed.remove(key);
            let (s, w) = (&mut stage.sizing, &mut stage.watermark);
            match key {
                "max_width" => s.max_width = v,
                "max_height" => s.max_height = v,
                "resize_percentage" => s.percentage = v,
                "megapixels" => s.megapixels = v,
                "resolution" => s.resolution = v,
                "watermark_opacity" => w.opacity_percent = v,
                "watermark_inset_x" => w.inset_x_percent = v,
                "watermark_inset_y" => w.inset_y_percent = v,
                _ => {
                    // watermark_size_value: the percent of a proportional size (a fit / fill size has none).
                    if !matches!(w.size, WatermarkSize::Proportional(_)) {
                        return Err(Refusal::new("export_field_value", format!("watermark_size_value applies to a proportional size, not {}", w.size.rule())));
                    }
                    w.size = WatermarkSize::Proportional(v);
                }
            }
            return Ok(());
        }
        let bad = || not_a_value(key, value);
        let (s, sh, w) = (&mut stage.sizing, &mut stage.sharpening, &mut stage.watermark);
        match key {
            "constrain_size" => s.constrain_size = boolean(value).ok_or_else(bad)?,
            "do_not_enlarge" => s.do_not_enlarge = boolean(value).ok_or_else(bad)?,
            "output_sharpening_enabled" => sh.enabled = boolean(value).ok_or_else(bad)?,
            "use_watermark" => w.use_watermark = boolean(value).ok_or_else(bad)?,
            "resize_type" => s.resize_type = *RESIZE_TYPES.iter().find(|t| t.spec_member() == value).ok_or_else(bad)?,
            "size_unit" => s.size_unit = *SIZE_UNITS.iter().find(|t| t.spec_member() == value).ok_or_else(bad)?,
            "resolution_unit" => s.resolution_unit = *RESOLUTION_UNITS.iter().find(|t| t.spec_member() == value).ok_or_else(bad)?,
            "output_sharpening_media" => sh.media = *MEDIA.iter().find(|t| t.spec_member() == value).ok_or_else(bad)?,
            // A level outside 1..=3 goes to develop as typed (its typed refusal); only non-integers are refused here.
            "output_sharpening_level" => sh.level = value.trim().parse::<u8>().map_err(|_| bad())?,
            "watermark_anchor" => w.anchor = *ANCHORS.iter().find(|a| anchor_key(**a) == value).ok_or_else(bad)?,
            "watermark_rotation" => w.rotation = *ROTATIONS.iter().find(|r| r.degrees().to_string() == value).ok_or_else(bad)?,
            "watermark_size" => {
                w.size = match value {
                    "proportional" => match w.size {
                        WatermarkSize::Proportional(p) => WatermarkSize::Proportional(p),
                        // Back to proportional: develop's default size (`WatermarkSettings::default`).
                        WatermarkSize::Fit | WatermarkSize::Fill => hsk_studio_develop::WatermarkSettings::default().size,
                    },
                    "fit" => WatermarkSize::Fit,
                    "fill" => WatermarkSize::Fill,
                    _ => return Err(bad()),
                };
                self.text.remove("watermark_size_value");
                self.unparsed.remove("watermark_size_value");
            }
            "watermark_source" => {
                self.kind = match value {
                    "none" => SourceKind::None,
                    "graphic" => SourceKind::Graphic,
                    "text" => SourceKind::Text,
                    _ => return Err(bad()),
                };
                let source = self.source();
                if let Some(stage) = self.stage.as_mut() {
                    stage.watermark.source = source;
                }
            }
            "watermark_text" => {
                self.watermark_text = value.to_owned();
                if self.kind == SourceKind::Text {
                    w.source = WatermarkSource::Text(value.to_owned());
                }
            }
            _ => return Err(Refusal::new("unknown_export_field", key.to_owned())),
        }
        Ok(())
    }

    /// The stage's verdict on a `width` x `height` developed image: the fitted size, or the typed error
    /// (a field that does not parse first, then develop's `validate_export_stage`, then `fit_output_size`).
    /// `None` while the stage is off.
    fn check(&self, size: (u32, u32)) -> Option<Result<SizeFit, Refusal>> {
        let stage = self.stage.as_ref()?;
        if let Some((key, text)) = self.unparsed.iter().next() {
            return Some(Err(Refusal::new("export_field_not_a_number", format!("{key}: `{text}` is not a number"))));
        }
        Some(validate_export_stage(stage).and_then(|_| fit_output_size(&stage.sizing, size.0, size.1)).map_err(|e| stage_refusal(&e)))
    }

    /// Whether the PNG / JPEG / TIFF exports may run (stage off, or a valid stage).
    pub(super) fn ready(&self, size: (u32, u32)) -> Result<(), Refusal> {
        match self.check(size) {
            None | Some(Ok(_)) => Ok(()),
            Some(Err(r)) => Err(r),
        }
    }

    /// The dialog for agents (`develop.export_stage` of the state JSON).
    pub(super) fn json(&self, size: (u32, u32)) -> Value {
        let check = self.check(size);
        let keys: Vec<Value> = GROUPS
            .iter()
            .flat_map(|(g, _)| EXPORT_STAGE_KEYS.iter().filter(move |k| k.group == *g))
            .map(|k| {
                json!({"key": k.key, "id": widget::key(k.key), "group": k.group, "storage": storage_key(k.storage), "default": k.default,
                    "default_source": k.default_source, "value": self.stage.as_ref().map(|_| self.shown(k.key))})
            })
            .collect();
        json!({
            "on": self.stage.is_some(),
            "groups": GROUPS.iter().map(|(g, _)| *g).collect::<Vec<_>>(),
            "keys": keys,
            "watermark_size_value": self.stage.as_ref().map(|_| self.shown("watermark_size_value")),
            "watermark_text": self.watermark_text,
            "watermark_graphic": self.graphic.as_ref().map(|(p, g)| json!({"path": p.to_string_lossy(), "width": g.width, "height": g.height})),
            "unparsed": self.unparsed,
            "source_size": [size.0, size.1],
            "preview": match &check { Some(Ok(f)) => Some(format!("{} x {} px", f.width, f.height)), _ => None },
            "preview_size": match &check { Some(Ok(f)) => Some(json!([f.width, f.height])), _ => None },
            "fit_rule": match &check { Some(Ok(f)) => Some(f.rule), _ => None },
            "error": match &check { Some(Err(r)) => Some(json!({"code": r.code, "detail": r.detail})), _ => None },
            "export_enabled": !matches!(check, Some(Err(_))),
        })
    }
}

/// A stage refusal as the viewer's typed refusal (develop's code; the key or module it names).
pub(super) fn stage_refusal(e: &ExportStageError) -> Refusal {
    Refusal::new(e.code(), if e.detail().is_empty() { format!("{e:?}") } else { e.detail().to_owned() })
}

/// What the stage did, for the export report and the status log.
pub fn receipt_json(r: &ExportStageReceipt) -> Value {
    json!({
        "applied": true,
        "source_size": [r.source_size.0, r.source_size.1],
        "output_size": [r.output_size.0, r.output_size.1],
        "fit_rule": r.fit_rule,
        "scale": r.scale,
        "resized": r.resized,
        "filter": r.filter,
        "resolution_ppi": r.resolution_ppi,
        "sharpening": r.sharpening.map(|s| json!({"media": s.media.spec_member(), "level": s.level, "amount": s.amount.spec_member(),
            "sigma_px": s.sigma_px, "kernel_radius": s.kernel_radius, "strength": s.strength})),
        "sharpen_out_of_range": {"below_zero": r.sharpen_out_of_range.below_zero, "above_one": r.sharpen_out_of_range.above_one},
        "watermark": r.watermark.map(|w| json!({"x": w.x, "y": w.y, "width": w.width, "height": w.height, "size_rule": w.size_rule,
            "anchor": anchor_key(w.anchor), "rotation_degrees": w.rotation_degrees, "opacity_percent": w.opacity_percent})),
        "out_of_range": {"below_zero": r.out_of_range.below_zero, "above_one": r.out_of_range.above_one},
    })
}

/// A decoded PNG / JPEG as a straight-RGBA watermark graphic (sRGB-encoded samples, Studio reading;
/// an embedded profile is not applied). Size caps are develop's, checked before converting.
fn graphic_from(img: &raster::RasterImage) -> Result<WatermarkGraphic, Refusal> {
    let too_large = || Refusal::new("watermark_too_large", format!("{} x {}", img.width, img.height));
    let pixels = u64::from(img.width).checked_mul(u64::from(img.height)).ok_or_else(too_large)?;
    if img.width > MAX_WATERMARK_SIDE || img.height > MAX_WATERMARK_SIDE || pixels > MAX_WATERMARK_PIXELS {
        return Err(too_large());
    }
    let channels: usize = match img.colour {
        raster::Colour::Gray => 1,
        raster::Colour::GrayA => 2,
        raster::Colour::Rgb => 3,
        raster::Colour::Rgba => 4,
        raster::Colour::Cmyk => return Err(Refusal::new("watermark_colour_unsupported", "CMYK graphic")),
    };
    let unit: Vec<f32> = match &img.samples {
        raster::Samples::U8(v) => v.iter().map(|s| f32::from(*s) / 255.0).collect(),
        raster::Samples::U16(v) => v.iter().map(|s| f32::from(*s) / 65535.0).collect(),
    };
    let count = usize::try_from(pixels).map_err(|_| too_large())?;
    if count.checked_mul(channels) != Some(unit.len()) {
        return Err(Refusal::new("watermark_length_mismatch", format!("{} samples for {count} pixels", unit.len())));
    }
    let mut rgba = Vec::with_capacity(count.checked_mul(4).ok_or_else(too_large)?);
    for p in unit.chunks_exact(channels) {
        let px = match *p {
            [g] => [g, g, g, 1.0],
            [g, a] => [g, g, g, a],
            [r, g, b] => [r, g, b, 1.0],
            [r, g, b, a, ..] => [r, g, b, a],
            [] => [0.0; 4],
        };
        rgba.extend_from_slice(&px);
    }
    Ok(WatermarkGraphic { width: img.width, height: img.height, rgba, transfer: MarkTransfer::SrgbTrc })
}

impl Viewer {
    fn export_dialog_mut(&mut self) -> Result<&mut ExportDialog, Refusal> {
        self.develop.as_mut().map(|d| &mut d.export).ok_or_else(|| Refusal::new("not_a_raw_document", "open a camera raw or DNG first"))
    }

    /// Sets the develop export stage (sizing, output sharpening, watermark) the PNG / JPEG / TIFF
    /// exports run; stored as given (develop validates it at export and in the dialog).
    pub fn set_develop_export_stage(&mut self, stage: ExportStage) -> Result<(), Refusal> {
        self.export_dialog_mut()?.set_stage(Some(stage));
        Ok(())
    }

    /// Switches the export stage off: exports are the unstaged (pre-stage) bytes again.
    pub fn clear_develop_export_stage(&mut self) -> Result<(), Refusal> {
        self.export_dialog_mut()?.set_stage(None);
        Ok(())
    }

    /// The export stage, `None` while it is off (or no raw is open).
    pub fn develop_export_stage(&self) -> Option<&ExportStage> {
        self.develop.as_ref()?.export.stage()
    }

    /// Writes one dialog field from its text, exactly as the window does. `id` is the field's widget
    /// id (`develop.export.<key>`, see [`widget`]); numbers are passed unclamped; choices take the
    /// values the dialog offers (`true`/`false`, STU-IO-136 member names, `top_left`..`bottom_right`,
    /// `0`/`90`/`180`/`270`, `proportional`/`fit`/`fill`, `none`/`graphic`/`text`). A number that does
    /// not parse is kept as typed and refused `export_field_not_a_number` (export stays disabled).
    pub fn set_develop_export_field(&mut self, id: &str, value: &str) -> Result<(), Refusal> {
        let key = field_key(id).ok_or_else(|| Refusal::new("unknown_export_field", id.to_owned()))?;
        let result = self.export_dialog_mut()?.write(key, value);
        if let Err(r) = &result {
            self.say(format!("develop export setting {id} = {value}: {r}"));
        }
        result
    }

    /// Loads a PNG or JPEG as the watermark graphic (decoded through hsk-studio-interop-raster under
    /// develop's watermark size caps) and selects it as the watermark source.
    pub fn load_develop_watermark(&mut self, path: &Path) -> Result<(), Refusal> {
        if self.develop_export_stage().is_none() {
            return Err(Refusal::new("export_stage_off", format!("switch the export settings on first ({})", widget::STAGE)));
        }
        let side = MAX_WATERMARK_SIDE;
        // Every RGBA16 sample of the largest admitted graphic, the decoder's own aggregate budget.
        let decoded = MAX_WATERMARK_PIXELS.saturating_mul(8);
        let limits = raster::Limits { max_input_bytes: decoded, max_dimension: side, max_decoded_bytes: decoded, ..raster::Limits::default() };
        let size = std::fs::metadata(path).map_err(|_| Refusal::new("watermark_unreadable", path.display().to_string()))?.len();
        if size > limits.max_input_bytes {
            return Err(Refusal::new("watermark_too_large", format!("{size} bytes")));
        }
        let bytes = std::fs::read(path).map_err(|_| Refusal::new("watermark_unreadable", path.display().to_string()))?;
        let img = raster::decode(&bytes, &limits, self.cancel).map_err(|e| Refusal::new(e.code(), format!("watermark {}", path.display())))?;
        let graphic = graphic_from(&img)?;
        let dialog = self.export_dialog_mut()?;
        dialog.graphic = Some((path.to_path_buf(), graphic));
        dialog.write("watermark_source", "graphic")?;
        self.say(format!("develop export watermark graphic {} ({} x {})", path.display(), img.width, img.height));
        Ok(())
    }

    /// Size of the developed image the stage resizes (the develop window of the raw).
    pub(super) fn develop_export_source_size(&self) -> (u32, u32) {
        self.develop.as_ref().map_or((0, 0), |d| (d.prepared.window.width, d.prepared.window.height))
    }

    /// The dialog: the stage switch, the three STU-IO-135 groups, the size preview and the typed error.
    pub(super) fn export_dialog_ui(&mut self, ui: &mut egui::Ui) {
        let size = self.develop_export_source_size();
        let Some(d) = self.develop.as_ref() else {
            return;
        };
        let dlg = &d.export;
        let id = super::super::widget::id;
        let mut edits: Vec<(&'static str, String)> = Vec::new();
        let mut drawn: Vec<String> = Vec::new();
        let mut choose = false;
        ui.separator();
        ui.strong("Export settings");
        let mut on = dlg.stage.is_some();
        if ui.push_id(id(widget::STAGE), |ui| ui.checkbox(&mut on, "Apply export settings")).inner.on_hover_text("Image sizing, watermark and output sharpening on the PNG / JPEG / TIFF export (not DNG)").changed() {
            edits.push(("stage", on.to_string()));
        }
        drawn.push(widget::STAGE.into());
        if let Some(stage) = dlg.stage.as_ref() {
            for (group, title) in GROUPS {
                ui.push_id(id(&widget::group(group)), |ui| ui.label(egui::RichText::new(title).strong()));
                drawn.push(widget::group(group));
                for k in EXPORT_STAGE_KEYS.iter().filter(|k| k.group == group) {
                    let name = widget::key(k.key);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(k.key);
                        field(ui, dlg, stage, k.key, &name, &mut edits, &mut choose, &mut drawn);
                        let class = format!("{} | default {} ({})", storage_key(k.storage), k.default, k.default_source);
                        ui.small(class).on_hover_text("STU-IO-134 class of the key and where its default comes from; studio_declared = Studio picked it (STU-IO-138)");
                    });
                    drawn.push(name);
                }
            }
            let (text, colour) = match dlg.check(size) {
                Some(Ok(f)) => (format!("{} x {} px", f.width, f.height), None),
                Some(Err(r)) => (format!("{} x {} px (no size)", size.0, size.1), Some(r)),
                None => (String::new(), None),
            };
            ui.push_id(id(widget::PREVIEW), |ui| ui.label(text)).inner.on_hover_text("Output size from develop fit_output_size over the developed image");
            drawn.push(widget::PREVIEW.into());
            if let Some(r) = colour {
                ui.push_id(id(widget::ERROR), |ui| ui.colored_label(egui::Color32::RED, r.to_string()));
                drawn.push(widget::ERROR.into());
            }
        }
        let shell_dialog_open = self.file_dialog().is_some();
        let Some(d) = self.develop.as_mut() else {
            return;
        };
        if choose && !shell_dialog_open && d.export.file.is_none() {
            let start = d.export.graphic.as_ref().and_then(|(p, _)| p.parent().map(Path::to_path_buf)).filter(|p| !p.as_os_str().is_empty());
            let start = start.or_else(|| std::env::current_dir().ok()).unwrap_or_default();
            d.export.file = Some(FileDialog::new(Purpose::Open, &start, None));
        }
        let mut accepted = None;
        if let Some(fd) = d.export.file.as_mut() {
            let (outcome, names) = fd.ui(ui.ctx());
            drawn.extend(names);
            match outcome {
                Some(Outcome::Accept(path)) => accepted = Some(path),
                Some(Outcome::Cancel) => d.export.file = None,
                None => {}
            }
        }
        if accepted.is_some() {
            d.export.file = None;
        }
        self.drawn.extend(drawn);
        for (key, value) in edits {
            let _ = self.set_develop_export_field(&widget::key(key), &value);
        }
        if let Some(path) = accepted
            && let Err(r) = self.load_develop_watermark(&path)
        {
            self.say(format!("develop export watermark refused: {r}"));
        }
    }
}

/// One field of row `key` (its widget is under the id `name`).
#[allow(clippy::too_many_arguments)]
fn field(ui: &mut egui::Ui, dlg: &ExportDialog, stage: &ExportStage, key: &'static str, name: &str, edits: &mut Vec<(&'static str, String)>, choose: &mut bool, drawn: &mut Vec<String>) {
    let id = super::super::widget::id;
    let shown = dlg.shown(key);
    let mut combo = |ui: &mut egui::Ui, options: &[(String, String)]| {
        let current = options.iter().find(|(v, _)| *v == shown).map_or(shown.clone(), |(_, l)| l.clone());
        egui::ComboBox::from_id_salt(id(name)).selected_text(current).show_ui(ui, |ui| {
            for (value, label) in options {
                if ui.selectable_label(*value == shown, label).clicked() && *value != shown {
                    edits.push((key, value.clone()));
                }
            }
        });
    };
    let pairs = |v: &[&str]| v.iter().map(|s| ((*s).to_owned(), (*s).to_owned())).collect::<Vec<_>>();
    match key {
        "constrain_size" | "do_not_enlarge" | "output_sharpening_enabled" | "use_watermark" => {
            let mut b = shown == "true";
            if ui.push_id(id(name), |ui| ui.checkbox(&mut b, "")).inner.changed() {
                edits.push((key, b.to_string()));
            }
        }
        "resize_type" => combo(ui, &pairs(&RESIZE_TYPES.map(ResizeType::spec_member))),
        "size_unit" => combo(ui, &pairs(&SIZE_UNITS.map(SizeUnit::spec_member))),
        "resolution_unit" => combo(ui, &pairs(&RESOLUTION_UNITS.map(ResolutionUnit::spec_member))),
        "output_sharpening_media" => combo(ui, &pairs(&MEDIA.map(SharpenMedia::spec_member))),
        "output_sharpening_level" => combo(ui, &LEVELS.map(|(v, l)| (v.to_owned(), format!("{v} {l}")))),
        "watermark_anchor" => combo(ui, &pairs(&ANCHORS.map(anchor_key))),
        "watermark_rotation" => combo(ui, &ROTATIONS.map(|r| (r.degrees().to_string(), format!("{} deg", r.degrees())))),
        "watermark_size" => {
            combo(ui, &pairs(&SIZES));
            if matches!(stage.watermark.size, WatermarkSize::Proportional(_)) {
                text_field(ui, dlg, "watermark_size_value", widget::WATERMARK_SIZE_VALUE, edits);
                ui.label("% of the output width");
                drawn.push(widget::WATERMARK_SIZE_VALUE.into());
            }
        }
        "watermark_source" => {
            combo(ui, &pairs(&SOURCES));
            match dlg.kind {
                SourceKind::Graphic => {
                    let label = dlg.graphic.as_ref().map_or("Choose graphic...".to_owned(), |(p, g)| {
                        let file = p.file_name().map_or_else(|| "graphic".into(), |f| f.to_string_lossy().into_owned());
                        format!("{file} ({} x {})", g.width, g.height)
                    });
                    if ui.push_id(id(widget::WATERMARK_GRAPHIC), |ui| ui.button(label)).inner.on_hover_text("A PNG or JPEG through the file dialog").clicked() {
                        *choose = true;
                    }
                    drawn.push(widget::WATERMARK_GRAPHIC.into());
                }
                SourceKind::Text => {
                    text_field(ui, dlg, "watermark_text", widget::WATERMARK_TEXT, edits);
                    drawn.push(widget::WATERMARK_TEXT.into());
                }
                SourceKind::None => {}
            }
        }
        _ => text_field(ui, dlg, key, name, edits),
    }
}

fn text_field(ui: &mut egui::Ui, dlg: &ExportDialog, key: &'static str, name: &str, edits: &mut Vec<(&'static str, String)>) {
    let mut text = dlg.shown(key);
    let bad = dlg.unparsed.contains_key(key);
    let mut edit = egui::TextEdit::singleline(&mut text).id(super::super::widget::id(name)).desired_width(70.0);
    if bad {
        edit = edit.text_color(egui::Color32::RED);
    }
    if ui.add(edit).changed() {
        edits.push((key, text));
    }
}
