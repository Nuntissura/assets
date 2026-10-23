use super::*;

impl CabinetView {
    pub(super) fn entry_menu(
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
                self.state.click = None;
                self.state.peak.dismiss();
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
            self.state.click = None;
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
            self.state.click = None;
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
            self.state.moving = Some(entry.id.clone());
            self.state.click = None;
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
            self.state.color_draft = Some(ColorDraft {
                id: entry.id.clone(),
                revision: entry.revision,
                color,
                hex: rgb_hex(color),
            });
            self.state.click = None;
            ui.close();
        }
        if ui
            .add_enabled(
                idle && entry.capabilities.tag,
                egui::Button::new("Tags and tag colors…"),
            )
            .clicked()
        {
            self.state.tag_draft = Some(TagDraft {
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
            self.state.click = None;
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
                    Err(e) => self.state.last_error = Some(e.to_string()),
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
            self.state.delete = Some(if selected {
                cabinet.selected()
            } else {
                vec![entry.id.clone()]
            });
            self.state.click = None;
            ui.close();
        }
    }
    pub(super) fn root_target(
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
}
