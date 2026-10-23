//! Optional embedded egui wall. Hosts own layout integration, authority, storage,
//! preview textures and actual player construction/disposal.
use std::collections::BTreeMap;

use egui::{Align, Align2, Color32, FontId, Id, Layout, Rect, Ui, Vec2};
use hsscrap::{interaction::Inspection, AvailabilityState, MediaKind, MemberRef};
use hsscrap_gui::{Inspector, ViewAction};
use rss_contracts::{AcquisitionState, Error, HostRef, ItemId, Result};
use rss_core::query::*;

/// Called only for visible rectangles. A host must independently authorize media
/// and honor StopPlayback/InvalidateMedia before presenting any cached content.
pub trait MediaPainter {
    fn preview(&mut self, ui: &mut Ui, rect: Rect, member: &MemberRef) -> bool;
    fn player(&mut self, ui: &mut Ui, rect: Rect, member: &MemberRef) -> bool;
}

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Query(PageTicket),
    Wall(WallIntent),
    Details(ViewAction),
    FocusGroup(Option<HostRef>),
    SetGroupSelected { group: HostRef, selected: bool },
    SetSourceSelected { source: HostRef, selected: bool },
    SetPaused { source: HostRef, paused: bool },
}
#[derive(Default)]
pub struct Response {
    pub actions: Vec<Action>,
    pub controls: BTreeMap<String, Rect>,
    pub error: Option<Error>,
    pub restored_position: Option<RefreshApplied>,
    pub reading_position: Option<ReadingPosition>,
}

pub struct WallView {
    namespace: String,
    manager_open: bool,
    inspector: Option<(InspectionTicket, Inspector)>,
    restore_info: Option<ItemId>,
    focus_details: bool,
    restore_scroll: Option<ReadingPosition>,
}
impl WallView {
    pub fn new(namespace: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            manager_open: false,
            inspector: None,
            restore_info: None,
            focus_details: false,
            restore_scroll: None,
        }
    }
    pub fn inspector(&mut self) -> Option<&mut Inspector> {
        self.inspector.as_mut().map(|(_, inspector)| inspector)
    }

    pub fn complete_inspection(
        &mut self,
        wall: &WallState,
        ticket: &InspectionTicket,
        inspection: Inspection,
    ) -> Result<()> {
        wall.validate_inspection(ticket, &inspection)?;
        let inspector = Inspector::new(format!("{}-details", self.namespace), inspection)?;
        self.inspector = Some((ticket.clone(), inspector));
        self.focus_details = true;
        Ok(())
    }

    fn control(
        &self,
        ui: &Ui,
        response: &egui::Response,
        key: String,
        label: &str,
        out: &mut Response,
    ) {
        out.controls.insert(key.clone(), response.rect);
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_author_id(format!("rss.{}.{}", self.namespace, key));
            node.set_label(label);
        });
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        query: &mut QuerySession,
        wall: &mut WallState,
        manager: &ManagerModel,
        media: &mut impl MediaPainter,
    ) -> Response {
        let mut out = Response::default();
        out.actions
            .extend(wall.reconcile(query).into_iter().map(Action::Wall));
        if wall.inspection().is_none() {
            self.inspector = None;
        }
        ui.push_id(self.namespace.clone(), |ui| {
            if wall.is_revoked() || query.query().is_none() {
                ui.label("Access is no longer available. Refresh authorization before viewing this feed.");
                return;
            }
            ui.spacing_mut().interact_size.y = ui.spacing().interact_size.y.max(40.0);
            ui.set_max_width(ui.available_width());
            let viewport = ui.available_rect_before_wrap();
            ui.horizontal_wrapped(|ui| {
                ui.heading("Feeds");
                let manager_button = ui.button(if self.manager_open { "Close sources" } else { "Sources" });
                self.control(ui, &manager_button, "sources".into(), "Manage sources", &mut out);
                if manager_button.clicked() { self.manager_open = !self.manager_open; }
                let refresh = ui.add_enabled(query.snapshot().is_some() && !query.refreshing() && !query.refresh_available(), egui::Button::new("Refresh"));
                self.control(ui, &refresh, "refresh".into(), "Refresh feeds", &mut out);
                if refresh.clicked() { match query.refresh() { Ok(ticket) => out.actions.push(Action::Query(ticket)), Err(error) => out.error = Some(error) } }
                if query.refreshing() { ui.label("Checking for updates…"); }
                if query.refresh_available() {
                    let apply = ui.button("Apply updates");
                    self.control(ui, &apply, "apply-refresh".into(), "Apply updates", &mut out);
                    if apply.clicked() { match query.apply_refresh() {
                        Ok(position) => {
                            self.restore_scroll = match position { RefreshApplied::Preserved(position) => Some(position), _ => None };
                            out.restored_position = Some(position); out.actions.extend(wall.reconcile(query).into_iter().map(Action::Wall));
                        },
                        Err(error) => out.error = Some(error),
                    } }
                }
            });
            if self.manager_open { self.manager(ui, manager, &mut out); }
            match query.phase() {
                QueryPhase::Idle => { ui.label("Choose sources, then load the feed."); },
                QueryPhase::Loading => { ui.label("Loading feeds…"); },
                QueryPhase::Empty => { ui.label("No items match this feed selection."); },
                QueryPhase::Partial(reason) => { ui.label(format!("Some items are unavailable · {reason}")); },
                QueryPhase::Reconnect => { ui.label("Reconnect this source to continue. Saved items remain available."); },
                QueryPhase::Failed(_) => { ui.label("The feed could not be loaded. Existing items and your position are preserved."); },
                QueryPhase::Revoked => { ui.label("Access is no longer available."); },
                QueryPhase::Ready => {},
            }
            egui::ScrollArea::vertical().id_salt("wall").show(ui, |ui| {
                let gap = 12.0;
                let columns = ((ui.available_width() + gap) / 280.0).floor().max(1.0) as usize;
                let width = ((ui.available_width() - gap * (columns - 1) as f32) / columns as f32).max(1.0);
                let origin = ui.cursor().min;
                let mut bottoms = vec![origin.y; columns];
                for row in query.rows() {
                    let column = (0..columns).min_by(|a,b| bottoms[*a].total_cmp(&bottoms[*b])).unwrap_or(0);
                    let member = wall.selected_member(row.item_id).cloned();
                    let ratio = member.as_ref().and_then(|member| member.observed_width.zip(member.observed_height))
                        .filter(|(w,h)| *w > 0 && *h > 0).map(|(w,h)| h as f32 / w as f32).unwrap_or(0.85);
                    let rect = Rect::from_min_size(egui::pos2(origin.x + column as f32 * (width + gap), bottoms[column]), Vec2::new(width, width * ratio));
                    bottoms[column] = rect.bottom() + gap;
                    if let Some(position) = self.restore_scroll.filter(|position| position.item_id == row.item_id) {
                        let y = rect.top() + rect.height() * f32::from(position.offset_basis_points) / 10_000.0;
                        ui.scroll_to_rect(Rect::from_min_size(egui::pos2(rect.left(),y),Vec2::new(width,1.0)),Some(Align::Min));
                    }
                    if !ui.is_rect_visible(rect) { continue; }
                    if out.reading_position.is_none() {
                        let offset = ((ui.clip_rect().top() - rect.top()).max(0.0) / rect.height()).clamp(0.0,1.0);
                        out.reading_position = Some(ReadingPosition { item_id: row.item_id, offset_basis_points: (offset * 10_000.0) as u16 });
                    }
                        let mut card_ui = ui.new_child(egui::UiBuilder::new().id_salt(row.item_id).max_rect(rect));
                        card_ui.set_clip_rect(rect.intersect(ui.clip_rect()));
                        self.card(&mut card_ui, rect, row.item_id, query, wall, media, &mut out);
                }
                let bottom = bottoms.into_iter().fold(origin.y, f32::max);
                ui.allocate_space(Vec2::new(ui.available_width(), (bottom - origin.y).max(1.0)));
                if query.loading() { ui.label("Loading more…"); }
                let more = ui.add_enabled(query.has_next() && !query.loading(), egui::Button::new("Load more"));
                self.control(ui, &more, "more".into(), "Load more feed items", &mut out);
                if more.clicked() { match query.load_next() { Ok(ticket) => out.actions.push(Action::Query(ticket)), Err(error) => out.error = Some(error) } }
            });
            if self.restore_scroll.take().is_none() {
                if let Some(position) = out.reading_position { let _ = query.set_position(position); }
            }
            if let Some(error) = &out.error { ui.label(format!("Action could not complete: {error:?}")); }
            if self.inspector.is_some() { self.overlay(ui, viewport, wall, &mut out); }
        });
        out
    }

    fn manager(&self, ui: &mut Ui, model: &ManagerModel, out: &mut Response) {
        egui::ScrollArea::vertical()
            .id_salt("source-manager")
            .max_height((ui.available_height() * 0.55).min(480.0))
            .show(ui, |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.heading("Sources and collections");
                    ui.label(
                        "Changes apply after the owning service confirms the canonical selection.",
                    );
                    let all = ui.button("All selected collections");
                    if all.clicked() {
                        out.actions.push(Action::FocusGroup(None));
                    }
                    for group in &model.groups {
                        ui.push_id(("group", &group.group.group_ref), |ui| {
                            ui.horizontal_wrapped(|ui| {
                                let mut selected = group.selected;
                                let checkbox = ui.checkbox(&mut selected, &group.group.name);
                                self.control(
                                    ui,
                                    &checkbox,
                                    reference_key("group", &group.group.group_ref),
                                    &format!("Select collection {}", group.group.name),
                                    out,
                                );
                                if checkbox.changed() {
                                    out.actions.push(Action::SetGroupSelected {
                                        group: group.group.group_ref.clone(),
                                        selected,
                                    });
                                }
                                if ui
                                    .add_enabled(
                                        group.selected,
                                        egui::Button::new("Open collection"),
                                    )
                                    .clicked()
                                {
                                    out.actions.push(Action::FocusGroup(Some(
                                        group.group.group_ref.clone(),
                                    )));
                                }
                            });
                        });
                    }
                    ui.separator();
                    for source in &model.sources {
                        ui.push_id(("source", &source.source.source_ref), |ui| {
                            ui.horizontal_wrapped(|ui| {
                                let mut selected = source.selected;
                                let checkbox =
                                    ui.checkbox(&mut selected, &source.source.display_name);
                                self.control(
                                    ui,
                                    &checkbox,
                                    reference_key("source", &source.source.source_ref),
                                    &format!("Select source {}", source.source.display_name),
                                    out,
                                );
                                if checkbox.changed() {
                                    out.actions.push(Action::SetSourceSelected {
                                        source: source.source.source_ref.clone(),
                                        selected,
                                    });
                                }
                                if source.visible && !source.selected {
                                    ui.label("Included through a selected collection");
                                }
                                ui.label(source_status(&source.state));
                                let pause = ui.button(if source.source.paused {
                                    "Resume"
                                } else {
                                    "Pause"
                                });
                                if pause.clicked() {
                                    out.actions.push(Action::SetPaused {
                                        source: source.source.source_ref.clone(),
                                        paused: !source.source.paused,
                                    });
                                }
                            });
                        });
                    }
                });
            });
    }

    fn card(
        &mut self,
        ui: &mut Ui,
        rect: Rect,
        item: ItemId,
        query: &QuerySession,
        wall: &mut WallState,
        media: &mut impl MediaPainter,
        out: &mut Response,
    ) {
        ui.painter()
            .rect_filled(rect, 10.0, ui.visuals().faint_bg_color);
        let selected = wall.selected_member(item).cloned();
        if let Some(member) = &selected {
            let reference = MemberRef {
                item_id: item,
                member_id: member.member_id,
            };
            let painted = member.availability.state == AvailabilityState::Available
                && wall.media_state(&reference) == MediaState::Ready
                && if wall.player() == Some(&reference) {
                    media.player(ui, rect, &reference)
                } else {
                    media.preview(ui, rect, &reference)
                };
            if !painted {
                let status = match wall.media_state(&reference) {
                    MediaState::Unavailable => "Media unavailable",
                    MediaState::Failed => "Media could not load",
                    _ => "Preview loading…",
                };
                ui.painter().text(
                    rect.center(),
                    Align2::CENTER_CENTER,
                    status,
                    FontId::proportional(16.0),
                    ui.visuals().text_color(),
                );
            }
            if let Ok(Some(intent)) = wall.request_preview(item) {
                out.actions.push(Action::Wall(intent));
            }
            if matches!(member.media_kind, MediaKind::Video | MediaKind::Audio) {
                let play_rect = Rect::from_center_size(rect.center(), Vec2::new(72.0, 44.0));
                let button = ui.put(
                    play_rect,
                    egui::Button::new(if wall.player() == Some(&reference) {
                        "Stop"
                    } else {
                        "Play"
                    }),
                );
                self.control(
                    ui,
                    &button,
                    format!("play-{item}"),
                    "Play or stop media",
                    out,
                );
                if button.clicked() {
                    if wall.player() == Some(&reference) {
                        out.actions.extend(wall.stop_player().map(Action::Wall));
                    } else {
                        match wall.play(item) {
                            Ok(intents) => {
                                out.actions.extend(intents.into_iter().map(Action::Wall))
                            }
                            Err(error) => out.error = Some(error),
                        }
                    }
                }
            }
            let order = wall
                .card(item)
                .map(|card| card.item.member_order.clone())
                .unwrap_or_default();
            if order.len() > 1 {
                let index = order
                    .iter()
                    .position(|id| *id == member.member_id)
                    .unwrap_or(0);
                let footer = Rect::from_min_size(
                    egui::pos2(rect.left() + 8.0, rect.bottom() - 48.0),
                    Vec2::new((rect.width() - 16.0).min(224.0), 40.0),
                );
                ui.scope_builder(
                    egui::UiBuilder::new()
                        .max_rect(footer)
                        .layout(Layout::left_to_right(Align::Center)),
                    |ui| {
                        for (label, target) in [
                            ("Previous media", index.checked_sub(1)),
                            ("Next media", (index + 1 < order.len()).then_some(index + 1)),
                        ] {
                            let button = ui.add_enabled(
                                target.is_some(),
                                egui::Button::new(if label.starts_with("Previous") {
                                    "‹"
                                } else {
                                    "›"
                                }),
                            );
                            self.control(
                                ui,
                                &button,
                                format!(
                                    "{}-{item}",
                                    if label.starts_with("Previous") {
                                        "previous"
                                    } else {
                                        "next"
                                    }
                                ),
                                label,
                                out,
                            );
                            if button.clicked() {
                                if let Some(target) = target {
                                    if let Ok(intents) = wall.select_member(item, order[target]) {
                                        out.actions.extend(intents.into_iter().map(Action::Wall));
                                    }
                                }
                            }
                        }
                        ui.label(format!("{} / {}", index + 1, order.len()));
                    },
                );
            }
        } else {
            let title = wall
                .card(item)
                .and_then(|card| card.title().ok().flatten())
                .unwrap_or_else(|| "Loading item…".into());
            let text_rect = rect.shrink2(Vec2::new(24.0, 56.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(text_rect), |ui| {
                ui.add(egui::Label::new(egui::RichText::new(title).size(22.0)).wrap());
            });
        }
        let info_rect = Rect::from_min_size(
            egui::pos2(rect.right() - 48.0, rect.top() + 8.0),
            Vec2::splat(40.0),
        );
        let response = ui.put(info_rect, egui::Button::new("i"));
        self.control(
            ui,
            &response,
            format!("info-{item}"),
            &format!("Inspect item metadata and tags {item}"),
            out,
        );
        if self.restore_info == Some(item) {
            response.request_focus();
            self.restore_info = None;
        }
        if response.clicked() {
            match wall.inspect(query, item) {
                Ok(intent) => out.actions.push(Action::Wall(intent)),
                Err(error) => out.error = Some(error),
            }
        }
    }

    fn overlay(&mut self, ui: &mut Ui, viewport: Rect, wall: &mut WallState, out: &mut Response) {
        let mut close = false;
        let context = ui.ctx().clone();
        let modal = egui::Modal::new(Id::new((&self.namespace, "details-modal")))
            .backdrop_color(Color32::from_black_alpha(180))
            .frame(
                egui::Frame::new()
                    .fill(ui.visuals().panel_fill)
                    .inner_margin(16.0),
            )
            .show(&context, |ui| {
                ui.set_width((viewport.width() - 48.0).clamp(1.0, 672.0));
                ui.set_height((viewport.height() - 48.0).max(160.0));
                let can_close = self
                    .inspector
                    .as_mut()
                    .is_none_or(|(_, inspector)| inspector.session.draft().is_none());
                let dismiss = ui.add_enabled(can_close, egui::Button::new("Close details"));
                self.control(
                    ui,
                    &dismiss,
                    "close-details".into(),
                    "Close feed item details",
                    out,
                );
                if self.focus_details {
                    dismiss.request_focus();
                    self.focus_details = false;
                }
                close |= dismiss.clicked();
                if let Some((_, inspector)) = &mut self.inspector {
                    let response = inspector.show(ui);
                    out.controls.extend(
                        response
                            .controls
                            .into_iter()
                            .map(|(key, rect)| (format!("details-{key}"), rect)),
                    );
                    for action in response.actions {
                        if action == ViewAction::Close {
                            close = true;
                        } else {
                            out.actions.push(Action::Details(action));
                        }
                    }
                }
            });
        let can_close = self
            .inspector
            .as_mut()
            .is_none_or(|(_, inspector)| inspector.session.draft().is_none());
        close |= can_close && modal.should_close();
        if close {
            self.restore_info = self.inspector.as_ref().map(|(ticket, _)| ticket.item_id);
            self.inspector = None;
            wall.close_inspection();
            ui.ctx().request_repaint();
        }
    }
}

fn source_status(state: &AcquisitionState) -> &str {
    match state {
        AcquisitionState::NeverStarted => "Not checked yet",
        AcquisitionState::Running => "Checking…",
        AcquisitionState::Ready { .. } => "Up to date",
        AcquisitionState::VerifiedEmpty { .. } => "Verified empty",
        AcquisitionState::Partial { .. } => "Partly available",
        AcquisitionState::Reconnect => "Reconnect needed",
        AcquisitionState::Expired => "Connection expired",
        AcquisitionState::Failed { .. } => "Source failed",
        AcquisitionState::Unsupported => "Unsupported source",
        AcquisitionState::Cancelled => "Check cancelled",
    }
}

fn reference_key(kind: &str, reference: &HostRef) -> String {
    format!(
        "{kind}:{}:{}:{}:{}:{}",
        reference.host_instance_id.len(),
        reference.host_instance_id,
        reference.owner_kind.len(),
        reference.owner_kind,
        reference.native_id
    )
}
