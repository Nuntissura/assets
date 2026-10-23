use hsscrap::interaction::{Inspection, TagChoice, TargetAccess};
use hsscrap::ports::CommandContext;
use hsscrap::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectionWire {
    pub scope: ScopeRef,
    pub item: Item,
    pub selected_observation: Option<MetadataObservation>,
    pub captures: Vec<URLCapture>,
    pub memberships: Vec<ProjectMembership>,
    pub assignments: Vec<TagAssignment>,
    pub targets: Vec<TargetWire>,
    pub tag_choices: Vec<TagChoiceWire>,
    pub can_edit_metadata: bool,
    pub item_owner: HostRef,
    pub owner_fences: Vec<ExpectedRevision>,
    pub bounds: Bounds,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetWire {
    pub target: TagTarget,
    pub scope: ScopeRef,
    pub label: String,
    pub writable: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagChoiceWire {
    pub tag: TagRef,
    pub label: String,
}

impl From<Inspection> for InspectionWire {
    fn from(value: Inspection) -> Self {
        Self {
            scope: value.scope,
            item: value.item,
            selected_observation: value.selected_observation,
            captures: value.captures,
            memberships: value.memberships,
            assignments: value.assignments,
            targets: value
                .targets
                .into_iter()
                .map(|target| TargetWire {
                    target: target.target,
                    scope: target.scope,
                    label: target.label,
                    writable: target.writable,
                })
                .collect(),
            tag_choices: value
                .tag_choices
                .into_iter()
                .map(|choice| TagChoiceWire {
                    tag: choice.tag,
                    label: choice.label,
                })
                .collect(),
            can_edit_metadata: value.can_edit_metadata,
            item_owner: value.item_owner,
            owner_fences: value.owner_fences,
            bounds: value.bounds,
        }
    }
}

impl InspectionWire {
    pub fn into_native(self) -> std::result::Result<Inspection, String> {
        self.bounds.validate().map_err(super::core_error)?;
        if self.bounds.max_text_bytes > super::MAX_TEXT_BYTES
            || self.bounds.max_url_bytes > super::MAX_TEXT_BYTES
            || self.bounds.max_operation_bytes > super::MAX_INPUT_BYTES
            || self.bounds.max_collection_items > super::MAX_COLLECTION_ITEMS
            || self.bounds.max_page_items > super::MAX_COLLECTION_ITEMS
        {
            return Err("bounds".into());
        }
        Ok(Inspection {
            scope: self.scope,
            item: self.item,
            selected_observation: self.selected_observation,
            captures: self.captures,
            memberships: self.memberships,
            assignments: self.assignments,
            targets: self
                .targets
                .into_iter()
                .map(|target| TargetAccess {
                    target: target.target,
                    scope: target.scope,
                    label: target.label,
                    writable: target.writable,
                })
                .collect(),
            tag_choices: self
                .tag_choices
                .into_iter()
                .map(|choice| TagChoice {
                    tag: choice.tag,
                    label: choice.label,
                })
                .collect(),
            can_edit_metadata: self.can_edit_metadata,
            item_owner: self.item_owner,
            owner_fences: self.owner_fences,
            bounds: self.bounds,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Initial {
    pub version: u32,
    pub instance_id: String,
    pub inspection: InspectionWire,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub version: u32,
    pub action: Action,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    BeginEdit,
    SetTitle { value: Override<String> },
    SetDescription { value: Override<String> },
    AddTag { assignment: TagAssignment },
    RemoveTag { assignment_id: AssignmentId },
    UndoRemoval { assignment_id: AssignmentId },
    Cancel,
    BeginSave { context: CommandContext },
    Complete { token: String, result: Completion },
    Reload { inspection: InspectionWire },
    Revoke,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Completion {
    Outcome { outcome: OperationOutcome },
    Unknown,
    Denied,
    Conflict,
    NotFound,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Panel {
    pub version: u32,
    pub width: f32,
    pub height: f32,
}
