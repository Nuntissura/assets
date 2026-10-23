use rss_acquisition::jobs::*;
use rss_contracts::*;
fn reference(id: &str) -> HostRef { HostRef { host_instance_id: "host".into(), owner_kind: "rss".into(), native_id: id.into() } }
fn request(operation: u64) -> SourceRequest {
    SourceRequest {
        subscription: Subscription { subscription_ref: reference("subscription"), source_ref: reference("source"),
            partition: AccessPartition { scope: ScopeRef("account".into()), mode: AccessMode::PublicFeed, connection_ref: None },
            executor_ref: reference("executor"), generation: Revision(1), paused: false, common: Extensions::default() },
        operation_id: format!("00000000-0000-0000-0000-{operation:012}").parse().unwrap(), continuation: None,
        bounds: Bounds { max_url_bytes: 4096, max_text_bytes: 4096, max_collection_items: 100, max_page_items: 25, max_operation_bytes: 100000 },
    }
}
fn limits() -> JobLimits { JobLimits { max_jobs: 2, max_running: 1, max_attempts: 3, timeout_ticks: 10, initial_backoff_ticks: 2, max_backoff_ticks: 5 } }

#[test]
fn one_authoritative_job_survives_duplicate_profile_views_and_serialization() {
    let mut queue = JobQueue::new(limits()).unwrap();
    let first = request(1);
    queue.enqueue(first.clone(), 0).unwrap();
    queue.enqueue(first.clone(), 99).unwrap();
    assert_eq!(queue.jobs().len(), 1);
    assert_eq!(queue.enqueue(request(2), 0), Err(Error::Conflict));
    let mut reused = first.clone(); reused.subscription.generation = Revision(2);
    assert_eq!(queue.enqueue(reused, 0), Err(Error::Conflict));
    let bytes = serde_json::to_vec(&queue).unwrap();
    let mut queue: JobQueue = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(queue.start(first.operation_id, &reference("other-host"), 0), Err(Error::Denied));
    queue.start(first.operation_id, &reference("executor"), 0).unwrap();
    queue.recover_interrupted(1, true).unwrap();
    assert_eq!(queue.jobs()[0].state, JobState::AwaitingRecovery);
    assert_eq!(queue.start(first.operation_id, &reference("executor"), 12), Err(Error::Conflict));
    assert_eq!(queue.remove_terminal(first.operation_id), Err(Error::Conflict));
}

#[test]
fn retries_are_bounded_and_rate_limit_deadlines_are_respected() {
    let mut queue = JobQueue::new(limits()).unwrap();
    let first = request(1); queue.enqueue(first.clone(), 0).unwrap();
    queue.start(first.operation_id, &reference("executor"), 0).unwrap();
    queue.fail(first.operation_id, &Error::RateLimited { retry_after: None }, 1, Some(20)).unwrap();
    assert_eq!(queue.jobs()[0].state, JobState::Backoff { eligible_at: 20 });
    assert_eq!(queue.start(first.operation_id, &reference("executor"), 19), Err(Error::Conflict));
    queue.start(first.operation_id, &reference("executor"), 20).unwrap();
    queue.fail(first.operation_id, &Error::Network, 21, None).unwrap();
    assert_eq!(queue.jobs()[0].state, JobState::Backoff { eligible_at: 25 });
    queue.start(first.operation_id, &reference("executor"), 25).unwrap();
    queue.fail(first.operation_id, &Error::Network, 26, None).unwrap();
    assert_eq!(queue.jobs()[0].state, JobState::Failed);
    assert_eq!(queue.jobs()[0].attempts, 3);
}

#[test]
fn cancellation_and_expiry_preserve_uncertain_commit_for_recovery() {
    let mut queue = JobQueue::new(limits()).unwrap();
    let first = request(1); queue.enqueue(first.clone(), 0).unwrap();
    queue.start(first.operation_id, &reference("executor"), 0).unwrap();
    queue.begin_commit(first.operation_id, &reference("executor"), 1).unwrap();
    queue.cancel(first.operation_id).unwrap();
    assert!(queue.jobs()[0].cancel_requested);
    assert_eq!(queue.jobs()[0].state, JobState::AwaitingRecovery);
    assert_eq!(queue.fail(first.operation_id, &Error::Expired, 1, None), Err(Error::UnknownOutcome { operation_id: first.operation_id }));
    assert_eq!(queue.remove_terminal(first.operation_id), Err(Error::Conflict));

    let mut queue = JobQueue::new(limits()).unwrap();
    queue.enqueue(first.clone(), 0).unwrap(); queue.start(first.operation_id, &reference("executor"), 0).unwrap();
    queue.fail(first.operation_id, &Error::Expired, 1, None).unwrap();
    assert_eq!(queue.jobs()[0].state, JobState::Reconnect);
}

#[test]
fn capacity_timeouts_and_invalid_recovered_state_fail_closed() {
    let mut queue = JobQueue::new(limits()).unwrap();
    let first = request(1); let mut second = request(2); second.subscription.subscription_ref = reference("second");
    queue.enqueue(first.clone(), 0).unwrap(); queue.enqueue(second.clone(), 0).unwrap();
    let mut third = request(3); third.subscription.subscription_ref = reference("third");
    assert!(matches!(queue.enqueue(third, 0), Err(Error::LimitExceeded { .. })));
    queue.start(first.operation_id, &reference("executor"), 0).unwrap();
    assert!(matches!(queue.start(second.operation_id, &reference("executor"), 0), Err(Error::LimitExceeded { .. })));
    queue.recover_interrupted(10, false).unwrap();
    assert_eq!(queue.jobs()[0].state, JobState::AwaitingRecovery);
    assert!(matches!(queue.start(second.operation_id, &reference("executor"), 10), Err(Error::LimitExceeded { .. })));
    let mut wire = serde_json::to_value(&queue).unwrap(); wire["jobs"][0]["attempts"] = serde_json::json!(0);
    let invalid: JobQueue = serde_json::from_value(wire).unwrap();
    assert!(invalid.validate().is_err());
}

#[test]
fn cancellation_deadline_and_owner_reservations_fence_new_effects() {
    let mut queue = JobQueue::new(limits()).unwrap();
    let first = request(1); let mut second = request(2); second.subscription.subscription_ref = reference("second");
    queue.enqueue(first.clone(), 0).unwrap(); queue.enqueue(second.clone(), 0).unwrap();
    queue.start(first.operation_id, &reference("executor"), 0).unwrap();
    assert_eq!(queue.begin_commit(first.operation_id, &reference("executor"), 10), Err(Error::Conflict));
    assert_eq!(queue.begin_commit(first.operation_id, &reference("wrong"), 1), Err(Error::Denied));
    queue.begin_commit(first.operation_id, &reference("executor"), 1).unwrap();
    assert!(matches!(queue.start(second.operation_id, &reference("executor"), 1), Err(Error::LimitExceeded { .. })));
    let mut queue = JobQueue::new(limits()).unwrap(); queue.enqueue(first.clone(), 0).unwrap();
    queue.start(first.operation_id, &reference("executor"), 0).unwrap(); queue.cancel(first.operation_id).unwrap();
    assert_eq!(queue.begin_commit(first.operation_id, &reference("executor"), 1), Err(Error::Cancelled));
}

#[test]
fn explicit_cancel_settlement_requires_exact_identity_and_releases_only_its_slot() {
    for began_commit in [false, true] {
        let first = request(1);
        let mut second = request(2); second.subscription.subscription_ref = reference("second");
        let mut queue = JobQueue::new(limits()).unwrap();
        queue.enqueue(first.clone(), 0).unwrap(); queue.enqueue(second.clone(), 0).unwrap();
        queue.start(first.operation_id, &reference("executor"), 0).unwrap();
        if began_commit { queue.begin_commit(first.operation_id, &reference("executor"), 1).unwrap(); }
        let before = queue.clone();
        assert_eq!(queue.settle_cancelled(&first, &reference("executor")), Err(Error::Conflict));
        assert_eq!(queue, before);
        queue.cancel(first.operation_id).unwrap();
        let unresolved = queue.clone();
        assert!(matches!(queue.start(second.operation_id, &reference("executor"), 1), Err(Error::LimitExceeded { .. })));
        assert_eq!(queue, unresolved);
        assert_eq!(queue.settle_cancelled(&first, &reference("wrong-executor")), Err(Error::Denied));
        assert_eq!(queue, unresolved);
        let mut changed = Vec::new();
        let mut value = first.clone(); value.subscription.source_ref = reference("different-source"); changed.push(value);
        let mut value = first.clone(); value.subscription.subscription_ref = reference("different-subscription"); changed.push(value);
        let mut value = first.clone(); value.subscription.partition.scope = ScopeRef("different-profile".into()); changed.push(value);
        let mut value = first.clone(); value.subscription.generation = Revision(2); changed.push(value);
        let mut value = first.clone(); value.subscription.executor_ref = reference("other"); changed.push(value);
        let mut value = first.clone(); value.bounds.max_page_items += 1; changed.push(value);
        let mut value = first.clone(); value.operation_id = request(3).operation_id; changed.push(value);
        for value in changed {
            assert!(queue.settle_cancelled(&value, &reference("executor")).is_err());
            assert_eq!(queue, unresolved);
        }
        // The trusted host's termination and canonical reconciliation occur outside this pure reducer.
        queue.settle_cancelled(&first, &reference("executor")).unwrap();
        assert_eq!(queue.jobs()[0].state, JobState::Cancelled);
        assert!(queue.jobs()[0].cancel_requested);
        assert_eq!(queue.jobs()[0].request, first);
        assert_eq!(queue.jobs()[1], unresolved.jobs()[1]);
        let bytes = serde_json::to_vec(&queue).unwrap();
        let mut recovered: JobQueue = serde_json::from_slice(&bytes).unwrap();
        recovered.settle_cancelled(&first, &reference("executor")).unwrap();
        assert_eq!(recovered, queue);
        recovered.start(second.operation_id, &reference("executor"), 1).unwrap();
        recovered.remove_terminal(first.operation_id).unwrap();
        assert_eq!(recovered.jobs().len(), 1);
        assert_eq!(recovered.jobs()[0].request, second);
        assert!(matches!(recovered.jobs()[0].state, JobState::Running { .. }));
    }
    for error in [Error::Expired, Error::Invalid { field: "source".into() }] {
        let first = request(1); let mut queue = JobQueue::new(limits()).unwrap();
        queue.enqueue(first.clone(), 0).unwrap(); queue.start(first.operation_id, &reference("executor"), 0).unwrap();
        queue.cancel(first.operation_id).unwrap(); queue.fail(first.operation_id, &error, 1, None).unwrap();
        let before = queue.clone();
        assert_eq!(queue.settle_cancelled(&first, &reference("executor")), Err(Error::Conflict));
        assert_eq!(queue, before);
    }
}
