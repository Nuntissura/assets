use super::*;
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

/// Construct only after the negotiated host envelope has verified trust, audience and live access.
#[derive(Debug, Clone)]
pub struct VerifiedEnvelope {
    pub origin_instance_id: String,
    pub export_stream_id: ExportStreamId,
    pub audience_ref: ScopeRef,
    pub projection_policy_id: String,
}
/// Host-resolved, type-preserving mappings for exactly one origin and destination scope.
#[derive(Debug, Clone)]
pub struct DestinationMappings {
    pub origin_instance_id: String,
    pub destination_scope: ScopeRef,
    pub references: BTreeMap<OriginRef, OriginRef>,
    /// Canonical destination resource owner references, keyed by the source record identity.
    pub local_record_refs: BTreeMap<OriginRef, HostRef>,
    /// Supplied only after the destination media owner verifies/materializes bytes.
    pub materialized_bindings: BTreeMap<BindingId, RetainedMediaBinding>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineReason {
    MissingReferences(Vec<OriginRef>),
    MediaMaterialization(BindingId),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportResolution {
    Preserve,
    /// Explicit host conflict resolution, fenced by the current local record revision.
    Resolve {
        expected_local_revision: Revision,
    },
}
#[derive(Debug, Clone, PartialEq)]
pub enum ImportPlan {
    Reuse {
        mapping: ImportMapping,
    },
    Apply {
        record: TransferRecord,
        mapping: ImportMapping,
        source_payload_hash: String,
        source_gallery_digest: Option<String>,
    },
    ProvenanceOnly {
        mapping: ImportMapping,
    },
    Quarantined {
        reason: QuarantineReason,
    },
    Conflict {
        field: String,
    },
}
fn conflict(field: &str) -> ImportPlan {
    ImportPlan::Conflict {
        field: field.into(),
    }
}
fn local_revision(record: &TransferRecord) -> Option<Revision> {
    match record {
        TransferRecord::Item(v) => Some(v.revision),
        TransferRecord::Stream(v) => Some(v.revision),
        TransferRecord::Assignment(v) => Some(v.revision),
        TransferRecord::Membership(v) => Some(v.revision),
        TransferRecord::Placement(v) => Some(v.revision),
        TransferRecord::Occurrence(v) => Some(v.revision),
        _ => None,
    }
}
fn removed(record: &TransferRecord) -> bool {
    match record {
        TransferRecord::Item(v) => v.lifecycle == Lifecycle::Tombstoned,
        TransferRecord::Member(v) => v.lifecycle == MemberLifecycle::Tombstoned,
        TransferRecord::Identity(v) => v.retired_at.is_some(),
        TransferRecord::Assignment(v) => v.removed_at.is_some(),
        TransferRecord::Membership(v) => v.lifecycle == Lifecycle::Tombstoned,
        TransferRecord::Placement(v) => v.lifecycle == Lifecycle::Tombstoned,
        _ => false,
    }
}
fn local_authored(record: &TransferRecord) -> bool {
    match record {
        TransferRecord::Item(v) => {
            !matches!(v.title_override, Override::Inherit)
                || !matches!(v.description_override, Override::Inherit)
                || !matches!(v.cover_override, Override::Inherit)
        }
        TransferRecord::Occurrence(v) => {
            !matches!(v.caption_override, Override::Inherit) || v.presentation.is_some()
        }
        _ => false,
    }
}
fn accept_version(
    mapping: &mut ImportMapping,
    source: &TransferProjection,
    b: &Bounds,
    s: &BTreeSet<String>,
) -> Result<()> {
    let fields = &source.fields;
    let version = AcceptedStreamVersion {
        stream_id: fields.export_stream_id,
        version: AcceptedVersion {
            revision: fields.revision,
            payload_hash: source.payload_hash.clone(),
        },
    };
    if let Some(previous) = mapping
        .accepted_versions
        .iter_mut()
        .find(|v| v.stream_id == fields.export_stream_id)
    {
        *previous = version;
    } else {
        mapping.accepted_versions.push(version);
    }
    mapping.accepted_versions.sort_by_key(|v| v.stream_id);
    mapping.validate(b, s)
}
/// Verify the complete source fields before remapping. Returned plans require the host's atomic
/// revision/authorization fence at commit; planning cannot authorize a destination write.
pub fn plan_import(
    source: &TransferProjection,
    envelope: &VerifiedEnvelope,
    mappings: &DestinationMappings,
    previous: Option<&ImportMapping>,
    current: Option<&TransferRecord>,
    local_revision_now: Revision,
    at: Instant,
    resolution: ImportResolution,
    b: &Bounds,
    s: &BTreeSet<String>,
) -> Result<ImportPlan> {
    validate_fields(&source.fields, b, s)?;
    verify_hash(source)?;
    local_revision_now.validate()?;
    let fields = &source.fields;
    if envelope.origin_instance_id != fields.origin_instance_id
        || envelope.export_stream_id != fields.export_stream_id
        || envelope.audience_ref != fields.audience_ref
        || envelope.projection_policy_id != fields.projection_policy_id
        || mappings.origin_instance_id != fields.origin_instance_id
    {
        return Err(Error::Denied);
    }
    if mappings.destination_scope.0.is_empty() {
        return Err(invalid("destination_scope", "required"));
    }
    b.text(
        "destination_scope",
        &mappings.destination_scope.0,
        b.max_text_bytes,
    )?;
    if mappings.references.len() > b.max_collection_items
        || mappings.materialized_bindings.len() > b.max_collection_items
        || mappings.local_record_refs.len() > b.max_collection_items
    {
        return Err(Error::LimitExceeded {
            field: "destination_mappings".into(),
            limit: b.max_collection_items,
        });
    }
    let resolved = matches!(resolution,ImportResolution::Resolve{expected_local_revision} if expected_local_revision==local_revision_now);
    if matches!(resolution, ImportResolution::Resolve { .. }) && !resolved {
        return Ok(conflict("local_revision"));
    }
    if let Some(current) = current {
        current.validate(b, s)?;
        if local_revision(current).is_some_and(|r| r != local_revision_now) {
            return Ok(conflict("local_revision"));
        }
    }
    if let Some(previous) = previous {
        super::model::preflight_extensions(&previous.common, b, &mut 0)?;
        previous.validate(b, s)?;
        if previous.destination_scope != mappings.destination_scope
            || previous.origin_instance_id != fields.origin_instance_id
            || previous.origin_kind != fields.origin_kind
            || previous.origin_id != fields.origin_id
        {
            return Ok(conflict("import_mapping_scope"));
        }
        if let Some(accepted) = previous
            .accepted_versions
            .iter()
            .find(|v| v.stream_id == fields.export_stream_id)
        {
            if fields.revision == accepted.version.revision {
                return Ok(if source.payload_hash == accepted.version.payload_hash {
                    ImportPlan::Reuse {
                        mapping: previous.clone(),
                    }
                } else {
                    conflict("equal_revision_hash")
                });
            }
            if fields.revision.0 < accepted.version.revision.0 {
                return Ok(conflict("older_revision"));
            }
        } else if !previous.accepted_versions.is_empty() && !resolved {
            return Ok(conflict("different_export_stream"));
        }
        if removed(&fields.record) && current.is_some() {
            let mut mapping = previous.clone();
            mapping.state = ImportState::SourceDeleted;
            mapping.local_revision_at_import = local_revision_now;
            mapping.imported_at = at;
            accept_version(&mut mapping, source, b, s)?;
            return Ok(ImportPlan::ProvenanceOnly { mapping });
        }
        if (previous.state != ImportState::Active
            || previous.local_revision_at_import != local_revision_now)
            && !resolved
        {
            return Ok(conflict("local_changes"));
        }
    } else if current.is_some() && !resolved {
        return Ok(conflict("unmapped_local_record"));
    }
    if current.is_some_and(|v| removed(v) || local_authored(v)) && !resolved {
        return Ok(conflict("local_authored_or_tombstone"));
    }

    let mut missing = BTreeSet::new();
    let mut record = fields.record.clone();
    // A media receipt is local evidence, never an ordinary rewrite of the foreign receipt UUID.
    let binding = if let TransferRecord::Binding(source_binding) = &record {
        let Some(local) = mappings
            .materialized_bindings
            .get(&source_binding.binding_id)
        else {
            return Ok(ImportPlan::Quarantined {
                reason: QuarantineReason::MediaMaterialization(source_binding.binding_id),
            });
        };
        super::model::preflight_extensions(&local.common, b, &mut 0)?;
        local.validate(b, s)?;
        Some(local.clone())
    } else {
        None
    };
    super::references::map_references(&mut record, &mut |origin| {
        if binding.is_some()
            && matches!(&fields.record,TransferRecord::Binding(v) if origin==OriginRef::Host(v.materialization_receipt_ref.clone()))
        {
            return Ok(OriginRef::Host(
                binding
                    .as_ref()
                    .unwrap()
                    .materialization_receipt_ref
                    .clone(),
            ));
        }
        match mappings.references.get(&origin) {
            Some(destination) => Ok(destination.clone()),
            None => {
                missing.insert(origin.clone());
                Ok(origin)
            }
        }
    })?;
    if !missing.is_empty() {
        return Ok(ImportPlan::Quarantined {
            reason: QuarantineReason::MissingReferences(missing.into_iter().collect()),
        });
    }
    let local_origin = record.origin();
    let Some(local_ref) = mappings
        .local_record_refs
        .get(&fields.record.origin())
        .cloned()
    else {
        return Ok(ImportPlan::Quarantined {
            reason: QuarantineReason::MissingReferences(vec![fields.record.origin()]),
        });
    };
    for value in [
        &local_ref.host_instance_id,
        &local_ref.owner_kind,
        &local_ref.native_id,
    ] {
        if value.is_empty() {
            return Err(invalid(
                "local_record_ref",
                "canonical owner reference required",
            ));
        }
        b.text("local_record_ref", value, b.max_text_bytes)?;
    }
    if let Some(previous) = previous {
        if previous.local_ref != local_ref {
            return Ok(conflict("local_identity"));
        }
    }
    if let Some(current) = current {
        if current.origin() != local_origin {
            return Ok(conflict("local_identity"));
        }
    }
    let source_gallery_digest = if let TransferRecord::Observation(v) = &fields.record {
        v.gallery_snapshot
            .as_ref()
            .and_then(|g| g.full_set_digest.clone())
    } else {
        None
    };
    if let TransferRecord::Observation(v) = &mut record {
        if let Some(g) = &mut v.gallery_snapshot {
            if g.coverage == Coverage::Complete {
                let source_observation = match &fields.record {
                    TransferRecord::Observation(v) => v,
                    _ => unreachable!(),
                };
                let source_gallery = source_observation.gallery_snapshot.as_ref().unwrap();
                let source_ids: Vec<_> =
                    source_gallery.members.iter().map(|m| m.member_id).collect();
                if source_gallery.full_set_digest.as_ref()
                    != Some(&crate::gallery::gallery_digest(&source_ids)?)
                {
                    return Ok(conflict("source_gallery_digest"));
                }
                g.full_set_digest = Some(crate::gallery::gallery_digest(
                    &g.members.iter().map(|m| m.member_id).collect::<Vec<_>>(),
                )?);
            }
        }
    }
    if let Some(mut local) = binding {
        local.validate(b, s)?;
        let TransferRecord::Binding(remapped) = &record else {
            unreachable!()
        };
        if local.binding_id != remapped.binding_id
            || local.member_ref != remapped.member_ref
            || local.media_ref != remapped.media_ref
            || local.role != remapped.role
            || local.derived_from_binding_id != remapped.derived_from_binding_id
        {
            return Ok(conflict("materialization_mapping"));
        }
        if local.common != Extensions::default() && local.common != remapped.common {
            return Ok(conflict("materialization_extensions"));
        }
        local.common = remapped.common.clone();
        record = TransferRecord::Binding(local);
    }
    record.validate(b, s)?;
    let mut mapping = previous.cloned().unwrap_or_else(|| ImportMapping {
        destination_scope: mappings.destination_scope.clone(),
        origin_instance_id: fields.origin_instance_id.clone(),
        origin_kind: fields.origin_kind.clone(),
        origin_id: fields.origin_id.clone(),
        local_ref,
        accepted_versions: Vec::new(),
        local_revision_at_import: local_revision_now,
        imported_at: at.clone(),
        state: ImportState::Active,
        common: Extensions::default(),
    });
    accept_version(&mut mapping, source, b, s)?;
    mapping.local_revision_at_import = local_revision_now.next()?;
    mapping.imported_at = at;
    if removed(&fields.record) && current.is_some() {
        mapping.state = ImportState::SourceDeleted;
        mapping.local_revision_at_import = local_revision_now;
        mapping.validate(b, s)?;
        return Ok(ImportPlan::ProvenanceOnly { mapping });
    }
    mapping.state = ImportState::Active;
    mapping.validate(b, s)?;
    let next_revision = mapping.local_revision_at_import;
    match &mut record {
        TransferRecord::Item(v) => v.revision = next_revision,
        TransferRecord::Stream(v) => v.revision = next_revision,
        TransferRecord::Assignment(v) => v.revision = next_revision,
        TransferRecord::Membership(v) => v.revision = next_revision,
        TransferRecord::Placement(v) => v.revision = next_revision,
        TransferRecord::Occurrence(v) => v.revision = next_revision,
        _ => {}
    }
    Ok(ImportPlan::Apply {
        record,
        mapping,
        source_payload_hash: source.payload_hash.clone(),
        source_gallery_digest,
    })
}

/// Revocation changes provenance/access only. The host preserves locally authored records and grants.
pub fn plan_revocation(
    mapping: &ImportMapping,
    b: &Bounds,
    s: &BTreeSet<String>,
) -> Result<ImportMapping> {
    b.validate()?;
    super::model::preflight_extensions(&mapping.common, b, &mut 0)?;
    mapping.validate(b, s)?;
    let mut next = mapping.clone();
    next.state = ImportState::Revoked;
    Ok(next)
}
