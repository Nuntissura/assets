use crate::{Inspector, ViewAction, ViewResponse};
use egui::Ui;
use hsscrap::interaction::Inspection;
use hsscrap::*;

impl Inspector {
    pub(super) fn tags(&mut self, ui: &mut Ui, inspection: &Inspection, out: &mut ViewResponse) {
        if self.selected_tag.as_ref().is_some_and(|tag| {
            !inspection
                .tag_choices
                .iter()
                .any(|choice| &choice.tag == tag)
        }) {
            self.selected_tag = None;
        }
        let draft = self.session.draft().cloned();
        let mut count = 0;
        for assignments in inspection.effective_tags().values() {
            for assignment in assignments {
                count += 1;
                let removed = draft
                    .as_ref()
                    .is_some_and(|value| value.removals.contains(&assignment.assignment_id));
                self.tag_row(ui, inspection, assignment, removed, false, out);
            }
        }
        if let Some(draft) = &draft {
            for assignment in &draft.additions {
                count += 1;
                self.tag_row(ui, inspection, assignment, false, true, out);
            }
        }
        if count == 0 {
            ui.label("No tags in the authorized item and contexts.");
        }
        if draft.is_none() {
            return;
        }
        ui.add_enabled_ui(self.session.editable(), |ui| {
            ui.separator();
            ui.label(egui::RichText::new("Add a canonical tag").strong());
            let tag = self.selected_tag.as_ref().and_then(|tag| {
                inspection
                    .tag_choices
                    .iter()
                    .find(|choice| &choice.tag == tag)
            });
            let response = egui::ComboBox::from_id_salt("tag-choice")
                .selected_text(
                    tag.map(|choice| choice.label.as_str())
                        .unwrap_or("Choose tag"),
                )
                .show_ui(ui, |ui| {
                    for choice in &inspection.tag_choices {
                        ui.selectable_value(
                            &mut self.selected_tag,
                            Some(choice.tag.clone()),
                            format!("{} · {}", choice.label, choice.tag.0.native_id),
                        );
                    }
                })
                .response;
            self.control(ui, &response, "tag-choice", "Choose canonical tag", out);
            let targets: Vec<_> = inspection
                .targets
                .iter()
                .filter(|target| target.writable)
                .collect();
            if self
                .selected_target
                .as_ref()
                .is_none_or(|target| !targets.iter().any(|value| &value.target == target))
            {
                self.selected_target = targets.first().map(|target| target.target.clone());
            }
            let target = targets
                .iter()
                .find(|value| Some(&value.target) == self.selected_target.as_ref());
            let response = egui::ComboBox::from_id_salt("tag-target")
                .selected_text(
                    target
                        .map(|value| value.label.as_str())
                        .unwrap_or("No writable context"),
                )
                .show_ui(ui, |ui| {
                    for target in &targets {
                        ui.selectable_value(
                            &mut self.selected_target,
                            Some(target.target.clone()),
                            &target.label,
                        );
                    }
                })
                .response;
            self.control(ui, &response, "tag-target", "Tag destination", out);
            let canonical_tag = self.selected_tag.as_ref().filter(|tag| {
                inspection
                    .tag_choices
                    .iter()
                    .any(|choice| &choice.tag == *tag)
            });
            let writable_target = self
                .selected_target
                .as_ref()
                .filter(|target| targets.iter().any(|value| &value.target == *target));
            let add = ui.add_enabled(
                canonical_tag.is_some() && writable_target.is_some(),
                egui::Button::new("Add tag to draft"),
            );
            self.control(ui, &add, "add-tag", "Add tag to draft", out);
            if add.clicked() {
                if let (Some(tag), Some(target)) = (canonical_tag, writable_target) {
                    out.actions.push(ViewAction::ChooseTag {
                        tag: tag.clone(),
                        target: target.clone(),
                    });
                }
            }
        });
    }

    fn tag_row(
        &mut self,
        ui: &mut Ui,
        inspection: &Inspection,
        assignment: &TagAssignment,
        removed: bool,
        added: bool,
        out: &mut ViewResponse,
    ) {
        let label = inspection
            .tag_choices
            .iter()
            .find(|choice| choice.tag == assignment.tag_ref)
            .map(|choice| choice.label.as_str())
            .unwrap_or(&assignment.tag_ref.0.native_id);
        let target = inspection.target(&assignment.target);
        ui.group(|ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(label).strong());
                ui.label(match assignment.origin {
                    TagOrigin::User => "User",
                    TagOrigin::Import => "Imported",
                    TagOrigin::LegacyUnknown => "Unknown origin",
                });
            });
            ui.label(format!(
                "{} · {}",
                target
                    .map(|target| target.label.as_str())
                    .unwrap_or("Unknown context"),
                target_kind(&assignment.target)
            ));
            if added {
                ui.label("New in this draft");
            }
            if removed {
                ui.label("Will be removed from its owning context on save");
            }
            if self.session.draft().is_some() {
                let name = format!("tag-{}", assignment.assignment_id);
                let text = if removed {
                    format!("Undo removal of {label}")
                } else {
                    format!("Remove {label} from {}", target_kind(&assignment.target))
                };
                let button = ui.add_enabled(
                    self.session.editable() && target.is_some_and(|target| target.writable),
                    egui::Button::new(&text),
                );
                self.control(ui, &button, &name, &text, out);
                if button.clicked() {
                    let result = if removed {
                        self.session.undo_removal(assignment.assignment_id)
                    } else {
                        self.session.remove_tag(assignment.assignment_id)
                    };
                    self.result(result);
                }
                if !target.is_some_and(|target| target.writable) {
                    ui.label("Read only at this owner");
                }
            }
        });
    }
}

fn target_kind(target: &TagTarget) -> &'static str {
    match target {
        TagTarget::Item(_) => "Item",
        TagTarget::Project(_) => "Project",
        TagTarget::Membership(_) => "Membership",
        TagTarget::Folder(_) => "Folder",
        TagTarget::Occurrence(_) => "Occurrence",
    }
}
