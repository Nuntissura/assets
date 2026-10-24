//! Pure operation identity, replay, cancellation and receipt-completeness decisions.
use std::collections::BTreeSet;
use serde::{Deserialize,Serialize};
use crate::*;
use crate::ports::{CommandContext,OwnerCommand};

#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct OperationPlan {pub context:CommandContext,pub commands:Vec<OwnerCommand>}

#[derive(Serialize)]
struct Intent<'a> {
    operation_id:OperationId,scope:&'a ScopeRef,actor:&'a HostRef,
    #[serde(with="expected_revision_map")] expected_revisions:&'a [ExpectedRevision],
    commands:&'a [OwnerCommand],
}

pub fn plan_operation(mut context:CommandContext,commands:Vec<OwnerCommand>,supported:&BTreeSet<String>)->Result<OperationPlan> {
    context.bounds.validate()?;
    if commands.is_empty() || commands.len()>context.bounds.max_collection_items {
        return Err(Error::LimitExceeded{field:"owner_commands".into(),limit:context.bounds.max_collection_items});
    }
    if context.expected_revisions.len()>context.bounds.max_collection_items {return Err(Error::LimitExceeded{field:"expected_revisions".into(),limit:context.bounds.max_collection_items});}
    for value in [&context.scope.0,&context.actor.host_instance_id,&context.actor.owner_kind,&context.actor.native_id] {
        if value.is_empty() {return Err(invalid("operation_context","nonempty owner identity required"));}
        context.bounds.text("operation_context",value,context.bounds.max_text_bytes)?;
    }
    for expected in &context.expected_revisions {
        expected.revision.validate()?;
        for value in [&expected.owner.host_instance_id,&expected.owner.owner_kind,&expected.owner.native_id] {context.bounds.text("expected_owner",value,context.bounds.max_text_bytes)?;}
    }
    for command in &commands {
        match command {
            OwnerCommand::Capture{item,capture}=>{
                capture.validate(&context.bounds,supported)?;
                if capture.item_id!=*item || capture.access_partition!=context.scope
                    || capture.operation_id!=context.operation_id
                    || capture.actor_ref.as_ref().is_some_and(|actor|actor!=&context.actor) {
                    return Err(invalid("capture_context","capture must agree with its operation context"));
                }
                let normalized=crate::identity::normalize_url(&capture.captured_url,&context.bounds)?;
                if capture.normalization_version!=normalized.normalization_version {
                    return Err(Error::UnsupportedRequiredFeature{feature:"hsscrap.url_normalization_version".into()});
                }
                if capture.identity_url!=normalized.identity_url {
                    return Err(invalid("identity_url","capture URL normalization mismatch"));
                }
            },
            OwnerCommand::ItemPatch{patch,..}=>patch.validate(&context.bounds,supported)?,
            OwnerCommand::AcceptObservation{observation}=>{
                observation.validate(&context.bounds,supported)?;
                if observation.access_partition!=context.scope {
                    return Err(invalid("observation_scope","observation must agree with its operation scope"));
                }
            },
            OwnerCommand::AcceptInitialObservation{observation,initial}=>{
                initial.validate(&context.bounds,supported)?;
                observation.validate(&context.bounds,supported)?;
                if observation.access_partition!=context.scope || initial.capture.access_partition!=context.scope
                    || observation.item_id!=initial.item.item_id
                    || observation.capture_id!=Some(initial.capture.capture_id)
                    || observation.baseline_observation_id.is_some() || observation.sequence!=Revision(1)
                    || observation.status.state!=AvailabilityState::Available
                    || initial.capture.operation_id==context.operation_id
                    || initial.capture.actor_ref.as_ref().is_some_and(|actor|actor!=&context.actor) {
                    return Err(invalid("initial_observation_context","original creation and first observation must agree with their operation context"));
                }
            },
            OwnerCommand::AssertSourceIdentity(v)=>{
                v.validate(&context.bounds,supported)?;
                if [&v.provider,&v.namespace,&v.provider_item_id,&v.access_partition.0].iter().any(|value|value.is_empty()) {
                    return Err(invalid("source_identity","nonempty provider identity and scope required"));
                }
            },
            OwnerCommand::AddTag(v)=>v.validate(&context.bounds,supported)?,
            OwnerCommand::SaveMembership(v)=>v.validate(&context.bounds,supported)?,
            OwnerCommand::AddPlacement(v)|OwnerCommand::MovePlacement(v)=>v.validate(&context.bounds,supported)?,
            _=>{}
        }
    }
    let intent=Intent{operation_id:context.operation_id,scope:&context.scope,actor:&context.actor,expected_revisions:&context.expected_revisions,commands:&commands};
    let bytes=serde_json::to_vec(&intent).map_err(|_|invalid("operation_intent","invalid or duplicate revision map"))?;
    if bytes.len()>context.bounds.max_operation_bytes {return Err(Error::LimitExceeded{field:"operation_intent".into(),limit:context.bounds.max_operation_bytes});}
    let digest=handshake_canonical_json::sha256_hex(&bytes);
    if !context.intent_digest.is_empty() && context.intent_digest!=digest {return Err(Error::Conflict{field:"intent_digest".into()});}
    context.intent_digest=digest;
    Ok(OperationPlan{context,commands})
}

#[derive(Debug,Clone,PartialEq)]
pub enum OperationDecision {Begin,Replay(OperationOutcome),Recover(OperationOutcome)}

/// Authorization must be freshly resolved by the owning host before inspecting/replaying outcomes.
pub fn decide_operation(plan:&OperationPlan,existing:Option<&OperationOutcome>,authorized:bool)->Result<OperationDecision> {
    if !authorized {return Err(Error::Denied);}
    let Some(existing)=existing else {return Ok(OperationDecision::Begin);};
    if existing.operation_id!=plan.context.operation_id || existing.scope!=plan.context.scope {
        return Err(Error::Conflict{field:"operation_lookup".into()});
    }
    if existing.intent_digest!=plan.context.intent_digest {return Err(Error::Conflict{field:"operation_intent_reuse".into()});}
    if existing.outcome==OutcomeState::Pending {return Ok(OperationDecision::Recover(existing.clone()));}
    Ok(OperationDecision::Replay(existing.clone()))
}

pub fn pending_outcome(plan:&OperationPlan)->OperationOutcome {
    OperationOutcome{operation_id:plan.context.operation_id,scope:plan.context.scope.clone(),intent_digest:plan.context.intent_digest.clone(),expected_revisions:plan.context.expected_revisions.clone(),outcome:OutcomeState::Pending,result_refs:vec![],owner_receipt_refs:vec![],committed_at:None,common:Extensions::default()}
}

#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
pub struct OwnerStepReceipt {
    pub operation_id:OperationId,pub intent_digest:String,pub command_index:usize,pub owner:HostRef,pub receipt:HostRef,
}

/// The host verifies each receipt with its canonical owner. This checks complete attribution, not bytes or grants.
pub fn finish_operation(plan:&OperationPlan,receipts:&[OwnerStepReceipt],resolved_owners:&[HostRef],result_refs:Vec<HostRef>,at:Instant)->Result<OperationOutcome> {
    if receipts.len()!=plan.commands.len() || resolved_owners.len()!=plan.commands.len() {return Err(Error::Conflict{field:"incomplete_owner_commit".into()});}
    if result_refs.len()>plan.context.bounds.max_collection_items {return Err(Error::LimitExceeded{field:"result_refs".into(),limit:plan.context.bounds.max_collection_items});}
    let mut indexes=BTreeSet::new();let mut receipt_ids=BTreeSet::new();
    for receipt in receipts {
        if receipt.operation_id!=plan.context.operation_id || receipt.intent_digest!=plan.context.intent_digest
            || receipt.command_index>=plan.commands.len() || !indexes.insert(receipt.command_index)
            || resolved_owners[receipt.command_index]!=receipt.owner
            || !receipt_ids.insert(&receipt.receipt) {return Err(Error::Conflict{field:"owner_receipt".into()});}
        for reference in [&receipt.owner,&receipt.receipt] {for value in [&reference.host_instance_id,&reference.owner_kind,&reference.native_id] {plan.context.bounds.text("owner_receipt",value,plan.context.bounds.max_text_bytes)?;}}
    }
    for reference in &result_refs {for value in [&reference.host_instance_id,&reference.owner_kind,&reference.native_id] {plan.context.bounds.text("result_refs",value,plan.context.bounds.max_text_bytes)?;}}
    let mut outcome=pending_outcome(plan);outcome.outcome=OutcomeState::Committed;outcome.committed_at=Some(at);
    outcome.result_refs=result_refs;outcome.owner_receipt_refs=receipts.iter().map(|r|r.receipt.clone()).collect();
    Ok(outcome)
}

/// Cancellation never reverses a committed owner effect. Partial operations remain pending for explicit recovery.
pub fn cancel_operation(outcome:&OperationOutcome,authorized:bool)->Result<OperationOutcome> {
    if !authorized {return Err(Error::Denied);}
    let mut next=outcome.clone();
    if next.outcome==OutcomeState::Pending {
        next.common.extensions.insert("hsscrap.cancel_requested".into(),serde_json::Value::Bool(true));
        if next.owner_receipt_refs.is_empty() {next.outcome=OutcomeState::Rejected;}
    }
    Ok(next)
}

pub fn uncertain_outcome(operation_id:OperationId)->Error {Error::UnknownOutcome{operation_id}}
