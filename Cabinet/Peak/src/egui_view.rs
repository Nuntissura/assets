//! egui owns window geometry, resize and focus; Peak supplies fenced preview state.
use crate::{Content, Key, Peak, Stage};
use egui::{Rect, TextureHandle};
pub struct PeakView {
    namespace: String,
    texture: Option<TextureHandle>,
    texture_key: Option<Key>,
    last_rect: Option<Rect>,
    last_expanded: bool,
}
impl Default for PeakView {
    fn default() -> Self {
        Self::new("peak")
    }
}
impl PeakView {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            texture: None,
            texture_key: None,
            last_rect: None,
            last_expanded: false,
        }
    }
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        peak: &mut Peak,
        anchor: Rect,
        now_ms: u64,
    ) -> Option<Rect> {
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            peak.dismiss();
        }
        if peak.stage() == Stage::Hidden || !peak.settings.enabled {
            self.texture = None;
            self.texture_key = None;
            self.last_rect = None;
            return None;
        }
        let key = peak.key()?.clone();
        let name = peak.name().to_owned();
        let expanded = peak.stage() == Stage::Expanded;
        if self.texture_key.as_ref() != Some(&key) {
            self.texture = None;
            self.last_rect = None;
            self.last_expanded = false;
            self.texture_key = Some(key.clone());
        }
        if self.texture.is_none() {
            if let Some(Content::Image {
                width,
                height,
                rgba,
            }) = peak.content()
            {
                self.texture = Some(ctx.load_texture(
                    format!(
                        "peak-{}-{}-{}",
                        hex(&self.namespace),
                        hex(&key.resource_id),
                        key.entry_revision
                    ),
                    egui::ColorImage::from_rgba_unmultiplied(
                        [*width as usize, *height as usize],
                        rgba,
                    ),
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
        let screen = ctx.content_rect();
        let width = if expanded {
            peak.settings.width
        } else {
            peak.settings.width.min(320.0)
        }
        .min(screen.width().max(100.0));
        let height = if expanded {
            peak.settings.height
        } else {
            peak.settings.height.min(200.0)
        }
        .min(screen.height().max(60.0));
        let x = anchor
            .right()
            .min((screen.right() - width).max(screen.left()));
        let y = anchor
            .top()
            .min((screen.bottom() - height).max(screen.top()));
        let window = egui::Window::new("Peak")
            .id(egui::Id::new((
                &self.namespace,
                "peak",
                &key.scope,
                &key.entry_id,
                key.generation,
                expanded,
            )))
            .default_pos(egui::pos2(x, y))
            .default_size(egui::vec2(width, height))
            .min_size(egui::vec2(100.0, 60.0))
            .resizable(expanded)
            .title_bar(false)
            .movable(false)
            .collapsible(false)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                if expanded {
                    // Reserve egui's current resize rectangle, including user-sized changes.
                    // default_size alone is only a content wrapping preference.
                    ui.set_min_size(ui.available_size());
                }
                ui.ctx().accesskit_node_builder(ui.id(), |n| {
                    n.set_author_id(format!(
                        "peak.{}.{}.preview",
                        hex(&self.namespace),
                        hex(&key.entry_id)
                    ));
                    n.set_label(format!("Preview of {name}"));
                });
                ui.add(egui::Label::new(&name).wrap());
                ui.horizontal_wrapped(|ui| {
                    for tag in peak.tags() {
                        let color =
                            egui::Color32::from_rgb(tag.color[0], tag.color[1], tag.color[2]);
                        let r = ui.colored_label(color, &tag.label);
                        ui.ctx().accesskit_node_builder(r.id, |n| {
                            n.set_author_id(format!(
                                "peak.{}.{}.tag.{}",
                                hex(&self.namespace),
                                hex(&key.entry_id),
                                hex(&tag.id)
                            ));
                            n.set_label(tag.label.clone());
                        });
                    }
                });
                if expanded {
                    egui::ScrollArea::both().max_height(height).show(ui, |ui| {
                        match peak.content() {
                            Some(Content::Text { body, truncated }) => {
                                ui.add(
                                    egui::Label::new(egui::RichText::new(body).monospace())
                                        .selectable(true),
                                );
                                if *truncated {
                                    ui.weak("Preview truncated");
                                }
                            }
                            Some(Content::Image { .. }) => {
                                if let Some(texture) = &self.texture {
                                    let size = texture.size_vec2();
                                    let scale = (ui.available_width() / size.x)
                                        .min(height / size.y)
                                        .min(1.0);
                                    ui.image((texture.id(), size * scale));
                                }
                            }
                            Some(Content::Unsupported { reason }) => {
                                ui.label(format!("Preview unavailable: {reason}"));
                            }
                            Some(Content::Failed { reason }) => {
                                ui.colored_label(ui.visuals().error_fg_color, reason);
                            }
                            None => {
                                ui.spinner();
                                ui.label("Loading preview…");
                            }
                        }
                    });
                }
            })?;
        let rect = window.response.rect;
        if ctx.input(|i| i.pointer.hover_pos().is_some_and(|p| rect.contains(p))) {
            peak.retain();
        }
        if expanded
            && self.last_expanded
            && self
                .last_rect
                .is_some_and(|previous| (previous.size() - rect.size()).length() > 1.0)
            && ctx.input(|i| i.pointer.any_down())
        {
            peak.settings.width = rect.width().clamp(100.0, 4096.0);
            peak.settings.height = rect.height().clamp(60.0, 4096.0);
        }
        self.last_rect = Some(rect);
        self.last_expanded = expanded;
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
        let _ = now_ms;
        Some(rect)
    }
}
fn hex(s: &str) -> String {
    s.as_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Settings, Tag};
    fn render(ctx: &egui::Context, view: &mut PeakView, peak: &mut Peak) -> Rect {
        render_events(ctx, view, peak, vec![])
    }
    fn render_events(
        ctx: &egui::Context,
        view: &mut PeakView,
        peak: &mut Peak,
        events: Vec<egui::Event>,
    ) -> Rect {
        let mut rect = None;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                rect = view.show(
                    ctx,
                    peak,
                    Rect::from_min_size(egui::pos2(20.0, 40.0), egui::vec2(200.0, 26.0)),
                    2000,
                );
            },
        );
        rect.unwrap()
    }
    #[test]
    fn expansion_uses_fresh_geometry_without_compact_size_cache() {
        let ctx = egui::Context::default();
        let mut view = PeakView::new("geometry-test");
        let mut peak = Peak::new(Settings {
            width: 520.0,
            height: 360.0,
            ..Default::default()
        });
        peak.hover(
            Key {
                scope: "s".into(),
                generation: 1,
                context_revision: 1,
                entry_id: "a".into(),
                resource_id: "a".into(),
                entry_revision: 1,
            },
            "Full file name.rs",
            vec![Tag {
                id: "tag".into(),
                label: "Code".into(),
                color: [40, 150, 220],
            }],
            0,
        );
        assert!(peak.tick(450).is_none());
        let _ = render(&ctx, &mut view, &mut peak);
        let compact = render(&ctx, &mut view, &mut peak);
        assert!(peak.tick(1650).is_some());
        let _ = render(&ctx, &mut view, &mut peak);
        let expanded = render(&ctx, &mut view, &mut peak);
        assert!(
            expanded.width() > compact.width() + 100.0,
            "compact={compact:?}, expanded={expanded:?}"
        );
        let corner = expanded.right_bottom() - egui::vec2(2.0, 2.0);
        render_events(
            &ctx,
            &mut view,
            &mut peak,
            vec![egui::Event::PointerMoved(corner)],
        );
        render_events(
            &ctx,
            &mut view,
            &mut peak,
            vec![egui::Event::PointerButton {
                pos: corner,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let smaller = corner - egui::vec2(90.0, 70.0);
        render_events(
            &ctx,
            &mut view,
            &mut peak,
            vec![egui::Event::PointerMoved(smaller)],
        );
        render_events(&ctx, &mut view, &mut peak, vec![]);
        render_events(
            &ctx,
            &mut view,
            &mut peak,
            vec![egui::Event::PointerButton {
                pos: smaller,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let resized = render(&ctx, &mut view, &mut peak);
        assert!(
            resized.width() < expanded.width() - 40.0,
            "before={expanded:?}, resized={resized:?}"
        );
        assert!(
            (peak.settings.width - resized.width()).abs() < 2.0,
            "settings must retain user resize"
        );
    }
}
