//! Hosted panels of the desktop shell (lane SMB-HARNESS-PANELS-1): every viewer surface that had a
//! Viewer API but no widget in the window is hosted as a floating panel behind WORKSPACE > Panels
//! (spec 14.30 [STU-SHL-036]), through the same [`super::shell::Panel`] mechanism as the layout
//! editor and the monitors: Appearance, Stroke, Swatches, Graphic Styles, Align & Distribute,
//! Transform, Path Operations, Masks, Envelope Distort, Brushes, Vector Export, Vector Import and
//! Audio Batch (feature `audio-batch`).
//!
//! Hosting only: each panel's controls build a [`PanelAction`] and [`Viewer::hosted_action`] calls
//! the existing Viewer method (fill and stroke styles, swatches, graphic styles, align / distribute
//! / transform, path edits, offset path, outline stroke, clipping and vector masks, envelopes,
//! brushes, vector SVG / PDF export, vector-artwork import, the audio batch queue); no engine
//! behaviour is added here. Single-layer actions act on the layer selected in the Layers panel;
//! multi-layer actions (align, distribute, transform, brush art) carry their layers, which the
//! panels take from their "Targets" checklist.
//!
//! Model navigation ([STU-CMP-110], [STU-SHL-009]): every panel window has the stable widget name
//! `panel.<key>` and its controls `panel.<key>.<control>` ([`widget`]); [`Viewer::hosted_action`]
//! runs any panel action headlessly (the exact call a button makes); `Viewer::state_json()["shell"]`
//! projects each hosted panel (`hosted.<key>`: open, visible, summary, last result, panel state) and
//! the key state of the shell-hosted surfaces (`surfaces`: layout editor, program monitor, source
//! monitor, prototype player). Panels are egui windows inside the one viewport: opening one never
//! moves the OS focus or takes keyboard focus ([STU-QUIET-005]).
//!
//! Naming ([STU-SHL-008]): panel labels are the spec's panel catalogue names where one exists
//! ([STU-SHL-113]: Appearance, Stroke, Swatches, Graphic Styles, Align & Distribute, Transform,
//! Masks, Brushes); the others describe the spec menu node they host (OBJECT > Path, OBJECT >
//! Procedural > Envelope Distort, FILE > Export / Import, AUTOMATE batch processing). Panel keys are
//! harness ids.
#![deny(clippy::arithmetic_side_effects)]

use super::Viewer;
use super::shell::{Availability, Panel};
use super::vector::arrange::{Align, Distribute, ReferencePoint, TransformEach};
use super::vector::brushes::PatternTileLayers;
use super::vector::{AlignTarget, EnvelopeSpec, LiveTextKind, PaintSpec, PathEdit, RowKind, StrokePanel, TransformOp};
use super::widget as vw;
use hsk_studio_folio::{
    self as folio, StudioAppearanceRow, StudioArtScale, StudioBrushColorization, StudioBrushKind, StudioBrushParam, StudioEnvelopeShape, StudioMaskMode,
    StudioPatternFit, StudioPatternTiles, StudioRotationRelative, StudioWarpAxis, StudioWarpStyle,
};
use hsk_studio_nib::Mirroring;
use hsk_studio_nib::anchor_path::{AnchorAddr, End, EndAddr};
use hsk_studio_nib::freeform::CornerTreatment;
use hsk_studio_nib::stroke::LineJoin;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Stable widget names of the hosted panels.
pub mod widget {
    /// The panel window: `panel.<key>`.
    pub fn window(key: &str) -> String {
        format!("panel.{key}")
    }
    /// A control of a panel: `panel.<key>.<control>`.
    pub fn control(key: &str, control: &str) -> String {
        format!("panel.{key}.{control}")
    }
    /// The summary line of a panel (its AccessKit label is the `summary` of the state projection).
    pub fn summary(key: &str) -> String {
        control(key, "summary")
    }
}

/// The panels this module hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostedPanel {
    Appearance,
    Stroke,
    Swatches,
    GraphicStyles,
    AlignDistribute,
    Transform,
    PathOperations,
    Masks,
    EnvelopeDistort,
    Brushes,
    VectorExport,
    VectorImport,
    AudioBatch,
}

impl HostedPanel {
    pub const ALL: [HostedPanel; 13] = [
        Self::Appearance,
        Self::Stroke,
        Self::Swatches,
        Self::GraphicStyles,
        Self::AlignDistribute,
        Self::Transform,
        Self::PathOperations,
        Self::Masks,
        Self::EnvelopeDistort,
        Self::Brushes,
        Self::VectorExport,
        Self::VectorImport,
        Self::AudioBatch,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Stroke => "stroke",
            Self::Swatches => "swatches",
            Self::GraphicStyles => "graphic_styles",
            Self::AlignDistribute => "align_distribute",
            Self::Transform => "transform",
            Self::PathOperations => "path_operations",
            Self::Masks => "masks",
            Self::EnvelopeDistort => "envelope_distort",
            Self::Brushes => "brushes",
            Self::VectorExport => "vector_export",
            Self::VectorImport => "vector_import",
            Self::AudioBatch => "audio_batch",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Stroke => "Stroke",
            Self::Swatches => "Swatches",
            Self::GraphicStyles => "Graphic Styles",
            Self::AlignDistribute => "Align & Distribute",
            Self::Transform => "Transform",
            Self::PathOperations => "Path Operations",
            Self::Masks => "Masks",
            Self::EnvelopeDistort => "Envelope Distort",
            Self::Brushes => "Brushes",
            Self::VectorExport => "Vector Export",
            Self::VectorImport => "Vector Import",
            Self::AudioBatch => "Audio Batch",
        }
    }
    /// The cargo feature the panel needs (beyond `viewer`), if any.
    pub fn feature(self) -> Option<&'static str> {
        match self {
            Self::AudioBatch => Some("audio-batch"),
            _ => None,
        }
    }
    /// Whether the panel acts on an open document (import and the audio batch do not).
    pub fn needs_document(self) -> bool {
        !matches!(self, Self::VectorImport | Self::AudioBatch)
    }
    /// Shortcut modifiers: Shift + F2..F12 for the first eleven, Ctrl+Shift + F2/F3 for the rest
    /// (F1 and the plain function keys stay with the shell; extra Shift never reaches the plain keys
    /// because the shell consumes these first).
    pub fn modifiers(self) -> egui::Modifiers {
        match self {
            Self::VectorImport | Self::AudioBatch => egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            _ => egui::Modifiers::SHIFT,
        }
    }
    pub fn key_code(self) -> egui::Key {
        match self {
            Self::Appearance => egui::Key::F2,
            Self::Stroke => egui::Key::F3,
            Self::Swatches => egui::Key::F4,
            Self::GraphicStyles => egui::Key::F5,
            Self::AlignDistribute => egui::Key::F6,
            Self::Transform => egui::Key::F7,
            Self::PathOperations => egui::Key::F8,
            Self::Masks => egui::Key::F9,
            Self::EnvelopeDistort => egui::Key::F10,
            Self::Brushes => egui::Key::F11,
            Self::VectorExport => egui::Key::F12,
            Self::VectorImport => egui::Key::F2,
            Self::AudioBatch => egui::Key::F3,
        }
    }
    pub fn shortcut_text(self) -> &'static str {
        match self {
            Self::Appearance => "Shift+F2",
            Self::Stroke => "Shift+F3",
            Self::Swatches => "Shift+F4",
            Self::GraphicStyles => "Shift+F5",
            Self::AlignDistribute => "Shift+F6",
            Self::Transform => "Shift+F7",
            Self::PathOperations => "Shift+F8",
            Self::Masks => "Shift+F9",
            Self::EnvelopeDistort => "Shift+F10",
            Self::Brushes => "Shift+F11",
            Self::VectorExport => "Shift+F12",
            Self::VectorImport => "Ctrl+Shift+F2",
            Self::AudioBatch => "Ctrl+Shift+F3",
        }
    }
}

/// Which brush kind a new brush is (default parameters of each kind; art from the targets, a
/// pattern's side tiles from the targets).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrushChoice {
    Art,
    Scatter,
    Pattern,
}

/// One audio batch item of the Audio Batch panel.
#[cfg(feature = "audio-batch")]
#[derive(Clone, Debug, PartialEq)]
pub struct BatchItem {
    pub input: PathBuf,
    pub output: PathBuf,
    /// Gain effect in dB (`None`: import and export only).
    pub gain_db: Option<f64>,
    pub format: super::audio_import::MixFormat,
    pub overwrite: bool,
}

/// What a hosted panel's control does; [`Viewer::hosted_action`] runs it (one existing Viewer
/// method each).
#[derive(Clone, Debug, PartialEq)]
pub enum PanelAction {
    /// Appearance: solid fill of the selected layer (`None`: no fill).
    SetFill(Option<[u8; 3]>),
    ClearAppearance,
    ReduceToBasicAppearance,
    /// Stroke: the stroke panel of the selected layer (`None`: no stroke).
    SetStroke(Option<StrokePanel>),
    AddSwatch { name: String, colour: [u8; 3] },
    /// Swatches: links a swatch to the selected layer's fill or stroke (optionally tinted, percent).
    ApplySwatch { swatch_id: String, kind: RowKind, tint: Option<f64> },
    DetachSwatch(RowKind),
    /// Graphic Styles: a new style from the selected layer's appearance.
    AddGraphicStyle { name: String },
    ApplyGraphicStyle(String),
    MergeGraphicStyle(String),
    RedefineGraphicStyle(String),
    BreakGraphicStyle,
    Align { layers: Vec<String>, to: AlignTarget, how: Align },
    Distribute { layers: Vec<String>, to: AlignTarget, how: Distribute },
    Transform { layers: Vec<String>, op: TransformOp, reference: ReferencePoint },
    TransformEach { layers: Vec<String>, each: TransformEach },
    /// Path Operations on the selected layer.
    OffsetPath { offset: f64, join: LineJoin },
    OutlineStroke,
    ExpandShape,
    PromoteNetwork,
    EditPath(PathEdit),
    /// Type on the selected vector layer's path (lane SMB-AI-AREA-TYPE-3): live path text, or area
    /// text inside its closed path ([`Viewer::bind_live_text`]).
    BindLiveText { text: String, kind: LiveTextKind },
    /// Point text <-> area text of the selected layer ([`Viewer::convert_text_kind`]).
    ConvertTextKind,
    /// Create Outlines of the selected text ([`Viewer::create_outlines`]; one undo step).
    CreateOutlines,
    /// Masks: the selected layer is clipped or masked by `source`.
    MakeClip { source: String, even_odd: bool },
    MakeMask { source: String, mode: StudioMaskMode, invert: bool },
    ReleaseMasks,
    MakeEnvelope { spec: EnvelopeSpec, fidelity: f64, distort_appearance: bool },
    ReleaseEnvelope,
    ExpandEnvelope,
    AddBrush { name: String, kind: BrushChoice, colorization: StudioBrushColorization, layers: Vec<String> },
    /// Brushes: strokes the selected layer with `brush_id` (scale override percent).
    ApplyBrush { brush_id: String, scale: Option<f64> },
    RemoveBrush,
    ExportSvg { path: PathBuf, overwrite: bool },
    ExportPdf { path: PathBuf, overwrite: bool },
    /// Vector Import: converts vector artwork (`.ai`) into a native package at `dest` and opens it.
    ImportVector { source: PathBuf, dest: PathBuf, overwrite: bool },
    #[cfg(feature = "audio-batch")]
    BatchAdd(BatchItem),
    #[cfg(feature = "audio-batch")]
    BatchRun,
    #[cfg(feature = "audio-batch")]
    BatchClear,
}

impl PanelAction {
    /// The panel the action belongs to.
    pub fn panel(&self) -> HostedPanel {
        use HostedPanel as H;
        match self {
            Self::SetFill(_) | Self::ClearAppearance | Self::ReduceToBasicAppearance => H::Appearance,
            Self::SetStroke(_) => H::Stroke,
            Self::AddSwatch { .. } | Self::ApplySwatch { .. } | Self::DetachSwatch(_) => H::Swatches,
            Self::AddGraphicStyle { .. } | Self::ApplyGraphicStyle(_) | Self::MergeGraphicStyle(_) | Self::RedefineGraphicStyle(_) | Self::BreakGraphicStyle => H::GraphicStyles,
            Self::Align { .. } | Self::Distribute { .. } => H::AlignDistribute,
            Self::Transform { .. } | Self::TransformEach { .. } => H::Transform,
            Self::OffsetPath { .. } | Self::OutlineStroke | Self::ExpandShape | Self::PromoteNetwork | Self::EditPath(_) | Self::BindLiveText { .. } | Self::ConvertTextKind | Self::CreateOutlines => H::PathOperations,
            Self::MakeClip { .. } | Self::MakeMask { .. } | Self::ReleaseMasks => H::Masks,
            Self::MakeEnvelope { .. } | Self::ReleaseEnvelope | Self::ExpandEnvelope => H::EnvelopeDistort,
            Self::AddBrush { .. } | Self::ApplyBrush { .. } | Self::RemoveBrush => H::Brushes,
            Self::ExportSvg { .. } | Self::ExportPdf { .. } => H::VectorExport,
            Self::ImportVector { .. } => H::VectorImport,
            #[cfg(feature = "audio-batch")]
            Self::BatchAdd(_) | Self::BatchRun | Self::BatchClear => H::AudioBatch,
        }
    }
    fn needs_selection(&self) -> bool {
        matches!(
            self,
            Self::SetFill(_)
                | Self::ClearAppearance
                | Self::ReduceToBasicAppearance
                | Self::SetStroke(_)
                | Self::ApplySwatch { .. }
                | Self::DetachSwatch(_)
                | Self::AddGraphicStyle { .. }
                | Self::ApplyGraphicStyle(_)
                | Self::MergeGraphicStyle(_)
                | Self::RedefineGraphicStyle(_)
                | Self::BreakGraphicStyle
                | Self::OffsetPath { .. }
                | Self::OutlineStroke
                | Self::ExpandShape
                | Self::PromoteNetwork
                | Self::EditPath(_)
                | Self::BindLiveText { .. }
                | Self::ConvertTextKind
                | Self::CreateOutlines
                | Self::MakeClip { .. }
                | Self::MakeMask { .. }
                | Self::ReleaseMasks
                | Self::MakeEnvelope { .. }
                | Self::ReleaseEnvelope
                | Self::ExpandEnvelope
                | Self::ApplyBrush { .. }
                | Self::RemoveBrush
        )
    }
}

const REFERENCE_POINTS: [(ReferencePoint, &str); 9] = [
    (ReferencePoint::TopLeft, "top left"),
    (ReferencePoint::Top, "top"),
    (ReferencePoint::TopRight, "top right"),
    (ReferencePoint::Left, "left"),
    (ReferencePoint::Centre, "centre"),
    (ReferencePoint::Right, "right"),
    (ReferencePoint::BottomLeft, "bottom left"),
    (ReferencePoint::Bottom, "bottom"),
    (ReferencePoint::BottomRight, "bottom right"),
];
const ALIGNS: [(Align, &str); 6] = [
    (Align::Left, "Left"),
    (Align::CentreHorizontal, "Centre H"),
    (Align::Right, "Right"),
    (Align::Top, "Top"),
    (Align::CentreVertical, "Centre V"),
    (Align::Bottom, "Bottom"),
];
const WARP_STYLES: [(StudioWarpStyle, &str); 15] = [
    (StudioWarpStyle::Arc, "arc"),
    (StudioWarpStyle::ArcLower, "arc lower"),
    (StudioWarpStyle::ArcUpper, "arc upper"),
    (StudioWarpStyle::Arch, "arch"),
    (StudioWarpStyle::Bulge, "bulge"),
    (StudioWarpStyle::ShellLower, "shell lower"),
    (StudioWarpStyle::ShellUpper, "shell upper"),
    (StudioWarpStyle::Flag, "flag"),
    (StudioWarpStyle::Wave, "wave"),
    (StudioWarpStyle::Fish, "fish"),
    (StudioWarpStyle::Rise, "rise"),
    (StudioWarpStyle::Fisheye, "fisheye"),
    (StudioWarpStyle::Inflate, "inflate"),
    (StudioWarpStyle::Squeeze, "squeeze"),
    (StudioWarpStyle::Twist, "twist"),
];

/// Form state of the hosted panels (what their fields hold between frames).
pub struct HostedState {
    open: Vec<HostedPanel>,
    last: BTreeMap<HostedPanel, String>,
    /// Layers the multi-layer panels act on (align, distribute, transform, brush art).
    targets: Vec<String>,
    fill: [u8; 3],
    stroke_colour: [u8; 3],
    stroke_weight: f64,
    stroke_cap: folio::StrokeCap,
    stroke_join: folio::StrokeJoin,
    stroke_miter: f64,
    stroke_dash: String,
    stroke_dash_offset: f64,
    stroke_align: folio::StrokeAlign,
    swatch_name: String,
    swatch_colour: [u8; 3],
    swatch_tint: f64,
    swatch_tinted: bool,
    style_name: String,
    align_to: usize,
    distribute_gap: String,
    reference: usize,
    move_by: [f64; 2],
    rotate: f64,
    scale: [f64; 2],
    reflect_axis: f64,
    shear: [f64; 2],
    each: TransformEach,
    offset: f64,
    offset_join: LineJoin,
    /// Type section of Path Operations (lane SMB-AI-AREA-TYPE-3): the live text and area inset.
    live_text: String,
    live_inset: f64,
    anchor: [usize; 2],
    corner: CornerTreatment,
    corner_radius: f64,
    mirroring: Mirroring,
    join_contours: [usize; 2],
    mask_source: Option<String>,
    clip_even_odd: bool,
    mask_mode: StudioMaskMode,
    mask_invert: bool,
    envelope_kind: usize,
    warp_style: usize,
    warp_vertical: bool,
    warp: [f64; 3],
    mesh: [u32; 2],
    envelope_top: Option<String>,
    fidelity: f64,
    distort_appearance: bool,
    brush_name: String,
    brush_kind: BrushChoice,
    brush_colorization: StudioBrushColorization,
    brush_scale: String,
    export_path: String,
    export_overwrite: bool,
    import_source: String,
    import_dest: String,
    import_overwrite: bool,
    #[cfg(feature = "audio-batch")]
    batch: BatchForm,
}

#[cfg(feature = "audio-batch")]
#[derive(Default)]
struct BatchForm {
    input: String,
    output: String,
    gain: bool,
    gain_db: f64,
    aiff: bool,
    overwrite: bool,
    pending: Vec<BatchItem>,
    receipts: Vec<Value>,
}

impl Default for HostedState {
    fn default() -> Self {
        Self {
            open: Vec::new(),
            last: BTreeMap::new(),
            targets: Vec::new(),
            fill: [255, 0, 0],
            stroke_colour: [0, 0, 0],
            stroke_weight: 1.0,
            stroke_cap: folio::StrokeCap::Butt,
            stroke_join: folio::StrokeJoin::Miter,
            stroke_miter: 4.0,
            stroke_dash: String::new(),
            stroke_dash_offset: 0.0,
            stroke_align: folio::StrokeAlign::Center,
            swatch_name: "Swatch".into(),
            swatch_colour: [0, 0, 255],
            swatch_tint: 50.0,
            swatch_tinted: false,
            style_name: "Style".into(),
            align_to: 0,
            distribute_gap: String::new(),
            reference: 4,
            move_by: [0.0, 0.0],
            rotate: 0.0,
            scale: [100.0, 100.0],
            reflect_axis: 90.0,
            shear: [0.0, 0.0],
            each: TransformEach::default(),
            offset: 2.0,
            offset_join: LineJoin::Miter,
            live_text: String::new(),
            live_inset: 0.0,
            anchor: [0, 0],
            corner: CornerTreatment::Round,
            corner_radius: 4.0,
            mirroring: Mirroring::None,
            join_contours: [0, 0],
            mask_source: None,
            clip_even_odd: false,
            mask_mode: StudioMaskMode::Alpha,
            mask_invert: false,
            envelope_kind: 0,
            warp_style: 0,
            warp_vertical: false,
            warp: [50.0, 0.0, 0.0],
            mesh: [2, 2],
            envelope_top: None,
            fidelity: 50.0,
            distort_appearance: false,
            brush_name: "Brush".into(),
            brush_kind: BrushChoice::Art,
            brush_colorization: StudioBrushColorization::None,
            brush_scale: String::new(),
            export_path: String::new(),
            export_overwrite: false,
            import_source: String::new(),
            import_dest: String::new(),
            import_overwrite: false,
            #[cfg(feature = "audio-batch")]
            batch: BatchForm::default(),
        }
    }
}

impl HostedState {
    pub(super) fn is_open(&self, panel: HostedPanel) -> bool {
        self.open.contains(&panel)
    }
    pub(super) fn open(&mut self, panel: HostedPanel) {
        if !self.open.contains(&panel) {
            self.open.push(panel);
        }
    }
    /// FILE > Close: layer references of the closed document go; the panels stay open.
    pub(super) fn close_document(&mut self) {
        self.targets.clear();
        self.mask_source = None;
        self.envelope_top = None;
    }
}

/// What the panels show this frame (read before drawing; the panels never borrow the viewer).
struct PanelView {
    document: bool,
    selected: Option<(String, String)>,
    /// Vector layers (id, name), top first.
    vectors: Vec<(String, String)>,
    /// Every layer (id, name), top first.
    layers: Vec<(String, String)>,
    swatches: Vec<(String, String)>,
    styles: Vec<(String, String)>,
    brushes: Vec<(String, String)>,
    summaries: BTreeMap<HostedPanel, String>,
}

fn name_of(rows: &[(String, String)], id: &str) -> String {
    rows.iter().find(|(i, _)| i == id).map_or_else(|| id.to_owned(), |(_, n)| n.clone())
}

/// A button with a stable widget name; true when clicked.
fn button(ui: &mut egui::Ui, drawn: &mut Vec<String>, name: String, label: &str, enabled: bool) -> bool {
    let clicked = super::wrap::wrap_item(ui, vw::id(&name), label, egui::TextStyle::Button, |ui| ui.add_enabled(enabled, egui::Button::new(label))).clicked();
    drawn.push(name);
    clicked
}

fn number(ui: &mut egui::Ui, drawn: &mut Vec<String>, name: String, label: &str, v: &mut f64, speed: f64) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.push_id(vw::id(&name), |ui| ui.add(egui::DragValue::new(v).speed(speed)));
    });
    drawn.push(name);
}

fn text(ui: &mut egui::Ui, drawn: &mut Vec<String>, name: String, label: &str, v: &mut String, hint: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.add(egui::TextEdit::singleline(v).id(vw::id(&name)).hint_text(hint).desired_width(180.0));
    });
    drawn.push(name);
}

fn choice<T: PartialEq + Copy>(ui: &mut egui::Ui, drawn: &mut Vec<String>, name: String, label: &str, v: &mut T, options: &[(T, &str)]) {
    let current = options.iter().find(|(o, _)| o == v).map_or("?", |(_, l)| l);
    egui::ComboBox::new(vw::id(&name), label).selected_text(current).show_ui(ui, |ui| {
        for (o, l) in options {
            ui.selectable_value(v, *o, *l);
        }
    });
    drawn.push(name);
}

/// A layer picker (combo box over `rows`).
fn layer_choice(ui: &mut egui::Ui, drawn: &mut Vec<String>, name: String, label: &str, v: &mut Option<String>, rows: &[(String, String)]) {
    let current = v.as_deref().map_or_else(|| "(none)".to_owned(), |id| name_of(rows, id));
    egui::ComboBox::new(vw::id(&name), label).selected_text(current).show_ui(ui, |ui| {
        for (id, n) in rows {
            ui.selectable_value(v, Some(id.clone()), n.as_str());
        }
    });
    drawn.push(name);
}

/// The Targets checklist of the multi-layer panels.
fn targets_ui(ui: &mut egui::Ui, drawn: &mut Vec<String>, key: &str, targets: &mut Vec<String>, vectors: &[(String, String)]) {
    targets.retain(|t| vectors.iter().any(|(id, _)| id == t));
    ui.label(format!("Targets ({} of {} vector layers)", targets.len(), vectors.len()));
    ui.horizontal(|ui| {
        if button(ui, drawn, widget::control(key, "targets.all"), "All", !vectors.is_empty()) {
            *targets = vectors.iter().map(|(id, _)| id.clone()).collect();
        }
        if button(ui, drawn, widget::control(key, "targets.none"), "None", !targets.is_empty()) {
            targets.clear();
        }
    });
    for (id, n) in vectors {
        let mut on = targets.contains(id);
        let name = widget::control(key, &format!("target.{id}"));
        if ui.push_id(vw::id(&name), |ui| ui.checkbox(&mut on, n.as_str())).inner.changed() {
            if on {
                targets.push(id.clone());
            } else {
                targets.retain(|t| t != id);
            }
        }
        drawn.push(name);
    }
}

fn parse_dash(text: &str) -> Result<Vec<f64>, String> {
    text.split([' ', ',']).filter(|t| !t.is_empty()).map(|t| t.parse::<f64>().map_err(|_| format!("dash: '{t}' is not a number"))).collect()
}

fn brush_kind(choice: BrushChoice) -> StudioBrushKind {
    let p = StudioBrushParam::fixed;
    match choice {
        BrushChoice::Art => StudioBrushKind::Art { art_width: p(100.0), art_scale_mode: StudioArtScale::StretchToStrokeLength, art_guide_start: None, art_guide_end: None, flip_along: false, flip_across: false, art: Vec::new() },
        BrushChoice::Scatter => StudioBrushKind::Scatter {
            scatter_size: p(100.0),
            scatter_spacing: p(100.0),
            scatter_offset: p(0.0),
            scatter_rotation: p(0.0),
            scatter_rotation_relative_to: StudioRotationRelative::Path,
            seed: 1,
            art: Vec::new(),
        },
        BrushChoice::Pattern => StudioBrushKind::Pattern { pattern_brush_scale: p(100.0), fit_mode: StudioPatternFit::StretchToFit, flip_along: false, flip_across: false, show_auto_generated_corner_tiles: false, tiles: StudioPatternTiles::default() },
    }
}

impl HostedState {
    /// Draws the body of `panel`; clicked controls push their action.
    fn panel_ui(&mut self, panel: HostedPanel, ui: &mut egui::Ui, view: &PanelView, actions: &mut Vec<PanelAction>, drawn: &mut Vec<String>) {
        let key = panel.key();
        let c = |control: &str| widget::control(key, control);
        let summary = view.summaries.get(&panel).cloned().unwrap_or_default();
        ui.push_id(vw::id(&widget::summary(key)), |ui| ui.label(summary));
        drawn.push(widget::summary(key));
        if panel.needs_document() && !view.document {
            ui.label("Open a document first.");
            return;
        }
        let has = view.selected.is_some();
        let selected_line = |ui: &mut egui::Ui| {
            let line = view.selected.as_ref().map_or_else(|| "Select a layer in the Layers panel.".to_owned(), |(_, n)| format!("Selected layer: {n}"));
            ui.label(line);
        };
        match panel {
            HostedPanel::Appearance => {
                selected_line(ui);
                ui.horizontal(|ui| {
                    ui.label("Fill");
                    ui.push_id(vw::id(&c("fill.colour")), |ui| ui.color_edit_button_srgb(&mut self.fill));
                });
                drawn.push(c("fill.colour"));
                ui.horizontal(|ui| {
                    if button(ui, drawn, c("fill.set"), "Set Fill", has) {
                        actions.push(PanelAction::SetFill(Some(self.fill)));
                    }
                    if button(ui, drawn, c("fill.none"), "No Fill", has) {
                        actions.push(PanelAction::SetFill(None));
                    }
                });
                ui.horizontal(|ui| {
                    if button(ui, drawn, c("clear"), "Clear Appearance", has) {
                        actions.push(PanelAction::ClearAppearance);
                    }
                    if button(ui, drawn, c("reduce"), "Reduce to Basic", has) {
                        actions.push(PanelAction::ReduceToBasicAppearance);
                    }
                });
            }
            HostedPanel::Stroke => {
                selected_line(ui);
                ui.horizontal(|ui| {
                    ui.label("Colour");
                    ui.push_id(vw::id(&c("colour")), |ui| ui.color_edit_button_srgb(&mut self.stroke_colour));
                });
                drawn.push(c("colour"));
                number(ui, drawn, c("weight"), "Weight", &mut self.stroke_weight, 0.1);
                choice(ui, drawn, c("cap"), "Cap", &mut self.stroke_cap, &[(folio::StrokeCap::Butt, "butt"), (folio::StrokeCap::Round, "round"), (folio::StrokeCap::Projecting, "projecting")]);
                choice(ui, drawn, c("join"), "Join", &mut self.stroke_join, &[(folio::StrokeJoin::Miter, "miter"), (folio::StrokeJoin::Round, "round"), (folio::StrokeJoin::Bevel, "bevel")]);
                number(ui, drawn, c("miter_limit"), "Miter limit", &mut self.stroke_miter, 0.1);
                text(ui, drawn, c("dash"), "Dash", &mut self.stroke_dash, "dash gap ... (empty: solid)");
                number(ui, drawn, c("dash_offset"), "Dash offset", &mut self.stroke_dash_offset, 0.1);
                choice(ui, drawn, c("align"), "Align", &mut self.stroke_align, &[(folio::StrokeAlign::Center, "centre"), (folio::StrokeAlign::Inside, "inside"), (folio::StrokeAlign::Outside, "outside")]);
                ui.horizontal(|ui| {
                    if button(ui, drawn, c("apply"), "Apply Stroke", has) {
                        match parse_dash(&self.stroke_dash) {
                            Ok(dash) => actions.push(PanelAction::SetStroke(Some(StrokePanel {
                                paint: PaintSpec::Solid(self.stroke_colour),
                                weight: self.stroke_weight,
                                cap: self.stroke_cap,
                                join: self.stroke_join,
                                miter_limit: self.stroke_miter,
                                dash,
                                dash_offset: self.stroke_dash_offset,
                                align: self.stroke_align,
                            }))),
                            Err(e) => {
                                self.last.insert(panel, e);
                            }
                        }
                    }
                    if button(ui, drawn, c("none"), "No Stroke", has) {
                        actions.push(PanelAction::SetStroke(None));
                    }
                });
            }
            HostedPanel::Swatches => {
                text(ui, drawn, c("name"), "Name", &mut self.swatch_name, "swatch name");
                ui.horizontal(|ui| {
                    ui.push_id(vw::id(&c("colour")), |ui| ui.color_edit_button_srgb(&mut self.swatch_colour));
                    if button(ui, drawn, c("add"), "New Swatch", true) {
                        actions.push(PanelAction::AddSwatch { name: self.swatch_name.clone(), colour: self.swatch_colour });
                    }
                });
                drawn.push(c("colour"));
                ui.horizontal(|ui| {
                    ui.push_id(vw::id(&c("tinted")), |ui| ui.checkbox(&mut self.swatch_tinted, "Tint %"));
                    ui.push_id(vw::id(&c("tint")), |ui| ui.add(egui::DragValue::new(&mut self.swatch_tint).range(0.0..=100.0)));
                });
                drawn.extend([c("tinted"), c("tint")]);
                selected_line(ui);
                let tint = self.swatch_tinted.then_some(self.swatch_tint);
                for (id, n) in &view.swatches {
                    ui.horizontal(|ui| {
                        ui.label(n.as_str());
                        if button(ui, drawn, c(&format!("{id}.fill")), "To Fill", has) {
                            actions.push(PanelAction::ApplySwatch { swatch_id: id.clone(), kind: RowKind::Fill, tint });
                        }
                        if button(ui, drawn, c(&format!("{id}.stroke")), "To Stroke", has) {
                            actions.push(PanelAction::ApplySwatch { swatch_id: id.clone(), kind: RowKind::Stroke, tint });
                        }
                    });
                }
                ui.horizontal(|ui| {
                    if button(ui, drawn, c("detach.fill"), "Detach Fill", has) {
                        actions.push(PanelAction::DetachSwatch(RowKind::Fill));
                    }
                    if button(ui, drawn, c("detach.stroke"), "Detach Stroke", has) {
                        actions.push(PanelAction::DetachSwatch(RowKind::Stroke));
                    }
                });
            }
            HostedPanel::GraphicStyles => {
                selected_line(ui);
                text(ui, drawn, c("name"), "Name", &mut self.style_name, "style name");
                ui.horizontal(|ui| {
                    if button(ui, drawn, c("add"), "New from Selection", has) {
                        actions.push(PanelAction::AddGraphicStyle { name: self.style_name.clone() });
                    }
                    if button(ui, drawn, c("break"), "Break Link", has) {
                        actions.push(PanelAction::BreakGraphicStyle);
                    }
                });
                for (id, n) in &view.styles {
                    ui.horizontal(|ui| {
                        ui.label(n.as_str());
                        if button(ui, drawn, c(&format!("{id}.apply")), "Apply", has) {
                            actions.push(PanelAction::ApplyGraphicStyle(id.clone()));
                        }
                        if button(ui, drawn, c(&format!("{id}.merge")), "Merge", has) {
                            actions.push(PanelAction::MergeGraphicStyle(id.clone()));
                        }
                        if button(ui, drawn, c(&format!("{id}.redefine")), "Redefine", has) {
                            actions.push(PanelAction::RedefineGraphicStyle(id.clone()));
                        }
                    });
                }
            }
            HostedPanel::AlignDistribute => {
                targets_ui(ui, drawn, key, &mut self.targets, &view.vectors);
                choice(ui, drawn, c("to"), "Align to", &mut self.align_to, &[(0, "selection"), (1, "key object (selected layer)"), (2, "artboard")]);
                let to = match self.align_to {
                    1 => view.selected.as_ref().map(|(id, _)| AlignTarget::KeyObject(id.clone())),
                    2 => Some(AlignTarget::Artboard),
                    _ => Some(AlignTarget::Selection),
                };
                let ready = to.is_some() && !self.targets.is_empty();
                ui.label("Align");
                ui.horizontal_wrapped(|ui| {
                    for (i, (how, l)) in ALIGNS.iter().enumerate() {
                        if button(ui, drawn, c(&format!("align.{i}")), l, ready)
                            && let Some(to) = to.clone()
                        {
                            actions.push(PanelAction::Align { layers: self.targets.clone(), to, how: *how });
                        }
                    }
                });
                ui.label("Distribute edges");
                ui.horizontal_wrapped(|ui| {
                    for (i, (how, l)) in ALIGNS.iter().enumerate() {
                        if button(ui, drawn, c(&format!("distribute.{i}")), l, ready)
                            && let Some(to) = to.clone()
                        {
                            actions.push(PanelAction::Distribute { layers: self.targets.clone(), to, how: Distribute::Edge(*how) });
                        }
                    }
                });
                text(ui, drawn, c("gap"), "Spacing", &mut self.distribute_gap, "gap (empty: equal)");
                ui.horizontal(|ui| {
                    for (axis, l) in [(0_usize, "Space H"), (1, "Space V")] {
                        if button(ui, drawn, c(&format!("spacing.{axis}")), l, ready)
                            && let Some(to) = to.clone()
                        {
                            let gap = self.distribute_gap.trim();
                            match (!gap.is_empty()).then(|| gap.parse::<f64>()).transpose() {
                                Ok(gap) => actions.push(PanelAction::Distribute { layers: self.targets.clone(), to, how: Distribute::Spacing { axis, gap } }),
                                Err(_) => {
                                    self.last.insert(panel, format!("spacing: '{gap}' is not a number"));
                                }
                            }
                        }
                    }
                });
            }
            HostedPanel::Transform => {
                targets_ui(ui, drawn, key, &mut self.targets, &view.vectors);
                let options: Vec<(usize, &str)> = REFERENCE_POINTS.iter().enumerate().map(|(i, (_, l))| (i, *l)).collect();
                choice(ui, drawn, c("reference"), "Reference", &mut self.reference, &options);
                let reference = REFERENCE_POINTS.get(self.reference).map_or(ReferencePoint::Centre, |(r, _)| *r);
                let ready = !self.targets.is_empty();
                let mut op = |ui: &mut egui::Ui, drawn: &mut Vec<String>, name: &str, label: &str, t: TransformOp| {
                    if button(ui, drawn, c(name), label, ready) {
                        actions.push(PanelAction::Transform { layers: self.targets.clone(), op: t, reference });
                    }
                };
                ui.horizontal(|ui| {
                    ui.push_id(vw::id(&c("move.x")), |ui| ui.add(egui::DragValue::new(&mut self.move_by[0]).prefix("dx ")));
                    ui.push_id(vw::id(&c("move.y")), |ui| ui.add(egui::DragValue::new(&mut self.move_by[1]).prefix("dy ")));
                    op(ui, drawn, "move", "Move", TransformOp::Move { dx: self.move_by[0], dy: self.move_by[1] });
                });
                ui.horizontal(|ui| {
                    ui.push_id(vw::id(&c("rotate.angle")), |ui| ui.add(egui::DragValue::new(&mut self.rotate).suffix(" deg")));
                    op(ui, drawn, "rotate", "Rotate", TransformOp::Rotate(self.rotate));
                });
                ui.horizontal(|ui| {
                    ui.push_id(vw::id(&c("scale.h")), |ui| ui.add(egui::DragValue::new(&mut self.scale[0]).suffix(" % h")));
                    ui.push_id(vw::id(&c("scale.v")), |ui| ui.add(egui::DragValue::new(&mut self.scale[1]).suffix(" % v")));
                    op(ui, drawn, "scale", "Scale", TransformOp::Scale { horizontal: self.scale[0], vertical: self.scale[1] });
                });
                ui.horizontal(|ui| {
                    ui.push_id(vw::id(&c("reflect.axis")), |ui| ui.add(egui::DragValue::new(&mut self.reflect_axis).suffix(" deg axis")));
                    op(ui, drawn, "reflect", "Reflect", TransformOp::Reflect { axis_deg: self.reflect_axis });
                });
                ui.horizontal(|ui| {
                    ui.push_id(vw::id(&c("shear.angle")), |ui| ui.add(egui::DragValue::new(&mut self.shear[0]).suffix(" deg")));
                    ui.push_id(vw::id(&c("shear.axis")), |ui| ui.add(egui::DragValue::new(&mut self.shear[1]).suffix(" deg axis")));
                    op(ui, drawn, "shear", "Shear", TransformOp::Shear { angle_deg: self.shear[0], axis_deg: self.shear[1] });
                });
                drawn.extend(["move.x", "move.y", "rotate.angle", "scale.h", "scale.v", "reflect.axis", "shear.angle", "shear.axis"].map(c));
                ui.separator();
                ui.label("Transform each (about each layer's reference point)");
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.each.scale_horizontal).suffix(" % h"));
                    ui.add(egui::DragValue::new(&mut self.each.scale_vertical).suffix(" % v"));
                    ui.add(egui::DragValue::new(&mut self.each.rotate).suffix(" deg"));
                });
                ui.horizontal(|ui| {
                    ui.add(egui::DragValue::new(&mut self.each.move_horizontal).prefix("dx "));
                    ui.add(egui::DragValue::new(&mut self.each.move_vertical).prefix("dy "));
                    ui.checkbox(&mut self.each.randomize, "random");
                });
                if button(ui, drawn, c("each"), "Transform Each", ready) {
                    actions.push(PanelAction::TransformEach { layers: self.targets.clone(), each: TransformEach { reference, ..self.each } });
                }
            }
            HostedPanel::PathOperations => {
                selected_line(ui);
                ui.horizontal(|ui| {
                    ui.push_id(vw::id(&c("offset.value")), |ui| ui.add(egui::DragValue::new(&mut self.offset).speed(0.1).prefix("offset ")));
                    choice(ui, drawn, c("offset.join"), "", &mut self.offset_join, &[(LineJoin::Miter, "miter"), (LineJoin::Round, "round"), (LineJoin::Bevel, "bevel")]);
                    if button(ui, drawn, c("offset"), "Offset Path", has) {
                        actions.push(PanelAction::OffsetPath { offset: self.offset, join: self.offset_join });
                    }
                });
                drawn.push(c("offset.value"));
                ui.horizontal_wrapped(|ui| {
                    if button(ui, drawn, c("outline_stroke"), "Outline Stroke", has) {
                        actions.push(PanelAction::OutlineStroke);
                    }
                    if button(ui, drawn, c("expand_shape"), "Expand Shape", has) {
                        actions.push(PanelAction::ExpandShape);
                    }
                    if button(ui, drawn, c("reverse"), "Reverse Path Direction", has) {
                        actions.push(PanelAction::EditPath(PathEdit::Reverse(None)));
                    }
                    if button(ui, drawn, c("promote"), "Promote to Network", has) {
                        actions.push(PanelAction::PromoteNetwork);
                    }
                });
                ui.separator();
                ui.label("Anchor (contour, index)");
                ui.horizontal(|ui| {
                    ui.push_id(vw::id(&c("anchor.contour")), |ui| ui.add(egui::DragValue::new(&mut self.anchor[0]).range(0..=4096)));
                    ui.push_id(vw::id(&c("anchor.index")), |ui| ui.add(egui::DragValue::new(&mut self.anchor[1]).range(0..=65536)));
                });
                drawn.extend([c("anchor.contour"), c("anchor.index")]);
                let at = AnchorAddr { contour: self.anchor[0], anchor: self.anchor[1] };
                ui.horizontal(|ui| {
                    if button(ui, drawn, c("split"), "Split at Anchor", has) {
                        actions.push(PanelAction::EditPath(PathEdit::SplitAtAnchor(at)));
                    }
                    if button(ui, drawn, c("delete_anchor"), "Remove Anchor", has) {
                        actions.push(PanelAction::EditPath(PathEdit::DeleteAnchor(at)));
                    }
                });
                ui.horizontal(|ui| {
                    choice(ui, drawn, c("mirroring"), "", &mut self.mirroring, &[(Mirroring::None, "corner"), (Mirroring::Angle, "smooth angle"), (Mirroring::AngleAndLength, "smooth symmetric")]);
                    if button(ui, drawn, c("convert"), "Convert Anchor", has) {
                        actions.push(PanelAction::EditPath(PathEdit::Convert(at, self.mirroring)));
                    }
                });
                ui.horizontal(|ui| {
                    choice(ui, drawn, c("corner.kind"), "", &mut self.corner, &[(CornerTreatment::Round, "round"), (CornerTreatment::InvertedRound, "inverted round"), (CornerTreatment::Chamfer, "chamfer")]);
                    ui.push_id(vw::id(&c("corner.radius")), |ui| ui.add(egui::DragValue::new(&mut self.corner_radius).speed(0.1).prefix("r ")));
                    if button(ui, drawn, c("corner"), "Live Corner", has) {
                        actions.push(PanelAction::EditPath(PathEdit::Corner { at, kind: self.corner, radius: self.corner_radius }));
                    }
                });
                drawn.push(c("corner.radius"));
                ui.horizontal(|ui| {
                    ui.label("Join end of contour");
                    ui.push_id(vw::id(&c("join.a")), |ui| ui.add(egui::DragValue::new(&mut self.join_contours[0]).range(0..=4096)));
                    ui.label("to start of contour");
                    ui.push_id(vw::id(&c("join.b")), |ui| ui.add(egui::DragValue::new(&mut self.join_contours[1]).range(0..=4096)));
                    if button(ui, drawn, c("join"), "Join", has) {
                        let a = EndAddr { contour: self.join_contours[0], end: End::End };
                        let b = EndAddr { contour: self.join_contours[1], end: End::Start };
                        actions.push(PanelAction::EditPath(PathEdit::Join(a, b)));
                    }
                });
                drawn.extend([c("join.a"), c("join.b")]);
                ui.separator();
                ui.label("Type (live text on the selected path; point / area text)");
                text(ui, drawn, c("type.text"), "Text", &mut self.live_text, "one line of text");
                number(ui, drawn, c("type.inset"), "Area inset (pt)", &mut self.live_inset, 0.1);
                let typed = has && !self.live_text.is_empty();
                ui.horizontal_wrapped(|ui| {
                    if button(ui, drawn, c("type.on_path"), "Type on Path", typed) {
                        actions.push(PanelAction::BindLiveText { text: self.live_text.clone(), kind: LiveTextKind::Path });
                    }
                    if button(ui, drawn, c("type.area"), "Area Type", typed) {
                        actions.push(PanelAction::BindLiveText { text: self.live_text.clone(), kind: LiveTextKind::Area { inset_pt: self.live_inset } });
                    }
                    if button(ui, drawn, c("type.convert"), "Convert Point / Area Type", has) {
                        actions.push(PanelAction::ConvertTextKind);
                    }
                    if button(ui, drawn, c("type.outlines"), "Create Outlines", has) {
                        actions.push(PanelAction::CreateOutlines);
                    }
                });
            }
            HostedPanel::Masks => {
                selected_line(ui);
                let others: Vec<(String, String)> = view.layers.iter().filter(|(id, _)| view.selected.as_ref().is_none_or(|(s, _)| s != id)).cloned().collect();
                layer_choice(ui, drawn, c("source"), "Source layer", &mut self.mask_source, &others);
                let source = self.mask_source.clone().filter(|s| others.iter().any(|(id, _)| id == s));
                let ready = has && source.is_some();
                ui.horizontal(|ui| {
                    ui.push_id(vw::id(&c("even_odd")), |ui| ui.checkbox(&mut self.clip_even_odd, "even-odd"));
                    if button(ui, drawn, c("clip"), "Make Clipping Mask", ready)
                        && let Some(source) = source.clone()
                    {
                        actions.push(PanelAction::MakeClip { source, even_odd: self.clip_even_odd });
                    }
                });
                drawn.push(c("even_odd"));
                ui.horizontal(|ui| {
                    choice(ui, drawn, c("mode"), "", &mut self.mask_mode, &[(StudioMaskMode::Alpha, "alpha"), (StudioMaskMode::Luminance, "luminance"), (StudioMaskMode::Vector, "vector")]);
                    ui.push_id(vw::id(&c("invert")), |ui| ui.checkbox(&mut self.mask_invert, "invert"));
                    if button(ui, drawn, c("mask"), "Make Mask", ready)
                        && let Some(source) = source.clone()
                    {
                        actions.push(PanelAction::MakeMask { source, mode: self.mask_mode, invert: self.mask_invert });
                    }
                });
                drawn.push(c("invert"));
                if button(ui, drawn, c("release"), "Release", has) {
                    actions.push(PanelAction::ReleaseMasks);
                }
            }
            HostedPanel::EnvelopeDistort => {
                selected_line(ui);
                choice(ui, drawn, c("kind"), "Make with", &mut self.envelope_kind, &[(0, "warp"), (1, "mesh"), (2, "top object")]);
                let spec = match self.envelope_kind {
                    1 => {
                        ui.horizontal(|ui| {
                            ui.push_id(vw::id(&c("mesh.rows")), |ui| ui.add(egui::DragValue::new(&mut self.mesh[0]).range(1..=64).prefix("rows ")));
                            ui.push_id(vw::id(&c("mesh.columns")), |ui| ui.add(egui::DragValue::new(&mut self.mesh[1]).range(1..=64).prefix("columns ")));
                        });
                        drawn.extend([c("mesh.rows"), c("mesh.columns")]);
                        Some(EnvelopeSpec::Mesh { rows: self.mesh[0], columns: self.mesh[1] })
                    }
                    2 => {
                        let others: Vec<(String, String)> = view.vectors.iter().filter(|(id, _)| view.selected.as_ref().is_none_or(|(s, _)| s != id)).cloned().collect();
                        layer_choice(ui, drawn, c("top"), "Top object", &mut self.envelope_top, &others);
                        self.envelope_top.clone().filter(|t| others.iter().any(|(id, _)| id == t)).map(|layer| EnvelopeSpec::TopObject { layer })
                    }
                    _ => {
                        let options: Vec<(usize, &str)> = WARP_STYLES.iter().enumerate().map(|(i, (_, l))| (i, *l)).collect();
                        choice(ui, drawn, c("warp.style"), "Style", &mut self.warp_style, &options);
                        ui.push_id(vw::id(&c("warp.vertical")), |ui| ui.checkbox(&mut self.warp_vertical, "vertical"));
                        number(ui, drawn, c("warp.bend"), "Bend %", &mut self.warp[0], 1.0);
                        number(ui, drawn, c("warp.horizontal"), "Horizontal %", &mut self.warp[1], 1.0);
                        number(ui, drawn, c("warp.vertical_distortion"), "Vertical %", &mut self.warp[2], 1.0);
                        drawn.push(c("warp.vertical"));
                        let style = WARP_STYLES.get(self.warp_style).map_or(StudioWarpStyle::Arc, |(s, _)| *s);
                        let axis = if self.warp_vertical { StudioWarpAxis::Vertical } else { StudioWarpAxis::Horizontal };
                        Some(EnvelopeSpec::Warp { style, axis, bend: self.warp[0], horizontal: self.warp[1], vertical: self.warp[2] })
                    }
                };
                number(ui, drawn, c("fidelity"), "Fidelity", &mut self.fidelity, 1.0);
                ui.push_id(vw::id(&c("distort_appearance")), |ui| ui.checkbox(&mut self.distort_appearance, "distort appearance"));
                drawn.push(c("distort_appearance"));
                ui.horizontal(|ui| {
                    if button(ui, drawn, c("make"), "Make", has && spec.is_some())
                        && let Some(spec) = spec.clone()
                    {
                        actions.push(PanelAction::MakeEnvelope { spec, fidelity: self.fidelity, distort_appearance: self.distort_appearance });
                    }
                    if button(ui, drawn, c("release"), "Release", has) {
                        actions.push(PanelAction::ReleaseEnvelope);
                    }
                    if button(ui, drawn, c("expand"), "Expand", has) {
                        actions.push(PanelAction::ExpandEnvelope);
                    }
                });
            }
            HostedPanel::Brushes => {
                targets_ui(ui, drawn, key, &mut self.targets, &view.vectors);
                text(ui, drawn, c("name"), "Name", &mut self.brush_name, "brush name");
                choice(ui, drawn, c("kind"), "Kind", &mut self.brush_kind, &[(BrushChoice::Art, "art"), (BrushChoice::Scatter, "scatter"), (BrushChoice::Pattern, "pattern (side tiles)")]);
                choice(
                    ui,
                    drawn,
                    c("colorization"),
                    "Colorization",
                    &mut self.brush_colorization,
                    &[(StudioBrushColorization::None, "none"), (StudioBrushColorization::Tints, "tints"), (StudioBrushColorization::TintsAndShades, "tints and shades"), (StudioBrushColorization::HueShift, "hue shift")],
                );
                if button(ui, drawn, c("add"), "New Brush from Targets", !self.targets.is_empty()) {
                    actions.push(PanelAction::AddBrush { name: self.brush_name.clone(), kind: self.brush_kind, colorization: self.brush_colorization, layers: self.targets.clone() });
                }
                ui.separator();
                selected_line(ui);
                text(ui, drawn, c("scale"), "Scale %", &mut self.brush_scale, "empty: brush scale");
                let scale = self.brush_scale.trim().parse::<f64>().ok();
                for (id, n) in &view.brushes {
                    ui.horizontal(|ui| {
                        ui.label(n.as_str());
                        if button(ui, drawn, c(&format!("{id}.apply")), "Apply", has) {
                            actions.push(PanelAction::ApplyBrush { brush_id: id.clone(), scale });
                        }
                    });
                }
                if button(ui, drawn, c("remove"), "Remove Brush", has) {
                    actions.push(PanelAction::RemoveBrush);
                }
            }
            HostedPanel::VectorExport => {
                text(ui, drawn, c("path"), "Path", &mut self.export_path, "out.svg / out.pdf");
                ui.push_id(vw::id(&c("overwrite")), |ui| ui.checkbox(&mut self.export_overwrite, "overwrite"));
                drawn.push(c("overwrite"));
                let path = self.export_path.trim();
                let ready = !path.is_empty();
                ui.horizontal(|ui| {
                    if button(ui, drawn, c("svg"), "Export SVG", ready) {
                        actions.push(PanelAction::ExportSvg { path: PathBuf::from(path).with_extension("svg"), overwrite: self.export_overwrite });
                    }
                    if button(ui, drawn, c("pdf"), "Export PDF", ready) {
                        actions.push(PanelAction::ExportPdf { path: PathBuf::from(path).with_extension("pdf"), overwrite: self.export_overwrite });
                    }
                });
            }
            HostedPanel::VectorImport => {
                text(ui, drawn, c("source"), "Artwork", &mut self.import_source, "path to .ai");
                text(ui, drawn, c("dest"), "Save as", &mut self.import_dest, "out.handshake");
                ui.push_id(vw::id(&c("overwrite")), |ui| ui.checkbox(&mut self.import_overwrite, "overwrite"));
                drawn.push(c("overwrite"));
                let ready = !self.import_source.trim().is_empty() && !self.import_dest.trim().is_empty();
                if button(ui, drawn, c("import"), "Import as Editable Vectors", ready) {
                    actions.push(PanelAction::ImportVector { source: PathBuf::from(self.import_source.trim()), dest: PathBuf::from(self.import_dest.trim()), overwrite: self.import_overwrite });
                }
            }
            HostedPanel::AudioBatch => self.batch_ui(ui, drawn, actions),
        }
        if let Some(last) = self.last.get(&panel) {
            ui.separator();
            ui.push_id(vw::id(&c("last")), |ui| ui.label(last.as_str()));
            drawn.push(c("last"));
        }
    }

    #[cfg(feature = "audio-batch")]
    fn batch_ui(&mut self, ui: &mut egui::Ui, drawn: &mut Vec<String>, actions: &mut Vec<PanelAction>) {
        let key = HostedPanel::AudioBatch.key();
        let c = |control: &str| widget::control(key, control);
        let b = &mut self.batch;
        text(ui, drawn, c("input"), "Input", &mut b.input, "audio file");
        text(ui, drawn, c("output"), "Output", &mut b.output, "out.flac / out.aiff");
        ui.horizontal(|ui| {
            ui.push_id(vw::id(&c("gain.on")), |ui| ui.checkbox(&mut b.gain, "gain"));
            ui.push_id(vw::id(&c("gain.db")), |ui| ui.add(egui::DragValue::new(&mut b.gain_db).speed(0.1).suffix(" dB")));
            ui.push_id(vw::id(&c("aiff")), |ui| ui.checkbox(&mut b.aiff, "AIFF (else FLAC)"));
            ui.push_id(vw::id(&c("overwrite")), |ui| ui.checkbox(&mut b.overwrite, "overwrite"));
        });
        drawn.extend([c("gain.on"), c("gain.db"), c("aiff"), c("overwrite")]);
        let ready = !b.input.trim().is_empty() && !b.output.trim().is_empty();
        ui.horizontal(|ui| {
            if button(ui, drawn, c("add"), "Add to Queue", ready) {
                use super::audio_import::MixFormat;
                actions.push(PanelAction::BatchAdd(BatchItem {
                    input: PathBuf::from(b.input.trim()),
                    output: PathBuf::from(b.output.trim()),
                    gain_db: b.gain.then_some(b.gain_db),
                    format: if b.aiff { MixFormat::Aiff { bits: 24 } } else { MixFormat::Flac { bits: 24 } },
                    overwrite: b.overwrite,
                }));
            }
            if button(ui, drawn, c("run"), "Run Queue", !b.pending.is_empty()) {
                actions.push(PanelAction::BatchRun);
            }
            if button(ui, drawn, c("clear"), "Clear", !b.pending.is_empty() || !b.receipts.is_empty()) {
                actions.push(PanelAction::BatchClear);
            }
        });
        for item in &b.pending {
            ui.label(format!("queued: {} -> {}", item.input.display(), item.output.display()));
        }
        for r in &b.receipts {
            ui.label(format!("{}: {}", r["artifact"].as_str().unwrap_or("?"), r["outcome"].as_str().unwrap_or("?")));
        }
    }

    #[cfg(not(feature = "audio-batch"))]
    fn batch_ui(&mut self, ui: &mut egui::Ui, _: &mut Vec<String>, _: &mut Vec<PanelAction>) {
        ui.label("Audio Batch needs feature audio-batch.");
    }
}

impl Viewer {
    /// Availability of a hosted panel's WORKSPACE entry (the shell checked the feature).
    pub(super) fn hosted_availability(&self, panel: HostedPanel) -> Availability {
        if panel.needs_document() && self.run.snapshot().is_none() {
            return Availability::Unavailable("open a document first".into());
        }
        Availability::Available
    }

    fn hosted_view(&self, shown: &[HostedPanel]) -> PanelView {
        let rows = self.layers();
        let layers: Vec<(String, String)> = rows.iter().map(|r| (r.layer_id.clone(), r.name.clone())).collect();
        let vectors: Vec<(String, String)> = rows.iter().filter(|r| r.kind == "vector").map(|r| (r.layer_id.clone(), r.name.clone())).collect();
        let selected = self.selected.as_ref().map(|id| (id.clone(), name_of(&layers, id)));
        let wants = |p: HostedPanel| shown.contains(&p);
        PanelView {
            document: self.run.snapshot().is_some(),
            swatches: if wants(HostedPanel::Swatches) { self.swatches().unwrap_or_default() } else { Vec::new() },
            styles: if wants(HostedPanel::GraphicStyles) { self.graphic_styles().unwrap_or_default() } else { Vec::new() },
            brushes: if wants(HostedPanel::Brushes) { self.brushes().unwrap_or_default() } else { Vec::new() },
            summaries: shown.iter().map(|p| (*p, self.hosted_summary(*p))).collect(),
            selected,
            vectors,
            layers,
        }
    }

    /// One line of key state per hosted panel (drawn as the panel's first label and projected as
    /// `state_json()["shell"]["hosted"][key]["summary"]`).
    pub fn hosted_summary(&self, panel: HostedPanel) -> String {
        let selected = || self.selected.clone();
        let layer = |id: &str| self.layers().into_iter().find(|r| r.layer_id == id).map_or_else(|| id.to_owned(), |r| r.name);
        let rows = |id: &str| -> (usize, usize) {
            self.layer_paint_style(id).map_or((0, 0), |s| {
                let fills = s.rows.iter().filter(|r| matches!(r, StudioAppearanceRow::Fill { .. })).count();
                (fills, s.rows.len().saturating_sub(fills))
            })
        };
        let targets = self.shell.hosted.targets.len();
        match panel {
            HostedPanel::Appearance | HostedPanel::Stroke => match selected() {
                Some(id) => {
                    let (f, s) = rows(&id);
                    format!("{}: {f} fill rows, {s} stroke rows", layer(&id))
                }
                None => "no layer selected".into(),
            },
            HostedPanel::Swatches => format!("{} swatches", self.swatches().map_or(0, |s| s.len())),
            HostedPanel::GraphicStyles => format!("{} graphic styles", self.graphic_styles().map_or(0, |s| s.len())),
            HostedPanel::AlignDistribute | HostedPanel::Transform => format!("{targets} target layers"),
            HostedPanel::PathOperations => match selected() {
                Some(id) => {
                    let kind = match (self.live_shape(&id), self.vector_path(&id)) {
                        (Ok(Some(_)), _) => "live shape",
                        (_, Ok(_)) => "path",
                        _ => "not a vector path",
                    };
                    // The live text bound to the layer (lane SMB-VIEW-POLISH-1): the status line's
                    // placed / overset counts; nothing appended without live text.
                    let live: Vec<String> = self.live_text_lines().into_iter().filter(|(l, _, _)| *l == id).map(|(_, _, line)| line).collect();
                    if live.is_empty() { format!("{}: {kind}", layer(&id)) } else { format!("{}: {kind}; {}", layer(&id), live.join("; ")) }
                }
                None => "no layer selected".into(),
            },
            HostedPanel::Masks => match selected() {
                Some(id) => {
                    let a = self.vector_entry().ok().and_then(|e| e.records.attachments.iter().find(|a| a.layer_id == id).cloned());
                    let state = match a {
                        Some(a) if a.clip_id.is_some() && a.mask_id.is_some() => "clipped and masked",
                        Some(a) if a.clip_id.is_some() => "clipped",
                        Some(_) => "masked",
                        None => "no clip or mask",
                    };
                    format!("{}: {state}", layer(&id))
                }
                None => "no layer selected".into(),
            },
            HostedPanel::EnvelopeDistort => match selected() {
                Some(id) => {
                    let state = match self.envelope(&id) {
                        Ok(Some(e)) => match e.envelope {
                            StudioEnvelopeShape::Warp { .. } => "warp envelope",
                            StudioEnvelopeShape::Mesh { .. } => "mesh envelope",
                            StudioEnvelopeShape::TopObject { .. } => "top-object envelope",
                        },
                        _ => "no envelope",
                    };
                    format!("{}: {state}", layer(&id))
                }
                None => "no layer selected".into(),
            },
            HostedPanel::Brushes => format!("{} brushes, {targets} target layers", self.brushes().map_or(0, |b| b.len())),
            HostedPanel::VectorExport => {
                let p = self.shell.hosted.export_path.trim();
                if p.is_empty() { "no export path".into() } else { format!("export to {p}") }
            }
            HostedPanel::VectorImport => {
                let s = self.shell.hosted.import_source.trim();
                if s.is_empty() { "no artwork chosen".into() } else { format!("artwork {s}") }
            }
            HostedPanel::AudioBatch => self.batch_summary(),
        }
    }

    #[cfg(feature = "audio-batch")]
    fn batch_summary(&self) -> String {
        let b = &self.shell.hosted.batch;
        format!("{} queued, {} receipts", b.pending.len(), b.receipts.len())
    }

    #[cfg(not(feature = "audio-batch"))]
    fn batch_summary(&self) -> String {
        "needs feature audio-batch".into()
    }

    /// Runs one hosted panel action (the exact call its control makes). The result is also kept as
    /// the panel's last result and said in the status log.
    pub fn hosted_action(&mut self, action: PanelAction) -> Result<String, String> {
        let panel = action.panel();
        let result = self.hosted_action_inner(action);
        let line = match &result {
            Ok(s) => format!("{}: {s}", panel.label()),
            Err(e) => format!("{}: {e}", panel.label()),
        };
        self.shell.hosted.last.insert(panel, line.clone());
        self.say(line);
        result
    }

    fn hosted_action_inner(&mut self, action: PanelAction) -> Result<String, String> {
        let layer = match (action.needs_selection(), self.selected.clone()) {
            (true, None) => return Err("select a layer in the Layers panel first".into()),
            (_, l) => l.unwrap_or_default(),
        };
        let losses = |l: Vec<String>| if l.is_empty() { "no losses".to_owned() } else { format!("losses: {}", l.join("; ")) };
        match action {
            PanelAction::SetFill(c) => self.set_fill(&layer, c.map(PaintSpec::Solid).as_ref()).map(|_| "fill set".into()),
            PanelAction::ClearAppearance => self.clear_appearance(&layer).map(|_| "appearance cleared".into()),
            PanelAction::ReduceToBasicAppearance => self.reduce_to_basic_appearance(&layer).map(|_| "reduced to basic appearance".into()),
            PanelAction::SetStroke(s) => self.set_stroke(&layer, s.as_ref()).map(|_| "stroke set".into()),
            PanelAction::AddSwatch { name, colour } => self.add_swatch(&name, &PaintSpec::Solid(colour)).map(|id| format!("swatch {id}")),
            PanelAction::ApplySwatch { swatch_id, kind, tint } => self.apply_swatch_tinted(&layer, &swatch_id, kind, tint).map(|_| format!("{} linked to {swatch_id}", kind.token())),
            PanelAction::DetachSwatch(kind) => self.detach_swatch(&layer, kind).map(|_| format!("{} detached", kind.token())),
            PanelAction::AddGraphicStyle { name } => self.add_graphic_style(&name, &layer).map(|id| format!("graphic style {id}")),
            PanelAction::ApplyGraphicStyle(id) => self.apply_graphic_style(&layer, &id).map(|_| format!("applied {id}")),
            PanelAction::MergeGraphicStyle(id) => self.merge_graphic_style(&layer, &id).map(|_| format!("merged {id}")),
            PanelAction::RedefineGraphicStyle(id) => self.redefine_graphic_style(&id, &layer).map(|_| format!("redefined {id}")),
            PanelAction::BreakGraphicStyle => self.break_graphic_style(&layer).map(|_| "link broken".into()),
            PanelAction::Align { layers, to, how } => self.align_layers(&layers, &to, how).map(|()| format!("aligned {} layers", layers.len())),
            PanelAction::Distribute { layers, to, how } => self.distribute_layers(&layers, &to, how).map(|()| format!("distributed {} layers", layers.len())),
            PanelAction::Transform { layers, op, reference } => self.transform_layers(&layers, op, reference).map(|()| format!("transformed {} layers", layers.len())),
            PanelAction::TransformEach { layers, each } => self.transform_each_layers(&layers, &each).map(|()| format!("transformed {} layers each", layers.len())),
            PanelAction::OffsetPath { offset, join } => self.offset_path_layer(&layer, offset, join).map(|id| format!("offset path layer {id}")),
            PanelAction::OutlineStroke => self.outline_stroke_layer(&layer).map(|_| "stroke outlined".into()),
            PanelAction::ExpandShape => self.expand_shape(&layer).map(|_| "shape expanded".into()),
            PanelAction::PromoteNetwork => self.promote_network(&layer).map(|_| "promoted to a network".into()),
            PanelAction::EditPath(edit) => self.edit_path(&layer, edit).map(|_| format!("path edit {edit:?}")),
            PanelAction::BindLiveText { text, kind } => self.bind_live_text(&layer, &text, kind).map(|id| format!("live text {id}")),
            PanelAction::ConvertTextKind => self.convert_text_kind(&layer).map(|id| format!("converted to {id}")),
            PanelAction::CreateOutlines => self.create_outlines(&layer).map(|id| format!("outlines in {id}")),
            PanelAction::MakeClip { source, even_odd } => self.make_clip(&layer, &source, even_odd).map(|id| format!("clip {id}")),
            PanelAction::MakeMask { source, mode, invert } => self.make_vector_mask(&layer, &source, mode, invert).map(|id| format!("mask {id}")),
            PanelAction::ReleaseMasks => self.release_masks(&layer).map(|()| "released".into()),
            PanelAction::MakeEnvelope { spec, fidelity, distort_appearance } => self.make_envelope(&layer, &spec, fidelity, distort_appearance).map(|_| "envelope made".into()),
            PanelAction::ReleaseEnvelope => self.release_envelope(&layer).map(|_| "envelope released".into()),
            PanelAction::ExpandEnvelope => self.expand_envelope(&layer).map(|_| "envelope expanded".into()),
            PanelAction::AddBrush { name, kind, colorization, layers } => {
                let (art, tiles) = match kind {
                    BrushChoice::Pattern => (Vec::new(), PatternTileLayers { side: layers, ..PatternTileLayers::default() }),
                    _ => (layers, PatternTileLayers::default()),
                };
                self.add_brush(&name, brush_kind(kind), colorization, &art, &tiles).map(|id| format!("brush {id}"))
            }
            PanelAction::ApplyBrush { brush_id, scale } => self.apply_brush(&layer, &brush_id, scale).map(|_| format!("brushed with {brush_id}")),
            PanelAction::RemoveBrush => self.remove_brush(&layer).map(|_| "brush removed".into()),
            PanelAction::ExportSvg { path, overwrite } => self.export_vector_svg(&path, overwrite).map(|l| format!("wrote {}; {}", path.display(), losses(l))),
            PanelAction::ExportPdf { path, overwrite } => self.export_vector_pdf(&path, overwrite).map(|l| format!("wrote {}; {}", path.display(), losses(l))),
            PanelAction::ImportVector { source, dest, overwrite } => self.import_ai(&source, &dest, overwrite).map(|l| format!("opened {}; {}", dest.display(), losses(l))),
            #[cfg(feature = "audio-batch")]
            PanelAction::BatchAdd(item) => {
                let b = &mut self.shell.hosted.batch;
                if b.pending.len() >= MAX_BATCH_ITEMS {
                    return Err(format!("the queue holds at most {MAX_BATCH_ITEMS} items"));
                }
                b.pending.push(item);
                Ok(format!("{} queued", b.pending.len()))
            }
            #[cfg(feature = "audio-batch")]
            PanelAction::BatchRun => self.run_batch(),
            #[cfg(feature = "audio-batch")]
            PanelAction::BatchClear => {
                let b = &mut self.shell.hosted.batch;
                b.pending.clear();
                b.receipts.clear();
                Ok("cleared".into())
            }
        }
    }

    /// Runs the queued audio batch items through the existing queue (in submission order, on this
    /// thread); receipts replace the previous run's.
    #[cfg(feature = "audio-batch")]
    fn run_batch(&mut self) -> Result<String, String> {
        use super::audio_batch::{AudioBatchJob, AudioBatchQueue, BatchOutcome, EffectSpec};
        let items = std::mem::take(&mut self.shell.hosted.batch.pending);
        if items.is_empty() {
            return Err("nothing queued".into());
        }
        let mut queue = AudioBatchQueue::new(crate::harness::default_actor());
        for item in items {
            let artifact = item.output.file_name().map_or_else(|| "output".to_owned(), |n| n.to_string_lossy().into_owned());
            let _handle = queue.submit(AudioBatchJob {
                input: item.input,
                output: item.output,
                overwrite: item.overwrite,
                artifact,
                options: super::audio_import::ImportOptions::default(),
                limits: super::audio_import::ImportLimits::default(),
                chain: item.gain_db.map(|db| vec![EffectSpec::Gain { db }]).unwrap_or_default(),
                keyframes: Vec::new(),
                format: item.format,
            });
        }
        let receipts = queue.run();
        let complete = receipts.iter().filter(|r| r.outcome == BatchOutcome::Complete).count();
        let total = receipts.len();
        self.shell.hosted.batch.receipts = receipts.iter().map(|r| r.to_json()).collect();
        Ok(format!("{complete} of {total} complete"))
    }

    /// State projection of the hosted panels (`state_json()["shell"]["hosted"]`).
    pub(super) fn hosted_json(&self) -> Value {
        let h = &self.shell.hosted;
        let panels: serde_json::Map<String, Value> = HostedPanel::ALL
            .iter()
            .map(|p| {
                let mut v = json!({
                    "open": h.is_open(*p),
                    "visible": self.panel_visible(Panel::Hosted(*p)),
                    "summary": self.hosted_summary(*p),
                    "last": h.last.get(p),
                    "window": widget::window(p.key()),
                });
                match p {
                    HostedPanel::Swatches => v["swatches"] = json!(self.swatches().unwrap_or_default()),
                    HostedPanel::GraphicStyles => v["styles"] = json!(self.graphic_styles().unwrap_or_default()),
                    HostedPanel::Brushes => v["brushes"] = json!(self.brushes().unwrap_or_default()),
                    HostedPanel::AlignDistribute | HostedPanel::Transform => v["targets"] = json!(h.targets),
                    HostedPanel::VectorExport => v["path"] = json!(h.export_path),
                    HostedPanel::VectorImport => v["source"] = json!(h.import_source),
                    #[cfg(feature = "audio-batch")]
                    HostedPanel::AudioBatch => {
                        v["pending"] = json!(h.batch.pending.iter().map(|i| json!({"input": i.input.to_string_lossy(), "output": i.output.to_string_lossy(), "gain_db": i.gain_db})).collect::<Vec<_>>());
                        v["receipts"] = json!(h.batch.receipts);
                    }
                    _ => {}
                }
                (p.key().to_owned(), v)
            })
            .collect();
        Value::Object(panels)
    }

    /// Key state of the shell-hosted surfaces that keep their own state projection
    /// (`state_json()["shell"]["surfaces"]`; mixer, waveform and spectral are top-level entries).
    pub(super) fn surfaces_json(&self) -> Value {
        json!({
            "layout_editor": self.shell.layout_state(),
            "program_monitor": self.monitor.as_ref().map(|m| m.monitor.state_json()),
            "source_monitor": self.source_monitor().map(|m| m.state_json()),
            "prototype_player": self.prototype.as_ref().map(|(p, _)| p.state_json()),
        })
    }

    /// Draws the open, shown hosted panels as floating windows; their actions run after drawing.
    pub(super) fn hosted_ui(&mut self, ctx: &egui::Context) {
        let shown: Vec<HostedPanel> = HostedPanel::ALL.into_iter().filter(|p| self.shell.hosted.is_open(*p) && self.shell.visible(Panel::Hosted(*p))).collect();
        if shown.is_empty() {
            return;
        }
        let view = self.hosted_view(&shown);
        let mut state = std::mem::take(&mut self.shell.hosted);
        let mut actions = Vec::new();
        let mut drawn = Vec::new();
        for (i, p) in shown.iter().enumerate() {
            let step = i as f32 * 28.0;
            egui::Window::new(p.label())
                .id(vw::id(&widget::window(p.key())))
                .default_pos(egui::pos2(280.0 + step, 90.0 + step))
                .default_width(300.0)
                .resizable(true)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| state.panel_ui(*p, ui, &view, &mut actions, &mut drawn));
                });
            drawn.push(widget::window(p.key()));
        }
        self.shell.hosted = state;
        self.drawn.extend(drawn);
        for a in actions {
            let _ = self.hosted_action(a);
        }
    }
}

/// Most audio batch items the panel queues at once.
#[cfg(feature = "audio-batch")]
pub const MAX_BATCH_ITEMS: usize = 256;
