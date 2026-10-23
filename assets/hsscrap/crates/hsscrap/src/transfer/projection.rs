use super::*;
use crate::*;
use std::collections::BTreeSet;

/// Host-authorized identities. capture_order is the owner's complete chronological capture order.
/// Authorized member identity and permission to expose its locators/metadata are separate decisions.
#[derive(Debug, Clone)]
pub struct ProjectionPolicy {
    pub policy_id: String,
    pub audience_ref: ScopeRef,
    pub authorized: BTreeSet<OriginRef>,
    pub readable_members: BTreeSet<MemberId>,
    pub capture_order: Vec<CaptureId>,
}
fn partial(common: &mut Extensions, key: &str) {
    common
        .extensions
        .insert(key.into(), serde_json::Value::Bool(true));
}
fn restricted() -> Availability {
    Availability {
        state: AvailabilityState::Restricted,
        checked_at: None,
        reason_code: Some("projection_restricted".into()),
        retry_after: None,
    }
}

/// Input order is retained only where the model gives an array semantic order.
pub fn project_records(
    records: &[TransferRecord],
    policy: &ProjectionPolicy,
    b: &Bounds,
    s: &BTreeSet<String>,
) -> Result<Vec<TransferRecord>> {
    b.validate()?;
    if records.len() > b.max_collection_items
        || policy.authorized.len() > b.max_collection_items
        || policy.capture_order.len() > b.max_collection_items
    {
        return Err(Error::LimitExceeded {
            field: "projection".into(),
            limit: b.max_collection_items,
        });
    }
    let unique: BTreeSet<_> = records.iter().map(TransferRecord::origin).collect();
    if unique.len() != records.len() {
        return Err(invalid("records", "duplicate origin"));
    }
    let mut captures = std::collections::BTreeMap::new();
    for record in records {
        record.validate(b, s)?;
        if let TransferRecord::Capture(c) = record {
            captures.insert(c.capture_id, c);
        }
    }
    if policy.capture_order.len() != captures.len()
        || policy
            .capture_order
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .len()
            != policy.capture_order.len()
        || policy
            .capture_order
            .iter()
            .any(|id| !captures.contains_key(id))
    {
        return Err(invalid(
            "capture_order",
            "canonical unique capture references required",
        ));
    }
    let allowed = |r: &OriginRef| policy.authorized.contains(r);
    let denied_locator_members:BTreeSet<_>=records.iter().filter_map(|record|match record {
        TransferRecord::Remote(remote) if matches!(&remote.target,RemoteTarget::PrivateLocator(locator) if !allowed(&OriginRef::Host(locator.clone())))=>Some(remote.member_ref.member_id),
        _=>None,
    }).collect();
    let readable_member = |id: &MemberId| {
        policy.readable_members.contains(id) && !denied_locator_members.contains(id)
    };
    let usable_capture = |capture: &URLCapture| {
        super::references::references(&TransferRecord::Capture(capture.clone()))
            .is_ok_and(|refs| refs.iter().all(&allowed))
    };
    let mut projected = Vec::new();
    for source in records {
        if !allowed(&source.origin()) {
            continue;
        }
        let mut record = source.clone();
        match &mut record {
            TransferRecord::Item(v) => {
                if !captures
                    .get(&v.first_capture_id)
                    .is_some_and(|capture| usable_capture(capture))
                {
                    let first = policy
                        .capture_order
                        .iter()
                        .filter_map(|id| captures.get(id).copied())
                        .find(|capture| {
                            capture.item_id == v.item_id
                                && allowed(&OriginRef::Capture(capture.capture_id))
                                && usable_capture(capture)
                        });
                    let Some(first) = first else {
                        continue;
                    };
                    v.first_capture_id = first.capture_id;
                    partial(&mut v.common, "hsscrap.capture_history_partial");
                }
                for selected in [
                    &mut v.selected_observation_id,
                    &mut v.gallery_observation_id,
                ] {
                    if selected.is_some_and(|id| !allowed(&OriginRef::Observation(id))) {
                        *selected = None;
                    }
                }
                let old = v.member_order.len();
                v.member_order.retain(|id| allowed(&OriginRef::Member(*id)));
                if old != v.member_order.len() {
                    partial(&mut v.common, "hsscrap.gallery_partial");
                }
                if matches!(v.cover_override,Override::Set(id) if !v.member_order.contains(&id)) {
                    v.cover_override = Override::Inherit;
                }
            }
            TransferRecord::Observation(v) => {
                if v.baseline_observation_id
                    .is_some_and(|id| !allowed(&OriginRef::Observation(id)))
                {
                    v.baseline_observation_id = None;
                }
                if v.capture_id
                    .is_some_and(|id| !allowed(&OriginRef::Capture(id)))
                {
                    v.capture_id = None;
                }
                if v.appearance_ref
                    .as_ref()
                    .is_some_and(|r| !allowed(&OriginRef::Host(r.clone())))
                {
                    v.appearance_ref = None;
                }
                if v.capture_id.is_none() && v.appearance_ref.is_none() {
                    continue;
                }
                if let Some(g) = &mut v.gallery_snapshot {
                    let old = g.members.len();
                    g.members
                        .retain(|m| allowed(&OriginRef::Member(m.member_id)));
                    if g.members.len() != old {
                        g.coverage = Coverage::Partial;
                        g.full_set_digest = None;
                        partial(&mut v.common, "hsscrap.gallery_partial");
                    }
                    for m in &mut g.members {
                        m.remote_reference_ids
                            .retain(|r| allowed(&OriginRef::Remote(r.clone())));
                        if !readable_member(&m.member_id) {
                            m.producer_member_key = None;
                            m.caption = None;
                            m.observed_mime = None;
                            m.observed_width = None;
                            m.observed_height = None;
                            m.observed_duration_ms = None;
                            m.availability = restricted();
                            m.remote_reference_ids.clear();
                            m.common = Extensions::default();
                        }
                    }
                }
            }
            TransferRecord::Member(v) if !readable_member(&v.member_id) => {
                v.producer_member_key = None;
                v.caption = None;
                v.observed_mime = None;
                v.observed_width = None;
                v.observed_height = None;
                v.observed_duration_ms = None;
                v.availability = restricted();
                v.common = Extensions::default();
            }
            TransferRecord::Remote(v) => {
                if !readable_member(&v.member_ref.member_id) {
                    v.target = RemoteTarget::Unavailable("projection_restricted".into());
                    v.availability = restricted();
                    v.expires_at = None;
                    v.common = Extensions::default();
                }
            }
            TransferRecord::Stream(v) => {
                if v.latest_observation_id
                    .is_some_and(|id| !allowed(&OriginRef::Observation(id)))
                {
                    v.latest_observation_id = None;
                    v.last_accepted_sequence = None;
                }
            }
            _ => {}
        }
        if super::references::references(&record)?.iter().all(&allowed) {
            record.validate(b, s)?;
            projected.push(record);
        }
    }
    // A permitted identity is insufficient when its canonical record was omitted by filtering.
    // Repeatedly remove records whose domain references became unavailable; host references are external.
    loop {
        let present: BTreeSet<_> = projected.iter().map(TransferRecord::origin).collect();
        let old = projected.len();
        projected.retain(|r| {
            super::references::references(r).is_ok_and(|refs| {
                refs.iter().all(|id| match id {
                    OriginRef::Item(_)
                    | OriginRef::Capture(_)
                    | OriginRef::Identity(_)
                    | OriginRef::Observation(_)
                    | OriginRef::Stream(_)
                    | OriginRef::Member(_)
                    | OriginRef::Binding(_)
                    | OriginRef::Assignment(_)
                    | OriginRef::Membership(_)
                    | OriginRef::Placement(_)
                    | OriginRef::Remote(_) => present.contains(id),
                    _ => true,
                })
            })
        });
        if old == projected.len() {
            break;
        }
    }
    Ok(projected)
}

pub fn plan_export(
    mut fields: TransferFields,
    previous: Option<&ExportCursor>,
    b: &Bounds,
    s: &BTreeSet<String>,
) -> Result<ExportPlan> {
    // The revision is excluded only from the private change fingerprint, never from payload_hash.
    fields.revision = Revision(1);
    validate_fields(&fields, b, s)?;
    let content_hash = handshake_canonical_json::sha256_hex(&canonical_fields_bytes(&fields)?);
    let origin = fields.record.origin();
    if let Some(previous) = previous {
        previous.revision.validate()?;
        if previous.export_stream_id != fields.export_stream_id
            || previous.origin_instance_id != fields.origin_instance_id
            || previous.origin != origin
            || previous.projection_policy_id != fields.projection_policy_id
            || previous.audience_ref != fields.audience_ref
        {
            return Err(Error::Conflict {
                field: "export_lineage".into(),
            });
        }
        fields.revision = if previous.content_hash == content_hash {
            previous.revision
        } else {
            previous.revision.next()?
        };
    }
    let payload_hash = handshake_canonical_json::sha256_hex(&canonical_fields_bytes(&fields)?);
    let cursor = ExportCursor {
        export_stream_id: fields.export_stream_id,
        origin_instance_id: fields.origin_instance_id.clone(),
        origin,
        projection_policy_id: fields.projection_policy_id.clone(),
        audience_ref: fields.audience_ref.clone(),
        revision: fields.revision,
        content_hash,
    };
    Ok(ExportPlan {
        projection: TransferProjection {
            fields,
            payload_hash,
        },
        cursor,
    })
}

/// Bind the host's current authorization policy to the stream fields before hashing.
/// A withheld record returns no projection and never mutates the canonical source.
pub fn project_export(
    mut fields: TransferFields,
    records: &[TransferRecord],
    policy: &ProjectionPolicy,
    previous: Option<&ExportCursor>,
    b: &Bounds,
    s: &BTreeSet<String>,
) -> Result<Option<ExportPlan>> {
    let origin = fields.record.origin();
    let projected = project_records(records, policy, b, s)?;
    let Some(record) = projected.into_iter().find(|v| v.origin() == origin) else {
        return Ok(None);
    };
    fields.record = record;
    fields.projection_policy_id = policy.policy_id.clone();
    fields.audience_ref = policy.audience_ref.clone();
    plan_export(fields, previous, b, s).map(Some)
}
