//! Editing in the viewer (lane SMB-HARNESS-UI-2): appearance edits, selections, brush painting,
//! undo/redo. Every action is a [`Viewer`] method first (headless, model-navigable, recorded in
//! the run's operation log); the egui tools in [`Viewer::ui`] only call these methods.
//!
//! - Opacity / blend mode: chronicle `opacity` / `blend_mode` through the session (undoable).
//! - Selection: hsk-studio-pigment `selection` producers (rect, ellipse, lasso polygon) combined
//!   with the current [`CombineMode`] into one coverage plane in document pixels; shown as marching
//!   ants (the 50 % coverage outline).
//! - Brush: hsk-studio-pigment `brush` (place_dabs with pen pressure on size, stroke_coverage,
//!   clip_to_selection) on the selected raster layer; the stroke's new tile bodies are staged in the
//!   session and committed as chronicle `tiles` (undoable). Painting into an empty 256-pixel cell
//!   creates its tile (lane SMB-HARNESS-UI-3). The per-pixel mix runs through pigment
//!   `masked_replace` itself ([`super::brush_mix`], 64 x 64 blocks under its 4096-pixel cap).
//! - Undo / redo: the run's chronicle inverse history ([`crate::Run::undo`], [`crate::Run::redo`]).
//! - Text: [`super::text`] (lane SMB-HARNESS-UI-3): a click places a point-text layer.
use super::Viewer;
use super::brush_mix::{self, MixTarget};
use hsk_studio_accord::{BoundSource, DomainId, Redaction, SettingConstraint, SettingEntry, SettingScope, SettingValue};
use hsk_studio_folio::{Payload, TileRef};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::brush::{
    BrushDynamics, BrushLimits, BrushTip, Channel, Control, StrokeSample, clip_to_selection, place_dabs, stroke_coverage,
};
use hsk_studio_pigment::selection::{self, CombineMode, CoveragePlane, FillRule, RectF, SelectionContext};
use serde_json::{Value, json};


/// Pixel edge of the importers' tile cells (`hsk_studio_interop_psd::TILE_EDGE`).
pub(super) const CELL: usize = 256;
pub(super) const COLOUR_CELL: usize = CELL * CELL * 12;
pub(super) const ALPHA_CELL: usize = CELL * CELL * 4;
/// Marching-ants segments drawn at most (larger outlines are drawn partially and say so).
pub const MAX_ANT_SEGMENTS: usize = 200_000;

/// The active canvas tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Drag pans, wheel zooms.
    View,
    RectSelect,
    EllipseSelect,
    Lasso,
    Brush,
    Text,
    /// Vector tools (lane SMB-ILLUSTRATOR-PEN, module [`super::vector`]).
    Pen,
    DirectSelect,
    Rectangle,
    RoundedRectangle,
    EllipseShape,
    Polygon,
    Star,
    Line,
    /// Lane SMB-ILLUSTRATOR-PEN-2.
    Curvature,
    Pencil,
    Knife,
    ShapeBuilder,
    ArcSegment,
    RectangularGrid,
    PolarGrid,
    Spiral,
    /// Lane SMB-ILLUSTRATOR-PEN-3.
    Width,
    Reshape,
    BlobBrush,
    PaintBucket,
    /// Lane SMB-PS-SELECT-LIQUIFY-2 (module [`super::select_liquify`]).
    MagicWand,
    QuickSelect,
    MagneticLasso,
    Liquify,
    /// Lane SMB-PS-SMART-FILTER-WIRING: the STU-RAS-122 refinement brush set.
    RefineBrush,
    /// Lane SMB-PS-GRADIENT-BUCKET-1 (module [`super::fill_tools`]): the raster gradient and the
    /// raster paint bucket (STU-SHL-155 `gradient`, `flood_fill`).
    Gradient,
    FloodFill,
}

impl Tool {
    pub const ALL: [Tool; 33] = [
        Tool::View,
        Tool::RectSelect,
        Tool::EllipseSelect,
        Tool::Lasso,
        Tool::Brush,
        Tool::Text,
        Tool::Pen,
        Tool::DirectSelect,
        Tool::Rectangle,
        Tool::RoundedRectangle,
        Tool::EllipseShape,
        Tool::Polygon,
        Tool::Star,
        Tool::Line,
        Tool::Curvature,
        Tool::Pencil,
        Tool::Knife,
        Tool::ShapeBuilder,
        Tool::ArcSegment,
        Tool::RectangularGrid,
        Tool::PolarGrid,
        Tool::Spiral,
        Tool::Width,
        Tool::Reshape,
        Tool::BlobBrush,
        Tool::PaintBucket,
        Tool::MagicWand,
        Tool::QuickSelect,
        Tool::MagneticLasso,
        Tool::Liquify,
        Tool::RefineBrush,
        Tool::Gradient,
        Tool::FloodFill,
    ];
    pub fn key(self) -> &'static str {
        match self {
            Tool::View => "view",
            Tool::RectSelect => "rect_select",
            Tool::EllipseSelect => "ellipse_select",
            Tool::Lasso => "lasso",
            Tool::Brush => "brush",
            Tool::Text => "text",
            Tool::Pen => "pen",
            Tool::DirectSelect => "direct_select",
            Tool::Rectangle => "rectangle",
            Tool::RoundedRectangle => "rounded_rectangle",
            Tool::EllipseShape => "ellipse",
            Tool::Polygon => "polygon",
            Tool::Star => "star",
            Tool::Line => "line",
            Tool::Curvature => "curvature",
            Tool::Pencil => "pencil",
            Tool::Knife => "knife",
            Tool::ShapeBuilder => "shape_builder",
            Tool::ArcSegment => "arc",
            Tool::RectangularGrid => "rectangular_grid",
            Tool::PolarGrid => "polar_grid",
            Tool::Spiral => "spiral",
            Tool::Width => "width",
            Tool::Reshape => "reshape",
            Tool::BlobBrush => "blob_brush",
            Tool::PaintBucket => "paint_bucket",
            Tool::MagicWand => "magic_wand",
            Tool::QuickSelect => "quick_selection",
            Tool::MagneticLasso => "magnetic_lasso",
            Tool::Liquify => "liquify",
            Tool::RefineBrush => "refine_edge_brush",
            Tool::Gradient => "gradient",
            Tool::FloodFill => "flood_fill",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Tool::View => "Hand",
            Tool::RectSelect => "Rect",
            Tool::EllipseSelect => "Ellipse",
            Tool::Lasso => "Lasso",
            Tool::Brush => "Brush",
            Tool::Text => "Text",
            Tool::Pen => "Pen",
            Tool::DirectSelect => "Direct",
            Tool::Rectangle => "Rectangle",
            Tool::RoundedRectangle => "Rounded",
            Tool::EllipseShape => "Ellipse shape",
            Tool::Polygon => "Polygon",
            Tool::Star => "Star",
            Tool::Line => "Line",
            Tool::Curvature => "Curvature",
            Tool::Pencil => "Pencil",
            Tool::Knife => "Knife",
            Tool::ShapeBuilder => "Shape builder",
            Tool::ArcSegment => "Arc",
            Tool::RectangularGrid => "Grid",
            Tool::PolarGrid => "Polar grid",
            Tool::Spiral => "Spiral",
            Tool::Width => "Width",
            Tool::Reshape => "Reshape",
            Tool::BlobBrush => "Blob brush",
            Tool::PaintBucket => "Paint bucket",
            Tool::MagicWand => "Magic wand",
            Tool::QuickSelect => "Quick select",
            Tool::MagneticLasso => "Magnetic lasso",
            Tool::Liquify => "Liquify",
            Tool::RefineBrush => "Refine edge brush",
            Tool::Gradient => "Gradient",
            Tool::FloodFill => "Bucket fill",
        }
    }
}

/// Stable key of a combine mode.
pub fn combine_key(mode: CombineMode) -> &'static str {
    match mode {
        CombineMode::Replace => "replace",
        CombineMode::Extend => "extend",
        CombineMode::Diminish => "diminish",
        CombineMode::Intersect => "intersect",
    }
}

/// Brush parameters (round parametric tip). Colour is 8-bit sRGB; painting converts it into the
/// linear working space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushSettings {
    pub diameter_px: f64,
    pub hardness_pct: f64,
    pub spacing_pct: f64,
    pub opacity_pct: f64,
    pub rgb8: [u8; 3],
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self { diameter_px: 24.0, hardness_pct: 80.0, spacing_pct: 25.0, opacity_pct: 100.0, rgb8: [220, 30, 40] }
    }
}

pub const BRUSH_DIAMETER: &str = "studio.harness-view.brush.diameter";
pub const BRUSH_HARDNESS: &str = "studio.harness-view.brush.hardness";
pub const BRUSH_SPACING: &str = "studio.harness-view.brush.spacing";
pub const BRUSH_OPACITY: &str = "studio.harness-view.brush.opacity";

const fn brush_entry(id: &'static str, label: &'static str, unit: &'static str, min: f64, max: f64, default: f64) -> SettingEntry {
    SettingEntry {
        id,
        module: "harness-view",
        feature: "brush",
        label,
        unit: Some(unit),
        constraint: SettingConstraint::FloatRange { min, max },
        default: SettingValue::Float(default),
        scope: SettingScope::Workspace,
        redaction: Redaction::Public,
        bound_source: BoundSource::StudioDeclared,
    }
}

/// [`BrushSettings`] as typed settings entries (lane SMB-SETTINGS-SCHEMA-1, RF-09 phase 0), module
/// tab `harness-view`, feature subtab `brush`; joined into the Studio catalog by
/// [`super::settings_catalog`]. Defaults equal [`BrushSettings::default`] field for field. Bounds
/// lie inside pigment's brush domain (`BrushTip::validate`: diameter 1..=5000 px, hardness
/// 0..=100 %, spacing above 0 with no upper clamp; the dynamics opacity channel 0..=100 %); the
/// spacing maximum 1000 % is a Studio reading (Photoshop's range UNVERIFIED, D5). The colour
/// (`rgb8`) gets no entry: it is per-stroke tool state and Handshake's constraints have no colour
/// kind (SyntaxColorMap is editor-only).
pub const BRUSH_SETTING_ENTRIES: &[SettingEntry] = &[
    brush_entry(BRUSH_DIAMETER, "Brush size", "px", 1.0, 5000.0, 24.0),
    brush_entry(BRUSH_HARDNESS, "Brush hardness", "%", 0.0, 100.0, 80.0),
    brush_entry(BRUSH_SPACING, "Brush spacing", "%", 1.0, 1000.0, 25.0),
    brush_entry(BRUSH_OPACITY, "Brush opacity", "%", 0.0, 100.0, 100.0),
];

/// The current values of `b` as `(id, value)` pairs in [`BRUSH_SETTING_ENTRIES`] order.
pub fn brush_values(b: &BrushSettings) -> [(&'static str, SettingValue<'static>); 4] {
    [
        (BRUSH_DIAMETER, SettingValue::Float(b.diameter_px)),
        (BRUSH_HARDNESS, SettingValue::Float(b.hardness_pct)),
        (BRUSH_SPACING, SettingValue::Float(b.spacing_pct)),
        (BRUSH_OPACITY, SettingValue::Float(b.opacity_pct)),
    ]
}

/// Editing state held by the viewer.
#[derive(Clone, Debug)]
pub struct EditState {
    pub tool: Tool,
    pub combine: CombineMode,
    pub brush: BrushSettings,
    pub(super) selection: Option<CoveragePlane>,
    /// Document-space outline segments of the selection (horizontal and vertical pixel edges).
    pub(super) ants: Vec<[[f32; 2]; 2]>,
    pub(super) ants_truncated: bool,
    /// The gesture in progress, in document pixels (and pressure for the brush).
    pub(super) gesture: Vec<StrokeSample>,
    /// Vector tool settings and pen state (module [`super::vector`]).
    pub vector: super::vector::VectorTool,
    /// The vector entry of the open document while the session has no `vector` token.
    pub(super) vector_local: Option<Vec<u8>>,
    /// Magic wand, quick select, magnetic lasso and liquify settings (module
    /// [`super::select_liquify`]).
    pub ps: super::select_liquify::PsSettings,
    /// The refinement surface state of the refine-edge brush (module [`super::select_liquify`]).
    pub(super) refine: Option<super::select_liquify::RefineSession>,
    /// Gradient tool, paint bucket and Edit > Fill settings (module [`super::fill_tools`]).
    pub fill: super::fill_tools::FillSettings,
}

impl Default for EditState {
    fn default() -> Self {
        Self {
            tool: Tool::View,
            combine: CombineMode::Replace,
            brush: BrushSettings::default(),
            selection: None,
            ants: Vec::new(),
            ants_truncated: false,
            gesture: Vec::new(),
            vector: super::vector::VectorTool::default(),
            vector_local: None,
            ps: super::select_liquify::PsSettings::default(),
            refine: None,
            fill: super::fill_tools::FillSettings::default(),
        }
    }
}

/// IEC 61966-2-1 decode of an 8-bit sRGB value into linear light.
fn linear(v: u8) -> f32 {
    let c = f32::from(v) / 255.0;
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// Placement of a layer's tiles: a tile (column, row) lies on a grid of the layer's largest tile
/// size and keeps its own size (composite `tile_rects`, the rule every renderer uses).
pub(super) fn tile_rects(tiles: &[TileRef]) -> Option<Vec<Rect>> {
    let px = |l: &hsk_studio_folio::Length| (l.value.is_finite() && l.value >= 1.0 && l.value <= CELL as f64 && l.value.fract() == 0.0).then_some(l.value as u32);
    let (mut gw, mut gh) = (0u32, 0u32);
    for t in tiles {
        gw = gw.max(px(&t.width)?);
        gh = gh.max(px(&t.height)?);
    }
    tiles
        .iter()
        .map(|t| {
            Some(Rect { x: t.column.checked_mul(i64::from(gw))?, y: t.row.checked_mul(i64::from(gh))?, width: px(&t.width)?, height: px(&t.height)? })
        })
        .collect()
}

/// Smallest rectangle holding both.
fn union(a: Rect, b: Rect) -> Rect {
    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
    let x1 = a.x.saturating_add(i64::from(a.width)).max(b.x.saturating_add(i64::from(b.width)));
    let y1 = a.y.saturating_add(i64::from(a.height)).max(b.y.saturating_add(i64::from(b.height)));
    Rect { x: x0, y: y0, width: u32::try_from(x1.saturating_sub(x0)).unwrap_or(u32::MAX), height: u32::try_from(y1.saturating_sub(y0)).unwrap_or(u32::MAX) }
}

/// The colour profile new tiles are written in: the layer's own tiles', else any raster tile of
/// the document (the importers write one working profile), else the assigned RGB working space.
pub(super) fn new_tile_profile(doc: &hsk_studio_folio::StudioDocument, tiles: &[TileRef]) -> Option<String> {
    tiles
        .first()
        .or_else(|| doc.layers.iter().find_map(|l| if let Payload::Raster { tiles } = &l.payload { tiles.first() } else { None }))
        .map(|t| t.colour_profile_id.clone())
        .or_else(|| match &doc.working_space_rgb {
            hsk_studio_folio::ProfileBinding::Assigned(id) => Some(id.clone()),
            hsk_studio_folio::ProfileBinding::Unassigned(()) => None,
        })
}

/// A tile reference for a new 256-pixel cell of `layer_id` (format `rgb_f32le` + straight alpha,
/// the importers' layout), with an object key unique in the layer.
pub(super) fn new_tile(layer_id: &str, column: i64, row: i64, profile: &str, digest: &hsk_studio_folio::ContentDigest, existing: &[TileRef]) -> Result<TileRef, String> {
    let mut key = format!("t_{column}_{row}");
    if existing.iter().any(|t| t.object_key == key) {
        key = format!("p_{column}_{row}");
    }
    serde_json::from_value(json!({
        "object_key": key, "layer_id": layer_id, "column": column, "row": row,
        "width": {"value": CELL, "unit": "px"}, "height": {"value": CELL, "unit": "px"},
        "format": "rgb_f32le", "colour_profile_id": profile, "schema_id": "hsk.studio.raster_tile@1",
        "content_digest": digest, "artifact_manifest_id": format!("package/{layer_id}/{key}"),
    }))
    .map_err(|e| format!("paint: new tile reference: {e}"))
}

/// Outline of the 50 % coverage region as unit pixel edges, merged into runs.
fn outline(plane: &CoveragePlane) -> (Vec<[[f32; 2]; 2]>, bool) {
    let inside = |x: i64, y: i64| plane.at(x, y) >= 32_768;
    let b = plane.bounds;
    let (Ok((ex, ey)), mut out, mut truncated) = (b.end(), Vec::new(), false) else {
        return (Vec::new(), false);
    };
    // Horizontal edges: between rows y-1 and y.
    let mut y = b.y;
    while y <= ey {
        let mut run: Option<i64> = None;
        let mut x = b.x;
        while x <= ex {
            let edge = x < ex && inside(x, y.saturating_sub(1)) != inside(x, y);
            match (edge, run) {
                (true, None) => run = Some(x),
                (false, Some(start)) => {
                    out.push([[start as f32, y as f32], [x as f32, y as f32]]);
                    run = None;
                }
                _ => {}
            }
            x = x.saturating_add(1);
        }
        y = y.saturating_add(1);
        if out.len() > MAX_ANT_SEGMENTS {
            truncated = true;
            break;
        }
    }
    // Vertical edges: between columns x-1 and x.
    let mut x = b.x;
    while x <= ex && !truncated {
        let mut run: Option<i64> = None;
        let mut y = b.y;
        while y <= ey {
            let edge = y < ey && inside(x.saturating_sub(1), y) != inside(x, y);
            match (edge, run) {
                (true, None) => run = Some(y),
                (false, Some(start)) => {
                    out.push([[x as f32, start as f32], [x as f32, y as f32]]);
                    run = None;
                }
                _ => {}
            }
            y = y.saturating_add(1);
        }
        x = x.saturating_add(1);
        if out.len() > MAX_ANT_SEGMENTS {
            truncated = true;
        }
    }
    (out, truncated)
}

impl Viewer {
    /// The editing state (tool, combine mode, brush settings).
    pub fn edit(&self) -> &EditState {
        &self.edit
    }

    pub fn set_tool(&mut self, tool: Tool) {
        self.edit.tool = tool;
        self.edit.gesture.clear();
        self.edit.vector.press = None;
        if !matches!(tool, Tool::Pen | Tool::Curvature) {
            self.edit.vector.pen_layer = None;
            self.edit.vector.pen_vertex = None;
        }
        if tool == Tool::Text {
            self.say("text tool: click the canvas to place a text layer".into());
        }
    }

    pub fn set_combine(&mut self, mode: CombineMode) {
        self.edit.combine = mode;
    }

    pub fn set_brush(&mut self, brush: BrushSettings) {
        self.edit.brush = brush;
    }

    /// The current selection (coverage 0..=65535 in document pixels), `None` = everything.
    pub fn selection(&self) -> Option<&CoveragePlane> {
        self.edit.selection.as_ref()
    }

    /// Number of marching-ants segments of the current selection outline.
    pub fn ant_segments(&self) -> usize {
        self.edit.ants.len()
    }

    pub(super) fn canvas(&self) -> Result<Rect, String> {
        let snap = self.run.snapshot().ok_or("nothing open")?;
        let board = snap.document().artboards.first().ok_or("the document has no artboard")?;
        let b = [board.bounds.x.value, board.bounds.y.value, board.bounds.width.value, board.bounds.height.value];
        if !b.iter().all(|v| v.is_finite() && v.fract() == 0.0 && v.abs() < f64::from(u32::MAX)) || b[2] < 1.0 || b[3] < 1.0 {
            return Err(format!("artboard bounds {b:?} are not whole pixels"));
        }
        Ok(Rect { x: b[0] as i64, y: b[1] as i64, width: b[2] as u32, height: b[3] as u32 })
    }

    fn apply_selection(&mut self, label: &str, new: Result<CoveragePlane, selection::SelectionError>) -> Result<(), String> {
        let new = new.map_err(|e| format!("{label}: {}", e.code()))?;
        let ctx = SelectionContext::new(self.canvas()?);
        let combined = selection::combine(self.edit.selection.as_ref(), &new, self.edit.combine, &ctx.limits).map_err(|e| format!("{label}: {}", e.code()))?;
        let empty = combined.data.iter().all(|v| *v == 0);
        let (ants, truncated) = if empty { (Vec::new(), false) } else { outline(&combined) };
        self.edit.ants = ants;
        self.edit.ants_truncated = truncated;
        self.say(format!("{label} {} -> selection {:?}{}", combine_key(self.edit.combine), combined.bounds, if empty { " (empty)" } else { "" }));
        self.edit.selection = (!empty).then_some(combined);
        Ok(())
    }

    /// Rectangular marquee in document pixels, combined with the current mode.
    pub fn select_rect(&mut self, x: f64, y: f64, width: f64, height: f64) -> Result<(), String> {
        let ctx = SelectionContext::new(self.canvas()?);
        let plane = selection::rect(RectF { x, y, width, height }, &ctx);
        self.apply_selection("select_rect", plane)
    }

    /// Elliptical marquee inscribed in the rectangle, combined with the current mode.
    pub fn select_ellipse(&mut self, x: f64, y: f64, width: f64, height: f64) -> Result<(), String> {
        let ctx = SelectionContext::new(self.canvas()?);
        let plane = selection::ellipse(RectF { x, y, width, height }, &ctx);
        self.apply_selection("select_ellipse", plane)
    }

    /// Freehand / polygonal lasso (implicitly closed, even-odd), combined with the current mode.
    pub fn select_lasso(&mut self, points: &[[f64; 2]]) -> Result<(), String> {
        let ctx = SelectionContext::new(self.canvas()?);
        let plane = selection::polygon(points, FillRule::EvenOdd, &ctx);
        self.apply_selection("select_lasso", plane)
    }

    /// Installs a selection plane as it was saved (no combine), with its outline.
    pub(super) fn restore_selection(&mut self, plane: CoveragePlane) {
        let empty = plane.data.iter().all(|v| *v == 0);
        let (ants, truncated) = if empty { (Vec::new(), false) } else { outline(&plane) };
        self.edit.ants = ants;
        self.edit.ants_truncated = truncated;
        self.say(format!("selection restored {:?}", plane.bounds));
        self.edit.selection = (!empty).then_some(plane);
    }

    pub fn clear_selection(&mut self) {
        self.edit.selection = None;
        self.edit.ants.clear();
        self.edit.ants_truncated = false;
        self.say("selection cleared".into());
    }

    /// Paints one stroke on the raster layer `layer_id` with the current brush, clipped to the
    /// selection; commits it as an undoable chronicle `tiles` edit. Returns the new revision.
    pub fn paint(&mut self, layer_id: &str, samples: &[StrokeSample]) -> Result<u64, String> {
        let result = self.paint_inner(layer_id, samples);
        if let Err(e) = &result {
            self.say(e.clone());
        }
        result
    }

    fn paint_inner(&mut self, layer_id: &str, samples: &[StrokeSample]) -> Result<u64, String> {
        let snap = self.run.snapshot().ok_or("paint: nothing open")?;
        let doc = snap.document();
        let layer = doc.layers.iter().find(|l| l.layer_id == layer_id).ok_or_else(|| format!("paint: unknown layer {layer_id}"))?;
        let Payload::Raster { tiles } = &layer.payload else {
            return Err(format!("paint: {layer_id} is not a raster layer ({})", layer.kind));
        };
        let tiles = tiles.clone();
        let rects = tile_rects(&tiles).ok_or("paint: tile sizes are not whole pixels up to 256")?;
        // New tiles go on the importers' 256-pixel grid, so only an empty layer or one on that
        // grid grows; a layer on another grid paints inside its existing tiles only.
        let on_cell_grid = tiles.iter().all(|t| t.width.value == CELL as f64 && t.height.value == CELL as f64);
        let profile = new_tile_profile(doc, &tiles);
        let canvas = self.canvas()?;
        let area = rects.iter().copied().fold(canvas, union);
        let b = self.edit.brush;
        let tip = BrushTip::round(b.diameter_px, b.hardness_pct, b.spacing_pct);
        // Pen pressure scales the tip size (mouse samples carry pressure 1).
        let dynamics = BrushDynamics {
            size: Channel { base_value: 100.0, minimum: 0.0, jitter: 0.0, control: Control::PenPressure },
            opacity: Channel { base_value: b.opacity_pct, ..Channel::off() },
            flow: Channel::off(),
        };
        let limits = BrushLimits::default();
        let dabs = place_dabs(&tip, &dynamics, samples, &limits).map_err(|e| format!("paint: {}", e.code()))?;
        let mut coverage = stroke_coverage(&dabs, tip.hardness_pct, area, &limits).map_err(|e| format!("paint: {}", e.code()))?;
        if let Some(sel) = &self.edit.selection {
            coverage = clip_to_selection(&coverage, sel, &limits).map_err(|e| format!("paint: {}", e.code()))?;
        }
        let colour = b.rgb8.map(linear);
        let document = DomainId::parse(&doc.document_id).map_err(|_| "paint: document id")?;
        let layer_domain = DomainId::parse(layer_id).map_err(|_| "paint: layer id")?;
        let actor = self.run.actor().clone();
        let mut edited = tiles.clone();
        let mut staged = Vec::new();
        for (i, (tile, rect)) in tiles.iter().zip(&rects).enumerate() {
            if rect.intersect(coverage.bounds).ok().flatten().is_none() {
                continue;
            }
            let before = match self.assets.get(&tile.content_digest.digest) {
                Some(bytes) => bytes.clone(),
                None => self.run.render_assets().map_err(|e| format!("paint: tile bytes {e}"))?.remove(&tile.content_digest.digest).ok_or("paint: tile bytes missing")?,
            };
            let target = MixTarget { document: &document, layer: &layer_domain, profile: &DomainId::parse(&tile.colour_profile_id).map_err(|_| "paint: tile colour profile id")?, actor: &actor };
            let Some(after) = brush_mix::paint_tile(&target, tile, &before, *rect, &coverage, colour).map_err(|e| format!("paint: {e}"))? else {
                continue;
            };
            let digest = self.run.stage_tile(after.clone()).map_err(|e| format!("paint: stage {e}"))?;
            staged.push((digest.digest.clone(), after));
            if let Some(slot) = edited.get_mut(i) {
                slot.content_digest = digest;
            }
        }
        // Painting into empty areas creates tiles: every 256-pixel cell the stroke covers that the
        // layer has no tile for starts transparent (colour 0, alpha 0) and is painted the same way.
        let mut created = 0usize;
        if on_cell_grid && let Ok((ex, ey)) = coverage.bounds.end() {
            let cell = i64::try_from(CELL).unwrap_or(256);
            let first = |v: i64| v.checked_div_euclid(cell).unwrap_or(0);
            let last = |v: i64| v.checked_sub(1).and_then(|v| v.checked_div_euclid(cell)).unwrap_or(0);
            let empty = vec![0u8; COLOUR_CELL.saturating_add(ALPHA_CELL)];
            let placeholder = hsk_studio_folio::ContentDigest { algorithm: "sha256".into(), digest: "0".repeat(64) };
            for row in first(coverage.bounds.y)..=last(ey) {
                for column in first(coverage.bounds.x)..=last(ex) {
                    if tiles.iter().any(|t| t.column == column && t.row == row) {
                        continue;
                    }
                    let (Some(x), Some(y)) = (column.checked_mul(cell), row.checked_mul(cell)) else {
                        continue;
                    };
                    let rect = Rect { x, y, width: CELL as u32, height: CELL as u32 };
                    if rect.intersect(coverage.bounds).ok().flatten().is_none() {
                        continue;
                    }
                    let profile = profile.as_deref().ok_or("paint: no colour profile for new tiles (no raster tile and no assigned RGB working space)")?;
                    let mut tile = new_tile(layer_id, column, row, profile, &placeholder, &edited)?;
                    let target = MixTarget { document: &document, layer: &layer_domain, profile: &DomainId::parse(profile).map_err(|_| "paint: colour profile id")?, actor: &actor };
                    let Some(after) = brush_mix::paint_tile(&target, &tile, &empty, rect, &coverage, colour).map_err(|e| format!("paint: {e}"))? else {
                        continue;
                    };
                    let digest = self.run.stage_tile(after.clone()).map_err(|e| format!("paint: stage {e}"))?;
                    tile.content_digest = digest.clone();
                    edited.push(tile);
                    staged.push((digest.digest.clone(), after));
                    created = created.saturating_add(1);
                }
            }
        }
        if staged.is_empty() {
            return Err("paint: the stroke changed no pixel (outside the canvas, the layer tiles or the selection)".into());
        }
        let count = staged.len();
        self.assets.extend(staged);
        if !self.run.set_tiles("paint", layer_id, edited) {
            return Err(format!("paint on {layer_id} refused: {}", self.op_error()));
        }
        let revision = self.run.snapshot().map_or(0, |s| s.document().revision);
        self.dirty = true;
        self.say(format!("painted {} dabs on {layer_id} ({count} tile(s), {created} new, revision {revision})", dabs.len()));
        Ok(revision)
    }

    /// Layer opacity in percent (undoable chronicle edit). Returns the new revision.
    pub fn set_opacity(&mut self, layer_id: &str, percent: f64) -> Result<u64, String> {
        let ok = self.run.set_opacity(layer_id, percent, None);
        self.after_edit(ok, format!("opacity = {percent} on {layer_id}"))
    }

    /// Layer blend mode (StudioBlendMode discriminant; undoable chronicle edit).
    pub fn set_blend_mode(&mut self, layer_id: &str, mode: u16) -> Result<u64, String> {
        let ok = self.run.set_blend_mode(layer_id, mode, None);
        self.after_edit(ok, format!("blend_mode = {mode} on {layer_id}"))
    }

    /// Undoes the newest step (appearance, visibility, name or paint).
    pub fn undo(&mut self) -> Result<u64, String> {
        let ok = self.run.undo();
        self.after_edit(ok, "undo".into())
    }

    /// Re-applies the newest undone step.
    pub fn redo(&mut self) -> Result<u64, String> {
        let ok = self.run.redo();
        self.after_edit(ok, "redo".into())
    }

    pub(super) fn after_edit(&mut self, ok: bool, what: String) -> Result<u64, String> {
        if ok {
            let revision = self.run.snapshot().map_or(0, |s| s.document().revision);
            self.dirty = true;
            self.say(format!("{what} (revision {revision})"));
            Ok(revision)
        } else {
            let e = format!("{what} refused: {}", self.op_error());
            self.say(e.clone());
            Err(e)
        }
    }

    /// Structured editing state for agents (part of [`Viewer::state_json`]).
    pub fn edit_json(&self) -> Value {
        let e = &self.edit;
        json!({
            "tool": e.tool.key(),
            "tools": Tool::ALL.iter().map(|t| t.key()).collect::<Vec<_>>(),
            "combine": combine_key(e.combine),
            "brush": {"diameter_px": e.brush.diameter_px, "hardness_pct": e.brush.hardness_pct, "spacing_pct": e.brush.spacing_pct, "opacity_pct": e.brush.opacity_pct, "rgb8": e.brush.rgb8},
            "selection": e.selection.as_ref().map(|s| json!({"bounds": [s.bounds.x, s.bounds.y, s.bounds.width, s.bounds.height], "coverage_sum": s.area_sum(), "ant_segments": e.ants.len(), "ants_truncated": e.ants_truncated})),
            "history": self.run.history(),
            "text_tool": {"available": true, "size_pt": self.text_size_pt, "fonts": self.fonts.iter().map(|f| f.family.clone()).collect::<Vec<_>>()},
            "vector": self.vector_json(),
            "fill": self.fill_tools_json(),
        })
    }
}
