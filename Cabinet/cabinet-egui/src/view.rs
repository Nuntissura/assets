//! Optional egui presentation. All durable effects leave as fenced host requests.
use crate::widgets::*;
use cabinet::interaction::{parse_hex, rgb_hex};
use cabinet::interaction::{ColorDraft, Edit, InteractionState, MoveTargets, TagDraft};
use cabinet::*;
use egui::{Color32, Key, Rect, Response, Sense, Stroke, Ui, Vec2};
use std::collections::BTreeMap;
mod dialogs;
mod menus;

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub text: Color32,
    pub muted: Color32,
    pub selected: Color32,
    pub accent: Color32,
    pub danger: Color32,
}
impl Palette {
    pub fn from_ui(ui: &Ui) -> Self {
        let v = ui.visuals();
        Self {
            text: v.text_color(),
            muted: v.weak_text_color(),
            selected: v.selection.bg_fill,
            accent: v.selection.stroke.color,
            danger: v.error_fg_color,
        }
    }
}
#[derive(Clone, Debug)]
pub struct DropDiagnostic {
    pub placement: Placement,
    pub valid: bool,
    pub rect: Rect,
}
#[derive(Default, Debug)]
pub struct ViewResponse {
    pub requests: Vec<Request>,
    pub rows: BTreeMap<EntryId, Rect>,
    pub controls: BTreeMap<String, Rect>,
    pub drop_target: Option<DropDiagnostic>,
    pub error: Option<String>,
    pub preview_requests: Vec<peak::PreviewRequest>,
    /// Host must persist per scope; on save failure reapply its last acknowledged settings.
    pub settings_changed: Option<Settings>,
    pub diagnostics: Diagnostics,
    pub recovery_requests: Vec<String>,
    pub search_navigation: Vec<SearchHit>,
    pub text_layouts: usize,
    pub preview_jobs: Vec<peak::PreviewJob>,
    pub preview_cancellations: Vec<u64>,
}
#[derive(Clone, Debug)]
struct Drag {
    namespace: String,
    context: Context,
    ids: Vec<EntryId>,
}
pub struct CabinetView {
    pub state: InteractionState,
    namespace: String,
    pub palette: Option<Palette>,
    edit_focus: bool,
    last_focus: Option<EntryId>,
    peak_view: cabinet_peak_egui::PeakView,
    peak_anchor: Rect,
    peak_rect: Option<Rect>,
    width_key: Option<(Context, u64, u32, u32)>,
    max_width: f32,
}
impl Default for CabinetView {
    fn default() -> Self {
        Self::new("cabinet")
    }
}
/// Hex encoding is injective even for labels containing punctuation or Unicode.
fn encoded(value: &str) -> String {
    value
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
impl CabinetView {
    pub fn new(namespace: impl Into<String>) -> Self {
        let namespace = namespace.into();
        let peak_view =
            cabinet_peak_egui::PeakView::new(format!("cabinet-{}", encoded(&namespace)));
        Self {
            state: InteractionState::default(),
            namespace,
            palette: None,
            edit_focus: false,
            last_focus: None,
            peak_view,
            peak_anchor: Rect::NOTHING,
            peak_rect: None,
            width_key: None,
            max_width: 0.0,
        }
    }
    fn author(&self, suffix: &str) -> String {
        format!("cabinet.{}.{}", encoded(&self.namespace), suffix)
    }
    /// Opt into bounded host jobs; repeated calls reconfigure limits while preserving active worker ownership.
    /// Legacy direct PreviewRequest output remains the default until the first successful call.
    pub fn enable_scheduler(
        &mut self,
        settings: peak::SchedulerSettings,
    ) -> Result<(), peak::SchedulerError> {
        self.state.enable_scheduler(settings)
    }
    pub fn complete_scheduled(&mut self, token: u64, content: peak::Content) -> bool {
        self.state.complete_scheduled(token, content)
    }
    /// Acknowledge only after the host has actually stopped that worker.
    pub fn acknowledge_preview_cancelled(&mut self, token: u64) {
        self.state.acknowledge_preview_cancelled(token);
    }
    pub fn set_tag_catalog(&mut self, context: Context, tags: Vec<Tag>) {
        self.state.set_tag_catalog(context, tags);
    }
    fn control(
        &self,
        ui: &Ui,
        response: &Response,
        name: &str,
        label: &str,
        output: &mut ViewResponse,
    ) {
        output.controls.insert(name.into(), response.rect);
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_author_id(self.author(name));
            node.set_label(label);
        });
    }
    fn emit(&mut self, cabinet: &mut Cabinet, op: Operation, out: &mut ViewResponse) {
        if let Ok(request) = self.state.request(cabinet, op) {
            out.requests.push(request);
        }
    }
    fn begin_create(&mut self, parent: Option<EntryId>) {
        self.state.begin_create(parent);
        self.edit_focus = true;
    }
    fn cancel_drag(&self, ui: &Ui) {
        if egui::DragAndDrop::payload::<Drag>(ui.ctx())
            .is_some_and(|d| d.namespace == self.namespace)
        {
            egui::DragAndDrop::clear_payload(ui.ctx());
        }
    }
    fn begin_rename(&mut self, cabinet: &Cabinet, id: EntryId) {
        self.state.begin_rename(cabinet, id);
        self.edit_focus = self.state.edit.is_some();
    }
    fn modifiers(ui: &Ui) -> SelectionModifiers {
        ui.input(|i| SelectionModifiers {
            toggle: i.modifiers.command,
            range: i.modifiers.shift,
        })
    }
    pub fn show(&mut self, ui: &mut Ui, cabinet: &mut Cabinet) -> ViewResponse {
        let mut out = ViewResponse::default();
        let now_ms = ui.input(|i| (i.time.max(0.0) * 1000.0) as u64);
        let settings_before = cabinet.settings().clone();
        let previous = self.state.context.clone();
        let mut draft_cancelled = self.state.sync(cabinet);
        let mut peak_hovered = false;
        if previous.as_ref().is_some_and(|old| {
            old.scope != cabinet.snapshot().context.scope
                || old.generation != cabinet.snapshot().context.generation
        }) {
            self.cancel_drag(ui);
        }
        let palette = self.palette.unwrap_or_else(|| Palette::from_ui(ui));
        let focus_changed = self.last_focus.as_deref() != cabinet.focused();
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            self.state.edit = None;
            self.state.delete = None;
            self.state.hover = None;
            self.cancel_drag(ui);
            self.state.click = None;
            self.state.color_draft = None;
            self.state.tag_draft = None;
            self.state.moving = None;
            self.state.move_targets = None;
            self.state.peak.dismiss();
        }
        let ns = self.namespace.clone();
        let scope = cabinet.snapshot().context.scope.clone();
        let generation = cabinet.snapshot().context.generation;
        ui.push_id((&ns,&scope,generation), |ui| {
            ui.horizontal(|ui| {
                let response = folder_plus(ui, palette);
                self.control(ui, &response, "new-folder", "New folder", &mut out);
                if response.clicked() {
                    let parent = cabinet
                        .focused()
                        .and_then(|id| cabinet.entry(id))
                        .filter(|e| e.kind == EntryKind::Folder)
                        .map(|e| e.id.clone());
                    if let Some(id) = &parent {
                        let _ = cabinet.expand(id, true);
                    }
                    self.begin_create(parent);
                }
                let undo = ui.add_enabled(
                    cabinet.can_undo() && cabinet.pending().is_none(),
                    egui::Button::new("Undo"),
                );
                self.control(ui, &undo, "undo", "Undo", &mut out);
                if undo.clicked() {
                    match cabinet.request_undo() {
                        Ok(r) => out.requests.push(r),
                        Err(e) => self.state.last_error = Some(e.to_string()),
                    }
                }
                let redo = ui.add_enabled(
                    cabinet.can_redo() && cabinet.pending().is_none(),
                    egui::Button::new("Redo"),
                );
                self.control(ui, &redo, "redo", "Redo", &mut out);
                if redo.clicked() {
                    match cabinet.request_redo() {
                        Ok(r) => out.requests.push(r),
                        Err(e) => self.state.last_error = Some(e.to_string()),
                    }
                }
                ui.menu_button("Settings", |ui| {
                    let mut settings = cabinet.settings().clone();
                    ui.label("Peak");
                    ui.checkbox(&mut settings.peak.enabled, "Enable previews");
                    ui.add(egui::Slider::new(&mut settings.peak.hover_delay_ms, 0..=1500).text("Name delay (ms)"));
                    ui.add(egui::Slider::new(&mut settings.peak.expand_delay_ms, 100..=5000).text("Expand delay (ms)"));
                    ui.add(egui::Slider::new(&mut settings.peak.grace_ms, 100..=1500).text("Leave grace (ms)"));
                    ui.add(egui::Slider::new(&mut settings.peak.width, 160.0..=1000.0).text("Preview width"));
                    ui.add(egui::Slider::new(&mut settings.peak.height, 100.0..=800.0).text("Preview height"));
                    ui.add(egui::Slider::new(&mut settings.peak.max_text_lines, 5..=200).text("Text lines"));
                    ui.checkbox(&mut settings.show_tags, "Show row tags");
                    ui.separator();
                    ui.selectable_value(&mut settings.click_behavior, ClickBehavior::SingleOpenDoubleRename, "Single open / double rename");
                    ui.selectable_value(&mut settings.click_behavior, ClickBehavior::SelectDoubleOpen, "Single select / double open");
                    ui.add(egui::Slider::new(&mut settings.double_click_ms,100..=1000).text("Double-click interval (ms)"));
                    ui.separator();
                    let mut hidden=settings.hidden_patterns.join(";");
                    if ui.add(egui::TextEdit::singleline(&mut hidden).hint_text("Hidden names: *.tmp;.*")).changed(){settings.hidden_patterns=hidden.split(';').map(str::trim).filter(|s|!s.is_empty()).map(str::to_owned).collect();}
                    let mut excluded=settings.watcher_exclusions.join(";");
                    if ui.add(egui::TextEdit::singleline(&mut excluded).hint_text("Host watcher exclusions")).changed(){settings.watcher_exclusions=excluded.split(';').map(str::trim).filter(|s|!s.is_empty()).map(str::to_owned).collect();}
                    ui.label("Hiding rows does not stop host file watching.");
                    let _ = cabinet.apply_settings(settings);
                });
            });
            let dragging=egui::DragAndDrop::payload::<Drag>(ui.ctx()).is_some();
            let crumbs=cabinet.focused().and_then(|id|cabinet.breadcrumbs(id).ok()).unwrap_or_default();
            // A fixed-height, horizontally scrolling path lane cannot shift drag targets.
            egui::ScrollArea::horizontal().id_salt("breadcrumb-scroll").max_height(22.0).auto_shrink([false,false]).show(ui,|ui| {
                ui.horizontal(|ui| {
                    for id in crumbs {
                        let label=cabinet.entry(&id).map(|e|e.label.clone()).unwrap_or_default();
                        if ui.small_button(label).clicked(){let _=cabinet.reveal(&id);}
                        ui.label("/");
                    }
                });
            });
            ui.horizontal_wrapped(|ui| {
                for id in cabinet.settings().pinned_folders.clone() {
                    let label=cabinet.full_path(&id).unwrap_or_else(|_|format!("{id} (unloaded)"));
                    if ui.add_enabled(cabinet.entry(&id).is_some(),egui::Button::new(label)).clicked(){let _=cabinet.reveal(&id);}
                }
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.state.quick_path).hint_text("Go to loaded path"));
                if ui.button("Go").clicked(){if let Err(e)=cabinet.navigate_path(&self.state.quick_path){self.state.last_error=Some(e.to_string());}}
            });
            ui.horizontal(|ui|{
                ui.selectable_value(&mut self.state.host_search,false,"Loaded tree");
                let mode=ui.selectable_value(&mut self.state.host_search,true,"Workspace search (host)");
                self.control(ui,&mode,"host-search-mode","Workspace search",&mut out);
            });
            let mut filter = if self.state.host_search {self.state.workspace_query.clone()} else {cabinet.filter().to_owned()};
            let f = ui.add(
                egui::TextEdit::singleline(&mut filter)
                    .hint_text("Filter files and folders")
                    .desired_width(f32::INFINITY)
                    .id_salt("filter"),
            );
            self.control(ui, &f, "filter", "Filter files and folders", &mut out);
            if f.changed() && self.state.host_search {self.state.workspace_query=filter.clone();}
            if f.changed() && !self.state.host_search {
                cabinet.set_filter(&filter);
            }
            if self.state.host_search {
                ui.label("Workspace search uses host authorization; loaded-tree filtering only examines loaded entries.");
                let submit=ui.add_enabled(cabinet.pending().is_none()&&!filter.trim().is_empty(),egui::Button::new("Search workspace"));self.control(ui,&submit,"search-submit","Search workspace",&mut out);
                if submit.clicked(){self.emit(cabinet,Operation::WorkspaceSearch{query:filter.clone()},&mut out);}
                if let Some(result)=cabinet.search_result().cloned(){for hit in result.hits{if ui.button(&hit.path).clicked(){out.search_navigation.push(hit);}}}
            }
            ui.label("Tree default · folder overrides stay independent");
            let mut sort = cabinet.sort();
            let combo = egui::ComboBox::from_id_salt("sort")
                .selected_text(sort_label(sort))
                .show_ui(ui, |ui| {
                    for s in sort_options() {
                        ui.selectable_value(&mut sort, s, sort_label(s));
                    }
                });
            self.control(ui, &combo.response, "sort", "Sort entries", &mut out);
            if sort != cabinet.sort() {
                cabinet.set_sort(sort);
            }
            let visible=cabinet.projected_rows();
            let hidden_edit=match &self.state.edit {
                Some(Edit::Rename{id})=>!visible.iter().any(|r|&r.id==id),
                Some(Edit::Create{parent:Some(id)})=>!visible.iter().any(|r|&r.id==id),
                _=>false,
            };
            if hidden_edit {
                draft_cancelled=true;
                self.state.edit=None;self.state.text.clear();self.edit_focus=false;
                self.state.last_error=Some("Editing cancelled because the entry is hidden by the current filter or collapsed folder.".into());
            }
            if let Some(ids) = self.state.delete.clone() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("Delete {} selected item(s)?", ids.len()));
                    let yes = ui.button("Delete");
                    self.control(ui, &yes, "confirm-delete", "Confirm delete", &mut out);
                    if yes.clicked() {
                        self.emit(cabinet, Operation::Delete { ids }, &mut out);
                        self.state.delete = None;
                    }
                    let no = ui.button("Cancel");
                    self.control(ui, &no, "cancel-delete", "Cancel delete", &mut out);
                    if no.clicked() {
                        self.state.delete = None;
                    }
                });
            }
            for grant in cabinet.approved_moves() {
                ui.label("Host checked the complete hierarchy. Confirm the approved move.");
                let confirm=ui.add_enabled(cabinet.pending().is_none(),egui::Button::new("Confirm host-approved move"));self.control(ui,&confirm,"confirm-authorized-move","Confirm host-approved move",&mut out);if confirm.clicked(){self.emit(cabinet,Operation::MoveAuthorized{ids:grant.ids,placement:grant.placement,token:grant.token},&mut out);}
            }
            if let Some(progress)=cabinet.bulk_progress() {
                let fraction=if progress.total==0 {0.0}else{progress.completed as f32/progress.total as f32};
                ui.add(egui::ProgressBar::new(fraction).text(format!("{} / {} · {}",progress.completed,progress.total,progress.message)));
                if let Some(token)=&progress.recovery_token {if ui.button("Ask host to resume recovery").clicked(){out.recovery_requests.push(token.clone());}}
            }
            if cabinet.pending().is_some() {
                ui.label("Waiting for host…");
            }
            out.error = self
                .state.last_error
                .clone()
                .or_else(|| cabinet.error().map(str::to_owned));
            if let Some(e) = &out.error {
                ui.colored_label(palette.danger, e);
            }
            let keyboard = !draft_cancelled && !f.has_focus() && self.state.edit.is_none() && self.state.delete.is_none() && self.state.color_draft.is_none() && self.state.tag_draft.is_none() && self.state.moving.is_none();
            let mut keyboard_handled = false;
            let projected=cabinet.projected_rows();
            let draft_index=match &self.state.edit {
                Some(Edit::Create{parent:None})=>Some((0,0)),
                Some(Edit::Create{parent:Some(id)})=>projected.iter().position(|r|&r.id==id).map(|at|(at+1,projected[at].depth+1)),
                _=>None,
            };
            let count=projected.len()+usize::from(draft_index.is_some())+1;
            let font_size=ui.text_style_height(&egui::TextStyle::Body);
            let width_key=(cabinet.snapshot().context.clone(),cabinet.diagnostics().projection_rebuilds,ui.ctx().pixels_per_point().to_bits(),font_size.to_bits());
            if self.width_key.as_ref()!=Some(&width_key) {
                self.max_width=0.0;
                self.width_key=Some(width_key);
            }
            let content_width=self.max_width.max(ui.available_width());
            let mut scroll=egui::ScrollArea::both().id_salt("tree-scroll").auto_shrink([false,false]);
            if focus_changed && !dragging {
                if let Some(at)=projected.iter().position(|r|Some(r.id.as_str())==cabinet.focused()) {
                    let at=at+usize::from(draft_index.is_some_and(|(insert,_)|insert<=at));
                    scroll=scroll.vertical_scroll_offset(at as f32*(26.0+ui.spacing().item_spacing.y));
                }
            }
            let area = scroll.show_rows(ui,26.0,count,|ui,range| {
                    cabinet.record_rendered(range.len());

                    ui.ctx().accesskit_node_builder(ui.id(), |n| {
                        n.set_role(egui::accesskit::Role::Tree);
                        n.set_author_id(self.author("tree"));
                        n.set_label("Files and folders");
                    });
                    ui.set_min_width(content_width);
                    for display_index in range {
                        if display_index==count-1 {self.root_target(ui,cabinet,palette,&mut out);continue;}
                        if draft_index.is_some_and(|(insert,_)|insert==display_index) {
                            self.create_row(ui,cabinet,&mut out,draft_index.unwrap().1);continue;
                        }
                        let index=display_index-usize::from(draft_index.is_some_and(|(insert,_)|insert<display_index));
                        let row=&projected[index];
                        let Some(entry) = cabinet.entry(&row.id).cloned() else {
                            continue;
                        };
                        let label_width=ui.painter().layout_no_wrap(presentation_label(&entry),egui::FontId::proportional(14.0),palette.text).size().x;
                        out.text_layouts+=1;
                        self.max_width=self.max_width.max(row.depth as f32*16.0+80.0+label_width);
                        ui.set_min_width(self.max_width.max(content_width));
                        let selected = cabinet.is_selected(&entry.id);
                        let expanded = cabinet.expanded(&entry.id);
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), 26.0),
                            Sense::hover(),
                        );
                        let id = ui.id().with(("row", &entry.id));
                        let body_rect = Rect::from_min_max(rect.min, rect.max - Vec2::new(28.0,0.0));
                        let response = ui.interact(body_rect, id, Sense::click_and_drag());
                        let options_rect = Rect::from_min_max(egui::pos2(rect.right()-26.0,rect.top()),rect.max);
                        let options = ui.put(options_rect,egui::Button::new("…").frame(false));
                        self.control(ui,&options,&format!("options:{}",entry.id),"File or folder options",&mut out);
                        if options.clicked() { self.state.click=None; self.state.peak.dismiss(); }
                        out.rows.insert(entry.id.clone(), rect);
                        if selected || response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                3.0,
                                if selected {
                                    palette.selected
                                } else {
                                    ui.visuals().widgets.hovered.bg_fill
                                },
                            );
                        }
                        let indent = row.depth as f32 * 16.0;
                        let disclosure = Rect::from_min_size(
                            rect.min + Vec2::new(indent, 3.0),
                            Vec2::splat(20.0),
                        );
                        if entry.kind == EntryKind::Folder {
                            let x = disclosure.center();
                            let d = 4.0;
                            let points = if expanded {
                                vec![
                                    x + Vec2::new(-d, -2.0),
                                    x + Vec2::new(d, -2.0),
                                    x + Vec2::new(0.0, d),
                                ]
                            } else {
                                vec![
                                    x + Vec2::new(-2.0, -d),
                                    x + Vec2::new(-2.0, d),
                                    x + Vec2::new(d, 0.0),
                                ]
                            };
                            ui.painter().add(egui::Shape::convex_polygon(
                                points,
                                palette.muted,
                                Stroke::NONE,
                            ));
                        }
                        let icon = Rect::from_min_size(
                            rect.min + Vec2::new(indent + 22.0, 7.0),
                            Vec2::new(14.0, 12.0),
                        );
                        let color = entry
                            .color
                            .map(|c| Color32::from_rgb(c[0], c[1], c[2]))
                            .unwrap_or(palette.muted);
                        ui.painter().rect_stroke(
                            icon,
                            1.0,
                            Stroke::new(1.0_f32, color),
                            egui::StrokeKind::Inside,
                        );
                        if entry.kind == EntryKind::Folder {
                            ui.painter().line_segment(
                                [icon.left_top(), icon.left_top() + Vec2::new(7.0, -3.0)],
                                Stroke::new(2.0_f32, color),
                            );
                        }
                        let text_rect =
                            Rect::from_min_max(rect.min + Vec2::new(indent + 42.0, 0.0), body_rect.max);
                        let editing = matches!(&self.state.edit,Some(Edit::Rename{id}) if id==&entry.id);
                        if editing {
                            self.edit_field(
                                ui,
                                cabinet,
                                &mut out,
                                Some(text_rect.shrink2(Vec2::new(2.0, 1.0))),
                            );
                        } else {
                            ui.painter()
                                .with_clip_rect(text_rect.intersect(ui.clip_rect()))
                                .text(
                                    text_rect.left_center(),
                                    egui::Align2::LEFT_CENTER,
                                    presentation_label(&entry),
                                    egui::FontId::proportional(14.0),
                                    palette.text,
                                );
                        }
                        if cabinet.settings().show_tags && !editing {
                            let name_width = ui.painter().layout_no_wrap(presentation_label(&entry),egui::FontId::proportional(14.0),palette.text).size().x;
                            let mut x = text_rect.left()+name_width+10.0;
                            for tag in &entry.tags {
                                let galley=ui.painter().layout_no_wrap(tag.label.clone(),egui::FontId::proportional(11.0),Color32::WHITE);
                                let chip=Rect::from_min_size(egui::pos2(x,rect.top()+4.0),Vec2::new(galley.size().x+10.0,18.0));
                                if chip.right()>body_rect.right() { break; }
                                ui.painter().rect_filled(chip,4.0,Color32::from_rgb(tag.color[0],tag.color[1],tag.color[2]).gamma_multiply(0.65));
                                ui.painter().galley(chip.min+Vec2::new(5.0,2.0),galley,Color32::WHITE);
                                x=chip.right()+5.0;
                            }
                        }
                        if response.hovered() && !editing && self.state.edit.is_none() && self.state.color_draft.is_none() && self.state.tag_draft.is_none() && !options.clicked() && egui::DragAndDrop::payload::<Drag>(ui.ctx()).is_none() {
                            peak_hovered=true;
                            self.peak_anchor=rect;
                            self.state.peak.hover(peak::Key { scope:cabinet.snapshot().context.scope.clone(), generation:cabinet.snapshot().context.generation,context_revision:cabinet.snapshot().context.revision,entry_id:entry.id.clone(),resource_id:entry.resource_id.clone(),entry_revision:entry.revision },entry.label.clone(),entry.tags.clone(),now_ms);
                        }
                        ui.ctx().accesskit_node_builder(id, |n| {
                            n.set_role(egui::accesskit::Role::TreeItem);
                            n.set_author_id(self.author(&format!("row.{}", encoded(&entry.id))));
                            n.set_label(presentation_label(&entry));
                            n.set_selected(selected);
                            n.set_level(row.depth + 1);
                            if entry.kind == EntryKind::Folder {
                                n.set_expanded(expanded);
                            }
                        });
                        if response.clicked() {
                            response.request_focus();
                            if entry.kind == EntryKind::Folder
                                && response
                                    .interact_pointer_pos()
                                    .is_some_and(|p| disclosure.contains(p))
                            {
                                self.expand(cabinet, &entry.id, !expanded, &mut out);
                            } else {
                                let _ = cabinet.select(&entry.id, Self::modifiers(ui));
                                let modified=ui.input(|i|i.modifiers.command||i.modifiers.shift);
                                if modified { self.state.click=None; }
                                if !modified && entry.kind==EntryKind::Leaf && cabinet.settings().click_behavior==ClickBehavior::SingleOpenDoubleRename {
                                    if self.state.click.as_ref().is_some_and(|(target,context,start)|target==&entry.id && context==&cabinet.snapshot().context && now_ms.saturating_sub(*start)<=cabinet.settings().double_click_ms) {
                                        self.state.click=None;
                                        if entry.capabilities.rename { self.begin_rename(cabinet,entry.id.clone()); }
                                    } else {
                                        self.state.click=Some((entry.id.clone(),cabinet.snapshot().context.clone(),now_ms));
                                    }
                                }
                            }
                        }
                        if response.double_clicked() && cabinet.settings().click_behavior==ClickBehavior::SelectDoubleOpen {
                            if entry.kind == EntryKind::Folder {
                                self.expand(cabinet, &entry.id, !expanded, &mut out);
                            } else {
                                self.emit(
                                    cabinet,
                                    Operation::Open {
                                        id: entry.id.clone(),
                                    },
                                    &mut out,
                                );
                            }
                        }
                        if response.drag_started() && entry.capabilities.move_entry {
                            self.state.click=None;
                            self.state.peak.dismiss();
                            if !selected {
                                let _ = cabinet.select(&entry.id, SelectionModifiers::default());
                            }
                            response.dnd_set_drag_payload(Drag {
                                namespace: self.namespace.clone(),
                                context: cabinet.snapshot().context.clone(),
                                ids: cabinet.selected(),
                            });
                        }
                        self.drop_row(ui, cabinet, &response, &entry, palette, &mut out);
                        egui::Popup::context_menu(&response).show(|ui| {
                            self.entry_menu(ui, cabinet, &entry, selected, &mut out);
                        });
                        egui::Popup::menu(&options).show(|ui| {
                            self.entry_menu(ui, cabinet, &entry, selected, &mut out);
                        });
                        if keyboard && self.state.edit.is_none() && self.state.delete.is_none() && !keyboard_handled && response.has_focus() {
                            self.keyboard(ui, cabinet, &mut out);
                            keyboard_handled = true;
                            if let Some(focused) = cabinet.focused() {
                                ui.ctx().memory_mut(|m| {
                                    m.request_focus(ui.id().with(("row", focused)))
                                });
                            }
                        }
                        if !dragging && (keyboard_handled || focus_changed)
                            && cabinet.focused() == Some(entry.id.as_str())
                        {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                    }
                    if egui::DragAndDrop::payload::<Drag>(ui.ctx())
                        .is_some_and(|drag| drag.namespace == self.namespace)
                    {
                        if let Some(p) = ui.input(|i| i.pointer.hover_pos()) {
                            let clip = ui.clip_rect();
                            let dt = ui.input(|i| i.stable_dt).min(0.1);
                            if clip.contains(p) && p.y < clip.top() + 30.0 {
                                ui.scroll_with_delta(Vec2::new(0.0, 220.0 * dt));
                                ui.ctx().request_repaint();
                            } else if clip.contains(p) && p.y > clip.bottom() - 30.0 {
                                ui.scroll_with_delta(Vec2::new(0.0, -220.0 * dt));
                                ui.ctx().request_repaint();
                            }
                        }
                    }
                });
            out.controls.insert("tree".into(), area.inner_rect);
            if out.drop_target.is_none() {
                self.state.hover = None;
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                self.state.edit = None;
                self.state.delete = None;
                self.state.hover = None;
                self.cancel_drag(ui);
            }

        });
        cabinet.record_rendered(out.rows.len());
        out.diagnostics = cabinet.diagnostics();
        self.last_focus = cabinet.focused().map(str::to_owned);
        let peak_contains_pointer = self.state.peak.stage() != peak::Stage::Hidden
            && self.peak_rect.is_some_and(|rect| {
                ui.input(|i| i.pointer.hover_pos().is_some_and(|p| rect.contains(p)))
            });
        if peak_contains_pointer {
            self.state.peak.retain();
        } else if !peak_hovered {
            self.state.peak.leave(now_ms);
        }
        if self.state.scheduler.is_some() {
            if let Some(scheduler) = &mut self.state.scheduler {
                if let Err(error) = self.state.peak.tick_scheduled(scheduler, now_ms) {
                    out.error = Some(format!("{error:?}"));
                }
                out.preview_jobs = scheduler.take_ready();
                out.preview_cancellations = scheduler.take_cancellations();
            }
        } else if let Some(request) = self.state.peak.tick(now_ms) {
            out.preview_requests.push(request);
        }
        if self.state.peak.stage() != peak::Stage::Hidden {
            self.peak_rect =
                self.peak_view
                    .show(ui.ctx(), &mut self.state.peak, self.peak_anchor, now_ms);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        } else if peak_hovered {
            self.peak_rect = None;
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        if self.state.peak.settings != cabinet.settings().peak {
            let mut settings = cabinet.settings().clone();
            settings.peak = self.state.peak.settings.clone();
            let _ = cabinet.apply_settings(settings);
        }
        ui.push_id((&ns, &scope, generation), |ui| {
            self.dialogs(ui, cabinet, &mut out)
        });
        if let Some((id, context, start)) = self.state.click.clone() {
            if now_ms.saturating_sub(start) > cabinet.settings().double_click_ms {
                self.state.click = None;
                if context == cabinet.snapshot().context
                    && cabinet.rows().iter().any(|r| r.id == id)
                    && self.state.edit.is_none()
                    && self.state.color_draft.is_none()
                    && self.state.tag_draft.is_none()
                    && self.state.delete.is_none()
                    && self.state.moving.is_none()
                    && cabinet.pending().is_none()
                {
                    self.emit(cabinet, Operation::Open { id }, &mut out);
                }
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(25));
            }
        }
        if cabinet.settings() != &settings_before {
            out.settings_changed = Some(cabinet.settings().clone());
        }
        out
    }

    fn expand(&mut self, cabinet: &mut Cabinet, id: &str, value: bool, out: &mut ViewResponse) {
        if let Ok(Some(request)) = self.state.expand(cabinet, id, value) {
            out.requests.push(request);
        }
    }
    fn create_row(
        &mut self,
        ui: &mut Ui,
        cabinet: &mut Cabinet,
        out: &mut ViewResponse,
        depth: usize,
    ) {
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
        self.edit_field(
            ui,
            cabinet,
            out,
            Some(Rect::from_min_max(
                r.min + Vec2::new(depth as f32 * 16.0 + 22.0, 1.0),
                r.max - Vec2::new(2.0, 1.0),
            )),
        );
    }
    fn edit_field(
        &mut self,
        ui: &mut Ui,
        cabinet: &mut Cabinet,
        out: &mut ViewResponse,
        rect: Option<Rect>,
    ) {
        let edit = egui::TextEdit::singleline(&mut self.state.text)
            .id_salt("inline-edit")
            .hint_text("Folder name");
        let response = if let Some(rect) = rect {
            ui.put(rect, edit)
        } else {
            ui.add(edit)
        };
        self.control(ui, &response, "inline-edit", "Entry name", out);
        if self.edit_focus {
            response.request_focus();
            self.edit_focus = false;
        }
        if ui.input(|i| i.key_pressed(Key::Enter))
            && (response.has_focus() || response.lost_focus())
        {
            if let Ok(Some(request)) = self.state.submit_edit(cabinet) {
                out.requests.push(request);
            }
        }
    }
    fn keyboard(&mut self, ui: &Ui, cabinet: &mut Cabinet, out: &mut ViewResponse) {
        if ui.input(|i| {
            i.key_pressed(Key::F2)
                || i.key_pressed(Key::Delete)
                || i.key_pressed(Key::Enter)
                || i.key_pressed(Key::ArrowUp)
                || i.key_pressed(Key::ArrowDown)
                || i.key_pressed(Key::Home)
                || i.key_pressed(Key::End)
        }) {
            self.state.click = None;
        }
        for (key, nav) in [
            (Key::ArrowUp, Navigation::Up),
            (Key::ArrowDown, Navigation::Down),
            (Key::ArrowLeft, Navigation::Left),
            (Key::ArrowRight, Navigation::Right),
            (Key::Home, Navigation::Home),
            (Key::End, Navigation::End),
        ] {
            if ui.input(|i| i.key_pressed(key)) {
                let _ = cabinet.navigate(nav, Self::modifiers(ui));
                if key == Key::ArrowRight {
                    if let Some(id) = cabinet.focused().map(str::to_owned) {
                        if cabinet.expanded(&id)
                            && cabinet.entry(&id).is_some_and(|e| {
                                matches!(
                                    e.children,
                                    ChildrenState::Unloaded | ChildrenState::Failed(_)
                                )
                            })
                        {
                            match cabinet.request_child_page(&id) {
                                Ok(request) => out.requests.push(request),
                                Err(error) => self.state.last_error = Some(error.to_string()),
                            }
                        }
                    }
                }
            }
        }
        if ui.input(|i| i.key_pressed(Key::F2)) {
            if let Some(id) = cabinet.focused().map(str::to_owned) {
                self.begin_rename(cabinet, id);
            }
        }
        if ui.input(|i| i.key_pressed(Key::Delete)) {
            let ids = cabinet.selected();
            if !ids.is_empty() {
                self.state.delete = Some(ids);
            }
        }
        if self.state.edit.is_none()
            && self.state.delete.is_none()
            && ui.input(|i| i.key_pressed(Key::Enter))
        {
            if let Some(id) = cabinet.focused().map(str::to_owned) {
                self.emit(cabinet, Operation::Open { id }, out);
            }
        }
        if ui.input(|i| i.modifiers.command && i.key_pressed(Key::Z)) {
            let result = if ui.input(|i| i.modifiers.shift) {
                cabinet.request_redo()
            } else {
                cabinet.request_undo()
            };
            match result {
                Ok(r) => out.requests.push(r),
                Err(e) => self.state.last_error = Some(e.to_string()),
            }
        }
    }
    fn drop_row(
        &mut self,
        ui: &Ui,
        cabinet: &mut Cabinet,
        response: &Response,
        entry: &Entry,
        palette: Palette,
        out: &mut ViewResponse,
    ) {
        if response.dnd_hover_payload::<Drag>().is_none() {
            return;
        }
        let y = ui
            .input(|i| i.pointer.hover_pos())
            .map_or(response.rect.center().y, |p| p.y);
        let placement = if y < response.rect.top() + 6.0 {
            Placement::Before(entry.id.clone())
        } else if y > response.rect.bottom() - 6.0 {
            Placement::After(entry.id.clone())
        } else {
            Placement::Into(Some(entry.id.clone()))
        };
        self.drop_target(ui, cabinet, response, placement.clone(), palette, out);
        if matches!(placement, Placement::Into(_))
            && entry.kind == EntryKind::Folder
            && out.drop_target.as_ref().is_some_and(|d| d.valid)
        {
            let now = ui.input(|i| i.time);
            if self
                .state
                .hover
                .as_ref()
                .is_none_or(|(id, _)| id != &entry.id)
            {
                self.state.hover = Some((entry.id.clone(), now));
            }
            if self
                .state
                .hover
                .as_ref()
                .is_some_and(|(_, start)| now - start > 0.7)
                && !cabinet.expanded(&entry.id)
            {
                self.expand(cabinet, &entry.id, true, out);
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        } else {
            self.state.hover = None;
        }
    }
    fn drop_target(
        &mut self,
        ui: &Ui,
        cabinet: &mut Cabinet,
        response: &Response,
        placement: Placement,
        palette: Palette,
        out: &mut ViewResponse,
    ) {
        let Some(payload) = response.dnd_hover_payload::<Drag>() else {
            return;
        };
        if payload.namespace != self.namespace {
            return;
        }
        let valid = payload.context == cabinet.snapshot().context
            && cabinet.pending().is_none()
            && cabinet
                .validate_move_preflight(&payload.ids, &placement)
                .is_ok();
        let color = if valid {
            palette.accent
        } else {
            palette.danger
        };
        let r = response.rect;
        match &placement {
            Placement::Before(_) => {
                ui.painter()
                    .line_segment([r.left_top(), r.right_top()], Stroke::new(2.0_f32, color));
            }
            Placement::After(_) => {
                ui.painter().line_segment(
                    [r.left_bottom(), r.right_bottom()],
                    Stroke::new(2.0_f32, color),
                );
            }
            Placement::Into(_) => {
                ui.painter().rect_stroke(
                    r,
                    2.0,
                    Stroke::new(2.0_f32, color),
                    egui::StrokeKind::Inside,
                );
            }
        }
        out.drop_target = Some(DropDiagnostic {
            placement: placement.clone(),
            valid,
            rect: r,
        });
        if let Some(payload) = response.dnd_release_payload::<Drag>() {
            if valid {
                self.emit(
                    cabinet,
                    Operation::Move {
                        ids: payload.ids.clone(),
                        placement,
                    },
                    out,
                );
            } else {
                self.state.last_error =
                    Some("Move unavailable: stale, denied or invalid destination".into());
            }
        }
    }
}

#[cfg(test)]
#[path = "view_tests.rs"]
mod tests;
