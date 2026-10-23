use super::*;

impl CabinetView {
    pub(super) fn dialogs(&mut self, ui: &mut Ui, cabinet: &mut Cabinet, out: &mut ViewResponse) {
        if let Some(id) = self.state.moving.clone() {
            let mut close = false;
            egui::Window::new("Move to folder")
                .id(ui.id().with("move-dialog"))
                .resizable(true)
                .show(ui.ctx(), |ui| {
                    if self.state.move_targets.as_ref().is_none_or(|cached| {
                        cached.context != cabinet.snapshot().context || cached.source != id
                    }) {
                        self.state.move_targets = Some(MoveTargets {
                            context: cabinet.snapshot().context.clone(),
                            source: id.clone(),
                            targets: cabinet.move_destinations(&id),
                        });
                    }
                    let destinations = self
                        .state
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
                self.state.moving = None;
                self.state.move_targets = None;
            }
        }
        if let Some(mut draft) = self.state.color_draft.take() {
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
                self.state.color_draft = Some(draft);
            }
        }
        if let Some(mut draft) = self.state.tag_draft.take() {
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
                    for tag in &self.state.available_tags {
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
                self.state.tag_draft = Some(draft);
            }
        }
    }
}
