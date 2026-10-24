//! Spot-removal section of the develop panel (lane SMB-LR-HEALING-2; STU-RAW-015 "a heal/clone
//! spot-removal tool with source-point control", STU-RAW-124 retouch areas, STU-RAS-143 core
//! distraction removal). Panel-local: the section shows a preview of the window's rendered frame
//! (the developed `Background` layer) and every gesture on it becomes one of the typed
//! [`DevelopOp`]s, i.e. one undo step through the develop re-develop + tiles commit.
//!
//! Gestures (Lightroom's spot tool is the reference, UNVERIFIED, D5):
//! - click: a spot of the current mode at the pointer with [`DEFAULT_RADIUS`];
//! - drag from an empty point: a spot centred at the press point, radius = drag length;
//! - drag a source handle, or Alt + drag from inside a spot: moves that spot's source point.
//!
//! Declared readings: preview coordinates are normalised to the rendered frame (the developed
//! frame after the crop, the frame the develop retouch stage normalises to); a radius is a fraction
//! of the frame's shorter side (develop `local.rs`). A clicked or dragged heal / clone spot gets an
//! explicit source 2.5 radii to the right (else left, below, above) when that circle lies inside the
//! frame, otherwise an engine-chosen source. Distraction spots are one-dab holes, seed 0.
#![deny(clippy::arithmetic_side_effects)]

use super::super::Viewer;
use super::super::widget;
use super::{DevelopEdit, DevelopOp};
use hsk_studio_develop::{RetouchStack, SourceState, SpotType};
use serde_json::{Value, json};

/// Radius (fraction of the shorter side) of a clicked spot.
pub const DEFAULT_RADIUS: f64 = 0.05;
/// Smallest radius a drag places.
pub const MIN_RADIUS: f64 = 0.005;
/// Longest side of the preview in points.
const PREVIEW_MAX_SIDE: f32 = 280.0;
/// Longest side of the preview texture in pixels (nearest-neighbour reduction of the frame).
const PREVIEW_TEXTURE_MAX: usize = 512;
/// Hit radius of a source handle in points (at least the source circle).
const HANDLE_PX: f32 = 8.0;
/// Source offset of an auto-placed source, in radii.
const AUTO_SOURCE_RADII: f64 = 2.5;

/// What a click or drag on the preview places.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpotMode {
    #[default]
    Heal,
    Clone,
    Distraction,
}

impl SpotMode {
    pub const ALL: [Self; 3] = [Self::Heal, Self::Clone, Self::Distraction];

    pub const fn key(self) -> &'static str {
        match self {
            Self::Heal => "heal",
            Self::Clone => "clone",
            Self::Distraction => "distraction",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Gesture {
    NewSpot { origin: egui::Pos2 },
    Source { spot: usize },
}

/// Per-document state of the section.
#[derive(Default)]
pub(in crate::view) struct SpotPanel {
    mode: SpotMode,
    gesture: Option<Gesture>,
    /// Reduced frame bytes and their texture.
    texture: Option<(Vec<u8>, egui::TextureHandle)>,
    /// Screen rectangle of the preview and the frame size it maps, as drawn last.
    rect: Option<egui::Rect>,
    frame: Option<(u32, u32)>,
}

impl SpotPanel {
    /// The section's state for agents (`develop.spot_preview`).
    pub(super) fn json(&self) -> Value {
        json!({
            "mode": self.mode.key(),
            "rect": self.rect.map(|r| [r.min.x, r.min.y, r.max.x, r.max.y]),
            "frame": self.frame.map(|(w, h)| [w, h]),
            "gesture": self.gesture.map(|g| match g {
                Gesture::NewSpot { .. } => "new_spot",
                Gesture::Source { .. } => "source",
            }),
        })
    }
}

/// The retouch areas for agents (`develop.spots`).
pub(super) fn spots_json(stack: &RetouchStack) -> Value {
    Value::Array(
        stack
            .areas
            .iter()
            .enumerate()
            .map(|(i, a)| {
                json!({
                    "index": i, "spot_type": a.spot_type.key(), "centre": [a.centre_x, a.centre_y], "radius": a.radius,
                    "feather": a.feather, "opacity": a.opacity, "source": [a.source_x, a.source_y],
                    "source_state": a.source_state.key(), "masks": a.masks.len(),
                })
            })
            .collect(),
    )
}

/// The distraction areas for agents (`develop.distraction`).
pub(super) fn distraction_json(stack: &RetouchStack) -> Value {
    Value::Array(
        stack
            .distraction
            .iter()
            .enumerate()
            .map(|(i, d)| {
                let first = d.masks.first();
                json!({
                    "index": i, "centre": first.and_then(|b| b.dabs.first()).map(|(x, y)| [*x, *y]),
                    "radius": first.map(|b| b.radius), "masks": d.masks.len(), "seed": d.seed,
                })
            })
            .collect(),
    )
}

/// An explicit source for a new spot at `centre` (see the module readings), else `None`
/// (engine-chosen).
pub fn auto_source(centre: (f64, f64), radius: f64, frame: (u32, u32)) -> Option<(f64, f64)> {
    let (w, h) = (f64::from(frame.0), f64::from(frame.1));
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let short = w.min(h);
    let (rx, ry) = (radius * short / w, radius * short / h);
    let (dx, dy) = (AUTO_SOURCE_RADII * rx, AUTO_SOURCE_RADII * ry);
    let (x, y) = centre;
    [(x + dx, y), (x - dx, y), (x, y + dy), (x, y - dy)]
        .into_iter()
        .find(|(sx, sy)| *sx - rx >= 0.0 && *sx + rx <= 1.0 && *sy - ry >= 0.0 && *sy + ry <= 1.0)
}

/// The op a placement in `mode` makes.
fn place(mode: SpotMode, centre: (f64, f64), radius: f64, frame: (u32, u32)) -> DevelopOp {
    let spot = |spot_type| DevelopOp::AddSpot { spot_type, centre, radius, source: auto_source(centre, radius, frame), feather: 0.0, opacity: 1.0 };
    match mode {
        SpotMode::Heal => spot(SpotType::Heal),
        SpotMode::Clone => spot(SpotType::Clone),
        SpotMode::Distraction => DevelopOp::AddDistraction { centre, radius },
    }
}

/// Nearest-neighbour reduction of an RGBA8 frame to at most [`PREVIEW_TEXTURE_MAX`] on its long side.
fn reduce(width: u32, height: u32, rgba: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let (w, h) = (usize::try_from(width).ok()?, usize::try_from(height).ok()?);
    if w == 0 || h == 0 || rgba.len() != w.checked_mul(h)?.checked_mul(4)? {
        return None;
    }
    let step = w.max(h).div_ceil(PREVIEW_TEXTURE_MAX).max(1);
    let (tw, th) = (w.div_ceil(step), h.div_ceil(step));
    let mut out = Vec::with_capacity(tw.checked_mul(th)?.checked_mul(4)?);
    for ty in 0..th {
        let y = ty.checked_mul(step)?.min(h.checked_sub(1)?);
        for tx in 0..tw {
            let x = tx.checked_mul(step)?.min(w.checked_sub(1)?);
            let at = y.checked_mul(w)?.checked_add(x)?.checked_mul(4)?;
            out.extend_from_slice(rgba.get(at..at.checked_add(4)?)?);
        }
    }
    Some((tw, th, out))
}

impl Viewer {
    /// The spot-removal section (top of the develop panel): mode buttons, the preview with spot
    /// overlays and gestures, and one row per spot (type switch, remove). Pushes typed ops only.
    pub(super) fn spot_ui(&mut self, ui: &mut egui::Ui, edit: &DevelopEdit, ops: &mut Vec<DevelopOp>) {
        let w = widget::id;
        let Some(d) = self.develop.as_mut() else {
            return;
        };
        let frame = self.frame.as_ref();
        let dims = frame.map(|f| (f.width, f.height)).filter(|(fw, fh)| *fw > 0 && *fh > 0).unwrap_or((d.prepared.window.width, d.prepared.window.height));
        ui.strong("Spot removal");
        ui.horizontal_wrapped(|ui| {
            for mode in SpotMode::ALL {
                let name = format!("develop.spot.mode.{}", mode.key());
                let (text, selected) = (format!("Spot mode {}", mode.key()), d.spot.mode == mode);
                if super::wrap_item(ui, w(&name), &text, egui::TextStyle::Button, |ui| ui.selectable_label(selected, text.as_str())).on_hover_text("What a click or drag on the preview places").clicked() {
                    d.spot.mode = mode;
                }
                self.drawn.push(name);
            }
        });

        // Preview texture: the rendered frame reduced; uploaded again only when the bytes change.
        let texture = match frame {
            Some(f) if f.texture.is_some() => f.texture,
            Some(f) => match reduce(f.width, f.height, &f.rgba8) {
                Some((tw, th, bytes)) => {
                    if d.spot.texture.as_ref().is_none_or(|(old, _)| *old != bytes) {
                        let image = egui::ColorImage::from_rgba_unmultiplied([tw, th], &bytes);
                        let handle = ui.ctx().load_texture("hsk-studio-ui.develop.spot.preview", image, egui::TextureOptions::LINEAR);
                        d.spot.texture = Some((bytes, handle));
                    }
                    d.spot.texture.as_ref().map(|(_, t)| t.id())
                }
                None => None,
            },
            None => None,
        };
        let aspect = dims.1 as f32 / dims.0 as f32;
        let side = ui.available_width().clamp(32.0, PREVIEW_MAX_SIDE);
        let size = if aspect <= 1.0 { egui::vec2(side, side * aspect) } else { egui::vec2(side / aspect, side) };
        let (rect, response) = ui.push_id(w("develop.spot.preview"), |ui| ui.allocate_exact_size(size, egui::Sense::click_and_drag())).inner;
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Spot preview"));
        let response = response.on_hover_text("Click: place a spot. Drag: place with a radius. Drag a source handle or Alt+drag inside a spot: move its source.");
        self.drawn.push("develop.spot.preview".into());
        d.spot.rect = Some(rect);
        d.spot.frame = Some(dims);

        let short = rect.width().min(rect.height()).max(1.0);
        let to_px = |x: f64, y: f64| egui::pos2(rect.min.x + x as f32 * rect.width(), rect.min.y + y as f32 * rect.height());
        let to_norm = |p: egui::Pos2| (f64::from((p.x - rect.min.x) / rect.width()), f64::from((p.y - rect.min.y) / rect.height()));
        let areas = &edit.adjust.retouch.areas;
        let hit_source = |p: egui::Pos2| {
            areas.iter().enumerate().rev().find_map(|(i, a)| {
                let reach = HANDLE_PX.max(a.radius as f32 * short);
                (a.source_state == SourceState::SourceSetExplicitly && to_px(a.source_x, a.source_y).distance(p) <= reach).then_some(i)
            })
        };
        let hit_target = |p: egui::Pos2| areas.iter().enumerate().rev().find_map(|(i, a)| (to_px(a.centre_x, a.centre_y).distance(p) <= a.radius as f32 * short).then_some(i));

        let alt = ui.input(|i| i.modifiers.alt);
        let latest = ui.input(|i| i.pointer.latest_pos());
        if response.drag_started()
            && let Some(origin) = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos())
        {
            let source = hit_source(origin).or_else(|| if alt { hit_target(origin) } else { None });
            d.spot.gesture = Some(match source {
                Some(spot) => Gesture::Source { spot },
                None => Gesture::NewSpot { origin },
            });
        }
        if response.drag_stopped() {
            if let (Some(gesture), Some(p)) = (d.spot.gesture.take(), latest) {
                match gesture {
                    Gesture::Source { spot } => ops.push(DevelopOp::MoveSpotSource { spot, source: to_norm(p) }),
                    Gesture::NewSpot { origin } => {
                        let radius = (f64::from(origin.distance(p) / short)).max(MIN_RADIUS);
                        ops.push(place(d.spot.mode, to_norm(origin), radius, dims));
                    }
                }
            }
        } else if response.clicked()
            && let Some(p) = response.interact_pointer_pos().or(latest)
            && hit_source(p).is_none()
        {
            ops.push(place(d.spot.mode, to_norm(p), DEFAULT_RADIUS, dims));
        }

        // Preview image and overlays: target circle, source circle and link line, distraction holes.
        let painter = ui.painter_at(rect);
        match texture {
            Some(id) => {
                painter.image(id, rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
            }
            None => {
                painter.rect_filled(rect, 0.0, egui::Color32::from_gray(60));
            }
        }
        let target = egui::Stroke::new(1.5_f32, egui::Color32::WHITE);
        let source = egui::Stroke::new(1.5_f32, egui::Color32::YELLOW);
        for a in areas {
            let (c, r) = (to_px(a.centre_x, a.centre_y), a.radius as f32 * short);
            painter.circle_stroke(c, r, target);
            if a.source_state == SourceState::SourceSetExplicitly {
                let s = to_px(a.source_x, a.source_y);
                painter.circle_stroke(s, r, source);
                painter.line_segment([c, s], source);
            }
        }
        for dist in &edit.adjust.retouch.distraction {
            for b in &dist.masks {
                for (x, y) in &b.dabs {
                    painter.circle_stroke(to_px(*x, *y), b.radius as f32 * short, egui::Stroke::new(1.5_f32, egui::Color32::RED));
                }
            }
        }
        if let (Some(Gesture::NewSpot { origin }), Some(p)) = (d.spot.gesture, latest) {
            painter.circle_stroke(origin, origin.distance(p), target);
        }

        // One row per spot: type switch and remove.
        for (i, a) in areas.iter().enumerate() {
            ui.horizontal_wrapped(|ui| {
                let other = if a.spot_type == SpotType::Heal { SpotType::Clone } else { SpotType::Heal };
                let name = format!("develop.spot.{i}.type");
                let text = format!("Spot {i} {} to {}", a.spot_type.key(), other.key());
                if super::wrap_item(ui, w(&name), &text, egui::TextStyle::Body, |ui| ui.small_button(text.as_str())).clicked() {
                    ops.push(DevelopOp::SetSpot { spot: i, spot_type: other, feather: a.feather, opacity: a.opacity, radius: a.radius });
                }
                self.drawn.push(name);
                let name = format!("develop.spot.{i}.remove");
                let text = format!("Remove spot {i}");
                if super::wrap_item(ui, w(&name), &text, egui::TextStyle::Body, |ui| ui.small_button(text.as_str())).clicked() {
                    ops.push(DevelopOp::RemoveSpot(i));
                }
                self.drawn.push(name);
            });
        }
        for i in 0..edit.adjust.retouch.distraction.len() {
            let name = format!("develop.distraction.{i}.remove");
            if ui.push_id(w(&name), |ui| ui.small_button(format!("Remove distraction spot {i}"))).inner.clicked() {
                ops.push(DevelopOp::RemoveDistraction(i));
            }
            self.drawn.push(name);
        }
        ui.separator();
    }
}
