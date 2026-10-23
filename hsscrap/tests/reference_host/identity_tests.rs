use super::*;
use hsscrap::ports::{IdentityQuery,ItemOwner,UrlIdentityQuery};

fn record(id:u128,url:&str)->(Item,URLCapture,MetadataObservation) {
    let item_id=ItemId(uuid::Uuid::from_u128(id));
    let capture_id=CaptureId(uuid::Uuid::from_u128(id+1));
    let observation_id=ObservationId(uuid::Uuid::from_u128(id+2));
    let item=Item{item_id,owner_scope:scope(),revision:Revision(1),first_capture_id:capture_id,selected_observation_id:None,
        gallery_observation_id:None,member_order:vec![],title_override:Override::Inherit,description_override:Override::Inherit,
        cover_override:Override::Inherit,created_at:at(),updated_at:at(),lifecycle:Lifecycle::Active,deleted_at:None,common:Extensions::default()};
    let normalized=hsscrap::identity::normalize_url(url,&bounds()).unwrap();
    let capture=URLCapture{capture_id,item_id,capture_kind:CaptureKind::UserInput,access_partition:scope(),captured_url:url.into(),
        original_url:Some(url.into()),identity_url:normalized.identity_url,normalization_version:normalized.normalization_version,
        captured_at:Some(at()),created_here_at:at(),actor_ref:Some(actor()),operation_id:OperationId(uuid::Uuid::from_u128(id+3)),common:Extensions::default()};
    let observation=MetadataObservation{observation_id,item_id,stream_id:StreamId(uuid::Uuid::from_u128(id+4)),sequence:Revision(1),
        baseline_observation_id:None,capture_id:Some(capture_id),appearance_ref:None,producer:"reference-provider".into(),access_partition:scope(),
        observed_at:at(),source_revision:None,canonical_url_assertion:None,provider:Some("test-provider".into()),host_site:"example.org".into(),
        title:None,description:None,author:None,published_at:None,provider_labels:vec![],gallery_snapshot:None,source_payload_digest:None,
        status:Availability{state:AvailabilityState::Available,checked_at:Some(at()),reason_code:None,retry_after:None},common:Extensions::default()};
    (item,capture,observation)
}
fn owner_fixture()->(tempfile::TempDir,ReferenceHost) {
    let dir=tempfile::Builder::new().prefix("hsscrap-identities-").tempdir().unwrap();
    let host=ReferenceHost::new(dir.path().join("state.json"),limits(),Instant::parse("2026-10-24T00:00:00Z").unwrap()).unwrap();
    let first=record(1,"https://example.org/one");let second=record(11,"https://example.org/two");
    host.initialize_with_observations(scope(),BTreeSet::from([actor()]),vec![first.0,second.0],vec![first.1,second.1],vec![first.2,second.2],BTreeSet::new()).unwrap();
    (dir,host)
}
fn context(operation:u128,item:u128,revision:u64)->CommandContext {
    CommandContext{actor:actor(),scope:scope(),operation_id:OperationId(uuid::Uuid::from_u128(operation)),intent_digest:String::new(),
        expected_revisions:vec![ExpectedRevision{owner:item_ref(ItemId(uuid::Uuid::from_u128(item))),revision:Revision(revision)}],bounds:bounds()}
}
fn assertion(id:u128,item:u128)->SourceIdentity {
    SourceIdentity{identity_id:IdentityId(uuid::Uuid::from_u128(id)),item_id:ItemId(uuid::Uuid::from_u128(item)),provider:"test-provider".into(),
        namespace:"posts/test".into(),provider_item_id:"post-one".into(),access_partition:scope(),
        verified_by_observation_id:ObservationId(uuid::Uuid::from_u128(item+2)),retired_at:None,common:Extensions::default()}
}
fn url_query(url:&str)->UrlIdentityQuery {
    UrlIdentityQuery{identity_url:url.into(),normalization_version:hsscrap::identity::NORMALIZATION_VERSION.into(),access_partition:scope()}
}
fn provider_query()->IdentityQuery {IdentityQuery{provider:"test-provider".into(),namespace:"posts/test".into(),provider_item_id:"post-one".into(),access_partition:scope()}}

#[test]
fn canonical_url_and_provider_identity_are_read_back_after_lost_response_restart_and_replay() {
    let (_dir,mut host)=owner_fixture();let request=context(200,1,1);let identity=assertion(100,1);
    assert_eq!(host.lookup_url(&request,url_query("https://example.org/one")).unwrap(),vec![item_id()]);
    assert_eq!(host.lookup_url(&request,url_query("https://example.org/missing")).unwrap(),vec![]);
    let plan=plan_operation(request.clone(),vec![OwnerCommand::AssertSourceIdentity(identity.clone())],&BTreeSet::new()).unwrap();
    assert!(matches!(host.commit(plan,host.commit_time.clone(),FailurePoint::AfterCommit),Err(Error::UnknownOutcome{..})));
    let canonical=read_canonical(&host.path);assert_eq!(canonical.identities[&identity.identity_id],identity);assert_eq!(canonical.items[&item_id()].revision,Revision(2));
    assert_eq!(canonical.items[&item_id()].updated_at,Instant::parse("2026-10-24T00:00:00Z").unwrap());
    assert_eq!(canonical.captures[&canonical.items[&item_id()].first_capture_id].captured_at,Some(at()));
    assert_eq!(canonical.observations[&identity.verified_by_observation_id].observed_at,at());
    let path=host.path.clone();drop(host);host=ReferenceHost::new(path,limits(),at()).unwrap();
    assert_eq!(host.assert_source_identity(request.clone(),identity.clone()).unwrap().outcome,OutcomeState::Committed);
    assert_eq!(host.inspect_source_identity(&request,identity.identity_id).unwrap(),identity);
    assert_eq!(host.lookup_identity(&request,provider_query()).unwrap(),Some(item_id()));
    assert_eq!(read_canonical(&host.path).items[&item_id()].revision,Revision(2));
    let other_urls=host.lookup_url(&request,url_query("https://example.org/two")).unwrap();
    assert!(matches!(hsscrap::identity::resolve_identity(&scope(),"test-provider","posts/test","post-one",&[identity.clone()],&other_urls),Err(Error::Conflict{..})));
    host.tombstone(&actor(),&scope(),item_id(),Revision(2),at()).unwrap();
    host.assert_source_identity(request.clone(),identity.clone()).unwrap();
    assert_eq!(read_canonical(&host.path).items[&item_id()].lifecycle,Lifecycle::Tombstoned);
    assert!(host.lookup_url(&request,url_query("https://example.org/one")).unwrap().is_empty());
    host.revoke(&actor()).unwrap();
    assert_eq!(host.inspect_source_identity(&request,identity.identity_id),Err(Error::Denied));
    assert_eq!(host.lookup_url(&request,url_query("https://example.org/one")),Err(Error::Denied));
}

#[test]
fn canonical_identity_collisions_and_unverified_observations_cannot_change_owner_records() {
    let (_dir,mut host)=owner_fixture();let identity=assertion(100,1);
    host.assert_source_identity(context(200,1,1),identity.clone()).unwrap();
    assert!(matches!(host.assert_source_identity(context(201,11,1),assertion(101,11)),Err(Error::Conflict{field}) if field=="source_identity"));
    let mut wrong=assertion(102,11);wrong.provider_item_id="post-two".into();wrong.verified_by_observation_id=identity.verified_by_observation_id;
    assert!(matches!(host.assert_source_identity(context(202,11,1),wrong),Err(Error::Conflict{field}) if field=="identity_observation"));
    let mut changed=identity.clone();changed.provider_item_id="changed-intent".into();
    assert!(matches!(host.assert_source_identity(context(200,1,2),changed),Err(Error::Conflict{field}) if field=="operation_intent_reuse"));
    let mut stale=assertion(103,1);stale.provider_item_id="post-stale".into();
    assert!(matches!(host.assert_source_identity(context(203,1,1),stale),Err(Error::Conflict{field}) if field=="expected_revision"));
    let mut unfenced=context(204,11,1);unfenced.expected_revisions.clear();
    assert!(matches!(host.assert_source_identity(unfenced,assertion(104,11)),Err(Error::Invalid{field,..}) if field=="expected_revisions"));
    let before=std::fs::read(&host.path).unwrap();
    let mut empty=assertion(105,11);empty.namespace.clear();
    assert!(matches!(host.assert_source_identity(context(205,11,1),empty),Err(Error::Invalid{field,..}) if field=="source_identity"));
    assert_eq!(std::fs::read(&host.path).unwrap(),before);
    let canonical=read_canonical(&host.path);assert_eq!(canonical.identities.len(),1);assert_eq!(canonical.identities[&identity.identity_id],identity);
    assert_eq!(canonical.items[&ItemId(uuid::Uuid::from_u128(11))].revision,Revision(1));
}

#[test]
fn identity_reads_enforce_partition_normalization_version_and_finite_query_bounds() {
    let (_dir,host)=owner_fixture();let request=context(200,1,1);
    let mut foreign=url_query("https://example.org/one");foreign.access_partition=ScopeRef("foreign".into());
    assert_eq!(host.lookup_url(&request,foreign),Err(Error::Denied));
    let mut unknown=url_query("https://example.org/one");unknown.normalization_version="future".into();
    assert!(matches!(host.lookup_url(&request,unknown),Err(Error::UnsupportedRequiredFeature{..})));
    let mut small=request.clone();small.bounds.max_url_bytes=5;
    assert!(matches!(host.lookup_url(&small,url_query("https://example.org/one")),Err(Error::LimitExceeded{..})));
    let mut other=request.clone();other.scope=ScopeRef("foreign".into());
    assert_eq!(host.lookup_url(&other,url_query("https://example.org/one")),Err(Error::Denied));
}

#[test]
fn identity_receipt_capacity_is_checked_before_pending_or_owner_effects() {
    let (_dir,mut host)=owner_fixture();let before=std::fs::read(&host.path).unwrap();
    let mut request=context(200,1,1);request.bounds.max_collection_items=1;
    assert_eq!(host.assert_source_identity(request,assertion(100,1)),Err(Error::LimitExceeded{field:"result_refs".into(),limit:1}));
    assert_eq!(std::fs::read(&host.path).unwrap(),before);
    let mut exact=context(200,1,1);exact.bounds.max_collection_items=2;
    let outcome=host.assert_source_identity(exact,assertion(100,1)).unwrap();
    assert_eq!(outcome.outcome,OutcomeState::Committed);assert_eq!(outcome.result_refs.len(),2);
    let canonical=read_canonical(&host.path);assert_eq!(canonical.identities.len(),1);assert_eq!(canonical.items[&item_id()].revision,Revision(2));
}
