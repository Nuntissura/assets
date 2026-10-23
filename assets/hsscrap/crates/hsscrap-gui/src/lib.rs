//! Optional egui presentation; authorization, storage and owner execution stay in the host.
use std::collections::BTreeMap;

use egui::{Key, Modifiers, Rect, Response, Ui};
use hsscrap::interaction::{Inspection, Phase, Session};
use hsscrap::*;

mod metadata;
mod tags;

#[derive(Debug, Clone, PartialEq)]
pub enum ViewAction {
    ChooseTag { tag: TagRef, target: TagTarget },
    Save,
    Reload,
    Recover(OperationId),
    Close,
}

#[derive(Debug, Default)]
pub struct ViewResponse {
    pub actions: Vec<ViewAction>,
    pub controls: BTreeMap<String, Rect>,
    pub focused_control: Option<String>,
    pub error: Option<Error>,
}

pub struct Inspector {
    pub session: Session,
    namespace: String,
    focus_cancel: bool,
    restore_edit: bool,
    selected_tag: Option<TagRef>,
    selected_target: Option<TagTarget>,
    local_error: Option<Error>,
}

impl Inspector {
    pub fn new(namespace: impl Into<String>, inspection: Inspection) -> Result<Self> {
        Ok(Self {
            session: Session::new(inspection)?,
            namespace: namespace.into(),
            focus_cancel: false,
            restore_edit: false,
            selected_tag: None,
            selected_target: None,
            local_error: None,
        })
    }

    pub fn show(&mut self, ui: &mut Ui) -> ViewResponse {
        let mut output = ViewResponse::default();
        ui.push_id(self.namespace.clone(), |ui| {
            ui.scope(|ui| {
                ui.spacing_mut().interact_size.y = ui.spacing().interact_size.y.max(40.0);
                ui.spacing_mut().item_spacing.y = 9.0;
                ui.set_max_width(ui.available_width().min(720.0));
                if !egui::Popup::is_any_open(ui.ctx())
                    && ui.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape))
                {
                    if self.session.cancel() {
                        self.restore_edit = true;
                        self.local_error = None;
                    } else if self.session.phase() == &Phase::Inspect {
                        output.actions.push(ViewAction::Close);
                    }
                }
                self.body(ui, &mut output);
            });
        });
        output.error = self.local_error.clone();
        output
    }

    fn control(
        &self,
        ui: &Ui,
        response: &Response,
        name: &str,
        label: &str,
        out: &mut ViewResponse,
    ) {
        out.controls.insert(name.into(), response.rect);
        if response.has_focus() {
            out.focused_control = Some(name.into());
        }
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_author_id(format!("hsscrap.{}.{}", self.namespace, name));
            node.set_label(label);
        });
    }

    fn result(&mut self, result: Result<()>) {
        self.local_error = result.err();
    }

    fn body(&mut self, ui: &mut Ui, out: &mut ViewResponse) {
        ui.heading("Item details");
        if self.session.phase() == &Phase::Revoked {
            ui.label("Access is no longer available.");
            ui.label("Private details and unsaved changes have been cleared.");
            let close = ui.button("Close details");
            self.control(ui, &close, "close", "Close details", out);
            if close.clicked() {
                out.actions.push(ViewAction::Close);
            }
            return;
        }
        let Some(inspection) = self.session.inspection().cloned() else {
            return;
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(if self.session.can_edit() {
                "Metadata and tags"
            } else {
                "Read only · metadata and tags"
            });
            let edit = ui.add_enabled(
                self.session.phase() == &Phase::Inspect && self.session.can_edit(),
                egui::Button::new("Edit details"),
            );
            self.control(ui, &edit, "edit", "Edit details", out);
            if self.restore_edit && edit.enabled() {
                edit.request_focus();
                self.restore_edit = false;
            }
            if edit.clicked() {
                let result = self.session.begin_edit();
                self.focus_cancel = result.is_ok();
                self.result(result);
            }
        });
        ui.separator();
        self.status(ui, out);
        if let Some(error) = &self.local_error {
            ui.label(egui::RichText::new(error_text(error)).strong());
        }
        let footer = if self.session.draft().is_some() {
            64.0
        } else {
            0.0
        };
        egui::ScrollArea::vertical()
            .id_salt("metadata-and-tags")
            .max_height((ui.available_height() - footer).max(80.0))
            .show(ui, |ui| {
                self.metadata(ui, &inspection, out);
                ui.add_space(8.0);
                ui.separator();
                ui.heading("Tags");
                ui.label("Each assignment keeps its item or context origin.");
                self.tags(ui, &inspection, out);
            });
        if self.session.draft().is_some() {
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                let save = ui.add_enabled(
                    self.session.editable() && self.session.dirty(),
                    egui::Button::new("Save changes"),
                );
                self.control(ui, &save, "save", "Save changes", out);
                if save.clicked() {
                    out.actions.push(ViewAction::Save);
                }
                let can_cancel = matches!(
                    self.session.phase(),
                    Phase::Editing | Phase::Conflict(_) | Phase::Error(_)
                );
                let cancel = ui.add_enabled(can_cancel, egui::Button::new("Cancel edits"));
                self.control(ui, &cancel, "cancel", "Cancel edits", out);
                if self.focus_cancel && cancel.enabled() {
                    cancel.request_focus();
                    self.focus_cancel = false;
                }
                if cancel.clicked() && self.session.cancel() {
                    self.restore_edit = true;
                    self.local_error = None;
                    ui.ctx().request_repaint();
                }
            });
        }
    }

    fn status(&mut self, ui: &mut Ui, out: &mut ViewResponse) {
        match self.session.phase() {
            Phase::Inspect => {}
            Phase::Editing => {
                ui.label("Editing a local draft · nothing is saved yet");
            }
            Phase::Saving(_) => {
                ui.label("Saving through the owning services…");
            }
            Phase::Conflict(field) => {
                ui.label(
                    egui::RichText::new(format!(
                        "The saved data changed ({field}). Your draft is still here."
                    ))
                    .strong(),
                );
                ui.label("Reload current data before editing again. Reload discards this draft.");
                let reload = ui.button("Reload current data");
                self.control(ui, &reload, "reload", "Reload current data", out);
                if reload.clicked() {
                    out.actions.push(ViewAction::Reload);
                }
            }
            Phase::Error(error) => {
                ui.label(egui::RichText::new(error_text(error)).strong());
            }
            Phase::Unknown(operation) => {
                ui.label(
                    "Save outcome is not confirmed. Check the original operation before retrying.",
                );
                let recover = ui.button("Check save status");
                self.control(ui, &recover, "recover", "Check save status", out);
                if recover.clicked() {
                    out.actions.push(ViewAction::Recover(*operation));
                }
            }
            Phase::ReloadRequired => {
                ui.label("Changes were saved. Reload to display the current owner data.");
                let reload = ui.button("Reload saved details");
                self.control(ui, &reload, "reload", "Reload saved details", out);
                if reload.clicked() {
                    out.actions.push(ViewAction::Reload);
                }
            }
            Phase::Revoked => {}
        }
    }
}

fn error_text(error: &Error) -> String {
    match error {
        Error::Denied => "Access denied. Refresh authorization before retrying.".into(),
        Error::NotFound => "The owner could not find this record. Reload current data.".into(),
        Error::Conflict { field } => format!("Data changed ({field}). Reload current data."),
        Error::LimitExceeded { field, limit } => {
            format!("{field} exceeds the supported limit ({limit}).")
        }
        Error::Invalid { field, reason } => format!("{field}: {reason}"),
        Error::UnsupportedRequiredFeature { feature } => {
            format!("Unsupported required feature: {feature}")
        }
        Error::ProjectContextUnresolved => {
            "The owning project context could not be resolved.".into()
        }
        Error::UnknownOutcome { .. } => {
            "The operation outcome is not confirmed. Check save status.".into()
        }
    }
}
