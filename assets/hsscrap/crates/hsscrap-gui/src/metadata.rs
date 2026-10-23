use crate::{Inspector, ViewResponse};
use egui::{Id, Ui};
use hsscrap::interaction::{metadata_origin, Inspection, MetadataOrigin};
use hsscrap::*;

impl Inspector {
    pub(super) fn metadata(
        &mut self,
        ui: &mut Ui,
        inspection: &Inspection,
        out: &mut ViewResponse,
    ) {
        let Ok(metadata) = inspection.metadata() else {
            return;
        };
        let observed = inspection.selected_observation.as_ref();
        let editing = self.session.draft().is_some();
        if let Some(draft) = self.session.draft().cloned() {
            let enabled = self.session.editable() && inspection.can_edit_metadata;
            if let Some(value) = self.field_editor(
                ui,
                "Title",
                "title",
                &draft.title,
                observed.and_then(|value| value.title.as_ref()),
                false,
                enabled,
                out,
            ) {
                let result = self.session.set_title(value);
                self.result(result);
            }
            if let Some(value) = self.field_editor(
                ui,
                "Description",
                "description",
                &draft.description,
                observed.and_then(|value| value.description.as_ref()),
                true,
                enabled,
                out,
            ) {
                let result = self.session.set_description(value);
                self.result(result);
            }
        } else {
            field(
                ui,
                "Title",
                metadata.title.as_deref(),
                metadata_origin(
                    &inspection.item.title_override,
                    observed.and_then(|value| value.title.as_ref()),
                ),
            );
            field(
                ui,
                "Description",
                metadata.description.as_deref(),
                metadata_origin(
                    &inspection.item.description_override,
                    observed.and_then(|value| value.description.as_ref()),
                ),
            );
        }
        if editing && !inspection.can_edit_metadata {
            ui.label("Metadata is read only in this context.");
        }
        field(
            ui,
            "Author",
            metadata.author.as_deref(),
            if metadata.author.is_some() {
                MetadataOrigin::Source
            } else {
                MetadataOrigin::Unknown
            },
        );
        let published = metadata
            .published_at
            .as_ref()
            .map(|value| format!("{} · {:?} precision", value.raw_value, value.precision));
        field(
            ui,
            "Published",
            published.as_deref(),
            if published.is_some() {
                MetadataOrigin::Source
            } else {
                MetadataOrigin::Unknown
            },
        );
        if let Some(observation) = observed {
            ui.label(format!(
                "Observed {} · {} · {:?}",
                observation.observed_at.as_str(),
                observation.producer,
                observation.status.state,
            ));
        }
        egui::CollapsingHeader::new("Link and capture provenance")
            .id_salt("provenance")
            .show(ui, |ui| {
                ui.label(format!("Item ID: {}", inspection.item.item_id));
                field(
                    ui,
                    "Canonical source link",
                    observed.and_then(|value| value.canonical_url_assertion.as_deref()),
                    if observed.is_some_and(|value| value.canonical_url_assertion.is_some()) {
                        MetadataOrigin::Source
                    } else {
                        MetadataOrigin::Unknown
                    },
                );
                field(
                    ui,
                    "Host site",
                    observed.map(|value| value.host_site.as_str()),
                    if observed.is_some() {
                        MetadataOrigin::Source
                    } else {
                        MetadataOrigin::Unknown
                    },
                );
                field(
                    ui,
                    "Provider",
                    observed.and_then(|value| value.provider.as_deref()),
                    if observed.is_some_and(|value| value.provider.is_some()) {
                        MetadataOrigin::Source
                    } else {
                        MetadataOrigin::Unknown
                    },
                );
                if inspection.captures.is_empty() {
                    ui.label("No authorized capture record supplied.");
                }
                for capture in &inspection.captures {
                    ui.separator();
                    ui.label("Captured link");
                    ui.label(&capture.captured_url);
                    field(
                        ui,
                        "Original link",
                        capture.original_url.as_deref(),
                        if capture.original_url.is_some() {
                            MetadataOrigin::Source
                        } else {
                            MetadataOrigin::Unknown
                        },
                    );
                    ui.label("Versioned identity link");
                    ui.label(&capture.identity_url);
                    field(
                        ui,
                        "Captured at",
                        capture.captured_at.as_ref().map(Instant::as_str),
                        if capture.captured_at.is_some() {
                            MetadataOrigin::Source
                        } else {
                            MetadataOrigin::Unknown
                        },
                    );
                    ui.label(format!(
                        "Created here {} · {:?}",
                        capture.created_here_at.as_str(),
                        capture.capture_kind
                    ));
                }
                if inspection.memberships.is_empty() {
                    ui.label("No authorized project membership supplied.");
                }
                for membership in &inspection.memberships {
                    let label = inspection
                        .target(&TagTarget::Project(membership.project_ref.clone()))
                        .or_else(|| {
                            inspection.target(&TagTarget::Membership(membership.membership_id))
                        })
                        .map(|value| value.label.as_str())
                        .unwrap_or("Project");
                    ui.separator();
                    ui.label(format!("{label} · saved {}", membership.saved_at.as_str()));
                    ui.label(format!(
                        "Membership {:?} · created here {}",
                        membership.lifecycle,
                        membership.created_here_at.as_str()
                    ));
                }
            });
    }

    fn field_editor(
        &self,
        ui: &mut Ui,
        label: &str,
        key: &str,
        current: &Override<String>,
        source: Option<&String>,
        multiline: bool,
        enabled: bool,
        out: &mut ViewResponse,
    ) -> Option<Override<String>> {
        let mut value = current.clone();
        ui.label(egui::RichText::new(label).strong());
        let mut mode = match &value {
            Override::Inherit => 0,
            Override::Set(_) => 1,
            Override::Clear => 2,
        };
        ui.add_enabled_ui(enabled, |ui| {
            let origin = ui.label(format!("{label} origin"));
            let choice = egui::ComboBox::from_id_salt((key, "origin"))
                .selected_text(["Use source", "User override", "Explicitly cleared"][mode])
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut mode, 0, "Use source");
                    ui.selectable_value(&mut mode, 1, "User override");
                    ui.selectable_value(&mut mode, 2, "Explicitly cleared");
                })
                .response
                .labelled_by(origin.id);
            self.control(
                ui,
                &choice,
                &format!("{key}-origin"),
                &format!("{label} origin"),
                out,
            );
            value = match mode {
                0 => Override::Inherit,
                2 => Override::Clear,
                _ => match &value {
                    Override::Set(text) => Override::Set(text.clone()),
                    _ => Override::Set(source.cloned().unwrap_or_default()),
                },
            };
            if let Override::Set(text) = &mut value {
                let response = if multiline {
                    ui.add(
                        egui::TextEdit::multiline(text)
                            .id(Id::new((&self.namespace, key)))
                            .desired_width(f32::INFINITY)
                            .desired_rows(3),
                    )
                } else {
                    ui.add(
                        egui::TextEdit::singleline(text)
                            .id(Id::new((&self.namespace, key)))
                            .desired_width(f32::INFINITY),
                    )
                };
                self.control(ui, &response, key, label, out);
            }
        });
        ui.label(format!(
            "Source: {}",
            source.map(String::as_str).unwrap_or("Unknown")
        ));
        (value != *current).then_some(value)
    }
}

fn origin_label(origin: MetadataOrigin) -> &'static str {
    match origin {
        MetadataOrigin::Source => "Source",
        MetadataOrigin::UserOverride => "User override",
        MetadataOrigin::Cleared => "Explicitly cleared",
        MetadataOrigin::Unknown => "Unknown",
    }
}

fn field(ui: &mut Ui, label: &str, value: Option<&str>, origin: MetadataOrigin) {
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(label).strong());
        ui.label(origin_label(origin));
    });
    ui.label(value.unwrap_or(if origin == MetadataOrigin::Cleared {
        "Cleared"
    } else {
        "Unknown"
    }));
}
