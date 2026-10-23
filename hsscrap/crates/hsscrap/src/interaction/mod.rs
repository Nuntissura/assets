//! Bounded metadata/tag drafts. Hosts authorize reads and execute owner plans.
use std::collections::{BTreeMap, BTreeSet};

use crate::metadata::{project_metadata, EffectiveMetadata};
use crate::operations::{plan_operation, OperationPlan};
use crate::ports::{CommandContext, ItemPatch, OwnerCommand};
use crate::*;

#[derive(Debug, Clone, PartialEq)]
pub struct TargetAccess {
    pub target: TagTarget,
    pub scope: ScopeRef,
    pub label: String,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TagChoice {
    pub tag: TagRef,
    pub label: String,
}

/// All records and fences come from current authorized owner reads, not GUI state.
#[derive(Debug, Clone, PartialEq)]
pub struct Inspection {
    pub scope: ScopeRef,
    pub item: Item,
    pub selected_observation: Option<MetadataObservation>,
    pub captures: Vec<URLCapture>,
    pub memberships: Vec<ProjectMembership>,
    pub assignments: Vec<TagAssignment>,
    pub targets: Vec<TargetAccess>,
    pub tag_choices: Vec<TagChoice>,
    pub can_edit_metadata: bool,
    pub item_owner: HostRef,
    pub owner_fences: Vec<ExpectedRevision>,
    pub bounds: Bounds,
}

impl Inspection {
    fn validate(&self) -> Result<()> {
        self.bounds.validate()?;
        if self.scope.0.is_empty() {
            return Err(invalid("scope", "current authorized scope required"));
        }
        self.bounds
            .text("scope", &self.scope.0, self.bounds.max_text_bytes)?;
        let supported = BTreeSet::new();
        self.item.validate(&self.bounds, &supported)?;
        if self.item.lifecycle != Lifecycle::Active {
            return Err(Error::Denied);
        }
        for (field, count) in [
            ("assignments", self.assignments.len()),
            ("targets", self.targets.len()),
            ("tag_choices", self.tag_choices.len()),
            ("owner_fences", self.owner_fences.len()),
            ("captures", self.captures.len()),
            ("memberships", self.memberships.len()),
        ] {
            if count > self.bounds.max_collection_items {
                return Err(Error::LimitExceeded {
                    field: field.into(),
                    limit: self.bounds.max_collection_items,
                });
            }
        }
        if let Some(observation) = &self.selected_observation {
            observation.validate(&self.bounds, &supported)?;
            if observation.item_id != self.item.item_id
                || Some(observation.observation_id) != self.item.selected_observation_id
            {
                return Err(Error::Conflict {
                    field: "selected_observation".into(),
                });
            }
        }
        let targets: BTreeSet<_> = self.targets.iter().map(|value| &value.target).collect();
        if targets.len() != self.targets.len() {
            return Err(invalid("targets", "duplicate target"));
        }
        for target in &self.targets {
            if target.scope.0.is_empty() {
                return Err(invalid(
                    "target_scope",
                    "explicit authorized scope required",
                ));
            }
            self.bounds
                .text("target_scope", &target.scope.0, self.bounds.max_text_bytes)?;
            self.bounds
                .text("target_label", &target.label, self.bounds.max_text_bytes)?;
        }
        let mut ids = BTreeSet::new();
        for assignment in &self.assignments {
            assignment.validate(&self.bounds, &supported)?;
            if !self
                .target(&assignment.target)
                .is_some_and(|target| target.scope == assignment.scope)
            {
                return Err(Error::Denied);
            }
            if !ids.insert(assignment.assignment_id) {
                return Err(invalid("assignments", "duplicate assignment"));
            }
        }
        for capture in &self.captures {
            capture.validate(&self.bounds, &supported)?;
            if capture.item_id != self.item.item_id {
                return Err(Error::Denied);
            }
        }
        for membership in &self.memberships {
            membership.validate(&self.bounds, &supported)?;
            if membership.item_id != self.item.item_id
                || !self.targets.iter().any(|target| {
                    target.scope == membership.scope
                        && (target.target == TagTarget::Project(membership.project_ref.clone())
                            || target.target == TagTarget::Membership(membership.membership_id))
                })
            {
                return Err(Error::Denied);
            }
        }
        let mut tags = BTreeSet::new();
        for choice in &self.tag_choices {
            self.bounds
                .text("tag_label", &choice.label, self.bounds.max_text_bytes)?;
            if !tags.insert(&choice.tag) {
                return Err(invalid("tag_choices", "duplicate canonical tag"));
            }
        }
        let fences = fences(&self.owner_fences)?;
        if fences.get(&self.item_owner) != Some(&self.item.revision) {
            return Err(invalid("owner_fences", "inspected item revision required"));
        }
        Ok(())
    }

    pub fn metadata(&self) -> Result<EffectiveMetadata> {
        project_metadata(&self.item, true, self.selected_observation.as_ref())
    }

    pub fn effective_tags(&self) -> BTreeMap<TagRef, Vec<&TagAssignment>> {
        let mut result: BTreeMap<TagRef, Vec<&TagAssignment>> = BTreeMap::new();
        for target in &self.targets {
            for (tag, assignments) in crate::tags::effective_tags(
                &self.assignments,
                &target.scope,
                std::slice::from_ref(&target.target),
            ) {
                result.entry(tag).or_default().extend(assignments);
            }
        }
        result
    }

    pub fn target(&self, target: &TagTarget) -> Option<&TargetAccess> {
        self.targets.iter().find(|value| &value.target == target)
    }
}

fn fences(values: &[ExpectedRevision]) -> Result<BTreeMap<HostRef, Revision>> {
    let mut result = BTreeMap::new();
    for fence in values {
        fence.revision.validate()?;
        if result.insert(fence.owner.clone(), fence.revision).is_some() {
            return Err(invalid("owner_fences", "duplicate owner"));
        }
    }
    Ok(result)
}

fn validate_completion(
    outcome: &OperationOutcome,
    pending: &SaveRequest,
    supported: &BTreeSet<String>,
) -> Result<()> {
    let context = &pending.plan.context;
    outcome.validate(&context.bounds, supported)?;
    if outcome.operation_id != context.operation_id
        || outcome.scope != context.scope
        || outcome.intent_digest != context.intent_digest
        || fences(&outcome.expected_revisions)? != fences(&context.expected_revisions)?
    {
        return Err(Error::Conflict {
            field: "operation_response".into(),
        });
    }
    let bytes = serde_json::to_vec(outcome)
        .map_err(|_| invalid("operation_response", "invalid outcome encoding"))?;
    if bytes.len() > context.bounds.max_operation_bytes {
        return Err(Error::LimitExceeded {
            field: "operation_response".into(),
            limit: context.bounds.max_operation_bytes,
        });
    }
    if outcome.outcome == OutcomeState::Committed {
        let receipts: BTreeSet<_> = outcome.owner_receipt_refs.iter().collect();
        if receipts.len() != pending.plan.commands.len()
            || receipts.len() != outcome.owner_receipt_refs.len()
            || receipts.iter().any(|receipt| {
                receipt.host_instance_id.is_empty()
                    || receipt.owner_kind.is_empty()
                    || receipt.native_id.is_empty()
            })
        {
            return Err(invalid(
                "owner_receipts",
                "one distinct owner receipt required per command",
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataOrigin {
    Source,
    UserOverride,
    Cleared,
    Unknown,
}

pub fn metadata_origin<T>(value: &Override<T>, observed: Option<&T>) -> MetadataOrigin {
    match value {
        Override::Set(_) => MetadataOrigin::UserOverride,
        Override::Clear => MetadataOrigin::Cleared,
        Override::Inherit if observed.is_some() => MetadataOrigin::Source,
        Override::Inherit => MetadataOrigin::Unknown,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub title: Override<String>,
    pub description: Override<String>,
    pub additions: Vec<TagAssignment>,
    pub removals: BTreeSet<AssignmentId>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Inspect,
    Editing,
    Saving(OperationId),
    Conflict(String),
    Error(Error),
    Unknown(OperationId),
    ReloadRequired,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveTicket {
    generation: u64,
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SaveRequest {
    pub ticket: SaveTicket,
    pub plan: OperationPlan,
    /// Resolve each assignment through this owning context, never the inspected item by default.
    pub removal_targets: Vec<(AssignmentId, TagTarget)>,
}

#[derive(Debug, Clone)]
pub struct Session {
    inspection: Option<Inspection>,
    draft: Option<Draft>,
    phase: Phase,
    generation: u64,
    pending: Option<SaveRequest>,
    pending_features: BTreeSet<String>,
}

impl Session {
    pub fn new(inspection: Inspection) -> Result<Self> {
        inspection.validate()?;
        Ok(Self {
            inspection: Some(inspection),
            draft: None,
            phase: Phase::Inspect,
            generation: 0,
            pending: None,
            pending_features: BTreeSet::new(),
        })
    }
    pub fn inspection(&self) -> Option<&Inspection> {
        self.inspection.as_ref()
    }
    pub fn draft(&self) -> Option<&Draft> {
        self.draft.as_ref()
    }
    pub fn phase(&self) -> &Phase {
        &self.phase
    }
    pub fn pending(&self) -> Option<&SaveRequest> {
        self.pending.as_ref()
    }
    pub fn editable(&self) -> bool {
        matches!(self.phase, Phase::Editing | Phase::Error(_))
    }
    pub fn can_edit(&self) -> bool {
        self.inspection.as_ref().is_some_and(|value| {
            value.can_edit_metadata || value.targets.iter().any(|target| target.writable)
        })
    }
    pub fn begin_edit(&mut self) -> Result<()> {
        if self.phase != Phase::Inspect || !self.can_edit() {
            return Err(Error::Denied);
        }
        let item = &self.inspection.as_ref().ok_or(Error::Denied)?.item;
        self.draft = Some(Draft {
            title: item.title_override.clone(),
            description: item.description_override.clone(),
            additions: vec![],
            removals: BTreeSet::new(),
        });
        self.phase = Phase::Editing;
        Ok(())
    }
    pub fn set_title(&mut self, value: Override<String>) -> Result<()> {
        self.set_text(value, true)
    }
    pub fn set_description(&mut self, value: Override<String>) -> Result<()> {
        self.set_text(value, false)
    }
    fn set_text(&mut self, value: Override<String>, title: bool) -> Result<()> {
        if !self.editable() {
            return Err(Error::Denied);
        }
        let inspection = self.inspection.as_ref().ok_or(Error::Denied)?;
        if !inspection.can_edit_metadata {
            return Err(Error::Denied);
        }
        if let Override::Set(text) = &value {
            inspection
                .bounds
                .text("metadata_draft", text, inspection.bounds.max_text_bytes)?;
        }
        let draft = self.draft.as_mut().ok_or(Error::Denied)?;
        if title {
            draft.title = value;
        } else {
            draft.description = value;
        }
        self.phase = Phase::Editing;
        Ok(())
    }
    /// The host first resolves the tag through TagCatalogue; it supplies IDs and time.
    pub fn stage_tag(&mut self, candidate: TagAssignment) -> Result<()> {
        if !self.editable() {
            return Err(Error::Denied);
        }
        let inspection = self.inspection.as_ref().ok_or(Error::Denied)?;
        candidate.validate(&inspection.bounds, &BTreeSet::new())?;
        if !inspection
            .tag_choices
            .iter()
            .any(|choice| choice.tag == candidate.tag_ref)
        {
            return Err(Error::Denied);
        }
        let draft = self.draft.as_mut().ok_or(Error::Denied)?;
        if draft.additions.len() >= inspection.bounds.max_collection_items {
            return Err(Error::LimitExceeded {
                field: "tag_draft".into(),
                limit: inspection.bounds.max_collection_items,
            });
        }
        let mut assignments = inspection.assignments.clone();
        assignments.extend(draft.additions.clone());
        let target_scope = inspection
            .target(&candidate.target)
            .filter(|target| target.writable && target.scope == candidate.scope)
            .ok_or(Error::Denied)?
            .scope
            .clone();
        let writable: Vec<_> = inspection
            .targets
            .iter()
            .filter(|target| target.writable)
            .map(|target| target.target.clone())
            .collect();
        let checked =
            crate::tags::add_assignment(candidate, &assignments, &target_scope, &writable)?;
        if inspection
            .assignments
            .iter()
            .any(|value| value.assignment_id == checked.assignment_id)
        {
            draft.removals.remove(&checked.assignment_id);
        } else if !draft
            .additions
            .iter()
            .any(|value| value.assignment_id == checked.assignment_id)
        {
            checked.validate(&inspection.bounds, &BTreeSet::new())?;
            draft.additions.push(checked);
        }
        self.phase = Phase::Editing;
        Ok(())
    }
    pub fn remove_tag(&mut self, id: AssignmentId) -> Result<()> {
        if !self.editable() {
            return Err(Error::Denied);
        }
        let inspection = self.inspection.as_ref().ok_or(Error::Denied)?;
        let draft = self.draft.as_mut().ok_or(Error::Denied)?;
        if let Some(index) = draft
            .additions
            .iter()
            .position(|value| value.assignment_id == id)
        {
            draft.additions.remove(index);
        } else {
            let assignment = inspection
                .assignments
                .iter()
                .find(|value| value.assignment_id == id && value.removed_at.is_none())
                .ok_or(Error::NotFound)?;
            if !inspection
                .target(&assignment.target)
                .is_some_and(|target| target.writable)
            {
                return Err(Error::Denied);
            }
            draft.removals.insert(id);
        }
        self.phase = Phase::Editing;
        Ok(())
    }
    pub fn undo_removal(&mut self, id: AssignmentId) -> Result<()> {
        if !self.editable() {
            return Err(Error::Denied);
        }
        self.draft
            .as_mut()
            .ok_or(Error::Denied)?
            .removals
            .remove(&id);
        Ok(())
    }
    /// A local draft has no effects. Dispatched operations must be resolved at their owners.
    pub fn cancel(&mut self) -> bool {
        if matches!(
            self.phase,
            Phase::Editing | Phase::Conflict(_) | Phase::Error(_)
        ) {
            self.draft = None;
            self.phase = Phase::Inspect;
            true
        } else {
            false
        }
    }
    pub fn dirty(&self) -> bool {
        match (&self.inspection, &self.draft) {
            (Some(inspection), Some(draft)) => {
                draft.title != inspection.item.title_override
                    || draft.description != inspection.item.description_override
                    || !draft.additions.is_empty()
                    || !draft.removals.is_empty()
            }
            _ => false,
        }
    }
    pub fn begin_save(
        &mut self,
        context: CommandContext,
        supported: &BTreeSet<String>,
    ) -> Result<SaveRequest> {
        if !self.editable() || !self.dirty() {
            return Err(Error::Denied);
        }
        let inspection = self.inspection.as_ref().ok_or(Error::Denied)?;
        if context.scope != inspection.scope || context.bounds != inspection.bounds {
            return Err(Error::Denied);
        }
        if fences(&context.expected_revisions)? != fences(&inspection.owner_fences)? {
            return Err(Error::Conflict {
                field: "owner_fences".into(),
            });
        }
        let draft = self.draft.as_ref().ok_or(Error::Denied)?;
        let mut commands = vec![];
        if draft.title != inspection.item.title_override
            || draft.description != inspection.item.description_override
        {
            if !inspection.can_edit_metadata {
                return Err(Error::Denied);
            }
            commands.push(OwnerCommand::ItemPatch {
                item: inspection.item.item_id,
                patch: ItemPatch {
                    title: if draft.title == inspection.item.title_override {
                        Patch::Keep
                    } else {
                        Patch::Set(draft.title.clone())
                    },
                    description: if draft.description == inspection.item.description_override {
                        Patch::Keep
                    } else {
                        Patch::Set(draft.description.clone())
                    },
                    cover: Patch::Keep,
                    selected_observation: Patch::Keep,
                    gallery_observation: Patch::Keep,
                },
            });
        }
        commands.extend(draft.additions.iter().cloned().map(OwnerCommand::AddTag));
        let mut removal_targets = vec![];
        for id in &draft.removals {
            let assignment = inspection
                .assignments
                .iter()
                .find(|value| value.assignment_id == *id)
                .ok_or(Error::NotFound)?;
            if !inspection
                .target(&assignment.target)
                .is_some_and(|target| target.writable)
            {
                return Err(Error::Denied);
            }
            commands.push(OwnerCommand::RemoveTag(*id));
            removal_targets.push((*id, assignment.target.clone()));
        }
        let plan = plan_operation(context, commands, supported)?;
        let request = SaveRequest {
            ticket: SaveTicket {
                generation: self.generation,
                operation_id: plan.context.operation_id,
            },
            plan,
            removal_targets,
        };
        self.phase = Phase::Saving(request.ticket.operation_id);
        self.pending_features = supported.clone();
        self.pending = Some(request.clone());
        Ok(request)
    }
    /// Returns false for a response to a different session or operation.
    pub fn complete(&mut self, ticket: SaveTicket, result: Result<OperationOutcome>) -> bool {
        let Some(pending) = &self.pending else {
            return false;
        };
        if pending.ticket != ticket || ticket.generation != self.generation {
            return false;
        }
        self.phase = match result {
            Err(Error::Denied) => {
                self.revoke();
                return true;
            }
            Err(Error::UnknownOutcome { .. }) => Phase::Unknown(ticket.operation_id),
            Err(Error::Conflict { field }) => Phase::Conflict(field),
            Err(error) => Phase::Error(error),
            Ok(outcome) => {
                if validate_completion(&outcome, pending, &self.pending_features).is_err() {
                    Phase::Unknown(ticket.operation_id)
                } else {
                    match outcome.outcome {
                        OutcomeState::Pending => Phase::Unknown(ticket.operation_id),
                        OutcomeState::Conflict => Phase::Conflict("owner_revision".into()),
                        OutcomeState::Rejected => {
                            Phase::Error(invalid("save", "owner rejected operation"))
                        }
                        OutcomeState::Committed => {
                            self.draft = None;
                            Phase::ReloadRequired
                        }
                    }
                }
            }
        };
        if !matches!(self.phase, Phase::Unknown(_)) {
            self.pending = None;
            self.pending_features.clear();
        }
        true
    }
    /// Call only with a newly authorized snapshot; stale save tickets are invalidated.
    pub fn reload(&mut self, inspection: Inspection) -> Result<()> {
        if let Some(pending) = &self.pending {
            return Err(Error::UnknownOutcome {
                operation_id: pending.ticket.operation_id,
            });
        }
        inspection.validate()?;
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid("session", "generation exhausted"))?;
        self.inspection = Some(inspection);
        self.draft = None;
        self.pending = None;
        self.pending_features.clear();
        self.phase = Phase::Inspect;
        Ok(())
    }
    pub fn revoke(&mut self) {
        self.inspection = None;
        self.draft = None;
        self.pending = None;
        self.pending_features.clear();
        self.phase = Phase::Revoked;
    }
}
