//! Magic wand, quick selection, magnetic lasso and the liquify brush in the viewer (lanes
//! SMB-PS-SELECT-LIQUIFY and -2), driven by hsk-studio-pigment `select_tools` and `liquify`.
//! Headless [`Viewer`] methods first (model-navigable, recorded in the status log); the egui tools
//! (`tool.magic_wand`, `tool.quick_selection`, `tool.magnetic_lasso`, `tool.liquify`) and their
//! option row (`ps.*` ids) only call these methods: a wand click, a quick-select / magnetic-lasso /
//! liquify drag ([`Viewer::ps_click`], [`Viewer::ps_gesture`]).
//!
//! - [`Viewer::select_wand`] / [`Viewer::select_quick`] read the layer's tiles over the canvas in
//!   display encoding (linear-light tiles encoded with the sRGB transfer, so tolerance is in 8-bit
//!   display levels; Studio reading, Photoshop UNVERIFIED), run the pigment tool, combine (wand: the
//!   current combine mode; quick select: add / subtract by stroke) and install the selection with
//!   its marching ants. Each call is one step of the viewer's selection history:
//!   [`Viewer::undo_selection`] restores the selection before it (selections are viewer state, not
//!   chronicle records, as for the marquee tools). `sample_merged` samples the rendered composite
//!   (the frame the viewer shows, sRGB-encoded straight RGBA, rendered first when stale) instead of
//!   one layer, so it sees exactly what is on screen.
//! - [`Viewer::liquify`] applies one brush stroke to a raster layer: an identity mesh over the
//!   layer's tiles, the stroke, pigment's resampler, and the changed tile bodies staged and
//!   committed as ONE chronicle `tiles` edit (undo / redo with [`Viewer::undo`]). The re-editable
//!   mesh form lives in pigment (`liquify::LiquifyMesh`, `smart_filter`); the viewer does not
//!   persist it (Folio has no live_filter record yet).
#![deny(clippy::arithmetic_side_effects)]
use super::Viewer;
use hsk_studio_folio::{Payload, TileRef};
use hsk_studio_pigment::Rect;
use hsk_studio_pigment::brush::StrokeSample;
use hsk_studio_pigment::liquify::{LiquifyBrush, LiquifyLimits, LiquifyMesh, LiquifyTool};
use hsk_studio_pigment::select_tools::ToolLimits;
use hsk_studio_pigment::select_tools::magnetic::{MagneticOptions, magnetic_lasso};
use super::edit::Tool;
use super::widget;
use hsk_studio_pigment::select_tools::quick::{GraphCutOptions, QuickStroke, StrokeMode, quick_select};
use hsk_studio_pigment::select_tools::wand::{WandOptions, magic_wand};
use hsk_studio_pigment::selection::{self, ColourView, CoveragePlane, SelectionContext};
use hsk_studio_pigment::transform_math::ResampleFilter;

/// Selection steps kept for [`Viewer::undo_selection`].
pub const MAX_SELECTION_HISTORY: usize = 64;
/// Largest layer region the tools read (16 bytes per pixel).
pub const MAX_REGION_BYTES: u64 = 256 * 1024 * 1024;
/// Liquify mesh vertex spacing in pixels.
pub const LIQUIFY_SPACING: u32 = 8;

/// Settings of the magic wand, quick select, magnetic lasso and liquify tools.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PsSettings {
    pub wand: WandOptions,
    pub quick_diameter_px: f64,
    /// Wand / quick select / magnetic lasso read the rendered composite instead of the layer.
    pub sample_merged: bool,
    pub magnetic: MagneticOptions,
    pub liquify_tool: LiquifyTool,
    pub liquify_brush: LiquifyBrush,
}
impl Default for PsSettings {
    fn default() -> Self {
        Self {
            wand: WandOptions::default(),
            quick_diameter_px: 12.0,
            sample_merged: false,
            magnetic: MagneticOptions::default(),
            liquify_tool: LiquifyTool::ForwardWarp,
            liquify_brush: LiquifyBrush::default(),
        }
    }
}

/// Widget ids of the option row.
pub const WAND_TOLERANCE: &str = "ps.wand_tolerance";
pub const WAND_CONTIGUOUS: &str = "ps.wand_contiguous";
pub const SAMPLE_MERGED: &str = "ps.sample_merged";
pub const QUICK_SIZE: &str = "ps.quick_size";
pub const MAGNETIC_WIDTH: &str = "ps.magnetic_width";
pub const LIQUIFY_TOOL: &str = "ps.liquify_tool";
pub const LIQUIFY_SIZE: &str = "ps.liquify_size";
pub const LIQUIFY_PRESSURE: &str = "ps.liquify_pressure";

/// Hover text of the tools.
pub fn tool_hover(tool: Tool) -> &'static str {
    match tool {
        Tool::MagicWand => "Click to select similar colour (tolerance, contiguous, sample merged below; combine mode applies)",
        Tool::QuickSelect => "Paint to grow the selection to the region (combine 'diminish' subtracts)",
        Tool::MagneticLasso => "Trace along an edge; the boundary snaps to it (width below)",
        Tool::Liquify => "Drag on the selected raster layer to warp it (tool and brush below)",
        _ => "",
    }
}

const LIQUIFY_TOOLS: [(&str, LiquifyTool); 9] = [
    ("forward warp", LiquifyTool::ForwardWarp),
    ("push left", LiquifyTool::PushLeft),
    ("twirl cw", LiquifyTool::Twirl { clockwise: true }),
    ("twirl ccw", LiquifyTool::Twirl { clockwise: false }),
    ("pucker", LiquifyTool::Pucker),
    ("bloat", LiquifyTool::Bloat),
    ("reconstruct", LiquifyTool::Reconstruct),
    ("smooth", LiquifyTool::Smooth),
    ("turbulence", LiquifyTool::Turbulence { seed: 1 }),
];

/// IEC 61966-2-1 encode of linear light (clamped to `[0, 1]`).
fn encode_srgb(v: f32) -> f32 {
    let c = v.clamp(0.0, 1.0);
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// A layer's pixels over a region: `rgb_f32le` colour and `f32le` alpha, row-major.
struct LayerPixels {
    colour: Vec<u8>,
    alpha: Vec<u8>,
    tiles: Vec<TileRef>,
    rects: Vec<Rect>,
}

impl Viewer {
    fn tool_canvas(&self) -> Result<Rect, String> {
        let snap = self.run.snapshot().ok_or("nothing open")?;
        let board = snap.document().artboards.first().ok_or("the document has no artboard")?;
        let b = [board.bounds.x.value, board.bounds.y.value, board.bounds.width.value, board.bounds.height.value];
        if !b.iter().all(|v| v.is_finite() && v.fract() == 0.0 && v.abs() < f64::from(u32::MAX)) || b[2] < 1.0 || b[3] < 1.0 {
            return Err(format!("artboard bounds {b:?} are not whole pixels"));
        }
        Ok(Rect { x: b[0] as i64, y: b[1] as i64, width: b[2] as u32, height: b[3] as u32 })
    }

    /// The raster layer's tile bodies copied into `region` (transparent where no tile is).
    fn layer_pixels(&self, label: &str, layer_id: &str, region: Option<Rect>) -> Result<(Rect, LayerPixels), String> {
        let snap = self.run.snapshot().ok_or_else(|| format!("{label}: nothing open"))?;
        let doc = snap.document();
        let layer = doc.layers.iter().find(|l| l.layer_id == layer_id).ok_or_else(|| format!("{label}: unknown layer {layer_id}"))?;
        let Payload::Raster { tiles } = &layer.payload else {
            return Err(format!("{label}: {layer_id} is not a raster layer ({})", layer.kind));
        };
        let px = |l: &hsk_studio_folio::Length| (l.value.is_finite() && l.value >= 1.0 && l.value <= 4096.0 && l.value.fract() == 0.0).then_some(l.value as u32);
        let (mut gw, mut gh) = (0u32, 0u32);
        for t in tiles {
            gw = gw.max(px(&t.width).ok_or_else(|| format!("{label}: tile size"))?);
            gh = gh.max(px(&t.height).ok_or_else(|| format!("{label}: tile size"))?);
        }
        let mut rects = Vec::with_capacity(tiles.len());
        for t in tiles {
            rects.push(Rect {
                x: t.column.checked_mul(i64::from(gw)).ok_or_else(|| format!("{label}: tile placement"))?,
                y: t.row.checked_mul(i64::from(gh)).ok_or_else(|| format!("{label}: tile placement"))?,
                width: px(&t.width).unwrap_or(1),
                height: px(&t.height).unwrap_or(1),
            });
        }
        let region = match region {
            Some(r) => r,
            None => {
                let mut it = rects.iter().copied();
                let first = it.next().ok_or_else(|| format!("{label}: {layer_id} has no tiles"))?;
                it.fold(first, |a, b| {
                    let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
                    let x1 = a.x.saturating_add(i64::from(a.width)).max(b.x.saturating_add(i64::from(b.width)));
                    let y1 = a.y.saturating_add(i64::from(a.height)).max(b.y.saturating_add(i64::from(b.height)));
                    Rect { x: x0, y: y0, width: u32::try_from(x1.saturating_sub(x0)).unwrap_or(u32::MAX), height: u32::try_from(y1.saturating_sub(y0)).unwrap_or(u32::MAX) }
                })
            }
        };
        let n = region.area();
        if n.checked_mul(16).is_none_or(|b| b > MAX_REGION_BYTES) {
            return Err(format!("{label}: budget_exceeded (region {region:?})"));
        }
        let n = usize::try_from(n).map_err(|_| format!("{label}: overflow"))?;
        let rw = region.width as usize;
        let mut colour = vec![0u8; n.saturating_mul(12)];
        let mut alpha = vec![0u8; n.saturating_mul(4)];
        let mut fetched = None;
        for (t, r) in tiles.iter().zip(&rects) {
            let Some(hit) = r.intersect(region).ok().flatten() else {
                continue;
            };
            let body = match self.assets.get(&t.content_digest.digest) {
                Some(b) => b.clone(),
                None => {
                    if fetched.is_none() {
                        fetched = Some(self.run.render_assets().map_err(|e| format!("{label}: tile bytes {e}"))?);
                    }
                    fetched.as_ref().and_then(|m| m.get(&t.content_digest.digest)).cloned().ok_or_else(|| format!("{label}: tile bytes missing"))?
                }
            };
            let (tw, th) = (r.width as usize, r.height as usize);
            let colour_len = tw.saturating_mul(th).saturating_mul(12);
            if body.len() != colour_len.saturating_add(tw.saturating_mul(th).saturating_mul(4)) {
                return Err(format!("{label}: tile body is {} bytes", body.len()));
            }
            let (hx, hy) = hit.end().map_err(|e| format!("{label}: {e:?}"))?;
            for y in hit.y..hy {
                for x in hit.x..hx {
                    let ti = (y.saturating_sub(r.y) as usize).saturating_mul(tw).saturating_add(x.saturating_sub(r.x) as usize);
                    let ri = (y.saturating_sub(region.y) as usize).saturating_mul(rw).saturating_add(x.saturating_sub(region.x) as usize);
                    let (tc, rc) = (ti.saturating_mul(12), ri.saturating_mul(12));
                    let (ta, ra) = (colour_len.saturating_add(ti.saturating_mul(4)), ri.saturating_mul(4));
                    if let (Some(dst), Some(src)) = (colour.get_mut(rc..rc.saturating_add(12)), body.get(tc..tc.saturating_add(12))) {
                        dst.copy_from_slice(src);
                    }
                    if let (Some(dst), Some(src)) = (alpha.get_mut(ra..ra.saturating_add(4)), body.get(ta..ta.saturating_add(4))) {
                        dst.copy_from_slice(src);
                    }
                }
            }
        }
        Ok((region, LayerPixels { colour, alpha, tiles: tiles.clone(), rects }))
    }

    /// Display-encoded copy of linear `rgb_f32le` colour bytes.
    fn encoded(colour: &[u8]) -> Vec<u8> {
        colour
            .chunks_exact(4)
            .flat_map(|c| encode_srgb(f32::from_le_bytes([c[0], c[1], c[2], c[3]])).to_le_bytes())
            .collect()
    }

    fn install_selection(&mut self, label: &str, plane: CoveragePlane) {
        if self.selection_history.len() >= MAX_SELECTION_HISTORY {
            self.selection_history.remove(0);
        }
        self.selection_history.push(self.edit.selection.clone());
        if plane.data.iter().all(|v| *v == 0) {
            self.clear_selection();
        } else {
            self.restore_selection(plane);
        }
        let sel = self.edit.selection.as_ref().map(|s| s.bounds);
        self.say(format!("{label} -> selection {sel:?} (undo_selection steps: {})", self.selection_history.len()));
    }

    /// Magic wand on the raster layer `layer_id` at document pixel `(x, y)`, combined with the
    /// current combine mode. One selection-history step.
    pub fn select_wand(&mut self, layer_id: &str, x: i64, y: i64, opts: &WandOptions) -> Result<(), String> {
        let result = self.select_wand_inner(layer_id, x, y, opts);
        if let Err(e) = &result {
            self.say(e.clone());
        }
        result
    }

    /// The sampled image in display encoding: the layer's tiles over the canvas, or with
    /// `merged` the rendered composite frame. Returns `(bounds, colour, alpha)`.
    fn sampled(&mut self, label: &str, layer_id: &str, merged: bool) -> Result<(Rect, Vec<u8>, Vec<u8>), String> {
        let canvas = self.tool_canvas()?;
        if !merged {
            let (region, px) = self.layer_pixels(label, layer_id, Some(canvas))?;
            return Ok((region, Self::encoded(&px.colour), px.alpha));
        }
        self.render_if_dirty();
        let frame = self.frame.as_ref().ok_or_else(|| format!("{label}: sample_merged: no rendered frame"))?;
        let n = (frame.width as usize).saturating_mul(frame.height as usize);
        if frame.rgba8.len() != n.saturating_mul(4) {
            return Err(format!("{label}: sample_merged: the frame lives only in the display texture"));
        }
        let mut colour = Vec::with_capacity(n.saturating_mul(12));
        let mut alpha = Vec::with_capacity(n.saturating_mul(4));
        for p in frame.rgba8.chunks_exact(4) {
            for c in &p[..3] {
                colour.extend_from_slice(&(f32::from(*c) / 255.0).to_le_bytes());
            }
            alpha.extend_from_slice(&(f32::from(p[3]) / 255.0).to_le_bytes());
        }
        Ok((Rect { x: frame.x, y: frame.y, width: frame.width, height: frame.height }, colour, alpha))
    }

    fn select_wand_inner(&mut self, layer_id: &str, x: i64, y: i64, opts: &WandOptions) -> Result<(), String> {
        let canvas = self.tool_canvas()?;
        let (region, colour, alpha) = self.sampled("select_wand", layer_id, opts.sample_merged)?;
        let views = [ColourView { bounds: region, colour: &colour, alpha: &alpha }];
        let ctx = SelectionContext::new(canvas);
        let one = WandOptions { sample_merged: false, ..*opts };
        let plane = magic_wand(&views, 0, (x, y), &one, &ctx, &ToolLimits::default()).map_err(|e| format!("select_wand: {}", e.code()))?;
        let combined = selection::combine(self.edit.selection.as_ref(), &plane, self.edit.combine, &ctx.limits).map_err(|e| format!("select_wand: {}", e.code()))?;
        self.install_selection("select_wand", combined);
        Ok(())
    }

    /// One quick-selection stroke on `layer_id` (Add grows, Subtract removes). One
    /// selection-history step.
    pub fn select_quick(&mut self, layer_id: &str, samples: &[StrokeSample], diameter_px: f64, mode: StrokeMode) -> Result<(), String> {
        self.select_quick_merged(layer_id, samples, diameter_px, mode, false)
    }

    /// [`Viewer::select_quick`] reading the rendered composite when `sample_merged`.
    pub fn select_quick_merged(&mut self, layer_id: &str, samples: &[StrokeSample], diameter_px: f64, mode: StrokeMode, sample_merged: bool) -> Result<(), String> {
        let result = (|| {
            let canvas = self.tool_canvas()?;
            let (region, colour, alpha) = self.sampled("select_quick", layer_id, sample_merged)?;
            let views = [ColourView { bounds: region, colour: &colour, alpha: &alpha }];
            let ctx = SelectionContext::new(canvas);
            let stroke = QuickStroke { samples, diameter_px, mode };
            quick_select(&views, 0, self.edit.selection.as_ref(), &stroke, &GraphCutOptions::default(), &ctx, &ToolLimits::default())
                .map_err(|e| format!("select_quick: {}", e.code()))
        })();
        match result {
            Ok(plane) => {
                self.install_selection("select_quick", plane);
                Ok(())
            }
            Err(e) => {
                self.say(e.clone());
                Err(e)
            }
        }
    }

    /// Magnetic lasso along a traced path on `layer_id` (or the composite with `opts.sample_merged`),
    /// combined with the current combine mode. One selection-history step.
    pub fn select_magnetic(&mut self, layer_id: &str, trace: &[[f64; 2]], opts: &MagneticOptions) -> Result<(), String> {
        let result = (|| {
            let canvas = self.tool_canvas()?;
            let (region, colour, alpha) = self.sampled("select_magnetic", layer_id, opts.sample_merged)?;
            let views = [ColourView { bounds: region, colour: &colour, alpha: &alpha }];
            let ctx = SelectionContext::new(canvas);
            let one = MagneticOptions { sample_merged: false, ..*opts };
            let plane = magnetic_lasso(&views, 0, trace, &one, &ctx, &ToolLimits::default()).map_err(|e| format!("select_magnetic: {}", e.code()))?;
            selection::combine(self.edit.selection.as_ref(), &plane, self.edit.combine, &ctx.limits).map_err(|e| format!("select_magnetic: {}", e.code()))
        })();
        match result {
            Ok(plane) => {
                self.install_selection("select_magnetic", plane);
                Ok(())
            }
            Err(e) => {
                self.say(e.clone());
                Err(e)
            }
        }
    }

    /// The layer the pixel tools read: the selected layer, or none needed with sample-merged.
    fn ps_layer(&self, label: &str, merged: bool) -> Result<String, String> {
        match (&self.selected, merged) {
            (Some(l), _) => Ok(l.clone()),
            (None, true) => Ok(String::new()),
            (None, false) => Err(format!("{label}: select a raster layer in the layer panel first (or turn on sample merged)")),
        }
    }

    /// Replaces the magic wand / quick select / magnetic lasso / liquify settings.
    pub fn set_ps(&mut self, settings: PsSettings) {
        self.edit.ps = settings;
    }

    /// A canvas click of the magic wand at document point `(x, y)` with the current settings.
    pub fn ps_click(&mut self, x: f64, y: f64) -> Result<(), String> {
        let s = self.edit.ps;
        let opts = WandOptions { sample_merged: s.sample_merged, ..s.wand };
        let layer = self.ps_layer("select_wand", s.sample_merged).inspect_err(|e| self.say(e.clone()))?;
        self.select_wand(&layer, x.floor() as i64, y.floor() as i64, &opts)
    }

    /// A finished canvas drag of a quick-select, magnetic-lasso or liquify tool.
    pub fn ps_gesture(&mut self, tool: Tool, samples: &[StrokeSample]) -> Result<(), String> {
        let s = self.edit.ps;
        match tool {
            Tool::QuickSelect => {
                let layer = self.ps_layer("select_quick", s.sample_merged).inspect_err(|e| self.say(e.clone()))?;
                let mode = if self.edit.combine == hsk_studio_pigment::selection::CombineMode::Diminish { StrokeMode::Subtract } else { StrokeMode::Add };
                self.select_quick_merged(&layer, samples, s.quick_diameter_px, mode, s.sample_merged)
            }
            Tool::MagneticLasso => {
                let layer = self.ps_layer("select_magnetic", s.sample_merged).inspect_err(|e| self.say(e.clone()))?;
                let trace: Vec<[f64; 2]> = samples.iter().map(|p| [p.x, p.y]).collect();
                self.select_magnetic(&layer, &trace, &MagneticOptions { sample_merged: s.sample_merged, ..s.magnetic })
            }
            Tool::Liquify => {
                let layer = self.ps_layer("liquify", false).inspect_err(|e| self.say(e.clone()))?;
                self.liquify(&layer, s.liquify_tool, samples, &s.liquify_brush).map(|_| ())
            }
            _ => Ok(()),
        }
    }

    /// Option row of the four tools (stable `ps.*` ids), shown in the tool row.
    pub(super) fn ps_options_ui(&mut self, ui: &mut egui::Ui, drawn: &mut Vec<String>) {
        let mut s = self.edit.ps;
        ui.separator();
        ui.push_id(widget::id(WAND_TOLERANCE), |ui| ui.add(egui::Slider::new(&mut s.wand.tolerance, 0.0..=255.0).text("tolerance")));
        drawn.push(WAND_TOLERANCE.into());
        ui.push_id(widget::id(WAND_CONTIGUOUS), |ui| ui.checkbox(&mut s.wand.contiguous, "contiguous"));
        drawn.push(WAND_CONTIGUOUS.into());
        ui.push_id(widget::id(SAMPLE_MERGED), |ui| ui.checkbox(&mut s.sample_merged, "sample merged"));
        drawn.push(SAMPLE_MERGED.into());
        ui.push_id(widget::id(QUICK_SIZE), |ui| ui.add(egui::Slider::new(&mut s.quick_diameter_px, 1.0..=500.0).logarithmic(true).text("quick size")));
        drawn.push(QUICK_SIZE.into());
        ui.push_id(widget::id(MAGNETIC_WIDTH), |ui| ui.add(egui::Slider::new(&mut s.magnetic.width, 0.0..=100.0).text("edge width")));
        drawn.push(MAGNETIC_WIDTH.into());
        let current = LIQUIFY_TOOLS.iter().find(|(_, t)| *t == s.liquify_tool).map_or("forward warp", |(n, _)| *n);
        ui.push_id(widget::id(LIQUIFY_TOOL), |ui| {
            egui::ComboBox::from_label("liquify").selected_text(current).show_ui(ui, |ui| {
                for (name, t) in LIQUIFY_TOOLS {
                    ui.selectable_value(&mut s.liquify_tool, t, name);
                }
            });
        });
        drawn.push(LIQUIFY_TOOL.into());
        ui.push_id(widget::id(LIQUIFY_SIZE), |ui| ui.add(egui::Slider::new(&mut s.liquify_brush.size, 1.0..=1000.0).logarithmic(true).text("liquify size")));
        drawn.push(LIQUIFY_SIZE.into());
        ui.push_id(widget::id(LIQUIFY_PRESSURE), |ui| ui.add(egui::Slider::new(&mut s.liquify_brush.pressure, 0.0..=100.0).text("pressure")));
        drawn.push(LIQUIFY_PRESSURE.into());
        self.edit.ps = s;
    }

    /// Restores the selection before the newest wand / quick-select step.
    pub fn undo_selection(&mut self) -> Result<(), String> {
        let previous = self.selection_history.pop().ok_or("undo_selection: no selection step to undo")?;
        match previous {
            Some(plane) => self.restore_selection(plane),
            None => self.clear_selection(),
        }
        Ok(())
    }

    /// Number of selection steps [`Viewer::undo_selection`] can undo.
    pub fn selection_steps(&self) -> usize {
        self.selection_history.len()
    }

    /// One liquify brush stroke on the raster layer `layer_id`, committed as one undoable
    /// chronicle `tiles` edit. Returns the new revision.
    pub fn liquify(&mut self, layer_id: &str, tool: LiquifyTool, samples: &[StrokeSample], brush: &LiquifyBrush) -> Result<u64, String> {
        let result = self.liquify_inner(layer_id, tool, samples, brush);
        if let Err(e) = &result {
            self.say(e.clone());
        }
        result
    }

    fn liquify_inner(&mut self, layer_id: &str, tool: LiquifyTool, samples: &[StrokeSample], brush: &LiquifyBrush) -> Result<u64, String> {
        let (region, px) = self.layer_pixels("liquify", layer_id, None)?;
        let limits = LiquifyLimits::default();
        let mut mesh = LiquifyMesh::new(region, LIQUIFY_SPACING, &limits).map_err(|e| format!("liquify: {}", e.code()))?;
        mesh.apply_stroke(tool, samples, brush, &limits).map_err(|e| format!("liquify: {}", e.code()))?;
        if mesh.is_identity() {
            return Err("liquify: the stroke moved no pixel".into());
        }
        let view = ColourView { bounds: region, colour: &px.colour, alpha: &px.alpha };
        let (colour, alpha) = mesh.render(&view, ResampleFilter::Bilinear, &limits).map_err(|e| format!("liquify: {}", e.code()))?;
        let rw = region.width as usize;
        let mut edited = px.tiles.clone();
        let mut staged = Vec::new();
        for (i, r) in px.rects.iter().enumerate() {
            let (tw, th) = (r.width as usize, r.height as usize);
            let colour_len = tw.saturating_mul(th).saturating_mul(12);
            let mut body = vec![0u8; colour_len.saturating_add(tw.saturating_mul(th).saturating_mul(4))];
            let mut changed = false;
            let (ex, ey) = r.end().map_err(|e| format!("liquify: {e:?}"))?;
            for y in r.y..ey {
                for x in r.x..ex {
                    let ti = (y.saturating_sub(r.y) as usize).saturating_mul(tw).saturating_add(x.saturating_sub(r.x) as usize);
                    let ri = (y.saturating_sub(region.y) as usize).saturating_mul(rw).saturating_add(x.saturating_sub(region.x) as usize);
                    let (tc, rc) = (ti.saturating_mul(12), ri.saturating_mul(12));
                    let (ta, ra) = (colour_len.saturating_add(ti.saturating_mul(4)), ri.saturating_mul(4));
                    if let (Some(dst), Some(new), Some(old)) = (body.get_mut(tc..tc.saturating_add(12)), colour.get(rc..rc.saturating_add(12)), px.colour.get(rc..rc.saturating_add(12))) {
                        dst.copy_from_slice(new);
                        changed |= new != old;
                    }
                    if let (Some(dst), Some(new), Some(old)) = (body.get_mut(ta..ta.saturating_add(4)), alpha.get(ra..ra.saturating_add(4)), px.alpha.get(ra..ra.saturating_add(4))) {
                        dst.copy_from_slice(new);
                        changed |= new != old;
                    }
                }
            }
            if !changed {
                continue;
            }
            let digest = self.run.stage_tile(body.clone()).map_err(|e| format!("liquify: stage {e}"))?;
            staged.push((digest.digest.clone(), body));
            if let Some(slot) = edited.get_mut(i) {
                slot.content_digest = digest;
            }
        }
        if staged.is_empty() {
            return Err("liquify: the stroke changed no pixel".into());
        }
        let count = staged.len();
        self.assets.extend(staged);
        if !self.run.set_tiles("liquify", layer_id, edited) {
            return Err(format!("liquify on {layer_id} refused: {}", self.op_error()));
        }
        let revision = self.run.snapshot().map_or(0, |s| s.document().revision);
        self.dirty = true;
        self.say(format!("liquify {tool:?} on {layer_id} ({count} tile(s), revision {revision})"));
        Ok(revision)
    }
}
