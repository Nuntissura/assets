use super::*;
use crate::*;

/// Visit typed references, including source provenance. No UUID equality fallback exists.
pub(crate) fn map_references(
    record: &mut TransferRecord,
    map: &mut impl FnMut(OriginRef) -> Result<OriginRef>,
) -> Result<()> {
    macro_rules! field {
        ($field:expr,$kind:ident) => {{
            let mapped = map(OriginRef::$kind($field.clone()))?;
            let OriginRef::$kind(value) = mapped else {
                return Err(invalid(
                    "reference_mapping",
                    "destination reference kind differs",
                ));
            };
            $field = value;
        }};
    }
    macro_rules! optional {
        ($field:expr,$kind:ident) => {
            if let Some(value) = &mut $field {
                field!(*value, $kind);
            }
        };
    }
    macro_rules! override_ref {
        ($field:expr,$kind:ident) => {
            if let Override::Set(value) = &mut $field {
                field!(*value, $kind);
            }
        };
    }
    match record {
        TransferRecord::Item(v) => {
            field!(v.item_id, Item);
            field!(v.first_capture_id, Capture);
            optional!(v.selected_observation_id, Observation);
            optional!(v.gallery_observation_id, Observation);
            for id in &mut v.member_order {
                field!(*id, Member);
            }
            override_ref!(v.cover_override, Member);
        }
        TransferRecord::Capture(v) => {
            field!(v.capture_id, Capture);
            field!(v.item_id, Item);
            field!(v.operation_id, Operation);
            optional!(v.actor_ref, Host);
        }
        TransferRecord::Identity(v) => {
            field!(v.identity_id, Identity);
            field!(v.item_id, Item);
            field!(v.verified_by_observation_id, Observation);
        }
        TransferRecord::Observation(v) => {
            field!(v.observation_id, Observation);
            field!(v.item_id, Item);
            field!(v.stream_id, Stream);
            optional!(v.baseline_observation_id, Observation);
            optional!(v.capture_id, Capture);
            optional!(v.appearance_ref, Host);
            if let Some(g) = &mut v.gallery_snapshot {
                for member in &mut g.members {
                    field!(member.member_id, Member);
                    for id in &mut member.remote_reference_ids {
                        field!(*id, Remote);
                    }
                }
            }
        }
        TransferRecord::Stream(v) => {
            field!(v.stream_id, Stream);
            field!(v.item_id, Item);
            field!(v.source_ref, Host);
            optional!(v.latest_observation_id, Observation);
        }
        TransferRecord::Member(v) => {
            field!(v.member_id, Member);
            field!(v.item_id, Item);
            field!(v.observation_id, Observation);
        }
        TransferRecord::Remote(v) => {
            field!(v.reference_id, Remote);
            field!(v.member_ref.item_id, Item);
            field!(v.member_ref.member_id, Member);
            field!(v.observation_id, Observation);
            if let RemoteTarget::PrivateLocator(r) = &mut v.target {
                field!(*r, Host);
            }
        }
        TransferRecord::Binding(v) => {
            field!(v.binding_id, Binding);
            field!(v.member_ref.item_id, Item);
            field!(v.member_ref.member_id, Member);
            field!(v.media_ref, Media);
            field!(v.materialization_receipt_ref, Host);
            optional!(v.derived_from_binding_id, Binding);
        }
        TransferRecord::Assignment(v) => {
            field!(v.assignment_id, Assignment);
            field!(v.tag_ref, Tag);
            match &mut v.target {
                TagTarget::Item(v) => field!(*v, Item),
                TagTarget::Project(v) => field!(*v, Project),
                TagTarget::Membership(v) => field!(*v, Membership),
                TagTarget::Folder(v) => field!(*v, Folder),
                TagTarget::Occurrence(v) => field!(*v, Occurrence),
            }
        }
        TransferRecord::Membership(v) => {
            field!(v.membership_id, Membership);
            field!(v.item_id, Item);
            field!(v.project_ref, Project);
        }
        TransferRecord::Placement(v) => {
            field!(v.placement_id, Placement);
            field!(v.item_id, Item);
            optional!(v.membership_id, Membership);
            optional!(v.folder_ref, Folder);
        }
        TransferRecord::Occurrence(v) => {
            field!(v.occurrence_ref, Occurrence);
            field!(v.item_id, Item);
            optional!(v.member_id, Member);
            optional!(v.context_membership_id, Membership);
        }
    }
    match record {
        TransferRecord::Item(v) => field!(v.owner_scope, Scope),
        TransferRecord::Capture(v) => field!(v.access_partition, Scope),
        TransferRecord::Identity(v) => field!(v.access_partition, Scope),
        TransferRecord::Observation(v) => field!(v.access_partition, Scope),
        TransferRecord::Stream(v) => field!(v.access_partition, Scope),
        TransferRecord::Remote(v) => field!(v.access_partition, Scope),
        TransferRecord::Assignment(v) => field!(v.scope, Scope),
        TransferRecord::Membership(v) => field!(v.scope, Scope),
        TransferRecord::Placement(v) => field!(v.scope, Scope),
        _ => {}
    }
    Ok(())
}
pub(crate) fn references(record: &TransferRecord) -> Result<Vec<OriginRef>> {
    let mut refs = Vec::new();
    map_references(&mut record.clone(), &mut |r| {
        refs.push(r.clone());
        Ok(r)
    })?;
    Ok(refs)
}
