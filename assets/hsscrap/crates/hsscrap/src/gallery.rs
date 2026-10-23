//! Pure gallery reconciliation. The host resolves member identities before applying this plan.
use std::collections::{BTreeMap,BTreeSet};
use crate::*;

#[derive(Debug,Clone,PartialEq,Eq)]
pub struct ResolvedMemberIdentity {
    pub item_id:ItemId,pub stream_id:StreamId,pub member_id:MemberId,pub producer_member_key:Option<String>,
}
#[derive(Debug,Clone,Copy)]
pub enum GallerySelection<'a> { Current(&'a MetadataObservation), Initial, ChangeFrom(&'a MetadataObservation) }
#[derive(Debug,Clone,PartialEq)]
pub struct GalleryPlan {pub item:Item,pub members:Vec<GalleryMember>,pub replaced:bool}

/// UUID strings contain no escaping or floating point; their JSON array encoding is JCS.
pub fn gallery_digest(ids:&[MemberId])->Result<String> {
    let bytes=serde_json::to_vec(ids).map_err(|_|invalid("members","cannot serialize member IDs"))?;
    Ok(handshake_canonical_json::sha256_hex(&bytes))
}

pub fn reconcile_gallery(
    item:&Item,stream:&ObservationStream,observation:&MetadataObservation,
    selection:GallerySelection<'_>,members:&[GalleryMember],identities:&[ResolvedMemberIdentity],
    expected_item:Revision,expected_stream:Revision,at:Instant,bounds:&Bounds,supported:&BTreeSet<String>,
)->Result<GalleryPlan> {
    item.validate(bounds,supported)?;observation.validate(bounds,supported)?;
    stream.validate(bounds,supported)?;
    let selected=match selection {GallerySelection::Current(v)|GallerySelection::ChangeFrom(v)=>Some(v),GallerySelection::Initial=>None};
    if let Some(selected)=selected {selected.validate(bounds,supported)?;}
    if item.lifecycle!=Lifecycle::Active {return Err(Error::Conflict{field:"item_tombstone".into()});}
    if members.len()>bounds.max_collection_items || identities.len()>bounds.max_collection_items {
        return Err(Error::LimitExceeded{field:"gallery_members".into(),limit:bounds.max_collection_items});
    }
    if item.revision!=expected_item || stream.revision!=expected_stream {return Err(Error::Conflict{field:"gallery_revision".into()});}
    if observation.item_id!=item.item_id || stream.item_id!=item.item_id || stream.stream_id!=observation.stream_id
        || stream.access_partition!=observation.access_partition || stream.producer!=observation.producer {
        return Err(Error::Conflict{field:"gallery_stream".into()});
    }
    let mut state:BTreeMap<MemberId,GalleryMember>=BTreeMap::new();
    for member in members {
        member.validate(bounds,supported)?;
        if member.item_id!=item.item_id || state.insert(member.member_id,member.clone()).is_some() {return Err(Error::Conflict{field:"gallery_members".into()});}
    }
    if item.member_order.iter().any(|id|state.get(id).is_none_or(|m|m.lifecycle!=MemberLifecycle::Active))
        || state.values().any(|m|m.lifecycle==MemberLifecycle::Active && !item.member_order.contains(&m.member_id)) {
        return Err(Error::Conflict{field:"member_order".into()});
    }
    let mut plan=GalleryPlan{item:item.clone(),members:members.to_vec(),replaced:false};
    if observation.status.state!=AvailabilityState::Available {return Ok(plan);}
    let Some(snapshot)=&observation.gallery_snapshot else {return Ok(plan);};
    if snapshot.coverage==Coverage::Complete {
        let order:Vec<_>=snapshot.members.iter().map(|m|m.member_id).collect();
        if snapshot.full_set_digest.as_ref()!=Some(&gallery_digest(&order)?) {return Err(Error::Conflict{field:"full_set_digest".into()});}
    }
    let mut mapped_ids=BTreeSet::new();let mut mapped_keys=BTreeSet::new();
    for identity in identities {
        if identity.item_id!=item.item_id || identity.stream_id!=stream.stream_id || !mapped_ids.insert(identity.member_id) {
            return Err(Error::Conflict{field:"member_mapping_scope".into()});
        }
        if identity.producer_member_key.as_ref().is_some_and(|key|!mapped_keys.insert(key)) {return Err(Error::Conflict{field:"member_mapping_key".into()});}
        if state.get(&identity.member_id).is_some_and(|m|m.producer_member_key!=identity.producer_member_key) {return Err(Error::Conflict{field:"member_mapping_key".into()});}
    }
    let selected_stream=selected.is_some_and(|selected|item.gallery_observation_id==Some(selected.observation_id)
        && selected.item_id==item.item_id && selected.stream_id==observation.stream_id
        && selected.access_partition==observation.access_partition && selected.producer==stream.producer);
    let explicit_selection=match selection {
        GallerySelection::Initial=>{
            if item.gallery_observation_id.is_some() || !item.member_order.is_empty() {return Err(Error::Conflict{field:"initial_gallery".into()});}
            true
        },
        GallerySelection::ChangeFrom(previous)=>{
            if item.gallery_observation_id!=Some(previous.observation_id) || previous.item_id!=item.item_id
                || previous.stream_id==observation.stream_id {return Err(Error::Conflict{field:"gallery_selection".into()});}
            true
        },
        GallerySelection::Current(_)=>false,
    };
    if selected_stream || matches!(selection,GallerySelection::ChangeFrom(_)) {
        let selected=selected.ok_or_else(||invalid("selected_gallery","missing prior snapshot"))?;
        let prior=selected.gallery_snapshot.as_ref().ok_or_else(||invalid("selected_gallery","missing complete selected snapshot"))?;
        let prior_order:Vec<_>=prior.members.iter().map(|m|m.member_id).collect();
        let prior_ids:BTreeSet<_>=prior_order.iter().copied().collect();
        if prior.coverage!=Coverage::Complete || prior_ids!=item.member_order.iter().copied().collect()
            || prior.full_set_digest.as_ref()!=Some(&gallery_digest(&prior_order)?) {
            return Err(Error::Conflict{field:"selected_gallery".into()});
        }
    }
    let accepted=stream.latest_observation_id==Some(observation.observation_id)
        && stream.last_accepted_sequence==Some(observation.sequence);
    if !accepted {return Err(Error::Conflict{field:"gallery_sequence".into()});}
    let mut incoming=BTreeSet::new();let mut producer_keys=BTreeSet::new();
    for observed in &snapshot.members {
        if !incoming.insert(observed.member_id) {return Err(Error::Conflict{field:"member_identity".into()});}
        if observed.producer_member_key.as_ref().is_some_and(|key|!producer_keys.insert(key)) {return Err(Error::Conflict{field:"producer_member_key".into()});}
        if selected_stream {
            if let Some(key)=&observed.producer_member_key {
                let mut historical=selected.into_iter().flat_map(|s|s.gallery_snapshot.as_ref()).flat_map(|s|&s.members);
                if historical.any(|old|old.producer_member_key.as_ref()==Some(key) && old.member_id!=observed.member_id) {
                    return Err(Error::Conflict{field:"member_mapping_key".into()});
                }
            }
        }
        let matched:Vec<_>=identities.iter().filter(|m|m.item_id==item.item_id && m.stream_id==observation.stream_id && m.member_id==observed.member_id && m.producer_member_key==observed.producer_member_key).collect();
        if matched.len()!=1 {return Err(Error::Conflict{field:"member_identity_unresolved".into()});}
        if !state.contains_key(&observed.member_id) {
            state.insert(observed.member_id,member(item.item_id,observation.observation_id,observed,MemberLifecycle::Staged));
        }
    }
    if state.len()>bounds.max_collection_items {return Err(Error::LimitExceeded{field:"gallery_members".into(),limit:bounds.max_collection_items});}
    if (selected_stream || explicit_selection) && snapshot.coverage==Coverage::Complete {
        if let Override::Set(cover)=item.cover_override {
            if !incoming.contains(&cover) {return Err(Error::Conflict{field:"cover_member_removed".into()});}
        }
        let order:Vec<_>=snapshot.members.iter().map(|m|m.member_id).collect();
        if snapshot.full_set_digest.as_ref()!=Some(&gallery_digest(&order)?) {return Err(Error::Conflict{field:"full_set_digest".into()});}
        for observed in &snapshot.members {
            // Explicit confirmed owner mapping is required even when the provider rotates a locator.
            state.insert(observed.member_id,member(item.item_id,observation.observation_id,observed,MemberLifecycle::Active));
        }
        for existing in state.values_mut() {
            if existing.lifecycle==MemberLifecycle::Active && !incoming.contains(&existing.member_id) {
                existing.lifecycle=MemberLifecycle::Tombstoned;existing.removed_at=Some(at.clone());
            }
        }
        plan.item.member_order=order;
        plan.item.gallery_observation_id=Some(observation.observation_id);
        plan.item.revision=item.revision.next()?;plan.item.updated_at=at;
        plan.replaced=true;
    }
    plan.members=state.into_values().collect();
    if !plan.replaced && plan.members.len()!=members.len() {
        plan.item.revision=item.revision.next()?;
    }
    Ok(plan)
}

fn member(item_id:ItemId,observation_id:ObservationId,m:&MemberObservation,lifecycle:MemberLifecycle)->GalleryMember {
    GalleryMember{member_id:m.member_id,item_id,producer_member_key:m.producer_member_key.clone(),observation_id,
        media_kind:m.media_kind,caption:m.caption.clone(),observed_mime:m.observed_mime.clone(),observed_width:m.observed_width,
        observed_height:m.observed_height,observed_duration_ms:m.observed_duration_ms,availability:m.availability.clone(),
        lifecycle,removed_at:None,common:m.common.clone()}
}

#[cfg(test)]
mod tests {
    use super::*;
    fn bounds()->Bounds {Bounds{max_url_bytes:4096,max_text_bytes:4096,max_collection_items:100,max_page_items:25,max_operation_bytes:20000}}
    fn at()->Instant {Instant::parse("2026-10-23T00:00:00Z").unwrap()}
    fn observed(id:u128)->MemberObservation {MemberObservation{
        member_id:MemberId(uuid::Uuid::from_u128(id)),producer_member_key:Some(id.to_string()),media_kind:MediaKind::Image,
        caption:Some(format!("member {id}")),observed_mime:Some("image/jpeg".into()),observed_width:Some(10),observed_height:Some(10),observed_duration_ms:None,
        availability:Availability{state:AvailabilityState::Available,checked_at:None,reason_code:None,retry_after:None},remote_reference_ids:vec![],common:Extensions::default(),
    }}
    fn fixture()->(Item,ObservationStream,MetadataObservation,MetadataObservation,Vec<GalleryMember>,Vec<ResolvedMemberIdentity>) {
        let iid=ItemId(uuid::Uuid::from_u128(10));let sid=StreamId(uuid::Uuid::from_u128(11));let oldid=ObservationId(uuid::Uuid::from_u128(12));let newid=ObservationId(uuid::Uuid::from_u128(13));
        let item=Item{item_id:iid,owner_scope:ScopeRef("owner".into()),revision:Revision(3),first_capture_id:CaptureId(uuid::Uuid::from_u128(14)),selected_observation_id:Some(oldid),gallery_observation_id:Some(oldid),member_order:vec![observed(1).member_id,observed(2).member_id],title_override:Override::Inherit,description_override:Override::Inherit,cover_override:Override::Inherit,created_at:at(),updated_at:at(),lifecycle:Lifecycle::Active,deleted_at:None,common:Extensions::default()};
        let stream=ObservationStream{stream_id:sid,item_id:iid,producer:"feed".into(),provider_namespace:"posts".into(),source_ref:HostRef{host_instance_id:"host".into(),owner_kind:"rss".into(),native_id:"source".into()},access_partition:ScopeRef("public".into()),last_issued_sequence:Revision(2),last_accepted_sequence:Some(Revision(2)),latest_observation_id:Some(newid),revision:Revision(4),common:Extensions::default()};
        let incoming=MetadataObservation{observation_id:newid,item_id:iid,stream_id:sid,sequence:Revision(2),baseline_observation_id:None,capture_id:Some(item.first_capture_id),appearance_ref:None,producer:"feed".into(),access_partition:ScopeRef("public".into()),observed_at:at(),source_revision:None,canonical_url_assertion:None,provider:None,host_site:"example.org".into(),title:None,description:None,author:None,published_at:None,provider_labels:vec![],gallery_snapshot:Some(GallerySnapshot{coverage:Coverage::Partial,members:vec![observed(3),observed(1)],full_set_digest:None}),source_payload_digest:None,status:observed(1).availability,common:Extensions::default()};
        let mut selected=incoming.clone();selected.observation_id=oldid;selected.sequence=Revision(1);
        selected.gallery_snapshot=Some(GallerySnapshot{coverage:Coverage::Complete,members:vec![observed(1),observed(2)],full_set_digest:Some(gallery_digest(&[observed(1).member_id,observed(2).member_id]).unwrap())});
        let members=vec![member(iid,oldid,&observed(1),MemberLifecycle::Active),member(iid,oldid,&observed(2),MemberLifecycle::Active)];
        let identities=(1..=3).map(|id|ResolvedMemberIdentity{item_id:iid,stream_id:sid,member_id:observed(id).member_id,producer_member_key:observed(id).producer_member_key}).collect();
        (item,stream,incoming,selected,members,identities)
    }
    #[test] fn partial_snapshot_stages_without_reordering_or_overwriting_active_members() {
        assert_eq!(gallery_digest(&[observed(1).member_id,observed(2).member_id]).unwrap(),"10c53fe88d1bef1acd1b226af2c50d2e46ac3902b263e55f8c6b65cb73a13fad");
        let (item,stream,obs,selected,members,ids)=fixture();
        let plan=reconcile_gallery(&item,&stream,&obs,GallerySelection::Current(&selected),&members,&ids,Revision(3),Revision(4),at(),&bounds(),&BTreeSet::new()).unwrap();
        assert!(!plan.replaced);assert_eq!(plan.item.member_order,item.member_order);
        for old in &members {assert_eq!(plan.members.iter().find(|m|m.member_id==old.member_id),Some(old));}
        assert_eq!(plan.members.iter().find(|m|m.member_id==observed(3).member_id).unwrap().lifecycle,MemberLifecycle::Staged);
    }
    #[test] fn selected_complete_snapshot_reorders_preserves_ids_and_tombstones_removed_members() {
        let (item,stream,mut obs,selected,members,ids)=fixture();
        let snap=obs.gallery_snapshot.as_mut().unwrap();snap.coverage=Coverage::Complete;snap.full_set_digest=Some(gallery_digest(&[observed(3).member_id,observed(1).member_id]).unwrap());
        let plan=reconcile_gallery(&item,&stream,&obs,GallerySelection::Current(&selected),&members,&ids,Revision(3),Revision(4),at(),&bounds(),&BTreeSet::new()).unwrap();
        assert!(plan.replaced);assert_eq!(plan.item.member_order,vec![observed(3).member_id,observed(1).member_id]);
        let removed=plan.members.iter().find(|m|m.member_id==observed(2).member_id).unwrap();
        assert_eq!(removed.lifecycle,MemberLifecycle::Tombstoned);assert!(removed.removed_at.is_some());
        assert_eq!(members[1].lifecycle,MemberLifecycle::Active);
    }
    #[test] fn stale_unselected_failed_and_bad_digest_cannot_replace_a_gallery() {
        let (item,stream,mut obs,mut selected,members,ids)=fixture();
        let snap=obs.gallery_snapshot.as_mut().unwrap();snap.coverage=Coverage::Complete;snap.full_set_digest=Some("bad".into());
        assert!(reconcile_gallery(&item,&stream,&obs,GallerySelection::Current(&selected),&members,&ids,Revision(3),Revision(4),at(),&bounds(),&BTreeSet::new()).is_err());
        assert!(reconcile_gallery(&item,&stream,&obs,GallerySelection::Current(&selected),&members,&ids,Revision(2),Revision(4),at(),&bounds(),&BTreeSet::new()).is_err());
        selected.stream_id=StreamId(uuid::Uuid::from_u128(99));
        assert!(reconcile_gallery(&item,&stream,&obs,GallerySelection::Current(&selected),&members,&ids,Revision(3),Revision(4),at(),&bounds(),&BTreeSet::new()).is_err());
        obs.gallery_snapshot.as_mut().unwrap().full_set_digest=Some(gallery_digest(&[observed(3).member_id,observed(1).member_id]).unwrap());
        let plan=reconcile_gallery(&item,&stream,&obs,GallerySelection::Current(&selected),&members,&ids,Revision(3),Revision(4),at(),&bounds(),&BTreeSet::new()).unwrap();
        assert!(!plan.replaced);assert_eq!(plan.item.member_order,item.member_order);
        obs.status.state=AvailabilityState::Restricted;
        let plan=reconcile_gallery(&item,&stream,&obs,GallerySelection::Current(&selected),&members,&ids,Revision(3),Revision(4),at(),&bounds(),&BTreeSet::new()).unwrap();
        assert_eq!(plan.item,item);assert_eq!(plan.members,members);
    }
    #[test] fn producer_mapping_conflicts_and_unsupported_snapshots_reject() {
        let (item,stream,obs,selected,members,mut ids)=fixture();
        ids[2].producer_member_key=ids[0].producer_member_key.clone();
        assert!(reconcile_gallery(&item,&stream,&obs,GallerySelection::Current(&selected),&members,&ids,Revision(3),Revision(4),at(),&bounds(),&BTreeSet::new()).is_err());
        let (_,_,_,_,mut broken,ids)=fixture();broken[0].common.required_features.insert("vendor.required".into());
        assert!(matches!(reconcile_gallery(&item,&stream,&obs,GallerySelection::Current(&selected),&broken,&ids,Revision(3),Revision(4),at(),&bounds(),&BTreeSet::new()),Err(Error::UnsupportedRequiredFeature{..})));
        let mut reassigned=obs.clone();reassigned.gallery_snapshot.as_mut().unwrap().members.truncate(1);
        let member=reassigned.gallery_snapshot.as_mut().unwrap().members.first_mut().unwrap();member.producer_member_key=Some("1".into());
        let only_new=vec![ResolvedMemberIdentity{item_id:item.item_id,stream_id:stream.stream_id,member_id:member.member_id,producer_member_key:member.producer_member_key.clone()}];
        assert!(reconcile_gallery(&item,&stream,&reassigned,GallerySelection::Current(&selected),&members,&only_new,Revision(3),Revision(4),at(),&bounds(),&BTreeSet::new()).is_err());
    }
    #[test] fn first_complete_gallery_and_explicit_source_change_are_atomic_selections() {
        let (mut item,stream,mut obs,selected,members,ids)=fixture();
        item.gallery_observation_id=None;item.member_order.clear();
        let snapshot=obs.gallery_snapshot.as_mut().unwrap();snapshot.coverage=Coverage::Complete;
        snapshot.full_set_digest=Some(gallery_digest(&[observed(3).member_id,observed(1).member_id]).unwrap());
        let initial=reconcile_gallery(&item,&stream,&obs,GallerySelection::Initial,&[],&ids,Revision(3),Revision(4),at(),&bounds(),&BTreeSet::new()).unwrap();
        assert!(initial.replaced);assert_eq!(initial.item.gallery_observation_id,Some(obs.observation_id));
        assert_eq!(initial.item.member_order,vec![observed(3).member_id,observed(1).member_id]);
        let (item,mut other_stream,mut other_obs,_,_,_)=fixture();
        other_stream.stream_id=StreamId(uuid::Uuid::from_u128(99));other_obs.stream_id=other_stream.stream_id;
        let snapshot=other_obs.gallery_snapshot.as_mut().unwrap();snapshot.coverage=Coverage::Complete;
        snapshot.full_set_digest=Some(gallery_digest(&[observed(3).member_id,observed(1).member_id]).unwrap());
        let mapped:Vec<_>=ids.into_iter().map(|mut m|{m.stream_id=other_stream.stream_id;m}).collect();
        let changed=reconcile_gallery(&item,&other_stream,&other_obs,GallerySelection::ChangeFrom(&selected),&members,&mapped,Revision(3),Revision(4),at(),&bounds(),&BTreeSet::new()).unwrap();
        assert!(changed.replaced);assert_eq!(changed.item.gallery_observation_id,Some(other_obs.observation_id));
    }
}
