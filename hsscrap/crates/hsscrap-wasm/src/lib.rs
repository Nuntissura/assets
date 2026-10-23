//! Versioned browser boundary. Canonical owners authorize and execute every effect.
use std::collections::BTreeSet;

use hsscrap::interaction::{metadata_origin, MetadataOrigin, Phase, SaveTicket, Session};
use hsscrap::*;
use hsscrap_gui::{Inspector, ViewAction};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

pub mod wire;

pub const VERSION: u32 = 1;
pub const MAX_INPUT_BYTES: usize = 1024 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 4 * MAX_INPUT_BYTES;
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
pub const MAX_COLLECTION_ITEMS: usize = 1024;

fn decode<T: DeserializeOwned>(input: &str) -> std::result::Result<T, String> {
    handshake_canonical_json::jcs_from_slice(input.as_bytes(), MAX_INPUT_BYTES)
        .map_err(|_| "invalid_json".to_owned())?;
    let value: Value = serde_json::from_str(input).map_err(|_| "invalid_input".to_owned())?;
    // The raw owner established the depth bound before this traversal.
    fn safe_numbers(value: &Value) -> bool {
        match value {
            Value::Number(number) => {
                number
                    .as_i64()
                    .is_none_or(|value| value.unsigned_abs() <= 9_007_199_254_740_991)
                    && number
                        .as_u64()
                        .is_none_or(|value| value <= 9_007_199_254_740_991)
                    && number.as_f64().is_none_or(|value| {
                        value.fract() != 0.0 || value.abs() <= 9_007_199_254_740_991_f64
                    })
            }
            Value::Array(values) => values.iter().all(safe_numbers),
            Value::Object(values) => values.values().all(safe_numbers),
            _ => true,
        }
    }
    if !safe_numbers(&value) {
        return Err("numeric_range".into());
    }
    serde_json::from_value(value).map_err(|_| "invalid_input".into())
}

fn encode(value: &impl Serialize) -> std::result::Result<String, String> {
    let output = serde_json::to_string(value).map_err(|_| "invalid_output".to_owned())?;
    if output.len() > MAX_OUTPUT_BYTES {
        return Err("output_bounds".into());
    }
    Ok(output)
}

fn version(value: u32) -> std::result::Result<(), String> {
    if value == VERSION {
        Ok(())
    } else {
        Err("unsupported_version".into())
    }
}

fn core_error(error: Error) -> String {
    match error {
        Error::Invalid { .. } => "invalid",
        Error::LimitExceeded { .. } => "bounds",
        Error::UnsupportedRequiredFeature { .. } => "unsupported_feature",
        Error::Denied => "denied",
        Error::NotFound => "not_found",
        Error::Conflict { .. } => "conflict",
        Error::ProjectContextUnresolved => "project_context_unresolved",
        Error::UnknownOutcome { .. } => "unknown_outcome",
    }
    .into()
}

fn state(session: &Session) -> &'static str {
    match session.phase() {
        Phase::Inspect => "inspect",
        Phase::Editing => "editing",
        Phase::Saving(_) => "saving",
        Phase::Conflict(_) => "conflict",
        Phase::Error(_) => "error",
        Phase::Unknown(_) => "unknown",
        Phase::ReloadRequired => "reload_required",
        Phase::Revoked => "revoked",
    }
}

fn origin(value: MetadataOrigin) -> &'static str {
    match value {
        MetadataOrigin::Source => "source",
        MetadataOrigin::UserOverride => "user_override",
        MetadataOrigin::Cleared => "cleared",
        MetadataOrigin::Unknown => "unknown",
    }
}

fn view(
    session: &Session,
    ticket: &Option<(String, SaveTicket)>,
) -> std::result::Result<Value, String> {
    let inspection = session.inspection();
    let metadata = inspection.map(|inspection| {
        let effective = inspection.metadata().map_err(core_error)?;
        let observed = inspection.selected_observation.as_ref();
        Ok::<_, String>(json!({
            "title": effective.title, "description": effective.description,
            "author": effective.author, "published_at": effective.published_at,
            "title_origin": origin(metadata_origin(&inspection.item.title_override, observed.and_then(|value| value.title.as_ref()))),
            "description_origin": origin(metadata_origin(&inspection.item.description_override, observed.and_then(|value| value.description.as_ref())))
        }))
    }).transpose()?;
    let draft = session.draft().map(|draft| {
        json!({
            "title": draft.title, "description": draft.description,
            "additions": draft.additions, "removals": draft.removals,
        })
    });
    let pending = match (session.pending(), ticket) {
        (Some(pending), Some((token, retained))) if pending.ticket == *retained => Some(json!({
            "token": token, "plan": pending.plan, "removal_targets": pending.removal_targets,
        })),
        (None, None) => None,
        _ => return Err("ticket_state".into()),
    };
    let error = match session.phase() {
        Phase::Error(error) => Some(core_error(error.clone())),
        Phase::Conflict(_) => Some("conflict".into()),
        _ => None,
    };
    Ok(json!({
        "phase": state(session), "can_edit": session.can_edit(), "dirty": session.dirty(),
        "inspection": inspection.cloned().map(wire::InspectionWire::from),
        "metadata": metadata, "draft": draft, "pending": pending, "error": error,
    }))
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct HsscrapBridge {
    instance_id: String,
    inspector: Inspector,
    context: egui::Context,
    ticket: Option<(String, SaveTicket)>,
    next_token: u64,
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl HsscrapBridge {
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new(input: &str) -> std::result::Result<Self, String> {
        let initial: wire::Initial = decode(input)?;
        version(initial.version)?;
        let instance =
            uuid::Uuid::parse_str(&initial.instance_id).map_err(|_| "invalid_instance")?;
        if instance.is_nil() || instance.to_string() != initial.instance_id {
            return Err("invalid_instance".into());
        }
        let bridge = Self {
            instance_id: initial.instance_id,
            inspector: Inspector::new("wasm", initial.inspection.into_native()?)
                .map_err(core_error)?,
            context: egui::Context::default(),
            ticket: None,
            next_token: 0,
        };
        bridge.snapshot()?;
        Ok(bridge)
    }

    pub fn snapshot(&self) -> std::result::Result<String, String> {
        encode(&json!({"version": VERSION, "view": view(&self.inspector.session, &self.ticket)?}))
    }

    pub fn apply(&mut self, input: &str) -> std::result::Result<String, String> {
        let command: wire::Command = decode(input)?;
        version(command.version)?;
        // Admission is transactional within this in-memory adapter: bounds/errors do not lose a draft.
        let mut session = self.inspector.session.clone();
        let mut ticket = self.ticket.clone();
        let mut next_token = self.next_token;
        use wire::Action;
        match command.action {
            Action::BeginEdit => session.begin_edit().map_err(core_error)?,
            Action::SetTitle { value } => session.set_title(value).map_err(core_error)?,
            Action::SetDescription { value } => {
                session.set_description(value).map_err(core_error)?
            }
            Action::AddTag { assignment } => session.stage_tag(assignment).map_err(core_error)?,
            Action::RemoveTag { assignment_id } => {
                session.remove_tag(assignment_id).map_err(core_error)?
            }
            Action::UndoRemoval { assignment_id } => {
                session.undo_removal(assignment_id).map_err(core_error)?
            }
            Action::Cancel => {
                session.cancel();
            }
            Action::BeginSave { context } => {
                next_token = next_token.checked_add(1).ok_or("ticket_exhausted")?;
                let request = session
                    .begin_save(context, &BTreeSet::new())
                    .map_err(core_error)?;
                ticket = Some((format!("{}:{next_token}", self.instance_id), request.ticket));
            }
            Action::Complete { token, result } => {
                let retained = ticket
                    .as_ref()
                    .filter(|(current, _)| current == &token)
                    .map(|(_, retained)| *retained)
                    .ok_or("stale_ticket")?;
                let result = match result {
                    wire::Completion::Outcome { outcome } => Ok(outcome),
                    wire::Completion::Unknown => Err(Error::UnknownOutcome {
                        operation_id: retained.operation_id,
                    }),
                    wire::Completion::Denied => Err(Error::Denied),
                    wire::Completion::Conflict => Err(Error::Conflict {
                        field: "owner_revision".into(),
                    }),
                    wire::Completion::NotFound => Err(Error::NotFound),
                };
                if !session.complete(retained, result) {
                    return Err("stale_ticket".into());
                }
            }
            Action::Reload { inspection } => session
                .reload(inspection.into_native()?)
                .map_err(core_error)?,
            Action::Revoke => session.revoke(),
        }
        if session.pending().is_none() {
            ticket = None;
        }
        let output = encode(&json!({"version": VERSION, "view": view(&session, &ticket)?}))?;
        self.inspector.session = session;
        self.ticket = ticket;
        self.next_token = next_token;
        Ok(output)
    }

    pub fn panel(&mut self, input: &str) -> std::result::Result<String, String> {
        let panel: wire::Panel = decode(input)?;
        version(panel.version)?;
        if !panel.width.is_finite()
            || !panel.height.is_finite()
            || !(64.0..=4096.0).contains(&panel.width)
            || !(64.0..=4096.0).contains(&panel.height)
        {
            return Err("viewport_bounds".into());
        }
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(panel.width, panel.height),
            )),
            ..Default::default()
        };
        let mut response = None;
        let _frame = self.context.run(raw, |context| {
            egui::CentralPanel::default().show(context, |ui| {
                response = Some(self.inspector.show(ui));
            });
        });
        let response = response.ok_or("panel_unavailable")?;
        let controls: Vec<_> = response
            .controls
            .into_iter()
            .map(|(id, rect)| {
                json!({
                    "id": id, "rect": [rect.min.x, rect.min.y, rect.max.x, rect.max.y],
                })
            })
            .collect();
        let actions: Vec<_> = response
            .actions
            .into_iter()
            .map(|action| match action {
                ViewAction::ChooseTag { tag, target } => {
                    json!({"type":"choose_tag","tag":tag,"target":target})
                }
                ViewAction::Save => json!({"type":"save"}),
                ViewAction::Reload => json!({"type":"reload"}),
                ViewAction::Recover(operation) => {
                    json!({"type":"recover","operation_id":operation})
                }
                ViewAction::Close => json!({"type":"close"}),
            })
            .collect();
        encode(
            &json!({"version": VERSION, "controls":controls, "actions":actions,
            "focused_control": response.focused_control,
            "error": response.error.map(core_error), "view":view(&self.inspector.session, &self.ticket)?}),
        )
    }
}
