//! Preflight panel (lane SMB-LAYOUT-UI-2 slice 5; STU-LAY-055/057/154): a status indicator (green
//! when no error-severity finding, else red with the error count), the findings of the live run
//! (rule id, severity, object, page, detail) with a jump to the offending frame or page, a page
//! scope (all pages or the selected page) and per-rule severity editing of the working profile.
//! The rules run in layout's preflight engine (`hsk_studio_layout::run_preflight`) over this
//! editor's compositions and records; the panel re-runs it on every draw (live). Declared gaps:
//! profiles are not stored in the document (no Folio record), not exported or embedded; rule
//! parameters (rule_data types) are not edited. InDesign preflight equality UNVERIFIED (D5).
#![deny(clippy::arithmetic_side_effects)]
use super::{LayoutEditor, LayoutUiError, widget};
use hsk_studio_layout::{
    LayerRects, PREFLIGHT_RULES, PreflightInput, PreflightProfile, PreflightReport, Severity, parent_set_from_records, run_preflight,
};
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub(crate) struct PreflightUi {
    pub profile: PreflightProfile,
    pub selected_page_only: bool,
}
impl Default for PreflightUi {
    fn default() -> Self {
        Self { profile: PreflightProfile::basic(), selected_page_only: false }
    }
}

/// Status colours (also the headless visual assertions).
pub mod colour {
    pub const OK: egui::Color32 = egui::Color32::from_rgb(0, 160, 60);
    pub const ERRORS: egui::Color32 = egui::Color32::from_rgb(200, 30, 30);
}

impl LayoutEditor {
    /// The working preflight profile (severities are editable through [`Self::set_preflight_severity`]).
    pub fn preflight_profile(&self) -> &PreflightProfile {
        &self.preflight_ui.profile
    }

    /// Sets one rule's severity in the working profile (not an undo step: the profile is not a
    /// document record).
    pub fn set_preflight_severity(&mut self, rule: &str, severity: Severity) -> Result<(), LayoutUiError> {
        let r = self.preflight_ui.profile.rules.iter_mut().find(|r| r.rule_id == rule).ok_or(LayoutUiError::Preflight("preflight_unknown_rule"))?;
        r.severity = severity;
        Ok(())
    }

    /// One live preflight run over the current state; `scope` = page keys (`None` = all pages).
    pub fn preflight(&self, scope: Option<&[String]>) -> Result<PreflightReport, LayoutUiError> {
        let compositions: Vec<(String, Result<hsk_studio_layout::LayoutProposal, hsk_studio_layout::LayoutError>)> = self
            .doc
            .stories
            .iter()
            .filter(|s| self.doc.document.layers.iter().any(|l| matches!(&l.payload, hsk_studio_folio::Payload::Text { story_id } if *story_id == s.story_id)))
            .map(|s| {
                let r = self.compose(&s.story_id).map_err(|e| match e {
                    LayoutUiError::Layout(l) => l,
                    _ => hsk_studio_layout::LayoutError::InvalidInput,
                });
                (s.story_id.clone(), r)
            })
            .collect();
        let verdict = parent_set_from_records(&self.rec.parents, &self.doc.document, &LayerRects::new()).map(|_| ()).map_err(|e| e.code());
        let input = PreflightInput {
            document: &self.doc.document,
            stories: &self.doc.stories,
            compositions: &compositions,
            styles: &self.doc.styles,
            flow: &self.doc.flow,
            parents: Some((&self.rec.parents, verdict)),
            pages: &[],
        };
        run_preflight(&input, &self.preflight_ui.profile, scope).map_err(|e| LayoutUiError::Preflight(e.code()))
    }

    pub(crate) fn preflight_json(&self) -> Value {
        match self.preflight(None) {
            Ok(r) => json!({
                "profile": r.profile, "ok": r.ok(), "errors": r.errors, "warnings": r.warnings, "informational": r.informational,
                "findings": r.findings.iter().map(|f| json!({"rule_id": f.rule_id, "severity": f.severity.code(), "object": f.object, "page": f.page, "detail": f.detail})).collect::<Vec<_>>(),
            }),
            Err(e) => json!({"refusal": e.code()}),
        }
    }

    /// The preflight panel.
    pub(crate) fn preflight_panel(&mut self, ui: &mut egui::Ui, drawn: &mut Vec<&'static str>) {
        let scope: Option<Vec<String>> = match (self.preflight_ui.selected_page_only, &self.selected_page) {
            (true, Some(p)) => Some(vec![p.clone()]),
            _ => None,
        };
        let report = self.preflight(scope.as_deref());
        ui.push_id(widget::PREFLIGHT_PANEL, |ui| {
            ui.horizontal(|ui| {
                let (c, text) = match &report {
                    Ok(r) if r.ok() => (colour::OK, format!("Preflight: no errors ({} warnings)", r.warnings)),
                    Ok(r) => (colour::ERRORS, format!("Preflight: {} errors", r.errors)),
                    Err(e) => (colour::ERRORS, format!("Preflight: {}", e.code())),
                };
                let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 0.0, c);
                ui.push_id(widget::PREFLIGHT_STATUS, |ui| ui.label(text));
                ui.checkbox(&mut self.preflight_ui.selected_page_only, "Selected page only");
            });
            if let Ok(r) = &report {
                ui.push_id(widget::PREFLIGHT_FINDINGS, |ui| {
                    for f in r.findings.iter().take(500) {
                        let line = format!("{} {} {} {}", f.severity.code(), f.rule_id, f.page.as_deref().unwrap_or("-"), f.detail);
                        if ui.link(line).clicked() {
                            if self.doc.document.layers.iter().any(|l| l.layer_id == f.object) {
                                self.selected = Some(f.object.clone());
                            }
                            if let Some(p) = &f.page {
                                self.selected_page = Some(p.clone());
                            }
                        }
                    }
                });
            }
            ui.push_id(widget::PREFLIGHT_PROFILE, |ui| {
                egui::CollapsingHeader::new(format!("Profile {}", self.preflight_ui.profile.name)).show(ui, |ui| {
                    for (rule, _) in PREFLIGHT_RULES {
                        let mut sev = self.preflight_ui.profile.rules.iter().find(|r| r.rule_id == rule).map_or(Severity::Disabled, |r| r.severity);
                        let before = sev;
                        egui::ComboBox::from_id_salt(rule).selected_text(format!("{rule}: {}", sev.code())).show_ui(ui, |ui| {
                            for s in [Severity::Disabled, Severity::Error, Severity::Warning, Severity::Informational] {
                                ui.selectable_value(&mut sev, s, s.code());
                            }
                        });
                        if sev != before {
                            let _ = self.set_preflight_severity(rule, sev);
                        }
                    }
                });
            });
        });
        drawn.extend([widget::PREFLIGHT_PANEL, widget::PREFLIGHT_STATUS, widget::PREFLIGHT_FINDINGS, widget::PREFLIGHT_PROFILE]);
    }
}
