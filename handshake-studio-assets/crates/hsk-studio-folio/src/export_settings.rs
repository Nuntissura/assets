//! Node export settings (lane SMB-ARTBOARD-EXPORT-1; [STU-IO-111] node-render setting record set,
//! [STU-IO-112] raster image settings, [STU-IO-113] SVG and node PDF settings, [STU-IO-114] export
//! constraint, [STU-IO-117] format-suffix and collision rule; [STU-IO-010] single-target export per
//! layer and per artboard).
//!
//! [`StudioExportSettings`] (`hsk.studio.export_settings@1`) holds, beside the document, the ordered
//! render settings of every node marked for export: a layer or an artboard ([STU-IO-111]: "A node,
//! layer, artboard or slice declares an ordered array of render settings"; no slice record exists
//! yet). Each setting is exactly one closed shape, tagged by `format`: `PNG` and `JPG` carry
//! [`StudioRasterImageSettings`], `SVG` [`StudioSvgSettings`], `PDF` [`StudioNodePdfSettings`] (the
//! node-render PDF, not the document PDF export of [STU-IO-125]). Multi-scale variants are several
//! settings, each with its own `SCALE` constraint and suffix ([STU-IO-114]).
//!
//! An entry naming a node the document no longer holds is inert, as artboard options are: a delete
//! leaves the record valid and an undo brings the node back with its settings. A node with no setting
//! has no entry; an entry with an empty list is refused, so one state has one form.
//!
//! Studio readings, declared (vendor parity UNVERIFIED, D5): `JSON_TREE` ([STU-IO-113]), `GIF`
//! ([STU-IO-115]) and the video settings ([STU-IO-116]) are not admitted yet, so an unknown `format` is
//! refused; [STU-IO-112] states no default for `suffix` (empty here) or `colour_profile` (`DOCUMENT`
//! here); a suffix holds no path separator, reserved filename character or control character and at
//! most [`MAX_EXPORT_SUFFIX_BYTES`]; a node holds at most [`MAX_SETTINGS_PER_NODE`] settings. The
//! file a setting writes is named by the node, the suffix and the format's extension, so two settings
//! of one node with the same suffix and extension collide: [STU-IO-117] refuses that here, at
//! authoring time, never as an overwrite at export time.
//!
//! The record arrives from untrusted wire JSON: integer arithmetic is denied (workflow V4-01), every
//! list is counted before it is walked and charged against the [`Budget`], and a parse refuses bytes
//! over [`MAX_EXPORT_SETTINGS_INPUT_BYTES`] whatever the budget allows.
#![deny(clippy::arithmetic_side_effects)]
use super::{Budget, Code, NodeKind, NodeRef, Result, StudioDocument, Unresolved, err, id, sequence_preset::parse_wire, values::Ctx};
use hsk_studio_accord::CancellationToken;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

schema_token!(ExportSettingsSchema, "hsk.studio.export_settings@1");

/// Nodes one record set may hold.
pub const MAX_EXPORT_NODES: usize = 4096;
/// Settings one node may hold.
pub const MAX_SETTINGS_PER_NODE: usize = 64;
/// Longest suffix in bytes.
pub const MAX_EXPORT_SUFFIX_BYTES: usize = 128;
/// [STU-IO-114]: the hard maximum of a `SCALE` value (Studio declares 100); the hard minimum is above 0.
pub const EXPORT_SCALE_MAX: f64 = 100.0;
/// [STU-IO-114]: the hard bounds of a `WIDTH` or `HEIGHT` value in pixels (Studio declares 100000).
pub const EXPORT_DIMENSION_RANGE: (f64, f64) = (1.0, 100_000.0);
/// Largest wire form of a record set in bytes, whatever the caller's budget allows.
pub const MAX_EXPORT_SETTINGS_INPUT_BYTES: usize = 4_194_304;

const CANCEL_STRIDE: usize = 256;
/// Characters a suffix may not hold: they name a directory or are reserved in a file name.
const RESERVED: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// [STU-IO-114] constraint type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ExportConstraintType {
    #[serde(rename = "SCALE")]
    Scale,
    #[serde(rename = "WIDTH")]
    Width,
    #[serde(rename = "HEIGHT")]
    Height,
}

/// [STU-IO-114] export constraint record: `SCALE` is a ratio above 0 and at most
/// [`EXPORT_SCALE_MAX`]; `WIDTH` and `HEIGHT` are whole pixels in [`EXPORT_DIMENSION_RANGE`]. Default
/// `SCALE` 1.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioExportConstraint {
    #[serde(rename = "type")]
    pub kind: ExportConstraintType,
    pub value: f64,
}
impl Default for StudioExportConstraint {
    fn default() -> Self {
        Self { kind: ExportConstraintType::Scale, value: 1.0 }
    }
}
impl StudioExportConstraint {
    /// The output size of a `width` by `height` pixel source under this constraint (each side at least
    /// 1; `WIDTH` and `HEIGHT` keep the aspect ratio). `None` when the constraint is out of bounds or a
    /// side does not fit a `u32`.
    pub fn output_size(&self, width: u32, height: u32) -> Option<(u32, u32)> {
        check_constraint(self).ok()?;
        if width == 0 || height == 0 {
            return None;
        }
        let (w, h) = (f64::from(width), f64::from(height));
        let (ow, oh) = match self.kind {
            ExportConstraintType::Scale => (w * self.value, h * self.value),
            ExportConstraintType::Width => (self.value, h * self.value / w),
            ExportConstraintType::Height => (w * self.value / h, self.value),
        };
        let side = |v: f64| {
            let r = v.round().max(1.0);
            // Used only when finite, at least 1 and within the u32 range.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let n = r as u32;
            (r.is_finite() && r <= f64::from(u32::MAX)).then_some(n)
        };
        Some((side(ow)?, side(oh)?))
    }
}

/// [STU-IO-112] `colour_profile`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ExportColourProfile {
    #[default]
    #[serde(rename = "DOCUMENT")]
    Document,
    #[serde(rename = "SRGB")]
    Srgb,
    #[serde(rename = "DISPLAY_P3_V4")]
    DisplayP3V4,
}

const fn yes() -> bool {
    true
}

/// [STU-IO-112] raster image settings (`format` `PNG` or `JPG` on the tagging enum).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioRasterImageSettings {
    /// Excludes overlapping content outside the node (default true).
    #[serde(default = "yes")]
    pub contents_only: bool,
    /// Exports the node's full bounds including clipped content (default false).
    #[serde(default)]
    pub use_absolute_bounds: bool,
    #[serde(default)]
    pub suffix: String,
    #[serde(default)]
    pub constraint: StudioExportConstraint,
    #[serde(default)]
    pub colour_profile: ExportColourProfile,
}

/// [STU-IO-113] SVG settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioSvgSettings {
    #[serde(default = "yes")]
    pub contents_only: bool,
    #[serde(default)]
    pub use_absolute_bounds: bool,
    #[serde(default)]
    pub suffix: String,
    #[serde(default)]
    pub colour_profile: ExportColourProfile,
    /// Converts text to paths.
    #[serde(default)]
    pub outline_text: bool,
    /// Emits node ids as `id` attributes.
    #[serde(default)]
    pub include_id_attribute: bool,
    /// Reduces stroke geometry.
    #[serde(default)]
    pub simplify_stroke: bool,
}

/// [STU-IO-113] node-render PDF settings.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioNodePdfSettings {
    #[serde(default = "yes")]
    pub contents_only: bool,
    #[serde(default)]
    pub use_absolute_bounds: bool,
    #[serde(default)]
    pub suffix: String,
    #[serde(default)]
    pub colour_profile: ExportColourProfile,
}

/// [STU-IO-111] one node-render setting: exactly one closed shape, tagged by `format`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "format")]
pub enum StudioNodeRenderSetting {
    #[serde(rename = "PNG")]
    Png(StudioRasterImageSettings),
    #[serde(rename = "JPG")]
    Jpg(StudioRasterImageSettings),
    #[serde(rename = "SVG")]
    Svg(StudioSvgSettings),
    #[serde(rename = "PDF")]
    Pdf(StudioNodePdfSettings),
}
impl StudioNodeRenderSetting {
    /// The `format` token.
    pub const fn format(&self) -> &'static str {
        match self {
            Self::Png(_) => "PNG",
            Self::Jpg(_) => "JPG",
            Self::Svg(_) => "SVG",
            Self::Pdf(_) => "PDF",
        }
    }
    /// The file name extension the setting writes.
    pub const fn extension(&self) -> &'static str {
        match self {
            Self::Png(_) => "png",
            Self::Jpg(_) => "jpg",
            Self::Svg(_) => "svg",
            Self::Pdf(_) => "pdf",
        }
    }
    pub fn suffix(&self) -> &str {
        match self {
            Self::Png(r) | Self::Jpg(r) => &r.suffix,
            Self::Svg(s) => &s.suffix,
            Self::Pdf(p) => &p.suffix,
        }
    }
    pub const fn contents_only(&self) -> bool {
        match self {
            Self::Png(r) | Self::Jpg(r) => r.contents_only,
            Self::Svg(s) => s.contents_only,
            Self::Pdf(p) => p.contents_only,
        }
    }
    pub const fn colour_profile(&self) -> ExportColourProfile {
        match self {
            Self::Png(r) | Self::Jpg(r) => r.colour_profile,
            Self::Svg(s) => s.colour_profile,
            Self::Pdf(p) => p.colour_profile,
        }
    }
    /// The raster constraint; vector and PDF settings have none.
    pub const fn constraint(&self) -> Option<StudioExportConstraint> {
        match self {
            Self::Png(r) | Self::Jpg(r) => Some(r.constraint),
            Self::Svg(_) | Self::Pdf(_) => None,
        }
    }
}

/// The settings of one node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioNodeExportSettings {
    /// A layer (`SLYR`) or an artboard (`SART`).
    pub node: NodeRef,
    /// In order; at least one.
    pub settings: Vec<StudioNodeRenderSetting>,
}

/// The per-document node export settings, one per `document_id`, beside the document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StudioExportSettings {
    pub schema_id: ExportSettingsSchema,
    /// `SDOC` id of the document.
    pub document_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<StudioNodeExportSettings>,
}
impl StudioExportSettings {
    /// An empty set for `document_id`.
    pub fn new(document_id: impl Into<String>) -> Self {
        Self { schema_id: ExportSettingsSchema::V1, document_id: document_id.into(), nodes: Vec::new() }
    }
    /// The settings of `node`.
    pub fn node(&self, node: &NodeRef) -> Option<&StudioNodeExportSettings> {
        self.nodes.iter().find(|n| &n.node == node)
    }
    /// This set with `node`'s settings replaced (kept in place), added at the end, or removed when
    /// `settings` is empty. Not validated: [`validate_export_settings`] judges the result.
    #[must_use]
    pub fn with_node(&self, node: &NodeRef, settings: Vec<StudioNodeRenderSetting>) -> Self {
        let mut next = self.clone();
        match (next.nodes.iter().position(|n| &n.node == node), settings.is_empty()) {
            (Some(at), true) => {
                next.nodes.remove(at);
            }
            (Some(at), false) => {
                if let Some(entry) = next.nodes.get_mut(at) {
                    entry.settings = settings;
                }
            }
            (None, true) => {}
            (None, false) => next.nodes.push(StudioNodeExportSettings { node: node.clone(), settings }),
        }
        next
    }
    /// Settings over all nodes.
    pub fn setting_count(&self) -> usize {
        self.nodes.iter().fold(0usize, |n, e| n.saturating_add(e.settings.len()))
    }
}

/// One constraint against [STU-IO-114]: `InvalidField` at `export_settings/constraint`.
pub fn check_constraint(c: &StudioExportConstraint) -> Result<()> {
    let v = c.value;
    let ok = v.is_finite()
        && match c.kind {
            ExportConstraintType::Scale => v > 0.0 && v <= EXPORT_SCALE_MAX,
            ExportConstraintType::Width | ExportConstraintType::Height => v.fract() == 0.0 && (EXPORT_DIMENSION_RANGE.0..=EXPORT_DIMENSION_RANGE.1).contains(&v),
        };
    if ok { Ok(()) } else { Err(err(Code::InvalidField, "export_settings/constraint")) }
}

/// One suffix: at most [`MAX_EXPORT_SUFFIX_BYTES`], no control or reserved character (`InvalidField`
/// at `export_settings/suffix`).
pub fn check_suffix(suffix: &str) -> Result<()> {
    if suffix.len() > MAX_EXPORT_SUFFIX_BYTES || suffix.chars().any(|c| c.is_control() || RESERVED.contains(&c)) {
        return Err(err(Code::InvalidField, "export_settings/suffix"));
    }
    Ok(())
}

/// The settings of one node: at least one and at most [`MAX_SETTINGS_PER_NODE`] (`InvalidField`,
/// `BudgetExceeded` at `export_settings/settings`), each suffix and constraint in bounds, and no two
/// with one suffix and extension ([STU-IO-117]: `DuplicateIdentity` at `export_settings/suffix`).
pub fn check_node_settings(settings: &[StudioNodeRenderSetting]) -> Result<()> {
    if settings.is_empty() {
        return Err(err(Code::InvalidField, "export_settings/settings"));
    }
    if settings.len() > MAX_SETTINGS_PER_NODE {
        return Err(err(Code::BudgetExceeded, "export_settings/settings"));
    }
    let mut names = BTreeSet::new();
    for s in settings {
        check_suffix(s.suffix())?;
        if let Some(c) = s.constraint() {
            check_constraint(&c)?;
        }
        if !names.insert((s.suffix(), s.extension())) {
            return Err(err(Code::DuplicateIdentity, "export_settings/suffix"));
        }
    }
    Ok(())
}

/// Validates the record set against the document: `SDOC` id equal to the document's; at most
/// [`MAX_EXPORT_NODES`] nodes (counted before they are walked, then charged with their settings against
/// `Budget::nodes`); each node a layer (`SLYR`) or an artboard (`SART`) of the id grammar (`InvalidId`;
/// a page spread is `InvalidField`), once (`DuplicateIdentity`), its settings by
/// [`check_node_settings`]; the suffixes charged to `Budget::payload_bytes`. A node the document does
/// not hold is inert, not refused (module notes).
pub fn validate_export_settings(r: &StudioExportSettings, d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<()> {
    let mut cx = Ctx::new(b, token, &Unresolved);
    cx.tick()?;
    id(&r.document_id, "SDOC", "document_id")?;
    if r.document_id != d.document_id {
        return Err(err(Code::InvalidField, "document_id"));
    }
    if r.nodes.len() > MAX_EXPORT_NODES {
        return Err(err(Code::BudgetExceeded, "export_settings"));
    }
    cx.nodes(r.nodes.len().saturating_add(r.setting_count()), "export_settings")?;
    let mut seen = BTreeSet::new();
    let mut payload = 0usize;
    for (i, n) in r.nodes.iter().enumerate() {
        if i.is_multiple_of(CANCEL_STRIDE) {
            cx.tick()?;
        }
        let prefix = match n.node.node_kind {
            NodeKind::Layer => "SLYR",
            NodeKind::Artboard => "SART",
            NodeKind::PageSpread => return Err(err(Code::InvalidField, "export_settings/node_kind")),
        };
        id(&n.node.node_id, prefix, "export_settings/node")?;
        if !seen.insert(n.node.node_id.as_str()) {
            return Err(err(Code::DuplicateIdentity, "export_settings/node"));
        }
        check_node_settings(&n.settings)?;
        for s in &n.settings {
            payload = payload.checked_add(s.suffix().len()).filter(|p| *p <= b.payload_bytes).ok_or_else(|| err(Code::BudgetExceeded, "export_settings/suffix"))?;
        }
    }
    Ok(())
}

/// JSON schema (draft 2020-12) of the node export settings.
pub fn export_settings_schema() -> schemars::Schema {
    schemars::generate::SchemaSettings::draft2020_12().into_generator().into_root_schema_for::<StudioExportSettings>()
}

/// Parses wire JSON (bounded by [`MAX_EXPORT_SETTINGS_INPUT_BYTES`], `Budget::input_bytes` and depth
/// before any parse; duplicate keys `DuplicateField`, unknown fields or formats, a missing field and
/// wrong types `InvalidJson`) and validates it against the document.
pub fn parse_export_settings(input: &[u8], d: &StudioDocument, b: Budget, token: &CancellationToken) -> Result<StudioExportSettings> {
    if input.len() > MAX_EXPORT_SETTINGS_INPUT_BYTES {
        return Err(err(Code::BudgetExceeded, "export_settings"));
    }
    let r: StudioExportSettings = parse_wire(input, b, token, "export_settings")?;
    validate_export_settings(&r, d, b, token)?;
    Ok(r)
}
