use rss_contracts::*;
fn reference(id: &str) -> HostRef { HostRef { host_instance_id: "host".into(), owner_kind: "rss".into(), native_id: id.into() } }
fn bounds() -> Bounds { Bounds { max_url_bytes:4096, max_text_bytes:4096, max_collection_items:100, max_page_items:25, max_operation_bytes:100000 } }
fn subscription() -> Subscription {
    Subscription { subscription_ref:reference("sub"), source_ref:reference("source"),
        partition:AccessPartition { scope:ScopeRef("public".into()), mode:AccessMode::PublicProfile, connection_ref:None },
        executor_ref:reference("executor"), generation:Revision(1), paused:false, common:Extensions::default() }
}
#[test]
fn access_modes_roundtrip_without_conflating_public_and_connected() {
    for mode in [AccessMode::PublicFeed, AccessMode::PublicProfile, AccessMode::PublicReels, AccessMode::PublicBoard,
        AccessMode::PublicChannel, AccessMode::Following, AccessMode::HomeForYou, AccessMode::PrivateSource,
        AccessMode::OwnUploads, AccessMode::SubscriptionImport] {
        let mut partition = AccessPartition { scope:ScopeRef("scope".into()), mode,
            connection_ref:mode.requires_connection().then(|| reference("connection")) };
        partition.validate(&bounds()).unwrap();
        let encoded=serde_json::to_string(&partition).unwrap();
        assert_eq!(serde_json::from_str::<AccessPartition>(&encoded).unwrap(),partition);
        partition.connection_ref = if partition.connection_ref.is_some() { None } else { Some(reference("connection")) };
        assert!(partition.validate(&bounds()).is_err());
    }
}
#[test]
fn continuation_cannot_cross_source_access_executor_or_generation() {
    let subscription=subscription();
    let cursor=Continuation { source_ref:subscription.source_ref.clone(), subscription_ref:subscription.subscription_ref.clone(),
        partition:subscription.partition.clone(), executor_ref:subscription.executor_ref.clone(), generation:Revision(1), token:"page2".into() };
    cursor.validate_for(&subscription,&bounds()).unwrap();
    let mut changed=cursor.clone(); changed.generation=Revision(2);
    assert_eq!(changed.validate_for(&subscription,&bounds()),Err(Error::StaleResult));
    let mut changed=cursor.clone(); changed.partition.scope=ScopeRef("private".into());
    assert_eq!(changed.validate_for(&subscription,&bounds()),Err(Error::StaleResult));
    let mut changed=cursor.clone(); changed.source_ref=reference("another");
    assert_eq!(changed.validate_for(&subscription,&bounds()),Err(Error::StaleResult));
    let mut changed=cursor; changed.executor_ref=reference("another");
    assert_eq!(changed.validate_for(&subscription,&bounds()),Err(Error::StaleResult));
}
#[test]
fn failed_or_expired_pages_do_not_become_verified_empty() {
    let subscription=subscription();
    let operation="00000000-0000-0000-0000-000000000001".parse().unwrap();
    let mut page=AcquisitionPage { subscription_ref:subscription.subscription_ref.clone(), source_ref:subscription.source_ref.clone(),
        partition:subscription.partition.clone(), executor_ref:subscription.executor_ref.clone(), generation:subscription.generation, operation_id:operation,
        entries:vec![], state:AcquisitionState::Expired, coverage:hsscrap::Coverage::Partial, continuation:None };
    for state in [AcquisitionState::Expired, AcquisitionState::Unsupported,
        AcquisitionState::Partial { observed_at:Instant::parse("2026-10-23T00:00:00Z").unwrap(), reason_code:"challenge".into() }] {
        page.state=state.clone();
        let wire=serde_json::to_string(&page).unwrap();
        assert_eq!(serde_json::from_str::<AcquisitionPage>(&wire).unwrap().state,state);
    }
    page.state=AcquisitionState::Ready { last_success_at:Instant::parse("2026-10-23T00:00:00Z").unwrap() };
    assert!(page.validate(&subscription,&bounds(),&Default::default()).is_err());
    page.state=AcquisitionState::VerifiedEmpty { observed_at:Instant::parse("2026-10-23T00:00:00Z").unwrap(), selected_source_identity:"source".into() };
    assert!(page.validate(&subscription,&bounds(),&Default::default()).is_err());
    page.coverage=hsscrap::Coverage::Complete;
    page.validate(&subscription,&bounds(),&Default::default()).unwrap();
    page.generation=Revision(2);
    assert_eq!(page.validate(&subscription,&bounds(),&Default::default()),Err(Error::StaleResult));
    page.generation=subscription.generation;
    page.executor_ref=reference("stale-executor");
    assert_eq!(page.validate(&subscription,&bounds(),&Default::default()),Err(Error::StaleResult));
}


#[test]
fn terminal_document_marker_requires_empty_partial_without_an_outgoing_cursor() {
    let subscription = subscription();
    let mut page = AcquisitionPage { subscription_ref: subscription.subscription_ref.clone(), source_ref: subscription.source_ref.clone(),
        partition: subscription.partition.clone(), executor_ref: subscription.executor_ref.clone(), generation: subscription.generation,
        operation_id: "00000000-0000-0000-0000-000000000010".parse().unwrap(), entries: vec![],
        state: AcquisitionState::Partial { observed_at: Instant::parse("2026-10-23T00:00:00Z").unwrap(), reason_code: TERMINAL_SOURCE_DOCUMENT.into() },
        coverage: hsscrap::Coverage::Partial, continuation: None };
    page.validate(&subscription, &bounds(), &Default::default()).unwrap(); assert!(page.is_terminal_partial_document());
    page.coverage = hsscrap::Coverage::Complete;
    assert!(page.validate(&subscription, &bounds(), &Default::default()).is_err());
    page.coverage = hsscrap::Coverage::Partial;
    page.continuation = Some(Continuation { source_ref: subscription.source_ref.clone(), subscription_ref: subscription.subscription_ref.clone(),
        partition: subscription.partition.clone(), executor_ref: subscription.executor_ref.clone(), generation: subscription.generation, token: "next".into() });
    assert!(page.validate(&subscription, &bounds(), &Default::default()).is_err());
}
