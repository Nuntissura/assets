//! Validate retained associations against explicit media-owner metadata and materialization receipts.
use std::collections::BTreeSet;
use crate::*;
use crate::ports::{MediaMetadata,MediaReceipt};

pub fn validate_binding(
    binding:&RetainedMediaBinding,target:&GalleryMember,metadata:&MediaMetadata,receipt:&MediaReceipt,
    known:&[RetainedMediaBinding],authorized_cross_item_parents:&BTreeSet<BindingId>,
    bounds:&Bounds,supported:&BTreeSet<String>,
)->Result<()> {
    binding.validate(bounds,supported)?;
    target.validate(bounds,supported)?;
    if binding.member_ref.item_id!=target.item_id || binding.member_ref.member_id!=target.member_id || target.lifecycle==MemberLifecycle::Tombstoned {
        return Err(Error::Conflict{field:"binding_member".into()});
    }
    if known.len()>bounds.max_collection_items {return Err(Error::LimitExceeded{field:"binding_lineage".into(),limit:bounds.max_collection_items});}
    if binding.media_ref!=metadata.media_ref || binding.media_ref!=receipt.media_ref
        || binding.media_revision_ref!=metadata.revision || binding.media_revision_ref!=receipt.revision
        || binding.materialization_receipt_ref!=receipt.receipt_ref {
        return Err(Error::Conflict{field:"materialization_receipt".into()});
    }
    if metadata.availability.state!=AvailabilityState::Available {return Err(invalid("media_availability","retained association requires available verified bytes"));}
    if binding.role==RetainedRole::Derived && binding.derived_from_binding_id.is_none() {return Err(invalid("derived_from_binding_id","derived media requires original lineage"));}
    let mut seen=BTreeSet::from([binding.binding_id]);
    let mut next=binding.derived_from_binding_id;
    while let Some(id)=next {
        if !seen.insert(id) {return Err(Error::Conflict{field:"binding_lineage_cycle".into()});}
        let mut candidates=known.iter().filter(|b|b.binding_id==id);
        let parent=candidates.next().ok_or(Error::NotFound)?;
        if candidates.next().is_some() {return Err(Error::Conflict{field:"binding_id".into()});}
        parent.validate(bounds,supported)?;
        if parent.member_ref.item_id!=binding.member_ref.item_id && !authorized_cross_item_parents.contains(&id) {return Err(Error::Denied);}
        next=parent.derived_from_binding_id;
    }
    Ok(())
}

pub fn validate_occurrence(
    occurrence:&OccurrenceReference,item:&Item,members:&[GalleryMember],
    memberships:&[ProjectMembership],bounds:&Bounds,supported:&BTreeSet<String>,
)->Result<()> {
    occurrence.validate(bounds,supported)?;
    item.validate(bounds,supported)?;
    if members.len()>bounds.max_collection_items || memberships.len()>bounds.max_collection_items {return Err(Error::LimitExceeded{field:"occurrence_context".into(),limit:bounds.max_collection_items});}
    let mut member_ids=BTreeSet::new();let mut membership_ids=BTreeSet::new();
    for member in members {
        member.validate(bounds,supported)?;
        if !member_ids.insert(member.member_id) {return Err(Error::Conflict{field:"member_id".into()});}
    }
    for membership in memberships {
        membership.validate(bounds,supported)?;
        if !membership_ids.insert(membership.membership_id) {return Err(Error::Conflict{field:"membership_id".into()});}
    }
    if occurrence.item_id!=item.item_id {return Err(Error::Conflict{field:"occurrence_item".into()});}
    if let Some(member)=occurrence.member_id {
        if !members.iter().any(|m|m.member_id==member && m.item_id==item.item_id) {return Err(Error::NotFound);}
    }
    if let Some(context)=occurrence.context_membership_id {
        if !memberships.iter().any(|m|m.membership_id==context && m.item_id==item.item_id) {return Err(Error::Conflict{field:"context_membership".into()});}
    }
    // Tombstone references remain resolvable. Hosts return removed/restricted state, never grants.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn host(id:&str)->HostRef {HostRef{host_instance_id:"host".into(),owner_kind:"media".into(),native_id:id.into()}}
    fn at()->Instant {Instant::parse("2026-10-23T00:00:00Z").unwrap()}
    fn bounds()->Bounds {Bounds{max_url_bytes:4096,max_text_bytes:4096,max_collection_items:100,max_page_items:25,max_operation_bytes:20000}}
    fn target(binding:&RetainedMediaBinding)->GalleryMember {GalleryMember{
        member_id:binding.member_ref.member_id,item_id:binding.member_ref.item_id,producer_member_key:None,
        observation_id:ObservationId(uuid::Uuid::from_u128(20)),media_kind:MediaKind::Image,caption:None,observed_mime:None,
        observed_width:None,observed_height:None,observed_duration_ms:None,
        availability:Availability{state:AvailabilityState::Available,checked_at:None,reason_code:None,retry_after:None},
        lifecycle:MemberLifecycle::Active,removed_at:None,common:Extensions::default(),
    }}
    #[test] fn repeated_bytes_remain_distinct_members_and_receipts_and_lineage_are_checked() {
        let media=MediaRef(host("same-bytes"));let receipt_ref=host("receipt");
        let metadata=MediaMetadata{media_ref:media.clone(),revision:"rev1".into(),mime:"image/jpeg".into(),measured_width:Some(10),measured_height:Some(10),content_hash:"hash".into(),availability:Availability{state:AvailabilityState::Available,checked_at:None,reason_code:None,retry_after:None}};
        let receipt=MediaReceipt{media_ref:media.clone(),receipt_ref:receipt_ref.clone(),revision:"rev1".into()};
        let first=RetainedMediaBinding{binding_id:BindingId(uuid::Uuid::from_u128(1)),member_ref:MemberRef{item_id:ItemId(uuid::Uuid::from_u128(10)),member_id:MemberId(uuid::Uuid::from_u128(11))},media_ref:media,media_revision_ref:"rev1".into(),role:RetainedRole::Original,materialization_receipt_ref:receipt_ref,created_at:at(),derived_from_binding_id:None,common:Extensions::default()};
        let mut second=first.clone();second.binding_id=BindingId(uuid::Uuid::from_u128(2));second.member_ref.member_id=MemberId(uuid::Uuid::from_u128(12));
        for b in [&first,&second] {validate_binding(b,&target(b),&metadata,&receipt,&[],&BTreeSet::new(),&bounds(),&BTreeSet::new()).unwrap();}
        assert_ne!(first.member_ref,second.member_ref);assert_eq!(first.media_ref,second.media_ref);
        let mut wrong=receipt.clone();wrong.receipt_ref=host("not-a-receipt");
        assert!(validate_binding(&first,&target(&first),&metadata,&wrong,&[],&BTreeSet::new(),&bounds(),&BTreeSet::new()).is_err());
        assert!(validate_binding(&first,&target(&second),&metadata,&receipt,&[],&BTreeSet::new(),&bounds(),&BTreeSet::new()).is_err());
        second.role=RetainedRole::Derived;second.derived_from_binding_id=Some(first.binding_id);
        let mut cycle=first.clone();cycle.derived_from_binding_id=Some(second.binding_id);
        assert!(validate_binding(&second,&target(&second),&metadata,&receipt,&[cycle],&BTreeSet::new(),&bounds(),&BTreeSet::new()).is_err());
    }
    #[test] fn occurrence_rejects_ambiguous_or_unbounded_owner_sets() {
        let item=Item{item_id:ItemId(uuid::Uuid::from_u128(10)),owner_scope:ScopeRef("scope".into()),revision:Revision(1),first_capture_id:CaptureId(uuid::Uuid::from_u128(15)),selected_observation_id:None,gallery_observation_id:None,member_order:vec![],title_override:Override::Inherit,description_override:Override::Inherit,cover_override:Override::Inherit,created_at:at(),updated_at:at(),lifecycle:Lifecycle::Active,deleted_at:None,common:Extensions::default()};
        let occurrence=OccurrenceReference{occurrence_ref:OccurrenceRef(host("note")),item_id:item.item_id,member_id:None,context_membership_id:None,caption_override:Override::Inherit,presentation:None,revision:Revision(1),common:Extensions::default()};
        let membership=ProjectMembership{membership_id:MembershipId(uuid::Uuid::from_u128(22)),item_id:item.item_id,project_ref:ProjectRef(host("project")),scope:ScopeRef("scope".into()),saved_at:at(),created_here_at:at(),revision:Revision(1),lifecycle:Lifecycle::Active,removed_at:None,common:Extensions::default()};
        assert!(validate_occurrence(&occurrence,&item,&[],&[membership.clone()],&bounds(),&BTreeSet::new()).is_ok());
        assert!(matches!(validate_occurrence(&occurrence,&item,&[],&[membership.clone(),membership.clone()],&bounds(),&BTreeSet::new()),Err(Error::Conflict{field}) if field=="membership_id"));
        let mut small=bounds();small.max_collection_items=1;
        assert!(matches!(validate_occurrence(&occurrence,&item,&[],&[membership.clone(),membership],&small,&BTreeSet::new()),Err(Error::LimitExceeded{..})));
    }
}
