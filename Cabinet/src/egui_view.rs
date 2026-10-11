//! Optional egui presentation. All durable effects leave as fenced host requests.
use crate::*;
use egui::{Color32, Key, Rect, Response, Sense, Stroke, Ui, Vec2};
use std::collections::BTreeMap;

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
enum Edit {
    Create { parent: Option<EntryId> },
    Rename { id: EntryId },
}
struct ColorDraft {
    id: EntryId,
    revision: u64,
    color: [u8; 3],
    hex: String,
}
struct TagDraft {
    id: EntryId,
    revision: u64,
    tags: Vec<Tag>,
    hexes: BTreeMap<String, String>,
    new_id: String,
    new_label: String,
}
struct MoveTargets {
    context: Context,
    source: EntryId,
    targets: Vec<(Option<EntryId>, bool)>,
}
pub struct CabinetView {
    namespace: String,
    pub palette: Option<Palette>,
    edit: Option<Edit>,
    text: String,
    edit_focus: bool,
    delete: Option<Vec<EntryId>>,
    hover: Option<(EntryId, f64)>,
    last_error: Option<String>,
    context: Option<Context>,
    last_focus: Option<EntryId>,
    pub peak: peak::Peak,
    peak_view: peak::egui_view::PeakView,
    peak_anchor: Rect,
    peak_rect: Option<Rect>,
    available_tags: Vec<Tag>,
    catalog_context: Option<Context>,
    color_draft: Option<ColorDraft>,
    tag_draft: Option<TagDraft>,
    moving: Option<EntryId>,
    move_targets: Option<MoveTargets>,
    click: Option<(EntryId, Context, u64)>,
    width_key: Option<(Context, u64, u32, u32)>,
    max_width: f32,
    quick_path: String,
    host_search: bool,
    workspace_query: String,
    scheduler: Option<peak::PreviewScheduler>,
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
        let peak_view = peak::egui_view::PeakView::new(format!("cabinet-{}", encoded(&namespace)));
        Self {
            namespace,
            palette: None,
            edit: None,
            text: String::new(),
            edit_focus: false,
            delete: None,
            hover: None,
            last_error: None,
            context: None,
            last_focus: None,
            peak: peak::Peak::default(),
            peak_view,
            peak_anchor: Rect::NOTHING,
            peak_rect: None,
            available_tags: Vec::new(),
            catalog_context: None,
            color_draft: None,
            tag_draft: None,
            moving: None,
            move_targets: None,
            click: None,
            width_key: None,
            max_width: 0.0,
            quick_path: String::new(),
            host_search: false,
            workspace_query: String::new(),
            scheduler: None,
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
        if let Some(scheduler) = self.scheduler.as_mut() {
            return scheduler.configure(settings);
        }
        self.scheduler = Some(peak::PreviewScheduler::new(settings)?);
        Ok(())
    }
    pub fn complete_scheduled(&mut self, token: u64, content: peak::Content) -> bool {
        self.scheduler
            .as_mut()
            .and_then(|scheduler| scheduler.finish(token, content))
            .is_some_and(|completion| self.peak.complete(completion))
    }
    /// Acknowledge only after the host has actually stopped that worker.
    pub fn acknowledge_preview_cancelled(&mut self, token: u64) {
        if let Some(scheduler) = &mut self.scheduler {
            scheduler.acknowledge_cancelled(token);
        }
    }
    pub fn set_tag_catalog(&mut self, context: Context, tags: Vec<Tag>) {
        self.catalog_context = Some(context);
        self.available_tags = tags;
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
        let op = match op {
            Operation::Move { ids, placement } if !cabinet.snapshot().hierarchy_complete => {
                Operation::MovePreflight { ids, placement }
            }
            other => other,
        };
        match cabinet.request(op) {
            Ok(r) => {
                out.requests.push(r);
                self.last_error = None;
            }
            Err(e) => self.last_error = Some(e.to_string()),
        }
    }
    fn begin_create(&mut self, parent: Option<EntryId>) {
        self.edit = Some(Edit::Create { parent });
        self.text.clear();
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
        if let Some(e) = cabinet.entry(&id) {
            self.text = e.label.clone();
            self.edit = Some(Edit::Rename { id });
            self.edit_focus = true;
        }
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
        if self.catalog_context.as_ref().is_some_and(|context| {
            context.scope != cabinet.snapshot().context.scope
                || context.generation != cabinet.snapshot().context.generation
        }) {
            self.available_tags.clear();
            self.catalog_context = None;
        }
        self.peak.settings = cabinet.settings().peak.clone();
        let mut peak_hovered = false;
        let mut draft_cancelled = false;
        if self.context.as_ref() != Some(&cabinet.snapshot().context) {
            self.click = None;
            self.color_draft = None;
            self.tag_draft = None;
            self.moving = None;
            self.move_targets = None;
            self.peak.dismiss();
            // Never re-fence an older inline draft against a refreshed canonical revision.
            if self.context.is_some() && self.edit.is_some() {
                draft_cancelled = true;
                self.edit = None;
                self.text.clear();
                self.last_error=Some("Entry changed while editing; draft cancelled. Rename again using the current entry.".into());
            }
            if self.context.is_some() && self.delete.take().is_some() {
                draft_cancelled = true;
                self.last_error = Some("Entries changed; delete confirmation cancelled. Review the current selection before deleting.".into());
            }
            if self.context.as_ref().is_some_and(|c| {
                c.scope != cabinet.snapshot().context.scope
                    || c.generation != cabinet.snapshot().context.generation
            }) {
                self.edit = None;
                self.text.clear();
                self.quick_path.clear();
                self.workspace_query.clear();
                self.host_search = false;
                if let Some(scheduler) = self.scheduler.as_mut() {
                    scheduler.cancel_all();
                    scheduler.clear_cache();
                }
                self.delete = None;
                self.hover = None;
                self.last_error = None;
                self.cancel_drag(ui);
            }
            self.context = Some(cabinet.snapshot().context.clone());
        }
        let palette = self.palette.unwrap_or_else(|| Palette::from_ui(ui));
        let focus_changed = self.last_focus.as_deref() != cabinet.focused();
        if ui.input(|i| i.key_pressed(Key::Escape)) {
            self.edit = None;
            self.delete = None;
            self.hover = None;
            self.cancel_drag(ui);
            self.click = None;
            self.color_draft = None;
            self.tag_draft = None;
            self.moving = None;
            self.move_targets = None;
            self.peak.dismiss();
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
                        Err(e) => self.last_error = Some(e.to_string()),
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
                        Err(e) => self.last_error = Some(e.to_string()),
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
                ui.add(egui::TextEdit::singleline(&mut self.quick_path).hint_text("Go to loaded path"));
                if ui.button("Go").clicked(){if let Err(e)=cabinet.navigate_path(&self.quick_path){self.last_error=Some(e.to_string());}}
            });
            ui.horizontal(|ui|{
                ui.selectable_value(&mut self.host_search,false,"Loaded tree");
                let mode=ui.selectable_value(&mut self.host_search,true,"Workspace search (host)");
                self.control(ui,&mode,"host-search-mode","Workspace search",&mut out);
            });
            let mut filter = if self.host_search {self.workspace_query.clone()} else {cabinet.filter().to_owned()};
            let f = ui.add(
                egui::TextEdit::singleline(&mut filter)
                    .hint_text("Filter files and folders")
                    .desired_width(f32::INFINITY)
                    .id_salt("filter"),
            );
            self.control(ui, &f, "filter", "Filter files and folders", &mut out);
            if f.changed() && self.host_search {self.workspace_query=filter.clone();}
            if f.changed() && !self.host_search {
                cabinet.set_filter(&filter);
            }
            if self.host_search {
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
            let hidden_edit=match &self.edit {
                Some(Edit::Rename{id})=>!visible.iter().any(|r|&r.id==id),
                Some(Edit::Create{parent:Some(id)})=>!visible.iter().any(|r|&r.id==id),
                _=>false,
            };
            if hidden_edit {
                draft_cancelled=true;
                self.edit=None;self.text.clear();self.edit_focus=false;
                self.last_error=Some("Editing cancelled because the entry is hidden by the current filter or collapsed folder.".into());
            }
            if let Some(ids) = self.delete.clone() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(format!("Delete {} selected item(s)?", ids.len()));
                    let yes = ui.button("Delete");
                    self.control(ui, &yes, "confirm-delete", "Confirm delete", &mut out);
                    if yes.clicked() {
                        self.emit(cabinet, Operation::Delete { ids }, &mut out);
                        self.delete = None;
                    }
                    let no = ui.button("Cancel");
                    self.control(ui, &no, "cancel-delete", "Cancel delete", &mut out);
                    if no.clicked() {
                        self.delete = None;
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
                .last_error
                .clone()
                .or_else(|| cabinet.error().map(str::to_owned));
            if let Some(e) = &out.error {
                ui.colored_label(palette.danger, e);
            }
            let keyboard = !draft_cancelled && !f.has_focus() && self.edit.is_none() && self.delete.is_none() && self.color_draft.is_none() && self.tag_draft.is_none() && self.moving.is_none();
            let mut keyboard_handled = false;
            let projected=cabinet.projected_rows();
            let draft_index=match &self.edit {
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
                        if options.clicked() { self.click=None; self.peak.dismiss(); }
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
                        let editing = matches!(&self.edit,Some(Edit::Rename{id}) if id==&entry.id);
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
                        if response.hovered() && !editing && self.edit.is_none() && self.color_draft.is_none() && self.tag_draft.is_none() && !options.clicked() && egui::DragAndDrop::payload::<Drag>(ui.ctx()).is_none() {
                            peak_hovered=true;
                            self.peak_anchor=rect;
                            self.peak.hover(peak::Key { scope:cabinet.snapshot().context.scope.clone(), generation:cabinet.snapshot().context.generation,context_revision:cabinet.snapshot().context.revision,entry_id:entry.id.clone(),resource_id:entry.resource_id.clone(),entry_revision:entry.revision },entry.label.clone(),entry.tags.clone(),now_ms);
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
                                if modified { self.click=None; }
                                if !modified && entry.kind==EntryKind::Leaf && cabinet.settings().click_behavior==ClickBehavior::SingleOpenDoubleRename {
                                    if self.click.as_ref().is_some_and(|(target,context,start)|target==&entry.id && context==&cabinet.snapshot().context && now_ms.saturating_sub(*start)<=cabinet.settings().double_click_ms) {
                                        self.click=None;
                                        if entry.capabilities.rename { self.begin_rename(cabinet,entry.id.clone()); }
                                    } else {
                                        self.click=Some((entry.id.clone(),cabinet.snapshot().context.clone(),now_ms));
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
                            self.click=None;
                            self.peak.dismiss();
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
                        if keyboard && self.edit.is_none() && self.delete.is_none() && !keyboard_handled && response.has_focus() {
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
                self.hover = None;
            }
            if ui.input(|i| i.key_pressed(Key::Escape)) {
                self.edit = None;
                self.delete = None;
                self.hover = None;
                self.cancel_drag(ui);
            }

        });
        cabinet.record_rendered(out.rows.len());
        out.diagnostics = cabinet.diagnostics();
        self.last_focus = cabinet.focused().map(str::to_owned);
        let peak_contains_pointer = self.peak.stage() != peak::Stage::Hidden
            && self.peak_rect.is_some_and(|rect| {
                ui.input(|i| i.pointer.hover_pos().is_some_and(|p| rect.contains(p)))
            });
        if peak_contains_pointer {
            self.peak.retain();
        } else if !peak_hovered {
            self.peak.leave(now_ms);
        }
        if self.scheduler.is_some() {
            if let Some(scheduler) = &mut self.scheduler {
                if let Err(error) = self.peak.tick_scheduled(scheduler, now_ms) {
                    out.error = Some(format!("{error:?}"));
                }
                out.preview_jobs = scheduler.take_ready();
                out.preview_cancellations = scheduler.take_cancellations();
            }
        } else if let Some(request) = self.peak.tick(now_ms) {
            out.preview_requests.push(request);
        }
        if self.peak.stage() != peak::Stage::Hidden {
            self.peak_rect =
                self.peak_view
                    .show(ui.ctx(), &mut self.peak, self.peak_anchor, now_ms);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        } else if peak_hovered {
            self.peak_rect = None;
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        if self.peak.settings != cabinet.settings().peak {
            let mut settings = cabinet.settings().clone();
            settings.peak = self.peak.settings.clone();
            let _ = cabinet.apply_settings(settings);
        }
        ui.push_id((&ns, &scope, generation), |ui| {
            self.dialogs(ui, cabinet, &mut out)
        });
        if let Some((id, context, start)) = self.click.clone() {
            if now_ms.saturating_sub(start) > cabinet.settings().double_click_ms {
                self.click = None;
                if context == cabinet.snapshot().context
                    && cabinet.rows().iter().any(|r| r.id == id)
                    && self.edit.is_none()
                    && self.color_draft.is_none()
                    && self.tag_draft.is_none()
                    && self.delete.is_none()
                    && self.moving.is_none()
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
    fn entry_menu(
        &mut self,
        ui: &mut Ui,
        cabinet: &mut Cabinet,
        entry: &Entry,
        selected: bool,
        out: &mut ViewResponse,
    ) {
        let idle = cabinet.pending().is_none();
        for (key, label, enabled, op) in [
            (
                "open",
                "Open",
                entry.capabilities.open,
                Operation::Open {
                    id: entry.id.clone(),
                },
            ),
            (
                "duplicate",
                "Duplicate",
                entry.capabilities.duplicate,
                Operation::Duplicate {
                    id: entry.id.clone(),
                },
            ),
            (
                "download",
                "Download",
                entry.capabilities.download,
                Operation::Download {
                    id: entry.id.clone(),
                },
            ),
            (
                "open-parent",
                "Open parent folder",
                entry.capabilities.open_parent,
                Operation::OpenParent {
                    id: entry.id.clone(),
                },
            ),
            (
                "copy-path",
                "Copy path",
                entry.capabilities.copy_reference,
                Operation::CopyPath {
                    id: entry.id.clone(),
                },
            ),
            (
                "copy-link",
                "Copy link",
                entry.capabilities.copy_reference,
                Operation::CopyLink {
                    id: entry.id.clone(),
                },
            ),
            (
                "properties",
                "Properties",
                entry.capabilities.properties,
                Operation::Properties {
                    id: entry.id.clone(),
                },
            ),
        ] {
            let action = ui.add_enabled(idle && enabled, egui::Button::new(label));
            self.control(ui, &action, &format!("menu-{key}:{}", entry.id), label, out);
            if action.clicked() {
                self.click = None;
                self.peak.dismiss();
                self.emit(cabinet, op, out);
                ui.close();
            }
        }
        ui.separator();
        if ui
            .add_enabled(
                idle && entry.kind == EntryKind::Folder && entry.capabilities.create,
                egui::Button::new("New folder"),
            )
            .clicked()
        {
            let _ = cabinet.expand(&entry.id, true);
            self.begin_create(Some(entry.id.clone()));
            self.click = None;
            ui.close();
        }
        if ui
            .add_enabled(
                idle && entry.capabilities.rename,
                egui::Button::new("Rename"),
            )
            .clicked()
        {
            self.begin_rename(cabinet, entry.id.clone());
            self.click = None;
            ui.close();
        }
        let move_button = ui.add_enabled(
            idle && entry.capabilities.move_entry,
            egui::Button::new("Move to…"),
        );
        self.control(
            ui,
            &move_button,
            &format!("menu-move:{}", entry.id),
            "Move to folder",
            out,
        );
        if move_button.clicked() {
            self.moving = Some(entry.id.clone());
            self.click = None;
            ui.close();
        }
        let color_button = ui.add_enabled(
            idle && entry.capabilities.color,
            egui::Button::new("Color…"),
        );
        self.control(
            ui,
            &color_button,
            &format!("menu-color:{}", entry.id),
            "Edit entry color",
            out,
        );
        if color_button.clicked() {
            let color = entry.color.unwrap_or([110, 150, 230]);
            self.color_draft = Some(ColorDraft {
                id: entry.id.clone(),
                revision: entry.revision,
                color,
                hex: rgb_hex(color),
            });
            self.click = None;
            ui.close();
        }
        if ui
            .add_enabled(
                idle && entry.capabilities.tag,
                egui::Button::new("Tags and tag colors…"),
            )
            .clicked()
        {
            self.tag_draft = Some(TagDraft {
                id: entry.id.clone(),
                revision: entry.revision,
                tags: entry.tags.clone(),
                hexes: entry
                    .tags
                    .iter()
                    .map(|t| (t.id.clone(), rgb_hex(t.color)))
                    .collect(),
                new_id: String::new(),
                new_label: String::new(),
            });
            self.click = None;
            ui.close();
        }
        if entry.kind == EntryKind::Folder {
            let page = ui.add_enabled(
                idle && !matches!(entry.children, ChildrenState::Loaded),
                egui::Button::new("Load next page / retry"),
            );
            self.control(
                ui,
                &page,
                &format!("menu-load-page:{}", entry.id),
                "Load next page or retry",
                out,
            );
            if page.clicked() {
                match cabinet.request_child_page(&entry.id) {
                    Ok(request) => out.requests.push(request),
                    Err(e) => self.last_error = Some(e.to_string()),
                }
                ui.close();
            }
            let pinned = cabinet.settings().pinned_folders.contains(&entry.id);
            if ui
                .button(if pinned { "Unpin folder" } else { "Pin folder" })
                .clicked()
            {
                let _ = cabinet.pin_folder(&entry.id, !pinned);
                ui.close();
            }
            ui.menu_button("Sort this folder", |ui| {
                let inherit = !cabinet.settings().folder_sorts.contains_key(&entry.id);
                if ui.selectable_label(inherit, "Use tree default").clicked() {
                    let _ = cabinet.set_folder_sort(&entry.id, None);
                    ui.close();
                }
                for sort in sort_options() {
                    if ui
                        .selectable_label(
                            !inherit && cabinet.effective_sort(Some(&entry.id)) == sort,
                            sort_label(sort),
                        )
                        .clicked()
                    {
                        let _ = cabinet.set_folder_sort(&entry.id, Some(sort));
                        ui.close();
                    }
                }
            });
            if ui.button("Expand folder").clicked() {
                self.expand(cabinet, &entry.id, true, out);
                ui.close();
            }
            if ui.button("Collapse folder").clicked() {
                let _ = cabinet.expand(&entry.id, false);
                ui.close();
            }
        }
        if ui
            .add_enabled(
                idle && entry.capabilities.delete,
                egui::Button::new("Delete…"),
            )
            .clicked()
        {
            self.delete = Some(if selected {
                cabinet.selected()
            } else {
                vec![entry.id.clone()]
            });
            self.click = None;
            ui.close();
        }
    }
    fn root_target(
        &mut self,
        ui: &mut Ui,
        cabinet: &mut Cabinet,
        palette: Palette,
        out: &mut ViewResponse,
    ) {
        let (root_rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
        let root = ui.interact(root_rect, ui.id().with("root-drop"), Sense::hover());
        if egui::DragAndDrop::payload::<Drag>(ui.ctx())
            .is_some_and(|drag| drag.namespace == self.namespace)
        {
            ui.painter()
                .rect_filled(root_rect, 3.0, ui.visuals().faint_bg_color);
            ui.painter().text(
                root_rect.center(),
                egui::Align2::CENTER_CENTER,
                "Drop at root",
                egui::FontId::proportional(13.0),
                palette.muted,
            );
        }
        self.control(ui, &root, "root-drop", "Move to root", out);
        self.drop_target(ui, cabinet, &root, Placement::Into(None), palette, out);
    }
    fn dialogs(&mut self, ui: &mut Ui, cabinet: &mut Cabinet, out: &mut ViewResponse) {
        if let Some(id) = self.moving.clone() {
            let mut close = false;
            egui::Window::new("Move to folder")
                .id(ui.id().with("move-dialog"))
                .resizable(true)
                .show(ui.ctx(), |ui| {
                    if self.move_targets.as_ref().is_none_or(|cached| {
                        cached.context != cabinet.snapshot().context || cached.source != id
                    }) {
                        self.move_targets = Some(MoveTargets {
                            context: cabinet.snapshot().context.clone(),
                            source: id.clone(),
                            targets: cabinet.move_destinations(&id),
                        });
                    }
                    let destinations = self
                        .move_targets
                        .as_ref()
                        .map(|cached| cached.targets.as_slice())
                        .unwrap_or_default();
                    let mut chosen = None;
                    egui::ScrollArea::vertical().max_height(260.0).show_rows(
                        ui,
                        26.0,
                        destinations.len(),
                        |ui, range| {
                            for index in range {
                                let (parent, allowed) = &destinations[index];
                                let label = parent
                                    .as_ref()
                                    .map(|id| cabinet.full_path(id).unwrap_or_else(|_| id.clone()))
                                    .unwrap_or_else(|| "Tree root".to_owned());
                                let target = parent.as_deref().unwrap_or("root");
                                let destination = ui.add_enabled(
                                    *allowed && cabinet.pending().is_none(),
                                    egui::Button::new(label),
                                );
                                self.control(
                                    ui,
                                    &destination,
                                    &format!("move-destination:{target}"),
                                    "Move destination",
                                    out,
                                );
                                if destination.clicked() {
                                    chosen = Some(Placement::Into(parent.clone()));
                                }
                            }
                        },
                    );
                    if let Some(placement) = chosen {
                        self.emit(
                            cabinet,
                            Operation::Move {
                                ids: vec![id.clone()],
                                placement,
                            },
                            out,
                        );
                        close = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            if close {
                self.moving = None;
                self.move_targets = None;
            }
        }
        if let Some(mut draft) = self.color_draft.take() {
            if cabinet
                .entry(&draft.id)
                .is_none_or(|e| e.revision != draft.revision)
            {
                return;
            }
            let mut close = false;
            egui::Window::new("Entry color")
                .id(ui.id().with("color-dialog"))
                .show(ui.ctx(), |ui| {
                    let (wheel, hex) = color_wheel(ui, &mut draft.color, &mut draft.hex);
                    out.controls.insert("color-wheel".into(), wheel);
                    out.controls.insert("color-hex".into(), hex);
                    ui.horizontal(|ui| {
                        let apply = ui.add_enabled(
                            parse_hex(&draft.hex).is_some() && cabinet.pending().is_none(),
                            egui::Button::new("Apply"),
                        );
                        self.control(ui, &apply, "apply-color", "Apply entry color", out);
                        if apply.clicked() {
                            self.emit(
                                cabinet,
                                Operation::SetColor {
                                    id: draft.id.clone(),
                                    color: Some(draft.color),
                                },
                                out,
                            );
                            close = true;
                        }
                        if ui.button("Clear").clicked() {
                            self.emit(
                                cabinet,
                                Operation::SetColor {
                                    id: draft.id.clone(),
                                    color: None,
                                },
                                out,
                            );
                            close = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            if !close {
                self.color_draft = Some(draft);
            }
        }
        if let Some(mut draft) = self.tag_draft.take() {
            if cabinet
                .entry(&draft.id)
                .is_none_or(|e| e.revision != draft.revision)
            {
                return;
            }
            let mut close = false;
            egui::Window::new("Tags and colors")
                .id(ui.id().with("tag-dialog"))
                .resizable(true)
                .show(ui.ctx(), |ui| {
                    for tag in &self.available_tags {
                        if !draft.tags.iter().any(|t| t.id == tag.id)
                            && ui.button(format!("Add {}", tag.label)).clicked()
                        {
                            draft.tags.push(tag.clone());
                        }
                    }
                    let mut remove = None;
                    for (index, tag) in draft.tags.iter_mut().enumerate() {
                        ui.push_id(tag.id.clone(), |ui| {
                            ui.horizontal(|ui| {
                                ui.label(&tag.label);
                                if ui.small_button("Remove").clicked() {
                                    remove = Some(index);
                                }
                            });
                            let hex = draft
                                .hexes
                                .entry(tag.id.clone())
                                .or_insert_with(|| rgb_hex(tag.color));
                            let (wheel, hex) = color_wheel(ui, &mut tag.color, hex);
                            out.controls.insert(format!("tag-wheel:{}", tag.id), wheel);
                            out.controls.insert(format!("tag-hex:{}", tag.id), hex);
                        });
                    }
                    if let Some(index) = remove {
                        draft.tags.remove(index);
                    }
                    ui.separator();
                    ui.label("New tag (host validates stable ID)");
                    ui.add(
                        egui::TextEdit::singleline(&mut draft.new_id).hint_text("Stable tag ID"),
                    );
                    ui.add(egui::TextEdit::singleline(&mut draft.new_label).hint_text("Tag name"));
                    if ui
                        .add_enabled(
                            !draft.new_id.trim().is_empty()
                                && !draft.new_label.trim().is_empty()
                                && !draft.tags.iter().any(|t| t.id == draft.new_id),
                            egui::Button::new("Add tag"),
                        )
                        .clicked()
                    {
                        draft.tags.push(Tag {
                            id: std::mem::take(&mut draft.new_id),
                            label: std::mem::take(&mut draft.new_label),
                            color: [110, 150, 230],
                        });
                    }
                    let valid = draft.hexes.values().all(|hex| parse_hex(hex).is_some());
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(
                                valid && cabinet.pending().is_none(),
                                egui::Button::new("Apply tags"),
                            )
                            .clicked()
                        {
                            let before = out.requests.len();
                            self.emit(
                                cabinet,
                                Operation::SetTags {
                                    id: draft.id.clone(),
                                    tags: draft.tags.clone(),
                                },
                                out,
                            );
                            close = out.requests.len() > before;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
            if !close {
                self.tag_draft = Some(draft);
            }
        }
    }
    fn expand(&mut self, cabinet: &mut Cabinet, id: &str, value: bool, out: &mut ViewResponse) {
        if let Err(e) = cabinet.expand(id, value) {
            self.last_error = Some(e.to_string());
            return;
        }
        if value
            && cabinet.entry(id).is_some_and(|e| {
                matches!(
                    e.children,
                    ChildrenState::Unloaded | ChildrenState::Failed(_)
                )
            })
        {
            match cabinet.request_child_page(id) {
                Ok(request) => out.requests.push(request),
                Err(error) => self.last_error = Some(error.to_string()),
            }
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
        let edit = egui::TextEdit::singleline(&mut self.text)
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
            let op = match self.edit.as_ref().unwrap() {
                Edit::Create { parent } => Operation::Create {
                    parent: parent.clone(),
                    name: self.text.clone(),
                    kind: EntryKind::Folder,
                },
                Edit::Rename { id } => Operation::Rename {
                    id: id.clone(),
                    name: self.text.clone(),
                },
            };
            let before = out.requests.len();
            self.emit(cabinet, op, out);
            if out.requests.len() > before {
                self.edit = None;
                self.text.clear();
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
            self.click = None;
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
                                Err(error) => self.last_error = Some(error.to_string()),
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
                self.delete = Some(ids);
            }
        }
        if self.edit.is_none() && self.delete.is_none() && ui.input(|i| i.key_pressed(Key::Enter)) {
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
                Err(e) => self.last_error = Some(e.to_string()),
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
            if self.hover.as_ref().is_none_or(|(id, _)| id != &entry.id) {
                self.hover = Some((entry.id.clone(), now));
            }
            if self
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
            self.hover = None;
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
                self.last_error =
                    Some("Move unavailable: stale, denied or invalid destination".into());
            }
        }
    }
}
fn presentation_label(entry: &Entry) -> String {
    if entry.kind != EntryKind::Folder {
        return entry.label.clone();
    }
    match &entry.children {
        ChildrenState::Loaded => entry.label.clone(),
        ChildrenState::Unloaded => format!("{} · Not loaded", entry.label),
        ChildrenState::Loading => format!("{} · Loading…", entry.label),
        ChildrenState::Failed(reason) => format!("{} · Failed: {}", entry.label, reason),
    }
}
fn sort_label(s: Sort) -> &'static str {
    match s {
        Sort::Manual => "Manual order",
        Sort::NameAscending => "Name A–Z",
        Sort::NameDescending => "Name Z–A",
        Sort::CreatedDescending => "Newest created",
        Sort::CreatedAscending => "Oldest created",
        Sort::ModifiedDescending => "Recently modified",
        Sort::ModifiedAscending => "Oldest modified",
        Sort::FileSizeAscending => "Size smallest first",
        Sort::FileSizeDescending => "Size largest first",
        Sort::FileTypeAscending => "File type A–Z",
        Sort::FileTypeDescending => "File type Z–A",
    }
}
fn sort_options() -> [Sort; 11] {
    [
        Sort::Manual,
        Sort::NameAscending,
        Sort::NameDescending,
        Sort::CreatedDescending,
        Sort::CreatedAscending,
        Sort::ModifiedDescending,
        Sort::ModifiedAscending,
        Sort::FileSizeAscending,
        Sort::FileSizeDescending,
        Sort::FileTypeAscending,
        Sort::FileTypeDescending,
    ]
}
fn rgb_hex(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}
fn parse_hex(hex: &str) -> Option<[u8; 3]> {
    let value = hex.trim().strip_prefix('#').unwrap_or(hex.trim());
    if value.len() != 6 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some([
        u8::from_str_radix(&value[0..2], 16).ok()?,
        u8::from_str_radix(&value[2..4], 16).ok()?,
        u8::from_str_radix(&value[4..6], 16).ok()?,
    ])
}
/// Hue/saturation disk with a brightness slider; changes remain a draft until Apply.
fn color_wheel(ui: &mut Ui, rgb: &mut [u8; 3], hex: &mut String) -> (Rect, Rect) {
    let mut hsv = egui::ecolor::Hsva::from_srgb(*rgb);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(130.0), Sense::click_and_drag());
    let radius = 60.0;
    let center = rect.center();
    let mut mesh = egui::epaint::Mesh::default();
    let n = 72;
    mesh.colored_vertex(
        center,
        Color32::from(egui::ecolor::Hsva::new(0.0, 0.0, hsv.v, 1.0)),
    );
    for i in 0..=n {
        let h = i as f32 / n as f32;
        let a = h * std::f32::consts::TAU;
        mesh.colored_vertex(
            center + Vec2::new(a.cos(), a.sin()) * radius,
            Color32::from(egui::ecolor::Hsva::new(h, 1.0, hsv.v, 1.0)),
        );
    }
    for i in 0..n {
        mesh.add_triangle(0, i + 1, i + 2);
    }
    ui.painter().add(egui::Shape::mesh(mesh));
    let mut changed = false;
    if (response.clicked() || response.dragged()) && response.interact_pointer_pos().is_some() {
        let delta = response.interact_pointer_pos().unwrap() - center;
        hsv.h = (delta.y.atan2(delta.x) / std::f32::consts::TAU).rem_euclid(1.0);
        hsv.s = (delta.length() / radius).clamp(0.0, 1.0);
        changed = true;
    }
    let angle = hsv.h * std::f32::consts::TAU;
    ui.painter().circle_stroke(
        center + Vec2::new(angle.cos(), angle.sin()) * radius * hsv.s,
        4.0,
        Stroke::new(2.0_f32, Color32::WHITE),
    );
    changed |= ui
        .add(egui::Slider::new(&mut hsv.v, 0.0..=1.0).text("Brightness"))
        .changed();
    if changed {
        *rgb = hsv.to_srgb();
        *hex = rgb_hex(*rgb);
    }
    let hex_response = ui.add(egui::TextEdit::singleline(hex).hint_text("#RRGGBB"));
    if hex_response.changed() {
        if let Some(color) = parse_hex(hex) {
            *rgb = color;
        }
    }
    if parse_hex(hex).is_none() {
        ui.colored_label(ui.visuals().error_fg_color, "Enter six hexadecimal digits");
    }
    (rect, hex_response.rect)
}
fn folder_plus(ui: &mut Ui, p: Palette) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(28.0), Sense::click());
    let painter = ui.painter();
    let color = if response.hovered() { p.accent } else { p.text };
    let r = rect.shrink(6.0);
    painter.rect_stroke(
        r,
        2.0,
        Stroke::new(1.5_f32, color),
        egui::StrokeKind::Inside,
    );
    painter.line_segment(
        [r.left_top(), r.left_top() + Vec2::new(6.0, -3.0)],
        Stroke::new(2.0_f32, color),
    );
    let c = r.center();
    painter.line_segment(
        [c - Vec2::new(3.0, 0.0), c + Vec2::new(3.0, 0.0)],
        Stroke::new(1.5_f32, color),
    );
    painter.line_segment(
        [c - Vec2::new(0.0, 3.0), c + Vec2::new(0.0, 3.0)],
        Stroke::new(1.5_f32, color),
    );
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "New folder"));
    response.on_hover_text("New folder")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn host_tag_catalog_cannot_survive_workspace_generation_change() {
        let ctx = egui::Context::default();
        let mut view = CabinetView::new("catalog-test");
        let mut cabinet = fixture();
        view.set_tag_catalog(
            cabinet.snapshot().context.clone(),
            vec![Tag {
                id: "private-tag".into(),
                label: "Private tag".into(),
                color: [10, 20, 30],
            }],
        );
        frame(&ctx, &mut view, &mut cabinet, vec![]);
        assert_eq!(view.available_tags.len(), 1);
        let mut next = cabinet.snapshot().clone();
        next.context.generation += 1;
        cabinet.replace(next).unwrap();
        frame(&ctx, &mut view, &mut cabinet, vec![]);
        assert!(view.available_tags.is_empty());
        assert!(view.catalog_context.is_none());
    }
    fn fixture() -> Cabinet {
        Cabinet::new(Snapshot {
            context: Context {
                scope: "test".into(),
                generation: 1,
                revision: 1,
            },
            hierarchy_complete: true,
            entries: vec![
                Entry::new("folder", None, "Folder", EntryKind::Folder),
                Entry::new("leaf", Some("folder".into()), "Leaf", EntryKind::Leaf),
            ],
        })
        .unwrap()
    }
    fn frame(
        ctx: &egui::Context,
        view: &mut CabinetView,
        cabinet: &mut Cabinet,
        events: Vec<egui::Event>,
    ) -> ViewResponse {
        let mut result = None;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(480.0, 420.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    result = Some(view.show(ui, cabinet));
                });
            },
        );
        result.unwrap()
    }
    fn click(
        ctx: &egui::Context,
        view: &mut CabinetView,
        cabinet: &mut Cabinet,
        pos: egui::Pos2,
    ) -> ViewResponse {
        frame(
            ctx,
            view,
            cabinet,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        frame(
            ctx,
            view,
            cabinet,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        )
    }
    #[test]
    fn folder_plus_opens_real_input_and_enter_emits_create_without_mutating_source() {
        let ctx = egui::Context::default();
        let mut view = CabinetView::default();
        let mut cabinet = fixture();
        let out = frame(&ctx, &mut view, &mut cabinet, vec![]);
        let pos = out.controls["new-folder"].center();
        click(&ctx, &mut view, &mut cabinet, pos);
        let input = frame(&ctx, &mut view, &mut cabinet, vec![]);
        assert!(input.controls.contains_key("inline-edit"));
        frame(
            &ctx,
            &mut view,
            &mut cabinet,
            vec![egui::Event::Text("Created".into())],
        );
        let result = frame(
            &ctx,
            &mut view,
            &mut cabinet,
            vec![egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        assert!(
            matches!(&result.requests[0].operation,Operation::Create{name,parent:None,..} if name=="Created")
        );
        assert_eq!(cabinet.snapshot().entries.len(), 2);
    }
    #[test]
    fn disclosure_click_expands_and_row_click_selects_stable_identity() {
        let ctx = egui::Context::default();
        let mut view = CabinetView::default();
        let mut cabinet = fixture();
        let out = frame(&ctx, &mut view, &mut cabinet, vec![]);
        let r = out.rows["folder"];
        click(
            &ctx,
            &mut view,
            &mut cabinet,
            r.left_center() + Vec2::new(10.0, 0.0),
        );
        assert!(cabinet.expanded("folder"));
        let out = frame(&ctx, &mut view, &mut cabinet, vec![]);
        click(&ctx, &mut view, &mut cabinet, out.rows["leaf"].center());
        assert_eq!(cabinet.selected(), vec!["leaf".to_owned()]);
    }
    fn drag_fixture() -> Cabinet {
        Cabinet::new(Snapshot {
            context: Context {
                scope: "test".into(),
                generation: 1,
                revision: 1,
            },
            hierarchy_complete: true,
            entries: vec![
                Entry::new("source", None, "Source", EntryKind::Folder),
                Entry::new("target", None, "Target", EntryKind::Folder),
                Entry::new("leaf", Some("source".into()), "Leaf", EntryKind::Leaf),
            ],
        })
        .unwrap()
    }
    fn press(ctx: &egui::Context, view: &mut CabinetView, cabinet: &mut Cabinet, pos: egui::Pos2) {
        frame(
            ctx,
            view,
            cabinet,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        frame(
            ctx,
            view,
            cabinet,
            vec![egui::Event::PointerMoved(pos + Vec2::new(12.0, 0.0))],
        );
    }
    fn release(
        ctx: &egui::Context,
        view: &mut CabinetView,
        cabinet: &mut Cabinet,
        pos: egui::Pos2,
    ) -> ViewResponse {
        frame(ctx, view, cabinet, vec![egui::Event::PointerMoved(pos)]);
        frame(
            ctx,
            view,
            cabinet,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        )
    }
    #[test]
    fn actual_pointer_folder_and_leaf_drag_into_folder_emit_fenced_moves() {
        for source in ["source", "leaf"] {
            let ctx = egui::Context::default();
            let mut view = CabinetView::default();
            let mut cabinet = drag_fixture();
            cabinet.expand("source", true).unwrap();
            let layout = frame(&ctx, &mut view, &mut cabinet, vec![]);
            let from = layout.rows[source].center();
            let to = layout.rows["target"].center();
            press(&ctx, &mut view, &mut cabinet, from);
            let hover = frame(
                &ctx,
                &mut view,
                &mut cabinet,
                vec![egui::Event::PointerMoved(to)],
            );
            assert!(hover.drop_target.as_ref().is_some_and(|d| d.valid));
            let out = release(&ctx, &mut view, &mut cabinet, to);
            assert!(
                matches!(out.requests.first().map(|r|&r.operation),Some(Operation::Move{ids,placement:Placement::Into(Some(id))}) if ids==&vec![source.to_owned()]&&id=="target"),
                "source={source} expected=Into(target) requests={:?} drop={:?} layout={:?} hover={:?} release={:?}",out.requests,out.drop_target,layout.rows,hover.rows,out.rows
            );
            assert_eq!(
                cabinet.entry(source).unwrap().parent,
                if source == "leaf" {
                    Some("source".into())
                } else {
                    None
                }
            );
        }
    }
    #[test]
    fn actual_pointer_drag_supports_manual_before_after_and_root() {
        for placement in [
            Placement::Before("target".into()),
            Placement::After("target".into()),
            Placement::Into(None),
        ] {
            let ctx = egui::Context::default();
            let mut view = CabinetView::default();
            let mut cabinet = drag_fixture();
            cabinet.expand("source", true).unwrap();
            let source = if matches!(&placement, Placement::Into(None)) {
                "leaf"
            } else {
                "source"
            };
            let layout = frame(&ctx, &mut view, &mut cabinet, vec![]);
            let from = layout.rows[source].center();
            let to = match &placement {
                Placement::Before(_) => layout.rows["target"].center_top() + Vec2::new(0.0, 2.0),
                Placement::After(_) => layout.rows["target"].center_bottom() - Vec2::new(0.0, 2.0),
                _ => layout.controls["root-drop"].center(),
            };
            press(&ctx, &mut view, &mut cabinet, from);
            let out = release(&ctx, &mut view, &mut cabinet, to);
            assert!(
                matches!(out.requests.first().map(|r|&r.operation),Some(Operation::Move{ids,placement:p}) if ids==&vec![source.to_owned()]&&p==&placement),
                "source={source} expected={placement:?} requests={:?} drop={:?} layout={:?} release={:?} controls={:?}",out.requests,out.drop_target,layout.rows,out.rows,out.controls
            );
        }
    }
    #[test]
    fn actual_pointer_escape_and_stale_context_cancel_without_host_mutation() {
        for stale in [false, true] {
            let ctx = egui::Context::default();
            let mut view = CabinetView::default();
            let mut cabinet = drag_fixture();
            let layout = frame(&ctx, &mut view, &mut cabinet, vec![]);
            let from = layout.rows["source"].center();
            let to = layout.rows["target"].center();
            press(&ctx, &mut view, &mut cabinet, from);
            if stale {
                let mut snapshot = cabinet.snapshot().clone();
                snapshot.context.generation += 1;
                cabinet.replace(snapshot).unwrap();
                frame(&ctx, &mut view, &mut cabinet, vec![]);
            } else {
                frame(
                    &ctx,
                    &mut view,
                    &mut cabinet,
                    vec![egui::Event::Key {
                        key: Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    }],
                );
            }
            let out = release(&ctx, &mut view, &mut cabinet, to);
            assert!(out.requests.is_empty());
            assert!(cabinet.pending().is_none());
            assert_eq!(cabinet.entry("source").unwrap().parent, None);
        }
    }
    #[test]
    fn filtering_away_inline_rename_cancels_draft_and_reenables_tree_navigation() {
        let ctx = egui::Context::default();
        let mut view = CabinetView::default();
        let mut cabinet = fixture();
        frame(&ctx, &mut view, &mut cabinet, vec![]);
        view.begin_rename(&cabinet, "folder".into());
        frame(&ctx, &mut view, &mut cabinet, vec![]);
        cabinet.set_filter("no-match");
        let out = frame(&ctx, &mut view, &mut cabinet, vec![]);
        assert!(!out.controls.contains_key("inline-edit"));
        assert!(view.edit.is_none());
        assert!(view.text.is_empty());
        assert!(out.error.as_ref().is_some_and(|e| e.contains("hidden")));
        assert!(out.requests.is_empty());
        cabinet.set_filter("");
        let out = frame(&ctx, &mut view, &mut cabinet, vec![]);
        click(&ctx, &mut view, &mut cabinet, out.rows["folder"].center());
        let out = frame(
            &ctx,
            &mut view,
            &mut cabinet,
            vec![egui::Event::Key {
                key: Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        assert!(matches!(&out.requests[0].operation,Operation::Open{id} if id=="folder"));
    }
    #[test]
    fn lazy_folder_state_labels_include_actual_failure_reason() {
        let mut entry = Entry::new("f", None, "Folder", EntryKind::Folder);
        for (state, suffix) in [
            (ChildrenState::Unloaded, "Not loaded"),
            (ChildrenState::Loading, "Loading…"),
            (
                ChildrenState::Failed("Access expired".into()),
                "Failed: Access expired",
            ),
        ] {
            entry.children = state;
            assert_eq!(presentation_label(&entry), format!("Folder · {suffix}"));
        }
    }
}
