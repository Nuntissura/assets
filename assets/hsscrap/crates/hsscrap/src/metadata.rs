//! Pure immutable observation materialization and revision-fenced acceptance.
use std::collections::BTreeSet;
use crate::*;

#[derive(Debug,Clone,PartialEq)]
pub struct MetadataPatch {
    pub title: Patch<String>, pub description: Patch<String>, pub author: Patch<String>,
    pub published_at: Patch<PublicationTime>, pub canonical_url_assertion: Patch<String>,
    pub provider: Patch<String>, pub provider_labels: Patch<Vec<String>>,
    pub gallery_snapshot: Patch<GallerySnapshot>, pub source_payload_digest: Patch<String>,
}

fn apply<T:Clone>(patch:&Patch<T>, baseline:&Option<T>)->Option<T> {
    match patch { Patch::Keep=>baseline.clone(), Patch::Set(v)=>Some(v.clone()), Patch::Clear=>None }
}

/// Producer omission is Keep, explicit JSON null is Clear; caller parses that boundary explicitly.
pub fn materialize_patch(
    mut observation:MetadataObservation, baseline:&MetadataObservation, patch:&MetadataPatch,
    bounds:&Bounds, supported:&BTreeSet<String>,
)->Result<MetadataObservation> {
    if observation.item_id!=baseline.item_id || observation.stream_id!=baseline.stream_id
        || observation.observation_id==baseline.observation_id
        || observation.access_partition!=baseline.access_partition || observation.producer!=baseline.producer
        || observation.baseline_observation_id!=Some(baseline.observation_id) {
        return Err(Error::Conflict{field:"observation_baseline".into()});
    }
    observation.title=apply(&patch.title,&baseline.title);
    observation.description=apply(&patch.description,&baseline.description);
    observation.author=apply(&patch.author,&baseline.author);
    observation.published_at=apply(&patch.published_at,&baseline.published_at);
    observation.canonical_url_assertion=apply(&patch.canonical_url_assertion,&baseline.canonical_url_assertion);
    observation.provider=apply(&patch.provider,&baseline.provider);
    observation.gallery_snapshot=apply(&patch.gallery_snapshot,&baseline.gallery_snapshot);
    observation.source_payload_digest=apply(&patch.source_payload_digest,&baseline.source_payload_digest);
    observation.provider_labels=match &patch.provider_labels {
        Patch::Keep=>baseline.provider_labels.clone(),Patch::Set(v)=>v.clone(),
        Patch::Clear=>return Err(invalid("provider_labels","nonnullable collection; use Set(empty)")),
    };
    observation.validate(bounds,supported)?;
    Ok(observation)
}

#[derive(Debug,Clone,PartialEq)]
pub struct ObservationAccepted { pub item:Item, pub stream:ObservationStream, pub observation:MetadataObservation }

pub fn reserve_sequence(stream:&ObservationStream,expected:Revision)->Result<ObservationStream> {
    if stream.revision!=expected {return Err(Error::Conflict{field:"stream_revision".into()});}
    let mut next=stream.clone(); next.last_issued_sequence=stream.last_issued_sequence.next()?;
    next.revision=stream.revision.next()?; Ok(next)
}

pub fn accept_observation(
    item:&Item,stream:&ObservationStream,observation:MetadataObservation,
    baseline:Option<&MetadataObservation>,expected_item:Revision,expected_stream:Revision,
    bounds:&Bounds,supported:&BTreeSet<String>,
)->Result<ObservationAccepted> {
    observation.validate(bounds,supported)?;
    if observation.status.state!=AvailabilityState::Available {
        return Err(invalid("observation_status","unavailable acquisition cannot replace accepted metadata; retain failure diagnostics separately"));
    }
    if item.lifecycle!=Lifecycle::Active {return Err(Error::Conflict{field:"item_tombstone".into()});}
    if stream.latest_observation_id==Some(observation.observation_id) {return Err(Error::Conflict{field:"immutable_observation_id".into()});}
    if item.revision!=expected_item || stream.revision!=expected_stream {return Err(Error::Conflict{field:"revision".into()});}
    if stream.item_id!=item.item_id || observation.item_id!=item.item_id || observation.stream_id!=stream.stream_id
        || observation.producer!=stream.producer || observation.access_partition!=stream.access_partition {
        return Err(Error::Conflict{field:"observation_stream".into()});
    }
    if observation.sequence>stream.last_issued_sequence || stream.last_accepted_sequence.is_some_and(|n|observation.sequence<=n) {
        return Err(Error::Conflict{field:"observation_sequence".into()});
    }
    if let Some(baseline_id)=observation.baseline_observation_id {
        let base=baseline.ok_or_else(||invalid("baseline","missing immutable predecessor"))?;
        if stream.latest_observation_id!=Some(baseline_id) || base.observation_id!=baseline_id
            || base.item_id!=item.item_id || base.stream_id!=stream.stream_id || base.access_partition!=stream.access_partition
            || base.producer!=stream.producer || stream.last_accepted_sequence!=Some(base.sequence) {
            return Err(Error::Conflict{field:"observation_baseline".into()});
        }
    }
    let mut next_item=item.clone(); let mut next_stream=stream.clone();
    if stream.latest_observation_id.is_some() && item.selected_observation_id==stream.latest_observation_id {
        next_item.selected_observation_id=Some(observation.observation_id);
    }
    next_item.revision=item.revision.next()?;
    next_item.updated_at=observation.observed_at.clone();
    next_stream.last_accepted_sequence=Some(observation.sequence);
    next_stream.latest_observation_id=Some(observation.observation_id);
    next_stream.revision=stream.revision.next()?;
    Ok(ObservationAccepted{item:next_item,stream:next_stream,observation})
}

#[derive(Debug,Clone,PartialEq)]
pub struct EffectiveMetadata {pub title:Option<String>,pub description:Option<String>,pub author:Option<String>,pub published_at:Option<PublicationTime>}

fn effective<T:Clone>(value:&Override<T>,observed:Option<&T>)->Option<T> {
    match value {Override::Inherit=>observed.cloned(),Override::Set(v)=>Some(v.clone()),Override::Clear=>None}
}

/// Hosts authorize the item and selected observation independently. Denied source data supplies None.
pub fn project_metadata(item:&Item,authorized_item:bool,authorized_selected:Option<&MetadataObservation>)->Result<EffectiveMetadata> {
    if !authorized_item {return Err(Error::Denied);}
    let observation=authorized_selected.filter(|o|o.item_id==item.item_id && Some(o.observation_id)==item.selected_observation_id);
    Ok(EffectiveMetadata{
        title:effective(&item.title_override,observation.and_then(|o|o.title.as_ref())),
        description:effective(&item.description_override,observation.and_then(|o|o.description.as_ref())),
        author:observation.and_then(|o|o.author.clone()),published_at:observation.and_then(|o|o.published_at.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn instant()->Instant {Instant::parse("2026-10-23T00:00:00Z").unwrap()}
    fn bounds()->Bounds {Bounds{max_url_bytes:4096,max_text_bytes:4096,max_collection_items:100,max_page_items:25,max_operation_bytes:20000}}
    fn observation(id:u128,sequence:u64)->MetadataObservation {
        MetadataObservation{
            observation_id:ObservationId(uuid::Uuid::from_u128(id)),item_id:ItemId(uuid::Uuid::from_u128(1)),stream_id:StreamId(uuid::Uuid::from_u128(2)),sequence:Revision(sequence),
            baseline_observation_id:None,capture_id:Some(CaptureId(uuid::Uuid::from_u128(3))),appearance_ref:None,producer:"provider".into(),access_partition:ScopeRef("private".into()),observed_at:instant(),source_revision:None,canonical_url_assertion:None,provider:Some("provider".into()),host_site:"example.org".into(),title:Some("source".into()),description:Some("private description".into()),author:None,published_at:None,provider_labels:vec!["label".into()],gallery_snapshot:None,source_payload_digest:None,status:Availability{state:AvailabilityState::Available,checked_at:None,reason_code:None,retry_after:None},common:Extensions::default(),
        }
    }
    fn item()->Item {
        Item{item_id:ItemId(uuid::Uuid::from_u128(1)),owner_scope:ScopeRef("account".into()),revision:Revision(7),first_capture_id:CaptureId(uuid::Uuid::from_u128(3)),selected_observation_id:Some(ObservationId(uuid::Uuid::from_u128(4))),gallery_observation_id:None,member_order:vec![],title_override:Override::Set("my title".into()),description_override:Override::Clear,cover_override:Override::Inherit,created_at:instant(),updated_at:instant(),lifecycle:Lifecycle::Active,deleted_at:None,common:Extensions::default()}
    }
    fn stream()->ObservationStream {
        ObservationStream{stream_id:StreamId(uuid::Uuid::from_u128(2)),item_id:ItemId(uuid::Uuid::from_u128(1)),producer:"provider".into(),provider_namespace:"posts".into(),source_ref:HostRef{host_instance_id:"host".into(),owner_kind:"capture".into(),native_id:"capture".into()},access_partition:ScopeRef("private".into()),last_issued_sequence:Revision(2),last_accepted_sequence:Some(Revision(1)),latest_observation_id:Some(ObservationId(uuid::Uuid::from_u128(4))),revision:Revision(8),common:Extensions::default()}
    }
    #[test] fn refresh_fences_sequences_and_preserves_overrides() {
        let item=item();let stream=stream();let old=observation(4,1);let mut incoming=observation(5,2);
        incoming.baseline_observation_id=Some(old.observation_id);
        let accepted=accept_observation(&item,&stream,incoming.clone(),Some(&old),Revision(7),Revision(8),&bounds(),&BTreeSet::new()).unwrap();
        assert_eq!(accepted.item.title_override,item.title_override);assert_eq!(accepted.item.description_override,Override::Clear);
        assert_eq!(accepted.item.selected_observation_id,Some(incoming.observation_id));
        assert!(accept_observation(&accepted.item,&accepted.stream,incoming.clone(),Some(&old),Revision(8),Revision(9),&bounds(),&BTreeSet::new()).is_err());
        assert!(accept_observation(&item,&stream,incoming,Some(&old),Revision(6),Revision(8),&bounds(),&BTreeSet::new()).is_err());
        let effective=project_metadata(&accepted.item,true,Some(&accepted.observation)).unwrap();
        assert_eq!(effective.title.as_deref(),Some("my title"));assert_eq!(effective.description,None);
        let mut failed=observation(6,2);failed.status.state=AvailabilityState::Restricted;
        assert!(accept_observation(&item,&stream,failed,None,Revision(7),Revision(8),&bounds(),&BTreeSet::new()).is_err());
        let reused=observation(4,2);
        assert!(accept_observation(&item,&stream,reused,None,Revision(7),Revision(8),&bounds(),&BTreeSet::new()).is_err());
    }
    #[test] fn partial_metadata_never_inherits_from_another_access_partition() {
        let base=observation(4,1);let mut incoming=observation(5,2);incoming.baseline_observation_id=Some(base.observation_id);
        let patch=MetadataPatch{title:Patch::Clear,description:Patch::Keep,author:Patch::Keep,published_at:Patch::Keep,canonical_url_assertion:Patch::Keep,provider:Patch::Keep,provider_labels:Patch::Set(vec![]),gallery_snapshot:Patch::Keep,source_payload_digest:Patch::Keep};
        let combined=materialize_patch(incoming.clone(),&base,&patch,&bounds(),&BTreeSet::new()).unwrap();
        assert_eq!(combined.title,None);assert_eq!(combined.description,base.description);assert!(combined.provider_labels.is_empty());
        let mut reused=incoming.clone();reused.observation_id=base.observation_id;
        assert!(materialize_patch(reused,&base,&patch,&bounds(),&BTreeSet::new()).is_err());
        incoming.access_partition=ScopeRef("public".into());
        assert!(materialize_patch(incoming,&base,&patch,&bounds(),&BTreeSet::new()).is_err());
        let mut inherited=item();inherited.title_override=Override::Inherit;inherited.description_override=Override::Inherit;
        let denied=project_metadata(&inherited,true,None).unwrap();assert_eq!(denied.title,None);assert_eq!(denied.description,None);
        assert_eq!(project_metadata(&inherited,false,Some(&base)),Err(Error::Denied));
    }
}
