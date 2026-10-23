use crate::*;
use std::collections::BTreeSet;

fn text(b: &Bounds, field: &str, value: &str) -> Result<()> {
    b.text(field, value, b.max_text_bytes)
}
fn optional_text(b: &Bounds, field: &str, value: &Option<String>) -> Result<()> {
    if let Some(value) = value { text(b, field, value)?; }
    Ok(())
}
fn reference(b: &Bounds, field: &str, value: &HostRef) -> Result<()> {
    text(b, field, &value.host_instance_id)?;
    text(b, field, &value.owner_kind)?;
    text(b, field, &value.native_id)
}
fn count(b: &Bounds, field: &str, len: usize) -> Result<()> {
    if len > b.max_collection_items {
        return Err(Error::LimitExceeded { field: field.into(), limit: b.max_collection_items });
    }
    Ok(())
}
fn availability(b: &Bounds, value: &Availability) -> Result<()> {
    optional_text(b, "availability.reason_code", &value.reason_code)
}
fn removed(lifecycle: Lifecycle, at: &Option<Instant>) -> Result<()> {
    if (lifecycle == Lifecycle::Tombstoned) != at.is_some() {
        return Err(invalid("lifecycle", "tombstone and removal time must agree"));
    }
    Ok(())
}
fn tag_target(b: &Bounds, value: &TagTarget) -> Result<()> {
    match value {
        TagTarget::Project(r) => reference(b, "project_ref", &r.0),
        TagTarget::Folder(r) => reference(b, "folder_ref", &r.0),
        TagTarget::Occurrence(r) => reference(b, "occurrence_ref", &r.0),
        TagTarget::Item(_) | TagTarget::Membership(_) => Ok(()),
    }
}

impl Validate for SourceIdentity {
    fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(b, s)?;
        text(b, "access_partition", &self.access_partition.0)?;
        text(b, "provider", &self.provider)?;
        text(b, "namespace", &self.namespace)?;
        text(b, "provider_item_id", &self.provider_item_id)
    }
}
impl Validate for MemberObservation {
    fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(b, s)?;
        optional_text(b, "producer_member_key", &self.producer_member_key)?;
        optional_text(b, "caption", &self.caption)?;
        optional_text(b, "observed_mime", &self.observed_mime)?;
        availability(b, &self.availability)?;
        count(b, "remote_reference_ids", self.remote_reference_ids.len())?;
        for value in &self.remote_reference_ids { reference(b, "remote_reference_ids", value)?; }
        Ok(())
    }
}
impl Validate for GalleryMember {
    fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(b, s)?;
        optional_text(b, "producer_member_key", &self.producer_member_key)?;
        optional_text(b, "caption", &self.caption)?;
        optional_text(b, "observed_mime", &self.observed_mime)?;
        availability(b, &self.availability)?;
        if (self.lifecycle == MemberLifecycle::Tombstoned) != self.removed_at.is_some() {
            return Err(invalid("lifecycle", "member tombstone and removal time must agree"));
        }
        Ok(())
    }
}
impl Validate for RemoteMediaReference {
    fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(b, s)?;
        reference(b, "reference_id", &self.reference_id)?;
        text(b, "access_partition", &self.access_partition.0)?;
        availability(b, &self.availability)?;
        match &self.target {
            RemoteTarget::PrivateLocator(r) => reference(b, "private_locator", r),
            RemoteTarget::Unavailable(reason) => text(b, "unavailable", reason),
            RemoteTarget::PublicUrl(value) => {
                b.text("public_url", value, b.max_url_bytes)?;
                if !crate::validation::http_url(value) { return Err(invalid("public_url", "absolute http(s) URL without userinfo required")); }
                Ok(())
            }
        }
    }
}
impl Validate for RetainedMediaBinding {
    fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(b, s)?;
        reference(b, "media_ref", &self.media_ref.0)?;
        reference(b, "materialization_receipt_ref", &self.materialization_receipt_ref)?;
        text(b, "media_revision_ref", &self.media_revision_ref)?;
        if self.derived_from_binding_id == Some(self.binding_id) { return Err(invalid("derived_from_binding_id", "binding cannot derive from itself")); }
        Ok(())
    }
}
impl Validate for TagAssignment {
    fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(b, s)?;
        self.revision.validate()?;
        text(b, "scope", &self.scope.0)?;
        reference(b, "tag_ref", &self.tag_ref.0)?;
        tag_target(b, &self.target)
    }
}
impl Validate for ProjectMembership {
    fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(b, s)?;
        self.revision.validate()?;
        text(b, "scope", &self.scope.0)?;
        reference(b, "project_ref", &self.project_ref.0)?;
        removed(self.lifecycle, &self.removed_at)
    }
}
impl Validate for FolderPlacement {
    fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(b, s)?;
        self.revision.validate()?;
        text(b, "scope", &self.scope.0)?;
        if let Some(folder) = &self.folder_ref { reference(b, "folder_ref", &folder.0)?; }
        removed(self.lifecycle, &self.removed_at)
    }
}
impl Validate for OccurrenceReference {
    fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(b, s)?;
        self.revision.validate()?;
        reference(b, "occurrence_ref", &self.occurrence_ref.0)?;
        if let Override::Set(value) = &self.caption_override {
            if value.is_empty() { return Err(invalid("caption_override", "set empty is invalid; use clear")); }
            text(b, "caption_override", value)?;
        }
        if let Some(p) = &self.presentation {
            p.layout.validate_bounded(b, s)?;
            if let Some(r) = &p.crop {
                if ![r.x, r.y, r.width, r.height].iter().all(|n| n.is_finite())
                    || r.x < 0.0 || r.y < 0.0 || r.width <= 0.0 || r.height <= 0.0
                    || r.x + r.width > 1.0 || r.y + r.height > 1.0 {
                    return Err(invalid("crop", "normalized rectangle must fit within unit bounds"));
                }
            }
        }
        Ok(())
    }
}
impl Validate for ImportMapping {
    fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(b, s)?;
        self.local_revision_at_import.validate()?;
        text(b, "destination_scope", &self.destination_scope.0)?;
        text(b, "origin_instance_id", &self.origin_instance_id)?;
        text(b, "origin_kind", &self.origin_kind)?;
        text(b, "origin_id", &self.origin_id)?;
        reference(b, "local_ref", &self.local_ref)?;
        count(b, "accepted_versions", self.accepted_versions.len())?;
        let mut seen = BTreeSet::new();
        for entry in &self.accepted_versions {
            if !seen.insert(entry.stream_id) { return Err(invalid("accepted_versions", "duplicate stream")); }
            entry.version.revision.validate()?;
            text(b, "payload_hash", &entry.version.payload_hash)?;
        }
        Ok(())
    }
}
impl Validate for OperationOutcome {
    fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(b, s)?;
        text(b, "scope", &self.scope.0)?;
        text(b, "intent_digest", &self.intent_digest)?;
        count(b, "expected_revisions", self.expected_revisions.len())?;
        let mut seen = BTreeSet::new();
        for expected in &self.expected_revisions {
            if !seen.insert(&expected.owner) { return Err(invalid("expected_revisions", "duplicate owner")); }
            reference(b, "expected_revision.owner", &expected.owner)?;
            expected.revision.validate()?;
        }
        for (name, values) in [("result_refs", &self.result_refs), ("owner_receipt_refs", &self.owner_receipt_refs)] {
            count(b, name, values.len())?;
            for value in values { reference(b, name, value)?; }
        }
        if (self.outcome == OutcomeState::Committed) != self.committed_at.is_some() {
            return Err(invalid("committed_at", "only committed outcomes carry commit time"));
        }
        Ok(())
    }
}
impl Validate for ObservationStream {
    fn validate(&self, b: &Bounds, s: &BTreeSet<String>) -> Result<()> {
        self.common.validate_bounded(b, s)?;
        self.revision.validate()?;
        self.last_issued_sequence.validate()?;
        text(b, "producer", &self.producer)?;
        text(b, "provider_namespace", &self.provider_namespace)?;
        text(b, "access_partition", &self.access_partition.0)?;
        reference(b, "source_ref", &self.source_ref)?;
        if let Some(accepted) = self.last_accepted_sequence {
            accepted.validate()?;
            if accepted > self.last_issued_sequence { return Err(invalid("last_accepted_sequence", "cannot exceed issued sequence")); }
        }
        if self.last_accepted_sequence.is_some() != self.latest_observation_id.is_some() {
            return Err(invalid("latest_observation_id", "accepted sequence and observation must agree"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_admission_preserves_optional_extensions_and_rejects_invalid_tombstone() {
        let bounds = Bounds {
            max_url_bytes: 4096, max_text_bytes: 1024, max_collection_items: 100,
            max_page_items: 25, max_operation_bytes: 20000,
        };
        let mut wire = serde_json::json!({
            "member_id": "00000000-0000-0000-0000-000000000001",
            "item_id": "00000000-0000-0000-0000-000000000002",
            "producer_member_key": null,
            "observation_id": "00000000-0000-0000-0000-000000000003",
            "media_kind": "image", "caption": null, "observed_mime": null,
            "observed_width": null, "observed_height": null, "observed_duration_ms": "0",
            "availability": {"state": "restricted", "checked_at": null, "reason_code": "denied", "retry_after": null},
            "lifecycle": "active", "removed_at": null,
            "extensions": {"vendor.metadata": {"keep": true}}, "required_features": []
        });
        let accepted: GalleryMember = decode(&wire.to_string(), &bounds, &BTreeSet::new()).unwrap();
        assert_eq!(serde_json::to_value(accepted).unwrap(), wire);
        wire["lifecycle"] = "tombstoned".into();
        assert!(matches!(decode::<GalleryMember>(&wire.to_string(), &bounds, &BTreeSet::new()), Err(Error::Invalid { field, .. }) if field == "lifecycle"));
        wire["lifecycle"] = "active".into();
        wire["required_features"] = serde_json::json!(["vendor.unknown"]);
        assert!(matches!(decode::<GalleryMember>(&wire.to_string(), &bounds, &BTreeSet::new()), Err(Error::UnsupportedRequiredFeature { .. })));
    }
}
